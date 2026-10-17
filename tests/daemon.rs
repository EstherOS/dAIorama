//! `daiorama daemon start|end` against the real binary, in an isolated environment.
//!
//! Portable by construction: the manager runs with `limits.mode = off` (no delegation
//! required) and no test spawns a compositor — the drain test's refused create stops before
//! any spawn. What it proves is the detached lifecycle end to end: start → status answers →
//! end → the cold answer again.

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

/// `config get/set` against the real manager over its socket: the RPC writes the instance's
/// own file (the file the local path writes), both read routes agree, and `config reload`
/// adopts the change (08 A2 authority + reload semantics).
#[test]
fn config_getset_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let conf = tmp.path().join("conf");
    let data = tmp.path().join("data");
    let run = tmp.path().join("run");
    let instance_config = conf.join("daiorama").join("cg");
    std::fs::create_dir_all(&instance_config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        instance_config.join("config.ron"),
        "(version: 1, limits: (mode: off))\n",
    )
    .unwrap();

    let out = run_binary(&conf, &data, &run, &["daemon", "start", "--instance", "cg"]);
    assert!(
        out.status.success(),
        "daemon start: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let socket = run
        .join("daiorama")
        .join("cg")
        .join("manager.sock")
        .display()
        .to_string();

    // Write through the manager (RPC), not the local file path.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "--instance",
            "cg",
            "--remote",
            socket.as_str(),
            "config",
            "set",
            "sessions.max_global",
            "25",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "config set: {out:?}");
    assert!(
        stdout.contains(
            "set sessions.max_global = 25; a running manager keeps the old value until \
             'daiorama config reload' (or restart)"
        ),
        "{stdout}"
    );

    // A restart-only key states restart, not reload — both call paths (04/08). The remote
    // set answers over RPC; the local set probes the socket and takes the same branch.
    let restart_line = "set manager.bind = 127.0.0.1:9; restart-only: a running manager keeps \
                        the old value until it restarts ('daiorama config reload' will not apply it)";
    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "--instance",
            "cg",
            "--remote",
            socket.as_str(),
            "config",
            "set",
            "manager.bind",
            "127.0.0.1:9",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "remote config set bind: {out:?}");
    assert!(stdout.contains(restart_line), "{stdout}");

    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "--instance",
            "cg",
            "config",
            "set",
            "manager.bind",
            "127.0.0.1:9",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "local config set bind: {out:?}");
    assert!(stdout.contains(restart_line), "{stdout}");

    // Read it back over the socket, and via the cold local read — one file, one answer.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "--instance",
            "cg",
            "--remote",
            socket.as_str(),
            "config",
            "get",
            "sessions.max_global",
        ],
    );
    assert!(out.status.success(), "config get: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("25"));
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["config", "get", "sessions.max_global", "--instance", "cg"],
    );
    assert!(out.status.success(), "local get: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("25"));

    // The reload adopts what the set wrote.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "--instance",
            "cg",
            "--remote",
            socket.as_str(),
            "config",
            "reload",
            "--json",
        ],
    );
    assert!(out.status.success(), "config reload: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("\"reloaded\":[\"sessions.max_global\"]"),
        "{stdout}"
    );

    let out = run_binary(&conf, &data, &run, &["daemon", "end", "--instance", "cg"]);
    assert!(out.status.success(), "cleanup end: {out:?}");
}

