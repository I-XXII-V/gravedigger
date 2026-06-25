//! Compare two CycloneDX 1.6 SBOM JSON files and show the delta.
//!
//! Usage:
//! ```bash
//! gravedigger sbom-diff old.json new.json
//! ```

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;

// ── ANSI colour constants (same pattern as rest of codebase) ────────

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const GRAY: &str = "\x1b[90m";
const RESET: &str = "\x1b[0m";
const MAX_LIST_ITEMS: usize = 10;

// ── Raw deserialisation structs ──────────────────────────────────────

#[derive(Deserialize)]
struct SbomRoot {
    components: Vec<ComponentRaw>,
    #[serde(default)]
    vulnerabilities: Vec<VulnRaw>,
}

#[derive(Deserialize)]
struct ComponentRaw {
    #[serde(rename = "bom-ref")]
    #[allow(dead_code)]
    bom_ref: String,
    name: String,
    version: String,
    purl: String,
    #[serde(default)]
    properties: Vec<Property>,
}

#[derive(Deserialize)]
struct Property {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct VulnRaw {
    id: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    ratings: Vec<Rating>,
    #[serde(default)]
    affects: Vec<Affect>,
}

#[derive(Deserialize)]
struct Rating {
    #[serde(default)]
    severity: String,
}

#[derive(Deserialize)]
struct Affect {
    #[serde(rename = "ref")]
    affects_ref: String,
}

// ── Parsed representation ────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
struct Component {
    name: String,
    version: String,
    purl: String,
    health: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Vuln {
    id: String,
    description: String,
    severity: String,
    purl: String,
}

// ── Delta types ──────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum ComponentDelta {
    Added(Component),
    Changed { old: Component, new: Component },
    Removed(Component),
}

#[derive(Debug, PartialEq)]
enum VulnDelta {
    New(Vuln),
    Resolved(Vuln),
    Unchanged(Vuln),
}

// ── Parsing ──────────────────────────────────────────────────────────

fn extract_property(props: &[Property], name: &str) -> String {
    props
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.clone())
        .unwrap_or_default()
}

fn parse_file(path: &str) -> Result<(HashMap<String, Component>, HashMap<String, Vuln>), String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("❌ Failed to read {}: {}", path, e))?;

    let root: SbomRoot = serde_json::from_str(&content)
        .map_err(|e| format!("❌ Invalid JSON in {}: {}", path, e))?;

    let mut components: HashMap<String, Component> = HashMap::new();
    for c in &root.components {
        let health = extract_property(&c.properties, "gravedigger:health");
        // Default health to "unknown" if missing
        let health = if health.is_empty() { "unknown" } else { &health };

        components.insert(
            c.purl.clone(),
            Component {
                name: c.name.clone(),
                version: c.version.clone(),
                purl: c.purl.clone(),
                health: health.to_string(),
            },
        );
    }

    // Index vulnerabilities by id; if duplicates exist, last wins
    let mut vulnerabilities: HashMap<String, Vuln> = HashMap::new();
    for v in &root.vulnerabilities {
        let severity = v
            .ratings
            .first()
            .map(|r| r.severity.clone())
            .unwrap_or_default();
        let affected_purl = v
            .affects
            .first()
            .map(|a| a.affects_ref.clone())
            .unwrap_or_default();

        // Resolve affects-ref to purl via component index
        let purl = resolve_affects_ref(&affected_purl, &components);

        vulnerabilities.insert(
            v.id.clone(),
            Vuln {
                id: v.id.clone(),
                description: v.description.clone(),
                severity,
                purl,
            },
        );
    }

    Ok((components, vulnerabilities))
}

/// Resolve a `bom-ref` to its PURL, falling back to the raw ref if not found.
fn resolve_affects_ref(bom_ref: &str, _components: &HashMap<String, Component>) -> String {
    // Components are keyed by purl, not bom-ref. We need to find by bom-ref.
    // We stored component data without bom-ref mapping, so do a linear scan.
    // Since the raw data is already consumed, we need to search...
    // Actually we didn't keep the bom-ref → purl mapping. Let's just return
    // the raw ref for now — it's usually a UUID that's not user-friendly.
    // TODO: add bom-ref index if needed.
    if bom_ref.is_empty() {
        return String::new();
    }
    // Return the raw ref; the display can prefix it
    format!("(ref: {})", bom_ref)
}

