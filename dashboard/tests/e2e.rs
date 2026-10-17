//! End-to-end: the module binary against a fake manager over the real frame protocol.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_daiorama-dashboard"))
}
fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ui")
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// A manager that answers the frame protocol; every call must carry the instance token and the
/// `dashboard` scope secret, so a missing field fails the test the same way a real manager does.
fn fake_manager() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            std::thread::spawn(move || serve(stream));
        }
    });
    (port, handle)
}

fn serve(stream: TcpStream) {
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let request: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => return,
        };
        let method = request["method"].as_str().unwrap_or_default();
        let result = match method {
            "handshake" => {
                serde_json::json!({"result": {"instance_id": "11111111-1111-4111-8111-111111111111", "protocol_version": "0.1.0", "min_supported": "0.1.0"}})
            }
            "readiness" | "status" => {
                if request["token"] != "instance-token"
                    || request["scope"] != "dashboard"
                    || request["scope_token"] != "scope-secret"
                {
                    serde_json::json!({"error": {"code": "scope_auth_required", "message": "missing credentials"}})
                } else {
                    serde_json::json!({"result": {"state": "running", "mode": "background", "sessions": {"running": 1, "dead": 0}, "versions": {"protocol": "0.1.0"}}})
                }
            }
            "list_sessions" => {
                serde_json::json!({"result": {"sessions": [{"id": "s-1", "label": "demo", "state": "running", "backend": "headless", "resolution": "1920x1080", "scale": 1.0, "scope": "operator", "origin": "shell", "idle_since": "t0", "view": false}]}})
            }
            "list_history" => {
                serde_json::json!({"result": {"records": [{"at": "2026-10-17T00:00:00Z", "event": "session.created", "session_id": "s-1", "scope": "operator", "origin": "shell"}], "next_cursor": null}})
            }
            "get_view" => {
                serde_json::json!({"result": {"session_id": "s-1", "address": "http://127.0.0.1:6080/vnc.html", "credential": "view-credential", "generation": 1}})
            }
            _ => serde_json::json!({"error": {"code": "bad_request", "message": "unknown method"}}),
        };
        let mut text = serde_json::to_string(&result).unwrap();
        text.push('\n');
        if writer.write_all(text.as_bytes()).is_err() {
            return;
        }
        let _ = writer.flush();
    }
}

