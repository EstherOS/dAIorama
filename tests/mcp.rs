//! `daiorama mcp` stdio conformance: the bridge is a presentation relay over the manager's
//! RPC (05 §The bridge). These tests spawn the real binary against a fake manager and drive
//! stdin/stdout exactly like a harness would.
//!
//! The fake manager asserts the carriage the bridge is responsible for: every forwarded call
//! carries the caller's `scope` and `origin: "mcp"` on the frame, and `create_session`'s
//! idempotency key travels as a frame field, never as a tool argument.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const BIN: &str = env!("CARGO_BIN_EXE_daiorama");
const INSTANCE_ID: &str = "11111111-1111-4111-8111-111111111111";

struct Harness {
    root: tempfile::TempDir,
    socket: PathBuf,
    frames: Arc<Mutex<Vec<Value>>>,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("manager.sock");
        Self {
            root,
            socket,
            frames: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn path(&self) -> &std::path::Path {
        self.root.path()
    }

    fn write_scope(&self, name: &str, ron: &str) {
        let dir = self.root.path().join("config/daiorama/default/scopes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.ron")), ron).unwrap();
    }

    /// Bind the fake manager and serve connections in the background.
    fn serve(&self) -> tokio::task::JoinHandle<()> {
        let listener = std::os::unix::net::UnixListener::bind(&self.socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = UnixListener::from_std(listener).unwrap();
        let frames = Arc::clone(&self.frames);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let frames = Arc::clone(&frames);
                tokio::spawn(async move {
                    let _ = serve_connection(stream, frames).await;
                });
            }
        })
    }

    fn bridge(&self, scope: Option<&str>) -> tokio::process::Child {
        let mut command = tokio::process::Command::new(BIN);
        command
            .arg("--remote")
            .arg(&self.socket)
            .arg("mcp")
            .env("HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join("config"))
            .env("XDG_DATA_HOME", self.path().join("data"))
            .env("XDG_RUNTIME_DIR", self.path().join("runtime"))
            .env("TMPDIR", self.path().join("tmp"))
            .env_remove("DAIORAMA_TOKEN")
            .env_remove("DAIORAMA_SCOPE_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match scope {
            Some(scope) => command.env("DAIORAMA_SCOPE", scope),
            None => command.env_remove("DAIORAMA_SCOPE"),
        };
        command.spawn().unwrap()
    }

    fn frames(&self) -> Vec<Value> {
        self.frames.lock().unwrap().clone()
    }
}

async fn serve_connection(
    stream: UnixStream,
    frames: Arc<Mutex<Vec<Value>>>,
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
        let method = request["method"].as_str().unwrap_or_default();
        let response = match method {
            "handshake" => json!({
                "result": {
                    "protocol_version": "0.1.0",
                    "min_supported": "0.1.0",
                    "instance_id": INSTANCE_ID,
                }
            }),
            "list_sessions" => json!({
                "result": { "sessions": [
                    {"id": "a1", "scope": "agent", "state": "running"},
                    {"id": "a2", "scope": "agent", "state": "dead"},
                    {"id": "o1", "scope": "operator", "state": "running"},
                ]}
            }),
            "create_session" => json!({
                "result": {
                    "id": "s-new",
                    "label": "",
                    "state": "running",
                    "backend": "headless",
                    "resolution": "1280x800",
                    "scale": 1.0,
                    "scope": "agent",
                    "origin": "mcp",
                    "idle_since": "2026-10-17T00:00:00Z",
                    "view": null,
                    "instance_id": INSTANCE_ID,
                    "renderer": "verified",
                }
            }),
            other => json!({
                "error": {"code": "internal", "message": format!("not implemented yet ({other})")}
            }),
        };
        write.write_all(response.to_string().as_bytes()).await?;
        write.write_all(b"\n").await?;
        write.flush().await?;
    }
}

async fn send(
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    request: Value,
) -> Value {
    stdin
        .write_all(format!("{}\n", request).as_bytes())
        .await
        .unwrap();
    stdin.flush().await.unwrap();
    let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("bridge answered within 10s")
        .unwrap()
        .expect("bridge wrote a response line");
    serde_json::from_str(&line).unwrap()
}

/// A bridge started without `DAIORAMA_SCOPE` refuses to run (05, 04 string).
#[tokio::test]
async fn missing_scope_refuses_to_run() {
    let harness = Harness::new();
    let mut child = harness.bridge(None);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("bridge exits")
        .unwrap();
    assert_eq!(status.code(), Some(1));
    let mut stderr = String::new();
    use tokio::io::AsyncReadExt;
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .await
        .unwrap();
    assert!(
        stderr.contains("scope required: set DAIORAMA_SCOPE."),
        "stderr: {stderr}"
    );
}

/// Both handshake paths answer locally; `tools/list` exposes the always-on set plus grants and
/// nothing else, and needs no manager (no connection is opened).
#[tokio::test]
async fn handshakes_and_manifest_are_local() {
    let harness = Harness::new();
    harness.write_scope(
        "agent",
        "(sessions: (visibility: own), grant: (categories: [\"input\"], tools: [\"screenshot\"]))",
    );
    let _server = harness.serve();
    let mut child = harness.bridge(Some("agent"));
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut stdin = child.stdin.take().unwrap();

    let discover = send(
        &mut stdin,
        &mut lines,
        json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}}),
    )
    .await;
    assert_eq!(discover["result"]["protocolVersion"], "2026-07-28");
    assert_eq!(discover["result"]["serverInfo"]["name"], "daiorama");

    let initialize = send(
        &mut stdin,
        &mut lines,
        json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
    )
    .await;
    assert_eq!(initialize["result"]["protocolVersion"], "2026-07-28");

    let listing = send(
        &mut stdin,
        &mut lines,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {}}),
    )
    .await;
    let names: Vec<&str> = listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for name in [
        "create_session",
        "list_capabilities",
        "pointer_move",
        "screenshot",
    ] {
        assert!(names.contains(&name), "manifest carries {name}: {names:?}");
    }
    assert!(
        !names.contains(&"get_view"),
        "views stay deny-by-default: {names:?}"
    );
    assert!(
        harness.frames().is_empty(),
        "tools/list and the handshakes stay local: {:?}",
        harness.frames()
    );

    child.kill().await.unwrap();
}