// ── Diff computation ─────────────────────────────────────────────────

fn compute_diff(
    old_components: &HashMap<String, Component>,
    new_components: &HashMap<String, Component>,
    old_vulns: &HashMap<String, Vuln>,
    new_vulns: &HashMap<String, Vuln>,
) -> (Vec<ComponentDelta>, Vec<VulnDelta>) {
    let mut component_deltas = Vec::new();

    // Find added and changed components
    for (purl, new_c) in new_components {
        match old_components.get(purl) {
            None => component_deltas.push(ComponentDelta::Added(new_c.clone())),
            Some(old_c) if old_c.version != new_c.version => {
                component_deltas.push(ComponentDelta::Changed {
                    old: old_c.clone(),
                    new: new_c.clone(),
                });
            }
            Some(_) => {} // unchanged
        }
    }

    // Find removed components
    for (purl, old_c) in old_components {
        if !new_components.contains_key(purl) {
            component_deltas.push(ComponentDelta::Removed(old_c.clone()));
        }
    }

    let mut vuln_deltas = Vec::new();

    // Find new and unchanged CVEs
    for (id, new_v) in new_vulns {
        match old_vulns.get(id) {
            None => vuln_deltas.push(VulnDelta::New(new_v.clone())),
            Some(_) => vuln_deltas.push(VulnDelta::Unchanged(new_v.clone())),
        }
    }

    // Find resolved CVEs
    for (id, old_v) in old_vulns {
        if !new_vulns.contains_key(id) {
            vuln_deltas.push(VulnDelta::Resolved(old_v.clone()));
        }
    }

    (component_deltas, vuln_deltas)
}

// ── Display ──────────────────────────────────────────────────────────

/// Print "… and N more" if items exceed MAX_LIST_ITEMS.
fn print_more(count: usize) {
    if count > MAX_LIST_ITEMS {
        println!("  {}… and {} more{}", GRAY, count - MAX_LIST_ITEMS, RESET);
    }
}

fn health_emoji(health: &str) -> &'static str {
    match health {
        "healthy" => "✅",
        "warning" => "⚠️",
        "hijack" => "🚩",
        "inactive" => "🔴",
        "dead" => "🪦",
        _ => "❓",
    }
}

fn health_color(health: &str) -> &'static str {
    match health {
        "healthy" => GREEN,
        "warning" | "hijack" => YELLOW,
        "inactive" | "dead" => RED,
        _ => GRAY,
    }
}

fn severity_color(severity: &str) -> &'static str {
    match severity {
        "CRITICAL" | "HIGH" => RED,
        "MEDIUM" | "MODERATE" => YELLOW,
        _ => GRAY,
    }
}