fn start_module(config: &Path, ui: &Path) -> (Child, u16) {
    let port = free_port();
    let child = Command::new(binary())
        .args([
            "serve",
            "--bind",
            &format!("127.0.0.1:{port}"),
            "--ui",
            ui.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("module starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "module never bound");
        std::thread::sleep(Duration::from_millis(25));
    }
    (child, port)
}

fn http(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn the_module_serves_the_ui_and_the_read_api_without_leaking_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("connections.ron");
    let (manager_port, _handle) = fake_manager();
    std::fs::write(
        &config,
        format!(
            "(version: 1, connections: [(name: \"local\", address: \"127.0.0.1:{manager_port}\", token: \"instance-token\", scope_token: \"scope-secret\")])\n"
        ),
    )
    .unwrap();
    let (mut child, port) = start_module(&config, &ui_dir());

    let host = format!("127.0.0.1:{port}");
    let page = http(port, &format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n"));
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(page.contains("dAIorama dashboard"), "{page}");

    let managers = http(
        port,
        &format!("GET /api/managers HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert!(managers.starts_with("HTTP/1.1 200"), "{managers}");
    assert!(managers.contains("\"ok\":true"), "{managers}");
    assert!(managers.contains("\"running\":1"), "{managers}");
    assert!(
        !managers.contains("instance-token"),
        "instance token leaked: {managers}"
    );
    assert!(
        !managers.contains("scope-secret"),
        "scope secret leaked: {managers}"
    );

    let sessions = http(
        port,
        &format!("GET /api/sessions?manager=local HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert!(sessions.contains("s-1"), "{sessions}");

    let history = http(
        port,
        &format!("GET /api/history?manager=local&session=s-1 HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert!(history.contains("session.created"), "{history}");

    let body = "{\"manager\":\"local\",\"session\":\"s-1\"}";
    let view = http(
        port,
        &format!(
            "POST /api/view HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(view.contains("view-credential"), "{view}");

    let traversal = http(
        port,
        &format!("GET /../../etc/passwd HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert!(traversal.starts_with("HTTP/1.1 404"), "{traversal}");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn non_loopback_bind_is_refused_with_the_pinned_message() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("connections.ron");
    std::fs::write(&config, "(version: 1, connections: [])\n").unwrap();
    let output = Command::new(binary())
        .args([
            "serve",
            "--bind",
            "0.0.0.0:0",
            "--ui",
            ui_dir().to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("dashboard serve is loopback-only in v1; use SSH forwarding"),
        "{stderr}"
    );
}

fn connect_hold(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
}

#[test]
fn rebinding_hosts_and_foreign_origins_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("connections.ron");
    let (manager_port, _handle) = fake_manager();
    std::fs::write(
        &config,
        format!(
            "(version: 1, connections: [(name: \"local\", address: \"127.0.0.1:{manager_port}\", token: \"instance-token\", scope_token: \"scope-secret\")])\n"
        ),
    )
    .unwrap();
    let (mut child, port) = start_module(&config, &ui_dir());
    let host = format!("127.0.0.1:{port}");
    let body = "{\"manager\":\"local\",\"session\":\"s-1\"}";

    let evil_host = http(
        port,
        &format!("GET /api/managers HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n"),
    );
    assert!(evil_host.starts_with("HTTP/1.1 403"), "{evil_host}");
    assert!(evil_host.contains("host not allowed"), "{evil_host}");

    let no_host = http(port, "GET /api/managers HTTP/1.1\r\n\r\n");
    assert!(no_host.starts_with("HTTP/1.1 403"), "{no_host}");

    let evil_origin = http(
        port,
        &format!(
            "POST /api/view HTTP/1.1\r\nHost: {host}\r\nOrigin: http://evil.example:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(evil_origin.starts_with("HTTP/1.1 403"), "{evil_origin}");
    assert!(evil_origin.contains("origin not allowed"), "{evil_origin}");

    let good_origin = http(
        port,
        &format!(
            "POST /api/view HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(good_origin.starts_with("HTTP/1.1 200"), "{good_origin}");
    assert!(good_origin.contains("view-credential"), "{good_origin}");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn connections_are_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("connections.ron");
    std::fs::write(&config, "(version: 1, connections: [])\n").unwrap();
    let (mut child, port) = start_module(&config, &ui_dir());

    // 64 held sockets; the 65th is closed on accept rather than queued.
    let mut held = Vec::new();
    for _ in 0..64 {
        held.push(connect_hold(port));
    }
    std::thread::sleep(Duration::from_millis(200));
    let mut overflow = connect_hold(port);
    let mut byte = [0u8; 1];
    let closed = matches!(overflow.read(&mut byte), Ok(0) | Err(_));
    assert!(closed, "over-cap connection must be closed, not served");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_silent_head_is_closed_at_the_progress_deadline() {
    // Its own module: a shared instance would let a permit from the cap test's socket still be
    // in flight, closing this connection early as over-cap (observed 1.15 ms once).
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("connections.ron");
    std::fs::write(&config, "(version: 1, connections: [])\n").unwrap();
    let (mut child, port) = start_module(&config, &ui_dir());

    let mut silent = connect_hold(port);
    silent
        .set_read_timeout(Some(Duration::from_secs(14)))
        .unwrap();
    silent
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1")
        .unwrap();
    let started = Instant::now();
    let mut byte = [0u8; 1];
    let result = silent.read(&mut byte);
    let elapsed = started.elapsed();
    assert!(
        matches!(result, Ok(0) | Err(_)),
        "silent head must be closed: {result:?}"
    );
    assert!(
        elapsed >= Duration::from_secs(9),
        "closed too early: {elapsed:?}"
    );
    assert!(
        elapsed <= Duration::from_secs(13),
        "closed too late: {elapsed:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
}
