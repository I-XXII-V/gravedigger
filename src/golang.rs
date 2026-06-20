use crate::api::{fetch_github_info, http_client, safe_prefix, GitHubRepo};
use crate::display::{health_color, is_stale};
use crate::osv;
use crate::types::{
    collect_results, days_since_date_prefix, health_to_string, print_summary, score_from_days,
    PackageResult, ScanOutput, Summary,
};
use serde::Deserialize;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

// ── Structs ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct GoProxyResponse {
    #[allow(dead_code)]
    Version: String,
    Time: String,
}

// ── Helpers ──────────────────────────────────────────────────────────

fn parse_go_mod(path: &str) -> Result<Vec<(String, String)>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
    parse_go_mod_lines(&content)
}

fn parse_go_mod_lines(content: &str) -> Result<Vec<(String, String)>, String> {
    let mut deps = Vec::new();
    let mut in_block = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with("module ")
            || line.starts_with("go ")
        {
            continue;
        }
        if line == "require (" {
            in_block = true;
            continue;
        }
        if line == ")" {
            in_block = false;
            continue;
        }
        if in_block || line.starts_with("require ") {
            let parts: Vec<&str> = line
                .split_whitespace()
                .filter(|p| !p.is_empty() && *p != "require" && !p.starts_with("//"))
                .collect();
            if parts.len() >= 2 {
                let name = parts[0].to_string();
                let version = parts[1].trim_start_matches('v').to_string();
                deps.push((name, version));
            }
        }
    }
    Ok(deps)
}

fn go_mod_to_github(mod_path: &str) -> Option<(String, String)> {
    if !mod_path.starts_with("github.com/") {
        return None;
    }
    let parts: Vec<&str> = mod_path.split('/').collect();
    if parts.len() < 3 {
        return None;
    }
    Some((parts[1].to_string(), parts[2].to_string()))
}

// ── Health scoring ───────────────────────────────────────────────────

fn get_go_health(proxy: &GoProxyResponse) -> &'static str {
    days_since_date_prefix(&proxy.Time)
        .map(score_from_days)
        .unwrap_or("❓")
}

/// Check if a Go module has been hijacked on GitHub.
/// Accepts cached `gh` data to avoid a second GitHub API call.
/// `gh_result` is `Ok(gh)` if the fetch succeeded, `Err(msg)` if it failed.
fn get_go_hijack(mod_path: &str, gh_result: Option<Result<&GitHubRepo, &str>>) -> Option<String> {
    let (_owner, _repo) = go_mod_to_github(mod_path)?;

    match gh_result {
        Some(Ok(gh)) => {
            if gh.archived {
                Some("Repo is archived — may be hijacked".to_string())
            } else {
                None
            }
        }
        Some(Err(e)) => {
            if e.contains("404") {
                Some("GitHub repo not found (404) — module path may be hijacked".to_string())
            } else {
                None
            }
        }
        None => None, // Non-GitHub module; no hijack risk
    }
}

fn get_go_stale_reason(proxy: &GoProxyResponse, gh: Option<&GitHubRepo>) -> Option<String> {
    if let Some(days) = days_since_date_prefix(&proxy.Time) {
        if days > 730 {
            return Some(format!("No release in {} days — DEAD", days));
        }
        if days > 365 {
            return Some(format!("No release in {} days — INACTIVE", days));
        }
        if days > 180 {
            return Some(format!("No release in {} days — STALE", days));
        }
    }

    // Go proxy is healthy — check cached GitHub data
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

    // No GitHub data and Go proxy is healthy — not stale
    None
}

// ── Go proxy API ─────────────────────────────────────────────────────

