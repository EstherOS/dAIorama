//! `daiorama daemon start|end` against the real binary, in an isolated environment.
//!
//! Portable by construction: the manager runs with `limits.mode = off` (no delegation
//! required) and the test never creates a session, so no compositor is involved. What it
//! proves is the detached lifecycle end to end: start → status answers → end → the cold
//! answer again.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_daiorama"))
}

fn env_on(command: &mut Command, conf: &Path, data: &Path, run: &Path) {
    command
        .env("XDG_CONFIG_HOME", conf)
        .env("XDG_DATA_HOME", data)
        .env("XDG_RUNTIME_DIR", run)
        // Never inherit a stray mode from the calling environment.
        .env_remove("DAIORAMA_MODE")
        .env_remove("DAIORAMA_TOKEN")
        .env_remove("DAIORAMA_SCOPE")
        .env_remove("DAIORAMA_SCOPE_TOKEN");
}

fn run_binary(conf: &Path, data: &Path, run: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(binary());
    env_on(&mut command, conf, data, run);
    command.args(args).output().expect("binary runs")
}

#[test]
fn daemon_start_then_end_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let conf = tmp.path().join("conf");
    let data = tmp.path().join("data");
    let run = tmp.path().join("run");
    let instance_config = conf.join("daiorama").join("dt");
    std::fs::create_dir_all(&instance_config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        instance_config.join("config.ron"),
        "(version: 1, limits: (mode: off))\n",
    )
    .unwrap();

    // Cold: end with no manager is the honest answer, exit 0.
    let out = run_binary(&conf, &data, &run, &["daemon", "end", "--instance", "dt"]);
    assert!(out.status.success(), "cold end: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no manager running"),
        "cold answer on stdout"
    );

    // Start detached.
    let out = run_binary(&conf, &data, &run, &["daemon", "start", "--instance", "dt"]);
    assert!(
        out.status.success(),
        "daemon start: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let started = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        started.contains("started manager (mode=background"),
        "{started}"
    );

    // It answers.
    let out = run_binary(&conf, &data, &run, &["status", "--instance", "dt"]);
    assert!(out.status.success(), "status after start: {out:?}");
    let status = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(status.contains("state: running"), "{status}");
    assert!(status.contains("sessions: 0 running"), "{status}");

    // End: no live sessions, so it stops without --force.
    let out = run_binary(&conf, &data, &run, &["daemon", "end", "--instance", "dt"]);
    assert!(
        out.status.success(),
        "daemon end: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("stopped manager"));

    // And the socket is gone: the cold answer is back.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let out = run_binary(&conf, &data, &run, &["status", "--instance", "dt"]);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        if stdout.contains("no manager running") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "manager still answering: {stdout}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Two starters racing: both must report the *serving* manager's pid (never the loser's
/// child), and both must succeed once a manager is up.
#[test]
fn two_racers_report_the_winner() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let conf = tmp.path().join("conf");
    let data = tmp.path().join("data");
    let run = tmp.path().join("run");
    let instance_config = conf.join("daiorama").join("race");
    std::fs::create_dir_all(&instance_config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        instance_config.join("config.ron"),
        "(version: 1, limits: (mode: off))\n",
    )
    .unwrap();

    let spawn_start = || {
        let mut command = Command::new(binary());
        env_on(&mut command, &conf, &data, &run);
        command
            .args(["daemon", "start", "--instance", "race"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn daemon start")
    };
    let first = spawn_start();
    let second = spawn_start();
    let out1 = first.wait_with_output().unwrap();
    let out2 = second.wait_with_output().unwrap();
    assert!(out1.status.success(), "racer 1: {out1:?}");
    assert!(out2.status.success(), "racer 2: {out2:?}");

    let pid_of = |out: &std::process::Output| -> u32 {
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        text.split("pid=")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .unwrap_or_else(|| panic!("no pid in: {text}"))
    };
    let pid1 = pid_of(&out1);
    let pid2 = pid_of(&out2);
    assert_eq!(pid1, pid2, "both racers report the same serving pid");

    let out = run_binary(&conf, &data, &run, &["status", "--instance", "race"]);
    let status = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        status.contains(&format!("pid: {pid1}")),
        "status agrees the reported pid serves: {status}"
    );

    let out = run_binary(&conf, &data, &run, &["daemon", "end", "--instance", "race"]);
    assert!(out.status.success(), "cleanup end: {out:?}");
}