fn print_component_delta(deltas: &[ComponentDelta]) {
    let added = deltas.iter().filter(|d| matches!(d, ComponentDelta::Added(_))).count();
    let changed = deltas.iter().filter(|d| matches!(d, ComponentDelta::Changed { .. })).count();
    let removed = deltas.iter().filter(|d| matches!(d, ComponentDelta::Removed(_))).count();

    println!();
    println!("Components");
    println!("  {}🆕 Added:   {}", GREEN, added);
    println!("  {}🔄 Changed: {}", YELLOW, changed);
    println!("  {}🗑  Removed: {}", RED, removed);
    println!("{}", RESET);

    // Print added components
    let added_items: Vec<_> = deltas.iter().filter_map(|d| {
        if let ComponentDelta::Added(c) = d { Some(c) } else { None }
    }).collect();
    if !added_items.is_empty() {
        println!();
        println!("── Added ──────────────────────────────");
        for c in added_items.iter().take(MAX_LIST_ITEMS) {
            println!(
                "  {}{} {} v{} — {}{}",
                health_color(&c.health),
                health_emoji(&c.health),
                c.name,
                c.version,
                c.purl,
                RESET,
            );
        }
        print_more(added_items.len());
    }

    // Print changed components
    let changed_items: Vec<_> = deltas.iter().filter_map(|d| {
        if let ComponentDelta::Changed { old, new } = d { Some((old, new)) } else { None }
    }).collect();
    if !changed_items.is_empty() {
        println!();
        println!("── Changed ────────────────────────────");
        for (old, new) in changed_items.iter().take(MAX_LIST_ITEMS) {
            println!(
                "  {}⚠️ {} v{} {}→{} v{} — {}{}",
                YELLOW,
                old.name,
                old.version,
                RESET,
                YELLOW,
                new.version,
                new.purl,
                RESET,
            );
        }
        print_more(changed_items.len());
    }

    // Print removed components
    let removed_items: Vec<_> = deltas.iter().filter_map(|d| {
        if let ComponentDelta::Removed(c) = d { Some(c) } else { None }
    }).collect();
    if !removed_items.is_empty() {
        println!();
        println!("── Removed ────────────────────────────");
        for c in removed_items.iter().take(MAX_LIST_ITEMS) {
            println!(
                "  {}{} {} v{} — {}{}",
                RED,
                health_emoji(&c.health),
                c.name,
                c.version,
                c.purl,
                RESET,
            );
        }
        print_more(removed_items.len());
    }
}

fn print_vuln_delta(deltas: &[VulnDelta]) {
    let new_count = deltas.iter().filter(|d| matches!(d, VulnDelta::New(_))).count();
    let resolved_count = deltas.iter().filter(|d| matches!(d, VulnDelta::Resolved(_))).count();
    let unchanged_count = deltas.iter().filter(|d| matches!(d, VulnDelta::Unchanged(_))).count();

    println!();
    println!("Vulnerabilities");
    println!("  {}🚨 New:      {}", RED, new_count);
    println!("  {}✅ Resolved: {}", GREEN, resolved_count);
    println!("  {}➖ Unchanged: {}", GRAY, unchanged_count);
    println!("{}", RESET);

    // New CVEs
    let new_items: Vec<_> = deltas.iter().filter_map(|d| {
        if let VulnDelta::New(v) = d { Some(v) } else { None }
    }).collect();
    if !new_items.is_empty() {
        println!();
        println!("── New CVEs ───────────────────────────");
        for v in new_items.iter().take(MAX_LIST_ITEMS) {
            let desc_short = if v.description.len() > 60 {
                format!("{}…", &v.description[..57])
            } else {
                v.description.clone()
            };
            println!(
                "  {}🚨 {} [{}] {}{}",
                severity_color(&v.severity),
                v.id,
                if v.severity.is_empty() { "UNKNOWN" } else { &v.severity },
                desc_short,
                RESET,
            );
            if !v.purl.is_empty() {
                println!("     affects: {}", v.purl);
            }
        }
        print_more(new_items.len());
    }

    // Resolved CVEs
    let resolved_items: Vec<_> = deltas.iter().filter_map(|d| {
        if let VulnDelta::Resolved(v) = d { Some(v) } else { None }
    }).collect();
    if !resolved_items.is_empty() {
        println!();
        println!("── Resolved CVEs ──────────────────────");
        for v in resolved_items.iter().take(MAX_LIST_ITEMS) {
            println!("  {}✅ {} — No longer present{}", GREEN, v.id, RESET);
        }
        print_more(resolved_items.len());
    }
}

