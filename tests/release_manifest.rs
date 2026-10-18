//! CLI-level checks for the release manifest verbs: the trust list is empty in the repo, so the
//! binary must refuse — not panic, not trust an example key — and the generator must produce the
//! pinned bytes (one compact object per line) that a POSIX installer can read without `jq`.
//! The docs are read here too: the refusal strings and the manifest schema are contract.

use std::process::Command;

const SPEC_04: &str = include_str!("../docs/04-cli.md");
const SPEC_09: &str = include_str!("../docs/09-packaging.md");
const STRINGS: &str = include_str!("../src/strings.rs");
const TRUSTED: &str = include_str!("../packaging/release/trusted-keys");

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
fn the_shipped_trust_list_is_the_explicit_empty_state() {
    // 09 C2: empty until the first operator-signed release; never an example key.
    let key_lines = TRUSTED
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .count();
    assert_eq!(key_lines, 0, "the trust list must ship empty: {TRUSTED}");
}

#[test]
fn the_generator_writes_the_pinned_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let core = dir.path().join("daiorama-0.1.0-x86_64-unknown-linux-gnu");
    std::fs::write(&core, b"core").unwrap();
    let asset = dir.path().join("novnc.tar.gz");
    std::fs::write(&asset, b"novnc").unwrap();
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
            "--asset",
            &format!("novnc.tar.gz:{}", asset.display()),
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

    // One artifact/asset object per line, no whitespace inside an object.
    let rows: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("{\"name\":"))
        .collect();
    assert_eq!(rows.len(), 2, "{text}");
    for row in &rows {
        assert!(!row.contains(": "), "whitespace inside an object: {row}");
        assert!(!row.contains(", "), "whitespace inside an object: {row}");
    }

    // The installer's pinned extraction, byte for byte:
    //   grep -F '"name":"daiorama-<version>-<target>"' manifest.json
    //   sed -n 's/.*"sha256":"\([0-9a-f]\{64\}\)".*/\1/p'
    let name = "daiorama-0.1.0-x86_64-unknown-linux-gnu";
    let row = text
        .lines()
        .find(|line| line.contains(&format!("\"name\":\"{name}\"")))
        .expect("the pinned grep finds the row");
    let digest = row
        .split("\"sha256\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the pinned sed extracts the digest");
    assert_eq!(digest.len(), 64, "{row}");
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{digest}"
    );
}

#[test]
fn the_generator_refuses_an_unknown_target_like_the_verifier() {
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
            &format!("core:windows-386:{}", core.display()),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("outside the release set"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!out.exists(), "a refused manifest must not be written");
}

#[test]
fn the_docs_and_the_strings_agree_on_the_release_refusals() {
    for needle in [
        "no trusted release key provisioned",
        "release signed by an untrusted key",
        "release signature verification failed",
        "release document too large",
        "release manifest is missing",
        "release manifest field",
        "unknown release channel",
        "unknown artifact kind",
        "outside the release set",
        "duplicate release manifest row",
        "invalid artifact name",
        "invalid sha256 in artifact",
        "release manifest lists no artifacts",
    ] {
        assert!(STRINGS.contains(needle), "strings.rs lost {needle:?}");
        assert!(SPEC_04.contains(needle), "docs/04 lost {needle:?}");
    }
}

#[test]
fn spec_09_pins_the_manifest_schema_fields() {
    for needle in [
        "format` (1)",
        "`version` (core semver)",
        "`protocol` (semver)",
        "`channel` (`stable` | `beta`)",
        "`released_at` (RFC 3339 UTC",
        "artifacts[] {name, target, kind, sha256, size}",
        "assets[] {name, sha256, size}",
        "facts {client_only, manager_platforms, components}",
        "\"name\":\"daiorama-<version>-<target>\"",
        "no whitespace inside an object",
        "bounded path (128 MiB)",
    ] {
        assert!(SPEC_09.contains(needle), "docs/09 lost {needle:?}");
    }
}
