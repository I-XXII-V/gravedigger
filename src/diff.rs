use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::display::health_color;
use crate::types::PackageResult;

// ── Type aliases ─────────────────────────────────────────────────────

/// Parser function signature: parses a lockfile content string into
/// `(name, version)` pairs.
type ParseFn = fn(&str) -> Result<Vec<(String, String)>, String>;

/// Scanner function signature: fetches health info for a single dependency.
type ScanFn = fn(&str, &str) -> PackageResult;

/// Internal type for collecting parallel scan results with ordering.
type ScoredDep<'a> = (usize, &'a DepDelta, Option<PackageResult>);

// ── Delta types ──────────────────────────────────────────────────────

/// A dependency change between two lockfile revisions.
#[derive(Debug, PartialEq)]
pub enum DepDelta {
    /// `(name, new_version)` — package was added
    Added(String, String),
    /// `(name, old_version, new_version)` — package version changed
    Upgraded(String, String, String),
    /// `(name, old_version)` — package was removed
    Removed(String, String),
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Convert a health string (as stored in `PackageResult.health`) back to
/// its display emoji.
fn string_to_emoji(s: &str) -> &'static str {
    match s {
        "healthy" => "✅",
        "warning" => "⚠️",
        "hijack" => "🚩",
        "inactive" => "🔴",
        "dead" => "🪦",
        _ => "❓",
    }
}

/// Fetch the content of a file at a given Git commit using `git show`.
/// Returns an error if the command fails or the path does not exist at
/// that commit.
fn git_show(commit: &str, path: &str) -> Result<String, String> {
    let ref_spec = format!("{}:{}", commit, path);
    let output = Command::new("git")
        .args(["show", &ref_spec])
        .output()
        .map_err(|e| format!("Failed to run git show: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not exist")
            || stderr.contains("not found")
            || stderr.contains("bad object")
            || stderr.contains("invalid object name")
        {
            return Err(format!(
                "Path '{}' does not exist at commit '{}'",
                path, commit
            ));
        }
        return Err(format!("git show failed: {}", stderr.trim()));
    }

    String::from_utf8(output.stdout)
        .map_err(|e| format!("Non-UTF8 output from git show: {}", e))
}

// ── Diff computation ─────────────────────────────────────────────────

/// Compute the delta between two dependency lists.
///
/// Comparison is by exact package name (string equality).  This is a
/// known limitation for PyPI where `Foo_Bar` and `foo-bar` are the same
/// package — case normalization is intentionally not applied for
/// performance and simplicity.
pub fn compute_diff(old: &[(String, String)], new: &[(String, String)]) -> Vec<DepDelta> {
    let old_map: HashMap<&str, &str> =
        old.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    let new_map: HashMap<&str, &str> =
        new.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();

    let mut deltas = Vec::new();

    // Added and upgraded
    for (name, new_ver) in &new_map {
        match old_map.get(name) {
            Some(old_ver) if **old_ver != **new_ver => {
                deltas.push(DepDelta::Upgraded(
                    name.to_string(),
                    old_ver.to_string(),
                    new_ver.to_string(),
                ));
            }
            None => {
                deltas.push(DepDelta::Added(name.to_string(), new_ver.to_string()));
            }
            _ => { /* unchanged */ }
        }
    }

    // Removed
    for (name, old_ver) in &old_map {
        if !new_map.contains_key(name) {
            deltas.push(DepDelta::Removed(name.to_string(), old_ver.to_string()));
        }
    }

    deltas
}

// ── Ecosystem auto-detection ─────────────────────────────────────────

/// Detect which lockfile ecosystem is present in the current directory.
/// Returns `(ecosystem_name, lockfile_path)`.
fn detect_ecosystem() -> Result<(String, String), String> {
    if fs::metadata("Cargo.lock").is_ok() {
        Ok(("cargo".to_string(), "Cargo.lock".to_string()))
    } else if fs::metadata("package-lock.json").is_ok() {
        Ok(("npm".to_string(), "package-lock.json".to_string()))
    } else if fs::metadata("poetry.lock").is_ok() {
        Ok(("pypi".to_string(), "poetry.lock".to_string()))
    } else if fs::metadata("Pipfile.lock").is_ok() {
        Ok(("pypi".to_string(), "Pipfile.lock".to_string()))
    } else if fs::metadata("go.mod").is_ok() {
        Ok(("go".to_string(), "go.mod".to_string()))
    } else {
        Err("No supported lockfile found (Cargo.lock, package-lock.json, poetry.lock, Pipfile.lock, go.mod)".to_string())
    }
}

