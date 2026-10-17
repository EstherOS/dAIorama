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
        std::fs::write(scopes.join("agent2.ron"), "(grant: (categories: []))").unwrap();
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

    /// The local manager socket the bridge connects to lazily (04/08 path rule).
    fn manager_socket(&self) -> PathBuf {
        self.root
            .path()
            .join("runtime/daiorama/default/manager.sock")
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
    let mut conn = bridge.connection().await;
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
    let mut conn = bridge.connection().await;
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
    let mut conn = bridge.connection().await;
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
    let mut conn = bridge.connection().await;
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
    let mut conn = bridge.connection().await;
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
    let token = bridge.token();
    let secret = bridge.scope_secret("agent1");
    let mut conn = bridge.connection().await;
    let larger = MAX_FRAME_BYTES + 1;
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nDaiorama-Token: {token}\r\n\
         Daiorama-Scope: agent1\r\nDaiorama-Scope-Token: {secret}\r\n\
         Content-Length: {larger}\r\n\r\n"
    );
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

// ---------------------------------------------------------------------------
// The shared manager link re-stamps the caller identity on EVERY relayed call
// ---------------------------------------------------------------------------

/// A fake manager on the bridge's local socket: captures every frame, answers `handshake`
/// and `list_sessions` — enough for the scope probe (the same shape as tests/mcp.rs's sink).
struct FrameSink {
    frames: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    socket: PathBuf,
    accept: tokio::task::JoinHandle<()>,
    conns: std::sync::Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl FrameSink {
    fn serve(socket: PathBuf) -> Self {
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();
        let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let conns = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = std::sync::Arc::clone(&frames);
        let tracked = std::sync::Arc::clone(&conns);
        let accept = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let frames = std::sync::Arc::clone(&captured);
                let handle = tokio::spawn(async move {
                    let _ = sink_connection(stream, frames).await;
                });
                tracked.lock().unwrap().push(handle);
            }
        });
        Self {
            frames,
            socket,
            accept,
            conns,
        }
    }

    /// Stop the fake manager: abort the accept loop AND every live connection task (the
    /// in-process equivalent of the process dying), then free the socket path — leaving any
    /// cached link in the bridge pointing at a dead end (the restart shape).
    fn stop(self) {
        self.accept.abort();
        for handle in self.conns.lock().unwrap().drain(..) {
            handle.abort();
        }
        std::fs::remove_file(&self.socket).ok();
    }
}

async fn sink_connection(
    stream: tokio::net::UnixStream,
    frames: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(trimmed)?;
        frames.lock().unwrap().push(request.clone());
        let result = match request["method"].as_str().unwrap_or_default() {
            "handshake" => json!({
                "protocol_version": "0.1.0",
                "min_supported": "0.1.0",
                "instance_id": "11111111-1111-4111-8111-111111111111",
            }),
            "list_sessions" => json!({ "sessions": [] }),
            _ => json!({}),
        };
        let mut response = json!({ "result": result });
        if let Some(id) = request.get("id") {
            response["id"] = id.clone();
        }
        write.write_all(format!("{response}\n").as_bytes()).await?;
        write.flush().await?;
    }
}

/// One bridge process, one shared manager link, two authenticated scopes on ONE keep-alive
/// connection: every relayed call must carry the scope of its own caller — a create-only
/// stamp would pin the first caller's identity onto every later request (confused deputy).
#[tokio::test]
async fn each_relayed_call_carries_its_own_scope() {
    let bridge = Bridge::start().await;
    let sink = FrameSink::serve(bridge.manager_socket());
    let host = format!("127.0.0.1:{}", bridge.port);
    let agent1 = [
        ("Host", host.as_str()),
        ("Daiorama-Token", &bridge.token()),
        ("Daiorama-Scope", "agent1"),
        ("Daiorama-Scope-Token", &bridge.scope_secret("agent1")),
    ];
    let agent2 = [
        ("Host", host.as_str()),
        ("Daiorama-Token", &bridge.token()),
        ("Daiorama-Scope", "agent2"),
        ("Daiorama-Scope-Token", &bridge.scope_secret("agent2")),
    ];
    let mut conn = bridge.connection().await;
    for (headers, id) in [(&agent1, 1), (&agent2, 2)] {
        let (status, _, response) = conn
            .mcp(
                headers,
                json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                       "params": {"name": "list_capabilities"}}),
            )
            .await;
        assert_eq!(status, 200, "relayed call {id}: {response}");
    }
    let scopes: Vec<String> = sink
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter(|frame| frame["method"] == "list_sessions")
        .map(|frame| frame["scope"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        scopes,
        vec!["agent1".to_string(), "agent2".to_string()],
        "each relayed call carries its own caller's scope"
    );
}

