//! The install script: `packaging/release/install.sh` with the distribution `minisign`.
//!
//! The live trust decision is `minisign`'s exit code, so most cases here stub it on PATH and
//! exercise the installer's ordering (verify → read facts → select a row → hash → place). The
//! real-signature agreement between `verify.sh`, the binary and a real keypair is
//! `release_script.rs` and the release-manifest probe; these tests never invent crypto.
//! Benches without `sha256sum` skip the placement legs with one printed line.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packaging/release/install.sh")
}

fn doc() -> String {
    std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/09-packaging.md"))
        .unwrap()
}

const TEST_KEY: &str = concat!(
    "untrusted comment: minisign public key 504C963C3ABCA66B\n",
    "RWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n"
);

/// The trust block is embedded, not fetched. Pull the here-doc body out of a script text.
fn embedded_keys(text: &str) -> String {
    let marker = "<<'TRUSTED_KEYS'\n";
    let start = text.find(marker).expect("trusted-keys here-doc") + marker.len();
    let end = text[start..]
        .find("\nTRUSTED_KEYS\n")
        .expect("here-doc terminator")
        + start;
    text[start..end].to_string()
}

/// Write a copy of the script with the embedded list replaced by `keys`, so each case owns its
/// trust anchors instead of editing the repo's shipped (empty) list.
fn install_script(dir: &Path, keys: &str) -> PathBuf {
    let text = std::fs::read_to_string(script()).unwrap();
    let body = embedded_keys(&text);
    let replaced = text.replacen(&body, keys.trim_end_matches('\n'), 1);
    let path = dir.join("install.sh");
    std::fs::write(&path, replaced).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A `minisign` stub whose exit code is the case's contract. `-Vqm` is all the installer passes.
fn stub_minisign(dir: &Path, rc: i32) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("minisign");
    std::fs::write(&path, format!("#!/bin/sh\nexit {rc}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn stub_uname(dir: &Path, system: &str) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("uname");
    std::fs::write(
        &path,
        format!("#!/bin/sh\ncase $1 in -s) echo {system} ;; *) echo x86_64 ;; esac\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn sha256sum(path: &Path) -> Option<String> {
    let output = Command::new("sha256sum").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()?
            .to_string(),
    )
}

/// One-line manifest in the shipped layout (one object per line, fixed key order).
fn manifest(version: &str, target: &str, digest: &str, rows: usize) -> String {
    let row = format!(
        "{{\"name\":\"daiorama-{version}-{target}\",\"target\":\"{target}\",\"kind\":\"core\",\"sha256\":\"{digest}\",\"size\":1}}"
    );
    let artifacts = std::iter::repeat_n(row, rows)
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{{\"format\":1,\"version\":\"{version}\",\"protocol\":\"0.1.0\",\"channel\":\"stable\",\"released_at\":\"2026-10-18T00:00:00Z\",\"artifacts\":[{artifacts}],\"assets\":[],\"facts\":{{\"client_only\":false,\"manager_platforms\":[\"linux\"],\"components\":[\"core\"]}}}}"
    )
}

/// A local release directory: manifest + signature + artifact, as `--base <dir>` reads them.
fn release(dir: &Path, version: &str, manifest_text: &str, artifact: Option<&[u8]>) -> PathBuf {
    let release = dir.join("release");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(
        release.join(format!("daiorama-release-{version}.json")),
        manifest_text,
    )
    .unwrap();
    std::fs::write(
        release.join(format!("daiorama-release-{version}.json.minisig")),
        "untrusted comment: signature\nRWSIGNATURE\n",
    )
    .unwrap();
    if let Some(bytes) = artifact {
        std::fs::write(
            release.join(format!("daiorama-{version}-x86_64-unknown-linux-gnu")),
            bytes,
        )
        .unwrap();
    }
    release
}

fn run(dir: &Path, args: &[&str], bin: Option<&Path>) -> Output {
    let mut command = Command::new("sh");
    command.arg(dir.join("install.sh")).args(args);
    if let Some(bin) = bin {
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        command.env("PATH", path);
    }
    command.output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn usage_is_two_without_a_version_or_with_an_unknown_option() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);

    let output = run(dir.path(), &[], None);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(stderr(&output).contains("usage:"), "{}", stderr(&output));

    let output = run(dir.path(), &["--version", "0.1.0", "--nope"], None);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains("unknown option --nope"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn empty_trust_list_refuses_before_any_fetch() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), "");
    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--prefix",
            dir.path().join("prefix").to_str().unwrap(),
            "--base",
            "/nonexistent-release-directory",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr(&output);
    assert!(
        stderr.contains("no trusted release key provisioned"),
        "{stderr}"
    );
    assert!(!stderr.contains("cannot read"), "{stderr}");
}

#[test]
fn the_embedded_block_matches_the_shipped_trust_list() {
    let text = std::fs::read_to_string(script()).unwrap();
    let shipped = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packaging/release/trusted-keys"),
    )
    .unwrap();
    // The here-doc body carries the file's lines verbatim; the final newline is structural to
    // both and not part of the body slice.
    assert_eq!(
        embedded_keys(&text),
        shipped.trim_end_matches('\n'),
        "install.sh's embedded trust block drifted from packaging/release/trusted-keys"
    );
}

#[test]
fn the_documented_contract_names_the_installer_strings() {
    let doc = doc();
    for needle in [
        "packaging/release/install.sh",
        "~/.local/bin",
        "https://github.com/ygypt/dAIorama/releases/download/v<version>",
        "no trusted release key provisioned",
        "does not match the requested",
        "does not match the manifest row",
        "no artifact \"daiorama-<version>-<target>\" in this release",
        "installed daiorama <version> (protocol <protocol>) <target>",
        "manager preflight:",
        "client-only install: CLI and bridge",
    ] {
        assert!(doc.contains(needle), "09-packaging.md is missing: {needle}");
    }
}

#[test]
fn a_failed_signature_places_nothing_and_never_reads_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 1);
    let prefix = dir.path().join("prefix");

    // A manifest with no version at all: if parsing ran first this would refuse for the missing
    // field instead of for the signature.
    let manifest_text =
        "{\"format\":1,\"protocol\":\"0.1.0\",\"artifacts\":[],\"assets\":[],\"facts\":{}}";
    let release = release(dir.path(), "0.1.0", manifest_text, None);
    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr(&output);
    // Either side of the signature split is the right refusal here; a parse error is not. With a
    // stub signature the embedded-key id never matches, so the untrusted-signer branch is the
    // expected one.
    assert!(
        stderr.contains("release signature verification failed")
            || stderr.contains("release signed by an untrusted key"),
        "{stderr}"
    );
    assert!(!stderr.contains("missing version"), "{stderr}");
    assert!(!prefix.join("daiorama").exists());
}

