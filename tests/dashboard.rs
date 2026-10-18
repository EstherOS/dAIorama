//! The 04 `dashboard` verbs against the real binary: the core CLI locates the module through
//! the activation link (D1) and execs it with the module's own grammar; a missing or skewed
//! module is refused before anything runs.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_daiorama"))
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run(instance: &str, args: &[&str], stdin: Option<&str>) -> Output {
    let tmp = root().join("target").join("dashboard-test");
    let conf = tmp.join("conf");
    let data = tmp.join("data");
    let run_dir = tmp.join("run");
    for dir in [&conf, &data, &run_dir] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut command = Command::new(binary());
    command
        .env("XDG_CONFIG_HOME", &conf)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_RUNTIME_DIR", &run_dir)
        .args(args)
        .args(["--instance", instance]);
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("binary runs");
    if let Some(text) = stdin {
        child
            .stdin
            .as_mut()
            .expect("stdin piped")
            .write_all(text.as_bytes())
            .unwrap();
    }
    child.wait_with_output().expect("binary finishes")
}

fn instance_data(instance: &str) -> PathBuf {
    root()
        .join("target")
        .join("dashboard-test")
        .join("data")
        .join("daiorama")
        .join(instance)
}

fn install(instance: &str, version: &str, body: &str) {
    let dir = instance_data(instance)
        .join("modules")
        .join("daiorama-dashboard")
        .join(version);
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("daiorama-dashboard");
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let parent = instance_data(instance)
        .join("modules")
        .join("daiorama-dashboard");
    let link = parent.join("active");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(version, &link).unwrap();
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn a_missing_module_is_the_pinned_refusal() {
    let out = run("dash-missing", &["dashboard", "serve"], None);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("module not installed; run 'daiorama dashboard install'."),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_skewed_module_is_refused_before_exec() {
    let instance = "dash-skew";
    let marker = root().join("target").join("dash-skew-ran");
    let _ = std::fs::remove_file(&marker);
    install(
        instance,
        "9.9.9",
        &format!("#!/bin/sh\ntouch {}\n", marker.display()),
    );
    let out = run(instance, &["dashboard", "serve"], None);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("does not match this dashboard"),
        "{}",
        stderr(&out)
    );
    assert!(!marker.exists(), "skewed module must not execute");
}

#[test]
fn verbs_forward_to_the_module_with_its_own_grammar() {
    let instance = "dash-forward";
    install(
        instance,
        env!("CARGO_PKG_VERSION"),
        "#!/bin/sh\necho \"FAKE:$@\"\ncat\n",
    );

    let out = run(instance, &["dashboard", "connect", "127.0.0.1:8756"], None);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("FAKE:connect 127.0.0.1:8756"),
        "{}",
        stdout(&out)
    );

    let out = run(instance, &["dashboard", "connections", "list"], None);
    assert!(
        stdout(&out).contains("FAKE:connections"),
        "{}",
        stdout(&out)
    );

    let out = run(
        instance,
        &["dashboard", "token", "set", "127.0.0.1:8756"],
        Some("s3cret\n"),
    );
    assert!(
        stdout(&out).contains("FAKE:token set 127.0.0.1:8756"),
        "{}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("s3cret"),
        "stdin must reach the module: {}",
        stdout(&out)
    );
}

// ---------------------------------------------------------------------------
// `dashboard install` (09; D5): checksum first, stage, rename, one symlink swap
// ---------------------------------------------------------------------------

/// Build a release-asset tarball: `daiorama-dashboard-<version>-<triple>.tar.gz`.
fn write_artifact(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (entry, body) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        builder.append_data(&mut header, entry, *body).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    path
}

fn asset(dir: &Path, version: &str) -> PathBuf {
    write_artifact(
        dir,
        &format!("daiorama-dashboard-{version}-x86_64-unknown-linux-gnu.tar.gz"),
        &[(
            "daiorama-dashboard",
            b"#!/bin/sh\necho MODULE:$@\n".as_slice(),
        )],
    )
}

fn digest(path: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("sha256sum runs");
    assert!(out.status.success(), "sha256sum {path:?}");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("a digest")
        .to_string()
}

fn modules(instance: &str) -> PathBuf {
    instance_data(instance)
        .join("modules")
        .join("daiorama-dashboard")
}

/// Each install test starts from no module tree: `target/dashboard-test` is shared across
/// runs, so a previous run's installs must not decide this one's assertions.
fn fresh(instance: &str) {
    let _ = std::fs::remove_dir_all(instance_data(instance).join("modules"));
}

#[test]
fn install_refuses_a_checksum_mismatch_before_touching_the_module_tree() {
    fresh("dash-sha-bad");
    let tmp = tempfile::tempdir().unwrap();
    let artifact = asset(tmp.path(), "0.1.0");
    let out = run(
        "dash-sha-bad",
        &[
            "dashboard",
            "install",
            "--artifact",
            artifact.to_str().unwrap(),
            "--sha256",
            &"0".repeat(64),
        ],
        None,
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("artifact checksum mismatch: expected"),
        "{}",
        stderr(&out)
    );
    assert!(
        !instance_data("dash-sha-bad").join("modules").exists(),
        "nothing under modules/ may exist before the checksum verifies"
    );
}

