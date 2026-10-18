//! CLI-level checks for the release manifest verbs: the trust list is empty in the repo, so the
//! binary must refuse — not panic, not trust an example key — and the generator must produce
//! parseable bytes with one object per line.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_daiorama"))
}

#[test]
fn an_empty_trust_list_is_a_real_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");
    let signature = dir.path().join("manifest.json.minisig");
    std::fs::write(&manifest, b"{}").unwrap();
    std::fs::write(&signature, b"not a signature\n").unwrap();
    let output = bin()
        .args([
            "release",
            "verify",
            "--manifest",
            manifest.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no trusted release key provisioned"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_generator_writes_a_parseable_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let core = dir.path().join("daiorama-0.1.0-x86_64-unknown-linux-gnu");
    std::fs::write(&core, b"core").unwrap();
    let out = dir.path().join("manifest.json");
    let output = bin()
        .args([
            "release",
            "manifest",
            "--version",
            "0.1.0",
            "--protocol",
            "0.1.0",
            "--released-at",
            "2026-10-18T00:00:00Z",
            "--artifact",
            &format!("core:x86_64-unknown-linux-gnu:{}", core.display()),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let text = std::fs::read_to_string(&out).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["version"], "0.1.0");
    assert_eq!(parsed["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(
        text.lines()
            .filter(|line| line.trim_start().starts_with("{\"name\""))
            .count(),
        1
    );
}
