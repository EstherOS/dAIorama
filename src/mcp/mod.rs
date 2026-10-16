//! MCP bridge: **types only** until the bridge lands (P3).
//!
//! The bridge itself (`daiorama mcp`, stdio and `--listen`) is P3: this layer ships the
//! contract shapes a bridge implementation needs — the MCP revision, JSON-RPC framing, the
//! content-type rule for capture results, the tool registry the exposed manifests are written
//! against, and the error mapping (05 §Errors, §Tool exposure model). `daiorama mcp` exists in
//! the CLI surface and answers the not-implemented envelope.
//!
//! No JSON-RPC behavior lives here on purpose: no socket, no transport, no call
//! dispatch.
//!
//! Note on lints: this module is **types only by design** — the bridge that
//! constructs most of these values is P3, and the types are exercised by the unit tests below
//! until then. `dead_code` is allowed module-wide with that reason in mind.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{KNOWN_CATEGORIES, KNOWN_TOOLS, ToolSpec};
use crate::ipc::{ErrorCode, ErrorEnvelope};

/// The MCP revision the bridge implements, pinned by 05 (per-request `_meta` version
/// declaration plus `server/discover`; the legacy `initialize` negotiation is the path for
/// earlier revisions).
pub const MCP_REVISION: &str = "2026-07-28";

/// The JSON-RPC error code MCP uses for an unsupported protocol version; it maps to
/// `version_incompatible` with both pairs in the message (05).
pub const UNSUPPORTED_PROTOCOL_VERSION_ERROR: i32 = -32022;

/// Instance-token header for network MCP endpoints (05 pinned credentials).
pub const HEADER_INSTANCE_TOKEN: &str = "Daiorama-Token";
/// Scope header.
pub const HEADER_SCOPE: &str = "Daiorama-Scope";
/// Scope-secret header.
pub const HEADER_SCOPE_TOKEN: &str = "Daiorama-Scope-Token";

/// The two bridge modes (05): spawned per connection over stdio, or one long-lived network
/// service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeMode {
    /// Default: stdio, scope from the spawn environment (`DAIORAMA_SCOPE`,
    /// `DAIORAMA_SCOPE_TOKEN`).
    Stdio,
    /// `--listen <addr>`: per-connection scope auth, no anonymous calls, no default scope.
    Listen(String),
}

// ---------------------------------------------------------------------------
// JSON-RPC surface (types only)
// ---------------------------------------------------------------------------

/// A JSON-RPC request as the bridge receives it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcRequest {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
}

fn jsonrpc_version() -> String {
    "2.0".to_string()
}

/// A JSON-RPC error object. Protocol errors are reserved for malformed MCP requests; tool
/// failures travel as tool-execution errors carrying the shared envelope (05).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Map an MCP protocol error to the canonical envelope code (05: `-32022` →
/// `version_incompatible` with both pairs in the message).
pub fn protocol_error_to_envelope(
    code: i32,
    message: &str,
    both_pairs: Option<&str>,
) -> ErrorEnvelope {
    match code {
        UNSUPPORTED_PROTOCOL_VERSION_ERROR => {
            let message = match both_pairs {
                Some(pairs) => format!("{message} ({pairs})"),
                None => message.to_string(),
            };
            ErrorEnvelope::new(ErrorCode::VersionIncompatible, message)
        }
        _ => ErrorEnvelope::internal(message),
    }
}

// ---------------------------------------------------------------------------
// Content types (05 §Capture, media, and the content-type rule)
// ---------------------------------------------------------------------------

/// Visual/audio data uses MCP's standard content types — no custom datatype: `screenshot` →
/// an `image` block; `clip_audio` → an `audio` block; `clip_video` → a **resource link** (the
/// MCP spec has no video content type).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    ResourceLink {
        uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
}

/// A tool-execution result: `isError: true` carries the shared envelope in
/// `structuredContent` (05 §Pinned read shapes).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    #[serde(
        rename = "isError",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentBlock>,
    #[serde(
        rename = "structuredContent",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub structured_content: Option<Value>,
}

