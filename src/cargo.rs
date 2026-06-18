use crate::api::*;
use chrono::{Utc, NaiveDate};
use serde::Deserialize;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

// ── Structs ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[allow(dead_code)]
struct CargoLock {
    version: u32,
    package: Vec<LockPackage>,
}

#[derive(Deserialize)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
}

#[derive(Deserialize)]
struct CrateResponse {
    #[serde(rename = "crate")]
    crate_data: CrateData,
    // versions is array of version IDs (integers), not objects
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct CrateData {
    name: String,
    max_stable_version: String,
    updated_at: String,
    downloads: u64,
    recent_downloads: Option<u64>,
    repository: Option<String>,
    description: Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────

fn fmt_downloads(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

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

// ── Health scoring ───────────────────────────────────────────────────

fn get_crate_health(data: &CrateData) -> &'static str {
    // Check crates.io freshness
    if let Ok(updated) = NaiveDate::parse_from_str(&data.updated_at[..10], "%Y-%m-%d") {
        let days = (Utc::now().date_naive() - updated).num_days();
        if days > 730 {
            return "🪦";
        }
        if days > 365 {
            return "🔴";
        }
        if days > 180 {
            return "⚠️";
        }
        // Under 180 days → check GitHub for finer detail
    } else {
        return "❓";
    }

    // Check GitHub activity if repo is available
    if let Some(ref repo_url) = data.repository {
        if let Some((owner, repo)) = parse_github_repo(repo_url) {
            if let Ok(gh) = fetch_github_info(&owner, &repo) {
                let pushed = &gh.pushed_at[..10];
                if let Ok(last) = NaiveDate::parse_from_str(pushed, "%Y-%m-%d") {
                    let days = (Utc::now().date_naive() - last).num_days();
                    if days > 730 {
                        return "🪦";
                    }
                    if days > 365 {
                        return "🔴";
                    }
                    if days > 180 {
                        return "⚠️";
                    }
                }
            }
        }
    }

    // Recent on crates.io → good enough
    "✅"
}

fn get_crate_stale_reason(data: &CrateData) -> Option<String> {
    // Crates.io staleness
    if let Ok(updated) = NaiveDate::parse_from_str(&data.updated_at[..10], "%Y-%m-%d") {
        let days = (Utc::now().date_naive() - updated).num_days();
        if days > 730 {
            return Some(format!("No release on crates.io in {} days — DEAD", days));
        }
        if days > 365 {
            return Some(format!("No release on crates.io in {} days", days));
        }
        if days > 180 {
            return Some(format!("No release on crates.io in {} days", days));
        }
    }

    // GitHub staleness
    if let Some(ref repo_url) = data.repository {
        if let Some((owner, repo)) = parse_github_repo(repo_url) {
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
            return Some("Not a GitHub repository".to_string());
        }
    } else {
        return Some("No upstream URL".to_string());
    }

    None
}

// ── Crates.io API ────────────────────────────────────────────────────

fn fetch_crate_info(name: &str) -> Result<CrateResponse, String> {
    let url = format!("https://crates.io/api/v1/crates/{}", name);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "watchtower")
        .send()
        .map_err(|e| format!("Network error: {}", e))?;

    let status = resp.status();
    let text = resp.text().map_err(|e| format!("Read error: {}", e))?;

    if !status.is_success() {
        return Err(format!(
            "HTTP {} — {}",
            status,
            &text[..200.min(text.len())]
        ));
    }

    serde_json::from_str(&text).map_err(|e| {
        format!(
            "JSON error: {} — body: {}",
            e,
            &text[..200.min(text.len())]
        )
    })
}

// ── Public entry point ───────────────────────────────────────────────

pub fn scan_cargo_deps(stale_only: bool) {
    // Find Cargo.lock in current directory
    let lock_path = "Cargo.lock";

    if fs::metadata(lock_path).is_err() {
        eprintln!("❌ Cargo.lock not found in current directory");
        return;
    }

    let content = match fs::read_to_string(lock_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ Failed to read Cargo.lock: {}", e);
            return;
        }
    };

    let lock: CargoLock = match toml::from_str(&content) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("❌ Failed to parse Cargo.lock: {}", e);
            return;
        }
    };

    let registry_deps: Vec<&LockPackage> = lock
        .package
        .iter()
        .filter(|p| {
            p.source
                .as_deref()
                .map_or(false, |s| s.starts_with("registry+"))
        })
        .collect();

    if registry_deps.is_empty() {
        println!("📦 No registry dependencies found in Cargo.lock");
        return;
    }

    println!(
        "📦 Scanning {} crate dependencies from Cargo.lock\n",
        registry_deps.len()
    );

    let count_healthy = &AtomicU32::new(0);
    let count_warning = &AtomicU32::new(0);
    let count_inactive = &AtomicU32::new(0);
    let count_dead = &AtomicU32::new(0);
    let count_unknown = &AtomicU32::new(0);

    thread::scope(|s| {
        for pkg in &registry_deps {
            let name = pkg.name.clone();
            let version = pkg.version.clone();
            s.spawn(move || {
                match fetch_crate_info(&name) {
                    Ok(crate_resp) => {
                        let data = &crate_resp.crate_data;
                        let health = get_crate_health(data);

                        // Tally
                        match health {
                            "✅" => {
                                count_healthy.fetch_add(1, Ordering::Relaxed);
                            }
                            "⚠️" => {
                                count_warning.fetch_add(1, Ordering::Relaxed);
                            }
                            "🔴" => {
                                count_inactive.fetch_add(1, Ordering::Relaxed);
                            }
                            "🪦" => {
                                count_dead.fetch_add(1, Ordering::Relaxed);
                            }
                            _ => {
                                count_unknown.fetch_add(1, Ordering::Relaxed);
                            }
                        }

                        if stale_only && !is_stale(health) {
                            return;
                        }

                        let downloads = fmt_downloads(data.downloads);
                        let desc = data
                            .description
                            .as_deref()
                            .unwrap_or("no description")
                            .split('.')
                            .next()
                            .unwrap_or("")
                            .to_string();

                        let stale_info = if stale_only {
                            get_crate_stale_reason(data)
                                .map(|r| format!("\n   \x1b[90m└─ {}\x1b[0m", r))
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };

                        println!(
                            "{}{}\x1b[0m {} v{} — {}, downloads: {}{}",
                            health_color(health),
                            health,
                            name,
                            version,
                            desc,
                            downloads,
                            stale_info
                        );
                    }
                    Err(e) => {
                        count_unknown.fetch_add(1, Ordering::Relaxed);
                        if !stale_only {
                            println!(
                                "\x1b[90m❓ {} v{} — fetch failed: {}\x1b[0m",
                                name, version, e
                            );
                        }
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
    println!();
    println!(
        "\x1b[1m📊 Summary:\x1b[0m \x1b[32m✅ {}\x1b[0m  \x1b[33m⚠️ {}\x1b[0m  \x1b[31m🔴 {}\x1b[0m  \x1b[31m🪦 {}\x1b[0m  \x1b[90m❓ {}\x1b[0m",
        h, w, i, d, u
    );
}
