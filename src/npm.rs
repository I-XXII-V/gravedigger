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
struct NpmLock {
    packages: Option<HashMap<String, NpmPkg>>,
    dependencies: Option<HashMap<String, NpmDep>>,
}

#[derive(Deserialize)]
struct NpmPkg {
    version: Option<String>,
}

#[derive(Deserialize)]
struct NpmDep {
    version: String,
}

#[derive(Deserialize)]
struct NpmRegistryResponse {
    #[serde(rename = "dist-tags")]
    dist_tags: HashMap<String, String>,
    time: HashMap<String, String>,
    repository: Option<NpmRepo>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct NpmRepo {
    url: Option<String>,
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

fn clean_github_url(raw: &str) -> &str {
    let s = raw.trim_start_matches("git+");
    s.trim_end_matches(".git")
}

fn extract_npm_deps(lock: &NpmLock) -> Vec<(String, String)> {
    let mut deps = Vec::new();

    if let Some(packages) = &lock.packages {
        let mut seen = std::collections::HashSet::new();
        for (path, info) in packages {
            if path.is_empty() { continue; }
            if let Some(version) = &info.version {
                let name = path.trim_start_matches("node_modules/");
                if seen.insert(name.to_string()) {
                    deps.push((name.to_string(), version.clone()));
                }
            }
        }
        return deps;
    }

    if let Some(deps_map) = &lock.dependencies {
        for (name, info) in deps_map {
            deps.push((name.clone(), info.version.clone()));
        }
    }

    deps
}

// ── Health scoring ───────────────────────────────────────────────────

fn get_npm_health(data: &NpmRegistryResponse) -> &'static str {
    if let Some(modified) = data.time.get("modified") {
        if let Ok(updated) = NaiveDate::parse_from_str(&modified[..10], "%Y-%m-%d") {
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

    if let Some(repo) = &data.repository {
        if let Some(ref url) = repo.url {
            let clean = clean_github_url(url);
            if let Some((owner, repo_name)) = parse_github_repo(clean) {
                if let Ok(gh) = fetch_github_info(&owner, &repo_name) {
                    let pushed = &gh.pushed_at[..10];
                    if let Ok(last) = NaiveDate::parse_from_str(pushed, "%Y-%m-%d") {
                        let days = (Utc::now().date_naive() - last).num_days();
                        if days > 730 { return "🪦"; }
                        if days > 365 { return "🔴"; }
                        if days > 180 { return "⚠️"; }
                    }
                }
            }
        }
    }

    "✅"
}

fn get_npm_stale_reason(data: &NpmRegistryResponse) -> Option<String> {
    if let Some(modified) = data.time.get("modified") {
        if let Ok(updated) = NaiveDate::parse_from_str(&modified[..10], "%Y-%m-%d") {
            let days = (Utc::now().date_naive() - updated).num_days();
            if days > 730 {
                return Some(format!("No update on npm in {} days — DEAD", days));
            }
            if days > 365 {
                return Some(format!("No update on npm in {} days", days));
            }
            if days > 180 {
                return Some(format!("No update on npm in {} days", days));
            }
        }
    }

    if let Some(repo) = &data.repository {
        if let Some(ref url) = repo.url {
            let clean = clean_github_url(url);
            if let Some((owner, repo_name)) = parse_github_repo(clean) {
                match fetch_github_info(&owner, &repo_name) {
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
                return Some("Not a GitHub repository".to_string());
            }
        }
    } else {
        return Some("No repository URL".to_string());
    }

    None
}

// ── npm registry API ────────────────────────────────────────────────

fn fetch_npm_info(name: &str) -> Result<NpmRegistryResponse, String> {
    let encoded = name.replace('/', "%2F");
    let url = format!("https://registry.npmjs.org/{}", encoded);

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

    serde_json::from_str(&text).map_err(|e| format!("JSON error: {} — body: {}", e, &text[..200.min(text.len())]))
}

// ── Public entry point ───────────────────────────────────────────────

pub fn scan_npm_deps(stale_only: bool, output_json: bool) {
    let lock_path = "package-lock.json";

    if fs::metadata(lock_path).is_err() {
        eprintln!("❌ package-lock.json not found in current directory");
        return;
    }

    let content = match fs::read_to_string(lock_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ Failed to read package-lock.json: {}", e);
            return;
        }
    };

    let lock: NpmLock = match serde_json::from_str(&content) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("❌ Failed to parse package-lock.json: {}", e);
            return;
        }
    };

    let deps = extract_npm_deps(&lock);

    if deps.is_empty() {
        if output_json {
            let output = ScanOutput {
                ecosystem: "npm".to_string(),
                packages: vec![],
                summary: Summary::new(),
            };
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
        } else {
            println!("📦 No dependencies found in package-lock.json");
        }
        return;
    }

    if !output_json {
        println!("📦 Scanning {} npm packages from package-lock.json\n", deps.len());
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
            s.spawn(move || match fetch_npm_info(&pkg_name) {
                Ok(reg) => {
                    let health = get_npm_health(&reg);

                    match health {
                        "✅" => { count_healthy.fetch_add(1, Ordering::Relaxed); }
                        "⚠️" => { count_warning.fetch_add(1, Ordering::Relaxed); }
                        "🔴" => { count_inactive.fetch_add(1, Ordering::Relaxed); }
                        "🪦" => { count_dead.fetch_add(1, Ordering::Relaxed); }
                        _ => { count_unknown.fetch_add(1, Ordering::Relaxed); }
                    }

                    if stale_only && !is_stale(health) { return; }

                    if output_json {
                        let mut r = results.lock().unwrap();
                        r.push(PackageResult {
                            name: pkg_name.clone(),
                            version: pkg_version.clone(),
                            health: health_to_string(health),
                            description: reg.description.clone(),
                            latest_version: reg.dist_tags.get("latest").cloned(),
                            stale_reason: get_npm_stale_reason(&reg),
                        });
                        return;
                    }

                    let desc = reg
                        .description
                        .as_deref()
                        .unwrap_or("")
                        .split('.')
                        .next()
                        .unwrap_or("")
                        .to_string();

                    let latest = reg
                        .dist_tags
                        .get("latest")
                        .map(|v| v.as_str())
                        .unwrap_or("?");

                    let stale_info = if stale_only {
                        get_npm_stale_reason(&reg)
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
                        latest,
                        stale_info
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
            ecosystem: "npm".to_string(),
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