impl ToolResult {
    /// A tool-execution error: the envelope, structured, never a raw protocol error.
    pub fn tool_error(envelope: &ErrorEnvelope) -> Self {
        Self {
            is_error: true,
            content: vec![ContentBlock::Text {
                text: envelope.message.clone(),
            }],
            structured_content: Some(
                serde_json::to_value(envelope).expect("envelopes always serialize"),
            ),
        }
    }

    /// A successful read: `structuredContent` carries the pinned shape.
    pub fn ok(value: Value) -> Self {
        Self {
            is_error: false,
            content: Vec::new(),
            structured_content: Some(value),
        }
    }
}

// ---------------------------------------------------------------------------
// The tool registry (names are pinned by 05; the manifest is per-scope)
// ---------------------------------------------------------------------------

/// One exposed tool definition: name, category, whether it is in the always-on
/// session-lifecycle set, and the input schema of its named arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub spec: ToolSpec,
    pub description: &'static str,
    pub input_schema: Value,
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Every known tool with its pinned argument schema (05 §Tool exposure model: the first
/// landing category is input, nine tools, and the core pinned names).
pub fn tool_definitions() -> Vec<ToolDefinition> {
    KNOWN_TOOLS
        .iter()
        .copied()
        .map(|spec| ToolDefinition {
            spec,
            description: description_for(spec.name),
            input_schema: input_schema_for(spec.name),
        })
        .collect()
}

pub fn description_for(name: &str) -> &'static str {
    match name {
        "create_session" => "Create a session and return its id.",
        "close_session" => "Explicit teardown of a session. Always explicit; never a side effect.",
        "list_sessions" => "List sessions visible to the caller's scope.",
        "session_status" => {
            "Detail for one session: state, view state, address, idle-since, versions."
        }
        "list_capabilities" => {
            "What this scope may call, plus its effective quotas and audit flags."
        }
        "screenshot" => "Capture the session output as an image content block.",
        "clip_video" => "Capture video and return a single-use, TTL-bounded resource link.",
        "clip_audio" => "Capture audio as an audio content block (when built).",
        "get_view" => "Start the view if none is running and return its address (idempotent).",
        "stop_view" => "Stop the view only; the session is unaffected.",
        "list_history" => "Read the history record, same scoping rule as sessions.",
        "pointer_move" => "Move the pointer to absolute coordinates in session output pixels.",
        "pointer_click" => "Press and release a button at the current pointer position.",
        "pointer_button_down" => "Press a button (the drag primitive: down, move, up).",
        "pointer_button_up" => "Release a button.",
        "pointer_scroll" => {
            "Scroll wheel lines; positive x scrolls right, positive y scrolls down."
        }
        "key_tap" => "Press and release one xkb keysym.",
        "key_down" => "Press and hold one key (chords are down/up around a tap).",
        "key_up" => "Release one key.",
        "type_text" => "Type a string.",
        _ => "Known tool.",
    }
}

/// The pinned input schemas. Parameter names are part of the contract (05).
pub fn input_schema_for(name: &str) -> Value {
    match name {
        "create_session" => schema(
            json!({
                "backend": {"type": "string", "enum": ["headless", "nested", "host"], "default": "headless"},
                "resolution": {"type": "string", "default": "1920x1080"},
                "scale": {"type": "number", "default": 1.0},
                "view": {"type": "boolean", "default": false},
                "label": {"type": "string"},
                "idempotency_key": {"type": "string", "maxLength": 64},
            }),
            &[],
        ),
        "close_session" | "session_status" | "get_view" | "stop_view" => {
            schema(json!({"session_id": {"type": "string"}}), &["session_id"])
        }
        "list_sessions" => schema(
            json!({
                "limit": {"type": "integer"},
                "cursor": {"type": "string"},
            }),
            &[],
        ),
        "list_capabilities" => schema(json!({}), &[]),
        "screenshot" => schema(json!({"session_id": {"type": "string"}}), &[]),
        "clip_video" => schema(json!({"session_id": {"type": "string"}}), &[]),
        "clip_audio" => schema(json!({"session_id": {"type": "string"}}), &[]),
        "list_history" => schema(
            json!({
                "session_id": {"type": "string"},
                "scope": {"type": "string"},
                "limit": {"type": "integer"},
                "cursor": {"type": "string"},
            }),
            &[],
        ),
        "pointer_move" => schema(
            json!({"x": {"type": "integer"}, "y": {"type": "integer"}}),
            &["x", "y"],
        ),
        "pointer_click" => schema(
            json!({"button": {"type": "string", "enum": ["left", "right", "middle"]}}),
            &["button"],
        ),
        "pointer_button_down" | "pointer_button_up" => schema(
            json!({"button": {"type": "string", "enum": ["left", "right", "middle"]}}),
            &["button"],
        ),
        "pointer_scroll" => schema(
            json!({"dx": {"type": "integer"}, "dy": {"type": "integer"}}),
            &["dx", "dy"],
        ),
        "key_tap" | "key_down" | "key_up" => schema(json!({"key": {"type": "string"}}), &["key"]),
        "type_text" => schema(json!({"text": {"type": "string"}}), &["text"]),
        _ => schema(json!({}), &[]),
    }
}

