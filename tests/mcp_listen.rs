//! `daiorama mcp --listen` conformance: the network bridge authenticates every request with
//! the pinned headers and answers failures as MCP-level errors (05 §The bridge, 07 §Scope
//! model). These tests spawn the real binary and speak raw HTTP, exactly what a harness does.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const BIN: &str = env!("CARGO_BIN_EXE_daiorama");
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

struct Bridge {
    root: tempfile::TempDir,
    child: tokio::process::Child,
    port: u16,
}

impl Bridge {
    async fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let scopes = root.path().join("config/daiorama/default/scopes");
        std::fs::create_dir_all(&scopes).unwrap();
        std::fs::write(
            scopes.join("agent1.ron"),
            "(grant: (categories: [\"input\"]))",
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("data")).unwrap();
        std::fs::create_dir_all(root.path().join("runtime")).unwrap();
        std::fs::create_dir_all(root.path().join("tmp")).unwrap();

        let mut child = tokio::process::Command::new(BIN)
            .arg("--instance")
            .arg("default")
            .arg("mcp")
            .arg("--listen")
            .arg("127.0.0.1:0")
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
            .env("TMPDIR", root.path().join("tmp"))
            .env_remove("DAIORAMA_TOKEN")
            .env_remove("DAIORAMA_SCOPE")
            .env_remove("DAIORAMA_SCOPE_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();

        let stderr = child.stderr.take().unwrap();
        let mut lines = BufReader::new(stderr).lines();
        let mut port = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while port.is_none() {
            let line = tokio::time::timeout_at(deadline, lines.next_line())
                .await
                .expect("the bridge announces its listener")
                .unwrap();
            let line = line.unwrap_or_else(|| panic!("the bridge exited before listening"));
            if let Some(rest) = line.strip_prefix("listening on ") {
                port = rest.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
            }
        }
        // Keep draining stderr so a chatty bridge can never block on a full pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Self {
            root,
            child,
            port: port.unwrap(),
        }
    }

    fn data_path(&self, name: &str) -> PathBuf {
        self.root.path().join("data/daiorama/default").join(name)
    }

    fn token(&self) -> String {
        std::fs::read_to_string(self.data_path("token"))
            .unwrap()
            .trim()
            .to_string()
    }

    fn scope_secret(&self, scope: &str) -> String {
        std::fs::read_to_string(self.data_path(&format!("scope-tokens/{scope}")))
            .unwrap()
            .trim()
            .to_string()
    }

    async fn connection(&self) -> Conn {
        Conn::connect(self.port).await
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

struct Conn {
    reader: BufReader<TcpStream>,
}

impl Conn {
    async fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        Self {
            reader: BufReader::new(stream),
        }
    }

    /// One HTTP request on this connection: JSON-RPC body in, `(status, headers, body)` out.
    async fn request(
        &mut self,
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, HashMap<String, String>, Vec<u8>) {
        let mut request = format!("{method} {target} HTTP/1.1\r\n");
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        request.push_str("\r\n");
        request.push_str(body);
        let stream = self.reader.get_mut();
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();

        let mut head = Vec::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = self.reader.read_until(b'\n', &mut line).await.unwrap();
            if read == 0 {
                return (0, HashMap::new(), Vec::new());
            }
            head.extend_from_slice(&line);
            let trimmed = String::from_utf8_lossy(&line);
            if trimmed.trim_end().is_empty() {
                break;
            }
        }
        let head = String::from_utf8_lossy(&head).to_string();
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);
        let mut parsed = HashMap::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                parsed.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        let length = parsed
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut payload = vec![0u8; length];
        if length > 0 {
            self.reader.read_exact(&mut payload).await.unwrap();
        }
        (status, parsed, payload)
    }

    async fn mcp(
        &mut self,
        headers: &[(&str, &str)],
        body: Value,
    ) -> (u16, HashMap<String, String>, Value) {
        let (status, parsed, payload) = self
            .request("POST", "/mcp", headers, &body.to_string())
            .await;
        let value = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload).unwrap()
        };
        (status, parsed, value)
    }
}

fn handshake() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})
}

fn envelope_code(response: &Value) -> &str {
    response["error"]["data"]["code"]
        .as_str()
        .expect("gate refusals carry the envelope in data")
}

