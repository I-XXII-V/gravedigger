use crate::api::{fetch_github_info, http_client, parse_github_repo, safe_prefix, GitHubRepo};
use crate::display::{health_color, health_sort_key, is_stale, DisplayEntry};
use crate::osv;
use crate::types::{
    collect_results, days_since_date_prefix, health_to_string, print_summary, score_from_days,
    track_license, PackageResult, ScanOutput, Summary,
};
use base64::Engine;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
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
    license: Option<String>,
}

#[derive(Deserialize)]
struct NpmRepo {
    url: Option<String>,
}

// ── NPM provenance attestation structs ───────────────────────────────

#[derive(Deserialize)]
struct NpmAttestationsResponse {
    attestations: Vec<NpmAttestation>,
}

#[derive(Deserialize)]
struct NpmAttestation {
    #[serde(rename = "predicateType")]
    predicate_type: String,
    bundle: NpmBundle,
}

#[derive(Deserialize)]
struct NpmBundle {
    #[serde(rename = "dsseEnvelope")]
    dsse_envelope: NpmDsseEnvelope,
}

#[derive(Deserialize)]
struct NpmDsseEnvelope {
    payload: String, // base64-encoded JSON
}

#[derive(Deserialize)]
struct NpmProvenancePayload {
    predicate: NpmPredicate,
}

#[derive(Deserialize)]
struct NpmPredicate {
    #[serde(rename = "buildDefinition")]
    build_definition: NpmBuildDefinition,
    #[serde(rename = "runDetails")]
    run_details: NpmRunDetails,
}

#[derive(Deserialize)]
struct NpmBuildDefinition {
    #[serde(rename = "externalParameters")]
    external_parameters: NpmExternalParams,
    #[serde(rename = "resolvedDependencies")]
    resolved_dependencies: Vec<NpmResolvedDep>,
}

#[derive(Deserialize)]
struct NpmExternalParams {
    workflow: Option<NpmWorkflow>,
}

#[derive(Deserialize)]
struct NpmWorkflow {
    repository: String,
    #[allow(dead_code)]
    path: String,
}

#[derive(Deserialize)]
struct NpmResolvedDep {
    #[allow(dead_code)]
    uri: String,
    digest: HashMap<String, String>,
}

#[derive(Deserialize)]
struct NpmBuilder {
    id: String,
}

#[derive(Deserialize)]
struct NpmMetadata {
    #[allow(dead_code)]
    #[serde(rename = "invocationId")]
    invocation_id: String,
}

#[derive(Deserialize)]
struct NpmRunDetails {
    builder: NpmBuilder,
    #[allow(dead_code)]
    metadata: NpmMetadata,
}

// ── Helpers ──────────────────────────────────────────────────────────

