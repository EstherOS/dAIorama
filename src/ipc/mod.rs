//! IPC contract: frames, error envelope + canonical codes, socket-path resolution, client.
//!
//! Everything here is pinned by `docs/05-mcp-bridge.md` (§Errors,
//! §Version handshake) and `docs/08-config.md` (paths). Two rules that everything else
//! depends on:
//!
//! 1. **Framing.** Newline-delimited UTF-8 JSON over a unix stream socket (or TCP when
//!    `--remote host:port` names one). One object per line, no other framing, at most
//!    [`MAX_FRAME_BYTES`] of JSON text per frame. Requests are
//!    `{"method": "...", "params": {...}|null, "idempotency_key": "..."|omitted}`; responses
//!    are `{"result": {...}}` or `{"error": {"code", "message", "hint?"}}`.
//! 2. **Handshake first.** `handshake` is the first call on every connection, tokenless on
//!    every transport, and authorizes nothing (05 A7).
//!
//! Dependency position: this module sits directly above [`crate::strings`] (text only) and
//! depends on nothing else in the crate; `cli`, `manager` and `mcp` build on it.
//!
//! Note on lints: this crate is the P2 phase-1 **contract layer** — the canonical envelope
//! compositions below are constructed by the runtime half (W2/W3) as much as by today's CLI,
//! and are all exercised by the module's unit tests until then. `dead_code` is allowed
//! module-wide with that reason in mind; it goes away as the consumers land.
#![allow(dead_code)]

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::strings;

// ---------------------------------------------------------------------------
// Protocol version constants
// ---------------------------------------------------------------------------

/// Protocol version this build speaks.
///
/// Protocol version is its own semver, separate from the binary version (05). These numbers
/// are provisional until P2 review pins them: the contract layer ships `0.1.0`/`0.1.0` and the
/// handshake comparison is what everything else keys off, so pinning the numbers later is a
/// one-line change here and nowhere else.
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// Oldest protocol version this build still accepts (05: surfaced by `version`,
/// `status --json` and readiness).
pub const MIN_SUPPORTED: &str = "0.1.0";

/// The binary's own version (`daiorama version` prints it as the core version).
pub const BINARY_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Method names. Tool-shaped methods mirror their MCP tool names exactly (05); the rest are
/// RPC-only.
pub mod methods {
    pub const HANDSHAKE: &str = "handshake";
    pub const STATUS: &str = "status";
    pub const READINESS: &str = "readiness";
    pub const CREATE_SESSION: &str = "create_session";
    pub const CLOSE_SESSION: &str = "close_session";
    pub const LIST_SESSIONS: &str = "list_sessions";
    pub const SESSION_STATUS: &str = "session_status";
    pub const GET_VIEW: &str = "get_view";
    pub const STOP_VIEW: &str = "stop_view";
    pub const LIST_HISTORY: &str = "list_history";
    pub const CONFIG_GET: &str = "config_get";
    pub const CONFIG_SET: &str = "config_set";
    /// Re-read the core config on a running manager (04/08). RPC-only, added by the contract
    /// layer because the CLI's `config reload` needs a method to call (see report).
    pub const CONFIG_RELOAD: &str = "config_reload";
    pub const DRAIN: &str = "drain";
    pub const DAEMON_END: &str = "daemon_end";
}

// ---------------------------------------------------------------------------
// Protocol versions and the handshake
// ---------------------------------------------------------------------------

/// A protocol version: strict `major.minor.patch`, numeric components only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl ProtocolVersion {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for ProtocolVersion {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = raw.split('.').collect();
        if parts.len() != 3 {
            return Err(format!("{raw:?} is not major.minor.patch"));
        }
        let mut nums = [0u64; 3];
        for (i, part) in parts.iter().enumerate() {
            nums[i] = part
                .parse::<u64>()
                .map_err(|_| format!("{raw:?} is not major.minor.patch"))?;
        }
        Ok(Self::new(nums[0], nums[1], nums[2]))
    }
}

/// `handshake` params: what the client speaks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandshakeParams {
    pub protocol_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_supported: Option<String>,
}

impl HandshakeParams {
    /// The params this build opens every connection with.
    pub fn ours() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            min_supported: Some(MIN_SUPPORTED.to_string()),
        }
    }

    /// 05: an omitted `min_supported` is treated as equal to `protocol_version`.
    pub fn range(&self) -> Result<(ProtocolVersion, ProtocolVersion), String> {
        let version = ProtocolVersion::from_str(&self.protocol_version)?;
        let min = match &self.min_supported {
            Some(raw) => ProtocolVersion::from_str(raw)?,
            None => version,
        };
        Ok((version, min))
    }
}

/// `handshake` result: what the manager speaks, plus the routing aid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandshakeResult {
    pub protocol_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_supported: Option<String>,
    pub instance_id: String,
}

impl HandshakeResult {
    /// 05: an omitted `min_supported` is treated as equal to `protocol_version`.
    pub fn range(&self) -> Result<(ProtocolVersion, ProtocolVersion), String> {
        let version = ProtocolVersion::from_str(&self.protocol_version)?;
        let min = match &self.min_supported {
            Some(raw) => ProtocolVersion::from_str(raw)?,
            None => version,
        };
        Ok((version, min))
    }
}

/// 05 §Version handshake, the normative rule: a connection is allowed iff the supported
/// ranges overlap, i.e. `max(client.min, manager.min) <= min(client.version, manager.version)`.
/// Inverted or empty ranges fail this by construction and are `version_incompatible`.
pub fn ranges_overlap(
    client_signature: (ProtocolVersion, ProtocolVersion),
    manager_signature: (ProtocolVersion, ProtocolVersion),
) -> bool {
    let (client_version, client_min) = client_signature;
    let (manager_version, manager_min) = manager_signature;
    client_min.max(manager_min) <= client_version.min(manager_version)
}

// ---------------------------------------------------------------------------
// Error codes and the error envelope
// ---------------------------------------------------------------------------

