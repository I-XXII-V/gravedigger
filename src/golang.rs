use crate::api::*;
use chrono::{Utc, NaiveDate};
use serde::Deserialize;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
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

/// Parse go.mod and extract required modules with versions
fn parse_go_mod(path: &str) -> Result<Vec<(String, String)>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
    let mut deps = Vec::new();
    let mut in_block = false;

    for line in content.lines() {
        let line = line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with("//") || line.starts_with("module ") || line.starts_with("go ") {
            continue;
        }

        // Handle require block
        if line == "require (" {
            in_block = true;
            continue;
        }
        if line == ")" {
            in_block = false;
            continue;
        }

        // Single require line: require github.com/foo/bar v1.0.0
        // Or inside block: github.com/foo/bar v1.0.0
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

/// Extract owner/repo from a Go module path (for GitHub-hosted modules)
fn go_mod_to_github(mod_path: &str) -> Option<(String, String)> {
    // Go module paths like: github.com/owner/repo, github.com/owner/repo/subpkg
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
    if let Ok(updated) = NaiveDate::parse_from_str(&proxy.Time[..10], "%Y-%m-%d") {
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
    } else {
        return "❓";
    }
    "✅"
}

fn get_go_stale_reason(proxy: &GoProxyResponse, mod_path: &str) -> Option<String> {
    if let Ok(updated) = NaiveDate::parse_from_str(&proxy.Time[..10], "%Y-%m-%d") {
        let days = (Utc::now().date_naive() - updated).num_days();
        if days > 730 {
            return Some(format!("No release in {} days — DEAD", days));
        }
        if days > 365 {
            return Some(format!("No release in {} days", days));
        }
        if days > 180 {
            return Some(format!("No release in {} days", days));
        }
    }

    // Check GitHub for Go modules hosted on GitHub
    if let Some((owner, repo)) = go_mod_to_github(mod_path) {
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
    }

    None
}

// ── Go proxy API ─────────────────────────────────────────────────────

fn fetch_go_proxy(mod_path: &str) -> Result<GoProxyResponse, String> {
    // URL-encode: / becomes %2F
    let encoded = mod_path.replace('/', "%2F");
    let url = format!("https://proxy.golang.org/{}/@latest", encoded);

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

    serde_json::from_str(&text)
        .map_err(|e| format!("JSON error: {}", e))
}

// ── Public entry point ───────────────────────────────────────────────

pub fn scan_go_deps(stale_only: bool) {
    if fs::metadata("go.mod").is_err() {
        eprintln!("❌ go.mod not found in current directory");
        return;
    }

    let deps = match parse_go_mod("go.mod") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("❌ Failed to parse go.mod: {}", e);
            return;
        }
    };

    if deps.is_empty() {
        println!("📦 No dependencies found in go.mod");
        return;
    }

    println!("📦 Scanning {} Go modules from go.mod\n", deps.len());

    let count_healthy = &AtomicU32::new(0);
    let count_warning = &AtomicU32::new(0);
    let count_inactive = &AtomicU32::new(0);
    let count_dead = &AtomicU32::new(0);
    let count_unknown = &AtomicU32::new(0);

    thread::scope(|s| {
        for (name, version) in &deps {
            let mod_name = name.clone();
            let mod_version = version.clone();
            s.spawn(move || match fetch_go_proxy(&mod_name) {
                Ok(proxy) => {
                    let health = get_go_health(&proxy);

                    match health {
                        "✅" => count_healthy.fetch_add(1, Ordering::Relaxed),
                        "⚠️" => count_warning.fetch_add(1, Ordering::Relaxed),
                        "🔴" => count_inactive.fetch_add(1, Ordering::Relaxed),
                        "🪦" => count_dead.fetch_add(1, Ordering::Relaxed),
                        _ => count_unknown.fetch_add(1, Ordering::Relaxed),
                    };

                    if stale_only && !is_stale(health) {
                        return;
                    }

                    let latest = proxy.Version.trim_start_matches('v');

                    let stale_info = if stale_only {
                        get_go_stale_reason(&proxy, &mod_name)
                            .map(|r| format!("\n   \x1b[90m└─ {}\x1b[0m", r))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };

                    println!(
                        "{}{}\x1b[0m {} v{} (latest: {}){}",
                        health_color(health),
                        health,
                        mod_name,
                        mod_version,
                        latest,
                        stale_info,
                    );
                }
                Err(e) => {
                    count_unknown.fetch_add(1, Ordering::Relaxed);
                    if !stale_only {
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
    let i = count_inactive.load(Ordering::Relaxed);
    let d = count_dead.load(Ordering::Relaxed);
    let u = count_unknown.load(Ordering::Relaxed);
    println!();
    println!(
        "\x1b[1m📊 Summary:\x1b[0m \x1b[32m✅ {}\x1b[0m  \x1b[33m⚠️ {}\x1b[0m  \x1b[31m🔴 {}\x1b[0m  \x1b[31m🪦 {}\x1b[0m  \x1b[90m❓ {}\x1b[0m",
        h, w, i, d, u
    );
}
