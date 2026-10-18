//! Manager: the server skeleton — listener, handshake, registry types, state enums.
//!
//! **Phase 1 scope.** This module carries the contract shapes the runtime half
//! (W2) builds against: the request listener and its handshake gate, the session/view
//! registry types and state machine semantics from `docs/06-sessions-views.md`, the instance
//! status shape from 05, the startup containment decision, and **trait stubs** for everything
//! that needs a real machine:
//!
//! - [`SessionSpawner`] — compositor/session spawning (forbidden here: no compositor, no
//!   cgroups, no wayvnc).
//! - [`ViewOps`] — view start/stop (wayvnc + manager-owned front).
//! - [`startup::ContainmentProbe`] — the cgroup v2 delegation precondition.
//!
//! Those traits have no production implementation in this layer, and none of this is wired
//! into `daiorama serve` — that is the runtime side's dispatcher (`runtime::service`); the
//! skeleton below remains the contract layer's testable handler, and the CLI's `mcp`/
//! `dashboard` subcommands answer the not-implemented envelope until their slices land. The
//! listener here exists so the frame, the handshake gate and the read shapes can be exercised
//! end to end in tests.
//!
//! Note on lints: this crate is the P2 phase-1 **contract layer** — the registry transitions,
//! the state enums, the instance-status builders and the trait stubs are driven by the runtime
//! half (W2) rather than by today's binary, and every one of them is exercised by the unit
//! tests below until then. `dead_code` is allowed module-wide with that reason in mind; it goes
//! away as the consumers land.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener};

use crate::config::{CoreConfig, Toggle};
use crate::ipc::{
    CappedLine, ErrorCode, ErrorEnvelope, HandshakeParams, HandshakeResult, MAX_FRAME_BYTES,
    MIN_SUPPORTED, PROTOCOL_VERSION, Request, Response, methods, ranges_overlap, read_capped_line,
};
use crate::media::{MediaHandle, MediaStore};
use crate::strings;

// ---------------------------------------------------------------------------
// State enums (06)
// ---------------------------------------------------------------------------

/// 06 B3: `running → degraded → dead → closed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// Compositor alive and healthy (IPC socket answers a trivial query).
    Running,
    /// Health checks failing, the kill path is engaged. Transient; never a resting state.
    Degraded,
    /// The tree has been killed via `cgroup.kill`, resources freed. Listable until closed.
    Dead,
    /// Registry entry removed. Final.
    Closed,
}

impl SessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Degraded => "degraded",
            Self::Dead => "dead",
            Self::Closed => "closed",
        }
    }

    /// Alive states hold live resources and count against quotas; `dead` does not (06).
    pub fn is_live(self) -> bool {
        matches!(self, Self::Running | Self::Degraded)
    }
}

/// 06 view registry: `state: starting | running | down`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewState {
    Starting,
    Running,
    Down,
}

impl ViewState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Down => "down",
        }
    }
}

/// Instance state as it appears in the status shape (05). Only reachable while the manager is
/// answering, hence two values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceState {
    Running,
    Draining,
}

impl InstanceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Draining => "draining",
        }
    }
}

/// `mode` in the instance status shape (04: attached/background).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceMode {
    Attached,
    Background,
}

impl InstanceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::Background => "background",
        }
    }
}

/// `--backend` literals (04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Headless,
    Nested,
    Host,
}

impl Backend {
    pub const LITERALS: [&'static str; 3] = ["headless", "nested", "host"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::Nested => "nested",
            Self::Host => "host",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ErrorEnvelope> {
        match raw {
            "headless" => Ok(Self::Headless),
            "nested" => Ok(Self::Nested),
            "host" => Ok(Self::Host),
            other => Err(ErrorEnvelope::bad_request(format!(
                "invalid backend {other:?}; expected one of: {}",
                Self::LITERALS.join(", ")
            ))),
        }
    }
}

/// History origin (06 B8): `shell` | `mcp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Shell,
    Mcp,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Mcp => "mcp",
        }
    }

    /// The frame's `origin` values (06 B8); anything else is a malformed request.
    pub fn parse(raw: &str) -> Result<Self, ErrorEnvelope> {
        match raw {
            "shell" => Ok(Self::Shell),
            "mcp" => Ok(Self::Mcp),
            other => Err(ErrorEnvelope::bad_request(strings::invalid_origin(other))),
        }
    }
}

/// The renderer mark a session record carries (06 D6): never silently `verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RendererMark {
    Verified,
    Software,
    Unverified,
}

impl RendererMark {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Software => "software",
            Self::Unverified => "unverified",
        }
    }
}

/// Why a session left `running` — history entries carry it where relevant (06 B8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cause {
    /// The idle reaper: a reap is not a close (06 B1).
    Reaped,
    /// Health failure or crash: the kill path ran (06 crash policy).
    Dead,
    /// Operator/agent teardown.
    Closed,
}

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reaped => "reaped",
            Self::Dead => "dead",
            Self::Closed => "closed",
        }
    }
}

// ---------------------------------------------------------------------------
// Creation parameters (04, 06)
// ---------------------------------------------------------------------------

/// `--resolution WxH` (04 pinned default `1920x1080`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const DEFAULT: Self = Self {
        width: 1920,
        height: 1080,
    };

    pub fn parse(raw: &str) -> Result<Self, ErrorEnvelope> {
        let invalid = || {
            ErrorEnvelope::bad_request(format!(
                "invalid resolution {raw:?}; expected WxH (e.g. 1920x1080)"
            ))
        };
        let (width, height) = raw.split_once('x').ok_or_else(invalid)?;
        let width: u32 = width.parse().map_err(|_| invalid())?;
        let height: u32 = height.parse().map_err(|_| invalid())?;
        if width == 0 || height == 0 {
            return Err(invalid());
        }
        Ok(Self { width, height })
    }

    pub fn as_str(self) -> String {
        format!("{}x{}", self.width, self.height)
    }
}

/// 04 pinned scale default.
pub const DEFAULT_SCALE: f64 = 1.0;

/// Everything `create_session` carries (04 `new`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSpec {
    pub backend: Backend,
    pub resolution: String,
    pub scale: f64,
    pub view: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Default for SessionSpec {
    fn default() -> Self {
        Self {
            backend: Backend::Headless,
            resolution: Resolution::DEFAULT.as_str(),
            scale: DEFAULT_SCALE,
            view: false,
            label: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Registry types and the state machine
// ---------------------------------------------------------------------------

/// 06 §Views registry: `{state, address, front_port, vnc_port, generation}`. Views are
/// first-class registry entries; front ports allocate upward from `view.port_base`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewRecord {
    pub state: ViewState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub front_port: u16,
    pub vnc_port: u16,
    pub generation: u64,
}

/// 05 pinned session object:
/// `{id, label, state, backend, resolution, scale, scope, origin, idle_since, view, instance_id, renderer}`.
///
/// `cause` is deliberately **not** part of the serialized shape (05's session object has no
/// such key; the cause belongs to history records) — it is in-memory state-machine data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub label: String,
    pub state: SessionState,
    pub backend: Backend,
    pub resolution: String,
    pub scale: f64,
    pub scope: String,
    pub origin: Origin,
    pub idle_since: String,
    pub view: Option<ViewRecord>,
    pub instance_id: String,
    pub renderer: RendererMark,
    #[serde(skip)]
    pub cause: Option<Cause>,
    /// Monotonic view generation (06 B2): increments on every successful view start and is
    /// kept across stops, so an address cached before a stop/death is detectable as stale.
    #[serde(skip)]
    pub view_generation: u64,
}

/// 05 instance status: `{state, mode, pid, socket, versions, sessions, drain, instance_id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceStatus {
    pub state: InstanceState,
    pub mode: InstanceMode,
    pub pid: u32,
    pub socket: String,
    pub versions: VersionInfo,
    pub sessions: SessionCounts,
    pub drain: DrainState,
    pub instance_id: String,
}

/// `versions: {binary, protocol, min_supported, dashboard_module?}` — the `readiness` call is
/// the same field set plus `dashboard_module` (05).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub binary: String,
    pub protocol: String,
    pub min_supported: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dashboard_module: Option<String>,
}

impl VersionInfo {
    pub fn current() -> Self {
        Self {
            binary: crate::ipc::BINARY_VERSION.to_string(),
            protocol: PROTOCOL_VERSION.to_string(),
            min_supported: MIN_SUPPORTED.to_string(),
            dashboard_module: None,
        }
    }
}

/// `sessions: {running, dead}` — alive/dead shown separately (06: dead sessions hold no live
/// resources and do not count against `max_sessions` or the global ceiling).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCounts {
    pub running: u32,
    pub dead: u32,
}

/// `drain: {active, remaining}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainState {
    pub active: bool,
    pub remaining: u32,
}

/// 06 §Tombstones: closed/reaped IDs are remembered for 24 h (bounded; cleared on manager
/// restart). Calls inside the window get the distinct code; outside it, `session_not_found`.
pub const TOMBSTONE_TTL: StdDuration = StdDuration::from_secs(24 * 60 * 60);

/// Bound on concurrent TCP manager connections (07): the bridge's 64, applied to the other
/// network listener. Over-cap sockets are closed on accept; the unix socket is unaffected.
const MANAGER_MAX_CONNECTIONS: usize = 64;
/// A TCP connection must make progress within this bound — handshake, frame or media head —
/// or it closes. The unix socket keeps no deadline (local, credential-free).
const MANAGER_PROGRESS: StdDuration = StdDuration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TombstoneKind {
    Closed,
    Reaped,
}

#[derive(Debug, Clone, Copy)]
struct Tombstone {
    kind: TombstoneKind,
    at: Instant,
}