// ── Parallel health scanning ─────────────────────────────────────────

/// Scan changed dependencies in parallel using `thread::scope`.
/// Only packages that were added or upgraded are scanned (removed deps
/// are shown without health data).
fn scan_deps(
    deltas: &[DepDelta],
    scan: fn(&str, &str) -> PackageResult,
) -> Vec<(&DepDelta, Option<PackageResult>)> {
    let results: Arc<Mutex<Vec<ScoredDep>>> = Arc::new(Mutex::new(Vec::new()));

    thread::scope(|s| {
        for (i, delta) in deltas.iter().enumerate() {
            let results = Arc::clone(&results);
            s.spawn(move || {
                let result = match delta {
                    DepDelta::Added(name, version) | DepDelta::Upgraded(name, version, _) => {
                        Some(scan(name, version))
                    }
                    DepDelta::Removed(_, _) => None,
                };
                let mut guard = results.lock().expect("diff results mutex poisoned");
                guard.push((i, delta, result));
            });
        }
    });

    let guard = results.lock().expect("diff results mutex poisoned");
    let mut sorted: Vec<_> = guard.clone();
    sorted.sort_by_key(|(i, _, _)| *i);
    sorted.into_iter().map(|(_, d, r)| (d, r)).collect()
}

// ── Display ──────────────────────────────────────────────────────────

/// Display the diff results to stdout, grouped by delta type.
/// Uses `health_color` from the display module for consistent coloring.
fn display_diff(
    eco: &str,
    deltas: &[DepDelta],
    scanned: &[(&DepDelta, Option<PackageResult>)],
    unchanged_count: usize,
) {
    println!(
        "\n📦 {} dependency diff — {} changed, {} unchanged\n",
        eco,
        deltas.len(),
        unchanged_count
    );

    let mut added = String::new();
    let mut upgraded = String::new();
    let mut removed = String::new();

    // Group by delta type in order: Added → Upgraded → Removed
    for (delta, result) in scanned {
        match delta {
            DepDelta::Added(name, version) => {
                let health_line = result
                    .as_ref()
                    .map(|r| {
                        let emoji = string_to_emoji(&r.health);
                        format!(
                            "   {}{}{} {} v{} — {}{}",
                            health_color(emoji),
                            emoji,
                            "\x1b[0m",
                            name,
                            version,
                            r.description.as_deref().unwrap_or("no description"),
                            vulns_suffix(&r.vulns),
                        )
                    })
                    .unwrap_or_else(|| {
                        format!("   {}?{} {} v{}", "\x1b[90m", "\x1b[0m", name, version)
                    });
                added.push_str(&health_line);
                added.push('\n');
            }
            DepDelta::Upgraded(name, old_ver, new_ver) => {
                let health_line = result
                    .as_ref()
                    .map(|r| {
                        let emoji = string_to_emoji(&r.health);
                        format!(
                            "   {}{}{} {} v{} → v{} ({}){}",
                            health_color(emoji),
                            emoji,
                            "\x1b[0m",
                            name,
                            old_ver,
                            new_ver,
                            r.description.as_deref().unwrap_or("no description"),
                            vulns_suffix(&r.vulns),
                        )
                    })
                    .unwrap_or_else(|| {
                        format!(
                            "   {}?{} {} v{} → v{}",
                            "\x1b[90m", "\x1b[0m", name, old_ver, new_ver
                        )
                    });
                upgraded.push_str(&health_line);
                upgraded.push('\n');
            }
            DepDelta::Removed(name, version) => {
                removed.push_str(&format!(
                    "   {}🗑 {} v{} removed{}\n",
                    "\x1b[90m",
                    name,
                    version,
                    "\x1b[0m"
                ));
            }
        }
    }

    if !added.is_empty() {
        println!("\x1b[1m🟢 Added:\x1b[0m\n{}", added.trim_end());
    }
    if !upgraded.is_empty() {
        println!("\x1b[1m🔵 Upgraded:\x1b[0m\n{}", upgraded.trim_end());
    }
    if !removed.is_empty() {
        println!("\x1b[1m🔴 Removed:\x1b[0m\n{}", removed.trim_end());
    }
}

