//! Packaging pins (P5): the shipped unit and its runbook are contract, not prose.
//!
//! 09 names the channels and 10 pins the supervision shape, so the files an
//! operator actually installs are read back here — a directive or command that
//! drifts from the docs fails the build (the fixture/doc pin pattern).

const UNIT: &str = include_str!("../packaging/systemd/daiorama@.service");
const README: &str = include_str!("../packaging/README.md");
const STRINGS: &str = include_str!("../src/strings.rs");
const NOVNC_FETCH: &str = include_str!("../packaging/novnc/fetch.sh");
const NOVNC_NOTE: &str = include_str!("../p5/novnc/README.md");
const SPEC_06: &str = include_str!("../docs/06-sessions-views.md");
const SPEC_09: &str = include_str!("../docs/09-packaging.md");
const SPEC_10: &str = include_str!("../docs/10-operations.md");
const IMAGE_NOTE: &str = include_str!("../p5/image/README.md");
const ENTRYPOINT: &str = include_str!("../packaging/container/entrypoint.sh");

#[test]
fn unit_is_a_user_service_that_runs_serve_for_the_instance() {
    // 10: units always use `serve` explicitly, never the auto-estimate.
    assert!(
        UNIT.contains("ExecStart=%h/.local/bin/daiorama serve --instance %i"),
        "{UNIT}"
    );
    assert!(UNIT.contains("WantedBy=default.target"), "{UNIT}");
    assert!(!UNIT.contains("daemon start"), "{UNIT}");
}

#[test]
fn unit_sweeps_the_tree_and_delegates_the_cgroup() {
    // 10: a unit stop sweeps the whole tree; 06: limits need a delegated subtree.
    assert!(UNIT.contains("KillMode=control-group"), "{UNIT}");
    assert!(UNIT.contains("Delegate=yes"), "{UNIT}");
}

#[test]
fn the_runbook_mentions_linger_for_headless_hosts() {
    // 10: a user unit needs the user manager alive; on a headless box that is only
    // true after `loginctl enable-linger`, so the runbook must not omit it.
    assert!(
        README.contains("loginctl enable-linger"),
        "runbook omits linger"
    );
}

#[test]
fn unit_holds_no_secret_and_creates_no_directories() {
    // 08: the in-binary ensure is the only path that creates the data/runtime dirs.
    assert!(!UNIT.contains("ExecStartPre"), "{UNIT}");
    // 07/10: the token lives in its 0600 file, never in the unit or `systemctl show`.
    assert!(!UNIT.contains("DAIORAMA_TOKEN"), "{UNIT}");
}

#[test]
fn runbook_carries_the_operator_path() {
    for needle in [
        "cargo build --release",
        "install -Dm644 packaging/systemd/daiorama@.service ~/.config/systemd/user/daiorama@.service",
        "systemctl --user enable --now daiorama@default",
        "daiorama doctor",
        "daiorama config setup",
        "daiorama drain --wait",
        "daiorama daemon end --force",
        "systemctl --user restart daiorama@default",
    ] {
        assert!(README.contains(needle), "runbook lost: {needle}");
    }
}

#[test]
fn runbook_quotes_the_pinned_view_assets_refusal() {
    // The operator's 503 exit path must quote the string the manager prints.
    let pinned = "view assets not installed (expected <data>/novnc)";
    assert!(
        STRINGS.contains(pinned),
        "strings.rs no longer pins the view-assets refusal"
    );
    assert!(
        README.contains(pinned),
        "runbook must quote the pinned refusal"
    );
}

#[test]
fn novnc_installer_and_sourcing_note_carry_one_pin() {
    // Two records, one artifact: the sourcing note and the shipped installer must agree.
    let sha = "b1003a11b6e6e8d8f7f5e5586daae7f8ca651d8aee0aa155ff9ac841c48f52c6";
    assert!(NOVNC_FETCH.contains(sha), "installer pin drifted");
    assert!(
        NOVNC_NOTE.contains(sha),
        "note pin drifted from the installer"
    );
    assert!(NOVNC_FETCH.contains("refs/tags/v1.7.0"), "{NOVNC_FETCH}");
    // 09 assigns the install to packaging; doctor prints the same path (strings.rs).
    assert!(README.contains("packaging/novnc/fetch.sh"), "{README}");
    assert!(STRINGS.contains("packaging/novnc/fetch.sh"), "{STRINGS}");
    // Staging precedes the first run (10), so the target's parent may not exist yet: the
    // installer creates it rather than relying on a data root the binary has not made.
    assert!(
        NOVNC_FETCH.contains("mkdir -p \"$(dirname \"$DEST\")\""),
        "{NOVNC_FETCH}"
    );
}