/// `drain` against the real manager over its socket: the command enters the state, status
/// carries it, new sessions are refused with the pinned 04 string, and `--wait` completes on
/// an empty instance. Nothing spawns (the refusal precedes the spawner), so no compositor is
/// involved.
#[test]
fn drain_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let conf = tmp.path().join("conf");
    let data = tmp.path().join("data");
    let run = tmp.path().join("run");
    let instance_config = conf.join("daiorama").join("dr");
    std::fs::create_dir_all(&instance_config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        instance_config.join("config.ron"),
        "(version: 1, limits: (mode: off))\n",
    )
    .unwrap();

    let out = run_binary(&conf, &data, &run, &["daemon", "start", "--instance", "dr"]);
    assert!(
        out.status.success(),
        "daemon start: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let socket = run
        .join("daiorama")
        .join("dr")
        .join("manager.sock")
        .display()
        .to_string();

    // Before: status says not draining.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["--instance", "dr", "--remote", socket.as_str(), "status"],
    );
    assert!(out.status.success(), "status before: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("drain: inactive"));

    // Enter drain; nothing is running, so it is complete on the spot.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["--instance", "dr", "--remote", socket.as_str(), "drain"],
    );
    assert!(out.status.success(), "drain: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("drain complete"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Status carries the live state...
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["--instance", "dr", "--remote", socket.as_str(), "status"],
    );
    assert!(out.status.success(), "status: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("drain: active, 0 still running"),
        "{stdout}"
    );

    // ...new sessions are refused with the pinned string before any spawn...
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["--instance", "dr", "--remote", socket.as_str(), "new"],
    );
    assert_eq!(out.status.code(), Some(1), "new while draining: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(
            "manager is draining for upgrade; new sessions are refused (0 still running). \
             (draining)"
        ),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // ...`--wait` on zero running completes, and the JSON form is the pinned shape.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "--instance",
            "dr",
            "--remote",
            socket.as_str(),
            "drain",
            "--wait",
            "--timeout",
            "5",
        ],
    );
    assert!(out.status.success(), "drain --wait: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("drain complete"));

    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "--instance",
            "dr",
            "--remote",
            socket.as_str(),
            "drain",
            "--json",
        ],
    );
    assert!(out.status.success(), "drain --json: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "{\"active\":true,\"remaining\":0}"
    );

    let out = run_binary(&conf, &data, &run, &["daemon", "end", "--instance", "dr"]);
    assert!(out.status.success(), "cleanup end: {out:?}");
}

// ---------------------------------------------------------------------------
// Token lifecycle (07 A5, F12): local-only commands, the rotation window's file
// semantics, and the one printed path+hint (never the value)
// ---------------------------------------------------------------------------

fn token_root(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let conf = tmp.join("conf");
    let data = tmp.join("data");
    let run = tmp.join("run");
    let instance_config = conf.join("daiorama").join("dt");
    std::fs::create_dir_all(&instance_config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        instance_config.join("config.ron"),
        "(version: 1, limits: (mode: off))\n",
    )
    .unwrap();
    (conf, data, run)
}

#[test]
fn token_commands_refuse_remote() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (conf, data, run) = token_root(tmp.path());
    for args in [
        [
            "token",
            "show",
            "--remote",
            "127.0.0.1:8756",
            "--instance",
            "dt",
        ],
        [
            "token",
            "rotate",
            "--remote",
            "127.0.0.1:8756",
            "--instance",
            "dt",
        ],
    ] {
        let out = run_binary(&conf, &data, &run, &args);
        assert!(!out.status.success(), "{args:?} must refuse: {out:?}");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains("local-instance only"),
            "{args:?} refusal names the rule: {text}"
        );
    }
}

#[test]
fn token_rotate_writes_0600_and_show_prints_it_locally() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (conf, data, run) = token_root(tmp.path());
    let out = run_binary(&conf, &data, &run, &["token", "rotate", "--instance", "dt"]);
    assert!(out.status.success(), "rotate: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let printed = stdout
        .lines()
        .next()
        .expect("a value line")
        .trim()
        .to_string();
    assert_eq!(printed.len(), 32, "a 32-hex id: {printed}");
    assert!(
        printed
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "lowercase hex: {printed}"
    );
    let token_file = data.join("daiorama").join("dt").join("token");
    let on_disk = std::fs::read_to_string(&token_file).unwrap();
    assert_eq!(
        on_disk.trim(),
        printed,
        "the file holds what rotate printed"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&token_file).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let show = run_binary(&conf, &data, &run, &["token", "show", "--instance", "dt"]);
    assert!(show.status.success(), "show: {show:?}");
    let shown = String::from_utf8_lossy(&show.stdout).to_string();
    assert!(
        shown.contains(&printed),
        "show prints the value locally: {shown}"
    );
    assert!(shown.contains("path:"), "{shown}");

    let show_json = run_binary(
        &conf,
        &data,
        &run,
        &["token", "show", "--json", "--instance", "dt"],
    );
    let parsed: serde_json::Value = serde_json::from_slice(&show_json.stdout).expect("json shape");
    assert_eq!(parsed["token"].as_str(), Some(printed.as_str()));
    assert!(parsed["path"].as_str().unwrap().ends_with("/token"));
}

