//! The `manager.bind` TCP surface against the real binary: bind, credential gate, and the
//! rotation window.
//!
//! Portable by construction: `limits.mode = off`, no compositor, no spawn. What it proves is
//! the network contract end to end — the CLI over `--remote host:port` presents
//! `DAIORAMA_TOKEN` and `DAIORAMA_SCOPE_TOKEN`, a missing credential gets the pinned refusal,
//! and a token rotated under a running manager takes effect only at the next start (07).

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_daiorama"))
}

fn env_on(command: &mut Command, conf: &Path, data: &Path, run: &Path) {
    command
        .env("XDG_CONFIG_HOME", conf)
        .env("XDG_DATA_HOME", data)
        .env("XDG_RUNTIME_DIR", run)
        .env_remove("DAIORAMA_MODE")
        .env_remove("DAIORAMA_TOKEN")
        .env_remove("DAIORAMA_SCOPE")
        .env_remove("DAIORAMA_SCOPE_TOKEN");
}

fn run_binary(
    conf: &Path,
    data: &Path,
    run: &Path,
    args: &[&str],
    token: Option<&str>,
    scope_token: Option<&str>,
) -> Output {
    let mut command = Command::new(binary());
    env_on(&mut command, conf, data, run);
    if let Some(token) = token {
        command.env("DAIORAMA_TOKEN", token);
    }
    if let Some(scope_token) = scope_token {
        command.env("DAIORAMA_SCOPE_TOKEN", scope_token);
    }
    command.args(args).output().expect("binary runs")
}

/// A port the kernel just handed out and took back; the window before the manager binds it is
/// microseconds, and the test is single-instance.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
    listener.local_addr().expect("probe addr").port()
}

fn read_trimmed(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
        .trim()
        .to_string()
}

