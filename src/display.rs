use crate::api::*;
use crate::types::{PackageResult, ScanOutput, Summary, health_to_string};
use chrono::{Utc, NaiveDate};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

// ANSI color codes
const GREEN:  &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED:    &str = "\x1b[31m";
const GRAY:   &str = "\x1b[90m";
const BOLD:   &str = "\x1b[1m";
const RESET:  &str = "\x1b[0m";

pub fn get_health(pkg: &AurPackage) -> &str {
    if pkg.outofdate.is_some() {
        return "⚠️";
    }
    if let Some(ref url) = pkg.url {
        if let Some((owner, repo)) = parse_github_repo(url) {
            if let Ok(gh) = fetch_github_info(&owner, &repo) {
                let pushed = &gh.pushed_at[..10];
                if let Ok(last) = NaiveDate::parse_from_str(pushed, "%Y-%m-%d") {
                    let days = (Utc::now().date_naive() - last).num_days();
                    if days > 730 { return "🪦"; }
                    else if days > 365 { return "🔴"; }
                    else if days > 180 { return "⚠️"; }
                    else { return "✅"; }
                }
            }
        }
    }
    "❓"
}

fn health_color(health: &str) -> &str {
    match health {
        "✅" => GREEN,
        "⚠️" => YELLOW,
        "🔴" | "🪦" => RED,
        _ => GRAY,
    }
}

pub fn is_stale(health: &str) -> bool {
    health == "🪦" || health == "🔴" || health == "⚠️" || health == "❓"
}

fn get_stale_reason(pkg: &AurPackage) -> Option<String> {
    if pkg.outofdate.is_some() {
        return Some("Marked out-of-date on AUR".to_string());
    }
    if let Some(ref url) = pkg.url {
        if let Some((owner, repo)) = parse_github_repo(url) {
            match fetch_github_info(&owner, &repo) {
                Ok(gh) => {
                    let pushed = &gh.pushed_at[..10];
                    if let Ok(last) = NaiveDate::parse_from_str(pushed, "%Y-%m-%d") {
                        let days = (Utc::now().date_naive() - last).num_days();
                        if days > 730 {
                            return Some(format!("No GitHub activity in {} days — DEAD", days));
                        } else if days > 365 {
                            return Some(format!("No GitHub activity in {} days — INACTIVE", days));
                        } else if days > 180 {
                            return Some(format!("No GitHub activity in {} days — STALE", days));
                        }
                    }
                    None
                }
                Err(e) => Some(format!("GitHub fetch failed: {}", e)),
            }
        } else {
            Some("Not a GitHub repository".to_string())
        }
    } else {
        Some("No upstream URL".to_string())
    }
}

#[derive(Serialize)]
struct SinglePackageOutput {
    ecosystem: String,
    name: String,
    version: String,
    description: Option<String>,
    url: Option<String>,
    maintainer: Option<String>,
    numvotes: u32,
    popularity: f64,
    outofdate: Option<u32>,
    lastmodified: u64,
    health: String,
    github: Option<SingleGitHubOutput>,
}

#[derive(Serialize)]
struct SingleGitHubOutput {
    owner: String,
    repo: String,
    stars: u32,
    forks: u32,
    open_issues: u32,
    watchers: u32,
    pushed_at: String,
    archived: bool,
}

// ── Scan installed AUR packages ──────────────────────────────────────

