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
    JsonRpcError, JsonRpcRequest, MCP_REVISION, ToolDefinition, ToolResult,
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
    let mut bridge = Bridge {
        instance: instance.to_string(),
        scope,
        definition,
        address,
        client: None,
    };
    bridge
        .serve(BufReader::new(tokio::io::stdin()), tokio::io::stdout())
        .await
}

/// One bridge process: scope, manifest, and (lazily) the manager connection.
struct Bridge {
    instance: String,
    scope: String,
    definition: ScopeFile,
    address: Option<Address>,
    client: Option<Client>,
}

impl Bridge {
    async fn serve<R, W>(&mut self, mut input: R, mut output: W) -> Result<(), ErrorEnvelope>
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
            let response = self.handle_line(line).await;
            if let Some(response) = response {
                write_line(&mut output, &response).await?;
            }
        }
    }

    /// One request line in, at most one response line out (notifications answer nothing).
    async fn handle_line(&mut self, line: &str) -> Option<Value> {
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
        let id = match request.id.clone() {
            None => return None,
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
        };

        if let Some(declared) = request
            .params
            .as_ref()
            .and_then(|params| params.get("_meta"))
            .and_then(|meta| meta.get("protocolVersion"))
            .and_then(Value::as_str)
            && declared != MCP_REVISION
        {
            // 05: the 2026-07-28 path declares its revision per request; an unsupported one
            // is -32022 and maps to `version_incompatible` with both pairs.
            let envelope =
                ErrorEnvelope::version_incompatible(declared, declared, MCP_REVISION, MCP_REVISION);
            return Some(json_rpc_envelope_error(Some(id), &envelope));
        }

        let outcome = match request.method.as_str() {
            "initialize" | "server/discover" => Ok(handshake_payload(&request.method)),
            "tools/list" => Ok(json!({ "tools": self.tool_listing() })),
            "ping" => Ok(json!({})),
            "tools/call" => self.tool_call(request.params.as_ref()).await,
            other => Err(ErrorEnvelope::bad_request(format!(
                "unknown method {other:?}"
            ))),
        };

        Some(match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            // A malformed MCP request is a protocol error; tool failures returned below are
            // tool-execution results, never JSON-RPC errors (05 §Pinned read shapes).
            Err(envelope) if request.method == "tools/call" => {
                json!({ "jsonrpc": "2.0", "id": id, "result": ToolResult::tool_error(&envelope) })
            }
            Err(envelope) => json_rpc_envelope_error(Some(id), &envelope),
        })
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
    async fn tool_call(&mut self, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
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
            return self.list_capabilities(arguments.as_ref()).await;
        }

        let (params, idempotency_key) = split_create_arguments(name, arguments);
        let result = {
            let client = self.client().await?;
            client.request(name, params, idempotency_key).await
        };
        match result {
            Ok(value) => Ok(serde_json::to_value(ToolResult::ok(value)).expect("tool result")),
            Err(envelope) => {
                if envelope.code == ErrorCode::ManagerUnreachable {
                    self.client = None;
                }
                Ok(serde_json::to_value(ToolResult::tool_error(&envelope)).expect("tool result"))
            }
        }
    }

    /// `list_capabilities`: the exposed manifest, effective quotas and audit flags (05). The
    /// live part of the quota comes from the manager's own filtered listing; without a manager
    /// there is no live count and the call answers `no_manager_running`.
    async fn list_capabilities(
        &mut self,
        arguments: Option<&Value>,
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

        let live = self.scope_live_count().await?;
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

    /// Live sessions in the caller's scope, from the manager's own filtered listing.
    async fn scope_live_count(&mut self) -> Result<u32, ErrorEnvelope> {
        let result = {
            let client = self.client().await?;
            client.request("list_sessions", None, None).await
        };
        let listing = result.map_err(|envelope| {
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
    /// surfaced as the shared reachability envelope, never a raw connection error.
    async fn client(&mut self) -> Result<&mut Client, ErrorEnvelope> {
        if self.client.is_none() {
            let token = std::env::var("DAIORAMA_TOKEN")
                .ok()
                .filter(|value| !value.is_empty());
            let mut client = match &self.address {
                Some(address) => {
                    Client::connect(address, CONNECT_TIMEOUT, token.as_deref()).await?
                }
                None => Client::connect_local(&self.instance).await?,
            };
            // The handshake is first and pre-auth — it carries no caller identity; the scope
            // rides every call after it (05 A7, 07).
            client.handshake().await?;
            client.set_caller(Some(self.scope.clone()), Some("mcp".to_string()));
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("client set above"))
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

/// The two handshake paths answer the same identity locally (05 §Version handshake): they
/// authorize nothing and reveal nothing beyond the bridge revision and binary version.
fn handshake_payload(method: &str) -> Value {
    let mut payload = json!({
        "protocolVersion": MCP_REVISION,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "daiorama", "version": BINARY_VERSION },
    });
    if method == "initialize" {
        // The legacy negotiation result carries the same three keys; no extra fields.
        payload["instructions"] =
            json!("dAIorama desktop sessions. Call list_capabilities to see this scope's tools.");
    }
    payload
}

fn json_rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": JsonRpcError { code, message: message.to_string(), data: None },
    })
}

/// A protocol error whose envelope is version_incompatible (`-32022`): both pairs in the
/// message (05), the canonical code in `data`.
fn json_rpc_envelope_error(id: Option<Value>, envelope: &ErrorEnvelope) -> Value {
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
    fn handshake_payloads_are_local_and_carry_the_revision() {
        for method in ["initialize", "server/discover"] {
            let payload = handshake_payload(method);
            assert_eq!(payload["protocolVersion"], MCP_REVISION);
            assert_eq!(payload["serverInfo"]["name"], "daiorama");
            assert_eq!(payload["serverInfo"]["version"], BINARY_VERSION);
        }
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
}