/// Every canonical code from 05 §Errors. Additive-only, like tool names: never renamed,
/// never removed. The exhaustive-set unit test reads the frozen document and fails if this
/// enum and 05 drift apart in either direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ErrorCode {
    // Session
    SessionNotFound,
    SessionReaped,
    SessionClosed,
    SessionDead,
    // Identity
    NoSessionSpecified,
    ScopeUnknown,
    ScopeAuthRequired,
    ScopeRequired,
    TokenRequired,
    NoManagerRunning,
    // Permissions / limits
    ToolNotGranted,
    QuotaExceeded,
    RateLimited,
    Draining,
    // Creation
    RendererRefused,
    BackendBusy,
    LimitsUnavailable,
    // Protocol
    VersionIncompatible,
    BadRequest,
    Internal,
    // Views / media
    ViewUnavailable,
    MediaExpired,
    // Host / deps
    BindRefused,
    HyprlandMissing,
    HyprlandUnsupported,
    ModuleNotInstalled,
    // Transport (client-side presentation)
    ManagerUnreachable,
}

impl ErrorCode {
    /// The canonical wire string (05).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionNotFound => "session_not_found",
            Self::SessionReaped => "session_reaped",
            Self::SessionClosed => "session_closed",
            Self::SessionDead => "session_dead",
            Self::NoSessionSpecified => "no_session_specified",
            Self::ScopeUnknown => "scope_unknown",
            Self::ScopeAuthRequired => "scope_auth_required",
            Self::ScopeRequired => "scope_required",
            Self::TokenRequired => "token_required",
            Self::NoManagerRunning => "no_manager_running",
            Self::ToolNotGranted => "tool_not_granted",
            Self::QuotaExceeded => "quota_exceeded",
            Self::RateLimited => "rate_limited",
            Self::Draining => "draining",
            Self::RendererRefused => "renderer_refused",
            Self::BackendBusy => "backend_busy",
            Self::LimitsUnavailable => "limits_unavailable",
            Self::VersionIncompatible => "version_incompatible",
            Self::BadRequest => "bad_request",
            Self::Internal => "internal",
            Self::ViewUnavailable => "view_unavailable",
            Self::MediaExpired => "media_expired",
            Self::BindRefused => "bind_refused",
            Self::HyprlandMissing => "hyprland_missing",
            Self::HyprlandUnsupported => "hyprland_unsupported",
            Self::ModuleNotInstalled => "module_not_installed",
            Self::ManagerUnreachable => "manager_unreachable",
        }
    }

    /// Every code, in the order 05 lists them.
    pub const ALL: [Self; 27] = [
        Self::SessionNotFound,
        Self::SessionReaped,
        Self::SessionClosed,
        Self::SessionDead,
        Self::NoSessionSpecified,
        Self::ScopeUnknown,
        Self::ScopeAuthRequired,
        Self::ScopeRequired,
        Self::TokenRequired,
        Self::NoManagerRunning,
        Self::ToolNotGranted,
        Self::QuotaExceeded,
        Self::RateLimited,
        Self::Draining,
        Self::RendererRefused,
        Self::BackendBusy,
        Self::LimitsUnavailable,
        Self::VersionIncompatible,
        Self::BadRequest,
        Self::Internal,
        Self::ViewUnavailable,
        Self::MediaExpired,
        Self::BindRefused,
        Self::HyprlandMissing,
        Self::HyprlandUnsupported,
        Self::ModuleNotInstalled,
        Self::ManagerUnreachable,
    ];
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ErrorCode {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|code| code.as_str() == raw)
            .ok_or_else(|| format!("unknown error code {raw:?}"))
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

