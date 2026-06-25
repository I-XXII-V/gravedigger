use crate::api::{fetch_github_info, http_client, parse_github_repo, safe_prefix, GitHubRepo};
use crate::display::{fmt_downloads, health_color, health_sort_key, is_stale, DisplayEntry};
use crate::osv;
use crate::types::{
    collect_results, days_since_date_prefix, health_to_string, print_summary, score_from_days,
    track_license, PackageResult, ScanOutput, Summary,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
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

/// Health scoring for crates.io packages.
///
/// Strategy:
/// 1. Check crates.io `updated_at` — if stale (⚠️/🔴/🪦), return immediately.
/// 2. If crates.io says ✅ (fresh), also check GitHub `pushed_at` for a finer-
///    grained score. A crate may have a recent release but an abandoned repo.
/// 3. `gh` is an optional cached `GitHubRepo` — pass `None` to skip the
///    GitHub check entirely (no second API call).
fn get_crate_health(data: &CrateData, gh: Option<&GitHubRepo>) -> &'static str {
    if let Some(days) = days_since_date_prefix(&data.updated_at) {
        let health = score_from_days(days);
        if health != "✅" {
            return health;
        }
    } else {
        return "❓";
    }

    // Registry says fresh — use cached GitHub data
    if let Some(gh) = gh && let Some(days) = days_since_date_prefix(&gh.pushed_at) {
        return score_from_days(days);
    }

    "✅"
}

/// Stale reason for crates.io packages.
///
/// Returns `None` when the package is fully healthy (both registry and
/// GitHub are active).  This prevents false positives where a healthy
/// package with no repository URL would get "No upstream URL" as a stale
/// reason.
///
/// `gh` is an optional cached `GitHubRepo`.  When the registry check does
/// not find staleness but `gh` is `None`, we return `None` (the absence
/// of a repo URL is not a health problem).
fn get_crate_stale_reason(data: &CrateData, gh: Option<&GitHubRepo>) -> Option<String> {
    if let Some(days) = days_since_date_prefix(&data.updated_at) {
        if days > 730 {
            return Some(format!("No release on crates.io in {} days — DEAD", days));
        }
        if days > 365 {
            return Some(format!(
                "No release on crates.io in {} days — INACTIVE",
                days
            ));
        }
        if days > 180 {
            return Some(format!("No release on crates.io in {} days — STALE", days));
        }
    }

    // Registry is healthy — check cached GitHub data
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
        // gh is present and ≤180 days → healthy
        return None;
    }

    // No GitHub data and registry is healthy — not stale
    None
}

// ── Crates.io API ────────────────────────────────────────────────────

fn fetch_crate_info(name: &str) -> Result<CrateResponse, String> {
    let url = format!("https://crates.io/api/v1/crates/{}", name);
    let resp = http_client()
        .get(&url)
        .header("User-Agent", "gravedigger")
        .send()
        .map_err(|e| format!("Network error: {}", e))?;

    let status = resp.status();
    let text = resp.text().map_err(|e| format!("Read error: {}", e))?;

    if !status.is_success() {
        return Err(format!("HTTP {} — {}", status, safe_prefix(&text, 200)));
    }

    serde_json::from_str(&text)
        .map_err(|e| format!("JSON error: {} — body: {}", e, safe_prefix(&text, 200)))
}

// ── Public parser ────────────────────────────────────────────────────

/// Parse a Cargo.lock content string and return registry dependencies
/// as `(name, version)` pairs. Non-registry sources (git, path) are skipped.
pub fn parse_cargo_lock(content: &str) -> Result<Vec<(String, String)>, String> {
    let lock: CargoLock = toml::from_str(content)
        .map_err(|e| format!("Failed to parse Cargo.lock: {}", e))?;
    Ok(lock
        .package
        .into_iter()
        .filter(|p| {
            p.source
                .as_deref()
                .is_some_and(|s| s.starts_with("registry+"))
        })
        .map(|p| (p.name, p.version))
        .collect())
}

/// Scan a single crate dependency and return its health result directly.
/// Combines fetch + health scoring + OSV query in one call.
pub(crate) fn scan_single(name: &str, version: &str) -> PackageResult {
    match fetch_crate_info(name) {
        Ok(crate_resp) => {
            let data = &crate_resp.crate_data;

            let gh_info: Option<GitHubRepo> = data
                .repository
                .as_deref()
                .and_then(|url| {
                    let (owner, repo) = parse_github_repo(url)?;
                    fetch_github_info(&owner, &repo).ok()
                });

            let health = get_crate_health(data, gh_info.as_ref());
            let stale_reason = get_crate_stale_reason(data, gh_info.as_ref());
            let vulns = osv::query_package("crates.io", name, version);

            PackageResult {
                name: name.to_string(),
                version: version.to_string(),
                health: health_to_string(health),
                description: data.description.clone(),
                latest_version: Some(data.max_stable_version.clone()),
                stale_reason,
                vulns,
                provenance: None,
            }
        }
        Err(e) => PackageResult {
            name: name.to_string(),
            version: version.to_string(),
            health: "unknown".to_string(),
            description: None,
            latest_version: None,
            stale_reason: Some(e),
            vulns: vec![],
            provenance: None,
        },
    }
}

// ── Public entry point ───────────────────────────────────────────────