#[test]
fn a_manifest_for_another_version_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let prefix = dir.path().join("prefix");

    let manifest_text = manifest("0.1.0", "x86_64-unknown-linux-gnu", &"0".repeat(64), 1);
    let release = release(dir.path(), "0.1.1", &manifest_text, None);
    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.1",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output)
            .contains("release manifest version 0.1.0 does not match the requested 0.1.1"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_unknown_target_is_a_refusal_not_a_fallback() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let prefix = dir.path().join("prefix");

    let manifest_text = manifest("0.1.0", "x86_64-unknown-linux-gnu", &"0".repeat(64), 1);
    let release = release(dir.path(), "0.1.0", &manifest_text, None);
    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--target",
            "aarch64-unknown-linux-gnu",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output)
            .contains("no artifact \"daiorama-0.1.0-aarch64-unknown-linux-gnu\" in this release"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn duplicate_rows_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let prefix = dir.path().join("prefix");

    let manifest_text = manifest("0.1.0", "x86_64-unknown-linux-gnu", &"0".repeat(64), 2);
    let release = release(dir.path(), "0.1.0", &manifest_text, None);
    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output).contains("duplicate release manifest row"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_digest_mismatch_places_nothing() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let prefix = dir.path().join("prefix");

    let manifest_text = manifest("0.1.0", "x86_64-unknown-linux-gnu", &"0".repeat(64), 1);
    let release = release(dir.path(), "0.1.0", &manifest_text, Some(b"not the digest"));
    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output).contains("does not match the manifest row"),
        "{}",
        stderr(&output)
    );
    assert!(!prefix.join("daiorama").exists());
    assert!(
        !prefix.exists()
            || std::fs::read_dir(&prefix)
                .unwrap()
                .next()
                .map(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".daiorama.tmp."))
                .unwrap_or(true)
    );
}

