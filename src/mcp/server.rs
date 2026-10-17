//! `daiorama mcp` — the stdio bridge (P3, 05 §The bridge).
//!
//! One process per harness connection: newline-delimited JSON-RPC on stdin/stdout. Scope
//! arrives from the spawn environment (`DAIORAMA_SCOPE`), and sessions belong to the manager —
//! the bridge keeps no session state, so a bridge crash or harness restart costs nothing.
//!
//! The bridge is a presentation relay, not an authority: it loads the caller scope's exposed
//! manifest (deny-by-default plus the always-on lifecycle set) and relays every tool call over
//! the manager's RPC with the caller identity riding the frame (`scope`, `origin: mcp`). The
//! manager enforces quotas, visibility and the pinned wire shapes — the same path for shell and
//! MCP callers.
//!
//! Handshake paths (05): `server/discover` is the first call a 2026-07-28 client may make;
//! `initialize` is the legacy negotiation for earlier revisions. Both are answered locally —
//! they authorize nothing.

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::config::{Paths, ScopeFile};
use crate::ipc::{
    Address, BINARY_VERSION, CONNECT_TIMEOUT, CappedLine, Client, ErrorCode, ErrorEnvelope,
    MAX_FRAME_BYTES, read_capped_line,
};
use crate::mcp::{
    ContentBlock, JsonRpcError, JsonRpcRequest, MCP_REVISION, ToolDefinition, ToolResult,
    UNSUPPORTED_PROTOCOL_VERSION_ERROR, capabilities_payload, tool_definitions,
};
use crate::strings;

/// JSON-RPC protocol error codes the bridge uses for malformed MCP requests. Tool failures
/// never travel here — they are tool-execution results carrying the shared envelope (05).
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;

/// The scope the harness set for this process (`DAIORAMA_SCOPE`, 05).
pub fn scope_from_env() -> Result<String, ErrorEnvelope> {
    std::env::var("DAIORAMA_SCOPE")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(ErrorEnvelope::scope_required)
}

/// Load the caller scope's definition. A scope without a file is `scope_unknown` — the bridge
/// never invents a manifest.
pub fn load_scope(instance: &str, scope: &str) -> Result<ScopeFile, ErrorEnvelope> {
    if !crate::config::valid_scope_name(scope) {
        return Err(ErrorEnvelope::bad_request(
            crate::strings::INVALID_SCOPE_NAME,
        ));
    }
    let path = Paths::from_process(instance).scope_file(scope);
    if !path.exists() {
        return Err(ErrorEnvelope::scope_unknown(scope));
    }
    ScopeFile::load(&path)
}

/// The exposed manifest for a scope: the always-on lifecycle set plus every tool named
/// directly or through a granted category (05 §Tool exposure model, deny-by-default).
pub fn exposed_tools(scope: &ScopeFile) -> Vec<ToolDefinition> {
    tool_definitions()
        .into_iter()
        .filter(|definition| {
            definition.spec.always_on
                || scope
                    .grant
                    .tools
                    .iter()
                    .any(|tool| tool == definition.spec.name)
                || scope
                    .grant
                    .categories
                    .iter()
                    .any(|category| category == definition.spec.category)
        })
        .collect()
}

/// Run the stdio bridge until stdin closes.
pub async fn run_stdio(instance: &str, remote: Option<String>) -> Result<(), ErrorEnvelope> {
    let scope = scope_from_env()?;
    let definition = load_scope(instance, &scope)?;
    let address = match remote {
        Some(raw) => Some(Address::parse(&raw)?),
        None => None,
    };
    let mut bridge = Bridge::new(instance, scope, definition, address);
    let mut client = None;
    bridge
        .serve(
            BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
            &mut client,
        )
        .await
}

/// One bridge instance: scope, manifest, and (lazily) the manager connection. The connection
/// lives with the caller so the network mode can share one link across requests.
pub(crate) struct Bridge {
    instance: String,
    scope: String,
    definition: ScopeFile,
    address: Option<Address>,
    /// The base a `clip_video` resource link is built against: the network listener's own
    /// `http://<Host>` (so a proxy or non-loopback bind keeps working), or `None` on stdio —
    /// where the link uses the `daiorama://` scheme and the `media fetch` CLI is the fetcher.
    media_origin: Option<String>,
}