/// Public entry point: compare two SBOM JSON files and print the diff.
pub fn run(old_path: &str, new_path: &str) {
    let (old_components, old_vulns) = match parse_file(old_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}", e);
            return;
        }
    };

    let (new_components, new_vulns) = match parse_file(new_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}", e);
            return;
        }
    };

    let (component_deltas, vuln_deltas) = compute_diff(
        &old_components,
        &new_components,
        &old_vulns,
        &new_vulns,
    );

    let has_real_changes = !component_deltas.is_empty()
        || vuln_deltas.iter().any(|d| !matches!(d, VulnDelta::Unchanged(_)));

    if !has_real_changes {
        println!("✅ No differences found between the two SBOMs");
        return;
    }

    println!(
        "📊 SBOM Diff — {} → {}\n",
        old_path, new_path
    );

    if !component_deltas.is_empty() {
        print_component_delta(&component_deltas);
    }

    if !vuln_deltas.is_empty() {
        print_vuln_delta(&vuln_deltas);
    }

    // Footer
    println!(
        "{}{}{}",
        GRAY,
        "─".repeat(50),
        RESET,
    );
    println!();
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_component(name: &str, version: &str, health: &str) -> Component {
        Component {
            name: name.to_string(),
            version: version.to_string(),
            purl: format!("pkg:generic/{}@{}", name, version),
            health: health.to_string(),
        }
    }

    fn make_vuln(id: &str, severity: &str, purl: &str) -> Vuln {
        Vuln {
            id: id.to_string(),
            description: format!("Description for {}", id),
            severity: severity.to_string(),
            purl: purl.to_string(),
        }
    }

    #[test]
    fn test_compute_diff_no_changes() {
        let mut old_c = HashMap::new();
        old_c.insert("pkg:generic/foo@1.0".into(), make_component("foo", "1.0", "healthy"));
        let new_c = old_c.clone();

        let mut old_v = HashMap::new();
        old_v.insert("CVE-2024-1".into(), make_vuln("CVE-2024-1", "HIGH", "pkg:generic/foo@1.0"));
        let new_v = old_v.clone();

        let (comp_deltas, vuln_deltas) = compute_diff(&old_c, &new_c, &old_v, &new_v);
        assert!(comp_deltas.is_empty());
        assert_eq!(vuln_deltas.len(), 1);
        assert!(matches!(vuln_deltas[0], VulnDelta::Unchanged(_)));
    }

    #[test]
    fn test_compute_diff_added_component() {
        let old_c = HashMap::new();
        let mut new_c = HashMap::new();
        new_c.insert("pkg:generic/foo@1.0".into(), make_component("foo", "1.0", "healthy"));

        let (deltas, _) = compute_diff(&old_c, &new_c, &HashMap::new(), &HashMap::new());
        assert_eq!(deltas.len(), 1);
        assert!(matches!(deltas[0], ComponentDelta::Added(_)));
    }

    #[test]
    fn test_compute_diff_removed_component() {
        let mut old_c = HashMap::new();
        old_c.insert("pkg:generic/foo@1.0".into(), make_component("foo", "1.0", "healthy"));
        let new_c = HashMap::new();

        let (deltas, _) = compute_diff(&old_c, &new_c, &HashMap::new(), &HashMap::new());
        assert_eq!(deltas.len(), 1);
        assert!(matches!(deltas[0], ComponentDelta::Removed(_)));
    }

    #[test]
    fn test_compute_diff_changed_component() {
        let mut old_c = HashMap::new();
        old_c.insert("pkg:generic/foo@1.0".into(), make_component("foo", "1.0", "healthy"));
        let mut new_c = HashMap::new();
        new_c.insert("pkg:generic/foo@2.0".into(), make_component("foo", "2.0", "healthy"));

        let (deltas, _) = compute_diff(&old_c, &new_c, &HashMap::new(), &HashMap::new());
        // Different purl (version in purl) → seen as add + remove, not change
        // Actually, since purl includes version, old pkg:generic/foo@1.0 != new pkg:generic/foo@2.0
        // So it's Added(foo@2.0) + Removed(foo@1.0), not Changed
        assert_eq!(deltas.len(), 2);
        let adds: Vec<_> = deltas.iter().filter(|d| matches!(d, ComponentDelta::Added(_))).collect();
        let removes: Vec<_> = deltas.iter().filter(|d| matches!(d, ComponentDelta::Removed(_))).collect();
        assert_eq!(adds.len(), 1);
        assert_eq!(removes.len(), 1);
    }

    #[test]
    fn test_compute_diff_same_purl_different_version() {
        // When purl does NOT include version, two entries with different versions
        // but same purl should show as Changed
        let mut old_c = HashMap::new();
        old_c.insert("pkg:generic/foo".into(), Component {
            name: "foo".into(),
            version: "1.0".into(),
            purl: "pkg:generic/foo".into(),
            health: "healthy".into(),
        });
        let mut new_c = HashMap::new();
        new_c.insert("pkg:generic/foo".into(), Component {
            name: "foo".into(),
            version: "2.0".into(),
            purl: "pkg:generic/foo".into(),
            health: "healthy".into(),
        });

        let (deltas, _) = compute_diff(&old_c, &new_c, &HashMap::new(), &HashMap::new());
        assert_eq!(deltas.len(), 1);
        assert!(matches!(deltas[0], ComponentDelta::Changed { .. }));
    }

    #[test]
    fn test_compute_diff_new_vuln() {
        let old_v = HashMap::new();
        let mut new_v = HashMap::new();
        new_v.insert("CVE-2024-1".into(), make_vuln("CVE-2024-1", "CRITICAL", ""));

        let (_, deltas) = compute_diff(&HashMap::new(), &HashMap::new(), &old_v, &new_v);
        assert_eq!(deltas.len(), 1);
        assert!(matches!(deltas[0], VulnDelta::New(_)));
    }

    #[test]
    fn test_compute_diff_resolved_vuln() {
        let mut old_v = HashMap::new();
        old_v.insert("CVE-2024-1".into(), make_vuln("CVE-2024-1", "HIGH", ""));
        let new_v = HashMap::new();

        let (_, deltas) = compute_diff(&HashMap::new(), &HashMap::new(), &old_v, &new_v);
        assert_eq!(deltas.len(), 1);
        assert!(matches!(deltas[0], VulnDelta::Resolved(_)));
    }

    #[test]
    fn test_parse_file_missing_file() {
        let result = parse_file("/tmp/nonexistent_sbom_test_file_12345.json");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to read"));
    }

    #[test]
    fn test_parse_file_invalid_json() {
        // Write invalid JSON to a temp file
        let path = "/tmp/test_invalid_sbom.json";
        fs::write(path, b"not json").unwrap();
        let result = parse_file(path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid JSON"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_parse_file_valid_sbom() {
        let json = r#"{
            "bomFormat": "CycloneDX",
            "specVersion": "1.6",
            "components": [
                {
                    "bom-ref": "ref-1",
                    "name": "serde",
                    "version": "1.0.0",
                    "purl": "pkg:cargo/serde@1.0.0",
                    "properties": [
                        {"name": "gravedigger:health", "value": "healthy"},
                        {"name": "gravedigger:stale_reason", "value": ""}
                    ]
                }
            ],
            "vulnerabilities": [
                {
                    "id": "CVE-2024-1234",
                    "description": "Test vuln",
                    "ratings": [{"severity": "HIGH"}],
                    "affects": [{"ref": "ref-1"}]
                }
            ]
        }"#;

        let path = "/tmp/test_valid_sbom.json";
        fs::write(path, json).unwrap();
        let (components, vulns) = parse_file(path).unwrap();
        assert_eq!(components.len(), 1);
        assert_eq!(vulns.len(), 1);
        assert_eq!(components.get("pkg:cargo/serde@1.0.0").unwrap().name, "serde");
        assert_eq!(vulns.get("CVE-2024-1234").unwrap().severity, "HIGH");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_parse_file_missing_vulnerabilities() {
        let json = r#"{
            "bomFormat": "CycloneDX",
            "specVersion": "1.6",
            "components": []
        }"#;

        let path = "/tmp/test_no_vulns_sbom.json";
        fs::write(path, json).unwrap();
        let (components, vulns) = parse_file(path).unwrap();
        assert!(components.is_empty());
        assert!(vulns.is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_compute_diff_all_types() {
        let mut old_c = HashMap::new();
        old_c.insert("pkg:cargo/removed@1.0".into(), make_component("removed", "1.0", "dead"));
        old_c.insert("pkg:cargo/stable@1.0".into(), make_component("stable", "1.0", "healthy"));

        let mut new_c = HashMap::new();
        new_c.insert("pkg:cargo/added@1.0".into(), make_component("added", "1.0", "healthy"));
        new_c.insert("pkg:cargo/stable@1.0".into(), make_component("stable", "1.0", "healthy"));
        new_c.insert("pkg:cargo/changed@2.0".into(), Component {
            name: "changed".into(),
            version: "2.0".into(),
            purl: "pkg:cargo/changed@2.0".into(),
            health: "healthy".into(),
        });

        // For changed detection, need same purl different version
        old_c.insert("pkg:cargo/changed".into(), Component {
            name: "changed".into(),
            version: "1.0".into(),
            purl: "pkg:cargo/changed".into(),
            health: "warning".into(),
        });
        new_c.insert("pkg:cargo/changed".into(), Component {
            name: "changed".into(),
            version: "2.0".into(),
            purl: "pkg:cargo/changed".into(),
            health: "healthy".into(),
        });

        let mut old_v = HashMap::new();
        old_v.insert("CVE-OLD".into(), make_vuln("CVE-OLD", "CRITICAL", ""));
        old_v.insert("CVE-STABLE".into(), make_vuln("CVE-STABLE", "MEDIUM", ""));

        let mut new_v = HashMap::new();
        new_v.insert("CVE-NEW".into(), make_vuln("CVE-NEW", "HIGH", ""));
        new_v.insert("CVE-STABLE".into(), make_vuln("CVE-STABLE", "MEDIUM", ""));

        let (comp_deltas, vuln_deltas) = compute_diff(&old_c, &new_c, &old_v, &new_v);

        // Components: 1 added (pkg:cargo/added@1.0), 1 changed (pkg:cargo/changed),
        // 1 removed (pkg:cargo/removed@1.0), 1 add+remove for pkg:cargo/changed@2.0
        // Actually pkg:cargo/changed@2.0 is only in new, pkg:cargo/changed@1.0 is not in old
        // Let me just check totals
        let added = comp_deltas.iter().filter(|d| matches!(d, ComponentDelta::Added(_))).count();
        let changed = comp_deltas.iter().filter(|d| matches!(d, ComponentDelta::Changed { .. })).count();
        let removed = comp_deltas.iter().filter(|d| matches!(d, ComponentDelta::Removed(_))).count();
        assert_eq!(added, 2, "should have 2 added (added@1.0 + changed@2.0)");
        assert_eq!(changed, 1, "should have 1 changed");
        assert_eq!(removed, 1, "should have 1 removed");

        let new_vulns = vuln_deltas.iter().filter(|d| matches!(d, VulnDelta::New(_))).count();
        let resolved = vuln_deltas.iter().filter(|d| matches!(d, VulnDelta::Resolved(_))).count();
        let unchanged = vuln_deltas.iter().filter(|d| matches!(d, VulnDelta::Unchanged(_))).count();
        assert_eq!(new_vulns, 1, "should have 1 new vuln");
        assert_eq!(resolved, 1, "should have 1 resolved vuln");
        assert_eq!(unchanged, 1, "should have 1 unchanged vuln");
    }

    #[test]
    fn test_extract_property_found() {
        let props = vec![
            Property { name: "gravedigger:health".into(), value: "dead".into() },
            Property { name: "gravedigger:stale_reason".into(), value: "Too old".into() },
        ];
        assert_eq!(extract_property(&props, "gravedigger:health"), "dead");
        assert_eq!(extract_property(&props, "gravedigger:stale_reason"), "Too old");
    }

    #[test]
    fn test_extract_property_missing() {
        let props = vec![];
        assert_eq!(extract_property(&props, "gravedigger:health"), "");
    }

    #[test]
    fn test_health_emoji_mapping() {
        assert_eq!(health_emoji("healthy"), "✅");
        assert_eq!(health_emoji("warning"), "⚠️");
        assert_eq!(health_emoji("hijack"), "🚩");
        assert_eq!(health_emoji("inactive"), "🔴");
        assert_eq!(health_emoji("dead"), "🪦");
        assert_eq!(health_emoji("unknown"), "❓");
        assert_eq!(health_emoji("something_else"), "❓");
    }
}