/// The session registry: the single source of truth for what exists (03). Phase 1 carries the
/// types and the state-machine transitions; the runtime side drives them.
#[derive(Debug)]
pub struct Registry {
    pub instance_id: String,
    sessions: BTreeMap<String, SessionRecord>,
    tombstones: BTreeMap<String, Tombstone>,
}

impl Registry {
    pub fn new(instance_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            sessions: BTreeMap::new(),
            tombstones: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, record: SessionRecord) {
        self.sessions.insert(record.id.clone(), record);
    }

    pub fn get(&self, id: &str) -> Option<&SessionRecord> {
        self.sessions.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut SessionRecord> {
        self.sessions.get_mut(id)
    }

    /// The session's view generation (06 B2); 0 before any view start.
    pub fn view_generation(&self, id: &str) -> u64 {
        self.sessions
            .get(id)
            .map(|record| record.view_generation)
            .unwrap_or(0)
    }

    /// Record a view transition: view state is registry state (06 B2).
    pub fn set_view(&mut self, id: &str, view: Option<ViewRecord>, generation: u64) {
        if let Some(record) = self.sessions.get_mut(id) {
            record.view = view;
            record.view_generation = generation;
        }
    }

    /// Insertion-ordered session list (BTreeMap key order: stable across runs).
    pub fn list(&self) -> Vec<&SessionRecord> {
        self.sessions.values().collect()
    }

    /// Alive/dead counts as the status shape reports them.
    pub fn counts(&self) -> SessionCounts {
        let mut counts = SessionCounts::default();
        for record in self.sessions.values() {
            if record.state == SessionState::Dead {
                counts.dead += 1;
            } else if record.state.is_live() {
                counts.running += 1;
            }
        }
        counts
    }

    /// Sessions counting against quotas: alive only — a dead session holds no live resources
    /// and does **not** count (06 §Quota accounting).
    pub fn live_count(&self) -> u32 {
        self.sessions
            .values()
            .filter(|record| record.state.is_live())
            .count() as u32
    }

    /// The gate every session-scoped call passes through: the session record, or the code the
    /// call must answer with.
    pub fn call_gate(&self, id: &str) -> Result<&SessionRecord, ErrorEnvelope> {
        match self.sessions.get(id) {
            Some(record) => match record.state {
                SessionState::Dead => Err(match record.cause() {
                    Some(Cause::Reaped) => ErrorEnvelope::session_reaped(),
                    _ => ErrorEnvelope::session_dead(),
                }),
                SessionState::Closed => Err(ErrorEnvelope::session_closed()),
                SessionState::Running | SessionState::Degraded => Ok(record),
            },
            None => Err(self.tombstone_error(id)),
        }
    }

    /// The code for an ID that is not in the registry.
    pub fn tombstone_error(&self, id: &str) -> ErrorEnvelope {
        match self.tombstones.get(id).map(|tombstone| tombstone.kind) {
            Some(TombstoneKind::Closed) => ErrorEnvelope::session_closed(),
            Some(TombstoneKind::Reaped) => ErrorEnvelope::session_reaped(),
            None => ErrorEnvelope::session_not_found(),
        }
    }

    /// Explicit close: removes the entry, final (06). A dead session may be closed too —
    /// "an operator or agent may close one immediately". Closing a closed ID is
    /// `session_closed`, never a silent success.
    pub fn close(&mut self, id: &str) -> Result<SessionRecord, ErrorEnvelope> {
        match self.sessions.get(id).map(|record| record.state) {
            Some(state) if state != SessionState::Closed => {
                let record = self.sessions.remove(id).expect("checked above");
                self.tombstones.insert(
                    id.to_string(),
                    Tombstone {
                        kind: TombstoneKind::Closed,
                        at: Instant::now(),
                    },
                );
                Ok(record)
            }
            Some(_) => Err(ErrorEnvelope::session_closed()),
            None => Err(self.tombstone_error(id)),
        }
    }

    /// One reaper pass over dead entries (06: the dead-session sweep closes them; a reaped
    /// entry keeps its `reaped` tombstone so callers still see the distinct code).
    pub fn sweep_dead(&mut self) -> Vec<SessionRecord> {
        let dead: Vec<String> = self
            .sessions
            .values()
            .filter(|record| record.state == SessionState::Dead)
            .map(|record| record.id.clone())
            .collect();
        let mut swept = Vec::new();
        for id in dead {
            let record = self.sessions.remove(&id).expect("just collected");
            let kind = match record.cause() {
                Some(Cause::Reaped) => TombstoneKind::Reaped,
                _ => TombstoneKind::Closed,
            };
            self.tombstones.insert(
                id,
                Tombstone {
                    kind,
                    at: Instant::now(),
                },
            );
            swept.push(record);
        }
        swept
    }

    /// A reap: the session ends with cause `reaped` (06 B1). It is not a close — the entry
    /// stays listable until the sweep (or an operator close) removes it.
    pub fn reap(&mut self, id: &str) -> Result<(), ErrorEnvelope> {
        match self.sessions.get_mut(id) {
            Some(record) => match record.state {
                SessionState::Dead => Err(ErrorEnvelope::session_reaped()),
                SessionState::Closed => Err(ErrorEnvelope::session_closed()),
                SessionState::Running | SessionState::Degraded => {
                    record.state = SessionState::Dead;
                    record.cause = Some(Cause::Reaped);
                    record.view = None;
                    Ok(())
                }
            },
            None => Err(self.tombstone_error(id)),
        }
    }

    /// The browser-side half of D6 (06): a live session whose browser reports a software
    /// renderer moves to `software`, one-way. Returns whether the mark changed, so the caller
    /// records `renderer.software` exactly once.
    pub fn mark_software(&mut self, id: &str) -> bool {
        match self.sessions.get_mut(id) {
            Some(record) if record.renderer != RendererMark::Software => {
                record.renderer = RendererMark::Software;
                true
            }
            _ => false,
        }
    }

    /// Health failure or crash: mark dead, no auto-restart (06 §Crash policy).
    pub fn mark_dead(&mut self, id: &str) -> Result<(), ErrorEnvelope> {
        match self.sessions.get_mut(id) {
            Some(record) => match record.state {
                SessionState::Dead => Err(ErrorEnvelope::session_dead()),
                SessionState::Closed => Err(ErrorEnvelope::session_closed()),
                SessionState::Running | SessionState::Degraded => {
                    record.state = SessionState::Dead;
                    record.cause = Some(Cause::Dead);
                    record.view = None;
                    Ok(())
                }
            },
            None => Err(self.tombstone_error(id)),
        }
    }

    /// Drop tombstones past their 24 h window (bounded memory; cleared on restart anyway).
    pub fn forget_expired_tombstones(&mut self, now: Instant) -> usize {
        let expired: Vec<String> = self
            .tombstones
            .iter()
            .filter(|(_, tombstone)| now.duration_since(tombstone.at) >= TOMBSTONE_TTL)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.tombstones.remove(id);
        }
        expired.len()
    }

    /// Quota check for a create: scope-local and global, with the pinned 04 message.
    pub fn quota_check(
        &self,
        scope: &str,
        scope_live: u32,
        scope_max: u32,
        global_max: u32,
    ) -> Result<(), ErrorEnvelope> {
        let global_live = self.live_count();
        if scope_live >= scope_max || global_live >= global_max {
            return Err(ErrorEnvelope::quota_exceeded(
                scope,
                scope_live,
                scope_max,
                global_live,
                global_max,
            ));
        }
        Ok(())
    }
}

impl SessionRecord {
    /// The record's end cause, when it has one.
    pub fn cause(&self) -> Option<Cause> {
        self.cause
    }

    /// 05 shape, exactly: no extra keys.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("session records always serialize")
    }
}

// ---------------------------------------------------------------------------
// Startup: containment classes (06 C6)
// ---------------------------------------------------------------------------

/// The startup containment decision (06 C6). The check runs at manager startup, after the
/// runtime lock, before the manager binds anything; a failure fails the start with
/// `limits_unavailable` and every lazy-start racing the winner surfaces the same error.
pub mod startup {
    use super::*;