#[test]
fn install_unpacks_activates_and_prints_version_and_path() {
    fresh("dash-install-ok");
    let tmp = tempfile::tempdir().unwrap();
    let artifact = asset(tmp.path(), "0.1.0");
    let out = run(
        "dash-install-ok",
        &[
            "dashboard",
            "install",
            "--artifact",
            artifact.to_str().unwrap(),
            "--sha256",
            &digest(&artifact),
        ],
        None,
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let dir = modules("dash-install-ok").join("0.1.0");
    let printed = stdout(&out);
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(
        lines,
        vec!["0.1.0", dir.to_str().unwrap()],
        "version + path only"
    );

    let binary = dir.join("daiorama-dashboard");
    let mode = std::fs::metadata(&binary).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "the module binary must be executable");
    assert_eq!(
        std::fs::read_link(modules("dash-install-ok").join("active")).unwrap(),
        PathBuf::from("0.1.0"),
        "activation is one slug component"
    );

    // The installed module is what `dashboard serve` execs.
    let served = run("dash-install-ok", &["dashboard", "serve"], None);
    assert!(served.status.success(), "{}", stderr(&served));
    assert!(
        stdout(&served).contains("MODULE:serve"),
        "{}",
        stdout(&served)
    );
}

#[test]
fn install_reads_the_version_from_the_asset_name_when_omitted() {
    fresh("dash-install-name");
    let tmp = tempfile::tempdir().unwrap();
    let artifact = asset(tmp.path(), "2026.10.0");
    let out = run(
        "dash-install-name",
        &[
            "dashboard",
            "install",
            "--artifact",
            artifact.to_str().unwrap(),
            "--sha256",
            &digest(&artifact),
        ],
        None,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(modules("dash-install-name").join("2026.10.0").is_dir());
}

#[test]
fn install_refuses_a_symlinked_version_directory() {
    fresh("dash-install-symlink");
    let tmp = tempfile::tempdir().unwrap();
    let artifact = asset(tmp.path(), "0.1.0");
    let parent = modules("dash-install-symlink");
    std::fs::create_dir_all(&parent).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, parent.join("0.1.0")).unwrap();

    let out = run(
        "dash-install-symlink",
        &[
            "dashboard",
            "install",
            "--artifact",
            artifact.to_str().unwrap(),
            "--sha256",
            &digest(&artifact),
            "--force",
        ],
        None,
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("refusing to replace"),
        "{}",
        stderr(&out)
    );
    assert!(outside.is_dir(), "the symlink target is never touched");
    assert!(
        !parent.join("active").exists(),
        "a refused install activates nothing"
    );
}

#[test]
fn force_replaces_a_real_version_directory_and_keeps_the_previous_one() {
    fresh("dash-install-force");
    let tmp = tempfile::tempdir().unwrap();
    let first = asset(tmp.path(), "0.1.0");
    let digest_first = digest(&first);
    let parent = modules("dash-install-force");
    let dir = parent.join("0.1.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stale"), b"old build").unwrap();

    let refused = run(
        "dash-install-force",
        &[
            "dashboard",
            "install",
            "--artifact",
            first.to_str().unwrap(),
            "--sha256",
            &digest_first,
        ],
        None,
    );
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("is already installed at"),
        "{}",
        stderr(&refused)
    );
    assert!(
        dir.join("stale").exists(),
        "a refused replace leaves the tree alone"
    );

    let replaced = run(
        "dash-install-force",
        &[
            "dashboard",
            "install",
            "--artifact",
            first.to_str().unwrap(),
            "--sha256",
            &digest_first,
            "--force",
        ],
        None,
    );
    assert!(replaced.status.success(), "{}", stderr(&replaced));
    assert!(
        !dir.join("stale").exists(),
        "--force replaces the directory"
    );
    assert!(dir.join("daiorama-dashboard").is_file());

    // A second version installs beside the first; `active` moves, the old version stays.
    let second = asset(tmp.path(), "0.2.0");
    let upgraded = run(
        "dash-install-force",
        &[
            "dashboard",
            "install",
            "--artifact",
            second.to_str().unwrap(),
            "--sha256",
            &digest(&second),
        ],
        None,
    );
    assert!(upgraded.status.success(), "{}", stderr(&upgraded));
    assert!(
        parent.join("0.1.0").is_dir(),
        "the previous version is kept"
    );
    assert_eq!(
        std::fs::read_link(parent.join("active")).unwrap(),
        PathBuf::from("0.2.0")
    );

    // `--version <old>` reverts by re-activating, no artifact involved.
    let reverted = run(
        "dash-install-force",
        &["dashboard", "install", "--version", "0.1.0"],
        None,
    );
    assert!(reverted.status.success(), "{}", stderr(&reverted));
    assert_eq!(
        std::fs::read_link(parent.join("active")).unwrap(),
        PathBuf::from("0.1.0")
    );
}

#[test]
fn install_validates_the_version_slug_and_the_artifact_contents() {
    fresh("dash-install-slug");
    fresh("dash-install-nobin");
    let tmp = tempfile::tempdir().unwrap();
    let artifact = asset(tmp.path(), "0.1.0");
    let out = run(
        "dash-install-slug",
        &[
            "dashboard",
            "install",
            "--artifact",
            artifact.to_str().unwrap(),
            "--sha256",
            &digest(&artifact),
            "--version",
            "../escape",
        ],
        None,
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("invalid module version"),
        "{}",
        stderr(&out)
    );
    assert!(!modules("dash-install-slug").exists());

    let empty = write_artifact(
        tmp.path(),
        "daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz",
        &[("ui/index.html", b"<html></html>".as_slice())],
    );
    let missing = run(
        "dash-install-nobin",
        &[
            "dashboard",
            "install",
            "--artifact",
            empty.to_str().unwrap(),
            "--sha256",
            &digest(&empty),
        ],
        None,
    );
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("artifact does not contain 'daiorama-dashboard'"),
        "{}",
        stderr(&missing)
    );
    assert!(
        !modules("dash-install-nobin").join("0.1.0").exists(),
        "a refused artifact leaves no version dir"
    );
}

#[allow(dead_code)]
fn _path(_: &Path) {}