/// Format a vuln summary suffix (empty if no vulns).
fn vulns_suffix(vulns: &[crate::types::VulnInfo]) -> String {
    if vulns.is_empty() {
        return String::new();
    }
    let ids: Vec<&str> = vulns
        .iter()
        .flat_map(|v| v.aliases.first().map(|a| a.as_str()).or(Some(&v.id)))
        .take(3)
        .collect();
    format!("  \x1b[31m🚨 {} CVE{}: {}\x1b[0m", vulns.len(), if vulns.len() == 1 { "" } else { "s" }, ids.join(", "))
}

// ── Standard ecosystem diff ──────────────────────────────────────────

/// Run a diff for ecosystems with a single lockfile format
/// (Cargo.lock, package-lock.json, go.mod).
fn run_diff_standard(
    old_ref: &str,
    lockfile: &str,
    eco: &str,
    parse: ParseFn,
    scan: ScanFn,
) {
    // Get old lockfile content from git
    let old_content = match git_show(old_ref, lockfile) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ {}", e);
            return;
        }
    };

    // Parse old deps
    let old_deps = match parse(&old_content) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("❌ Failed to parse old {}: {}", lockfile, e);
            return;
        }
    };

    // Read and parse current deps
    let current_content = match fs::read_to_string(lockfile) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ Failed to read {}: {}", lockfile, e);
            return;
        }
    };

    let new_deps = match parse(&current_content) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("❌ Failed to parse {}: {}", lockfile, e);
            return;
        }
    };

    // Compute diff
    let deltas = compute_diff(&old_deps, &new_deps);
    let unchanged = old_deps.len().max(new_deps.len()) - deltas.iter().filter(|d| matches!(d, DepDelta::Removed(_, _))).count();

    if deltas.is_empty() {
        println!("✅ No dependency changes since {}", old_ref);
        return;
    }

    // Scan changed deps in parallel
    let scanned = scan_deps(&deltas, scan);

    // Display
    display_diff(eco, &deltas, &scanned, unchanged);
}

// ── PyPI diff (handles both poetry.lock and Pipfile.lock) ────────────

/// Run a diff for PyPI projects.  Auto-detects whether `poetry.lock` or
/// `Pipfile.lock` was tracked at the old commit and selects the
/// appropriate parser.
fn run_diff_pypi(old_ref: &str) {
    let (lockfile, parse): (&str, ParseFn) =
        if fs::metadata("poetry.lock").is_ok() {
            ("poetry.lock", crate::pypi::parse_poetry_lock_content)
        } else if fs::metadata("Pipfile.lock").is_ok() {
            ("Pipfile.lock", crate::pypi::parse_pipfile_lock_content)
        } else {
            eprintln!("❌ No poetry.lock or Pipfile.lock found in current directory");
            return;
        };

    run_diff_standard(old_ref, lockfile, "pypi", parse, crate::pypi::scan_single);
}

// ── Public entry point ───────────────────────────────────────────────