fn fetch_go_proxy(mod_path: &str) -> Result<GoProxyResponse, String> {
    let encoded = mod_path.replace('/', "%2F");
    let url = format!("https://proxy.golang.org/{}/@latest", encoded);

    let resp = http_client()
        .get(&url)
        .header("User-Agent", "watchtower")
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

pub fn scan_go_deps(stale_only: bool, output_json: bool, ci: bool, licenses: bool) {
    if fs::metadata("go.mod").is_err() {
        eprintln!("❌ go.mod not found in current directory");
        return;
    }

    if licenses && !output_json {
        eprintln!("⚠️  --licenses is not supported for Go modules (go.mod has no license metadata)");
    }

    let deps = match parse_go_mod("go.mod") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("❌ Failed to parse go.mod: {}", e);
            return;
        }
    };

    if deps.is_empty() {
        if output_json {
            let output = ScanOutput {
                ecosystem: "go".to_string(),
                packages: vec![],
                summary: Summary::new(),
            };
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
        } else {
            println!("📦 No dependencies found in go.mod");
        }
        return;
    }

    if !output_json {
        println!("📦 Scanning {} Go modules from go.mod\n", deps.len());
    }

    let count_healthy = &AtomicU32::new(0);
    let count_warning = &AtomicU32::new(0);
    let count_hijack = &AtomicU32::new(0);
    let count_inactive = &AtomicU32::new(0);
    let count_dead = &AtomicU32::new(0);
    let count_unknown = &AtomicU32::new(0);
    let count_cves = &AtomicU32::new(0);

    let results: Arc<Mutex<Vec<PackageResult>>> = Arc::new(Mutex::new(Vec::new()));

    thread::scope(|s| {
        for (name, version) in &deps {
            let mod_name = name.clone();
            let mod_version = version.clone();
            let results = Arc::clone(&results);
            s.spawn(move || match fetch_go_proxy(&mod_name) {
                Ok(proxy) => {
                    // Fetch GitHub info ONCE for GitHub-hosted Go modules
                    let gh_result: Option<Result<GitHubRepo, String>> =
                        go_mod_to_github(&mod_name)
                            .map(|(owner, repo)| fetch_github_info(&owner, &repo));

                    let gh_ref = gh_result.as_ref().and_then(|r| r.as_ref().ok());

                    let proxy_health = get_go_health(&proxy);

                    // Hijack check shares the same GitHub data
                    let hijack = get_go_hijack(&mod_name, gh_result.as_ref().map(|r| {
                        r.as_ref().map_err(|e| e.as_str())
                    }));
                    let health = if hijack.is_some() { "🚩" } else { proxy_health };

                    match health {
                        "✅" => count_healthy.fetch_add(1, Ordering::Relaxed),
                        "⚠️" => count_warning.fetch_add(1, Ordering::Relaxed),
                        "🚩" => count_hijack.fetch_add(1, Ordering::Relaxed),
                        "🔴" => count_inactive.fetch_add(1, Ordering::Relaxed),
                        "🪦" => count_dead.fetch_add(1, Ordering::Relaxed),
                        _ => count_unknown.fetch_add(1, Ordering::Relaxed),
                    };

                    // Query OSV for known vulnerabilities
                    let vulns = osv::query_package("Go", &mod_name, &mod_version);
                    let n_cves = vulns.len() as u32;
                    if n_cves > 0 {
                        count_cves.fetch_add(n_cves, Ordering::Relaxed);
                    }

                    if stale_only && !is_stale(health) && vulns.is_empty() {
                        return;
                    }

                    // Stale reason uses cached gh_ref — no second GitHub call
                    let stale_reason_raw = get_go_stale_reason(&proxy, gh_ref);

                    if output_json {
                        let sr = hijack.clone().or(stale_reason_raw);
                        let mut r = results.lock().unwrap();
                        r.push(PackageResult {
                            name: mod_name.clone(),
                            version: mod_version.clone(),
                            health: health_to_string(health),
                            description: None,
                            latest_version: Some(proxy.Version.trim_start_matches('v').to_string()),
                            stale_reason: sr,
                                vulns: vulns.clone(),
                                provenance: None,
                            });
                        return;
                    }

                    let latest = proxy.Version.trim_start_matches('v');

                    let mut extra = String::new();

                    // Show hijack reason (always, not just --stale)
                    if let Some(reason) = &hijack {
                        extra.push_str(&format!("\n   \x1b[33m└─ 🚩 {}\x1b[0m", reason));
                    }

                    if stale_only {
                        if let Some(reason) = stale_reason_raw.as_ref() {
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
                        "{}{}\x1b[0m {} v{} (latest: {}){}",
                        health_color(health),
                        health,
                        mod_name,
                        mod_version,
                        latest,
                        extra,
                    );
                }
                Err(e) => {
                    count_unknown.fetch_add(1, Ordering::Relaxed);
                    if output_json {
                        let mut r = results.lock().unwrap();
                        r.push(PackageResult {
                            name: mod_name.clone(),
                            version: mod_version.clone(),
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
                            mod_name, mod_version, e
                        );
                    }
                }
            });
        }
    });

    let h = count_healthy.load(Ordering::Relaxed);
    let w = count_warning.load(Ordering::Relaxed);
    let j = count_hijack.load(Ordering::Relaxed);
    let i = count_inactive.load(Ordering::Relaxed);
    let d = count_dead.load(Ordering::Relaxed);
    let u = count_unknown.load(Ordering::Relaxed);
    let c = count_cves.load(Ordering::Relaxed);

    let packages = collect_results(results);

    print_summary(
        "go",
        output_json,
        packages,
        Summary {
            healthy: h,
            warning: w,
            hijack: j,
            inactive: i,
            dead: d,
            unknown: u,
            cves: c,
        },
        false, // golang doesn't track licenses
        None,
        ci,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_mod_to_github_simple() {
        let result = go_mod_to_github("github.com/owner/repo");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn test_go_mod_to_github_with_subpackage() {
        let result = go_mod_to_github("github.com/owner/repo/subpkg");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn test_go_mod_to_github_not_github() {
        let result = go_mod_to_github("gitlab.com/owner/repo");
        assert_eq!(result, None);
    }

    #[test]
    fn test_go_mod_to_github_too_short() {
        let result = go_mod_to_github("github.com/owner");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_go_mod_require_block() {
        let content = r#"module example.com/m

go 1.21

require (
    github.com/foo/bar v1.0.0
    github.com/baz/qux v2.0.0
)
"#;
        let deps = parse_go_mod_lines(content).unwrap();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&("github.com/foo/bar".into(), "1.0.0".into())));
        assert!(deps.contains(&("github.com/baz/qux".into(), "2.0.0".into())));
    }

    #[test]
    fn test_parse_go_mod_single_require() {
        let content = r#"module example.com/m

go 1.21

require github.com/foo/bar v1.0.0
"#;
        let deps = parse_go_mod_lines(content).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0], ("github.com/foo/bar".into(), "1.0.0".into()));
    }

    #[test]
    fn test_parse_go_mod_empty() {
        let content = r#"module example.com/m

go 1.21
"#;
        let deps = parse_go_mod_lines(content).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn test_parse_go_mod_indirect() {
        let content = r#"module example.com/m

go 1.21

require (
    github.com/foo/bar v1.0.0 // indirect
    github.com/baz/qux v2.0.0
)
"#;
        let deps = parse_go_mod_lines(content).unwrap();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&("github.com/foo/bar".into(), "1.0.0".into())));
        assert!(deps.contains(&("github.com/baz/qux".into(), "2.0.0".into())));
    }

    #[test]
    fn test_get_go_hijack_non_github() {
        // Non-GitHub modules can't be hijack-checked — pass None for gh_result
        assert_eq!(get_go_hijack("gitlab.com/owner/repo", None), None);
        assert_eq!(get_go_hijack("bitbucket.org/owner/repo", None), None);
        assert_eq!(get_go_hijack("example.com/module", None), None);
    }

    #[test]
    fn test_get_go_hijack_too_short() {
        // Module path too short to extract owner/repo
        assert_eq!(get_go_hijack("github.com/owner", None), None);
    }
}