pub fn scan_cargo_deps(stale_only: bool, output_json: bool, ci: bool, licenses: bool, verbose: bool, sbom: bool) {
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

    let registry_deps = match parse_cargo_lock(&content) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("❌ {}", e);
            return;
        }
    };

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

    if !output_json && !sbom {
        println!(
            "📦 Scanning {} crate dependencies from Cargo.lock\n",
            registry_deps.len()
        );
    }

    let count_healthy = &AtomicU32::new(0);
    let count_warning = &AtomicU32::new(0);
    let count_inactive = &AtomicU32::new(0);
    let count_dead = &AtomicU32::new(0);
    let count_unknown = &AtomicU32::new(0);
    let count_cves = &AtomicU32::new(0);

    let results: Arc<Mutex<Vec<PackageResult>>> = Arc::new(Mutex::new(Vec::new()));
    let text_lines: Arc<Mutex<Vec<DisplayEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let licenses_map: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));

    thread::scope(|s| {
        for (pkg_name, pkg_version) in &registry_deps {
            let name = pkg_name.clone();
            let version = pkg_version.clone();
            let results = Arc::clone(&results);
            let text_lines = Arc::clone(&text_lines);
            let licenses_map = Arc::clone(&licenses_map);
            s.spawn(move || {
                match fetch_crate_info(&name) {
                    Ok(crate_resp) => {
                        let data = &crate_resp.crate_data;

                        // Fetch GitHub info ONCE — shared by health + stale_reason
                        let gh_info: Option<GitHubRepo> = data
                            .repository
                            .as_deref()
                            .and_then(|url| {
                                let (owner, repo) = parse_github_repo(url)?;
                                fetch_github_info(&owner, &repo).ok()
                            });

                        let health = get_crate_health(data, gh_info.as_ref());

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

                        // Query OSV for known vulnerabilities
                        let vulns = osv::query_package("crates.io", &name, &version);
                        let n_cves = vulns.len() as u32;
                        if n_cves > 0 {
                            count_cves.fetch_add(n_cves, Ordering::Relaxed);
                        }

                        // Track license if --licenses is active
                        if licenses {
                            track_license(&licenses_map, data.license.as_deref());
                        }

                        // Stale reason uses cached gh_info — no second API call
                        let stale_reason = get_crate_stale_reason(data, gh_info.as_ref());

                        // Show if stale OR has CVEs (when --stale is active)
                        if stale_only && !is_stale(health) && vulns.is_empty() {
                            return;
                        }

                        // JSON or SBOM output — populate results
                        if output_json || sbom {
                            let mut r = results.lock().expect("results mutex poisoned");
                            r.push(PackageResult {
                                name: name.clone(),
                                version: version.clone(),
                                health: health_to_string(health),
                                description: data.description.clone(),
                                latest_version: Some(data.max_stable_version.clone()),
                                stale_reason: stale_reason.clone(),
                                vulns: vulns.clone(),
                                provenance: None,
                            });
                            if output_json {
                                return;
                            }
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

                        if stale_only && let Some(reason) = stale_reason.as_ref() {
                            extra.push_str(&format!("\n   \x1b[90m└─ {}\x1b[0m", reason));
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

                        // Days since (Change 3)
                        let days_since = if health == "⚠️" || health == "🔴" || health == "🪦" {
                            days_since_date_prefix(&data.updated_at)
                                .or_else(|| gh_info.as_ref().and_then(|gh| days_since_date_prefix(&gh.pushed_at)))
                        } else {
                            None
                        };
                        let days_str = days_since
                            .map(|d| format!(" \x1b[90m— {} days ago\x1b[0m", d))
                            .unwrap_or_default();

                        let line = format!(
                            "{}{}\x1b[0m {} v{} — {}, downloads: {}{}{}",
                            health_color(health),
                            health,
                            name,
                            version,
                            desc,
                            downloads,
                            extra,
                            days_str,
                        );

                        let mut t = text_lines.lock().expect("text_lines mutex poisoned");
                        t.push(DisplayEntry {
                            health_emoji: health.to_string(),
                            line,
                        });
                    }
                    Err(e) => {
                        count_unknown.fetch_add(1, Ordering::Relaxed);
                        if output_json || sbom {
                            let mut r = results.lock().expect("results mutex poisoned");
                            r.push(PackageResult {
                                name: name.clone(),
                                version: version.clone(),
                                health: "unknown".to_string(),
                                description: None,
                                latest_version: None,
                                stale_reason: Some(e.clone()),
                                vulns: vec![],
                                provenance: None,
                            });
                        }
                        if !output_json && !sbom && !stale_only {
                            let line = format!(
                                "\x1b[90m❓ {} v{} — fetch failed: {}\x1b[0m",
                                name, version, e
                            );
                            let mut t = text_lines.lock().expect("text_lines mutex poisoned");
                            t.push(DisplayEntry {
                                health_emoji: "❓".to_string(),
                                line,
                            });
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

    if !output_json && !sbom {
        let mut entries = text_lines.lock().expect("text_lines mutex poisoned");
        let mut taken: Vec<DisplayEntry> = std::mem::take(&mut *entries);
        taken.sort_by_key(|e| health_sort_key(&e.health_emoji));

        let known_count = taken.iter().filter(|e| e.health_emoji != "❓").count();
        for entry in &taken {
            if entry.health_emoji == "❓" { continue; }
            println!("{}", entry.line);
        }

        if u > 0 && !stale_only {
            if verbose {
                for entry in &taken {
                    if entry.health_emoji == "❓" { println!("{}", entry.line); }
                }
            } else if known_count > 0 {
                println!();
                println!("\x1b[90m❓ {} packages failed to fetch — run with --verbose to see details\x1b[0m", u);
            }
        }
    }

    let packages = collect_results(results);

    if sbom {
        crate::sbom::render("cargo", &packages);
        return;
    }

    print_summary(
        "cargo",
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