    /// What the machine reports about cgroup v2 delegation.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Delegation {
        /// Human detail for diagnostics (e.g. which controllers are delegated).
        pub detail: String,
    }

    /// The precondition probe. **Trait stub**: the production sysfs implementation lands with
    /// the runtime side (W2); this layer ships the decision logic and test doubles.
    pub trait ContainmentProbe {
        fn probe(&self) -> Result<Delegation, String>;
    }

    /// A probe that reports delegation present.
    #[derive(Debug, Clone, Default)]
    pub struct DelegatedProbe {
        pub detail: String,
    }

    impl ContainmentProbe for DelegatedProbe {
        fn probe(&self) -> Result<Delegation, String> {
            Ok(Delegation {
                detail: if self.detail.is_empty() {
                    "memory, cpu and tasks delegated".to_string()
                } else {
                    self.detail.clone()
                },
            })
        }
    }

    /// A probe that reports delegation absent, with a reason.
    #[derive(Debug, Clone)]
    pub struct UnavailableProbe {
        pub detail: String,
    }

    impl UnavailableProbe {
        pub fn new(detail: impl Into<String>) -> Self {
            Self {
                detail: detail.into(),
            }
        }
    }

    impl ContainmentProbe for UnavailableProbe {
        fn probe(&self) -> Result<Delegation, String> {
            Err(self.detail.clone())
        }
    }

    /// What the manager will do about per-session limits this run.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum LimitsPlan {
        /// Containment on: per-session cgroups in use, startup sweep runs.
        Enabled { detail: String },
        /// `limits.mode = off`: the explicit, recorded downgrade (06) — no per-session
        /// limits, no crash-time `cgroup.kill`, and **no sweep** (the sweep operates on the
        /// instance's session-cgroup subtree, which exists only while cgroups are in use).
        Disabled,
    }

    /// The startup plan.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StartupPlan {
        pub limits: LimitsPlan,
        pub sweep: bool,
    }

    /// Proceed, or fail the start with the pinned refusal.
    #[derive(Debug, Clone, PartialEq)]
    pub enum StartupDecision {
        Proceed(StartupPlan),
        Refuse(ErrorEnvelope),
    }

    /// The whole containment decision, in one place: `limits.mode = off` proceeds with limits
    /// disabled and no sweep; `on` requires the probe and fails the start with
    /// `limits_unavailable` when delegation is missing.
    pub fn plan_startup(config: &CoreConfig, probe: &dyn ContainmentProbe) -> StartupDecision {
        match config.limits.mode {
            Toggle::Off => StartupDecision::Proceed(StartupPlan {
                limits: LimitsPlan::Disabled,
                sweep: false,
            }),
            Toggle::On => match probe.probe() {
                Ok(delegation) => StartupDecision::Proceed(StartupPlan {
                    limits: LimitsPlan::Enabled {
                        detail: delegation.detail,
                    },
                    sweep: true,
                }),
                Err(detail) => StartupDecision::Refuse(ErrorEnvelope::limits_unavailable(&detail)),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Trait stubs for the runtime half (no implementation in this layer)
// ---------------------------------------------------------------------------

/// What a successful spawn reports back (06 D6): the new session's id and the renderer mark
/// detected at creation — `verified` for a detected GPU renderer, `software` when a software
/// renderer was detected and the caller allowed it, `unverified` when the active renderer
/// could not be detected. Never silently `verified`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnOutcome {
    pub id: String,
    pub renderer: RendererMark,
}

/// The Wayland environment one live session's capture command runs under (05 §Capture): the
/// compositor's own runtime dir and socket, never the manager's. Built by the spawner, which
/// is the only layer that knows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureEnv {
    pub rt_dir: std::path::PathBuf,
    pub wayland_display: String,
    pub signature: String,
}

/// Session spawning, containment, and teardown. **Trait stub only** — the runtime side (W2)
/// implements this against a real compositor and real cgroups; this layer must never spawn a
/// compositor, touch cgroups or hold a wayvnc socket.
pub trait SessionSpawner: Send + Sync {
    /// Start a session from its creation parameters. The renderer policy (06 D6) is enforced
    /// here: a detected software renderer refuses with `renderer_refused` unless
    /// `allow_software` is set — the caller's live `renderer.allow_software` (08: reloadable,
    /// re-read per call). Returns the id and the creation-time renderer mark for the record.
    fn spawn(
        &self,
        spec: &SessionSpec,
        allow_software: bool,
    ) -> Result<SpawnOutcome, ErrorEnvelope>;

    /// Kill the session's process tree through its cgroup (the completion guarantee).
    fn kill_tree(&self, session_id: &str) -> Result<(), ErrorEnvelope>;

    /// The live session's capture environment; `None` when the spawner holds no running
    /// session for the id (a dead or closed session has nothing to capture).
    fn capture_env(&self, session_id: &str) -> Option<CaptureEnv>;
}

/// View operations: wayvnc plus the manager-owned front. **Trait stub only** — no wayvnc
/// here, ever.
pub trait ViewOps: Send + Sync {
    fn start_view(&self, session_id: &str) -> Result<ViewRecord, ErrorEnvelope>;
    fn stop_view(&self, session_id: &str) -> Result<(), ErrorEnvelope>;
}

// ---------------------------------------------------------------------------
// Listener skeleton
// ---------------------------------------------------------------------------

/// A handler for connections the skeleton has already handshake-gated.
pub trait Handler: Send + Sync {
    /// 05 A7: `handshake` is always the first call on every connection, tokenless, and
    /// authorizes nothing. The skeleton answers it itself.
    fn handshake_result(&self) -> HandshakeResult;

    /// Everything after the handshake. `scope` is the request's application-layer identity
    /// and `origin` where it came from (05, 06 B8): the frame's fields, or `operator` /
    /// `shell` when absent.
    fn dispatch(
        &self,
        scope: &str,
        origin: Origin,
        method: &str,
        params: Option<&Value>,
    ) -> Response;

    /// 07: the instance-token transport gate for a network surface. Only the runtime service
    /// holds the token this start adopted; the default refuses, so a handler with no
    /// credential source can never serve a TCP caller by omission.
    fn authorize_instance(
        &self,
        _token: Option<&str>,
        authority: &str,
    ) -> Result<(), ErrorEnvelope> {
        Err(ErrorEnvelope::token_required(authority))
    }

    /// 07: per-request scope authentication after the transport gate. A claimed string is
    /// never authentication, so the scope's secret is checked on every call. The default
    /// refuses: a handler with no scope files cannot authenticate a network caller.
    fn authorize_scope(
        &self,
        token: Option<&str>,
        _scope: Option<&str>,
        _scope_token: Option<&str>,
        authority: &str,
    ) -> Result<(), ErrorEnvelope> {
        self.authorize_instance(token, authority)?;
        Err(ErrorEnvelope::new(
            crate::ipc::ErrorCode::ScopeAuthRequired,
            strings::FRAME_SCOPE_REQUIRED,
        ))
    }
}

/// The reserved scope every CLI call runs as (04, 07); a request without a `scope` field is
/// the CLI's local path and carries it. Sessions created under it are operator-wide.
pub use crate::ipc::OPERATOR_SCOPE;

/// The skeleton handler: serves the read shapes that need no runtime (`status`, `readiness`)
/// and answers the not-implemented envelope for everything else — for it, every non-read
/// method genuinely is not implemented. The runtime side's dispatcher replaces this in
/// `serve`.
pub struct SkeletonHandler {
    pub status: InstanceStatus,
    /// `status`'s field set plus `dashboard_module` (05): the thing operators and the
    /// dashboard watch.
    pub dashboard_module: Option<String>,
}

impl Handler for SkeletonHandler {
    fn handshake_result(&self) -> HandshakeResult {
        HandshakeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            min_supported: Some(MIN_SUPPORTED.to_string()),
            instance_id: self.status.instance_id.clone(),
        }
    }

    fn dispatch(
        &self,
        _scope: &str,
        _origin: Origin,
        method: &str,
        _params: Option<&Value>,
    ) -> Response {
        match method {
            methods::STATUS => Response::Result(
                serde_json::to_value(&self.status).expect("instance status serializes"),
            ),
            methods::READINESS => {
                let mut status = self.status.clone();
                status.versions.dashboard_module = self.dashboard_module.clone();
                Response::Result(serde_json::to_value(&status).expect("instance status serializes"))
            }
            _ => Response::Error(ErrorEnvelope::not_implemented()),
        }
    }
}

/// The request listener: one connection at a time, handshake first, then dispatch.
pub struct Listener {
    listener: UnixListener,
    path: PathBuf,
}

/// How a connection reached the manager (07). The unix socket is filesystem-gated and
/// tokenless, ever; TCP is a network surface, so every post-handshake frame carries the
/// instance token and its scope's secret.
#[derive(Clone, Debug)]
enum Transport {
    Unix,
    Tcp { authority: String },
}