impl Bridge {
    /// A bridge bound to one caller scope (stdio: from the spawn environment; network: from
    /// the request's authenticated headers).
    pub(crate) fn new(
        instance: &str,
        scope: String,
        definition: ScopeFile,
        address: Option<Address>,
    ) -> Self {
        Self {
            instance: instance.to_string(),
            scope,
            definition,
            address,
            media_origin: None,
        }
    }

    /// Bind the media-link base for a network request (its own `Host`).
    pub(crate) fn with_media_origin(mut self, origin: Option<String>) -> Self {
        self.media_origin = origin;
        self
    }

    async fn serve<R, W>(
        &mut self,
        mut input: R,
        mut output: W,
        client: &mut Option<Client>,
    ) -> Result<(), ErrorEnvelope>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        loop {
            let line = match read_capped_line(&mut input)
                .await
                .map_err(|err| ErrorEnvelope::internal(format!("cannot read stdin: {err}")))?
            {
                CappedLine::Eof => return Ok(()),
                CappedLine::TooLarge => {
                    // The stdin frame class is bounded like the manager's read side: one
                    // refusal, then the connection ends — nothing follows it.
                    let response = json_rpc_error(
                        Value::Null,
                        INVALID_REQUEST,
                        &strings::frame_too_large(MAX_FRAME_BYTES),
                    );
                    write_line(&mut output, &response).await?;
                    return Ok(());
                }
                CappedLine::Line(bytes) => bytes,
            };
            let line = match String::from_utf8(line) {
                Ok(line) => line,
                Err(_) => {
                    let response =
                        json_rpc_error(Value::Null, INVALID_REQUEST, strings::LINE_NOT_UTF8);
                    write_line(&mut output, &response).await?;
                    return Ok(());
                }
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let response = self.handle_line(line, client).await;
            if let Some(response) = response {
                write_line(&mut output, &response).await?;
            }
        }
    }