/// 05: every failure is a stable envelope, identical over MCP and RPC:
/// `{"code", "message", "hint?"}`. `hint` is the corrective step where one exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorEnvelope {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ErrorEnvelope {
    /// Free-form envelope (only for codes whose message is genuinely dynamic).
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    // -- Session ----------------------------------------------------------

    /// 05 `session_not_found`. Message/hint pinned by the contract-layer fixture.
    pub fn session_not_found() -> Self {
        Self::new(ErrorCode::SessionNotFound, "session not found")
            .with_hint(strings::HINT_LIST_SESSIONS)
    }

    /// 05/06: reaped ≠ closed ≠ dead — distinct codes, distinct states.
    pub fn session_reaped() -> Self {
        Self::new(ErrorCode::SessionReaped, "session was reaped")
            .with_hint(strings::HINT_CREATE_A_NEW_SESSION)
    }

    pub fn session_closed() -> Self {
        Self::new(ErrorCode::SessionClosed, "session is closed")
            .with_hint(strings::HINT_CREATE_A_NEW_SESSION)
    }

    pub fn session_dead() -> Self {
        Self::new(ErrorCode::SessionDead, "session is dead")
            .with_hint(strings::HINT_CREATE_A_NEW_SESSION)
    }

    // -- Identity ---------------------------------------------------------

    /// 04 string, verbatim.
    pub fn no_session_specified() -> Self {
        Self::new(ErrorCode::NoSessionSpecified, strings::NO_SESSION_SPECIFIED)
            .with_hint(strings::HINT_CREATE_A_SESSION_FIRST)
    }

    pub fn scope_unknown(scope: &str) -> Self {
        Self::new(ErrorCode::ScopeUnknown, format!("unknown scope {scope:?}"))
    }

    pub fn scope_auth_required(scope: &str) -> Self {
        Self::new(
            ErrorCode::ScopeAuthRequired,
            format!("scope {scope:?} requires its scope secret (DAIORAMA_SCOPE_TOKEN)"),
        )
    }

    /// 04 string, verbatim.
    pub fn scope_required() -> Self {
        Self::new(ErrorCode::ScopeRequired, strings::SCOPE_REQUIRED)
            .with_hint(strings::HINT_SET_SCOPE_ENV)
    }

    /// 04 string, verbatim.
    pub fn token_required(addr: &str) -> Self {
        Self::new(ErrorCode::TokenRequired, strings::token_required(addr))
            .with_hint(strings::HINT_TOKEN_SHOW)
    }

    /// The cold answer: an answer, not an error — no hint (04 pins the exact two-field form).
    pub fn no_manager_running() -> Self {
        Self::new(ErrorCode::NoManagerRunning, strings::NO_MANAGER_RUNNING)
    }

    // -- Permissions / limits ---------------------------------------------

    pub fn tool_not_granted(tool: &str, scope: &str) -> Self {
        Self::new(
            ErrorCode::ToolNotGranted,
            format!("tool {tool:?} is not granted to scope {scope:?}"),
        )
        .with_hint(strings::HINT_GRANT_IN_SCOPE)
    }

    /// 04 string, verbatim (message carries scope + global numbers, per 05).
    pub fn quota_exceeded(scope: &str, n: u32, max: u32, global: u32, global_max: u32) -> Self {
        Self::new(
            ErrorCode::QuotaExceeded,
            strings::session_limit_reached(scope, n, max, global, global_max),
        )
        .with_hint(strings::HINT_CLOSE_OR_RAISE_QUOTA)
    }

    pub fn rate_limited(scope: &str) -> Self {
        Self::new(
            ErrorCode::RateLimited,
            format!("rate limit exceeded for scope {scope:?}"),
        )
        .with_hint(strings::HINT_BACK_OFF)
    }

    /// 04 string, verbatim.
    pub fn draining(remaining: u32) -> Self {
        Self::new(ErrorCode::Draining, strings::draining(remaining))
            .with_hint(strings::HINT_RETRY_AFTER_UPGRADE)
    }

    // -- Creation ---------------------------------------------------------

    /// 04 string, verbatim (06 renderer policy).
    pub fn renderer_refused(renderer: &str) -> Self {
        Self::new(
            ErrorCode::RendererRefused,
            strings::renderer_refused(renderer),
        )
        .with_hint(strings::HINT_RUN_DOCTOR)
    }

    /// 04 string, verbatim (06 B6 host exclusivity).
    pub fn backend_busy(id: &str) -> Self {
        Self::new(ErrorCode::BackendBusy, strings::backend_busy(id))
            .with_hint(strings::HINT_USE_HEADLESS)
    }

    /// 04 string, verbatim (06 C6 containment preconditions).
    pub fn limits_unavailable(detail: &str) -> Self {
        Self::new(
            ErrorCode::LimitsUnavailable,
            strings::containment_unavailable(detail),
        )
        .with_hint(strings::HINT_LIMITS_OFF)
    }

    // -- Protocol ---------------------------------------------------------

    /// 04 string, verbatim: the message carries both version pairs (05).
    pub fn version_incompatible(
        client_version: &str,
        client_min: &str,
        manager_version: &str,
        manager_min: &str,
    ) -> Self {
        Self::new(
            ErrorCode::VersionIncompatible,
            strings::protocol_mismatch(client_version, client_min, manager_version, manager_min),
        )
        .with_hint(strings::HINT_UPGRADE_PROTOCOL)
    }

    /// A malformed request only (schema, grammar, unknown keys, unknown cursor) — semantic
    /// refusals always carry their own code (05).
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message).with_hint(strings::HINT_CHECK_HELP)
    }

    /// `bad_request` for a local-file problem, where "check the command's --help" is not the
    /// corrective step (the file, not the request, is the thing to fix).
    pub fn bad_request_local(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// A pinned-surface method this build does not implement yet (P2 hardening): the call is
    /// valid and the build is what is missing. Pinned in the fixture README's frame contract.
    pub fn not_implemented() -> Self {
        Self::new(ErrorCode::Internal, strings::NOT_IMPLEMENTED_MESSAGE)
    }

    /// A method name outside the pinned surface (P2 hardening): a client-side refusal —
    /// dispatch-level, so the connection continues. Pinned in the frame contract.
    pub fn unknown_method(method: &str) -> Self {
        Self::new(ErrorCode::BadRequest, strings::unknown_method(method))
    }

    // -- Views / media ----------------------------------------------------

    /// 04 string, verbatim.
    pub fn view_unavailable(session: &str, reason: &str) -> Self {
        Self::new(
            ErrorCode::ViewUnavailable,
            strings::view_unavailable(session, reason),
        )
        .with_hint(strings::HINT_VIEW_RETRY)
    }

    pub fn media_expired() -> Self {
        Self::new(ErrorCode::MediaExpired, "media link expired")
            .with_hint(strings::HINT_CAPTURE_AGAIN)
    }

    // -- Host / deps ------------------------------------------------------

    /// 04 string, verbatim.
    pub fn bind_refused(addr: &str, path: &str) -> Self {
        Self::new(ErrorCode::BindRefused, strings::bind_refused(addr, path))
    }

    /// 04 string, verbatim.
    pub fn hyprland_missing(resolved_command: &str) -> Self {
        Self::new(
            ErrorCode::HyprlandMissing,
            strings::hyprland_missing(resolved_command),
        )
    }

    /// 04 string, verbatim.
    pub fn hyprland_unsupported(found: &str, min: &str, max: &str) -> Self {
        Self::new(
            ErrorCode::HyprlandUnsupported,
            strings::hyprland_unsupported(found, min, max),
        )
    }

    /// 04 string, verbatim.
    pub fn module_not_installed() -> Self {
        Self::new(ErrorCode::ModuleNotInstalled, strings::MODULE_NOT_INSTALLED)
    }

    // -- Transport --------------------------------------------------------

    /// 04 string, verbatim: the client-side presentation of an unreachable manager.
    pub fn manager_unreachable(addr: &str) -> Self {
        Self::new(
            ErrorCode::ManagerUnreachable,
            strings::manager_unreachable(addr),
        )
    }
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// The request frame: `{"method", "params": {...}|null, "idempotency_key": "..."|omitted}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub method: String,
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// 05: `idempotency_key` — string ≤ 64 chars, unique per (scope, logical operation).
pub const IDEMPOTENCY_KEY_MAX: usize = 64;

impl Request {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            method: method.into(),
            params,
            idempotency_key: None,
        }
    }

    pub fn with_idempotency_key(mut self, key: Option<String>) -> Self {
        self.idempotency_key = key;
        self
    }

    /// Frame-level validation the manager applies before dispatch.
    pub fn validate(&self) -> Result<(), ErrorEnvelope> {
        if self.method.is_empty() {
            return Err(ErrorEnvelope::bad_request("request has an empty method"));
        }
        if let Some(key) = &self.idempotency_key {
            if key.is_empty() {
                return Err(ErrorEnvelope::bad_request(
                    "idempotency_key must not be empty",
                ));
            }
            if key.chars().count() > IDEMPOTENCY_KEY_MAX {
                return Err(ErrorEnvelope::bad_request(format!(
                    "idempotency_key must be at most {IDEMPOTENCY_KEY_MAX} characters"
                )));
            }
        }
        Ok(())
    }
}