/// Run `vigil diff` — compare dependencies at an old Git ref with the
/// current state and show health information for changed packages.
///
/// If `ecosystem` is `None`, the ecosystem is auto-detected from lockfiles
/// present in the current directory.
pub fn run_diff(old_ref: &str, ecosystem: Option<&str>) {
    let (eco, lockfile) = match ecosystem {
        Some(e) => (e.to_string(), String::new()),
        None => match detect_ecosystem() {
            Ok((e, l)) => (e, l),
            Err(msg) => {
                eprintln!("❌ {}", msg);
                return;
            }
        },
    };

    match eco.as_str() {
        "cargo" => run_diff_standard(old_ref, &lockfile, "cargo", crate::cargo::parse_cargo_lock, crate::cargo::scan_single),
        "npm" => run_diff_standard(old_ref, &lockfile, "npm", crate::npm::parse_npm_lock, crate::npm::scan_single),
        "pypi" => run_diff_pypi(old_ref),
        "go" => run_diff_standard(old_ref, &lockfile, "go", crate::golang::parse_go_mod_content, crate::golang::scan_single),
        other => eprintln!("❌ Unknown ecosystem: {} (try cargo, npm, pypi, or go)", other),
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── compute_diff tests ──────────────────────────────────────────────

    #[test]
    fn test_compute_diff_empty() {
        let old: Vec<(String, String)> = vec![];
        let new: Vec<(String, String)> = vec![];
        let deltas = compute_diff(&old, &new);
        assert!(deltas.is_empty());
    }

    #[test]
    fn test_compute_diff_added() {
        let old: Vec<(String, String)> = vec![];
        let new = vec![("foo".into(), "1.0.0".into())];
        let deltas = compute_diff(&old, &new);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0], DepDelta::Added("foo".into(), "1.0.0".into()));
    }

    #[test]
    fn test_compute_diff_removed() {
        let old = vec![("foo".into(), "1.0.0".into())];
        let new: Vec<(String, String)> = vec![];
        let deltas = compute_diff(&old, &new);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0], DepDelta::Removed("foo".into(), "1.0.0".into()));
    }

    #[test]
    fn test_compute_diff_upgraded() {
        let old = vec![("foo".into(), "1.0.0".into())];
        let new = vec![("foo".into(), "2.0.0".into())];
        let deltas = compute_diff(&old, &new);
        assert_eq!(deltas.len(), 1);
        assert_eq!(
            deltas[0],
            DepDelta::Upgraded("foo".into(), "1.0.0".into(), "2.0.0".into())
        );
    }

    #[test]
    fn test_compute_diff_unchanged() {
        let old = vec![("foo".into(), "1.0.0".into())];
        let new = vec![("foo".into(), "1.0.0".into())];
        let deltas = compute_diff(&old, &new);
        assert!(deltas.is_empty());
    }

    #[test]
    fn test_compute_diff_mixed() {
        let old = vec![
            ("keep".into(), "1.0.0".into()),
            ("remove".into(), "1.0.0".into()),
            ("upgrade".into(), "1.0.0".into()),
        ];
        let new = vec![
            ("keep".into(), "1.0.0".into()),
            ("upgrade".into(), "2.0.0".into()),
            ("add".into(), "1.0.0".into()),
        ];
        let deltas = compute_diff(&old, &new);
        assert_eq!(deltas.len(), 3);

        // Check all expected deltas are present (HashMap iteration order
        // is non-deterministic, so check membership, not position)
        assert!(deltas.contains(&DepDelta::Added("add".into(), "1.0.0".into())));
        assert!(deltas.contains(&DepDelta::Upgraded(
            "upgrade".into(),
            "1.0.0".into(),
            "2.0.0".into()
        )));
        assert!(deltas.contains(&DepDelta::Removed("remove".into(), "1.0.0".into())));
    }

    #[test]
    fn test_compute_diff_multiple_versions_same_name() {
        // Same name with different versions should be treated as separate
        // (though this is unusual, compute_diff handles it)
        let old = vec![("foo".into(), "1.0.0".into())];
        let new = vec![("foo".into(), "1.0.0".into()), ("foo".into(), "2.0.0".into())];
        // HashMap deduplicates by name, so the second entry overwrites
        let deltas = compute_diff(&old, &new);
        assert!(deltas.is_empty() || deltas.len() == 1);
        // Either unchanged (if duplicate overwrites to same) or upgraded
    }

    // ── git_show tests ──────────────────────────────────────────────────

    #[test]
    fn test_git_show_current_head() {
        // This is a best-effort test: run `git show HEAD:src/diff.rs`
        // (which should exist after this file is committed, but during test
        // it won't be). We test the error path.
        match git_show("HEAD", "src/lib.rs") {
            Ok(content) => {
                // If it succeeds, content should be non-empty
                assert!(!content.is_empty());
            }
            Err(e) => {
                // Acceptable — tests run before commit
                assert!(e.contains("does not exist") || e.contains("not found"));
            }
        }
    }

    #[test]
    fn test_git_show_nonexistent_path() {
        let result = git_show("HEAD", "nonexistent/path/file.lock");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn test_git_show_invalid_ref() {
        let result = git_show("nonexistent-ref-12345", "Cargo.lock");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("does not exist"),
            "expected error about ref not existing, got: {}",
            err
        );
    }

    // ── string_to_emoji tests ───────────────────────────────────────────

    #[test]
    fn test_string_to_emoji_healthy() {
        assert_eq!(string_to_emoji("healthy"), "✅");
    }

    #[test]
    fn test_string_to_emoji_warning() {
        assert_eq!(string_to_emoji("warning"), "⚠️");
    }

    #[test]
    fn test_string_to_emoji_hijack() {
        assert_eq!(string_to_emoji("hijack"), "🚩");
    }

    #[test]
    fn test_string_to_emoji_inactive() {
        assert_eq!(string_to_emoji("inactive"), "🔴");
    }

    #[test]
    fn test_string_to_emoji_dead() {
        assert_eq!(string_to_emoji("dead"), "🪦");
    }

    #[test]
    fn test_string_to_emoji_unknown() {
        assert_eq!(string_to_emoji("unknown"), "❓");
    }

    #[test]
    fn test_string_to_emoji_fallback() {
        assert_eq!(string_to_emoji("something_else"), "❓");
    }
}
