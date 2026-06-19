//! OSV.dev API client — open source vulnerability database.
//!
//! Query endpoint: `POST https://api.osv.dev/v1/query`
//!
//! Supports ecosystems: crates.io, npm, PyPI, Go, and many more.

use crate::cache;
use crate::types::VulnInfo;
use serde::Deserialize;

#[derive(Deserialize)]
struct OsvResponse {
    vulns: Option<Vec<OsvVuln>>,
}

#[derive(Deserialize)]
struct OsvVuln {
    id: String,
    summary: Option<String>,
    aliases: Option<Vec<String>>,
    database_specific: Option<OsvDatabaseSpecific>,
    severity: Option<Vec<OsvSeverity>>,
}

#[derive(Deserialize)]
struct OsvDatabaseSpecific {
    severity: Option<String>,
}

#[derive(Deserialize)]
struct OsvSeverity {
    #[serde(rename = "type")]
    _type: String,
    score: String,
}

/// Map an internal ecosystem name to OSV's expected ecosystem string.
pub fn osv_ecosystem(eco: &str) -> Option<&'static str> {
    match eco {
        "crates.io" => Some("crates.io"),
        "npm" => Some("npm"),
        "PyPI" => Some("PyPI"),
        "Go" => Some("Go"),
        "RubyGems" => Some("RubyGems"),
        "NuGet" => Some("NuGet"),
        "Maven" => Some("Maven"),
        "Packagist" => Some("Packagist"),
        "Hex" => Some("Hex"),
        // No AUR support — AUR packages don't map to OSV
        _ => None,
    }
}

/// Query OSV.dev for known vulnerabilities affecting a package at a specific version.
///
/// Uses a 6-hour cache to avoid redundant API calls.
/// Returns an empty `Vec` on error or no vulns found (never fails the caller).
pub fn query_package(ecosystem: &str, name: &str, version: &str) -> Vec<VulnInfo> {
    let Some(osv_eco) = osv_ecosystem(ecosystem) else {
        return vec![];
    };

    let cache_key = format!("{}/{}/{}", osv_eco, name, version);
    let cache = cache::init();

    // Check cache first
    if let Some(cached) = cache.get("osv", &cache_key, 6) {
        if let Ok(vulns) = serde_json::from_str::<Vec<VulnInfo>>(&cached) {
            return vulns;
        }
    }

    // Query OSV API
    let url = "https://api.osv.dev/v1/query";
    let body = serde_json::json!({
        "package": { "name": name, "ecosystem": osv_eco },
        "version": version,
    });

    let client = reqwest::blocking::Client::new();
    let resp = match client
        .post(url)
        .header("User-Agent", "watchtower")
        .json(&body)
        .send()
    {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let text = match resp.text() {
        Ok(t) => t,
        Err(_) => return vec![],
    };

    let vulns = parse_osv_response(&text);

    // Cache the result (even empty list, so we don't re-query)
    if let Ok(json) = serde_json::to_string(&vulns) {
        cache.set("osv", &cache_key, &json);
    }

    vulns
}

fn parse_osv_response(text: &str) -> Vec<VulnInfo> {
    let resp: OsvResponse = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let Some(vulns) = resp.vulns else {
        return vec![];
    };

    vulns
        .into_iter()
        .map(|v| {
            // Determine severity: try database_specific.severity first,
            // then try severity[0].score for CVSS
            let severity = v
                .database_specific
                .as_ref()
                .and_then(|ds| ds.severity.clone())
                .or_else(|| {
                    v.severity
                        .as_ref()
                        .and_then(|s| s.first().map(|s| s.score.clone()))
                });

            VulnInfo {
                id: v.id,
                summary: v.summary,
                severity,
                aliases: v.aliases.unwrap_or_default(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_osv_ecosystem_supported() {
        assert_eq!(osv_ecosystem("crates.io"), Some("crates.io"));
        assert_eq!(osv_ecosystem("npm"), Some("npm"));
        assert_eq!(osv_ecosystem("PyPI"), Some("PyPI"));
        assert_eq!(osv_ecosystem("Go"), Some("Go"));
        assert_eq!(osv_ecosystem("RubyGems"), Some("RubyGems"));
        assert_eq!(osv_ecosystem("NuGet"), Some("NuGet"));
        assert_eq!(osv_ecosystem("Maven"), Some("Maven"));
        assert_eq!(osv_ecosystem("Packagist"), Some("Packagist"));
        assert_eq!(osv_ecosystem("Hex"), Some("Hex"));
    }

    #[test]
    fn test_osv_ecosystem_unsupported() {
        assert_eq!(osv_ecosystem("aur"), None);
        assert_eq!(osv_ecosystem("rubygems"), None); // case-sensitive
        assert_eq!(osv_ecosystem("Cargo"), None);
        assert_eq!(osv_ecosystem("PIP"), None);
    }

    #[test]
    fn test_parse_osv_response_with_vulns() {
        let json = r#"{
            "vulns": [
                {
                    "id": "GHSA-xxxx-xxxx-xxxx",
                    "summary": "Vulnerability in package",
                    "aliases": ["CVE-2024-1234"],
                    "database_specific": {
                        "severity": "HIGH"
                    }
                }
            ]
        }"#;
        let vulns = parse_osv_response(json);
        assert_eq!(vulns.len(), 1);
        assert_eq!(vulns[0].id, "GHSA-xxxx-xxxx-xxxx");
        assert_eq!(
            vulns[0].summary.as_deref(),
            Some("Vulnerability in package")
        );
        assert_eq!(vulns[0].severity.as_deref(), Some("HIGH"));
        assert_eq!(vulns[0].aliases, vec!["CVE-2024-1234"]);
    }

    #[test]
    fn test_parse_osv_response_no_vulns() {
        let json = r#"{"vulns": []}"#;
        let vulns = parse_osv_response(json);
        assert!(vulns.is_empty());
    }

    #[test]
    fn test_parse_osv_response_empty() {
        let json = r#"{}"#;
        let vulns = parse_osv_response(json);
        assert!(vulns.is_empty());
    }

    #[test]
    fn test_parse_osv_response_invalid_json() {
        let vulns = parse_osv_response("not json");
        assert!(vulns.is_empty());
    }

    #[test]
    fn test_parse_osv_response_cvss_severity() {
        let json = r#"{
            "vulns": [
                {
                    "id": "GHSA-yyyy",
                    "summary": "Critical issue",
                    "aliases": ["CVE-2024-5678"],
                    "severity": [
                        { "type": "CVSS_V3", "score": "9.8" }
                    ]
                }
            ]
        }"#;
        let vulns = parse_osv_response(json);
        assert_eq!(vulns.len(), 1);
        // Falls back to CVSS score when database_specific.severity is absent
        assert_eq!(vulns[0].severity.as_deref(), Some("9.8"));
    }
}
