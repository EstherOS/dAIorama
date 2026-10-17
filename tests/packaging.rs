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
