//! `daiorama config scope create` against the real binary: the three templates, the 0600
//! file, the pinned refusals, and the no-secret rule (04, 08; `p5/dashboard` D2). Cold
//! commands — nothing here starts or contacts a manager.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_daiorama"))
}

/// A fresh instance root and one invocation of the binary against it.
fn run(tmp: &Path, args: &[&str]) -> Output {
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
        .env_remove("DAIORAMA_SCOPE")
        .env_remove("DAIORAMA_TOKEN")
        .env_remove("DAIORAMA_MODE");
    command.args(args).output().expect("binary runs")
}

fn scope_path(tmp: &Path, name: &str) -> PathBuf {
    tmp.join("conf")
        .join("daiorama")
        .join("default")
        .join("scopes")
        .join(format!("{name}.ron"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn create_writes_the_own_template_and_prints_its_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(tmp.path(), &["config", "scope", "create", "agent1"]);
    assert!(out.status.success(), "create exits 0: {out:?}");
    let path = scope_path(tmp.path(), "agent1");
    assert_eq!(
        stdout(&out),
        format!("{}\n", path.display()),
        "the path is the whole answer"
    );

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "scope file is 0600 from the first byte"
    );

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("//"), "template is commented");
    assert!(text.contains("visibility: own"), "{text}");

    // The file the command wrote is a load-valid scope, per the existing read surface.
    let listed = run(tmp.path(), &["config", "scope", "list"]);
    assert_eq!(stdout(&listed), "agent1\n", "{listed:?}");
    let valid = run(tmp.path(), &["config", "scope", "validate", "agent1"]);
    assert!(valid.status.success(), "{valid:?}");
    let got = run(tmp.path(), &["config", "scope", "get", "agent1"]);
    assert!(stdout(&got).contains("sessions.visibility: own"), "{got:?}");
}

#[test]
fn dashboard_template_is_all_visibility_reads_with_audit_off() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(
        tmp.path(),
        &[
            "config",
            "scope",
            "create",
            "dashboard",
            "--template",
            "dashboard",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let got = run(tmp.path(), &["config", "scope", "get", "dashboard"]);
    let text = stdout(&got);
    assert!(text.contains("sessions.visibility: all"), "{text}");
    assert!(
        text.contains("audit: store_clips off, store_screenshots off"),
        "{text}"
    );
    assert!(
        text.contains("limits.rate: 20 per_sec, burst 40, mode pace"),
        "{text}"
    );
    assert!(text.contains("grant: categories [], tools []"), "{text}");
}

#[test]
fn all_template_sets_all_visibility() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(
        tmp.path(),
        &[
            "config",
            "scope",
            "create",
            "supervisor",
            "--template",
            "all",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let got = run(tmp.path(), &["config", "scope", "get", "supervisor"]);
    assert!(stdout(&got).contains("sessions.visibility: all"), "{got:?}");
}

#[test]
fn existing_scope_is_refused_and_left_untouched() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(
        run(tmp.path(), &["config", "scope", "create", "agent1"])
            .status
            .success()
    );
    let before = std::fs::read_to_string(scope_path(tmp.path(), "agent1")).unwrap();

    let out = run(tmp.path(), &["config", "scope", "create", "agent1"]);
    assert!(!out.status.success(), "second create refuses: {out:?}");
    assert!(
        stderr(&out).contains("scope \"agent1\" already exists (bad_request)"),
        "{}",
        stderr(&out)
    );
    let after = std::fs::read_to_string(scope_path(tmp.path(), "agent1")).unwrap();
    assert_eq!(before, after, "a refused create never overwrites");
}

#[test]
fn invalid_name_is_refused_before_any_write() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(tmp.path(), &["config", "scope", "create", "Bad Name"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        stderr(&out).contains("invalid scope name (bad_request)"),
        "{}",
        stderr(&out)
    );
    assert!(
        !tmp.path()
            .join("conf")
            .join("daiorama")
            .join("default")
            .join("scopes")
            .join("Bad Name.ron")
            .exists(),
        "nothing was written for an invalid name"
    );
}

#[test]
fn invalid_template_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(
        tmp.path(),
        &[
            "config",
            "scope",
            "create",
            "agent1",
            "--template",
            "dashbord",
        ],
    );
    assert!(!out.status.success(), "{out:?}");
    assert!(
        stderr(&out).contains(
            "invalid scope template \"dashbord\"; expected own, all, or dashboard (bad_request)"
        ),
        "{}",
        stderr(&out)
    );
    assert!(!scope_path(tmp.path(), "agent1").exists());
}

#[test]
fn create_writes_no_scope_secret() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(
        run(
            tmp.path(),
            &[
                "config",
                "scope",
                "create",
                "dashboard",
                "--template",
                "dashboard"
            ]
        )
        .status
        .success()
    );
    let secret = tmp
        .path()
        .join("data")
        .join("daiorama")
        .join("default")
        .join("scope-tokens")
        .join("dashboard");
    assert!(
        !secret.exists(),
        "create mints no secret; the next network start does (08, D2)"
    );
}