/// The response frame: exactly one of `{"result": ...}` / `{"error": {...}}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Response {
    Result(Value),
    Error(ErrorEnvelope),
}

impl Response {
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// The payload a `--json` read prints: the result body, or the envelope (04: errors emit
    /// the envelope verbatim; a cold answer is the same two-field shape with exit 0).
    pub fn payload(&self) -> &Value {
        match self {
            Self::Result(value) => value,
            Self::Error(_) => unreachable!("errors are printed as envelopes, not payloads"),
        }
    }
}

/// One JSON object per line, no other framing (write side).
pub async fn write_frame<W>(writer: &mut W, frame: &(impl Serialize + ?Sized)) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut line = serde_json::to_string(frame)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    debug_assert!(!line.contains('\n'), "frames carry no embedded newlines");
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

/// The read-side frame bound: one frame — the JSON text between newlines — is at most this
/// many bytes. The bound exists so neither side can be made to buffer an unbounded line:
/// worst case without it, a runaway peer OOMs the manager, which owns every session
/// (P2 hardening; refusal string pinned by the fixture README's frame contract).
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// One read off the wire: a complete line (newline stripped), clean EOF, or the frame cap
/// reached before a newline arrived. The manager refuses `TooLarge` (`bad_request`) and ends
/// the connection — nothing follows it, like a refused handshake.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CappedLine {
    Line(Vec<u8>),
    Eof,
    TooLarge,
}

/// Read one newline-terminated line, never buffering more than [`MAX_FRAME_BYTES`] (the
/// newline is not counted). Blank lines are not special here — callers skip them. EOF
/// mid-line returns the bytes so far as a final line (the reading `read_line` gives).
pub(crate) async fn read_capped_line<R>(reader: &mut R) -> io::Result<CappedLine>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() {
                CappedLine::Eof
            } else {
                CappedLine::Line(buf)
            });
        }
        match chunk.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                if buf.len() + pos > MAX_FRAME_BYTES {
                    reader.consume(pos + 1);
                    return Ok(CappedLine::TooLarge);
                }
                buf.extend_from_slice(&chunk[..pos]);
                reader.consume(pos + 1);
                return Ok(CappedLine::Line(buf));
            }
            None => {
                let n = chunk.len();
                if buf.len() + n > MAX_FRAME_BYTES {
                    reader.consume(n);
                    return Ok(CappedLine::TooLarge);
                }
                buf.extend_from_slice(chunk);
                reader.consume(n);
            }
        }
    }
}

/// One JSON object per line, no other framing (read side). Blank lines are skipped; a
/// non-object line, an over-cap line, an object that is neither `result` nor `error`, and
/// EOF all surface as `io::Error`s the caller turns into a transport or protocol error.
pub async fn read_frame<R>(reader: &mut R) -> io::Result<Response>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        match read_capped_line(reader).await? {
            CappedLine::Eof => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "manager closed the connection without a response",
                ));
            }
            CappedLine::TooLarge => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("frame exceeds the {MAX_FRAME_BYTES}-byte cap"),
                ));
            }
            CappedLine::Line(bytes) => {
                let line = String::from_utf8(bytes).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "response line is not valid UTF-8",
                    )
                })?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                return serde_json::from_str::<Response>(trimmed).map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("malformed response line: {err}"),
                    )
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Addresses and socket paths
// ---------------------------------------------------------------------------

/// A manager address: `host:port` or a literal unix socket path (04 `--remote`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Unix(PathBuf),
    Tcp { host: String, port: u16 },
}

impl Address {
    /// 04: `--remote` accepts `host:port` or a literal unix socket path, one meaning
    /// everywhere. Anything with a `/` in it is a path; otherwise it must be `host:port`.
    pub fn parse(raw: &str) -> Result<Self, ErrorEnvelope> {
        let invalid = || {
            ErrorEnvelope::bad_request(format!(
                "invalid remote address {raw:?}; expected 'host:port' or a literal unix socket path"
            ))
        };
        if raw.is_empty() {
            return Err(invalid());
        }
        if raw.contains('/') {
            return Ok(Self::Unix(PathBuf::from(raw)));
        }
        match raw.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() && !port.is_empty() => {
                let port: u16 = port.parse().map_err(|_| invalid())?;
                Ok(Self::Tcp {
                    host: host.to_string(),
                    port,
                })
            }
            _ => Err(invalid()),
        }
    }

    /// The address as it appears inside the pinned strings (`manager unreachable at {addr}`).
    pub fn display_string(&self) -> String {
        match self {
            Self::Unix(path) => path.display().to_string(),
            Self::Tcp { host, port } => format!("{host}:{port}"),
        }
    }

    /// 07: a non-loopback (and in practice any TCP) manager address requires the instance
    /// token; the unix socket stays tokenless, ever.
    pub fn requires_token(&self) -> bool {
        matches!(self, Self::Tcp { .. })
    }
}

/// The environment the socket-path rule reads, injected so the resolution is testable
/// without touching the process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvView {
    pub xdg_runtime_dir: Option<PathBuf>,
    pub tmpdir: Option<PathBuf>,
    pub uid: u32,
}