/// F12: the one surface that prints at generation time prints the path and the hint, never the
/// value; a network listen mints the token when none exists (07 A5).
#[test]
fn network_listen_prints_the_path_and_hint_never_the_value() {
    use std::io::{BufRead, BufReader};
    let tmp = tempfile::tempdir().expect("tempdir");
    let (conf, data, run) = token_root(tmp.path());
    let mut command = Command::new(binary());
    env_on(&mut command, &conf, &data, &run);
    command
        .args(["mcp", "--listen", "127.0.0.1:0", "--instance", "dt"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("bridge spawns");
    let stderr = child.stderr.take().unwrap();
    let mut lines = BufReader::new(stderr).lines();
    let mut seen = Vec::new();
    for _ in 0..50 {
        let Some(Ok(line)) = lines.next() else {
            break;
        };
        let listening = line.starts_with("listening on ");
        seen.push(line);
        if listening {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let text = seen.join("\n");
    assert!(
        text.contains("(mode 0600)"),
        "mint names the path and mode: {text}"
    );
    assert!(
        text.contains("'daiorama token show' prints the value"),
        "mint prints the hint: {text}"
    );
    let token = std::fs::read_to_string(data.join("daiorama").join("dt").join("token"))
        .expect("minted token file");
    let token = token.trim();
    assert_eq!(token.len(), 32);
    assert!(
        !text.contains(token),
        "the value never reaches stderr: {text}"
    );
}

/// `YYYY-MM-DDTHH:MM:SSZ` for `now - secs`, by the same civil algorithm the marker parser
/// inverts (the test binary cannot reach the crate's private helper).
fn stamp_now_minus(secs: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let total = now - secs;
    let days = total.div_euclid(86_400);
    let rem = total.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[test]
fn startup_retention_sweeps_expired_sessions_and_adopts_unmarked_ones() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let conf = tmp.path().join("conf");
    let data = tmp.path().join("data");
    let run = tmp.path().join("run");
    let instance_config = conf.join("daiorama").join("dt");
    std::fs::create_dir_all(&instance_config).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        instance_config.join("config.ron"),
        "(version: 1, limits: (mode: off))\n",
    )
    .unwrap();

    let root = data.join("daiorama").join("dt");
    let history = root.join("history");
    let media = root.join("media");
    // Expired: a marker well past the seven-day window, with persisted media beside it.
    std::fs::create_dir_all(history.join("old-session")).unwrap();
    std::fs::write(
        history.join("old-session").join(".ended"),
        "2020-01-01T00:00:00Z\tscope=operator\torigin=shell\tcause=closed\n",
    )
    .unwrap();
    std::fs::create_dir_all(media.join("old-session")).unwrap();
    std::fs::write(
        media
            .join("old-session")
            .join("0123456789abcdef0123456789abcdef"),
        b"png",
    )
    .unwrap();
    // Inside the window: kept.
    std::fs::create_dir_all(history.join("fresh-session")).unwrap();
    std::fs::write(
        history.join("fresh-session").join(".ended"),
        format!("{}\tcause=closed\n", stamp_now_minus(86_400)),
    )
    .unwrap();
    // Unmarked: a manager crash leftover — adopted, not deleted.
    std::fs::create_dir_all(history.join("crash-leftover")).unwrap();
    std::fs::write(history.join("crash-leftover").join("seg-0000.log"), b"{}\n").unwrap();

    let out = run_binary(&conf, &data, &run, &["daemon", "start", "--instance", "dt"]);
    assert!(
        out.status.success(),
        "daemon start: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !history.join("old-session").exists(),
        "expired history removed"
    );
    assert!(!media.join("old-session").exists(), "expired media removed");
    assert!(
        history.join("fresh-session").exists(),
        "inside the window: kept"
    );
    assert!(
        history.join("crash-leftover").exists(),
        "an unmarked directory is adopted, never deleted"
    );
    let marker = std::fs::read_to_string(history.join("crash-leftover").join(".ended")).unwrap();
    assert!(marker.contains("cause=manager_restart"), "{marker}");
    let log = std::fs::read_to_string(root.join("daemon.log")).unwrap();
    assert!(
        log.contains("retention sweep: 1 expired session(s) removed"),
        "{log}"
    );
    assert!(
        log.contains("retention sweep: 1 unmarked session(s) adopted"),
        "{log}"
    );

    let out = run_binary(&conf, &data, &run, &["daemon", "end", "--instance", "dt"]);
    assert!(out.status.success(), "daemon end: {out:?}");
}