#[tokio::test]
async fn network_gate_requires_token_scope_and_secret() {
    let bridge = Bridge::start().await;
    let token = bridge.token();
    let secret = bridge.scope_secret("agent1");
    let host = format!("127.0.0.1:{}", bridge.port);

    // No credentials at all: the transport gate answers first and reveals nothing else.
    let mut conn = bridge.connection().await;
    let (status, _, response) = conn.mcp(&[("Host", &host)], handshake()).await;
    assert_eq!(
        status, 200,
        "gate failures are MCP-level, not HTTP auth challenges"
    );
    assert_eq!(envelope_code(&response), "token_required");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("DAIORAMA_TOKEN")
    );

    // Instance token, no scope: no anonymous calls, no default scope.
    let (_, _, response) = conn
        .mcp(&[("Host", &host), ("Daiorama-Token", &token)], handshake())
        .await;
    assert_eq!(envelope_code(&response), "scope_auth_required");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Daiorama-Scope")
    );

    // A scope named without its secret is refused even though the scope file exists.
    let (_, _, response) = conn
        .mcp(
            &[
                ("Host", &host),
                ("Daiorama-Token", &token),
                ("Daiorama-Scope", "agent1"),
            ],
            handshake(),
        )
        .await;
    assert_eq!(envelope_code(&response), "scope_auth_required");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Daiorama-Scope-Token")
    );

    // An unknown scope is its own code; existence is not silently invented.
    let (_, _, response) = conn
        .mcp(
            &[
                ("Host", &host),
                ("Daiorama-Token", &token),
                ("Daiorama-Scope", "ghost"),
                ("Daiorama-Scope-Token", "whatever"),
            ],
            handshake(),
        )
        .await;
    assert_eq!(envelope_code(&response), "scope_unknown");

    // A wrong scope secret is refused.
    let (_, _, response) = conn
        .mcp(
            &[
                ("Host", &host),
                ("Daiorama-Token", &token),
                ("Daiorama-Scope", "agent1"),
                ("Daiorama-Scope-Token", "wrong"),
            ],
            handshake(),
        )
        .await;
    assert_eq!(envelope_code(&response), "scope_auth_required");

    // The full credential set reaches the handshake — answered locally, no manager needed.
    let (status, _, response) = conn
        .mcp(
            &[
                ("Host", &host),
                ("Daiorama-Token", &token),
                ("Daiorama-Scope", "agent1"),
                ("Daiorama-Scope-Token", &secret),
            ],
            handshake(),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(response["result"]["protocolVersion"], "2026-07-28");
    assert_eq!(response["result"]["serverInfo"]["name"], "daiorama");
}

#[tokio::test]
async fn tools_list_is_the_authenticated_scopes_manifest() {
    let bridge = Bridge::start().await;
    let host = format!("127.0.0.1:{}", bridge.port);
    let headers = [
        ("Host", host.as_str()),
        ("Daiorama-Token", &bridge.token()),
        ("Daiorama-Scope", "agent1"),
        ("Daiorama-Scope-Token", &bridge.scope_secret("agent1")),
    ];
    let mut conn = bridge.connection().await;
    let (status, _, response) = conn
        .mcp(
            &headers,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .await;
    assert_eq!(status, 200);
    let names: Vec<&str> = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        names.contains(&"pointer_move"),
        "the input category grant expands"
    );
    assert!(
        names.contains(&"create_session"),
        "the always-on lifecycle set rides along"
    );
    assert!(
        !names.contains(&"screenshot"),
        "ungranted families stay hidden"
    );
}

#[tokio::test]
async fn origin_must_match_the_host() {
    let bridge = Bridge::start().await;
    let host = format!("127.0.0.1:{}", bridge.port);
    let mut conn = bridge.connection().await;
    let (status, _, response) = conn
        .mcp(
            &[
                ("Host", &host),
                ("Origin", "http://evil.example"),
                ("Daiorama-Token", &bridge.token()),
                ("Daiorama-Scope", "agent1"),
                ("Daiorama-Scope-Token", &bridge.scope_secret("agent1")),
            ],
            handshake(),
        )
        .await;
    assert_eq!(status, 403);
    assert_eq!(envelope_code(&response), "bad_request");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("origin not allowed")
    );

    // The same request with a matching Origin passes.
    let mut conn = bridge.connection().await;
    let origin = format!("http://{host}");
    let (status, _, response) = conn
        .mcp(
            &[
                ("Host", &host),
                ("Origin", &origin),
                ("Daiorama-Token", &bridge.token()),
                ("Daiorama-Scope", "agent1"),
                ("Daiorama-Scope-Token", &bridge.scope_secret("agent1")),
            ],
            handshake(),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(response["result"]["serverInfo"]["name"], "daiorama");
}

