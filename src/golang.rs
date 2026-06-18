use crate::api::*;
use crate::display::{health_color, is_stale};
use crate::types::{PackageResult, ScanOutput, Summary, health_to_string};
use chrono::{Utc, NaiveDate};
use serde::Deserialize;
use std::fs;
use std::sync::{Arc, Mutex};
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

fn parse_go_mod(path: &str) -> Result<Vec<(String, String)>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
    parse_go_mod_lines(&content)
}

fn parse_go_mod_lines(content: &str) -> Result<Vec<(String, String)>, String> {
    let mut deps = Vec::new();
    let mut in_block = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with("module ") || line.starts_with("go ") {
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
    if let Ok(updated) = NaiveDate::parse_from_str(&proxy.Time[..10], "%Y-%m-%d") {
        let days = (Utc::now().date_naive() - updated).num_days();
        if days > 730 { return "🪦"; }
        if days > 365 { return "🔴"; }
        if days > 180 { return "⚠️"; }
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

    serde_json::from_str(&text).map_err(|e| format!("JSON error: {}", e))
}

// ── Public entry point ───────────────────────────────────────────────

pub fn scan_go_deps(stale_only: bool, output_json: bool) {
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
    let count_inactive = &AtomicU32::new(0);
    let count_dead = &AtomicU32::new(0);
    let count_unknown = &AtomicU32::new(0);

    let results: Arc<Mutex<Vec<PackageResult>>> = Arc::new(Mutex::new(Vec::new()));

    thread::scope(|s| {
        for (name, version) in &deps {
            let mod_name = name.clone();
            let mod_version = version.clone();
            let results = Arc::clone(&results);
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

                    if stale_only && !is_stale(health) { return; }

                    if output_json {
                        let mut r = results.lock().unwrap();
                        r.push(PackageResult {
                            name: mod_name.clone(),
                            version: mod_version.clone(),
                            health: health_to_string(health),
                            description: None,
                            latest_version: Some(proxy.Version.trim_start_matches('v').to_string()),
                            stale_reason: get_go_stale_reason(&proxy, &mod_name),
                        });
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
                    if output_json {
                        let mut r = results.lock().unwrap();
                        r.push(PackageResult {
                            name: mod_name.clone(),
                            version: mod_version.clone(),
                            health: "unknown".to_string(),
                            description: None,
                            latest_version: None,
                            stale_reason: Some(e.clone()),
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
    let i = count_inactive.load(Ordering::Relaxed);
    let d = count_dead.load(Ordering::Relaxed);
    let u = count_unknown.load(Ordering::Relaxed);

    if output_json {
        let packages = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
        let output = ScanOutput {
            ecosystem: "go".to_string(),
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
}