    /// One request line in, at most one response line out (notifications answer nothing).
    async fn handle_line(&mut self, line: &str, client: &mut Option<Client>) -> Option<Value> {
        let request: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(err) => {
                return Some(json_rpc_error(
                    Value::Null,
                    PARSE_ERROR,
                    &format!("parse error: {err}"),
                ));
            }
        };
        // A notification carries no `id` and never gets a response (JSON-RPC); nothing is
        // executed for one — MCP's only notifications are `notifications/*`, and an action
        // smuggled into one is ignored rather than run without an answer.
        request.id.as_ref()?;
        self.handle_request(request, client).await
    }

    /// One parsed request in, at most one response out. Shared by stdio and the network
    /// bridge; the caller has already bound this `Bridge` to the caller's scope.
    pub(crate) async fn handle_request(
        &mut self,
        request: JsonRpcRequest,
        client: &mut Option<Client>,
    ) -> Option<Value> {
        let id = match request.id.clone() {
            // `"id": null` is not a notification: it is an invalid request that still owes an
            // answer. Staying silent here hangs the caller exactly like a dropped frame.
            Some(Value::Null) => {
                return Some(json_rpc_error(
                    Value::Null,
                    INVALID_REQUEST,
                    strings::null_id(),
                ));
            }
            Some(id) => id,
            None => return None,
        };

        let declared = declared_revision(request.params.as_ref());
        if let Some(declared) = declared
            && declared != MCP_REVISION
        {
            // 05: the 2026-07-28 path declares its revision per request; an unsupported one
            // is -32022 and maps to `version_incompatible` with both pairs.
            let envelope =
                ErrorEnvelope::version_incompatible(declared, declared, MCP_REVISION, MCP_REVISION);
            return Some(json_rpc_envelope_error(Some(id), &envelope));
        }
        let modern = declared == Some(MCP_REVISION);

        let outcome = match request.method.as_str() {
            "initialize" => Ok(initialize_payload(request.params.as_ref())),
            "server/discover" => Ok(discover_payload()),
            "tools/list" => Ok(json!({ "tools": self.tool_listing() })),
            "ping" => Ok(json!({})),
            "tools/call" => self.tool_call(request.params.as_ref(), client).await,
            other => Err(ErrorEnvelope::bad_request(format!(
                "unknown method {other:?}"
            ))),
        };

        let mut result = match outcome {
            Ok(result) => result,
            // A malformed MCP request is a protocol error; tool failures returned below are
            // tool-execution results, never JSON-RPC errors (05 §Pinned read shapes).
            Err(envelope) if request.method == "tools/call" => {
                serde_json::to_value(ToolResult::tool_error(&envelope)).expect("tool result")
            }
            Err(envelope) => return Some(json_rpc_envelope_error(Some(id), &envelope)),
        };
        // A modern client validates these as required fields, so a modern request gets them;
        // a handshake-era client keeps the pre-2026 shape (05 §Stdio conformance surface).
        if modern {
            stamp_modern_envelope(request.method.as_str(), &mut result);
        }
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    fn tool_listing(&self) -> Vec<Value> {
        exposed_tools(&self.definition)
            .into_iter()
            .map(|definition| {
                json!({
                    "name": definition.spec.name,
                    "description": definition.description,
                    "inputSchema": definition.input_schema,
                })
            })
            .collect()
    }

    /// `tools/call`: gate the name against the exposed manifest, then relay. `list_capabilities`
    /// is answered locally (it describes the scope, not a session); every other name goes to
    /// the manager with the caller identity already on the connection.
    async fn tool_call(
        &mut self,
        params: Option<&Value>,
        client: &mut Option<Client>,
    ) -> Result<Value, ErrorEnvelope> {
        let params = params.and_then(Value::as_object).ok_or_else(|| {
            ErrorEnvelope::bad_request("tools/call params must be an object with a name")
        })?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ErrorEnvelope::bad_request("tools/call requires a tool name"))?;
        let arguments = params.get("arguments").cloned().filter(|v| !v.is_null());

        let known = tool_definitions()
            .iter()
            .any(|definition| definition.spec.name == name);
        if !known {
            return Err(ErrorEnvelope::bad_request(strings::unknown_tool(name)));
        }
        if !exposed_tools(&self.definition)
            .iter()
            .any(|definition| definition.spec.name == name)
        {
            return Err(ErrorEnvelope::tool_not_granted(name, &self.scope));
        }
        if name == "list_capabilities" {
            return self.list_capabilities(arguments.as_ref(), client).await;
        }

        let (params, idempotency_key) = split_create_arguments(name, arguments);
        match self.relay(client, name, params, idempotency_key).await {
            Ok(value) => Ok(serde_json::to_value(self.capture_result(value)).expect("tool result")),
            Err(envelope) => {
                Ok(serde_json::to_value(ToolResult::tool_error(&envelope)).expect("tool result"))
            }
        }
    }

    /// A capture result carries `capture: "image" | "media"` (05 §Capture): map it onto MCP's
    /// own content types — `image` for a screenshot, a `resource_link` for a clip — and keep
    /// the rest in `structuredContent`. Every other tool result is a plain `ok(value)`.
    fn capture_result(&self, value: Value) -> ToolResult {
        let Some(kind) = value.get("capture").and_then(Value::as_str) else {
            return ToolResult::ok(value);
        };
        match kind {
            "image" => {
                let data = value
                    .get("data_b64")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mime_type = value
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("image/png")
                    .to_string();
                let structured = json!({
                    "mimeType": mime_type,
                    "bytes": value.get("bytes").cloned().unwrap_or(Value::Null),
                    "width": value.get("width").cloned().unwrap_or(Value::Null),
                    "height": value.get("height").cloned().unwrap_or(Value::Null),
                });
                ToolResult {
                    is_error: false,
                    content: vec![ContentBlock::Image {
                        data,
                        mime_type: mime_type.clone(),
                    }],
                    structured_content: Some(structured),
                }
            }
            "media" => {
                let id = value
                    .get("media_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mime_type = value
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("video/webm")
                    .to_string();
                let (uri, mut content) = match &self.media_origin {
                    Some(origin) => (format!("{origin}/media/{id}"), Vec::new()),
                    None => (format!("daiorama://media/{id}"), Vec::new()),
                };
                content.push(ContentBlock::ResourceLink {
                    uri,
                    name: None,
                    mime_type: Some(mime_type.clone()),
                });
                if self.media_origin.is_none() {
                    // The stdio fetcher: one line naming the exact command.
                    content.push(ContentBlock::Text {
                        text: format!("Fetch with: daiorama media fetch {id} --out <path>"),
                    });
                }
                ToolResult {
                    is_error: false,
                    content,
                    structured_content: Some(value),
                }
            }
            _ => ToolResult::ok(value),
        }
    }

    /// `list_capabilities`: the exposed manifest, effective quotas and audit flags (05). The
    /// live part of the quota comes from the manager's own filtered listing; without a manager
    /// there is no live count and the call answers `no_manager_running`.
    async fn list_capabilities(
        &mut self,
        arguments: Option<&Value>,
        client: &mut Option<Client>,
    ) -> Result<Value, ErrorEnvelope> {
        let limit = arguments
            .and_then(|args| args.get("limit"))
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .max(1) as usize;
        let cursor = arguments
            .and_then(|args| args.get("cursor"))
            .and_then(Value::as_str)
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| ErrorEnvelope::bad_request("invalid cursor"))?
            .unwrap_or(0);

        let live = self.scope_live_count(client).await?;
        let definitions = exposed_tools(&self.definition);
        let start = cursor.min(definitions.len());
        let end = start.saturating_add(limit).min(definitions.len());
        let tools: Vec<&str> = definitions[start..end]
            .iter()
            .map(|definition| definition.spec.name)
            .collect();
        let categories: Vec<&str> = self
            .definition
            .grant
            .categories
            .iter()
            .map(String::as_str)
            .collect();
        let mut payload = capabilities_payload(
            &tools,
            &categories,
            json!({
                "sessions": {
                    "max": self.definition.sessions.max,
                    "live": live,
                }
            }),
            json!({
                "store_clips": self.definition.audit.store_clips,
                "store_screenshots": self.definition.audit.store_screenshots,
            }),
        );
        if end < definitions.len() {
            payload["next_cursor"] = json!(end.to_string());
        }
        Ok(serde_json::to_value(ToolResult::ok(payload)).expect("tool result"))
    }

    /// Live sessions in the caller's scope, from the manager's own filtered listing. A read,
    /// so it takes the replay-once path: the harness polls this call every turn, and a
    /// manager restart between polls must not leave it serving a stale link.
    async fn scope_live_count(
        &mut self,
        client: &mut Option<Client>,
    ) -> Result<u32, ErrorEnvelope> {
        let listing = self
            .relay_read(client, "list_sessions")
            .await
            .map_err(|envelope| {
                if envelope.code == ErrorCode::ManagerUnreachable {
                    ErrorEnvelope::no_manager_running()
                } else {
                    envelope
                }
            })?;
        Ok(listing["sessions"]
            .as_array()
            .map(|sessions| {
                sessions
                    .iter()
                    .filter(|session| {
                        session["scope"].as_str() == Some(self.scope.as_str())
                            && matches!(session["state"].as_str(), Some("running" | "degraded"))
                    })
                    .count() as u32
            })
            .unwrap_or(0))
    }

    /// The manager connection, connected and handshaken on first use. Transport failures are
    /// surfaced as the shared reachability envelope, never a raw connection error. The slot
    /// is caller-owned so stdio and the network bridge can share one link.
    async fn link<'a>(
        &self,
        slot: &'a mut Option<Client>,
    ) -> Result<&'a mut Client, ErrorEnvelope> {
        if slot.is_none() {
            let token = std::env::var("DAIORAMA_TOKEN")
                .ok()
                .filter(|value| !value.is_empty());
            let mut client = match &self.address {
                Some(address) => {
                    Client::connect(address, CONNECT_TIMEOUT, token.as_deref()).await?
                }
                None => Client::connect_local(&self.instance).await?,
            };
            if client.is_gated() {
                // 07: a remote manager on TCP needs the scope's secret on every frame; the
                // bridge receives it the same way the caller presented it (`DAIORAMA_SCOPE_TOKEN`).
                client.set_scope_token(
                    std::env::var("DAIORAMA_SCOPE_TOKEN")
                        .ok()
                        .filter(|value| !value.is_empty()),
                );
            }
            // The handshake is first and pre-auth — it carries no caller identity; the scope
            // rides every call after it (05 A7, 07).
            client.handshake().await?;
            client.set_caller(Some(self.scope.clone()), Some("mcp".to_string()));
            *slot = Some(client);
        }
        let client = slot.as_mut().expect("client set above");
        // The caller identity rides EVERY relayed call, not just the create (05 A7): the link
        // is shared across requests — and, on the network bridge, across scopes — so a
        // create-only stamp would pin the first caller's scope onto every later request: the
        // confused deputy the per-request gate exists to prevent.
        client.set_caller(Some(self.scope.clone()), Some("mcp".to_string()));
        Ok(client)
    }

    /// One relayed request over the shared manager slot. A transport failure means the link
    /// died with the manager it talked to (a crash or a restart between calls): evict it so
    /// the next call reconnects, and surface the reachability envelope for this call.
    async fn relay(
        &self,
        slot: &mut Option<Client>,
        method: &str,
        params: Option<Value>,
        idempotency_key: Option<String>,
    ) -> Result<Value, ErrorEnvelope> {
        let result = {
            let client = self.link(slot).await?;
            client.request(method, params, idempotency_key).await
        };
        if matches!(&result, Err(envelope) if envelope.code == ErrorCode::ManagerUnreachable) {
            *slot = None;
        }
        result
    }

    /// A relayed read. Unlike a tool call it is safe to replay, so a cached link that turned
    /// out dead is replaced once and the read retried: a harness polling this call across a
    /// manager restart sees an answer, not a stale-connection error. A manager that is
    /// genuinely down is not retried (there was no cached link to evict).
    async fn relay_read(
        &self,
        slot: &mut Option<Client>,
        method: &str,
    ) -> Result<Value, ErrorEnvelope> {
        let had_link = slot.is_some();
        match self.relay(slot, method, None, None).await {
            Err(envelope) if had_link && envelope.code == ErrorCode::ManagerUnreachable => {
                self.relay(slot, method, None, None).await
            }
            other => other,
        }
    }
}