impl Listener {
    /// Bind the manager socket. The parent directory is ensured with the pinned 0700 model
    /// (08); a socket already in use is *not* stolen — that belongs to a live manager.
    pub async fn bind(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        let listener = UnixListener::bind(&path)?;
        Ok(Self { listener, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept connections until the process ends. Each connection: read a frame, require it
    /// to be the handshake, then serve dispatches until the peer goes away.
    pub async fn serve(self, handler: Arc<dyn Handler>, media: Arc<MediaStore>) {
        loop {
            let Ok((stream, _)) = self.listener.accept().await else {
                continue;
            };
            let handler = Arc::clone(&handler);
            let media = Arc::clone(&media);
            tokio::spawn(async move {
                let _ = serve_connection(stream, handler, media, Transport::Unix, None).await;
            });
        }
    }
}

/// The `manager.bind` listener (07): the same frame protocol on TCP, with the credential gate
/// that makes a network surface safe. Bound in addition to the unix socket — never instead.
pub struct NetworkListener {
    listener: TcpListener,
    authority: String,
}

impl NetworkListener {
    /// Bind an already-parsed `manager.bind` authority. The bound address is read back (a
    /// `:0` port resolves to the real one), and that string is what refusals name.
    pub async fn bind(host: &str, port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind((host, port)).await?;
        let authority = listener.local_addr()?.to_string();
        Ok(Self {
            listener,
            authority,
        })
    }

    pub fn authority(&self) -> &str {
        &self.authority
    }

    pub async fn serve(self, handler: Arc<dyn Handler>, media: Arc<MediaStore>) {
        // 07: the TCP surface is reachable before any credential check, so its own bounds run
        // before the gate: a connection cap and a per-read progress deadline.
        let permits = Arc::new(tokio::sync::Semaphore::new(MANAGER_MAX_CONNECTIONS));
        loop {
            let Ok((stream, _)) = self.listener.accept().await else {
                continue;
            };
            let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let handler = Arc::clone(&handler);
            let media = Arc::clone(&media);
            let transport = Transport::Tcp {
                authority: self.authority.clone(),
            };
            tokio::spawn(async move {
                let _permit = permit;
                let _ = serve_connection(stream, handler, media, transport, Some(MANAGER_PROGRESS))
                    .await;
            });
        }
    }
}

/// One connection's lifecycle. `handshake` first, always: anything else is a malformed
/// request (`bad_request`), and the connection ends; a second `handshake` on the same
/// connection is refused the same way. A *refused* handshake ends the connection the same
/// way (05 A7: out-of-range ranges are rejected at connect — nothing follows a refusal).
/// An *over-cap* frame ([`MAX_FRAME_BYTES`]) is refused the same way — draining an
/// oversized line to resync would cost exactly the unbounded read the cap exists to
/// prevent (P2 hardening) — as is a line that is not valid UTF-8, which cannot be a frame.
async fn serve_connection<S>(
    stream: S,
    handler: Arc<dyn Handler>,
    media: Arc<MediaStore>,
    transport: Transport,
    progress: Option<StdDuration>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut handshake_done = false;
    loop {
        let bytes = match read_with_deadline(&mut reader, progress).await? {
            CappedLine::Line(bytes) => bytes,
            CappedLine::Eof => return Ok(()),
            CappedLine::TooLarge => {
                let response = Response::Error(ErrorEnvelope::bad_request(
                    strings::frame_too_large(MAX_FRAME_BYTES),
                ));
                write_response(&mut reader, &response).await?;
                return Ok(());
            }
        };
        let line = match std::str::from_utf8(&bytes) {
            Ok(raw) => raw.trim(),
            Err(_) => {
                // A line that is not valid UTF-8 cannot be a frame: refused like an
                // over-cap frame, and the connection ends. (P2 hardening)
                let response = Response::Error(ErrorEnvelope::bad_request(strings::LINE_NOT_UTF8));
                write_response(&mut reader, &response).await?;
                return Ok(());
            }
        };
        if line.is_empty() {
            continue;
        }
        // The socket serves two protocols, distinguished by the first line (05 A8): an HTTP
        // media fetch (a request line whose target path is `/media...`) or JSON-RPC frames.
        // Every other line stays on the frame path, whose pinned refusals keep their answers.
        let mut words = line.split_whitespace();
        let method = words.next().unwrap_or_default();
        let target = words.next().unwrap_or_default();
        let http_method = matches!(
            method,
            "GET" | "POST" | "PUT" | "DELETE" | "HEAD" | "OPTIONS" | "PATCH" | "TRACE" | "CONNECT"
        );
        if http_method && target.starts_with("/media") {
            return serve_media_http(&mut reader, line, &media, &handler, &transport, progress)
                .await;
        }
        let request: Request = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(err) => {
                let response = Response::Error(ErrorEnvelope::bad_request(format!(
                    "malformed request frame: {err}"
                )));
                write_response(&mut reader, &response).await?;
                continue;
            }
        };
        if let Err(envelope) = request.validate() {
            write_response(&mut reader, &Response::Error(envelope)).await?;
            continue;
        }
        if !handshake_done {
            if request.method != methods::HANDSHAKE {
                let response = Response::Error(ErrorEnvelope::bad_request(format!(
                    "handshake must be the first call on every connection (got {:?})",
                    request.method
                )));
                write_response(&mut reader, &response).await?;
                return Ok(());
            }
            let response = handshake_response(&handler, request.params.as_ref());
            let accepted = matches!(response, Response::Result(_));
            write_response(&mut reader, &response).await?;
            if !accepted {
                // The refusal is the whole exchange: an out-of-range (or malformed)
                // handshake ends the connection — nothing is dispatched on it (05 A7,
                // and the source fixture's note: a refused handshake must not be
                // followed by anything).
                return Ok(());
            }
            handshake_done = true;
            continue;
        }
        if request.method == methods::HANDSHAKE {
            // A second handshake on one connection: the gate allows exactly one, first
            // (05 A7). Refused, and the refusal ends the connection — the same shape as
            // the wrong-first-call case above. (P2 hardening)
            let response =
                Response::Error(ErrorEnvelope::bad_request(strings::DUPLICATE_HANDSHAKE));
            write_response(&mut reader, &response).await?;
            return Ok(());
        }
        if let Transport::Tcp { authority } = &transport {
            // 07: the network gate runs on every call after the tokenless handshake. The
            // instance token is the transport gate: a failure means this caller is not a
            // client of this instance, so the connection ends ("nothing follows a refusal").
            // Scope identity is per request — a pooled connection may multiplex scopes — so
            // its refusals are dispatch-level and the connection continues.
            if let Err(envelope) = handler.authorize_scope(
                request.token.as_deref(),
                request.scope.as_deref(),
                request.scope_token.as_deref(),
                authority,
            ) {
                let transport_gate = envelope.code == ErrorCode::TokenRequired;
                write_response(&mut reader, &Response::Error(envelope)).await?;
                if transport_gate {
                    return Ok(());
                }
                continue;
            }
        }
        let origin = match request.origin.as_deref() {
            None => Origin::Shell,
            Some(raw) => match Origin::parse(raw) {
                Ok(origin) => origin,
                Err(envelope) => {
                    // A malformed frame field refuses like any other malformed request; the
                    // connection continues (dispatch-level class).
                    write_response(&mut reader, &Response::Error(envelope)).await?;
                    continue;
                }
            },
        };
        // Dispatch is where the work blocks: capture waits on a configured command (up to
        // `capture.timeout_s` on top of the clip's own duration) and `new` waits on a
        // compositor. Inline, each call occupies a reactor worker for that whole time, and
        // one call per worker leaves the manager unable to answer anything — `status`,
        // `close`, `drain` included. The blocking pool carries it instead; frames on one
        // connection still run strictly in order.
        let response = dispatch_offloaded(
            Arc::clone(&handler),
            request
                .scope
                .as_deref()
                .unwrap_or(OPERATOR_SCOPE)
                .to_string(),
            origin,
            request.method,
            request.params,
        )
        .await;
        write_response(&mut reader, &response).await?;
    }
}

/// Run one dispatch on the blocking pool (see `serve_connection`). A panic inside a dispatch
/// answers `internal` instead of silently dropping the connection: the blocking task cannot
/// unwind into the reactor.
async fn dispatch_offloaded(
    handler: Arc<dyn Handler>,
    scope: String,
    origin: Origin,
    method: String,
    params: Option<Value>,
) -> Response {
    match tokio::task::spawn_blocking(move || {
        handler.dispatch(&scope, origin, &method, params.as_ref())
    })
    .await
    {
        Ok(response) => response,
        Err(_) => Response::Error(ErrorEnvelope::internal(strings::DISPATCH_PANICKED)),
    }
}

/// The manager's media endpoint (05 A8): one HTTP fetch per connection, no token on the
/// unix socket (07's local rule — peer-credential trust, the same rule the bridge relies on).
/// Ids are never echoed in errors or logs; unknown, expired and consumed ids are identical.
/// One capped line, bounded by the transport's progress deadline (`None` on the unix socket):
/// a stalled handshake, frame or media head closes the connection instead of holding it.
async fn read_with_deadline<R>(
    reader: &mut R,
    progress: Option<StdDuration>,
) -> std::io::Result<crate::ipc::CappedLine>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    match progress {
        None => read_capped_line(reader).await,
        Some(deadline) => tokio::time::timeout(deadline, read_capped_line(reader))
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "manager progress deadline exceeded",
                )
            })?,
    }
}

async fn serve_media_http<S>(
    reader: &mut BufReader<S>,
    request_line: &str,
    media: &MediaStore,
    handler: &Arc<dyn Handler>,
    transport: &Transport,
    progress: Option<StdDuration>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Ok(());
    }
    // Headers: a media fetch carries no body; chunked framing or a declared body is refused
    // (the same strict-head class the network bridge pins).
    let mut content_length = 0usize;
    let mut presented_token: Option<String> = None;
    loop {
        let bytes = match read_with_deadline(reader, progress).await? {
            CappedLine::Line(bytes) => bytes,
            CappedLine::Eof => return Ok(()),
            CappedLine::TooLarge => {
                let envelope = ErrorEnvelope::bad_request(strings::HEAD_TOO_LARGE);
                return write_media_http(reader, 400, Some(&envelope), None).await;
            }
        };
        let line = String::from_utf8_lossy(&bytes);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            let envelope = ErrorEnvelope::bad_request(strings::MALFORMED_HTTP_REQUEST);
            return write_media_http(reader, 400, Some(&envelope), None).await;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "transfer-encoding" => {
                let envelope = ErrorEnvelope::bad_request(strings::CHUNKED_UNSUPPORTED);
                return write_media_http(reader, 400, Some(&envelope), None).await;
            }
            "content-length" => {
                content_length = match value.parse::<usize>() {
                    Ok(length) => length,
                    Err(_) => {
                        let envelope =
                            ErrorEnvelope::bad_request(strings::invalid_content_length(value));
                        return write_media_http(reader, 400, Some(&envelope), None).await;
                    }
                };
            }
            "daiorama-token" => presented_token = Some(value.to_string()),
            _ => {}
        }
    }
    // 07: on the TCP surface the instance token is the listener's gate. The media id remains
    // the artifact's own capability, and scope headers are not part of a media fetch — but a
    // network listener never serves without the token. The unix path stays tokenless.
    if let Transport::Tcp { authority } = transport
        && let Err(envelope) = handler.authorize_instance(presented_token.as_deref(), authority)
    {
        return write_media_http(reader, 403, Some(&envelope), None).await;
    }
    if content_length > 0 {
        let envelope = ErrorEnvelope::bad_request(strings::MEDIA_BODY_NOT_ALLOWED);
        return write_media_http(reader, 400, Some(&envelope), None).await;
    }
    if method != "GET" {
        let envelope = ErrorEnvelope::bad_request(strings::MEDIA_GET_ONLY);
        return write_media_http(reader, 405, Some(&envelope), Some("Allow: GET")).await;
    }
    let id = target.strip_prefix("/media/").unwrap_or_default();
    let capability_shaped = id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let handle = if capability_shaped {
        media.open(id)
    } else {
        None
    };
    match handle {
        Some(handle) => write_media_http_ok(reader, handle).await,
        None => {
            let envelope = ErrorEnvelope::media_expired();
            write_media_http(reader, 404, Some(&envelope), None).await
        }
    }
}

