//! Packaging pins (P5): the shipped unit and its runbook are contract, not prose.
//!
//! 09 names the channels and 10 pins the supervision shape, so the files an
//! operator actually installs are read back here — a directive or command that
//! drifts from the docs fails the build (the fixture/doc pin pattern).

const UNIT: &str = include_str!("../packaging/systemd/daiorama@.service");
const README: &str = include_str!("../packaging/README.md");
const STRINGS: &str = include_str!("../src/strings.rs");

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