/// `create_session` carries its idempotency key as a frame field (05), not as a param.
fn split_create_arguments(name: &str, arguments: Option<Value>) -> (Option<Value>, Option<String>) {
    if name != "create_session" {
        return (arguments, None);
    }
    let mut map = match arguments {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    let key = map
        .remove("idempotency_key")
        .and_then(|value| value.as_str().map(str::to_string));
    let params = if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    };
    (params, key)
}

/// Handshake revisions the bridge serves through the legacy `initialize` path (05). The tool
/// surface is revision-stable, so a client that names one of these is answered with the
/// revision it asked for — the MCP rule that a server replies with a version it supports.
const LEGACY_HANDSHAKE_REVISIONS: [&str; 4] =
    ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

const INSTRUCTIONS: &str =
    "dAIorama desktop sessions. Call list_capabilities to see this scope's tools.";

/// `initialize` — the handshake-era path. Echo the client's revision when it is one the
/// bridge serves; a client that names the modern revision (or an unknown one) gets ours.
fn initialize_payload(params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = requested
        .filter(|version| LEGACY_HANDSHAKE_REVISIONS.contains(version))
        .unwrap_or(MCP_REVISION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "daiorama", "version": BINARY_VERSION },
        "instructions": INSTRUCTIONS,
    })
}

