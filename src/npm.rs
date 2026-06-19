use crate::api::*;
use crate::display::{health_color, is_stale};
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
    #[serde(rename = "invocationId")]
    invocation_id: String,
}

#[derive(Deserialize)]
struct NpmRunDetails {
    builder: NpmBuilder,
    metadata: NpmMetadata,
}

// ── Helpers ──────────────────────────────────────────────────────────

fn clean_github_url(raw: &str) -> &str {
    let s = raw.trim_start_matches("git+");
    s.trim_end_matches(".git")
}

fn extract_npm_deps(lock: &NpmLock) -> Vec<(String, String)> {
    let mut deps = Vec::new();

    if let Some(packages) = &lock.packages {
        let mut seen = std::collections::HashSet::new();
        for (path, info) in packages {
            if path.is_empty() {
                continue;
            }
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

    if let Some(repo) = &data.repository {
        if let Some(ref url) = repo.url {
            let clean = clean_github_url(url);
            if let Some((owner, repo_name)) = parse_github_repo(clean) {
                if let Ok(gh) = fetch_github_info(&owner, &repo_name) {
                    if let Some(days) = days_since_date_prefix(&gh.pushed_at) {
                        return score_from_days(days);
                    }
                }
            }
        }
    }

    "✅"
}

fn get_npm_stale_reason(data: &NpmRegistryResponse) -> Option<String> {
    if let Some(modified) = data.time.get("modified") {
        if let Some(days) = days_since_date_prefix(modified) {
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
    }

    if let Some(repo) = &data.repository {
        if let Some(ref url) = repo.url {
            let clean = clean_github_url(url);
            if let Some((owner, repo_name)) = parse_github_repo(clean) {
                match fetch_github_info(&owner, &repo_name) {
                    Ok(gh) => {
                        if let Some(days) = days_since_date_prefix(&gh.pushed_at) {
                            if days > 730 {
                                return Some(format!("No GitHub activity in {} days — DEAD", days));
                            }
                            if days > 365 {
                                return Some(format!(
                                    "No GitHub activity in {} days — INACTIVE",
                                    days
                                ));
                            }
                            if days > 180 {
                                return Some(format!(
                                    "No GitHub activity in {} days — STALE",
                                    days
                                ));
                            }
                        }
                    }
                    Err(e) => return Some(format!("GitHub fetch failed: {}", e)),
                }
            } else {
                return Some("Not a GitHub repository".to_string());
            }
        } else {
            return Some("No repository URL".to_string());
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
        return Err(format!(
            "HTTP {} — {}",
            status,
            &text[..200.min(text.len())]
        ));
    }

    serde_json::from_str(&text)
        .map_err(|e| format!("JSON error: {} — body: {}", e, &text[..200.min(text.len())]))
}

/// Parsed npm provenance information for display.
struct ProvenanceInfo {
    repo: String,
    commit: String,
    builder: String,
    #[allow(dead_code)]
    workflow_run: String,
}

/// Fetch npm provenance attestations for a package at a specific version.
fn fetch_npm_attestations(name: &str, version: &str) -> Option<ProvenanceInfo> {
    let encoded = name.replace('/', "%2F");
    let url = format!(
        "https://registry.npmjs.org/-/npm/v1/attestations/{}@{}",
        encoded, version
    );

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "watchtower")
        .send()
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let text = resp.text().ok()?;
    let att_response: NpmAttestationsResponse = serde_json::from_str(&text).ok()?;

    // Find the SLSA provenance attestation
    for att in &att_response.attestations {
        if !att.predicate_type.contains("slsa.dev/provenance") {
            continue;
        }

        // Decode base64 payload
        let payload_bytes = base64::engine::general_purpose::STANDARD
            .decode(&att.bundle.dsse_envelope.payload)
            .ok()?;

        let payload: NpmProvenancePayload = serde_json::from_slice(&payload_bytes).ok()?;
        let pred = &payload.predicate;

        let workflow = pred.build_definition.external_parameters.workflow.as_ref()?;
        let commit = pred
            .build_definition
            .resolved_dependencies
            .first()?
            .digest
            .get("gitCommit")?
            .clone();

        let short_commit = commit.get(..7).unwrap_or(&commit).to_string();

        return Some(ProvenanceInfo {
            repo: workflow.repository.trim_start_matches("https://").to_string(),
            commit: short_commit,
            builder: pred.run_details.builder.id.clone(),
            workflow_run: pred.run_details.metadata.invocation_id.clone(),
        });
    }

    None
}

// ── Public entry point ───────────────────────────────────────────────

pub fn scan_npm_deps(stale_only: bool, output_json: bool, ci: bool, licenses: bool) {
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
    let licenses_map: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));

    thread::scope(|s| {
        for (name, version) in &deps {
            let pkg_name = name.clone();
            let pkg_version = version.clone();
            let results = Arc::clone(&results);
            let licenses_map = Arc::clone(&licenses_map);
            s.spawn(move || match fetch_npm_info(&pkg_name) {
                Ok(reg) => {
                    let health = get_npm_health(&reg);

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
                        track_license(&*licenses_map, reg.license.as_deref());
                    }

                    if stale_only && !is_stale(health) && vulns.is_empty() {
                        return;
                    }

                    if output_json {
                        let mut r = results.lock().unwrap();
                        r.push(PackageResult {
                            name: pkg_name.clone(),
                            version: pkg_version.clone(),
                            health: health_to_string(health),
                            description: reg.description.clone(),
                            latest_version: reg.dist_tags.get("latest").cloned(),
                            stale_reason: get_npm_stale_reason(&reg),
                            vulns: vulns.clone(),
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

                    if stale_only {
                        if let Some(reason) = get_npm_stale_reason(&reg) {
                            extra.push_str(&format!("\n   \x1b[90m└─ {}\x1b[0m", reason));
                        }
                    }

                    // Check npm provenance attestation
                    if let Some(prov) = fetch_npm_attestations(&pkg_name, &pkg_version) {
                        extra.push_str(&format!(
                            "\n   \x1b[32m📜 Provenance\x1b[0m: {}@{} (built by {})",
                            prov.repo, prov.commit, prov.builder
                        ));
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

                    println!(
                        "{}{}\x1b[0m {} v{} — {} (latest: {}){}",
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
                            vulns: vec![],
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
    let c = count_cves.load(Ordering::Relaxed);

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
        Some(&*licenses_map),
        ci,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_github_url_git_https() {
        assert_eq!(
            clean_github_url("git+https://github.com/owner/repo.git"),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn test_clean_github_url_git_protocol() {
        assert_eq!(
            clean_github_url("git://github.com/owner/repo.git"),
            "git://github.com/owner/repo"
        );
    }

    #[test]
    fn test_clean_github_url_no_git_prefix() {
        assert_eq!(
            clean_github_url("https://github.com/owner/repo"),
            "https://github.com/owner/repo"
        );
    }

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