#[test]
fn novnc_installer_refuses_unsafe_destinations_before_downloading() {
    // These refusals run before the pinned URL is touched, so the test needs no network.
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/novnc/fetch.sh");
    for dest in ["/", ".", "..", ""] {
        let out = std::process::Command::new("sh")
            .arg(&script)
            .arg(dest)
            .output()
            .expect("script runs");
        assert!(
            !out.status.success(),
            "dest {dest:?} must be refused: {out:?}"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("refusing"), "dest {dest:?}: {err}");
    }

    // An existing non-tree directory is refused with its contents intact.
    let tmp = tempfile::tempdir().expect("tempdir");
    let keep = tmp.path().join("keep");
    std::fs::create_dir_all(&keep).unwrap();
    std::fs::write(keep.join("keep.txt"), "x").unwrap();
    let out = std::process::Command::new("sh")
        .arg(&script)
        .arg(&keep)
        .output()
        .expect("script runs");
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a noVNC tree"));
    assert_eq!(std::fs::read_to_string(keep.join("keep.txt")).unwrap(), "x");
}

#[test]
fn the_installer_names_and_checks_its_prerequisites() {
    // N2: a missing tool fails up front with a named prerequisite, not a bare shell error.
    assert!(
        NOVNC_FETCH.contains("for tool in tar sha256sum"),
        "the fixed tools are named"
    );
    assert!(
        NOVNC_FETCH.contains("command -v \"$tool\""),
        "each named tool is checked"
    );
    assert!(
        NOVNC_FETCH.contains("refusing: curl is required to download"),
        "download path names curl"
    );
    assert!(
        README.contains("Installer prerequisites"),
        "runbook states the prerequisites"
    );
}

#[test]
fn image_split_is_pinned_across_the_docs_and_the_runbook() {
    // 09 pins the two artifacts and scopes the no-default-CMD rule to the core image.
    assert!(
        SPEC_09.contains("| `daiorama-desktop` |"),
        "09 lost the desktop artifact row"
    );
    assert!(
        SPEC_09.contains("documented exception"),
        "09 lost the scoped CMD exception"
    );
    assert!(
        SPEC_09.contains("/usr/share/daiorama/novnc"),
        "09 lost the seed path"
    );
    // 06 names the launcher instead of "a fixed path".
    assert!(
        SPEC_06.contains("/usr/local/bin/daiorama-browser"),
        "06 does not name the launcher path"
    );
    // 10's staging note must describe the seed copy, not a build-time stage into /data.
    assert!(
        SPEC_10.contains("pre-seeds the volume"),
        "10 still describes build-time staging"
    );
    assert!(
        !SPEC_10.contains("runs it at build time into the persisted"),
        "10 kept the impossible build-time stage"
    );
    // One entrypoint, named by the spec, the runbook and the pre-read.
    for doc in [SPEC_09, README, IMAGE_NOTE] {
        assert!(
            doc.contains("packaging/container/entrypoint.sh"),
            "a doc stopped naming the entrypoint"
        );
    }
}

/// Run the shipped entrypoint with a seeded tree and a data home, returning its output.
fn run_entrypoint(
    seed: &std::path::Path,
    data: &std::path::Path,
    argv: &[&str],
) -> std::process::Output {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/container/entrypoint.sh");
    std::process::Command::new("sh")
        .arg(&script)
        .args(argv)
        .env("DAIORAMA_NOVNC_SEED", seed)
        .env("XDG_DATA_HOME", data)
        .env("DAIORAMA_INSTANCE", "test")
        .output()
        .expect("entrypoint runs")
}

#[test]
fn entrypoint_seeds_once_and_never_overwrites_a_staged_tree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let seed = tmp.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::write(seed.join("vnc.html"), "seed").unwrap();
    let data = tmp.path().join("data");
    let dest = data.join("daiorama/test/novnc");

    // Absent entry point: copied, and the command runs (its exit code proves the exec).
    let out = run_entrypoint(&seed, &data, &["sh", "-c", "echo ran; exit 7"]);
    assert_eq!(out.status.code(), Some(7), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ran"),
        "{out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("vnc.html")).unwrap(),
        "seed"
    );

    // Present: untouched — an operator's restage or newer tree is not clobbered on recreate.
    std::fs::write(dest.join("vnc.html"), "operator").unwrap();
    let out = run_entrypoint(&seed, &data, &["true"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(dest.join("vnc.html")).unwrap(),
        "operator"
    );

    // No seed in the image: one line, then exec — a missing view tree must not stop the manager.
    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let out = run_entrypoint(&empty, &data, &["sh", "-c", "exit 3"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no seeded view assets"),
        "{out:?}"
    );
}

#[test]
fn entrypoint_refuses_to_run_without_a_command() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run_entrypoint(tmp.path(), tmp.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("usage:"),
        "{out:?}"
    );
}

#[test]
fn the_entrypoint_creates_only_the_asset_tree() {
    // 08: directory creation stays in the binary; the seed copy is the one exception, and it
    // must not pre-create the runtime dir or a data tree beyond the asset path.
    assert_eq!(
        ENTRYPOINT.matches("mkdir").count(),
        1,
        "exactly one directory is created: the asset tree"
    );
    assert!(
        ENTRYPOINT.contains("mkdir -p \"$DEST\""),
        "the asset tree is the one directory created"
    );
    assert!(
        !ENTRYPOINT.contains("XDG_RUNTIME_DIR"),
        "the entrypoint must not pre-create the runtime dir"
    );
    assert!(
        !ENTRYPOINT.contains("DAIORAMA_TOKEN"),
        "the entrypoint holds no secret"
    );
}
