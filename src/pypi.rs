use crate::api::*;
use crate::types::{PackageResult, ScanOutput, Summary, health_to_string};
use chrono::{Utc, NaiveDate};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, Ordering};
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
}

#[derive(Deserialize)]
struct PyPIUrl {
    upload_time: Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────

fn health_color(health: &str) -> &str {
    match health {
        "✅" => "\x1b[32m",
        "⚠️" => "\x1b[33m",
        "🔴" | "🪦" => "\x1b[31m",
        _ => "\x1b[90m",
    }
}

fn is_stale(health: &str) -> bool {
    health == "🪦" || health == "🔴" || health == "⚠️" || health == "❓"
}

fn extract_github_url(info: &PyPIInfo) -> Option<(String, String)> {
    if let Some(urls) = &info.project_urls {
        for (_key, url) in urls {
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
    Ok(lock.package.unwrap_or_default().into_iter().map(|p| (p.name, p.version)).collect())
}

fn parse_pipfile_lock(path: &str) -> Result<Vec<(String, String)>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
    let lock: PipfileLock = serde_json::from_str(&content).map_err(|e| format!("Parse error: {}", e))?;

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

fn get_pypi_health(info: &PyPIInfo, urls: &[PyPIUrl]) -> &'static str {
    if let Some(url) = urls.first() {
        if let Some(ref upload_time) = url.upload_time {
            let clean = upload_time.trim_end_matches('Z');
            if let Ok(updated) = NaiveDate::parse_from_str(&clean[..10], "%Y-%m-%d") {
                let days = (Utc::now().date_naive() - updated).num_days();
                if days > 730 { return "🪦"; }
                if days > 365 { return "🔴"; }
                if days > 180 { return "⚠️"; }
            } else {
                return "❓";
            }
        } else {
            return "❓";
        }
    } else {
        return "❓";
    }

    if let Some((owner, repo)) = extract_github_url(info) {
        if let Ok(gh) = fetch_github_info(&owner, &repo) {
            let pushed = &gh.pushed_at[..10];
            if let Ok(last) = NaiveDate::parse_from_str(pushed, "%Y-%m-%d") {
                let days = (Utc::now().date_naive() - last).num_days();
                if days > 730 { return "🪦"; }
                if days > 365 { return "🔴"; }
                if days > 180 { return "⚠️"; }
            }
        }
    }

    "✅"
}

fn get_pypi_stale_reason(info: &PyPIInfo, urls: &[PyPIUrl]) -> Option<String> {
    if let Some(url) = urls.first() {
        if let Some(ref upload_time) = url.upload_time {
            let clean = upload_time.trim_end_matches('Z');
            if let Ok(updated) = NaiveDate::parse_from_str(&clean[..10], "%Y-%m-%d") {
                let days = (Utc::now().date_naive() - updated).num_days();
                if days > 730 {
                    return Some(format!("No release on PyPI in {} days — DEAD", days));
                }
                if days > 365 {
                    return Some(format!("No release on PyPI in {} days", days));
                }
                if days > 180 {
                    return Some(format!("No release on PyPI in {} days", days));
                }
            }
        }
    }

    if let Some((owner, repo)) = extract_github_url(info) {
        match fetch_github_info(&owner, &repo) {
            Ok(gh) => {
                let pushed = &gh.pushed_at[..10];
                if let Ok(last) = NaiveDate::parse_from_str(pushed, "%Y-%m-%d") {
                    let days = (Utc::now().date_naive() - last).num_days();
                    if days > 730 {
                        return Some(format!("No GitHub activity in {} days — DEAD", days));
                    }
                    if days > 365 {
                        return Some(format!("No GitHub activity in {} days", days));
                    }
                    if days > 180 {
                        return Some(format!("No GitHub activity in {} days", days));
                    }
                }
            }
            Err(e) => return Some(format!("GitHub fetch failed: {}", e)),
        }
    } else {
        return Some("No GitHub repository found".to_string());
    }

    None
}

// ── PyPI API ─────────────────────────────────────────────────────────

fn fetch_pypi_info(name: &str) -> Result<PyPIResponse, String> {
    let url = format!("https://pypi.org/pypi/{}/json", name);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "watchtower")
        .send()
        .map_err(|e| format!("Network error: {}", e))?;

    let status = resp.status();
    let text = resp.text().map_err(|e| format!("Read error: {}", e))?;

    if !status.is_success() {
        return Err(format!("HTTP {} — {}", status, &text[..200.min(text.len())]));
    }

    serde_json::from_str(&text).map_err(|e| format!("JSON error: {}", e))
}

// ── Public entry point ───────────────────────────────────────────────

pub fn scan_pypi_deps(stale_only: bool, output_json: bool) {
    let deps = if fs::metadata("poetry.lock").is_ok() {
        match parse_poetry_lock("poetry.lock") {
            Ok(d) => {
                if !output_json { println!("📦 Found poetry.lock"); }
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
                if !output_json { println!("📦 Found Pipfile.lock"); }
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

    let results: Arc<Mutex<Vec<PackageResult>>> = Arc::new(Mutex::new(Vec::new()));

    thread::scope(|s| {
        for (name, version) in &deps {
            let pkg_name = name.clone();
            let pkg_version = version.clone();
            let results = Arc::clone(&results);
            s.spawn(move || match fetch_pypi_info(&pkg_name) {
                Ok(resp) => {
                    let health = get_pypi_health(&resp.info, &resp.urls);

                    match health {
                        "✅" => count_healthy.fetch_add(1, Ordering::Relaxed),
                        "⚠️" => count_warning.fetch_add(1, Ordering::Relaxed),
                        "🔴" => count_inactive.fetch_add(1, Ordering::Relaxed),
                        "🪦" => count_dead.fetch_add(1, Ordering::Relaxed),
                        _ => count_unknown.fetch_add(1, Ordering::Relaxed),
                    };

                    if stale_only && !is_stale(health) { return; }

                    if output_json {
                        let mut r = results.lock().unwrap();
                        r.push(PackageResult {
                            name: pkg_name.clone(),
                            version: pkg_version.clone(),
                            health: health_to_string(health),
                            description: resp.info.summary.clone(),
                            latest_version: Some(resp.info.version.clone()),
                            stale_reason: get_pypi_stale_reason(&resp.info, &resp.urls),
                        });
                        return;
                    }

                    let desc = resp.info.summary.as_deref().unwrap_or("").to_string();

                    let stale_info = if stale_only {
                        get_pypi_stale_reason(&resp.info, &resp.urls)
                            .map(|r| format!("\n   \x1b[90m└─ {}\x1b[0m", r))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };

                    println!(
                        "{}{}\x1b[0m {} v{} — {} (latest: {}){}",
                        health_color(health),
                        health,
                        pkg_name,
                        pkg_version,
                        if desc.is_empty() { "no description" } else { &desc },
                        resp.info.version,
                        stale_info,
                    );
                }
                Err(e) => {
                    count_unknown.fetch_add(1, Ordering::Relaxed);
                    if output_json {
                        let mut r = results.lock().unwrap();
                        r.push(PackageResult {
                            name: pkg_name.clone(),
                            version: pkg_version.clone(),
                            health: "unknown".to_string(),
                            description: None,
                            latest_version: None,
                            stale_reason: Some(e.clone()),
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

    if output_json {
        let packages = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
        let output = ScanOutput {
            ecosystem: "pypi".to_string(),
            packages,
            summary: Summary { healthy: h, warning: w, inactive: i, dead: d, unknown: u },
        };
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        println!();
        println!(
            "\x1b[1m📊 Summary:\x1b[0m \x1b[32m✅ {}\x1b[0m  \x1b[33m⚠️ {}\x1b[0m  \x1b[31m🔴 {}\x1b[0m  \x1b[31m🪦 {}\x1b[0m  \x1b[90m❓ {}\x1b[0m",
            h, w, i, d, u
        );
    }
}
