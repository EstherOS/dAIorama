//! The release automation's verifier: `packaging/release/verify.sh` with the distribution
//! `minisign`. It is the independent second implementation of the manifest check; the binary is
//! the offline one. Tests that need a live `minisign` skip cleanly on benches without it, but the
//! empty-trust refusal is exercised everywhere.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packaging/release/verify.sh")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/release")
}

/// A temporary install layout: verify.sh locates trusted-keys beside itself, so each case gets
/// its own copy of both rather than touching the repo's empty list.
fn layout(trusted_keys: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(script(), dir.path().join("verify.sh")).unwrap();
    std::fs::write(dir.path().join("trusted-keys"), trusted_keys).unwrap();
    dir
}

fn run(dir: &Path, manifest: &Path, signature: &Path, artifact: Option<&Path>) -> Output {
    let mut command = Command::new("sh");
    command
        .arg(dir.join("verify.sh"))
        .arg(manifest)
        .arg(signature);
    if let Some(artifact) = artifact {
        command.arg(artifact);
    }
    command.output().unwrap()
}

fn minisign_available() -> bool {
    Command::new("minisign")
        .arg("-v")
        .output()
        .map(|output| output.status.success() || output.status.code().is_some())
        .unwrap_or(false)
}

#[test]
fn empty_trust_list_refuses_before_minisign_is_needed() {
    let dir = layout("# empty\n");
    let output = run(
        dir.path(),
        &fixtures().join("manifest.json"),
        &fixtures().join("manifest.json.minisig"),
        None,
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no trusted release key provisioned"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_script_agrees_with_the_binary_on_a_real_signature() {
    if !minisign_available() {
        eprintln!("skipped: minisign is not installed on this bench");
        return;
    }
    let dir = layout(include_str!("fixtures/release/test.pub"));
    let output = run(
        dir.path(),
        &fixtures().join("manifest.json"),
        &fixtures().join("manifest.json.minisig"),
        None,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("manifest 0.1.0 (protocol 0.1.0)"),
        "{stdout}"
    );
    assert!(stdout.contains("signed by 504C963C3ABCA66B"), "{stdout}");

    let artifact = dir.path().join("daiorama-0.1.0-x86_64-unknown-linux-gnu");
    std::fs::write(&artifact, b"x").unwrap();
    let output = run(
        dir.path(),
        &fixtures().join("manifest.json"),
        &fixtures().join("manifest.json.minisig"),
        Some(&artifact),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not match the manifest row"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = run(
        dir.path(),
        &fixtures().join("manifest.json"),
        &fixtures().join("manifest.json.minisig"),
        Some(&dir.path().join("not-in-the-manifest")),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is not in the manifest"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn tampering_is_a_verification_failure_not_an_untrusted_signer() {
    if !minisign_available() {
        eprintln!("skipped: minisign is not installed on this bench");
        return;
    }
    let dir = layout(include_str!("fixtures/release/test.pub"));
    let manifest = dir.path().join("manifest.json");
    let text = std::fs::read_to_string(fixtures().join("manifest.json"))
        .unwrap()
        .replace("\"version\":\"0.1.0\"", "\"version\":\"0.1.1\"");
    std::fs::write(&manifest, text).unwrap();
    let output = run(
        dir.path(),
        &manifest,
        &fixtures().join("manifest.json.minisig"),
        None,
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("verification failed"), "{stderr}");
    assert!(!stderr.contains("untrusted key"), "{stderr}");
}