/// `server/discover` — the 2026-07-28 first call. The modern result shape carries
/// `supportedVersions` (required; a modern client reads it), and the identity rides the
/// display-only `_meta` stamp (`io.modelcontextprotocol/serverInfo`, 05 §The bridge).
fn discover_payload() -> Value {
    json!({
        "supportedVersions": [MCP_REVISION],
        "capabilities": { "tools": { "listChanged": false } },
        "instructions": INSTRUCTIONS,
        "_meta": {
            "io.modelcontextprotocol/serverInfo": { "name": "daiorama", "version": BINARY_VERSION },
        },
    })
}

/// The revision a request declares in `_meta` (05 §The bridge). The 2026-07-28 key is
/// namespaced — `io.modelcontextprotocol/protocolVersion` — and that is what a real client
/// sends; the un-namespaced key is accepted too so an older declaration still gets the pinned
/// `-32022` refusal instead of slipping through.
fn declared_revision(params: Option<&Value>) -> Option<&str> {
    let meta = params?.get("_meta")?;
    meta.get("io.modelcontextprotocol/protocolVersion")
        .or_else(|| meta.get("protocolVersion"))
        .and_then(Value::as_str)
}

/// The 2026-07-28 result envelope (05 §Stdio conformance surface): a modern client validates
/// `resultType` on every result, and `ttlMs`/`cacheScope` (both required on the wire) on the
/// cacheable list result. `ttlMs: 0` keeps a scope's manifest never-stale across config reload.
fn stamp_modern_envelope(method: &str, result: &mut Value) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    object.insert("resultType".to_string(), json!("complete"));
    if method == "tools/list" {
        object.insert("ttlMs".to_string(), json!(0));
        object.insert("cacheScope".to_string(), json!("private"));
    }
}

pub(crate) fn json_rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": JsonRpcError { code, message: message.to_string(), data: None },
    })
}