#[test]
fn the_manager_bind_surface_gates_the_cli_end_to_end() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let conf = tmp.path().join("conf");
    let data = tmp.path().join("data");
    let run = tmp.path().join("run");
    let instance_config = conf.join("daiorama").join("net");
    std::fs::create_dir_all(instance_config.join("scopes")).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&run).unwrap();

    let port = free_port();
    let address = format!("127.0.0.1:{port}");
    std::fs::write(
        instance_config.join("config.ron"),
        format!(
            "(version: 1, limits: (mode: off), manager: (start_mode: auto, bind: Some(\"{address}\")))\n"
        ),
    )
    .unwrap();
    // A scope file makes the startup credential ensure mint that scope's network secret.
    std::fs::write(
        instance_config.join("scopes").join("operator.ron"),
        "(sessions: (visibility: all))\n",
    )
    .unwrap();

    let out = run_binary(
        &conf,
        &data,
        &run,
        &["daemon", "start", "--instance", "net"],
        None,
        None,
    );
    assert!(
        out.status.success(),
        "daemon start: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let instance_data = data.join("daiorama").join("net");
    let token = read_trimmed(&instance_data.join("token"));
    let scope_secret = read_trimmed(&instance_data.join("scope-tokens").join("operator"));
    let log = read_trimmed(&instance_data.join("daemon.log"));
    assert!(
        log.contains(&format!("listening on {address}")),
        "the bind is reported: {log}"
    );

    // Authenticated over TCP: the CLI's `operator` scope presents both credentials.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "status",
            "--instance",
            "net",
            "--remote",
            &address,
            "--json",
        ],
        Some(&token),
        Some(&scope_secret),
    );
    assert!(
        out.status.success(),
        "status over TCP: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status is JSON");
    assert_eq!(status["state"], "running");

    // No instance token: the transport gate refuses on the client, before any frame.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["status", "--instance", "net", "--remote", &address],
        None,
        Some(&scope_secret),
    );
    assert!(!out.status.success(), "missing token must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(&format!("token required for {address}")),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Token but no scope secret: the manager's per-request scope gate answers.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["status", "--instance", "net", "--remote", &address],
        Some(&token),
        None,
    );
    assert!(!out.status.success(), "missing scope secret must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("scope required: set scope and scope_token on the request frame"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Rotation window: the file moves, the running manager keeps the value it adopted.
    std::fs::write(instance_data.join("token"), "rotated-token\n").unwrap();
    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "status",
            "--instance",
            "net",
            "--remote",
            &address,
            "--json",
        ],
        Some(&token),
        Some(&scope_secret),
    );
    assert!(
        out.status.success(),
        "the adopted token keeps working until restart: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["status", "--instance", "net", "--remote", &address],
        Some("rotated-token"),
        Some(&scope_secret),
    );
    assert!(!out.status.success(), "the rotated token is not live yet");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("token_required"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Restart: the new value is adopted, the old one is refused.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["daemon", "end", "--instance", "net"],
        None,
        None,
    );
    assert!(
        out.status.success(),
        "daemon end: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut down = false;
    while Instant::now() < deadline {
        let out = run_binary(
            &conf,
            &data,
            &run,
            &["status", "--instance", "net"],
            None,
            None,
        );
        if String::from_utf8_lossy(&out.stdout).contains("no manager running") {
            down = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(down, "the old manager exits before the restart");
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["daemon", "start", "--instance", "net"],
        None,
        None,
    );
    assert!(
        out.status.success(),
        "restart: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_binary(
        &conf,
        &data,
        &run,
        &[
            "status",
            "--instance",
            "net",
            "--remote",
            &address,
            "--json",
        ],
        Some("rotated-token"),
        Some(&scope_secret),
    );
    assert!(
        out.status.success(),
        "the new token after restart: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["status", "--instance", "net", "--remote", &address],
        Some(&token),
        Some(&scope_secret),
    );
    assert!(
        !out.status.success(),
        "the old token after restart is refused"
    );

    let _ = run_binary(
        &conf,
        &data,
        &run,
        &["daemon", "end", "--instance", "net", "--force"],
        None,
        None,
    );
}

/// One raw line-protocol exchange over TCP: connect, send the given lines, read until the
/// peer closes or the read times out.
fn raw_frame(address: &str, lines: &[String]) -> Vec<String> {
    use std::io::{BufRead, BufReader, Write};
    let stream = std::net::TcpStream::connect(address).expect("tcp connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    for line in lines {
        writer.write_all(line.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
    }
    writer.flush().unwrap();
    let mut out = Vec::new();
    for line in BufReader::new(stream).lines() {
        match line {
            Ok(line) => out.push(line),
            Err(_) => break,
        }
    }
    out
}

fn handshake_line() -> String {
    r#"{"id":1,"method":"handshake","params":{"protocol_version":"0.1.0","min_supported":"0.1.0"}}"#
        .to_string()
}

fn endpoint_root(
    tmp: &Path,
    instance: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let conf = tmp.join("conf");
    let data = tmp.join("data");
    let run = tmp.join("run");
    let instance_config = conf.join("daiorama").join(instance);
    std::fs::create_dir_all(instance_config.join("scopes")).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        instance_config.join("scopes").join("operator.ron"),
        "(sessions: (visibility: all))\n",
    )
    .unwrap();
    (conf, data, run)
}

fn write_network_config(conf: &Path, instance: &str, bind: &str) {
    std::fs::write(
        conf.join("daiorama").join(instance).join("config.ron"),
        format!(
            "(version: 1, limits: (mode: off), manager: (start_mode: auto, bind: Some(\"{bind}\")))\n"
        ),
    )
    .unwrap();
}

fn start_daemon(conf: &Path, data: &Path, run: &Path, instance: &str) {
    let out = run_binary(
        conf,
        data,
        run,
        &["daemon", "start", "--instance", instance],
        None,
        None,
    );
    assert!(
        out.status.success(),
        "daemon start: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stop_daemon(conf: &Path, data: &Path, run: &Path, instance: &str) {
    let _ = run_binary(
        conf,
        data,
        run,
        &["daemon", "end", "--instance", instance, "--force"],
        None,
        None,
    );
}

/// The frame gate is the wire contract: after a tokenless handshake, a call without the
/// instance token is `token_required` and the connection ends; with the token, a call
/// without a scope is `scope required` and the connection continues (07).
#[test]
fn a_tcp_frame_needs_the_token_then_a_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (conf, data, run) = endpoint_root(tmp.path(), "frames");
    let port = free_port();
    let address = format!("127.0.0.1:{port}");
    write_network_config(&conf, "frames", &address);
    start_daemon(&conf, &data, &run, "frames");
    let instance_data = data.join("daiorama").join("frames");
    let token = read_trimmed(&instance_data.join("token"));

    let status = r#"{"id":2,"method":"status"}"#.to_string();
    let without = raw_frame(&address, &[handshake_line(), status]);
    assert!(
        without.iter().any(|line| line.contains("token_required")),
        "no token over TCP: {without:?}"
    );
    assert_eq!(
        without.len(),
        2,
        "the transport refusal ends the connection: {without:?}"
    );

    let with_token = format!(r#"{{"id":3,"method":"status","token":"{token}"}}"#);
    let scoped = raw_frame(&address, &[handshake_line(), with_token]);
    assert!(
        scoped.iter().any(|line| line.contains("scope required")),
        "token without scope: {scoped:?}"
    );

    stop_daemon(&conf, &data, &run, "frames");
}

/// The media endpoint on the TCP surface presents `Daiorama-Token`: a missing token is 403,
/// and a 32-hex id the store never minted is the same 404 `media_expired` as an unknown one.
#[test]
fn tcp_media_fetch_requires_the_instance_token() {
    use std::io::{Read, Write};
    let tmp = tempfile::tempdir().expect("tempdir");
    let (conf, data, run) = endpoint_root(tmp.path(), "media");
    let port = free_port();
    let address = format!("127.0.0.1:{port}");
    write_network_config(&conf, "media", &address);
    start_daemon(&conf, &data, &run, "media");
    let instance_data = data.join("daiorama").join("media");
    let token = read_trimmed(&instance_data.join("token"));

    let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let request = |auth: Option<&str>| {
        let mut stream = std::net::TcpStream::connect(&address).expect("tcp connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut head =
            format!("GET /media/{id} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n");
        if let Some(token) = auth {
            head.push_str(&format!("Daiorama-Token: {token}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    };

    let refused = request(None);
    assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
    let answered = request(Some(&token));
    assert!(answered.starts_with("HTTP/1.1 404"), "{answered}");
    assert!(answered.contains("media_expired"), "{answered}");

    stop_daemon(&conf, &data, &run, "media");
}

/// A malformed `manager.bind` refuses the start with the pinned string (04); no silent
/// unix-only manager.
#[test]
fn a_malformed_manager_bind_refuses_the_start() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (conf, data, run) = endpoint_root(tmp.path(), "badbind");
    write_network_config(&conf, "badbind", "not a bind/path");
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["daemon", "start", "--instance", "badbind"],
        None,
        None,
    );
    assert!(!out.status.success(), "bad bind must fail: {out:?}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("manager.bind takes a host or host:port address"),
        "{text}"
    );
}

/// The TCP surface is reachable before any credential check, so its own bounds run first:
/// the 65th connection is closed on accept, the held 64 stay, and the unix socket (a
/// separate listener) keeps answering throughout.
#[test]
fn tcp_connection_cap_closes_the_over_cap_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (conf, data, run) = endpoint_root(tmp.path(), "cap");
    let port = free_port();
    let address = format!("127.0.0.1:{port}");
    write_network_config(&conf, "cap", &address);
    start_daemon(&conf, &data, &run, "cap");

    let mut held = Vec::new();
    for _ in 0..64 {
        held.push(std::net::TcpStream::connect(&address).expect("held connection"));
    }
    std::thread::sleep(Duration::from_millis(300));

    use std::io::Read;
    let mut over = std::net::TcpStream::connect(&address).expect("over-cap connection");
    over.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut byte = [0u8; 1];
    match over.read(&mut byte) {
        Ok(0) | Err(_) => {}
        Ok(_) => panic!("an over-cap socket got bytes"),
    }

    // The unix socket is its own listener: it answers while the TCP cap is saturated.
    let out = run_binary(
        &conf,
        &data,
        &run,
        &["status", "--instance", "cap", "--json"],
        None,
        None,
    );
    assert!(
        out.status.success(),
        "unix stays up under the TCP cap: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    drop(held);
    stop_daemon(&conf, &data, &run, "cap");
}

/// A TCP connection that stops making progress closes at the 10 s deadline — pre-auth, so a
/// dribbled frame cannot hold a slot — and the surface serves the next caller.
#[test]
fn a_stalled_tcp_frame_closes_at_the_progress_deadline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (conf, data, run) = endpoint_root(tmp.path(), "stall");
    let port = free_port();
    let address = format!("127.0.0.1:{port}");
    write_network_config(&conf, "stall", &address);
    start_daemon(&conf, &data, &run, "stall");
    let instance_data = data.join("daiorama").join("stall");
    let token = read_trimmed(&instance_data.join("token"));
    let scope_secret = read_trimmed(&instance_data.join("scope-tokens").join("operator"));

    use std::io::{BufRead, BufReader, Write};
    let stream = std::net::TcpStream::connect(&address).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    writer.write_all(b"{\"id\":1,").unwrap();
    writer.flush().unwrap();
    let start = Instant::now();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader.read_line(&mut line).unwrap_or(0);
    let elapsed = start.elapsed();
    assert_eq!(read, 0, "a stalled frame is closed with nothing written");
    assert!(
        (Duration::from_secs(9)..=Duration::from_secs(13)).contains(&elapsed),
        "closed at the 10 s deadline, took {elapsed:?}"
    );

    // The surface serves the next caller.
    let with_token = format!(
        r#"{{"id":2,"method":"status","token":"{token}","scope":"operator","scope_token":"{scope_secret}"}}"#
    );
    let answered = raw_frame(&address, &[handshake_line(), with_token]);
    assert!(
        answered.iter().any(|line| line.contains("\"result\"")),
        "{answered:?}"
    );
    stop_daemon(&conf, &data, &run, "stall");
}