#[test]
fn installs_and_prints_the_receipt() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let prefix = dir.path().join("prefix");

    let artifact = dir.path().join("artifact");
    std::fs::write(&artifact, b"binary bytes").unwrap();
    let Some(digest) = sha256sum(&artifact) else {
        eprintln!("skipped: sha256sum is not installed on this bench");
        return;
    };
    let manifest_text = manifest("0.1.0", "x86_64-unknown-linux-gnu", &digest, 1);
    let release = release(dir.path(), "0.1.0", &manifest_text, Some(b"binary bytes"));

    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--client-only",
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = stdout(&output);
    assert!(
        stdout.contains("installed daiorama 0.1.0 (protocol 0.1.0) x86_64-unknown-linux-gnu"),
        "{stdout}"
    );
    assert!(stdout.contains("signed by 504C963C3ABCA66B"), "{stdout}");
    assert!(stdout.contains(&format!("sha256 {digest}")), "{stdout}");
    assert!(
        stdout.contains("client-only install: CLI and bridge"),
        "{stdout}"
    );

    let installed = prefix.join("daiorama");
    assert_eq!(std::fs::read(&installed).unwrap(), b"binary bytes");
    assert_eq!(
        std::fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn the_manager_preflight_is_advisory() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let prefix = dir.path().join("prefix");

    let artifact: &[u8] =
        b"#!/bin/sh\nif [ \"$1\" = doctor ]; then echo doctor-ran-here; exit 1; fi\nexit 0\n";
    let artifact_path = dir.path().join("artifact");
    std::fs::write(&artifact_path, artifact).unwrap();
    let Some(digest) = sha256sum(&artifact_path) else {
        eprintln!("skipped: sha256sum is not installed on this bench");
        return;
    };
    let manifest_text = manifest("0.1.0", "x86_64-unknown-linux-gnu", &digest, 1);
    let release = release(dir.path(), "0.1.0", &manifest_text, Some(artifact));

    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stdout(&output).contains("doctor-ran-here"),
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("doctor reported host gaps above; the binary is installed"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_non_linux_host_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_uname(dir.path(), "Darwin");
    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--prefix",
            dir.path().join("prefix").to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output).contains("install.sh installs Linux binaries; macOS uses the Homebrew tap"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_row_target_that_differs_from_the_requested_target_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let prefix = dir.path().join("prefix");

    // Name says x86_64, target says aarch64: a re-signed manifest trying to hand the wrong
    // architecture to an x86_64 install.
    let row = "{\"name\":\"daiorama-0.1.0-x86_64-unknown-linux-gnu\",\"target\":\"aarch64-unknown-linux-gnu\",\"kind\":\"core\",\"sha256\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"size\":1}";
    let manifest_text = format!(
        "{{\"format\":1,\"version\":\"0.1.0\",\"protocol\":\"0.1.0\",\"channel\":\"stable\",\"released_at\":\"2026-10-18T00:00:00Z\",\"artifacts\":[{row}],\"assets\":[],\"facts\":{{\"client_only\":false,\"manager_platforms\":[\"linux\"],\"components\":[\"core\"]}}}}"
    );
    let release = release(dir.path(), "0.1.0", &manifest_text, None);
    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output)
            .contains("targets \"aarch64-unknown-linux-gnu\", not \"x86_64-unknown-linux-gnu\""),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_release_file_over_one_mib_is_refused_before_verification() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let prefix = dir.path().join("prefix");

    let big = "x".repeat(1048577);
    let release = release(dir.path(), "0.1.0", &big, None);
    let output = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output).contains("larger than 1 MiB"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_invalid_version_or_target_never_reaches_a_path_or_url() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let prefix = dir.path().join("prefix");
    let release = release(
        dir.path(),
        "0.1.0",
        &manifest("0.1.0", "x86_64-unknown-linux-gnu", &"0".repeat(64), 1),
        None,
    );
    let version = run(
        dir.path(),
        &[
            "--version",
            "../../etc",
            "--prefix",
            prefix.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(version.status.code(), Some(1), "{version:?}");
    assert!(
        stderr(&version).contains("invalid version"),
        "{}",
        stderr(&version)
    );
    let target = run(
        dir.path(),
        &[
            "--version",
            "0.1.0",
            "--target",
            "../../evil",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base",
            release.to_str().unwrap(),
        ],
        Some(&bin),
    );
    assert_eq!(target.status.code(), Some(1), "{target:?}");
    assert!(
        stderr(&target).contains("unsupported target"),
        "{}",
        stderr(&target)
    );
}

#[test]
fn an_unset_home_without_prefix_is_a_named_refusal() {
    let dir = tempfile::tempdir().unwrap();
    install_script(dir.path(), TEST_KEY);
    let bin = stub_minisign(dir.path(), 0);
    let script = dir.path().join("install.sh");
    let repo = dir.path().join("../release");
    let _ = repo;
    let output = Command::new("sh")
        .arg(&script)
        .args(["--version", "0.1.0", "--base", "/nonexistent"])
        .env_remove("HOME")
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output).contains("HOME is not set; pass --prefix"),
        "{}",
        stderr(&output)
    );
}