impl EnvView {
    /// Read the real process environment (`XDG_RUNTIME_DIR`, `TMPDIR`, the uid).
    pub fn from_process() -> Self {
        Self {
            xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            tmpdir: std::env::var_os("TMPDIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            uid: current_uid(),
        }
    }

    /// 08: `$XDG_RUNTIME_DIR` is usable when it is set, absolute, an existing directory, and
    /// writable by us; otherwise the pinned fallback `$TMPDIR/daiorama-<uid>` applies.
    pub fn usable_runtime_root(&self) -> Option<&Path> {
        let candidate = self.xdg_runtime_dir.as_deref()?;
        if !candidate.is_absolute() {
            return None;
        }
        let metadata = std::fs::metadata(candidate).ok()?;
        if !metadata.is_dir() {
            return None;
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o200 == 0 {
            return None;
        }
        Some(candidate)
    }

    /// The per-instance runtime root: `$XDG_RUNTIME_DIR/daiorama/<instance>/` or
    /// `$TMPDIR/daiorama-<uid>/<instance>/`.
    pub fn runtime_dir(&self, instance: &str) -> PathBuf {
        match self.usable_runtime_root() {
            Some(root) => root.join("daiorama").join(instance),
            None => self
                .tmpdir
                .clone()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(format!("daiorama-{}", self.uid))
                .join(instance),
        }
    }

    /// `.../<instance>/manager.sock` — the path the CLI and the manager must agree on.
    pub fn socket_path(&self, instance: &str) -> PathBuf {
        self.runtime_dir(instance).join("manager.sock")
    }
}

/// Best-effort real uid without pulling in a libc dependency: the real uid from
/// `/proc/self/status`, then the owner of `/proc/self`, then 0.
fn current_uid() -> u32 {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:")
                && let Some(real) = rest.split_whitespace().next()
                && let Ok(uid) = real.parse::<u32>()
            {
                return uid;
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(metadata) = std::fs::metadata("/proc/self") {
            return metadata.uid();
        }
    }
    0
}

/// The instance name every unqualified command targets (04 pinned default).
pub const DEFAULT_INSTANCE: &str = "default";

/// The local socket path for an instance (08), computed identically by CLI and manager.
pub fn socket_path(instance: &str) -> PathBuf {
    EnvView::from_process().socket_path(instance)
}

/// Create the per-instance runtime dir with the pinned 0700 permission model (08) and return
/// it. The in-binary ensure is the only code path that creates it — bare metal and container
/// alike (08).
pub fn ensure_runtime_dir(env: &EnvView, instance: &str) -> io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = env.runtime_dir(instance);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    Ok(dir)
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// How long a connect attempt may take before it is `manager unreachable`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// A connection to a manager, with the handshake already validated by [`Client::handshake`].
pub struct Client {
    reader: BufReader<Box<dyn StreamHandle>>,
    address: String,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

/// Type-erased stream so unix-socket and TCP connections share one client.
trait StreamHandle: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> StreamHandle for T {}

impl Client {
    /// Connect to a manager. Failures are the shared transport presentations: an unreachable
    /// manager, or the token refusal for a token-gated address (04, 07).
    pub async fn connect(
        address: &Address,
        timeout: Duration,
        token: Option<&str>,
    ) -> Result<Self, ErrorEnvelope> {
        let display = address.display_string();
        if address.requires_token() && token.is_none() {
            return Err(ErrorEnvelope::token_required(&display));
        }
        match address {
            Address::Unix(path) => {
                let stream = tokio::time::timeout(timeout, tokio::net::UnixStream::connect(path))
                    .await
                    .map_err(|_| ErrorEnvelope::manager_unreachable(&display))?
                    .map_err(|_| ErrorEnvelope::manager_unreachable(&display))?;
                Ok(Self {
                    reader: BufReader::new(Box::new(stream)),
                    address: display,
                })
            }
            Address::Tcp { host, port } => {
                let stream = tokio::time::timeout(
                    timeout,
                    tokio::net::TcpStream::connect((host.clone(), *port)),
                )
                .await
                .map_err(|_| ErrorEnvelope::manager_unreachable(&display))?
                .map_err(|_| ErrorEnvelope::manager_unreachable(&display))?;
                Ok(Self {
                    reader: BufReader::new(Box::new(stream)),
                    address: display,
                })
            }
        }
    }

    /// Connect to the local instance's socket (04/08 path rule).
    pub async fn connect_local(instance: &str) -> Result<Self, ErrorEnvelope> {
        let env = EnvView::from_process();
        let address = Address::Unix(env.socket_path(instance));
        Self::connect(&address, CONNECT_TIMEOUT, None).await
    }

    /// Is a manager answering at this address? Used for the cold `config set` line and for
    /// the launcher's readiness probe; no RPC is exchanged.
    pub async fn probe(address: &Address, timeout: Duration) -> bool {
        match address {
            Address::Unix(path) => {
                matches!(
                    tokio::time::timeout(timeout, tokio::net::UnixStream::connect(path)).await,
                    Ok(Ok(_))
                )
            }
            Address::Tcp { host, port } => matches!(
                tokio::time::timeout(
                    timeout,
                    tokio::net::TcpStream::connect((host.clone(), *port))
                )
                .await,
                Ok(Ok(_))
            ),
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// The first call on every connection: `handshake`, always, tokenless (05 A7). Applies
    /// the overlap rule locally — only the client can pre-check versions (05) — and surfaces
    /// `version_incompatible` with both pairs in the message (04 string, verbatim).
    pub async fn handshake(&mut self) -> Result<HandshakeResult, ErrorEnvelope> {
        let params = serde_json::to_value(HandshakeParams::ours())
            .map_err(|err| ErrorEnvelope::internal(format!("cannot encode handshake: {err}")))?;
        let raw = self
            .request(methods::HANDSHAKE, Some(params), None)
            .await
            .map_err(|err| match err.code {
                // The handshake's own refusals pass through untouched.
                ErrorCode::VersionIncompatible => err,
                // A manager that cannot do its one pre-auth call at all is unreachable in
                // every way that matters to the caller.
                ErrorCode::Internal => ErrorEnvelope::manager_unreachable(&self.address),
                _ => err,
            })?;
        let result: HandshakeResult = serde_json::from_value(raw)
            .map_err(|err| ErrorEnvelope::internal(format!("malformed handshake result: {err}")))?;
        let client = HandshakeParams::ours()
            .range()
            .map_err(|err| ErrorEnvelope::internal(format!("bad client protocol range: {err}")))?;
        let manager = result
            .range()
            .map_err(|err| ErrorEnvelope::internal(format!("bad manager protocol range: {err}")))?;
        if !ranges_overlap(client, manager) {
            return Err(ErrorEnvelope::version_incompatible(
                &client.0.to_string(),
                &client.1.to_string(),
                &manager.0.to_string(),
                &manager.1.to_string(),
            ));
        }
        Ok(result)
    }

    /// One request/response exchange after the handshake.
    pub async fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
        idempotency_key: Option<String>,
    ) -> Result<Value, ErrorEnvelope> {
        let request = Request::new(method, params).with_idempotency_key(idempotency_key);
        request.validate()?;
        write_frame(&mut self.reader, &request)
            .await
            .map_err(|err| transport_error(&self.address, err))?;
        match read_frame(&mut self.reader)
            .await
            .map_err(|err| transport_error(&self.address, err))?
        {
            Response::Result(value) => Ok(value),
            Response::Error(envelope) => Err(envelope),
        }
    }
}

/// Transport failures after connect are still the shared reachability string when the
/// connection died, and `internal` when the manager answered garbage.
fn transport_error(address: &str, err: io::Error) -> ErrorEnvelope {
    match err.kind() {
        io::ErrorKind::InvalidData => ErrorEnvelope::internal(format!(
            "malformed response from manager at {address}: {err}"
        )),
        _ => ErrorEnvelope::manager_unreachable(address),
    }
}

/// Minimal helper for the read shapes the CLI renders (`status`, `session_status`, ...).
pub fn json_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Build the params object for methods whose params are optional fields only. Absent fields
/// are **omitted** from the wire (the manager sees exactly the arguments the caller gave), and
/// a call with no arguments at all sends `"params": null` — the frame's pinned form.
pub fn params_object(fields: Vec<(&str, Value)>) -> Option<Value> {
    let map: serde_json::Map<String, Value> = fields
        .into_iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The frozen document, read at test time.
    const DOC_05: &str = include_str!("../../docs/05-mcp-bridge.md");

    /// 05 §Errors: the enum is exactly the canonical list — nothing missing, nothing extra.
    #[test]
    fn error_code_enum_is_exhaustive_against_the_frozen_doc() {
        let section = DOC_05
            .split("Canonical codes:")
            .nth(1)
            .expect("05 lists canonical codes");
        let section = section
            .split("`hint` is the corrective step")
            .next()
            .expect("the codes list ends before the hint prose");

        let mut documented: Vec<String> = Vec::new();
        for line in section.lines() {
            if !line.starts_with("- ") {
                continue;
            }
            for token in line.split('`').skip(1).step_by(2) {
                let is_code = !token.is_empty()
                    && token.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                    && token
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
                if is_code {
                    documented.push(token.to_string());
                }
            }
        }
        documented.sort();

        let mut implemented: Vec<String> = ErrorCode::ALL
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        implemented.sort();

        assert_eq!(
            documented, implemented,
            "05 §Errors and ErrorCode drifted apart"
        );
        assert_eq!(documented.len(), 27, "05 lists 27 canonical codes");
    }

    #[test]
    fn codes_round_trip_through_the_wire_form() {
        for code in ErrorCode::ALL {
            let encoded = serde_json::to_string(&code).unwrap();
            assert_eq!(encoded, format!("\"{}\"", code.as_str()));
            let decoded: ErrorCode = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, code);
            assert_eq!(code.to_string(), code.as_str());
        }
        assert!(ErrorCode::from_str("nope").is_err());
    }

    #[test]
    fn error_envelope_shape_is_pinned() {
        let envelope = ErrorEnvelope::session_not_found();
        let encoded = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            encoded,
            json!({
                "code": "session_not_found",
                "message": "session not found",
                "hint": "list sessions to find one",
            })
        );

        // A hint-less envelope omits the field entirely.
        let encoded = serde_json::to_value(ErrorEnvelope::no_manager_running()).unwrap();
        assert_eq!(
            encoded,
            json!({"code": "no_manager_running", "message": "no manager running"})
        );
        assert!(envelope.hint.is_some());
    }

    /// The pinned 04 strings appear inside the envelopes of the codes that carry them.
    #[test]
    fn envelopes_carry_the_pinned_04_strings() {
        assert_eq!(
            ErrorEnvelope::renderer_refused("llvmpipe").message,
            strings::renderer_refused("llvmpipe")
        );
        assert_eq!(
            ErrorEnvelope::limits_unavailable("no cgroup2 fs").message,
            strings::containment_unavailable("no cgroup2 fs")
        );
        assert_eq!(
            ErrorEnvelope::quota_exceeded("operator", 4, 4, 16, 16).message,
            strings::session_limit_reached("operator", 4, 4, 16, 16)
        );
        assert_eq!(
            ErrorEnvelope::version_incompatible("0.1.0", "0.1.0", "0.2.0", "0.2.0").message,
            strings::protocol_mismatch("0.1.0", "0.1.0", "0.2.0", "0.2.0")
        );
        assert_eq!(
            ErrorEnvelope::no_session_specified().message,
            strings::NO_SESSION_SPECIFIED
        );
        assert_eq!(
            ErrorEnvelope::manager_unreachable("/run/x.sock").message,
            "manager unreachable at /run/x.sock"
        );
    }

    #[test]
    fn request_frame_round_trips_with_and_without_optional_fields() {
        let request = Request::new("list_sessions", Some(json!({"limit": 2})));
        let line = serde_json::to_string(&request).unwrap();
        assert_eq!(
            line, r#"{"method":"list_sessions","params":{"limit":2}}"#,
            "idempotency_key is omitted when absent"
        );
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), request);

        // params are always present in the frame, null when the call takes none.
        let bare = Request::new("status", None);
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"method":"status","params":null}"#
        );

        let keyed = Request::new("create_session", Some(json!({})))
            .with_idempotency_key(Some("k-1".into()));
        let line = serde_json::to_string(&keyed).unwrap();
        assert_eq!(
            line,
            r#"{"method":"create_session","params":{},"idempotency_key":"k-1"}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), keyed);
    }

    #[test]
    fn response_frame_round_trips_and_rejects_foreign_shapes() {
        let ok = Response::Result(json!({"id": "s-1"}));
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"result":{"id":"s-1"}}"#
        );
        assert_eq!(
            serde_json::from_str::<Response>(r#"{"result":{"id":"s-1"}}"#).unwrap(),
            ok
        );

        let err = Response::Error(ErrorEnvelope::session_closed());
        let line = serde_json::to_string(&err).unwrap();
        assert_eq!(
            line,
            r#"{"error":{"code":"session_closed","message":"session is closed","hint":"create a new session"}}"#
        );
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), err);
        assert!(err.is_error());

        // Neither result nor error: a protocol violation, not a frame.
        assert!(serde_json::from_str::<Response>(r#"{"weird":true}"#).is_err());
        // An unknown code is a protocol violation too: codes are a versioned surface.
        assert!(
            serde_json::from_str::<Response>(r#"{"error":{"code":"not_a_code","message":"x"}}"#)
                .is_err()
        );
    }

    #[tokio::test]
    async fn frames_are_newline_delimited_and_blank_lines_are_skipped() {
        let (mut client_side, server_side) = tokio::io::duplex(1024);
        write_frame(&mut client_side, &Request::new("status", None))
            .await
            .unwrap();
        write_frame(&mut client_side, &Request::new("readiness", None))
            .await
            .unwrap();
        drop(client_side);

        let mut reader = BufReader::new(server_side);
        let first: Request = serde_json::from_str(&{
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            line
        })
        .unwrap();
        assert_eq!(first.method, "status");

        let mut second = String::new();
        reader.read_line(&mut second).await.unwrap();
        assert_eq!(second.trim(), r#"{"method":"readiness","params":null}"#);
        assert_eq!(reader.read_line(&mut String::new()).await.unwrap(), 0);

        // Server side: blank lines skipped, EOF reported.
        let (mut a, b) = tokio::io::duplex(512);
        a.write_all(b"\n   \n{\"result\":42}\n").await.unwrap();
        drop(a); // without this the reader waits for more data instead of seeing EOF
        let mut reader = BufReader::new(b);
        assert_eq!(
            read_frame(&mut reader).await.unwrap(),
            Response::Result(json!(42))
        );
        assert_eq!(
            read_frame(&mut reader).await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );

        // A non-object line is a protocol error, not a panic.
        let (mut a, b) = tokio::io::duplex(512);
        a.write_all(b"not json\n").await.unwrap();
        let mut reader = BufReader::new(b);
        assert_eq!(
            read_frame(&mut reader).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    /// The read side is byte-capped (P2 hardening): a line past `MAX_FRAME_BYTES` reads as
    /// `TooLarge` instead of buffering — whether it arrives in one chunk or many — the
    /// boundary is exact (a line at the cap itself reads fine), and the EOF semantics are
    /// unchanged (mid-line EOF is a final line).
    #[tokio::test]
    async fn read_side_is_frame_capped() {
        // One chunk, exactly at the cap (the newline is not counted): a line.
        let mut exact = vec![b'x'; MAX_FRAME_BYTES];
        exact.push(b'\n');
        let mut reader = io::Cursor::new(exact);
        assert_eq!(
            read_capped_line(&mut reader).await.unwrap(),
            CappedLine::Line(vec![b'x'; MAX_FRAME_BYTES])
        );

        // One chunk, one byte over: `TooLarge` — the rest of the line is never buffered.
        let mut reader = io::Cursor::new(vec![b'x'; MAX_FRAME_BYTES + 1]);
        assert_eq!(
            read_capped_line(&mut reader).await.unwrap(),
            CappedLine::TooLarge
        );

        // Many chunks (a small duplex buffer forces the split): the same verdict.
        let (mut w, r) = tokio::io::duplex(1024);
        let writer = tokio::spawn(async move {
            let mut payload = vec![b'x'; MAX_FRAME_BYTES + 1];
            payload.push(b'\n');
            let _ = w.write_all(&payload).await;
        });
        let mut reader = BufReader::new(r);
        assert_eq!(
            read_capped_line(&mut reader).await.unwrap(),
            CappedLine::TooLarge
        );
        writer.abort();

        // Clean EOF: `Eof`; EOF mid-line: the bytes so far are a final line.
        let (a, b) = tokio::io::duplex(64);
        drop(a);
        let mut reader = BufReader::new(b);
        assert_eq!(
            read_capped_line(&mut reader).await.unwrap(),
            CappedLine::Eof
        );

        let (mut a, b) = tokio::io::duplex(64);
        a.write_all(b"{\"result\":1}").await.unwrap();
        drop(a);
        let mut reader = BufReader::new(b);
        assert_eq!(
            read_capped_line(&mut reader).await.unwrap(),
            CappedLine::Line(b"{\"result\":1}".to_vec())
        );

        // The client read (`read_frame`) refuses an over-cap response the same way.
        let mut reader = io::Cursor::new(vec![b'x'; MAX_FRAME_BYTES + 1]);
        let err = read_frame(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains(&MAX_FRAME_BYTES.to_string()));
    }

    #[test]
    fn idempotency_key_bounds_are_enforced() {
        let ok = Request::new("create_session", None).with_idempotency_key(Some("a".repeat(64)));
        assert!(ok.validate().is_ok());
        let too_long =
            Request::new("create_session", None).with_idempotency_key(Some("a".repeat(65)));
        assert_eq!(too_long.validate().unwrap_err().code, ErrorCode::BadRequest);
        let empty = Request::new("create_session", None).with_idempotency_key(Some(String::new()));
        assert_eq!(empty.validate().unwrap_err().code, ErrorCode::BadRequest);
        let no_method = Request::new("", None);
        assert_eq!(
            no_method.validate().unwrap_err().code,
            ErrorCode::BadRequest
        );
    }

    #[test]
    fn handshake_ranges_follow_the_overlap_rule_and_its_edge_cases() {
        let v010 = ProtocolVersion::new(0, 1, 0);
        let v020 = ProtocolVersion::new(0, 2, 0);
        let v030 = ProtocolVersion::new(0, 3, 0);

        // Identical ranges: overlap.
        assert!(ranges_overlap((v010, v010), (v010, v010)));
        // Disjoint ranges: refuse (this is the fixture's mismatch case).
        assert!(!ranges_overlap((v010, v010), (v020, v020)));
        // Overlapping windows: allow.
        assert!(ranges_overlap((v030, v010), (v020, v010)));
        // Touch exactly at the boundary: allow.
        assert!(ranges_overlap((v020, v010), (v020, v010)));
        // Inverted client range (min above version): refuse.
        assert!(!ranges_overlap((v010, v020), (v010, v010)));
        // Inverted manager range: refuse.
        assert!(!ranges_overlap((v010, v010), (v010, v020)));
        // Empty (zero-width) ranges on both sides, equal: allow; offset: refuse.
        assert!(ranges_overlap((v020, v020), (v020, v020)));
        assert!(!ranges_overlap((v020, v020), (v030, v030)));
    }

    #[test]
    fn omitted_min_supported_defaults_to_the_protocol_version() {
        let params: HandshakeParams =
            serde_json::from_value(json!({"protocol_version": "0.2.0"})).unwrap();
        assert_eq!(
            params.range().unwrap(),
            (ProtocolVersion::new(0, 2, 0), ProtocolVersion::new(0, 2, 0))
        );

        let result: HandshakeResult = serde_json::from_value(
            json!({"protocol_version": "0.2.0", "instance_id": "11111111-1111-4111-8111-111111111111"}),
        )
        .unwrap();
        assert_eq!(
            result.range().unwrap(),
            (ProtocolVersion::new(0, 2, 0), ProtocolVersion::new(0, 2, 0))
        );
        // A malformed version is a protocol error, surfaced as `internal` by the client.
        assert!(result.range().is_ok());
        let broken = HandshakeResult {
            protocol_version: "nonsense".into(),
            min_supported: None,
            instance_id: "x".into(),
        };
        assert!(broken.range().is_err());

        // Ours always sends both halves explicitly.
        let ours = HandshakeParams::ours();
        assert_eq!(ours.protocol_version, PROTOCOL_VERSION);
        assert_eq!(ours.min_supported.as_deref(), Some(MIN_SUPPORTED));
    }

    #[test]
    fn protocol_versions_parse_strictly() {
        assert_eq!(
            "0.1.0".parse::<ProtocolVersion>().unwrap(),
            ProtocolVersion::new(0, 1, 0)
        );
        assert_eq!(
            "12.34.56".parse::<ProtocolVersion>().unwrap().to_string(),
            "12.34.56"
        );
        for bad in ["", "1", "1.2", "1.2.3.4", "1.2.x", "v1.2.3", "-1.2.3"] {
            assert!(
                bad.parse::<ProtocolVersion>().is_err(),
                "{bad:?} must not parse"
            );
        }
    }

    #[test]
    fn addresses_parse_per_the_one_flag_one_meaning_rule() {
        assert_eq!(
            Address::parse("/run/user/1000/daiorama/default/manager.sock").unwrap(),
            Address::Unix(PathBuf::from(
                "/run/user/1000/daiorama/default/manager.sock"
            ))
        );
        assert_eq!(
            Address::parse("127.0.0.1:8756").unwrap(),
            Address::Tcp {
                host: "127.0.0.1".into(),
                port: 8756
            }
        );
        assert_eq!(
            Address::parse("manager.example:8756").unwrap(),
            Address::Tcp {
                host: "manager.example".into(),
                port: 8756
            }
        );
        assert_eq!(
            Address::parse("127.0.0.1:8756").unwrap().display_string(),
            "127.0.0.1:8756"
        );
        assert!(Address::parse("").is_err());
        assert!(Address::parse("not-an-address").is_err());
        assert!(Address::parse("127.0.0.1:99999").is_err());
        assert!(Address::parse(":8756").is_err());

        assert!(Address::parse("127.0.0.1:8756").unwrap().requires_token());
        assert!(!Address::parse("/tmp/x.sock").unwrap().requires_token());
    }

    #[test]
    fn socket_paths_follow_08_including_the_fallback() {
        let env = EnvView {
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
            tmpdir: Some(PathBuf::from("/tmp")),
            uid: 1000,
        };
        // The fixture harness creates a real directory; the check reads the filesystem, so
        // point at one that exists here (the crate root always does).
        let real = EnvView {
            xdg_runtime_dir: Some(std::env::temp_dir()),
            ..env.clone()
        };
        assert_eq!(
            real.socket_path("default"),
            std::env::temp_dir().join("daiorama/default/manager.sock")
        );

        // Unset runtime dir: the pinned fallback.
        let fallback = EnvView {
            xdg_runtime_dir: None,
            tmpdir: Some(PathBuf::from("/scratch")),
            uid: 4242,
        };
        assert_eq!(
            fallback.socket_path("beta"),
            PathBuf::from("/scratch/daiorama-4242/beta/manager.sock")
        );

        // Set but nonexistent: unusable, so the fallback applies.
        let missing = EnvView {
            xdg_runtime_dir: Some(PathBuf::from("/nonexistent/run/user/1000")),
            tmpdir: Some(PathBuf::from("/scratch")),
            uid: 7,
        };
        assert_eq!(
            missing.socket_path("default"),
            PathBuf::from("/scratch/daiorama-7/default/manager.sock")
        );

        // Set but relative: unusable.
        let relative = EnvView {
            xdg_runtime_dir: Some(PathBuf::from("run/user/1000")),
            tmpdir: None,
            uid: 0,
        };
        assert_eq!(
            relative.socket_path("default"),
            PathBuf::from("/tmp/daiorama-0/default/manager.sock"),
            "TMPDIR unset falls back to /tmp"
        );

        // A file where a directory is expected is unusable.
        let not_a_dir = EnvView {
            xdg_runtime_dir: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")),
            tmpdir: Some(PathBuf::from("/scratch")),
            uid: 3,
        };
        assert!(not_a_dir.usable_runtime_root().is_none());
    }

    #[test]
    fn ensure_runtime_dir_creates_0700_directories() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let env = EnvView {
            xdg_runtime_dir: Some(root.path().to_path_buf()),
            tmpdir: None,
            uid: 1000,
        };
        let dir = ensure_runtime_dir(&env, "alpha").unwrap();
        assert!(dir.is_dir());
        assert_eq!(dir, root.path().join("daiorama/alpha"));
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "08: 0700 dirs, same permission model everywhere"
        );
        // Idempotent.
        assert_eq!(ensure_runtime_dir(&env, "alpha").unwrap(), dir);
    }

    #[tokio::test]
    async fn unreachable_addresses_present_the_shared_string() {
        let missing = Address::Unix(PathBuf::from("/nonexistent/definitely/not/a/socket.sock"));
        let err = Client::connect(&missing, Duration::from_millis(250), None)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ManagerUnreachable);
        assert_eq!(
            err.message,
            "manager unreachable at /nonexistent/definitely/not/a/socket.sock"
        );
    }

    #[tokio::test]
    async fn tcp_addresses_require_the_token_before_dialing() {
        let tcp = Address::parse("203.0.113.9:8756").unwrap();
        let err = Client::connect(&tcp, Duration::from_millis(50), None)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::TokenRequired);
        assert_eq!(
            err.message,
            "token required for 203.0.113.9:8756: set DAIORAMA_TOKEN (value from 'daiorama token show' on the manager host)."
        );

        // With a token we dial for real and fail as unreachable (nothing listens there).
        let err = Client::connect(&tcp, Duration::from_millis(250), Some("secret"))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ManagerUnreachable);
        assert_eq!(err.message, "manager unreachable at 203.0.113.9:8756");
    }
}
