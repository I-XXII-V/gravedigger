use crate::api::{http_client, safe_prefix};
use crate::display::fmt_downloads;
use serde::Deserialize;
use std::collections::HashMap;

// ── Structs ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CrateRevDeps {
    dependencies: Vec<RevDep>,
    versions: Vec<RevVersion>,
    meta: RevMeta,
}

#[derive(Deserialize)]
struct RevDep {
    #[allow(dead_code)]
    version_id: u64,
    downloads: u64,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct RevVersion {
    id: u64,
    #[serde(rename = "crate")]
    crate_name: String,
    description: Option<String>,
    repository: Option<String>,
    downloads: u64,
    num: String,
}

#[derive(Deserialize)]
struct RevMeta {
    total: u32,
}

// ── crates.io reverse deps API ───────────────────────────────────────

fn fetch_crate_rev_deps(name: &str) -> Result<CrateRevDeps, String> {
    let url = format!(
        "https://crates.io/api/v1/crates/{}/reverse_dependencies?per_page=50",
        name
    );

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

pub fn who_depends_crates(package: &str) {
    println!("🔍 Searching for crates that depend on '{}'...\n", package);

    match fetch_crate_rev_deps(package) {
        Ok(rev) => {
            if rev.dependencies.is_empty() {
                println!("📦 No crates depend on '{}'", package);
                return;
            }

            let total = rev.meta.total;
            let shown = rev.dependencies.len();

            // Build a lookup: version_id -> version info
            let ver_map: HashMap<u64, &RevVersion> =
                rev.versions.iter().map(|v| (v.id, v)).collect();

            println!(
                "📦 Found {} reverse dependencies (showing top {})\n",
                total, shown
            );

            for dep in &rev.dependencies {
                let ver = match ver_map.get(&dep.version_id) {
                    Some(v) => v,
                    None => continue,
                };

                let is_github = ver
                    .repository
                    .as_deref()
                    .is_some_and(|u| u.contains("github.com"));

                let desc = ver
                    .description
                    .as_deref()
                    .unwrap_or("")
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .to_string();

                let dl = fmt_downloads(dep.downloads);

                println!(
                    "  {} \x1b[1m{}\x1b[0m v{} — downloads: {}{}",
                    if is_github { "🐙" } else { "📦" },
                    ver.crate_name,
                    ver.num,
                    dl,
                    if desc.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", desc)
                    }
                );
            }

            println!();
            println!(
                "   \x1b[90mShowing {} of {} total dependents\x1b[0m",
                shown, total
            );
        }
        Err(e) => {
            eprintln!("❌ Failed to fetch reverse dependencies: {}", e);
        }
    }
}
