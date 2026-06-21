use crate::api::{fetch_github_info, http_client, parse_github_repo, safe_prefix, GitHubRepo};
use crate::display::{health_color, is_stale};
use crate::osv;
use crate::types::{
    days_since_date_prefix, health_to_string, collect_results, print_summary, score_from_days, track_license,
    PackageResult, ScanOutput, Summary,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

// ── Structs ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PoetryLock {
    package: Option<Vec<PoetryPkg>>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct PoetryPkg {
    name: String,
    version: String,
    description: Option<String>,
}

#[derive(Deserialize)]
struct PipfileLock {
    default: Option<HashMap<String, PipPkg>>,
    develop: Option<HashMap<String, PipPkg>>,
}

#[derive(Deserialize)]
struct PipPkg {
    version: String,
}

#[derive(Deserialize)]
struct PyPIResponse {
    info: PyPIInfo,
    urls: Vec<PyPIUrl>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct PyPIInfo {
    name: String,
    version: String,
    summary: Option<String>,
    home_page: Option<String>,
    project_urls: Option<HashMap<String, String>>,
    license: Option<String>,
}

#[derive(Deserialize)]
struct PyPIUrl {
    upload_time: Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────

fn extract_github_url(info: &PyPIInfo) -> Option<(String, String)> {
    if let Some(urls) = &info.project_urls {
        for url in urls.values() {
            if let Some(gh) = parse_github_repo(url) {
                return Some(gh);
            }
        }
    }
    if let Some(ref url) = info.home_page {
        if let Some(gh) = parse_github_repo(url) {
            return Some(gh);
        }
    }
    None
}

fn parse_poetry_lock(path: &str) -> Result<Vec<(String, String)>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
    let lock: PoetryLock = toml::from_str(&content).map_err(|e| format!("Parse error: {}", e))?;
    Ok(lock
        .package
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.name, p.version))
        .collect())
}

fn parse_pipfile_lock(path: &str) -> Result<Vec<(String, String)>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
    let lock: PipfileLock =
        serde_json::from_str(&content).map_err(|e| format!("Parse error: {}", e))?;

    let mut deps = Vec::new();
    if let Some(default) = lock.default {
        for (name, info) in default {
            let ver = info.version.trim_start_matches("==").to_string();
            deps.push((name, ver));
        }
    }
    if let Some(develop) = lock.develop {
        for (name, info) in develop {
            let ver = info.version.trim_start_matches("==").to_string();
            deps.push((name, ver));
        }
    }
    Ok(deps)
}

// ── Health scoring ───────────────────────────────────────────────────

fn get_pypi_health(_info: &PyPIInfo, urls: &[PyPIUrl], gh: Option<&GitHubRepo>) -> &'static str {
    if let Some(url) = urls.first() {
        if let Some(ref upload_time) = url.upload_time {
            let clean = upload_time.trim_end_matches('Z');
            if let Some(days) = days_since_date_prefix(clean) {
                let health = score_from_days(days);
                if health != "✅" {
                    return health;
                }
            } else {
                return "❓";
            }
        } else {
            return "❓";
        }
    } else {
        return "❓";
    }

    // PyPI says fresh — check cached GitHub data
    if let Some(gh) = gh {
        if let Some(days) = days_since_date_prefix(&gh.pushed_at) {
            return score_from_days(days);
        }
    }

    "✅"
}

/// Stale reason for PyPI packages.
///
/// Returns `None` when the package is fully healthy (both PyPI and
/// GitHub are active).  This prevents false positives where a healthy
/// package with no GitHub URL would get "No GitHub repository found" as
/// a stale reason.
///
/// `gh` is an optional cached `GitHubRepo`.  When the PyPI check does
/// not find staleness but `gh` is `None`, we return `None`.
fn get_pypi_stale_reason(_info: &PyPIInfo, urls: &[PyPIUrl], gh: Option<&GitHubRepo>) -> Option<String> {
    if let Some(url) = urls.first() {
        if let Some(ref upload_time) = url.upload_time {
            let clean = upload_time.trim_end_matches('Z');
            if let Some(days) = days_since_date_prefix(clean) {
                if days > 730 {
                    return Some(format!("No release on PyPI in {} days — DEAD", days));
                }
                if days > 365 {
                    return Some(format!("No release on PyPI in {} days — INACTIVE", days));
                }
                if days > 180 {
                    return Some(format!("No release on PyPI in {} days — STALE", days));
                }
            }
        }
    }

    // PyPI is healthy — check cached GitHub data
    if let Some(gh) = gh {
        if let Some(days) = days_since_date_prefix(&gh.pushed_at) {
            if days > 730 {
                return Some(format!("No GitHub activity in {} days — DEAD", days));
            }
            if days > 365 {
                return Some(format!("No GitHub activity in {} days — INACTIVE", days));
            }
            if days > 180 {
                return Some(format!("No GitHub activity in {} days — STALE", days));
            }
        }
        return None;
    }

    // No GitHub data and PyPI is healthy — not stale
    None
}

