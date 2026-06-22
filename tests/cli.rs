use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_blight");

#[test]
fn test_help_success() {
    let output = Command::new(BINARY)
        .arg("--help")
        .output()
        .expect("Failed to run blight --help");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("blight"));
    assert!(stdout.contains("--cargo"));
    assert!(stdout.contains("--npm"));
    assert!(stdout.contains("--json"));
}

#[test]
fn test_help_short() {
    let output = Command::new(BINARY)
        .arg("-h")
        .output()
        .expect("Failed to run blight -h");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("blight"));
}

#[test]
fn test_cargo_no_lockfile() {
    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let output = Command::new(BINARY)
        .args(["--cargo"])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to run blight --cargo in empty dir");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Cargo.lock not found"));
}

#[test]
fn test_who_depends_serde() {
    let output = Command::new(BINARY)
        .args(["who-depends", "serde"])
        .output()
        .expect("Failed to run blight who-depends serde");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("serde"), "Output: {}", stdout);
    assert!(
        stdout.contains("reverse dependencies"),
        "Output: {}",
        stdout
    );
}

#[test]
fn test_who_depends_short() {
    let output = Command::new(BINARY)
        .args(["wd", "serde"])
        .output()
        .expect("Failed to run blight wd serde");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("serde"));
}

#[test]
fn test_json_flag_help_still_text() {
    // --json should not affect --help output
    let output = Command::new(BINARY)
        .args(["--help", "--json"])
        .output()
        .expect("Failed to run blight --help --json");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("blight"));
}

#[test]
fn test_invalid_package_not_found() {
    // Test that a non-existent single package gives error
    let output = Command::new(BINARY)
        .arg("this-package-definitely-does-not-exist-12345")
        .output()
        .expect("Failed to run blight with invalid package");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found"), "Stderr: {}", stderr);
}