fn extract_npm_deps(lock: &NpmLock) -> Vec<(String, String)> {
    let mut deps = Vec::new();

    if let Some(packages) = &lock.packages {
        // Track (name, version) pairs so nested packages at different
        // versions (e.g. foo@1.0.0 and foo@2.0.0) are both kept.
        let mut seen = std::collections::HashSet::new();
        for (path, info) in packages {
            if path.is_empty() {
                continue;
            }
            if let Some(version) = &info.version {
                // Use rsplitn to handle nested node_modules correctly:
                // "node_modules/a/node_modules/b" → "b", not "a/node_modules/b"
                let name = path
                    .rsplit("node_modules/")
                    .next()
                    .unwrap_or(path);
                let key = (name.to_string(), version.clone());
                if seen.insert(key) {
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

// ── Public parser ────────────────────────────────────────────────────

/// Parse a `package-lock.json` content string and return all dependencies
/// as `(name, version)` pairs. Supports both v3 (`packages`) and v1
/// (`dependencies`) lock file formats.
pub fn parse_npm_lock(content: &str) -> Result<Vec<(String, String)>, String> {
    let lock: NpmLock = serde_json::from_str(content)
        .map_err(|e| format!("Failed to parse package-lock.json: {}", e))?;
    Ok(extract_npm_deps(&lock))
}

// ── Health scoring ───────────────────────────────────────────────────

/// Compute npm package health.
///
/// Uses the npm registry `modified` date first. If the registry says
/// healthy (✅), falls back to GitHub `pushed_at` for finer granularity.
/// `gh` is an optional cached `GitHubRepo` — pass `None` to skip the
/// GitHub check (no second API call).
fn get_npm_health(data: &NpmRegistryResponse, gh: Option<&GitHubRepo>) -> &'static str {
    if let Some(modified) = data.time.get("modified") {
        if let Some(days) = days_since_date_prefix(modified) {
            let health = score_from_days(days);
            if health != "✅" {
                return health;
            }
        } else {
            return "❓";
        }
    } else {
        return "❓";
    }

    // Registry says fresh (✅) — use cached GitHub data for finer score
    if let Some(gh) = gh && let Some(days) = days_since_date_prefix(&gh.pushed_at) {
        return score_from_days(days);
    }

    "✅"
}

/// Stale reason for npm packages.
///
/// Returns `None` when the package is fully healthy (both npm registry
/// and GitHub are active).  This prevents false positives where a healthy
/// package with no GitHub URL would get "No repository URL" as a stale
/// reason.
///
/// `gh` is an optional cached `GitHubRepo`.  When the registry-side
/// check does not find staleness but `gh` is `None`, we return `None`
/// (the absence of a GitHub repo is not a health problem).
fn get_npm_stale_reason(data: &NpmRegistryResponse, gh: Option<&GitHubRepo>) -> Option<String> {
    // 1) Check npm registry modified date
    if let Some(modified) = data.time.get("modified")
        && let Some(days) = days_since_date_prefix(modified)
    {
        if days > 730 {
            return Some(format!("No update on npm in {} days — DEAD", days));
        }
        if days > 365 {
            return Some(format!("No update on npm in {} days — INACTIVE", days));
        }
        if days > 180 {
            return Some(format!("No update on npm in {} days — STALE", days));
        }
    }

    // 2) Registry is healthy (≤180 days) — check cached GitHub data
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
        // gh is present and activity is ≤180 days → healthy
        return None;
    }

    // 3) No GitHub data and registry is healthy — not stale
    None
}

// ── npm registry API ────────────────────────────────────────────────

fn fetch_npm_info(name: &str) -> Result<NpmRegistryResponse, String> {
    let encoded = name.replace('/', "%2F");
    let url = format!("https://registry.npmjs.org/{}", encoded);

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

/// Fetch npm provenance attestations for a package at a specific version.
/// Returns a human-readable status string, never None.
/// Uses the same 6-hour cache as OSV queries.
fn fetch_npm_attestation(name: &str, version: &str) -> String {
    let cache_key = format!("npm-attest/{}/{}", name, version);
    let cache = crate::cache::init();

    // Check cache first
    if let Some(cached) = cache.get("npm", &cache_key, 6) {
        return cached;
    }

    let encoded = name.replace('/', "%2F");
    let url = format!(
        "https://registry.npmjs.org/-/npm/v1/attestations/{}@{}",
        encoded, version
    );

    let resp = match http_client()
        .get(&url)
        .header("User-Agent", "gravedigger")
        .send()
    {
        Ok(r) => r,
        Err(_) => {
            let msg = "⚠️ Provenance: network error".to_string();
            cache.set("npm", &cache_key, &msg);
            return msg;
        }
    };

    if !resp.status().is_success() {
        // 404 = no attestations — common for packages without provenance
        let msg = "Provenance: not available".to_string();
        cache.set("npm", &cache_key, &msg);
        return msg;
    }

    let text = match resp.text() {
        Ok(t) => t,
        Err(_) => return "⚠️ Provenance: parse error".to_string(),
    };

    let att_response: NpmAttestationsResponse = match serde_json::from_str(&text) {
        Ok(a) => a,
        Err(_) => return "⚠️ Provenance: invalid response".to_string(),
    };

    // Find the SLSA provenance attestation
    for att in &att_response.attestations {
        if !att.predicate_type.contains("slsa.dev/provenance") {
            continue;
        }

        let payload_bytes = match base64::engine::general_purpose::STANDARD
            .decode(&att.bundle.dsse_envelope.payload)
        {
            Ok(b) => b,
            Err(_) => continue,
        };

        let payload: NpmProvenancePayload = match serde_json::from_slice(&payload_bytes) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let pred = &payload.predicate;

        let workflow = match pred.build_definition.external_parameters.workflow.as_ref() {
            Some(w) => w,
            None => continue,
        };

        let commit = pred
            .build_definition
            .resolved_dependencies
            .first()
            .and_then(|d| d.digest.get("gitCommit"))
            .map(|c| c.get(..7).unwrap_or(c).to_string());

        // Normalize repo URL: strip trailing junk, keep owner/repo
        let repo = workflow
            .repository
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("git@")
            .trim_end_matches(".git")
            .trim_end_matches('/')
            .to_string();

        let builder = pred
            .run_details
            .builder
            .id
            .trim_start_matches("https://")
            .to_string();

        let commit_part = match &commit {
            Some(c) => format!("@{}", c),
            None => String::new(),
        };

        let msg = format!(
            "🧾 Provenance: {}{} (built by {})",
            repo, commit_part, builder
        );
        cache.set("npm", &cache_key, &msg);
        return msg;
    }

    let msg = "Provenance: not available".to_string();
    cache.set("npm", &cache_key, &msg);
    msg
}

/// Scan a single npm package and return its health result directly.
/// Combines fetch + health scoring + OSV query + provenance in one call.
pub(crate) fn scan_single(name: &str, version: &str) -> PackageResult {
    match fetch_npm_info(name) {
        Ok(reg) => {
            let gh_info: Option<GitHubRepo> = reg
                .repository
                .as_ref()
                .and_then(|r| r.url.as_deref())
                .and_then(|url| {
                    let (owner, repo) = parse_github_repo(url)?;
                    fetch_github_info(&owner, &repo).ok()
                });

            let health = get_npm_health(&reg, gh_info.as_ref());
            let stale_reason = get_npm_stale_reason(&reg, gh_info.as_ref());
            let vulns = osv::query_package("npm", name, version);
            let provenance = fetch_npm_attestation(name, version);

            PackageResult {
                name: name.to_string(),
                version: version.to_string(),
                health: health_to_string(health),
                description: reg.description.clone(),
                latest_version: reg.dist_tags.get("latest").cloned(),
                stale_reason,
                vulns,
                provenance: Some(provenance),
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

pub fn scan_npm_deps(stale_only: bool, output_json: bool, ci: bool, licenses: bool, verbose: bool) {
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
        println!(
            "📦 Scanning {} npm packages from package-lock.json\n",
            deps.len()
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
        for (name, version) in &deps {
            let pkg_name = name.clone();
            let pkg_version = version.clone();
            let results = Arc::clone(&results);
            let text_lines = Arc::clone(&text_lines);
            let licenses_map = Arc::clone(&licenses_map);
            s.spawn(move || match fetch_npm_info(&pkg_name) {
                Ok(reg) => {
                    // Fetch GitHub info ONCE — shared by health scoring and stale reason
                    let gh_info: Option<GitHubRepo> = reg
                        .repository
                        .as_ref()
                        .and_then(|r| r.url.as_deref())
                        .and_then(|url| {
                            let (owner, repo) = parse_github_repo(url)?;
                            fetch_github_info(&owner, &repo).ok()
                        });

                    let health = get_npm_health(&reg, gh_info.as_ref());

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
                    let vulns = osv::query_package("npm", &pkg_name, &pkg_version);
                    let n_cves = vulns.len() as u32;
                    if n_cves > 0 {
                        count_cves.fetch_add(n_cves, Ordering::Relaxed);
                    }

                    // Track license if --licenses is active
                    if licenses {
                        track_license(&licenses_map, reg.license.as_deref());
                    }

                    // Stale reason uses cached gh_info — no second API call
                    let stale_reason = get_npm_stale_reason(&reg, gh_info.as_ref());

                    if stale_only && !is_stale(health) && vulns.is_empty() {
                        return;
                    }

                    // Check npm provenance attestation (cached, always returns a string)
                    let provenance = fetch_npm_attestation(&pkg_name, &pkg_version);

                    if output_json {
                        let mut r = results.lock().expect("results mutex poisoned");
                        r.push(PackageResult {
                            name: pkg_name.clone(),
                            version: pkg_version.clone(),
                            health: health_to_string(health),
                            description: reg.description.clone(),
                            latest_version: reg.dist_tags.get("latest").cloned(),
                            stale_reason,
                            vulns: vulns.clone(),
                            provenance: Some(provenance.clone()),
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

                    let mut extra = String::new();

                    if stale_only && let Some(reason) = stale_reason.as_ref() {
                        extra.push_str(&format!("\n   \x1b[90m└─ {}\x1b[0m", reason));
                    }

                    // Show provenance status (always, so user can see both verified and missing)
                    let prov_color = if provenance.starts_with("🧾") { "\x1b[32m" } else { "\x1b[90m" };
                    extra.push_str(&format!(
                        "\n   {}{}\x1b[0m",
                        prov_color, provenance
                    ));

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
                        reg.time.get("modified")
                            .and_then(|m| days_since_date_prefix(m))
                            .or_else(|| gh_info.as_ref().and_then(|gh| days_since_date_prefix(&gh.pushed_at)))
                    } else {
                        None
                    };
                    let days_str = days_since
                        .map(|d| format!(" \x1b[90m— {} days ago\x1b[0m", d))
                        .unwrap_or_default();

                    let line = format!(
                        "{}{}\x1b[0m {} v{} — {} (latest: {}){}{}",
                        health_color(health),
                        health,
                        pkg_name,
                        pkg_version,
                        if desc.is_empty() {
                            "no description"
                        } else {
                            &desc
                        },
                        latest,
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
                    if output_json {
                        let mut r = results.lock().expect("results mutex poisoned");
                        r.push(PackageResult {
                            name: pkg_name.clone(),
                            version: pkg_version.clone(),
                            health: "unknown".to_string(),
                            description: None,
                            latest_version: None,
                            stale_reason: Some(e.clone()),
                                vulns: vec![],
                                provenance: None,
                            });
                    } else if !stale_only {
                        let line = format!(
                            "\x1b[90m❓ {} v{} — fetch failed: {}\x1b[0m",
                            pkg_name, pkg_version, e
                        );
                        let mut t = text_lines.lock().expect("text_lines mutex poisoned");
                        t.push(DisplayEntry {
                            health_emoji: "❓".to_string(),
                            line,
                        });
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

    if !output_json {
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

    print_summary(
        "npm",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_npm_deps_v3_format() {
        let lock = NpmLock {
            packages: Some(HashMap::from([
                (
                    "node_modules/foo".into(),
                    NpmPkg {
                        version: Some("1.0.0".into()),
                    },
                ),
                (
                    "node_modules/bar".into(),
                    NpmPkg {
                        version: Some("2.0.0".into()),
                    },
                ),
                (
                    "".into(),
                    NpmPkg {
                        version: Some("1.0.0".into()),
                    },
                ), // root = skip
            ])),
            dependencies: None,
        };
        let deps = extract_npm_deps(&lock);
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&("foo".into(), "1.0.0".into())));
        assert!(deps.contains(&("bar".into(), "2.0.0".into())));
    }

    #[test]
    fn test_extract_npm_deps_v1_format() {
        let lock = NpmLock {
            packages: None,
            dependencies: Some(HashMap::from([
                (
                    "foo".into(),
                    NpmDep {
                        version: "1.0.0".into(),
                    },
                ),
                (
                    "bar".into(),
                    NpmDep {
                        version: "2.0.0".into(),
                    },
                ),
            ])),
        };
        let deps = extract_npm_deps(&lock);
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&("foo".into(), "1.0.0".into())));
        assert!(deps.contains(&("bar".into(), "2.0.0".into())));
    }

    #[test]
    fn test_extract_npm_deps_empty() {
        let lock = NpmLock {
            packages: Some(HashMap::new()),
            dependencies: None,
        };
        let deps = extract_npm_deps(&lock);
        assert!(deps.is_empty());
    }

    #[test]
    fn test_extract_npm_deps_nested_not_deduped() {
        let lock = NpmLock {
            packages: Some(HashMap::from([
                (
                    "node_modules/foo".into(),
                    NpmPkg {
                        version: Some("1.0.0".into()),
                    },
                ),
                (
                    "node_modules/other/node_modules/foo".into(),
                    NpmPkg {
                        version: Some("2.0.0".into()),
                    },
                ),
            ])),
            dependencies: None,
        };
        // Nested node_modules are different versions, keep both
        let deps = extract_npm_deps(&lock);
        assert_eq!(deps.len(), 2);
    }
}