// ── PyPI API ─────────────────────────────────────────────────────────

fn fetch_pypi_info(name: &str) -> Result<PyPIResponse, String> {
    let url = format!("https://pypi.org/pypi/{}/json", name);
    let resp = http_client()
        .get(&url)
        .header("User-Agent", "vigil")
        .send()
        .map_err(|e| format!("Network error: {}", e))?;

    let status = resp.status();
    let text = resp.text().map_err(|e| format!("Read error: {}", e))?;

    if !status.is_success() {
        return Err(format!("HTTP {} — {}", status, safe_prefix(&text, 200)));
    }

    serde_json::from_str(&text).map_err(|e| format!("JSON error: {}", e))
}

// ── Public entry point ───────────────────────────────────────────────

pub fn scan_pypi_deps(stale_only: bool, output_json: bool, ci: bool, licenses: bool) {
    let deps = if fs::metadata("poetry.lock").is_ok() {
        match parse_poetry_lock("poetry.lock") {
            Ok(d) => {
                if !output_json {
                    println!("📦 Found poetry.lock");
                }
                d
            }
            Err(e) => {
                eprintln!("❌ Failed to parse poetry.lock: {}", e);
                return;
            }
        }
    } else if fs::metadata("Pipfile.lock").is_ok() {
        match parse_pipfile_lock("Pipfile.lock") {
            Ok(d) => {
                if !output_json {
                    println!("📦 Found Pipfile.lock");
                }
                d
            }
            Err(e) => {
                eprintln!("❌ Failed to parse Pipfile.lock: {}", e);
                return;
            }
        }
    } else {
        eprintln!("❌ No poetry.lock or Pipfile.lock found in current directory");
        return;
    };

    if deps.is_empty() {
        if output_json {
            let output = ScanOutput {
                ecosystem: "pypi".to_string(),
                packages: vec![],
                summary: Summary::new(),
            };
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
        } else {
            println!("📦 No dependencies found in Python lockfile");
        }
        return;
    }

    if !output_json {
        println!("📦 Scanning {} Python packages from lockfile\n", deps.len());
    }

    let count_healthy = &AtomicU32::new(0);
    let count_warning = &AtomicU32::new(0);
    let count_inactive = &AtomicU32::new(0);
    let count_dead = &AtomicU32::new(0);
    let count_unknown = &AtomicU32::new(0);
    let count_cves = &AtomicU32::new(0);

    let results: Arc<Mutex<Vec<PackageResult>>> = Arc::new(Mutex::new(Vec::new()));
    let licenses_map: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));

    thread::scope(|s| {
        for (name, version) in &deps {
            let pkg_name = name.clone();
            let pkg_version = version.clone();
            let results = Arc::clone(&results);
            let licenses_map = Arc::clone(&licenses_map);
            s.spawn(move || match fetch_pypi_info(&pkg_name) {
                Ok(resp) => {
                    // Fetch GitHub info ONCE — shared by health + stale_reason
                    let gh_info: Option<GitHubRepo> = extract_github_url(&resp.info)
                        .and_then(|(owner, repo)| fetch_github_info(&owner, &repo).ok());

                    let health = get_pypi_health(&resp.info, &resp.urls, gh_info.as_ref());

                    match health {
                        "✅" => count_healthy.fetch_add(1, Ordering::Relaxed),
                        "⚠️" => count_warning.fetch_add(1, Ordering::Relaxed),
                        "🔴" => count_inactive.fetch_add(1, Ordering::Relaxed),
                        "🪦" => count_dead.fetch_add(1, Ordering::Relaxed),
                        _ => count_unknown.fetch_add(1, Ordering::Relaxed),
                    };

                    // Query OSV for known vulnerabilities
                    let vulns = osv::query_package("PyPI", &pkg_name, &pkg_version);
                    let n_cves = vulns.len() as u32;
                    if n_cves > 0 {
                        count_cves.fetch_add(n_cves, Ordering::Relaxed);
                    }

                    // Track license if --licenses is active
                    if licenses {
                        track_license(&licenses_map, resp.info.license.as_deref());
                    }

                    let stale_reason = get_pypi_stale_reason(&resp.info, &resp.urls, gh_info.as_ref());

                    if stale_only && !is_stale(health) && vulns.is_empty() {
                        return;
                    }

                    if output_json {
                        let mut r = results.lock().expect("results mutex poisoned");
                        r.push(PackageResult {
                            name: pkg_name.clone(),
                            version: pkg_version.clone(),
                            health: health_to_string(health),
                            description: resp.info.summary.clone(),
                            latest_version: Some(resp.info.version.clone()),
                            stale_reason,
                                vulns: vulns.clone(),
                                provenance: None,
                            });
                        return;
                    }

                    let desc = resp.info.summary.as_deref().unwrap_or("").to_string();

                    let mut extra = String::new();

                    if stale_only {
                        if let Some(reason) = stale_reason.as_ref() {
                            extra.push_str(&format!("\n   \x1b[90m└─ {}\x1b[0m", reason));
                        }
                    }

                    if !vulns.is_empty() {
                        let cve_ids: Vec<&str> = vulns
                            .iter()
                            .flat_map(|v| v.aliases.first().map(|a| a.as_str()).or(Some(&v.id)))
                            .take(3)
                            .collect();
                        let severity = vulns
                            .iter()
                            .filter_map(|v| v.severity.as_deref())
                            .max()
                            .unwrap_or("UNKNOWN");
                        let color = match severity {
                            "CRITICAL" | "HIGH" => "\x1b[31m",
                            "MODERATE" | "MEDIUM" => "\x1b[33m",
                            _ => "\x1b[90m",
                        };
                        extra.push_str(&format!(
                            "\n   {}🚨 {} CVE{}: {}{}",
                            color,
                            vulns.len(),
                            if vulns.len() == 1 { "" } else { "s" },
                            cve_ids.join(", "),
                            if cve_ids.len() < vulns.len() {
                                ", ..."
                            } else {
                                ""
                            },
                        ));
                        extra.push_str("\x1b[0m");
                    }

                    println!(
                        "{}{}\x1b[0m {} v{} — {} (latest: {}){}",
                        health_color(health),
                        health,
                        pkg_name,
                        pkg_version,
                        if desc.is_empty() {
                            "no description"
                        } else {
                            &desc
                        },
                        resp.info.version,
                        extra,
                    );
                }
                Err(e) => {
                    count_unknown.fetch_add(1, Ordering::Relaxed);
                    if output_json {
                        let mut r = results.lock().expect("results mutex poisoned");
                        r.push(PackageResult {
                            name: pkg_name.clone(),
                            version: pkg_version.clone(),
                            health: "unknown".to_string(),
                            description: None,
                            latest_version: None,
                            stale_reason: Some(e.clone()),
                                vulns: vec![],
                                provenance: None,
                            });
                    } else if !stale_only {
                        println!(
                            "\x1b[90m❓ {} v{} — fetch failed: {}\x1b[0m",
                            pkg_name, pkg_version, e
                        );
                    }
                }
            });
        }
    });

    let h = count_healthy.load(Ordering::Relaxed);
    let w = count_warning.load(Ordering::Relaxed);
    let i = count_inactive.load(Ordering::Relaxed);
    let d = count_dead.load(Ordering::Relaxed);
    let u = count_unknown.load(Ordering::Relaxed);
    let c = count_cves.load(Ordering::Relaxed);

    let packages = collect_results(results);

    print_summary(
        "pypi",
        output_json,
        packages,
        Summary {
            healthy: h,
            warning: w,
            hijack: 0,
            inactive: i,
            dead: d,
            unknown: u,
            cves: c,
        },
        licenses,
        Some(&licenses_map),
        ci,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_info(home_page: Option<&str>, project_urls: Option<Vec<(&str, &str)>>) -> PyPIInfo {
        PyPIInfo {
            name: "test".into(),
            version: "1.0.0".into(),
            summary: None,
            home_page: home_page.map(String::from),
            project_urls: project_urls
                .map(|v| v.into_iter().map(|(k, v)| (k.into(), v.into())).collect()),
            license: None,
        }
    }

    #[test]
    fn test_extract_github_url_from_project_urls() {
        let info = make_info(
            None,
            Some(vec![("Source", "https://github.com/owner/repo")]),
        );
        assert_eq!(
            extract_github_url(&info),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn test_extract_github_url_from_home_page() {
        let info = make_info(Some("https://github.com/owner/repo"), None);
        assert_eq!(
            extract_github_url(&info),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn test_extract_github_url_project_urls_preferred() {
        let info = make_info(
            Some("https://github.com/wrong/wrong"),
            Some(vec![("Source", "https://github.com/right/repo")]),
        );
        assert_eq!(
            extract_github_url(&info),
            Some(("right".into(), "repo".into()))
        );
    }

    #[test]
    fn test_extract_github_url_none() {
        let info = make_info(None, None);
        assert_eq!(extract_github_url(&info), None);
    }

    #[test]
    fn test_extract_github_url_not_github() {
        let info = make_info(Some("https://gitlab.com/owner/repo"), None);
        assert_eq!(extract_github_url(&info), None);
    }
}