pub fn scan_installed(stale_only: bool, output_json: bool) {
    let output = std::process::Command::new("pacman")
        .args(["-Qm"])
        .output()
        .expect("Failed to run pacman -Qm");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let packages: Vec<String> = stdout.lines()
        .filter_map(|line| line.split_whitespace().next().map(String::from))
        .collect();

    if !output_json {
        println!("📦 Scanning {} AUR packages...\n", packages.len());
    }

    let count_healthy = &AtomicU32::new(0);
    let count_warning = &AtomicU32::new(0);
    let count_inactive = &AtomicU32::new(0);
    let count_dead = &AtomicU32::new(0);
    let count_unknown = &AtomicU32::new(0);

    let results: Arc<Mutex<Vec<PackageResult>>> = Arc::new(Mutex::new(Vec::new()));

    thread::scope(|s| {
        for pkg_name in &packages {
            let name = pkg_name.clone();
            let results = Arc::clone(&results);
            s.spawn(move || {
                let url = format!("https://aur.archlinux.org/rpc/v5/info/{}", name);
                match fetch_aur_info(&url) {
                    Ok(response) if response.resultcount > 0 => {
                        let pkg = &response.results[0];
                        let health = get_health(pkg);

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
                                name: pkg.name.clone(),
                                version: pkg.version.clone(),
                                health: health_to_string(health),
                                description: pkg.description.clone(),
                                latest_version: None,
                                stale_reason: get_stale_reason(pkg),
                            });
                            return;
                        }

                        let maintainer_str = match pkg.maintainer.as_deref() {
                            None | Some("") => format!("{}{}[ORPHANED]{}", RED, BOLD, RESET),
                            Some(m) => m.to_string(),
                        };

                        let stale_info = if stale_only {
                            get_stale_reason(pkg)
                                .map(|r| format!("\n   {}└─ {}{}", GRAY, r, RESET))
                                .unwrap_or_default()
                        } else { String::new() };

                        println!("{}{}{} {} — maintainer: {}, popularity: {:.1}{}",
                            health_color(health), health, RESET, pkg.name,
                            maintainer_str, pkg.popularity, stale_info);
                    }
                    _ => {
                        count_unknown.fetch_add(1, Ordering::Relaxed);
                        if output_json {
                            let mut r = results.lock().unwrap();
                            r.push(PackageResult {
                                name: name.clone(),
                                version: "?".to_string(),
                                health: "unknown".to_string(),
                                description: None,
                                latest_version: None,
                                stale_reason: Some("AUR API fetch failed".to_string()),
                            });
                        } else if !stale_only {
                            println!("{}❓ {} — {}fetch failed{}", GRAY, name, GRAY, RESET);
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

    if output_json {
        let packages = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
        let output = ScanOutput {
            ecosystem: "aur".to_string(),
            packages,
            summary: Summary { healthy: h, warning: w, inactive: i, dead: d, unknown: u },
        };
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        println!();
        println!("{}📊 Summary:{} {}✅ {}  {}⚠️ {}  {}🔴 {}  {}🪦 {}  {}❓ {}{}",
            BOLD, RESET,
            GREEN, h, YELLOW, w, RED, i, RED, d, GRAY, u, RESET);
    }
}

// ── Search AUR ───────────────────────────────────────────────────────

pub fn search_and_display(query: &str, output_json: bool) {
    match search_aur(query) {
        Ok(response) => {
            if response.resultcount == 0 {
                if output_json {
                    let output = ScanOutput {
                        ecosystem: "aur-search".to_string(),
                        packages: vec![],
                        summary: Summary::new(),
                    };
                    println!("{}", serde_json::to_string_pretty(&output).unwrap());
                } else {
                    println!("🔍 No results for '{}'", query);
                }
                return;
            }

            if !output_json {
                println!("🔍 Search results: {} ({} found)\n", query, response.resultcount);
            }

            let results: Arc<Mutex<Vec<PackageResult>>> = Arc::new(Mutex::new(Vec::new()));

            thread::scope(|s| {
                for pkg in &response.results {
                    let results = Arc::clone(&results);
                    s.spawn(move || {
                        let health = get_health(pkg);

                        if output_json {
                            let mut r = results.lock().unwrap();
                            r.push(PackageResult {
                                name: pkg.name.clone(),
                                version: pkg.version.clone(),
                                health: health_to_string(health),
                                description: pkg.description.clone(),
                                latest_version: None,
                                stale_reason: None,
                            });
                            return;
                        }

                        let stars = if let Some(ref url) = pkg.url {
                            if let Some((owner, repo)) = parse_github_repo(url) {
                                if let Ok(gh) = fetch_github_info(&owner, &repo) {
                                    format!("⭐ {}", gh.stars)
                                } else { String::new() }
                            } else { String::new() }
                        } else { String::new() };

                        println!("{}{}{} {} {}{}",
                            health_color(health), health, RESET,
                            pkg.name,
                            if stars.is_empty() { String::new() } else { format!("({}) ", stars) },
                            pkg.description.as_deref().unwrap_or(""));
                    });
                }
            });

            if output_json {
                let packages = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
                let output = ScanOutput {
                    ecosystem: "aur-search".to_string(),
                    packages,
                    summary: Summary::new(), // no summary for search
                };
                println!("{}", serde_json::to_string_pretty(&output).unwrap());
            }
        }
        Err(e) => {
            if output_json {
                let output = serde_json::json!({
                    "ecosystem": "aur-search",
                    "error": format!("{}", e)
                });
                println!("{}", serde_json::to_string_pretty(&output).unwrap());
            } else {
                eprintln!("❌ Failed to search AUR: {}", e);
            }
        }
    }
}

// ── Single AUR package (JSON mode) ───────────────────────────────────

pub fn single_package_json(pkg_name: &str, output_json: bool) {
    let url = format!("https://aur.archlinux.org/rpc/v5/info/{}", pkg_name);
    match fetch_aur_info(&url) {
        Ok(response) => {
            if response.resultcount == 0 {
                if output_json {
                    let err = serde_json::json!({
                        "ecosystem": "aur",
                        "error": format!("Package '{}' not found in AUR", pkg_name)
                    });
                    println!("{}", serde_json::to_string_pretty(&err).unwrap());
                } else {
                    eprintln!("❌ Package '{}' not found in AUR", pkg_name);
                    std::process::exit(1);
                }
                return;
            }

            let pkg = &response.results[0];

            if output_json {
                let health_emoji = get_health(pkg);
                let health_str = health_to_string(health_emoji);
                let gh_output = pkg.url.as_ref().and_then(|upstream_url| {
                    parse_github_repo(upstream_url).and_then(|(owner, repo)| {
                        match fetch_github_info(&owner, &repo) {
                            Ok(gh) => Some(SingleGitHubOutput {
                                owner: owner.clone(),
                                repo: repo.clone(),
                                stars: gh.stars,
                                forks: gh.forks,
                                open_issues: gh.open_issues,
                                watchers: gh.watchers,
                                pushed_at: gh.pushed_at,
                                archived: gh.archived,
                            }),
                            Err(_) => None,
                        }
                    })
                });

                let output = SinglePackageOutput {
                    ecosystem: "aur".to_string(),
                    name: pkg.name.clone(),
                    version: pkg.version.clone(),
                    description: pkg.description.clone(),
                    url: pkg.url.clone(),
                    maintainer: pkg.maintainer.clone(),
                    numvotes: pkg.numvotes,
                    popularity: pkg.popularity,
                    outofdate: pkg.outofdate,
                    lastmodified: pkg.lastmodified,
                    health: health_str,
                    github: gh_output,
                };
                println!("{}", serde_json::to_string_pretty(&output).unwrap());
            } else {
                print_package_info(pkg);

                if let Some(ref upstream_url) = pkg.url {
                    if let Some((owner, repo)) = parse_github_repo(upstream_url) {
                        println!("\n🐙 GitHub: {}/{}", owner, repo);
                        match fetch_github_info(&owner, &repo) {
                            Ok(gh) => print_github_info(&gh),
                            Err(e) => eprintln!("   ❌ Fetch failed: {}", e),
                        }
                    } else {
                        println!("\n🐙 GitHub: not a GitHub repository");
                    }
                }
            }
        }
        Err(e) => {
            if output_json {
                let err = serde_json::json!({
                    "ecosystem": "aur",
                    "error": format!("Failed to fetch AUR: {}", e)
                });
                println!("{}", serde_json::to_string_pretty(&err).unwrap());
            } else {
                eprintln!("❌ Failed to fetch AUR: {}", e);
                std::process::exit(1);
            }
        }
    }
}

// ── Text display helpers ─────────────────────────────────────────────

pub fn print_package_info(pkg: &AurPackage) {
    println!("\n📦 Package: {}", pkg.name);
    println!("   Version: {}", pkg.version);
    println!("   Description: {}", pkg.description.as_deref().unwrap_or("-"));
    println!("   Upstream URL: {}", pkg.url.as_deref().unwrap_or("-"));
    match pkg.maintainer.as_deref() {
        None | Some("") => println!("   Maintainer: {}{}[ORPHANED]{}", RED, BOLD, RESET),
        Some(m) => println!("   Maintainer: {}", m),
    }
    println!("   Votes: {}", pkg.numvotes);
    println!("   Popularity: {:.2}", pkg.popularity);
    println!("   Out of date: {}", match pkg.outofdate {
        Some(_) => "⚠️ Yes",
        None => "✅ No",
    });

    let dur = std::time::Duration::from_secs(pkg.lastmodified);
    let time = std::time::UNIX_EPOCH + dur;
    let datetime = chrono::DateTime::<chrono::Utc>::from(time);
    println!("   Last updated: {}", datetime.format("%Y-%m-%d %H:%M UTC"));
}

pub fn print_github_info(repo: &GitHubRepo) {
    println!("   ⭐ Stars: {}", repo.stars);
    println!("   🍴 Forks: {}", repo.forks);
    println!("   🔥 Open issues: {}", repo.open_issues);
    println!("   👀 Watchers: {}", repo.watchers);
    println!("   📅 Pushed at: {}", &repo.pushed_at[..10]);

    let pushed = &repo.pushed_at[..10];
    let now = Utc::now();
    if let Ok(last_push) = NaiveDate::parse_from_str(pushed, "%Y-%m-%d") {
        let days_since = (now.date_naive() - last_push).num_days();
        if days_since > 730 {
            println!("   {}{}🪦 Last push > 2 years — DEAD{}", BOLD, RED, RESET);
        } else if days_since > 365 {
            println!("   {}🔴 Last push > 1 year — INACTIVE{}", RED, RESET);
        } else if days_since > 180 {
            println!("   {}⚠️ Last push > 6 months — check needed{}", YELLOW, RESET);
        } else {
            println!("   {}✅ Active ({} days ago){}", GREEN, days_since, RESET);
        }
    }

    if repo.archived {
        println!("   🗄️ ARCHIVED — no longer maintained");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_stale_dead() {
        assert!(is_stale("🪦"));
    }

    #[test]
    fn test_is_stale_inactive() {
        assert!(is_stale("🔴"));
    }

    #[test]
    fn test_is_stale_warning() {
        assert!(is_stale("⚠️"));
    }

    #[test]
    fn test_is_stale_unknown() {
        assert!(is_stale("❓"));
    }

    #[test]
    fn test_is_stale_healthy() {
        assert!(!is_stale("✅"));
    }

    #[test]
    fn test_is_stale_invalid() {
        assert!(!is_stale("🤷"));
    }

    #[test]
    fn test_health_color_healthy() {
        assert_eq!(health_color("✅"), "\x1b[32m");
    }

    #[test]
    fn test_health_color_warning() {
        assert_eq!(health_color("⚠️"), "\x1b[33m");
    }

    #[test]
    fn test_health_color_inactive() {
        assert_eq!(health_color("🔴"), "\x1b[31m");
    }

    #[test]
    fn test_health_color_dead() {
        assert_eq!(health_color("🪦"), "\x1b[31m");
    }

    #[test]
    fn test_health_color_unknown() {
        assert_eq!(health_color("❓"), "\x1b[90m");
    }
}