async fn write_media_http<S>(
    reader: &mut BufReader<S>,
    status: u16,
    envelope: Option<&ErrorEnvelope>,
    extra_header: Option<&str>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let body = envelope
        .map(|envelope| serde_json::to_vec(envelope).expect("media envelopes serialize"))
        .unwrap_or_default();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let content_type = if envelope.is_some() {
        "application/json"
    } else {
        "application/octet-stream"
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(extra) = extra_header {
        head.push_str(extra);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    reader.write_all(head.as_bytes()).await?;
    if !body.is_empty() {
        reader.write_all(&body).await?;
    }
    reader.flush().await
}

async fn write_media_http_ok<S>(
    reader: &mut BufReader<S>,
    handle: MediaHandle,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        handle.content_type,
        handle.bytes.len()
    );
    reader.write_all(head.as_bytes()).await?;
    reader.write_all(&handle.bytes).await?;
    reader.flush().await
}

/// Check the client's range against ours and answer, or refuse with the pinned mismatch
/// message (04/05).
fn handshake_response(handler: &Arc<dyn Handler>, params: Option<&Value>) -> Response {
    let ours = (
        PROTOCOL_VERSION
            .parse()
            .expect("our protocol version parses"),
        MIN_SUPPORTED.parse().expect("our min_supported parses"),
    );
    let client = params
        .and_then(|params| serde_json::from_value::<HandshakeParams>(params.clone()).ok())
        .and_then(|params| params.range().ok());
    match client {
        Some(range) if ranges_overlap(range, ours) => Response::Result(
            serde_json::to_value(handler.handshake_result()).expect("handshake result serializes"),
        ),
        Some(range) => Response::Error(ErrorEnvelope::version_incompatible(
            &range.0.to_string(),
            &range.1.to_string(),
            &ours.0.to_string(),
            &ours.1.to_string(),
        )),
        None => Response::Error(ErrorEnvelope::bad_request(
            "handshake params must carry protocol_version and min_supported",
        )),
    }
}

async fn write_response<W>(writer: &mut W, response: &Response) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    crate::ipc::write_frame(writer, response).await
}

