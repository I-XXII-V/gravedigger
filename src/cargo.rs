use crate::api::*;
use crate::display::{fmt_downloads, health_color, is_stale};
use crate::osv;
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
    license: Option<String>,
}

// ── Health scoring ───────────────────────────────────────────────────

fn get_crate_health(data: &CrateData) -> &'static str {
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
    } else {
        return "❓";
    }

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

    "✅"
}

fn get_crate_stale_reason(data: &CrateData) -> Option<String> {
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

pub fn scan_cargo_deps(stale_only: bool, output_json: bool, ci: bool, licenses: bool) {
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
                .is_some_and(|s| s.starts_with("registry+"))
        })
        .collect();

    if registry_deps.is_empty() {
        if output_json {
            let output = ScanOutput {
                ecosystem: "cargo".to_string(),
                packages: vec![],
                summary: Summary::new(),
            };
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
        } else {
            println!("📦 No registry dependencies found in Cargo.lock");
        }
        return;
    }

    if !output_json {
        println!("📦 Scanning {} crate dependencies from Cargo.lock\n", registry_deps.len());
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
        for pkg in &registry_deps {
            let name = pkg.name.clone();
            let version = pkg.version.clone();
            let results = Arc::clone(&results);
            let licenses_map = Arc::clone(&licenses_map);
            s.spawn(move || {
                match fetch_crate_info(&name) {
                    Ok(crate_resp) => {
                        let data = &crate_resp.crate_data;
                        let health = get_crate_health(data);

                        match health {
                            "✅" => { count_healthy.fetch_add(1, Ordering::Relaxed); }
                            "⚠️" => { count_warning.fetch_add(1, Ordering::Relaxed); }
                            "🔴" => { count_inactive.fetch_add(1, Ordering::Relaxed); }
                            "🪦" => { count_dead.fetch_add(1, Ordering::Relaxed); }
                            _ => { count_unknown.fetch_add(1, Ordering::Relaxed); }
                        }

                        // Query OSV for known vulnerabilities
                        let vulns = osv::query_package("crates.io", &name, &version);
                        let n_cves = vulns.len() as u32;
                        if n_cves > 0 {
                            count_cves.fetch_add(n_cves, Ordering::Relaxed);
                        }

                        // Track license if --licenses is active
                        if licenses {
                            if let Some(ref lic) = data.license {
                                let mut lm = licenses_map.lock().unwrap();
                                *lm.entry(if lic.is_empty() { "Unknown".into() } else { lic.clone() }).or_insert(0) += 1;
                            } else {
                                let mut lm = licenses_map.lock().unwrap();
                                *lm.entry("Unknown".into()).or_insert(0) += 1;
                            }
                        }

                        // Show if stale OR has CVEs (when --stale is active)
                        if stale_only && !is_stale(health) && vulns.is_empty() { return; }

                        // JSON output
                        if output_json {
                            let mut r = results.lock().unwrap();
                            r.push(PackageResult {
                                name: name.clone(),
                                version: version.clone(),
                                health: health_to_string(health),
                                description: data.description.clone(),
                                latest_version: Some(data.max_stable_version.clone()),
                                stale_reason: get_crate_stale_reason(data),
                                vulns: vulns.clone(),
                            });
                            return;
                        }

                        // Text output
                        let downloads = fmt_downloads(data.downloads);
                        let desc = data
                            .description
                            .as_deref()
                            .unwrap_or("no description")
                            .split('.')
                            .next()
                            .unwrap_or("")
                            .to_string();

                        let mut extra = String::new();

                        if stale_only {
                            if let Some(reason) = get_crate_stale_reason(data) {
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
                                if cve_ids.len() < vulns.len() { ", ..." } else { "" },
                            ));
                            extra.push_str("\x1b[0m");
                        }

                        println!(
                            "{}{}\x1b[0m {} v{} — {}, downloads: {}{}",
                            health_color(health),
                            health,
                            name,
                            version,
                            desc,
                            downloads,
                            extra,
                        );
                    }
                    Err(e) => {
                        count_unknown.fetch_add(1, Ordering::Relaxed);
                        if output_json {
                            let mut r = results.lock().unwrap();
                            r.push(PackageResult {
                                name: name.clone(),
                                version: version.clone(),
                                health: "unknown".to_string(),
                                description: None,
                                latest_version: None,
                                stale_reason: Some(e.clone()),
                                vulns: vec![],
                            });
                        } else if !stale_only {
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
    let c = count_cves.load(Ordering::Relaxed);

    if output_json {
        let packages = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
        let output = ScanOutput {
            ecosystem: "cargo".to_string(),
            packages,
            summary: Summary { healthy: h, warning: w, hijack: 0, inactive: i, dead: d, unknown: u, cves: c },
        };
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        println!();
        let cve_part = if c > 0 {
            format!("  \x1b[31m🚨 {}\x1b[0m", c)
        } else {
            String::new()
        };
        println!(
            "\x1b[1m📊 Summary:\x1b[0m \x1b[32m✅ {}\x1b[0m  \x1b[33m⚠️ {}\x1b[0m  \x1b[31m🔴 {}\x1b[0m  \x1b[31m🪦 {}\x1b[0m  \x1b[90m❓ {}\x1b[0m{}",
            h, w, i, d, u, cve_part
        );
    }

    if licenses {
        let map = licenses_map.lock().unwrap();
        let mut sorted: Vec<(String, u32)> = map.iter().map(|(k, v)| (k.clone(), *v)).collect();
        sorted.sort_by_key(|b| std::cmp::Reverse(b.1));
        let total: u32 = map.values().sum();
        println!("\n\x1b[1m📋 Licenses:\x1b[0m");
        for (name, count) in &sorted {
            let pct = (*count as f64 / total as f64) * 100.0;
            println!("   \x1b[90m{:20}\x1b[0m {} ({:.0}%)", name, count, pct);
        }
    }

    if ci && (d > 0 || c > 0) {
        std::process::exit(1);
    }
}
