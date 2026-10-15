//! Conformance runner: every scenario in `fixtures/ipc/` is a CLI-level test.
//!
//! One `#[tokio::test]` per scenario file (explicit, parallel, individually runnable), plus a
//! guard test that fails when a scenario file on disk is not wired into this list — a fixture
//! that silently never runs is worse than no fixture.
//!
//! Scenario schema and the harness rules: `fixtures/README.md`.

mod common;

use common::{run_scenario_file, scenario_names_on_disk};

/// Every conformance scenario in the suite, grouped by area.
const SCENARIOS: &[&str] = &[
    // Core exchanges and the handshake.
    "handshake-ok",
    "handshake-version-mismatch",
    "new-session-ok",
    // Creation refusals.
    "new-refused-software-renderer",
    "new-host-busy",
    "new-quota-exceeded",
    // Reads.
    "list-paged",
    "status-instance-json",
    "status-session-json",
    // Control surface.
    "control-unknown-action",
    // The registry and its tombstone transitions (06).
    "close-closed",
    "close-unknown",
    "session-reaped",
    "tombstone-expired",
    "state-transitions",
    "dead-sessions-separate",
    "session-dead-call",
    "view-down-session-running",
    "compositor-death-no-restart",
    // Cold/hot and the start classes.
    "cold-no-manager",
    "remote-unreachable",
    "manager-start-limits-unavailable",
    "manager-start-limits-off-mode",
    // The remaining frame-implemented verbs.
    "frame-verbs",
];

#[tokio::test]
async fn handshake_ok() {
    run_scenario_file("handshake-ok").await;
}

#[tokio::test]
async fn handshake_version_mismatch() {
    run_scenario_file("handshake-version-mismatch").await;
}

#[tokio::test]
async fn new_session_ok() {
    run_scenario_file("new-session-ok").await;
}

#[tokio::test]
async fn new_refused_software_renderer() {
    run_scenario_file("new-refused-software-renderer").await;
}

#[tokio::test]
async fn new_host_busy() {
    run_scenario_file("new-host-busy").await;
}

#[tokio::test]
async fn new_quota_exceeded() {
    run_scenario_file("new-quota-exceeded").await;
}

#[tokio::test]
async fn list_paged() {
    run_scenario_file("list-paged").await;
}

#[tokio::test]
async fn status_instance_json() {
    run_scenario_file("status-instance-json").await;
}

#[tokio::test]
async fn status_session_json() {
    run_scenario_file("status-session-json").await;
}

#[tokio::test]
async fn control_unknown_action() {
    run_scenario_file("control-unknown-action").await;
}

#[tokio::test]
async fn close_closed() {
    run_scenario_file("close-closed").await;
}

#[tokio::test]
async fn close_unknown() {
    run_scenario_file("close-unknown").await;
}

#[tokio::test]
async fn session_reaped() {
    run_scenario_file("session-reaped").await;
}

#[tokio::test]
async fn tombstone_expired() {
    run_scenario_file("tombstone-expired").await;
}

#[tokio::test]
async fn state_transitions() {
    run_scenario_file("state-transitions").await;
}

#[tokio::test]
async fn dead_sessions_separate() {
    run_scenario_file("dead-sessions-separate").await;
}

#[tokio::test]
async fn session_dead_call() {
    run_scenario_file("session-dead-call").await;
}

#[tokio::test]
async fn view_down_session_running() {
    run_scenario_file("view-down-session-running").await;
}

#[tokio::test]
async fn compositor_death_no_restart() {
    run_scenario_file("compositor-death-no-restart").await;
}

#[tokio::test]
async fn cold_no_manager() {
    run_scenario_file("cold-no-manager").await;
}

#[tokio::test]
async fn remote_unreachable() {
    run_scenario_file("remote-unreachable").await;
}

#[tokio::test]
async fn manager_start_limits_unavailable() {
    run_scenario_file("manager-start-limits-unavailable").await;
}

#[tokio::test]
async fn manager_start_limits_off_mode() {
    run_scenario_file("manager-start-limits-off-mode").await;
}

#[tokio::test]
async fn frame_verbs() {
    run_scenario_file("frame-verbs").await;
}

/// A fixture that is not wired into the suite is a fixture nobody runs.
#[test]
fn every_scenario_file_is_wired_into_the_suite() {
    let on_disk = scenario_names_on_disk();
    let mut listed: Vec<String> = SCENARIOS.iter().map(|name| name.to_string()).collect();
    listed.sort();
    assert_eq!(
        on_disk, listed,
        "fixtures/ipc/ and the SCENARIOS list drifted apart \
         (add the scenario file and its #[tokio::test] together)"
    );
    assert!(
        listed.len() >= 24,
        "the suite covers the full contract surface: handshake, creation, reads, control, \
         registry transitions, cold/hot classes and the frame verbs"
    );
}