/// Where the phases meet: the strings the runtime side prints when the machine cannot
/// contain sessions.
pub fn limits_disabled_notice(plan: &startup::StartupPlan) -> Option<&'static str> {
    match plan.limits {
        startup::LimitsPlan::Disabled => Some(
            "containment off (limits.mode = off): no per-session limits and no crash-time cleanup",
        ),
        startup::LimitsPlan::Enabled { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::startup::{
        ContainmentProbe, DelegatedProbe, LimitsPlan, StartupDecision, UnavailableProbe,
        plan_startup,
    };
    use super::*;
    use crate::config::ConfigValue;
    use crate::ipc::{Address, Client};
    use crate::strings;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    fn record(id: &str, state: SessionState) -> SessionRecord {
        SessionRecord {
            id: id.to_string(),
            label: String::new(),
            state,
            backend: Backend::Headless,
            resolution: "1920x1080".to_string(),
            scale: 1.0,
            scope: "operator".to_string(),
            origin: Origin::Shell,
            idle_since: "2026-10-15T00:00:00Z".to_string(),
            view: None,
            instance_id: "11111111-1111-4111-8111-111111111111".to_string(),
            renderer: RendererMark::Verified,
            cause: None,
            view_generation: 0,
        }
    }

    #[test]
    fn state_machine_transitions_carry_the_distinct_codes() {
        let mut registry = Registry::new("11111111-1111-4111-8111-111111111111");
        registry.insert(record("s-1", SessionState::Running));

        // Unknown id: session_not_found with the corrective hint.
        let err = registry.call_gate("nope").unwrap_err();
        assert_eq!(err.code, crate::ipc::ErrorCode::SessionNotFound);
        assert_eq!(err.message, "session not found");
        assert_eq!(err.hint.as_deref(), Some(strings::HINT_LIST_SESSIONS));

        // Close: allowed from running, final, tombstoned as closed.
        registry.close("s-1").unwrap();
        let err = registry.call_gate("s-1").unwrap_err();
        assert_eq!(err.code, crate::ipc::ErrorCode::SessionClosed);
        let err = registry.close("s-1").unwrap_err();
        assert_eq!(
            err.code,
            crate::ipc::ErrorCode::SessionClosed,
            "close of closed"
        );

        // A reap marks dead with cause reaped while the entry still lives.
        registry.insert(record("s-2", SessionState::Running));
        registry.reap("s-2").unwrap();
        assert_eq!(registry.get("s-2").unwrap().state, SessionState::Dead);
        assert_eq!(registry.get("s-2").unwrap().cause(), Some(Cause::Reaped));
        let err = registry.call_gate("s-2").unwrap_err();
        assert_eq!(err.code, crate::ipc::ErrorCode::SessionReaped);
        // The sweep closes it and keeps the reaped tombstone.
        let swept = registry.sweep_dead();
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].id, "s-2");
        assert_eq!(
            registry.call_gate("s-2").unwrap_err().code,
            crate::ipc::ErrorCode::SessionReaped
        );

        // Health failure / crash: same dead state, different code.
        registry.insert(record("s-3", SessionState::Running));
        registry.mark_dead("s-3").unwrap();
        assert_eq!(registry.get("s-3").unwrap().cause(), Some(Cause::Dead));
        assert_eq!(
            registry.call_gate("s-3").unwrap_err().code,
            crate::ipc::ErrorCode::SessionDead
        );

        // Degraded is alive: it answers calls and counts as running.
        registry.insert(record("s-4", SessionState::Degraded));
        assert!(registry.call_gate("s-4").is_ok());
        assert_eq!(registry.counts().running, 1, "s-4 alive, s-3 dead");
        assert_eq!(registry.counts().dead, 1);

        // Close on a dead session is allowed (an operator may close one immediately).
        registry.close("s-3").unwrap();
        assert_eq!(
            registry.call_gate("s-3").unwrap_err().code,
            crate::ipc::ErrorCode::SessionClosed
        );
    }

    /// Tombstones expire: inside the 24 h window the distinct code, outside it
    /// `session_not_found` (06).
    #[test]
    fn tombstone_window_bounds_the_distinct_codes() {
        let mut registry = Registry::new("instance");
        registry.insert(record("s-1", SessionState::Running));
        registry.close("s-1").unwrap();
        assert_eq!(
            registry.call_gate("s-1").unwrap_err().code,
            crate::ipc::ErrorCode::SessionClosed
        );

        let just_before = Instant::now() + TOMBSTONE_TTL - StdDuration::from_secs(1);
        assert_eq!(registry.forget_expired_tombstones(just_before), 0);
        assert_eq!(
            registry.call_gate("s-1").unwrap_err().code,
            crate::ipc::ErrorCode::SessionClosed
        );

        let just_after = Instant::now() + TOMBSTONE_TTL + StdDuration::from_secs(1);
        assert_eq!(registry.forget_expired_tombstones(just_after), 1);
        assert_eq!(
            registry.call_gate("s-1").unwrap_err().code,
            crate::ipc::ErrorCode::SessionNotFound,
            "outside the window the id is simply unknown"
        );
    }

    /// Dead sessions are shown separately and do not count against quotas (06).
    #[test]
    fn dead_sessions_do_not_count_against_quotas() {
        let mut registry = Registry::new("instance");
        registry.insert(record("alive-1", SessionState::Running));
        registry.insert(record("dead-1", SessionState::Dead));
        registry.insert(record("dead-2", SessionState::Dead));

        assert_eq!(
            registry.counts(),
            SessionCounts {
                running: 1,
                dead: 2
            }
        );
        assert_eq!(
            registry.live_count(),
            1,
            "dead entries hold no live resources"
        );

        // With a global ceiling of 1 and one live session, the next create is refused.
        let err = registry.quota_check("operator", 1, 4, 1).unwrap_err();
        assert_eq!(err.code, crate::ipc::ErrorCode::QuotaExceeded);
        assert_eq!(
            err.message,
            "session limit reached: scope 'operator' 1/4; global 1/1. Close a session first."
        );

        // Closing the dead entries frees the registry slots and changes nothing live.
        assert_eq!(registry.sweep_dead().len(), 2);
        assert_eq!(
            registry.counts(),
            SessionCounts {
                running: 1,
                dead: 0
            }
        );
        assert_eq!(registry.live_count(), 1);
    }

    #[test]
    fn session_records_serialize_to_the_pinned_05_shape() {
        let mut view_record = record("s-1", SessionState::Running);
        view_record.view = Some(ViewRecord {
            state: ViewState::Running,
            address: Some("http://127.0.0.1:6080/?credential=abc".to_string()),
            front_port: 6080,
            vnc_port: 5901,
            generation: 2,
        });
        let value = view_record.to_json();
        assert_eq!(
            value,
            json!({
                "id": "s-1",
                "label": "",
                "state": "running",
                "backend": "headless",
                "resolution": "1920x1080",
                "scale": 1.0,
                "scope": "operator",
                "origin": "shell",
                "idle_since": "2026-10-15T00:00:00Z",
                "view": {
                    "state": "running",
                    "address": "http://127.0.0.1:6080/?credential=abc",
                    "front_port": 6080,
                    "vnc_port": 5901,
                    "generation": 2
                },
                "instance_id": "11111111-1111-4111-8111-111111111111",
                "renderer": "verified",
            })
        );

        // A session with no view serializes `view: null` (the pinned shape's null case).
        let bare = record("s-2", SessionState::Running);
        assert_eq!(bare.to_json()["view"], Value::Null);
    }

    /// View death leaves the session running: view down ≠ session dead (06 §Views).
    #[test]
    fn view_down_is_not_session_dead() {
        let mut session = record("s-1", SessionState::Running);
        session.view = Some(ViewRecord {
            state: ViewState::Running,
            address: Some("http://127.0.0.1:6080/".to_string()),
            front_port: 6080,
            vnc_port: 5901,
            generation: 1,
        });

        // The view dies: mark it down, leave the session running.
        session.view.as_mut().unwrap().state = ViewState::Down;
        assert_eq!(
            session.state,
            SessionState::Running,
            "view down ≠ session dead"
        );
        assert_eq!(session.to_json()["view"]["state"], "down");
        assert_eq!(session.to_json()["state"], "running");

        // A fresh view start increments the generation, so a cached address is detectable.
        session.view.as_mut().unwrap().generation += 1;
        assert_eq!(session.to_json()["view"]["generation"], 2);

        // Session death kills the view with it (explicit kill-on-session-death).
        let mut registry = Registry::new("instance");
        registry.insert(session);
        registry.mark_dead("s-1").unwrap();
        assert!(registry.get("s-1").unwrap().view.is_none());
    }

    #[test]
    fn session_spec_defaults_are_the_pinned_ones() {
        let spec = SessionSpec::default();
        assert_eq!(spec.backend, Backend::Headless);
        assert_eq!(spec.resolution, "1920x1080");
        assert_eq!(spec.scale, 1.0);
        assert!(!spec.view);
        assert_eq!(spec.label, None);
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            json!({"backend": "headless", "resolution": "1920x1080", "scale": 1.0, "view": false})
        );

        assert_eq!(Resolution::parse("1024x768").unwrap().as_str(), "1024x768");
        assert!(Resolution::parse("1024").is_err());
        assert!(Resolution::parse("1024x0").is_err());
        assert!(Resolution::parse("x768").is_err());
        assert_eq!(Backend::parse("host").unwrap(), Backend::Host);
        assert!(Backend::parse("wayland").is_err());
    }

    /// 06 C6: off-mode proceeds with limits disabled and no sweep; on-mode refuses without
    /// delegation.
    #[test]
    fn startup_gate_matches_the_containment_classes() {
        let delegated = DelegatedProbe::default();
        let unavailable = UnavailableProbe::new("cgroupfs is read-only");

        // Default config: limits on, delegation present.
        let config = CoreConfig::default();
        match plan_startup(&config, &delegated) {
            StartupDecision::Proceed(plan) => {
                assert!(plan.sweep, "containment on sweeps the instance subtree");
                assert!(matches!(plan.limits, LimitsPlan::Enabled { .. }));
            }
            other => panic!("expected proceed, got {other:?}"),
        }

        // limits.mode = off: proceed, limits disabled, no sweep — the recorded downgrade.
        let mut off = CoreConfig::default();
        off.set("limits.mode", ConfigValue::Toggle(Toggle::Off))
            .unwrap();
        match plan_startup(&off, &unavailable) {
            StartupDecision::Proceed(plan) => {
                assert_eq!(plan.limits, LimitsPlan::Disabled);
                assert!(!plan.sweep, "off-mode never sweeps (no subtree to sweep)");
            }
            other => panic!("expected proceed, got {other:?}"),
        }

        // limits.mode = on without delegation: fail the start with the pinned string.
        match plan_startup(&config, &unavailable) {
            StartupDecision::Refuse(envelope) => {
                assert_eq!(envelope.code, crate::ipc::ErrorCode::LimitsUnavailable);
                assert_eq!(
                    envelope.message,
                    strings::containment_unavailable("cgroupfs is read-only")
                );
                assert!(envelope.message.contains("set limits.mode = off"));
            }
            other => panic!("expected refuse, got {other:?}"),
        }

        // The probe contract itself: an error carries the detail through.
        match unavailable.probe() {
            Err(detail) => assert_eq!(detail, "cgroupfs is read-only"),
            Ok(_) => panic!("unavailable probe must fail"),
        }
        assert!(delegated.probe().is_ok());
    }

    #[test]
    fn limits_disabled_notice_is_honest_about_the_cost() {
        let plan = startup::StartupPlan {
            limits: LimitsPlan::Disabled,
            sweep: false,
        };
        let notice = limits_disabled_notice(&plan).expect("off-mode is announced");
        assert!(notice.contains("no per-session limits"));
        assert!(notice.contains("no crash-time cleanup"));
        let enabled = startup::StartupPlan {
            limits: LimitsPlan::Enabled {
                detail: "delegated".to_string(),
            },
            sweep: true,
        };
        assert!(limits_disabled_notice(&enabled).is_none());
    }

    /// End to end over a real unix socket: handshake first, then the read shapes, then the
    /// not-implemented envelope for anything the runtime side owns.
    #[tokio::test]
    async fn listener_serves_handshake_then_dispatches() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("manager.sock");
        let listener = Listener::bind(&socket).await.unwrap();
        assert_eq!(listener.path(), socket.as_path());

        let status = InstanceStatus {
            state: InstanceState::Running,
            mode: InstanceMode::Attached,
            pid: 4242,
            socket: socket.display().to_string(),
            versions: VersionInfo::current(),
            sessions: SessionCounts {
                running: 2,
                dead: 1,
            },
            drain: DrainState::default(),
            instance_id: "11111111-1111-4111-8111-111111111111".to_string(),
        };
        let handler = Arc::new(SkeletonHandler {
            status: status.clone(),
            dashboard_module: Some("2026.10.0".to_string()),
        });
        let serving = tokio::spawn(listener.serve(
            handler,
            Arc::new(MediaStore::with_ttl(
                dir.path().to_path_buf(),
                crate::media::DEFAULT_TTL,
            )),
        ));

        let address = Address::Unix(socket.clone());
        let mut client = Client::connect(&address, StdDuration::from_secs(2), None)
            .await
            .expect("listen socket accepts");
        let handshake = client.handshake().await.expect("handshake succeeds");
        assert_eq!(handshake.protocol_version, PROTOCOL_VERSION);
        assert_eq!(handshake.instance_id, status.instance_id);

        // status: the pinned shape, verbatim.
        let value = client.request(methods::STATUS, None, None).await.unwrap();
        assert_eq!(value, serde_json::to_value(&status).unwrap());

        // readiness: the same field set plus dashboard_module (05).
        let value = client
            .request(methods::READINESS, None, None)
            .await
            .unwrap();
        assert_eq!(value["versions"]["dashboard_module"], "2026.10.0");
        assert_eq!(value["sessions"]["running"], 2);
        assert_eq!(value["sessions"]["dead"], 1);

        // Anything the runtime side owns answers the not-implemented envelope.
        let err = client
            .request(methods::CREATE_SESSION, Some(json!({})), None)
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::ipc::ErrorCode::Internal);
        assert_eq!(err.message, strings::NOT_IMPLEMENTED_MESSAGE);
        assert_eq!(err.hint, None);

        // A second connection must handshake first: anything else is bad_request.
        let mut rogue = Client::connect(&address, StdDuration::from_secs(2), None)
            .await
            .unwrap();
        let err = rogue
            .request(methods::STATUS, None, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::ipc::ErrorCode::BadRequest);
        assert!(err.message.contains("handshake must be the first call"));

        // A mismatched handshake is refused with both pairs in the message.
        let mut wrong = Client::connect(&address, StdDuration::from_secs(2), None)
            .await
            .unwrap();
        let err = wrong
            .request(
                methods::HANDSHAKE,
                Some(json!({"protocol_version": "9.9.9", "min_supported": "9.9.9"})),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::ipc::ErrorCode::VersionIncompatible);
        assert!(err.message.contains("this client speaks 9.9.9 (min 9.9.9)"));
        assert!(err.message.contains("manager speaks 0.1.0 (min 0.1.0)"));

        // A too-long idempotency key is frame validation, before dispatch.
        let err = client
            .request(
                methods::STATUS,
                None,
                Some("k".repeat(crate::ipc::IDEMPOTENCY_KEY_MAX + 1)),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::ipc::ErrorCode::BadRequest);

        serving.abort();
    }

    /// A dispatch that blocks (capture waits on a configured command; `new` waits on a
    /// compositor) must not occupy a reactor worker. Pre-fix, every worker sitting inside
    /// `dispatch` means a plain `status` from a third connection is not even read: the fast
    /// call waits out the slow ones, which in production is 10 s per screenshot and up to
    /// 40 s per clip — with one call per worker, the manager stops answering anything.
    struct BlockingHandler {
        entered: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
        release: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    }

    impl Handler for BlockingHandler {
        fn handshake_result(&self) -> HandshakeResult {
            HandshakeResult {
                protocol_version: PROTOCOL_VERSION.to_string(),
                min_supported: Some(MIN_SUPPORTED.to_string()),
                instance_id: "11111111-1111-4111-8111-111111111111".to_string(),
            }
        }

        fn dispatch(
            &self,
            _scope: &str,
            _origin: Origin,
            method: &str,
            _params: Option<&Value>,
        ) -> Response {
            if method == "slow" {
                let _ = self.entered.lock().expect("entered lock").send(());
                // Released by the test once the fast call has been timed; the deadline only
                // bounds the pre-fix run (where the test cannot get to the release).
                let deadline = std::time::Instant::now() + StdDuration::from_secs(3);
                let (lock, cvar) = &*self.release;
                let mut released = lock.lock().expect("release lock");
                while !*released {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        break;
                    }
                    released = cvar
                        .wait_timeout(released, deadline - now)
                        .expect("release wait")
                        .0;
                }
                return Response::Result(json!({"slow": true}));
            }
            Response::Result(json!({"ok": true}))
        }
    }

    /// One blocking client call outside the runtime: handshake, then `method`, one line back.
    fn blocking_call(socket: &Path, method: &str) -> String {
        use std::io::{BufRead, BufReader, Write};
        let stream = std::os::unix::net::UnixStream::connect(socket).expect("connect");
        stream
            .set_read_timeout(Some(StdDuration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut writer = stream;
        let handshake = json!({
            "method": methods::HANDSHAKE,
            "params": {"protocol_version": PROTOCOL_VERSION, "min_supported": MIN_SUPPORTED},
        });
        writeln!(writer, "{handshake}").unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).expect("handshake answer");
        writeln!(writer, "{}", json!({"method": method, "params": null})).unwrap();
        let mut answer = String::new();
        reader.read_line(&mut answer).expect("call answer");
        answer
    }

    #[test]
    fn a_blocking_dispatch_cannot_starve_the_reactor() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("manager.sock");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let release =
            std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let handler = Arc::new(BlockingHandler {
            entered: std::sync::Mutex::new(entered_tx),
            release: std::sync::Arc::clone(&release),
        });
        let listener = runtime
            .block_on(async { Listener::bind(&socket).await })
            .unwrap();
        runtime.spawn(listener.serve(
            handler,
            Arc::new(MediaStore::with_ttl(
                dir.path().to_path_buf(),
                crate::media::DEFAULT_TTL,
            )),
        ));

        // Two blocking dispatches: one per worker thread.
        let slow: Vec<_> = (0..2)
            .map(|_| {
                let socket = socket.clone();
                std::thread::spawn(move || blocking_call(&socket, "slow"))
            })
            .collect();
        for _ in 0..2 {
            entered_rx
                .recv_timeout(StdDuration::from_secs(5))
                .expect("both slow dispatches are in flight");
        }

        let started = std::time::Instant::now();
        let answer = blocking_call(&socket, methods::STATUS);
        let elapsed = started.elapsed();
        assert!(answer.contains("\"ok\":true"), "{answer}");
        assert!(
            elapsed < StdDuration::from_secs(1),
            "a fast call waited {elapsed:?} behind two blocking dispatches"
        );

        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        for thread in slow {
            thread.join().unwrap();
        }
    }

    /// The blocking-pool boundary still answers: a panic inside a dispatch is a JoinError, the
    /// connection continues, and the next call on it is served (the pinned `internal` is in
    /// fixtures/README and the README-reading test above).
    #[tokio::test]
    async fn a_panicking_dispatch_answers_internal_and_keeps_the_connection() {
        struct PanickingHandler;

        impl Handler for PanickingHandler {
            fn handshake_result(&self) -> HandshakeResult {
                HandshakeResult {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    min_supported: Some(MIN_SUPPORTED.to_string()),
                    instance_id: "11111111-1111-4111-8111-111111111111".to_string(),
                }
            }

            fn dispatch(
                &self,
                _scope: &str,
                _origin: Origin,
                method: &str,
                _params: Option<&Value>,
            ) -> Response {
                if method == "boom" {
                    panic!("dispatch blew up");
                }
                Response::Result(json!({"ok": true}))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("manager.sock");
        let listener = Listener::bind(&socket).await.unwrap();
        let serving = tokio::spawn(listener.serve(
            Arc::new(PanickingHandler),
            Arc::new(MediaStore::with_ttl(
                dir.path().to_path_buf(),
                crate::media::DEFAULT_TTL,
            )),
        ));
        let mut client = Client::connect(
            &Address::Unix(socket.clone()),
            StdDuration::from_secs(2),
            None,
        )
        .await
        .unwrap();
        client.handshake().await.unwrap();

        let err = client.request("boom", None, None).await.unwrap_err();
        assert_eq!(err.code, crate::ipc::ErrorCode::Internal);
        assert_eq!(err.message, strings::DISPATCH_PANICKED);

        let value = client.request(methods::STATUS, None, None).await.unwrap();
        assert_eq!(value["ok"], true);

        serving.abort();
    }

    /// 05 A7: an out-of-range handshake is rejected *at connect* — the refusal ends the
    /// connection, nothing may follow it (the conformance fixture's note: a refused
    /// handshake must not be followed by anything).
    #[tokio::test]
    async fn a_refused_handshake_ends_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("manager.sock");
        let listener = Listener::bind(&socket).await.unwrap();
        let handler = Arc::new(SkeletonHandler {
            status: InstanceStatus {
                state: InstanceState::Running,
                mode: InstanceMode::Attached,
                pid: 4242,
                socket: socket.display().to_string(),
                versions: VersionInfo::current(),
                sessions: SessionCounts {
                    running: 0,
                    dead: 0,
                },
                drain: DrainState::default(),
                instance_id: "11111111-1111-4111-8111-111111111111".to_string(),
            },
            dashboard_module: None,
        });
        let serving = tokio::spawn(listener.serve(
            handler,
            Arc::new(MediaStore::with_ttl(
                dir.path().to_path_buf(),
                crate::media::DEFAULT_TTL,
            )),
        ));
        let address = Address::Unix(socket.clone());

        // A disjoint range, and a params-less handshake: both are refusals, and both end
        // the connection — the next call on it cannot be dispatched.
        for params in [
            Some(json!({ "protocol_version": "9.9.9", "min_supported": "9.9.9" })),
            None,
        ] {
            let mut client = Client::connect(&address, StdDuration::from_secs(2), None)
                .await
                .unwrap();
            let refusal = client
                .request(methods::HANDSHAKE, params, None)
                .await
                .unwrap_err();
            assert!(matches!(
                refusal.code,
                crate::ipc::ErrorCode::VersionIncompatible | crate::ipc::ErrorCode::BadRequest
            ));

            let after = client.request(methods::STATUS, None, None).await;
            assert!(
                after.is_err(),
                "a call after a refused handshake must not be dispatched (got {after:?})"
            );
        }

        serving.abort();
    }

    /// P2 hardening: the read side is frame-capped. An over-cap frame is refused
    /// (`bad_request`, the pinned message) and the connection ends — nothing follows the
    /// refusal, like a refused handshake. The cap is exact: a frame at exactly
    /// `MAX_FRAME_BYTES` is read normally (here it fails as malformed JSON, and the
    /// connection stays usable) — no new refusal at the boundary.
    #[tokio::test]
    async fn frame_cap_refuses_oversized_frames_and_is_exact_at_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("manager.sock");
        let listener = Listener::bind(&socket).await.unwrap();
        let handler = Arc::new(SkeletonHandler {
            status: InstanceStatus {
                state: InstanceState::Running,
                mode: InstanceMode::Attached,
                pid: 4242,
                socket: socket.display().to_string(),
                versions: VersionInfo::current(),
                sessions: SessionCounts {
                    running: 0,
                    dead: 0,
                },
                drain: DrainState::default(),
                instance_id: "11111111-1111-4111-8111-111111111111".to_string(),
            },
            dashboard_module: None,
        });
        let serving = tokio::spawn(listener.serve(
            handler,
            Arc::new(MediaStore::with_ttl(
                dir.path().to_path_buf(),
                crate::media::DEFAULT_TTL,
            )),
        ));

        // --- One byte over the cap: refused with the pinned message, then the connection
        // ends. The writer may fail partway (broken pipe) once the manager stops reading
        // and closes — that is the refusal taking effect, not a test failure.
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut over = vec![b'x'; crate::ipc::MAX_FRAME_BYTES + 1];
        over.push(b'\n');
        let _ = write_half.write_all(&over).await;
        let mut reader = BufReader::new(read_half);
        match crate::ipc::read_frame(&mut reader)
            .await
            .expect("refusal frame")
        {
            Response::Error(envelope) => {
                assert_eq!(envelope.code, crate::ipc::ErrorCode::BadRequest);
                assert_eq!(
                    envelope.message,
                    strings::frame_too_large(crate::ipc::MAX_FRAME_BYTES)
                );
            }
            other => panic!("expected the cap refusal, got {other:?}"),
        }
        let after = crate::ipc::read_frame(&mut reader).await;
        assert_eq!(
            after.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof,
            "nothing follows the refusal"
        );

        // --- Exactly at the cap: read and answered normally; the connection stays usable.
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut exact = vec![b'x'; crate::ipc::MAX_FRAME_BYTES];
        exact.push(b'\n');
        write_half.write_all(&exact).await.unwrap();
        let handshake = Request::new(
            methods::HANDSHAKE,
            Some(json!({
                "protocol_version": PROTOCOL_VERSION,
                "min_supported": MIN_SUPPORTED,
            })),
        );
        let mut line = serde_json::to_string(&handshake).unwrap();
        line.push('\n');
        write_half.write_all(line.as_bytes()).await.unwrap();

        let mut reader = BufReader::new(read_half);
        match crate::ipc::read_frame(&mut reader)
            .await
            .expect("malformed-frame refusal")
        {
            Response::Error(envelope) => {
                assert_eq!(envelope.code, crate::ipc::ErrorCode::BadRequest);
                assert!(
                    envelope.message.starts_with("malformed request frame"),
                    "got {}",
                    envelope.message
                );
            }
            other => panic!("expected the malformed-frame bad_request, got {other:?}"),
        }
        assert!(
            matches!(
                crate::ipc::read_frame(&mut reader)
                    .await
                    .expect("handshake answer"),
                Response::Result(_)
            ),
            "a frame at exactly the cap is not oversized: the connection stays usable"
        );

        serving.abort();
    }

    /// Scaffold for the connection-gate tests: a live listener + skeleton handler over a
    /// fresh socket in `dir`. Returns the socket path and the serving task to abort.
    async fn serving_listener(dir: &Path) -> (PathBuf, tokio::task::JoinHandle<()>) {
        let socket = dir.join("manager.sock");
        let listener = Listener::bind(&socket).await.unwrap();
        let handler = Arc::new(SkeletonHandler {
            status: InstanceStatus {
                state: InstanceState::Running,
                mode: InstanceMode::Attached,
                pid: 4242,
                socket: socket.display().to_string(),
                versions: VersionInfo::current(),
                sessions: SessionCounts {
                    running: 0,
                    dead: 0,
                },
                drain: DrainState::default(),
                instance_id: "11111111-1111-4111-8111-111111111111".to_string(),
            },
            dashboard_module: None,
        });
        (
            socket,
            tokio::spawn(listener.serve(
                handler,
                Arc::new(MediaStore::with_ttl(
                    dir.to_path_buf(),
                    crate::media::DEFAULT_TTL,
                )),
            )),
        )
    }

    /// Connect a raw stream and complete the handshake; returns the reader (positioned
    /// after the handshake answer) and the write half.
    async fn connect_handshaked(
        socket: &Path,
    ) -> (
        BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
    ) {
        let stream = UnixStream::connect(socket).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let handshake = Request::new(
            methods::HANDSHAKE,
            Some(json!({
                "protocol_version": PROTOCOL_VERSION,
                "min_supported": MIN_SUPPORTED,
            })),
        );
        let mut line = serde_json::to_string(&handshake).unwrap();
        line.push('\n');
        write_half.write_all(line.as_bytes()).await.unwrap();
        assert!(matches!(
            crate::ipc::read_frame(&mut reader)
                .await
                .expect("handshake answer"),
            Response::Result(_)
        ));
        (reader, write_half)
    }

    /// P2 hardening (third pass): a line that is not valid UTF-8 cannot be a frame — it is
    /// refused with `bad_request` (the frame-contract string) and the connection ends;
    /// nothing follows, the same shape as the over-cap refusal.
    #[tokio::test]
    async fn a_non_utf8_line_is_refused_then_the_connection_ends() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, serving) = serving_listener(dir.path()).await;
        let (mut reader, mut write_half) = connect_handshaked(&socket).await;

        write_half
            .write_all(b"\xff\xfe{\"method\":\"status\"}\n")
            .await
            .unwrap();
        match crate::ipc::read_frame(&mut reader)
            .await
            .expect("refusal frame")
        {
            Response::Error(envelope) => {
                assert_eq!(envelope.code, crate::ipc::ErrorCode::BadRequest);
                assert_eq!(envelope.message, strings::LINE_NOT_UTF8);
            }
            other => panic!("expected the non-UTF-8 refusal, got {other:?}"),
        }
        assert_eq!(
            crate::ipc::read_frame(&mut reader)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof,
            "nothing follows the refusal"
        );

        serving.abort();
    }

    /// P2 hardening (third pass): a second `handshake` on one connection is refused with
    /// `bad_request` (the gate allows exactly one, first) and the connection ends — the
    /// same shape as the wrong-first-call case.
    #[tokio::test]
    async fn a_duplicate_handshake_is_refused_then_the_connection_ends() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, serving) = serving_listener(dir.path()).await;
        let (mut reader, mut write_half) = connect_handshaked(&socket).await;

        let handshake = Request::new(
            methods::HANDSHAKE,
            Some(json!({
                "protocol_version": PROTOCOL_VERSION,
                "min_supported": MIN_SUPPORTED,
            })),
        );
        let mut line = serde_json::to_string(&handshake).unwrap();
        line.push('\n');
        write_half.write_all(line.as_bytes()).await.unwrap();
        match crate::ipc::read_frame(&mut reader)
            .await
            .expect("refusal frame")
        {
            Response::Error(envelope) => {
                assert_eq!(envelope.code, crate::ipc::ErrorCode::BadRequest);
                assert_eq!(envelope.message, strings::DUPLICATE_HANDSHAKE);
            }
            other => panic!("expected the duplicate-handshake refusal, got {other:?}"),
        }
        assert_eq!(
            crate::ipc::read_frame(&mut reader)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof,
            "nothing follows the refusal"
        );

        serving.abort();
    }

    /// The fixture README's frame contract is normative: every refusal string it pins must
    /// be the string the manager sends, byte for byte — or spec drift fails the build.
    #[test]
    fn frame_contract_refusals_match_the_fixture_readme() {
        const README: &str = include_str!("../../fixtures/README.md");
        // The README wraps prose; compare on whitespace-flattened text so the pin tracks
        // wording, not line width.
        let flat = README.split_whitespace().collect::<Vec<_>>().join(" ");
        for pinned in [
            strings::frame_too_large(MAX_FRAME_BYTES),
            strings::LINE_NOT_UTF8.to_string(),
            strings::DUPLICATE_HANDSHAKE.to_string(),
            strings::NOT_IMPLEMENTED_MESSAGE.to_string(),
            strings::unknown_method("foo"),
            strings::unknown_tool("x"),
            strings::invalid_origin("x"),
            strings::takes_no_parameters("bogus"),
            strings::DISPATCH_PANICKED.to_string(),
            strings::FRAME_SCOPE_REQUIRED.to_string(),
            strings::scope_has_no_secret("x"),
            strings::frame_scope_auth_required("x"),
            strings::INVALID_SCOPE_NAME.to_string(),
        ] {
            assert!(
                flat.contains(&pinned),
                "fixtures/README.md must pin {pinned:?} (modulo line wrapping)"
            );
        }
    }

    #[tokio::test]
    async fn bind_refuses_to_steal_a_live_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("manager.sock");
        let _live = Listener::bind(&socket).await.unwrap();
        let second = Listener::bind(&socket).await;
        assert!(second.is_err(), "one manager per runtime path");
    }

    /// One live leg for the media store: an in-process manager serves a **test-minted** id and
    /// the real `daiorama mcp --listen` relay fetches it end to end. No compositor, no capture
    /// binaries — the store slice is env-independent by design.
    #[tokio::test]
    async fn media_endpoint_relays_a_test_minted_id() {
        use std::process::Stdio;
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        use tokio::process::Command;

        async fn fetch(port: u16, token: &str, path: &str) -> (String, Vec<u8>) {
            let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            let mut stream = stream;
            let request = format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nDaiorama-Token: {token}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut head = Vec::new();
            let mut line = Vec::new();
            loop {
                line.clear();
                reader.read_until(b'\n', &mut line).await.unwrap();
                if line == b"\r\n" {
                    break;
                }
                head.extend_from_slice(&line);
            }
            let head = String::from_utf8_lossy(&head).to_string();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).await.unwrap();
            (head, body)
        }

        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run/daiorama/default");
        std::fs::create_dir_all(&run).unwrap();
        let socket = run.join("manager.sock");
        let status = InstanceStatus {
            state: InstanceState::Running,
            mode: InstanceMode::Attached,
            pid: 4242,
            socket: socket.display().to_string(),
            versions: VersionInfo::current(),
            sessions: SessionCounts::default(),
            drain: DrainState::default(),
            instance_id: "11111111-1111-4111-8111-111111111111".to_string(),
        };
        let handler = Arc::new(SkeletonHandler {
            status,
            dashboard_module: None,
        });
        let store = Arc::new(MediaStore::with_ttl(
            dir.path().join("media"),
            crate::media::DEFAULT_TTL,
        ));
        let bytes = b"relayed-artifact".to_vec();
        let id = store.mint("session-1", "application/octet-stream", bytes.clone(), true);
        let listener = Listener::bind(&socket).await.unwrap();
        let serving = tokio::spawn(listener.serve(handler, Arc::clone(&store)));

        let bin = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("daiorama");
        let mut child = Command::new(bin)
            .arg("mcp")
            .arg("--listen")
            .arg("127.0.0.1:0")
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_DATA_HOME", dir.path().join("data"))
            .env("XDG_RUNTIME_DIR", dir.path().join("run"))
            .env("TMPDIR", dir.path().join("tmp"))
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
        let mut port = 0u16;
        while port == 0 {
            let line = lines.next_line().await.unwrap().unwrap_or_default();
            if let Some(rest) = line.strip_prefix("listening on ") {
                port = rest
                    .rsplit(':')
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
            }
        }
        let token = std::fs::read_to_string(dir.path().join("data/daiorama/default/token"))
            .unwrap()
            .trim()
            .to_string();

        let (head, body) = fetch(port, &token, &format!("/media/{id}")).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body, bytes, "the artifact is relayed byte-exact");
        let persisted = dir.path().join("media/session-1").join(&id);
        assert_eq!(
            std::fs::read(&persisted).unwrap(),
            bytes,
            "store_clips persisted the same artifact"
        );

        let (head, body) = fetch(port, &token, &format!("/media/{id}")).await;
        assert!(head.starts_with("HTTP/1.1 404"), "{head}");
        assert!(
            String::from_utf8_lossy(&body).contains("media_expired"),
            "a consumed id is media_expired"
        );
        assert!(persisted.exists(), "persistence never resurrects the URL");

        let (head, _) = fetch(port, &token, "/media/../../token").await;
        assert!(head.starts_with("HTTP/1.1 404"), "{head}");

        child.kill().await.ok();
        serving.abort();
    }
}