/// The always-on session-lifecycle set (05: the one named exception to deny-by-default).
pub fn always_on_tools() -> Vec<&'static str> {
    KNOWN_TOOLS
        .iter()
        .filter(|tool| tool.always_on)
        .map(|tool| tool.name)
        .collect()
}

/// The grantable categories.
pub fn categories() -> &'static [&'static str] {
    &KNOWN_CATEGORIES
}

/// Is this tool in the always-on set (visible without any grant)?
pub fn is_always_on(name: &str) -> bool {
    KNOWN_TOOLS
        .iter()
        .any(|tool| tool.name == name && tool.always_on)
}

/// The `list_capabilities` read shape: what the caller's scope may call (`null` = the
/// always-on set), plus effective quotas and audit flags (05).
pub fn capabilities_payload(
    tools: &[&str],
    categories: &[&str],
    quotas: Value,
    audit: Value,
) -> Value {
    json!({
        "tools": tools,
        "categories": categories,
        "always_on": always_on_tools(),
        "quotas": quotas,
        "audit": audit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_and_error_code_are_pinned() {
        assert_eq!(MCP_REVISION, "2026-07-28");
        assert_eq!(UNSUPPORTED_PROTOCOL_VERSION_ERROR, -32022);
        assert_eq!(HEADER_INSTANCE_TOKEN, "Daiorama-Token");
        assert_eq!(HEADER_SCOPE, "Daiorama-Scope");
        assert_eq!(HEADER_SCOPE_TOKEN, "Daiorama-Scope-Token");
    }

    #[test]
    fn unsupported_version_maps_to_version_incompatible_with_both_pairs() {
        let envelope = protocol_error_to_envelope(
            UNSUPPORTED_PROTOCOL_VERSION_ERROR,
            "unsupported protocol version",
            Some("client speaks 0.1.0 (min 0.1.0); manager speaks 0.2.0 (min 0.2.0)"),
        );
        assert_eq!(envelope.code, ErrorCode::VersionIncompatible);
        assert!(envelope.message.contains("client speaks 0.1.0"));
        assert!(envelope.message.contains("manager speaks 0.2.0"));

        let other = protocol_error_to_envelope(-32000, "parse error", None);
        assert_eq!(other.code, ErrorCode::Internal);
    }

    /// The nine input tools' arguments are part of the contract (05).
    #[test]
    fn input_tool_schemas_pin_their_arguments() {
        let schema_of = |name: &str| input_schema_for(name);
        assert_eq!(schema_of("pointer_move")["required"], json!(["x", "y"]));
        assert_eq!(schema_of("pointer_scroll")["required"], json!(["dx", "dy"]));
        assert_eq!(
            schema_of("pointer_click")["properties"]["button"]["enum"],
            json!(["left", "right", "middle"])
        );
        for name in ["key_tap", "key_down", "key_up"] {
            assert_eq!(schema_of(name)["properties"]["key"]["type"], "string");
            assert_eq!(schema_of(name)["required"], json!(["key"]));
        }
        assert_eq!(
            schema_of("type_text")["properties"]["text"]["type"],
            "string"
        );
        // Absolute coordinates in session output pixels: integers.
        assert_eq!(
            schema_of("pointer_move")["properties"]["x"]["type"],
            "integer"
        );
    }

    #[test]
    fn tool_definitions_cover_the_registry_and_always_on_set() {
        let definitions = tool_definitions();
        assert_eq!(definitions.len(), KNOWN_TOOLS.len());
        let names: Vec<&str> = definitions.iter().map(|def| def.spec.name).collect();
        for pinned in [
            "create_session",
            "get_view",
            "stop_view",
            "screenshot",
            "clip_video",
            "list_history",
        ] {
            assert!(names.contains(&pinned), "{pinned} is a pinned core name");
        }
        let always_on = always_on_tools();
        assert_eq!(always_on.len(), 5);
        assert!(always_on.contains(&"list_capabilities"));
        assert!(is_always_on("create_session"));
        assert!(!is_always_on("get_view"), "get_view stays deny-by-default");
        assert_eq!(categories().len(), KNOWN_CATEGORIES.len());
    }

    /// `screenshot` rides MCP's image content type; `clip_video` rides a resource link — no
    /// custom datatype (05).
    #[test]
    fn content_blocks_follow_the_content_type_rule() {
        let image = serde_json::to_value(ContentBlock::Image {
            data: "aGk=".to_string(),
            mime_type: "image/png".to_string(),
        })
        .unwrap();
        assert_eq!(
            image,
            json!({"type": "image", "data": "aGk=", "mimeType": "image/png"})
        );

        let link = serde_json::to_value(ContentBlock::ResourceLink {
            uri: "http://127.0.0.1:8756/media/abc123".to_string(),
            name: None,
            mime_type: Some("video/webm".to_string()),
        })
        .unwrap();
        assert_eq!(link["type"], "resource_link");
        assert!(
            link.get("name").is_none(),
            "unset optional fields are omitted"
        );

        let audio = serde_json::to_value(ContentBlock::Audio {
            data: "aGk=".to_string(),
            mime_type: "audio/ogg".to_string(),
        })
        .unwrap();
        assert_eq!(audio["type"], "audio");
    }

    /// Tool errors carry the shared envelope in `structuredContent`, with `isError: true`.
    #[test]
    fn tool_errors_carry_the_shared_envelope_structurally() {
        let envelope = ErrorEnvelope::tool_not_granted("screenshot", "operator");
        let result = ToolResult::tool_error(&envelope);
        assert!(result.is_error);
        let payload = serde_json::to_value(&result).unwrap();
        assert_eq!(payload["isError"], true);
        assert_eq!(payload["structuredContent"]["code"], "tool_not_granted");
        assert_eq!(
            payload["structuredContent"]["hint"],
            "grant the tool in the scope's manifest"
        );

        let ok = serde_json::to_value(ToolResult::ok(json!({"sessions": []}))).unwrap();
        assert!(ok.get("isError").is_none(), "success omits isError");
        assert_eq!(ok["structuredContent"]["sessions"], json!([]));
    }

    #[test]
    fn capabilities_payload_shows_the_always_on_set() {
        let payload = capabilities_payload(
            &["pointer_move"],
            &["input"],
            json!({"sessions": {"max": 4, "used": 1}}),
            json!({"store_clips": false, "store_screenshots": false}),
        );
        assert_eq!(payload["always_on"][0], "create_session");
        assert_eq!(payload["quotas"]["sessions"]["max"], 4);
    }

    #[test]
    fn json_rpc_types_round_trip() {
        let request: JsonRpcRequest = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "pointer_move", "arguments": {"x": 1, "y": 2}},
        }))
        .unwrap();
        assert_eq!(request.method, "tools/call");
        assert_eq!(request.params.unwrap()["name"], "pointer_move");

        // A request without an explicit jsonrpc version still parses (older revisions).
        let legacy: JsonRpcRequest =
            serde_json::from_value(json!({"method": "initialize", "id": 1})).unwrap();
        assert_eq!(legacy.jsonrpc, "2.0");

        let error = JsonRpcError {
            code: UNSUPPORTED_PROTOCOL_VERSION_ERROR,
            message: "unsupported protocol version".to_string(),
            data: None,
        };
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"code": -32022, "message": "unsupported protocol version"})
        );
    }
}