#[tokio::test]
async fn only_post_mcp_is_served() {
    let bridge = Bridge::start().await;
    let host = format!("127.0.0.1:{}", bridge.port);
    let mut conn = bridge.connection().await;
    let (status, headers, payload) = conn.request("GET", "/mcp", &[("Host", &host)], "").await;
    assert_eq!(status, 405);
    assert_eq!(headers.get("allow").map(String::as_str), Some("POST"));
    let response: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(envelope_code(&response), "bad_request");

    let mut conn = bridge.connection().await;
    let (status, _, payload) = conn
        .request("POST", "/other", &[("Host", &host)], "{}")
        .await;
    assert_eq!(status, 404);
    let response: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(envelope_code(&response), "bad_request");
}

#[tokio::test]
async fn oversized_bodies_are_refused_and_the_connection_ends() {
    let bridge = Bridge::start().await;
    let host = format!("127.0.0.1:{}", bridge.port);
    let mut conn = bridge.connection().await;
    let larger = MAX_FRAME_BYTES + 1;
    let head = format!("POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Length: {larger}\r\n\r\n");
    conn.reader
        .get_mut()
        .write_all(head.as_bytes())
        .await
        .unwrap();
    // The refusal is decided from the declared length; the body is never read.
    let mut line = Vec::new();
    let mut response_head = String::new();
    loop {
        line.clear();
        conn.reader.read_until(b'\n', &mut line).await.unwrap();
        response_head.push_str(&String::from_utf8_lossy(&line));
        if line == b"\r\n" {
            break;
        }
    }
    assert!(response_head.starts_with("HTTP/1.1 200"));
    let length = response_head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap();
    let mut payload = vec![0u8; length];
    conn.reader.read_exact(&mut payload).await.unwrap();
    let response: Value = serde_json::from_slice(&payload).unwrap();
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("8388608")
    );
    // Nothing follows the refusal: the connection is closed.
    let mut next = Vec::new();
    let read = conn.reader.read_until(b'\n', &mut next).await.unwrap();
    assert_eq!(read, 0, "the refusal ends the connection");
}

#[tokio::test]
async fn notifications_are_accepted_and_never_execute() {
    let bridge = Bridge::start().await;
    let host = format!("127.0.0.1:{}", bridge.port);
    let mut conn = bridge.connection().await;
    let (status, headers, payload) = conn
        .request(
            "POST",
            "/mcp",
            &[
                ("Host", &host),
                ("Daiorama-Token", &bridge.token()),
                ("Daiorama-Scope", "agent1"),
                ("Daiorama-Scope-Token", &bridge.scope_secret("agent1")),
            ],
            &json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "create_session"}})
                .to_string(),
        )
        .await;
    assert_eq!(status, 202);
    assert!(payload.is_empty());
    assert_eq!(headers.get("content-length").map(String::as_str), Some("0"));
}

#[tokio::test]
async fn network_mode_refuses_a_remote_manager() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    let output = tokio::process::Command::new(BIN)
        .arg("--instance")
        .arg("default")
        .arg("--remote")
        .arg("/nonexistent/manager.sock")
        .arg("mcp")
        .arg("--listen")
        .arg("127.0.0.1:0")
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
        .env("TMPDIR", root.path().join("tmp"))
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("network mode serves the local instance"),
        "stderr: {stderr}"
    );
}

#[tokio::test]
async fn connections_are_reused_across_requests() {
    let bridge = Bridge::start().await;
    let host = format!("127.0.0.1:{}", bridge.port);
    let mut conn = bridge.connection().await;
    let headers = [
        ("Host", host.as_str()),
        ("Daiorama-Token", &bridge.token()),
        ("Daiorama-Scope", "agent1"),
        ("Daiorama-Scope-Token", &bridge.scope_secret("agent1")),
    ];
    for _ in 0..2 {
        let (status, _, response) = conn.mcp(&headers, handshake()).await;
        assert_eq!(status, 200);
        assert_eq!(response["result"]["serverInfo"]["name"], "daiorama");
    }
}

#[tokio::test]
async fn a_relative_config_root_is_not_written_outside_the_temp_root() {
    // The bridge generates the token and the scope secret on first listen; both must land in
    // the instance's data dir and nowhere else.
    let bridge = Bridge::start().await;
    let token = bridge.data_path("token");
    let secret = bridge.data_path("scope-tokens/agent1");
    assert!(token.starts_with(bridge.root.path()));
    assert!(secret.starts_with(bridge.root.path()));
    assert_eq!(
        std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