/// The capability read is the call a harness makes every turn: across a manager restart it
/// must take the evict-and-replay path (the same one the stdio bridge uses), not sit on the
/// dead link it cached before the restart.
#[tokio::test]
async fn capability_read_survives_a_manager_restart_over_listen() {
    let bridge = Bridge::start().await;
    let socket = bridge.manager_socket();
    let first = FrameSink::serve(socket.clone());
    let host = format!("127.0.0.1:{}", bridge.port);
    let headers = [
        ("Host", host.as_str()),
        ("Daiorama-Token", &bridge.token()),
        ("Daiorama-Scope", "agent1"),
        ("Daiorama-Scope-Token", &bridge.scope_secret("agent1")),
    ];
    let call = |id: u64| {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
               "params": {"name": "list_capabilities"}})
    };
    let mut conn = bridge.connection().await;
    let (status, _, first_answer) = conn.mcp(&headers, call(1)).await;
    assert_eq!(status, 200, "{first_answer}");
    assert!(
        first_answer["result"].get("isError").is_none(),
        "the first manager answers the capability poll: {first_answer}"
    );

    // The manager process goes away and a new one binds the same socket while the harness
    // stays connected — nothing calls the bridge in between, so the cached link is a dead end.
    first.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = FrameSink::serve(socket);

    let (status, _, second_answer) = conn.mcp(&headers, call(2)).await;
    assert_eq!(status, 200, "{second_answer}");
    assert!(
        second_answer["result"].get("isError").is_none(),
        "the first post-restart capability call must answer, not surface the dead link: {second_answer}"
    );
    let live = &second_answer["result"]["structuredContent"]["quotas"]["sessions"]["live"];
    assert_eq!(
        live,
        &json!(0),
        "the answer comes from the restarted manager"
    );

    // The replay rode a fresh link, still stamped with the caller identity.
    let listings: Vec<Value> = second
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter(|frame| frame["method"] == "list_sessions")
        .cloned()
        .collect();
    assert_eq!(listings.len(), 1, "one replay listing on the new manager");
    assert_eq!(listings[0]["scope"], "agent1");
    assert_eq!(listings[0]["origin"], "mcp");
}

/// The identity gate answers from the head: an unauthenticated caller with a garbage body
/// gets the auth refusal, not a JSON parse error — the parser is not reachable without
/// credentials.
#[tokio::test]
async fn the_identity_gate_answers_before_the_body_is_parsed() {
    let bridge = Bridge::start().await;
    let host = format!("127.0.0.1:{}", bridge.port);
    let mut conn = bridge.connection().await;
    let (status, _, body) = conn
        .request("POST", "/mcp", &[("Host", &host)], "{not json")
        .await;
    assert_eq!(status, 200);
    let response: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        envelope_code(&response),
        "token_required",
        "auth refusal first, body never parsed: {response}"
    );
}

/// A refused gate closes the connection: with `Expect: 100-continue` the client stops on the
/// refusal and never sends the body — keep-alive would read that phantom body as the next
/// request's head (behind a pooled connection, another client's request).
#[tokio::test]
async fn a_gate_failure_closes_the_connection() {
    let bridge = Bridge::start().await;
    let host = format!("127.0.0.1:{}", bridge.port);
    let token = bridge.token();
    let mut conn = bridge.connection().await;
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nDaiorama-Token: {token}\r\n\
         Expect: 100-continue\r\nContent-Length: 50\r\n\r\n"
    );
    conn.reader
        .get_mut()
        .write_all(head.as_bytes())
        .await
        .unwrap();
    let mut response_head = String::new();
    loop {
        let mut line = Vec::new();
        let read = conn.reader.read_until(b'\n', &mut line).await.unwrap();
        if read == 0 {
            break;
        }
        response_head.push_str(&String::from_utf8_lossy(&line));
        if line == b"\r\n" {
            break;
        }
    }
    assert!(response_head.starts_with("HTTP/1.1 200"), "{response_head}");
    assert!(
        response_head
            .to_ascii_lowercase()
            .contains("connection: close"),
        "a refused gate closes: {response_head}"
    );
    // Nothing follows the refusal — no `100 Continue`, no body read, the connection ends.
    let mut tail = Vec::new();
    loop {
        let mut chunk = [0u8; 1024];
        let n = conn.reader.read(&mut chunk).await.unwrap();
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&chunk[..n]);
    }
    let tail = String::from_utf8_lossy(&tail);
    assert!(
        tail.contains("scope_auth_required") || tail.contains("token_required"),
        "the refusal body, nothing else: {tail}"
    );

    // A fresh connection starts clean.
    let mut fresh = bridge.connection().await;
    let (status, _, response) = fresh
        .mcp(
            &[
                ("Host", &host),
                ("Daiorama-Token", &token),
                ("Daiorama-Scope", "agent1"),
                ("Daiorama-Scope-Token", &bridge.scope_secret("agent1")),
            ],
            handshake(),
        )
        .await;
    assert_eq!(status, 200, "{response}");
    assert!(response["result"].is_object(), "{response}");
}

/// The connection cap closes over-cap sockets on accept: the fd set stays bounded, and the
/// connections already held keep working.
#[tokio::test]
async fn connection_cap_closes_the_over_cap_socket() {
    let bridge = Bridge::start().await;
    let addr = format!("127.0.0.1:{}", bridge.port);
    let mut held = Vec::new();
    for _ in 0..64 {
        held.push(tokio::net::TcpStream::connect(&addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut over = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let mut buf = [0u8; 1];
    match tokio::time::timeout(Duration::from_secs(3), over.read(&mut buf)).await {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(_)) => panic!("an over-cap socket got bytes"),
        Err(_) => panic!("an over-cap socket stayed open"),
    }
    drop(held);
}