/// A protocol error whose envelope is version_incompatible (`-32022`): both pairs in the
/// message (05), the canonical code in `data`. Every other envelope (auth and gate refusals
/// included) is a `-32600` invalid request carrying its `{code, message, hint?}` in `data`.
pub(crate) fn json_rpc_envelope_error(id: Option<Value>, envelope: &ErrorEnvelope) -> Value {
    let code = if envelope.code == ErrorCode::VersionIncompatible {
        UNSUPPORTED_PROTOCOL_VERSION_ERROR
    } else {
        INVALID_REQUEST
    };
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {
            "code": code,
            "message": envelope.message,
            "data": serde_json::to_value(envelope).expect("envelopes serialize"),
        },
    })
}

async fn write_line<W>(output: &mut W, value: &Value) -> Result<(), ErrorEnvelope>
where
    W: AsyncWrite + Unpin,
{
    let line = serde_json::to_string(value).expect("bridge responses serialize");
    output
        .write_all(line.as_bytes())
        .await
        .map_err(|err| ErrorEnvelope::internal(format!("cannot write stdout: {err}")))?;
    output
        .write_all(b"\n")
        .await
        .map_err(|err| ErrorEnvelope::internal(format!("cannot write stdout: {err}")))?;
    output
        .flush()
        .await
        .map_err(|err| ErrorEnvelope::internal(format!("cannot flush stdout: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScopeFile;

    fn scope_with(ron: &str) -> ScopeFile {
        ScopeFile::from_ron(ron).unwrap()
    }

    #[test]
    fn exposed_manifest_is_deny_by_default_plus_the_always_on_set() {
        let bare = scope_with("()");
        let names: Vec<&str> = exposed_tools(&bare)
            .iter()
            .map(|definition| definition.spec.name)
            .collect();
        assert_eq!(names.len(), 5, "the five always-on lifecycle tools");
        assert!(names.contains(&"create_session"));
        assert!(!names.contains(&"pointer_move"));
        assert!(!names.contains(&"screenshot"));

        let granted = scope_with("(grant: (categories: [\"input\"], tools: [\"screenshot\"]))");
        let names: Vec<&str> = exposed_tools(&granted)
            .iter()
            .map(|definition| definition.spec.name)
            .collect();
        assert!(names.contains(&"pointer_move"), "category grant expands");
        assert!(names.contains(&"screenshot"), "individual grant");
        assert!(!names.contains(&"get_view"), "views stay deny-by-default");
        assert_eq!(crate::mcp::always_on_tools().len(), 5);
    }

    #[test]
    fn initialize_echoes_a_handshake_revision_and_falls_back_to_the_modern_one() {
        let legacy = initialize_payload(Some(&json!({"protocolVersion": "2025-11-25"})));
        assert_eq!(legacy["protocolVersion"], "2025-11-25");
        assert_eq!(legacy["serverInfo"]["name"], "daiorama");
        assert_eq!(legacy["serverInfo"]["version"], BINARY_VERSION);

        let modern = initialize_payload(Some(&json!({"protocolVersion": MCP_REVISION})));
        assert_eq!(modern["protocolVersion"], MCP_REVISION);
        assert_eq!(initialize_payload(None)["protocolVersion"], MCP_REVISION);

        // An unknown (future) revision is never echoed: the server names one it serves.
        let unknown = initialize_payload(Some(&json!({"protocolVersion": "2099-01-01"})));
        assert_eq!(unknown["protocolVersion"], MCP_REVISION);
    }

    #[test]
    fn discover_answers_the_modern_result_shape() {
        let payload = discover_payload();
        assert_eq!(payload["supportedVersions"], json!([MCP_REVISION]));
        assert!(payload["capabilities"]["tools"].is_object());
        assert_eq!(
            payload["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "daiorama"
        );
        assert!(
            payload.get("protocolVersion").is_none(),
            "discover must not answer the initialize shape"
        );
    }

    #[test]
    fn declared_revision_reads_the_namespaced_meta_key() {
        let modern = json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}});
        assert_eq!(declared_revision(Some(&modern)), Some("2026-07-28"));
        let legacy = json!({"_meta": {"protocolVersion": "2025-06-18"}});
        assert_eq!(declared_revision(Some(&legacy)), Some("2025-06-18"));
        assert_eq!(declared_revision(None), None);
        assert_eq!(declared_revision(Some(&json!({}))), None);
    }

    #[test]
    fn modern_requests_get_the_modern_result_envelope() {
        let mut listing = json!({"tools": []});
        stamp_modern_envelope("tools/list", &mut listing);
        assert_eq!(listing["resultType"], "complete");
        assert_eq!(listing["ttlMs"], 0);
        assert_eq!(listing["cacheScope"], "private");

        let mut call = json!({"content": [], "structuredContent": {}});
        stamp_modern_envelope("tools/call", &mut call);
        assert_eq!(call["resultType"], "complete");
        assert!(
            call.get("ttlMs").is_none(),
            "only cacheable results carry cache hints"
        );
    }

    #[test]
    fn create_arguments_split_the_frame_level_idempotency_key() {
        let (params, key) = split_create_arguments(
            "create_session",
            Some(json!({"resolution": "1280x800", "idempotency_key": "k1"})),
        );
        assert_eq!(key.as_deref(), Some("k1"));
        let params = params.unwrap();
        assert_eq!(params["resolution"], "1280x800");
        assert!(params.get("idempotency_key").is_none());

        let (params, key) = split_create_arguments("pointer_move", Some(json!({"x": 1, "y": 2})));
        assert!(key.is_none());
        assert_eq!(params.unwrap()["x"], 1);
    }

    #[test]
    fn unknown_tool_name_is_a_bad_request_string() {
        assert_eq!(strings::unknown_tool("nope"), "unknown tool \"nope\"");
    }

    /// 05 §Capture: the manager's typed payloads become MCP content blocks — `image` for a
    /// screenshot (base64 never duplicated into structured content) and a `resource_link`
    /// for a clip, with the URI built from the serving edge.
    #[test]
    fn capture_payloads_map_to_content_types() {
        let bare = Bridge::new("default", "operator".to_string(), scope_with("()"), None);

        let image = json!({
            "capture": "image",
            "mimeType": "image/png",
            "data_b64": "aGk=",
            "bytes": 2,
            "width": 1920,
            "height": 1080,
        });
        let mapped = bare.capture_result(image);
        assert!(!mapped.is_error);
        match &mapped.content[0] {
            ContentBlock::Image { data, mime_type } => {
                assert_eq!(data, "aGk=");
                assert_eq!(mime_type, "image/png");
            }
            other => panic!("expected an image block, got {other:?}"),
        }
        let structured = mapped.structured_content.unwrap();
        assert_eq!(structured["width"], 1920);
        assert_eq!(structured["height"], 1080);
        assert!(
            structured.get("data_b64").is_none(),
            "no base64 duplication"
        );

        let media = json!({
            "capture": "media",
            "media_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "mimeType": "video/webm",
            "bytes": 10,
            "duration_s": 5,
            "expires_in_s": 600,
        });

        // stdio: the daiorama:// scheme plus one text block naming the CLI fetcher.
        let stdio = bare.capture_result(media.clone());
        match &stdio.content[0] {
            ContentBlock::ResourceLink { uri, mime_type, .. } => {
                assert_eq!(uri, "daiorama://media/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                assert_eq!(mime_type.as_deref(), Some("video/webm"));
            }
            other => panic!("expected a resource link, got {other:?}"),
        }
        match &stdio.content[1] {
            ContentBlock::Text { text } => assert!(
                text.contains("daiorama media fetch aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --out"),
                "{text}"
            ),
            other => panic!("expected the fetch hint, got {other:?}"),
        }

        // network: the request's own Host is the link base.
        let network = Bridge::new("default", "operator".to_string(), scope_with("()"), None)
            .with_media_origin(Some("http://127.0.0.1:8756".to_string()));
        let linked = network.capture_result(media);
        match &linked.content[0] {
            ContentBlock::ResourceLink { uri, .. } => {
                assert_eq!(
                    uri,
                    "http://127.0.0.1:8756/media/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                );
            }
            other => panic!("expected a resource link, got {other:?}"),
        }
        assert_eq!(linked.content.len(), 1, "no fetch hint on a network link");
    }

    #[test]
    fn non_capture_results_stay_plain_ok_values() {
        let bare = Bridge::new("default", "operator".to_string(), scope_with("()"), None);
        let mapped = bare.capture_result(json!({"id": "s-1", "state": "running"}));
        assert!(mapped.content.is_empty());
        assert_eq!(mapped.structured_content.unwrap()["id"], "s-1");
    }
}