/// A forwarded call carries the caller identity on the frame; `create_session`'s idempotency
/// key rides the frame, not the arguments.
#[tokio::test]
async fn forwarded_calls_carry_scope_and_origin() {
    let harness = Harness::new();
    harness.write_scope("agent", "(sessions: (visibility: own))");
    let _server = harness.serve();
    let mut child = harness.bridge(Some("agent"));
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut stdin = child.stdin.take().unwrap();

    let response = send(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "create_session",
                "arguments": {"resolution": "1280x800", "idempotency_key": "k1"}
            }
        }),
    )
    .await;
    assert_eq!(response["result"]["structuredContent"]["id"], "s-new");

    let frames = harness.frames();
    let handshake = frames
        .iter()
        .find(|frame| frame["method"] == "handshake")
        .expect("handshake first");
    assert!(
        handshake.get("scope").is_none(),
        "pre-auth handshake is tokenless and scopeless"
    );
    let create = frames
        .iter()
        .find(|frame| frame["method"] == "create_session")
        .expect("create forwarded");
    assert_eq!(create["scope"], "agent");
    assert_eq!(create["origin"], "mcp");
    assert_eq!(create["idempotency_key"], "k1");
    assert_eq!(create["params"]["resolution"], "1280x800");
    assert!(
        create["params"].get("idempotency_key").is_none(),
        "the key travels as a frame field"
    );

    child.kill().await.unwrap();
}

/// A known but ungranted tool is a tool-execution error that never reaches the manager.
#[tokio::test]
async fn ungranted_tool_is_refused_before_the_manager() {
    let harness = Harness::new();
    harness.write_scope("agent", "()");
    let _server = harness.serve();
    let mut child = harness.bridge(Some("agent"));
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut stdin = child.stdin.take().unwrap();

    let response = send(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {"name": "screenshot", "arguments": {"session_id": "s-1"}}
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "tool_not_granted"
    );
    assert!(
        harness.frames().is_empty(),
        "nothing is dialed for a denied tool"
    );

    // A name outside the registry is a malformed request, not a permission refusal.
    let unknown = send(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {"name": "nope", "arguments": {}}
        }),
    )
    .await;
    assert_eq!(unknown["result"]["isError"], true);
    assert_eq!(
        unknown["result"]["structuredContent"]["code"],
        "bad_request"
    );

    child.kill().await.unwrap();
}

/// `list_capabilities` is always-on, answered locally except for the live quota count, which
/// comes from the manager's own filtered listing.
#[tokio::test]
async fn list_capabilities_reports_the_live_quota() {
    let harness = Harness::new();
    harness.write_scope(
        "agent",
        "(sessions: (max: Some(4), visibility: own), audit: (store_clips: true, store_screenshots: false))",
    );
    let _server = harness.serve();
    let mut child = harness.bridge(Some("agent"));
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut stdin = child.stdin.take().unwrap();

    let response = send(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {"name": "list_capabilities", "arguments": {}}
        }),
    )
    .await;
    let payload = &response["result"]["structuredContent"];
    assert_eq!(payload["quotas"]["sessions"]["max"], 4);
    assert_eq!(
        payload["quotas"]["sessions"]["live"], 1,
        "one live agent session (the dead one and the operator one do not count)"
    );
    assert_eq!(payload["audit"]["store_clips"], true);
    assert!(payload["always_on"].as_array().unwrap().len() == 5);

    child.kill().await.unwrap();
}

/// `"id": null` is an invalid request and still owes an answer; an absent id is a
/// notification and owes nothing. Telling them apart matters: silence on a null id hangs the
/// caller exactly like a dropped frame.
#[tokio::test]
async fn null_id_is_refused_while_notifications_stay_silent() {
    let harness = Harness::new();
    harness.write_scope("agent", "()");
    let _server = harness.serve();
    let mut child = harness.bridge(Some("agent"));
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut stdin = child.stdin.take().unwrap();

    let refused = send(
        &mut stdin,
        &mut lines,
        json!({"jsonrpc": "2.0", "id": null, "method": "tools/list", "params": {}}),
    )
    .await;
    assert_eq!(refused["error"]["code"], -32600);
    assert_eq!(
        refused["error"]["message"],
        "id must be a string or a number, not null"
    );

    // An absent id is a notification: nothing is answered and nothing is executed.
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {"name": "list_sessions", "arguments": {}}
    });
    stdin
        .write_all(format!("{notification}\n").as_bytes())
        .await
        .unwrap();
    stdin.flush().await.unwrap();
    let quiet = tokio::time::timeout(Duration::from_millis(300), lines.next_line()).await;
    assert!(quiet.is_err(), "a notification must not be answered");

    // The refusal is dispatch-level: the connection continues.
    let ping = send(
        &mut stdin,
        &mut lines,
        json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
    )
    .await;
    assert_eq!(ping["result"], json!({}));
    assert!(
        harness.frames().is_empty(),
        "neither line reached the manager"
    );

    child.kill().await.unwrap();
}
