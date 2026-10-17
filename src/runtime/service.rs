//! The runtime service: the real dispatcher behind `daiorama serve`.
//!
//! This is the seam where the runtime side replaces the contract layer's `SkeletonHandler`:
//! it owns the session registry and speaks the pinned wire shapes for the lifecycle verbs —
//! `create_session`, `list_sessions`, `session_status`, `close_session` — plus the live
//! `status`/`readiness` reads, `config_reload` (08 reload semantics) and the `config get/set`
//! RPCs (08 A2 authority). Methods later phases own (views, history, the control tools) answer
//! the not-implemented envelope until their slices land; a name outside the pinned surface is a
//! `bad_request`.
//!
//! Slice scope (serve wiring, first piece):
//! - Sessions run through the injected [`SessionSpawner`]; the service never spawns directly.
//! - One scope exists for now: the implicit `operator` scope every CLI call runs as (04). Its
//!   quota comes from `scopes/operator.ron` (`sessions.max`, read per call — 08 D4) with the
//!   global ceiling from `sessions.max_global` (08).
//! - `view: true` on create is refused with `view_unavailable`: this build has no view
//!   provider yet (views land with P4). Nothing is spawned when it refuses.
//! - Renderer marks come from the spawner's creation gate (06 D6): the detected renderer
//!   rides back on the spawn outcome — `verified` for a detected GPU renderer, `software`
//!   when the live `renderer.allow_software` allowed a software one, `unverified` when
//!   detection was not possible. Nothing is ever silently `verified`.
//! - `list_sessions` caps a page at [`DEFAULT_LIST_LIMIT`] when `limit` is absent (04 leaves
//!   the default open; provisional and additive to change).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::config::{
    Authority, CoreConfig, Paths, RESTART_ONLY_KEYS, ScopeFile, ScopeSessions, Visibility,
    is_known_tool_name, key_spec, parse_value, read_token,
};
use crate::ipc::{
    ErrorCode, ErrorEnvelope, HandshakeResult, MIN_SUPPORTED, PROTOCOL_VERSION, Response, methods,
};
use crate::manager::{
    Backend, DrainState, Handler, InstanceMode, InstanceState, InstanceStatus, OPERATOR_SCOPE,
    Origin, Registry, Resolution, SessionRecord, SessionSpawner, SessionSpec, SessionState,
    VersionInfo, ViewRecord, ViewState,
};
use crate::strings;

/// `list_sessions` page size when the caller sends no `limit` (provisional; additive).
pub const DEFAULT_LIST_LIMIT: u64 = 100;

/// What the service needs to run: identity, its socket, the config, the path contract.
#[derive(Debug, Clone)]
pub struct ServiceOptions {
    pub instance_id: String,
    pub socket_path: PathBuf,
    pub mode: InstanceMode,
    pub config: CoreConfig,
    pub paths: Paths,
    /// The instance-token value this manager start adopts (07). The token file is
    /// restart-only, so `config reload` compares the file against this snapshot and
    /// reports drift instead of applying it (08).
    pub token_at_start: Option<String>,
}

/// The live dispatcher: registry + spawner + the wire shapes.
pub struct RuntimeService {
    registry: Mutex<Registry>,
    spawner: Arc<dyn SessionSpawner>,
    /// The media store the capture tools mint into and persist through (05 §Capture).
    media: Arc<crate::media::MediaStore>,
    /// The live config snapshot: `config reload` re-reads the file into this (08).
    config: RwLock<CoreConfig>,
    paths: Paths,
    socket_path: String,
    mode: InstanceMode,
    /// The token value adopted at start — see [`ServiceOptions::token_at_start`].
    token_at_start: Option<String>,
    /// Drain state (04/10): once entered, new sessions are refused until the manager stops.
    /// `drain` calls poll it, so entering is idempotent.
    draining: AtomicBool,
    /// Serializes creates end to end (quota → spawn → insert) so two racing creates can
    /// never both pass the quota check.
    create_lock: Mutex<()>,
    /// Live views: wayvnc + the manager-owned front (06 B2, 07).
    views: Arc<crate::runtime::views::ViewManager>,
    /// B9: per-session control-call buckets (07).
    rate: crate::runtime::rate::RateLimiter,
    /// One capture at a time per session (capture is exempt from the bucket, not free).
    capture_inflight: Mutex<std::collections::HashSet<String>>,
    /// Append-only session history (06 B8).
    history: Arc<crate::history::History>,
}

impl RuntimeService {
    pub fn new(
        options: ServiceOptions,
        spawner: Arc<dyn SessionSpawner>,
        media: Arc<crate::media::MediaStore>,
    ) -> Self {
        let history = Arc::new(crate::history::History::new(&options.paths));
        Self {
            registry: Mutex::new(Registry::new(options.instance_id)),
            spawner,
            media,
            config: RwLock::new(options.config),
            paths: options.paths,
            socket_path: options.socket_path.display().to_string(),
            mode: options.mode,
            token_at_start: options.token_at_start,
            draining: AtomicBool::new(false),
            create_lock: Mutex::new(()),
            views: Arc::new(crate::runtime::views::ViewManager::default()),
            rate: crate::runtime::rate::RateLimiter::default(),
            capture_inflight: Mutex::new(std::collections::HashSet::new()),
            history,
        }
    }

    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry.lock().expect("registry lock")
    }

    /// The live instance-status shape (05).
    fn instance_status(&self) -> Value {
        let registry = self.registry();
        let status = InstanceStatus {
            state: InstanceState::Running,
            mode: self.mode,
            pid: std::process::id(),
            socket: self.socket_path.clone(),
            versions: VersionInfo::current(),
            sessions: registry.counts(),
            drain: self.drain_state_locked(&registry),
            instance_id: registry.instance_id.clone(),
        };
        serde_json::to_value(&status).expect("instance status serializes")
    }

    /// The live drain state (04/10): `active` once drain has been entered, `remaining` the
    /// live sessions that must still exit before it completes.
    fn drain_state(&self) -> DrainState {
        self.drain_state_locked(&self.registry())
    }

    /// [`Self::drain_state`] against an already-held registry guard (the caller must not
    /// double-lock: `std::sync::Mutex` is not reentrant).
    fn drain_state_locked(&self, registry: &Registry) -> DrainState {
        let active = self.draining.load(Ordering::SeqCst);
        DrainState {
            active,
            remaining: if active { registry.counts().running } else { 0 },
        }
    }

    /// Enter drain (04/10). Idempotent — `--wait` polls this over one connection, and the
    /// manager keeps refusing new sessions until it stops. Answers with the same
    /// `{active, remaining}` shape the instance status carries (05).
    fn drain(&self) -> Result<Value, ErrorEnvelope> {
        // Serialize entry against an in-flight create (P2 hardening, round 6). The lock decides
        // the order: a create that already holds it finishes and is counted in `remaining`;
        // one that has not taken it yet re-checks drain state inside its own critical section
        // and is refused before spawning. Taking the flag without the lock left a window where
        // a spawn that had passed its own check landed after drain was entered — `remaining`
        // never counted it, and `drain --wait` could observe zero and complete while the
        // compositor chain was still starting.
        let _create_guard = self.create_lock.lock().expect("create lock");
        self.draining.store(true, Ordering::SeqCst);
        Ok(serde_json::to_value(self.drain_state()).expect("drain state serializes"))
    }

    // -----------------------------------------------------------------------
    // Lifecycle verbs
    // -----------------------------------------------------------------------

    fn create_session(
        &self,
        scope: &str,
        origin: Origin,
        params: Option<&Value>,
    ) -> Result<Value, ErrorEnvelope> {
        // 04/10: while draining, new sessions are refused before anything else — the manager
        // is on its way down and no spawn may start.
        let drain = self.drain_state();
        if drain.active {
            return Err(ErrorEnvelope::draining(drain.remaining));
        }
        let map = params_object(params)?;
        unknown_params(&map, &["backend", "resolution", "scale", "view", "label"])?;

        let backend = match string_field(&map, "backend")? {
            Some(raw) => Backend::parse(&raw)?,
            None => Backend::Headless,
        };
        let resolution = match string_field(&map, "resolution")? {
            Some(raw) => Resolution::parse(&raw)?.as_str(),
            None => Resolution::DEFAULT.as_str(),
        };
        let scale = match map.get("scale") {
            None => 1.0f64,
            Some(Value::Number(number)) => number
                .as_f64()
                .ok_or_else(|| ErrorEnvelope::bad_request("scale must be a finite number"))?,
            Some(other) => {
                return Err(ErrorEnvelope::bad_request(format!(
                    "scale must be a number, got {other}"
                )));
            }
        };
        if !scale.is_finite() || scale <= 0.0 {
            return Err(ErrorEnvelope::bad_request(format!(
                "invalid scale {scale}; expected a positive number"
            )));
        }
        let view = bool_field(&map, "view")?.unwrap_or(false);
        let label = string_field(&map, "label")?;

        if view {
            // Refused before anything spawns: a failed request must not leave a session behind.
            return Err(ErrorEnvelope::view_unavailable(
                label.as_deref().unwrap_or("new session"),
                "views are not available in this build yet (P4 wires the view provider)",
            ));
        }

        let spec = SessionSpec {
            backend,
            resolution: resolution.clone(),
            scale,
            view: false,
            label: label.clone(),
        };

        let _create_guard = self.create_lock.lock().expect("create lock");
        // The authoritative drain check (P2 hardening, round 6): the check above runs before the
        // create queue, so a caller that waited for this lock must re-read the flag that
        // `drain` set while it waited — otherwise it would start a spawn after drain was
        // entered, which the pinned contract forbids ("no spawn may start").
        let drain = self.drain_state();
        if drain.active {
            return Err(ErrorEnvelope::draining(drain.remaining));
        }
        let scope_max = self.scope_max(scope)?;
        let global_max = self.config.read().expect("config lock").sessions.max_global;
        let instance_id;
        {
            let registry = self.registry();
            let scope_live = registry
                .list()
                .iter()
                .filter(|record| record.scope == scope && record.state.is_live())
                .count() as u32;
            match scope_max {
                Some(max) => registry.quota_check(scope, scope_live, max, global_max)?,
                None => registry.quota_check(scope, scope_live, global_max, global_max)?,
            }
            instance_id = registry.instance_id.clone();
        }

        // The renderer policy value is read live (08: everything but `manager.bind`
        // re-reads on `config reload`), then the spawner detects and enforces (06 D6).
        let allow_software = self
            .config
            .read()
            .expect("config lock")
            .renderer
            .allow_software
            .is_on();
        let outcome = self.spawner.spawn(&spec, allow_software)?;
        let record = SessionRecord {
            id: outcome.id,
            label: label.unwrap_or_default(),
            state: SessionState::Running,
            backend,
            resolution,
            scale,
            scope: scope.to_string(),
            origin,
            idle_since: utc_now_rfc3339(),
            view: None,
            view_generation: 0,
            instance_id,
            // The creation-time mark the spawner detected (06 D6): `verified` / `software`
            // (override on) / `unverified` (not detectable — never guessed).
            renderer: outcome.renderer,
            cause: None,
        };
        self.record(
            "session.created",
            &record.id,
            &record.scope,
            record.origin,
            None,
            Some(json!({
                "backend": record.backend.as_str(),
                "resolution": record.resolution.clone(),
                "scale": record.scale,
                "view": record.view.is_some(),
            })),
        );
        let value = record.to_json();
        self.registry().insert(record);
        Ok(value)
    }

    fn list_sessions(&self, scope: &str, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let map = params_object(params)?;
        unknown_params(&map, &["limit", "cursor"])?;
        let limit = match map.get("limit") {
            None => DEFAULT_LIST_LIMIT,
            Some(Value::Number(number)) => match number.as_u64() {
                Some(0) => {
                    return Err(ErrorEnvelope::bad_request("limit must be at least 1"));
                }
                Some(limit) => limit,
                None => {
                    return Err(ErrorEnvelope::bad_request(format!(
                        "limit must be a positive integer, got {number}"
                    )));
                }
            },
            Some(other) => {
                return Err(ErrorEnvelope::bad_request(format!(
                    "limit must be a number, got {other}"
                )));
            }
        };
        let start = match map.get("cursor") {
            None => 0usize,
            Some(Value::String(raw)) => raw
                .parse::<usize>()
                .map_err(|_| ErrorEnvelope::bad_request(format!("invalid cursor {raw:?}")))?,
            Some(other) => {
                return Err(ErrorEnvelope::bad_request(format!(
                    "cursor must be a string, got {other}"
                )));
            }
        };

        let registry = self.registry();
        // 07: scopes constrain listing — `own` sees only its own sessions, `all` is
        // operator-wide. Visibility is read per call (scope files, 08 D4).
        let visibility = self.scope_definition(scope)?.sessions.visibility;
        let all: Vec<&SessionRecord> = registry
            .list()
            .into_iter()
            .filter(|record| visibility == Visibility::All || record.scope == scope)
            .collect();
        let start = start.min(all.len());
        let end = start.saturating_add(limit as usize).min(all.len());
        let sessions: Vec<Value> = all[start..end]
            .iter()
            .map(|record| record.to_json())
            .collect();
        let mut out = json!({ "sessions": sessions });
        if end < all.len() {
            out["next_cursor"] = json!(end.to_string());
        }
        Ok(out)
    }

    fn session_status(&self, scope: &str, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let id = required_session_id(params)?;
        let registry = self.registry();
        let record = registry.call_gate(&id)?;
        if !self.visible(scope, record)? {
            // 07: scopes constrain control; a session outside visibility does not exist for
            // this caller — existence is not leaked.
            return Err(ErrorEnvelope::session_not_found());
        }
        Ok(record.to_json())
    }

    fn close_session(&self, scope: &str, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let id = required_session_id(params)?;
        {
            let registry = self.registry();
            match registry.get(&id) {
                None => return Err(registry.tombstone_error(&id)),
                Some(record) if record.state == SessionState::Closed => {
                    return Err(ErrorEnvelope::session_closed());
                }
                Some(record) => {
                    if !self.visible(scope, record)? {
                        return Err(ErrorEnvelope::session_not_found());
                    }
                }
            }
        }
        // Completion guarantee: the tree dies before the entry is removed. A session the
        // spawner no longer tracks has nothing left to kill; that is not an error here.
        match self.spawner.kill_tree(&id) {
            Ok(()) => {}
            Err(err) if err.code == ErrorCode::SessionNotFound => {}
            Err(err) => return Err(err),
        }
        let mut registry = self.registry();
        let _ = registry.close(&id)?;
        drop(registry);
        // 08: persisted media keeps the *same retention as history* — 7 days from session end
        // (04). Closing a session therefore leaves `<data>/media/<session>/` in place; the
        // shared retention sweep removes it with the session's history, not before.
        // A view never outlives its session (06 B2: explicit kill-on-session-close).
        self.views.stop(&id);
        let (scope_of, origin_of) = {
            let registry = self.registry();
            match registry.get(&id) {
                Some(record) => (record.scope.clone(), record.origin),
                None => (OPERATOR_SCOPE.to_string(), Origin::Shell),
            }
        };
        self.record(
            "session.closed",
            &id,
            &scope_of,
            origin_of,
            Some("closed"),
            None,
        );
        // The retention sweep's input: a durable "ended at" marker, never inferred from mtimes.
        self.history
            .mark_ended(&id, &scope_of, origin_of.as_str(), "closed");
        Ok(json!({ "id": id, "state": "closed" }))
    }

    // -----------------------------------------------------------------------
    // Retention (04/08)
    // -----------------------------------------------------------------------

    /// One retention pass under the live config window. Liveness comes from the registry, so
    /// a session this manager still knows is never swept even if a marker exists.
    pub fn sweep_retention(&self) -> crate::runtime::retention::SweepReport {
        let retention = self.config.read().expect("config lock").history.retention;
        let live = |id: &str| self.registry().get(id).is_some();
        crate::runtime::retention::sweep(
            &self.paths.history_dir(),
            &self.paths.media_dir(),
            &live,
            retention.secs,
            crate::runtime::retention::now_secs(),
        )
    }

    /// The startup pass. A manager start owns no sessions, so a history directory without a
    /// parsable `.ended` can only be a leftover from a previous manager (a crash, or a
    /// shutdown that killed its sessions): stamp it `cause=manager_restart` so its window
    /// starts here instead of never — adoption keeps the directory, it does not delete it —
    /// then sweep.
    pub fn startup_retention(&self) -> Vec<String> {
        let root = self.paths.history_dir();
        let mut adopted = 0;
        for id in crate::runtime::retention::session_dirs(&root) {
            if crate::runtime::retention::marker_secs(&root, &id).is_some() {
                continue;
            }
            let scope = self
                .history
                .recorded_scope(&id)
                .unwrap_or_else(|| "unknown".to_string());
            self.history
                .mark_ended(&id, &scope, "startup", "manager_restart");
            adopted += 1;
        }
        let report = self.sweep_retention();
        let mut lines = Vec::new();
        if !report.removed.is_empty() {
            lines.push(format!(
                "retention sweep: {} expired session(s) removed",
                report.removed.len()
            ));
        }
        if adopted > 0 {
            lines.push(format!(
                "retention sweep: {adopted} unmarked session(s) adopted (cause=manager_restart)"
            ));
        }
        if !report.failed.is_empty() {
            lines.push(format!(
                "retention sweep: {} session(s) could not be removed",
                report.failed.len()
            ));
        }
        lines
    }

    /// One pass on the blocking pool — the hourly task's step, so the scan never occupies a
    /// reactor worker.
    pub async fn sweep_retention_off_reactor(
        self: &Arc<Self>,
    ) -> crate::runtime::retention::SweepReport {
        let service = Arc::clone(self);
        tokio::task::spawn_blocking(move || service.sweep_retention())
            .await
            .unwrap_or_default()
    }

    /// The hourly pass a running manager keeps (08): the window is days, the scan is one
    /// directory read, and the window is re-read every tick so `config reload` applies.
    pub fn spawn_retention_sweeper(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        self.spawn_retention_sweeper_every(crate::runtime::retention::SWEEP_INTERVAL)
    }

    /// [`Self::spawn_retention_sweeper`] with an injectable cadence (tests drive a short one;
    /// the production wrapper owns the pinned hour).
    pub fn spawn_retention_sweeper_every(
        self: &Arc<Self>,
        every: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        let service = Arc::clone(self);
        // The ticker is created before the spawn so its schedule starts now, not at the
        // task's first poll (a late poll must not shift every later tick).
        let mut ticker = tokio::time::interval(every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tokio::spawn(async move {
            // The first tick is immediate; startup already ran the pass.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let report = Arc::clone(&service).sweep_retention_off_reactor().await;
                if !report.removed.is_empty() {
                    eprintln!(
                        "retention sweep: {} expired session(s) removed",
                        report.removed.len()
                    );
                }
                if !report.failed.is_empty() {
                    eprintln!(
                        "retention sweep: {} session(s) could not be removed",
                        report.failed.len()
                    );
                }
            }
        })
    }

    /// Append one history record (06 B8): scope and origin always, cause where relevant,
    /// never control-call arguments, media ids or credentials.
    fn record(
        &self,
        event: &str,
        session_id: &str,
        scope: &str,
        origin: Origin,
        cause: Option<&str>,
        detail: Option<Value>,
    ) {
        let record = crate::history::Record {
            at: utc_now_rfc3339(),
            event: event.to_string(),
            session_id: session_id.to_string(),
            scope: scope.to_string(),
            origin: origin.as_str().to_string(),
            cause: cause.map(str::to_string),
            detail,
        };
        let _ = self.history.append(&record);
    }

    /// `list_history` (05/06 B8): records follow **session** visibility — own-scope default,
    /// operator sees all, a stranger's session is the same `session_not_found` as everywhere.
    /// A cursor is opaque and only valid with a `session_id` (cross-session paging is not
    /// offered; ask per session).
    fn list_history(&self, scope: &str, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let map = params_object(params)?;
        unknown_params(&map, &["session_id", "scope", "limit", "cursor"])?;
        let limit = map
            .get("limit")
            .and_then(Value::as_u64)
            .map(|value| (value as usize).clamp(1, crate::history::MAX_LIMIT))
            .unwrap_or(crate::history::DEFAULT_LIMIT);
        let cursor = map.get("cursor").and_then(Value::as_str);
        if let Some(requested) = map.get("scope").and_then(Value::as_str)
            && scope != OPERATOR_SCOPE
            && requested != scope
        {
            return Err(ErrorEnvelope::bad_request(
                "a scope filter may only name the caller's own scope",
            ));
        }
        let session = map.get("session_id").and_then(Value::as_str);
        let Some(session_id) = session else {
            if cursor.is_some() {
                return Err(ErrorEnvelope::bad_request("a cursor requires a session_id"));
            }
            let records: Vec<crate::history::Record> = self
                .history
                .read_all(limit)
                .into_iter()
                .filter(|record| scope == OPERATOR_SCOPE || record.scope == scope)
                .collect();
            return Ok(json!({ "records": records, "next_cursor": Value::Null }));
        };
        let visible = {
            let registry = self.registry();
            match registry.get(session_id) {
                Some(record) => self.visible(scope, record)?,
                None => match self.history.recorded_scope(session_id) {
                    Some(recorded) => scope == OPERATOR_SCOPE || recorded == scope,
                    None => false,
                },
            }
        };
        if !visible {
            return Err(ErrorEnvelope::session_not_found());
        }
        let page = self.history.read_session(session_id, limit, cursor)?;
        Ok(json!({ "records": page.records, "next_cursor": page.next_cursor }))
    }

    // -----------------------------------------------------------------------
    // Views (06 B2, 07)
    // -----------------------------------------------------------------------

    /// The instance token the view credential derives from (07 A5): the one adopted at start,
    /// the file, or — for a loopback-only instance that never needed one — a fresh generated
    /// token written 0600 (the same rule as a first non-loopback bind).
    fn instance_token(&self) -> Result<String, ErrorEnvelope> {
        if let Some(token) = &self.token_at_start {
            return Ok(token.clone());
        }
        if let Some(token) = crate::config::read_token(&self.paths)? {
            return Ok(token);
        }
        let token = crate::config::generate_token();
        crate::config::write_token(&self.paths, &token)?;
        Ok(token)
    }

    /// `get_view` (05): idempotent — a running view answers the same address and generation;
    /// otherwise one start under the registry lock. The credential is minted here and never
    /// stored in the session record (07: status/detail never echo it).
    fn get_view(&self, scope: &str, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let (id, _record) = self.capture_target(scope, params, &["session_id"])?;
        let config = self.config.read().expect("config lock").clone();
        let token = self.instance_token()?;
        if let Some((front_port, _vnc_port, generation)) = self.views.running(&id) {
            return Ok(view_answer(
                &id,
                front_port,
                generation,
                &token,
                config.view.advertise_base.as_deref(),
            ));
        }
        let env = self
            .spawner
            .capture_env(&id)
            .ok_or_else(ErrorEnvelope::session_dead)?;
        let generation = self.registry().view_generation(&id) + 1;
        let credential = crate::runtime::views::view_credential(&token, &id, generation);
        let started = self.views.start(
            &id,
            generation,
            &credential,
            &env,
            crate::runtime::views::ViewLaunch {
                argv: &config.view.wayvnc_command,
                port_base: config.view.port_base,
                assets: &self.paths.novnc_dir(),
                advertise_base: config.view.advertise_base.as_deref(),
            },
        )?;
        {
            let mut registry = self.registry();
            registry.set_view(
                &id,
                Some(ViewRecord {
                    state: ViewState::Running,
                    // The public base only: the credential rides the get_view answer, never
                    // the session object (07).
                    address: Some(crate::runtime::views::view_base(
                        config.view.advertise_base.as_deref(),
                        started.front_port,
                    )),
                    front_port: started.front_port,
                    vnc_port: started.vnc_port,
                    generation: started.generation,
                }),
                started.generation,
            );
        }
        self.record(
            "view.started",
            &id,
            scope,
            Origin::Shell,
            None,
            Some(json!({ "generation": started.generation })),
        );
        Ok(view_answer(
            &id,
            started.front_port,
            started.generation,
            &token,
            config.view.advertise_base.as_deref(),
        ))
    }

    /// `stop_view` (05): stops only the view; the session is untouched. Idempotent.
    fn stop_view(&self, scope: &str, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let (id, _record) = self.capture_target(scope, params, &["session_id"])?;
        self.views.stop(&id);
        let (generation, scope_of, origin_of) = {
            let mut registry = self.registry();
            let generation = registry.view_generation(&id);
            let (scope_of, origin_of) = match registry.get(&id) {
                Some(record) => (record.scope.clone(), record.origin),
                None => (scope.to_string(), Origin::Shell),
            };
            registry.set_view(&id, None, generation);
            (generation, scope_of, origin_of)
        };
        self.record(
            "view.stopped",
            &id,
            &scope_of,
            origin_of,
            None,
            Some(json!({ "generation": generation })),
        );
        Ok(json!({ "session_id": id, "state": "stopped" }))
    }

    // -----------------------------------------------------------------------
    // Capture (05 §Capture)
    // -----------------------------------------------------------------------

    /// Claim the session's single capture slot; `None` when a capture is already in flight.
    fn begin_capture(&self, id: &str) -> Option<CaptureSlot<'_>> {
        let mut inflight = self.capture_inflight.lock().expect("capture lock");
        if !inflight.insert(id.to_string()) {
            return None;
        }
        drop(inflight);
        Some(CaptureSlot {
            set: &self.capture_inflight,
            id: id.to_string(),
        })
    }

    /// Resolve a capture target exactly like `session_status`: tombstone-gated, visibility
    /// checked (a stranger's id is `session_not_found`, never existence), and only then does
    /// the caller get to look at anything.
    fn capture_target(
        &self,
        scope: &str,
        params: Option<&Value>,
        allowed: &[&str],
    ) -> Result<(String, Value), ErrorEnvelope> {
        let map = params_object(params)?;
        unknown_params(&map, allowed)?;
        let id = match map.get("session_id") {
            Some(Value::String(id)) if !id.is_empty() => id.clone(),
            Some(Value::String(_)) => {
                return Err(ErrorEnvelope::bad_request("session_id must not be empty"));
            }
            Some(other) => {
                return Err(ErrorEnvelope::bad_request(format!(
                    "session_id must be a string, got {other}"
                )));
            }
            None => return Err(ErrorEnvelope::bad_request("session_id is required")),
        };
        let registry = self.registry();
        let record = registry.call_gate(&id)?;
        if !self.visible(scope, record)? {
            return Err(ErrorEnvelope::session_not_found());
        }
        Ok((id, record.to_json()))
    }

    /// `screenshot` -> an image payload the bridge turns into an MCP `image` block. The raw
    /// PNG is size-checked **before** base64 (05 §Capture: the frame bound cannot be blown by
    /// a big capture).
    fn screenshot(&self, scope: &str, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let (id, _record) = self.capture_target(scope, params, &["session_id"])?;
        let Some(_slot) = self.begin_capture(&id) else {
            return Err(ErrorEnvelope::bad_request(
                crate::strings::CAPTURE_IN_FLIGHT,
            ));
        };
        let config = self.config.read().expect("config lock").clone();
        let env = self
            .spawner
            .capture_env(&id)
            .ok_or_else(ErrorEnvelope::session_dead)?;
        let timeout = std::time::Duration::from_secs(config.capture.timeout_s);
        let (bytes, width, height) = crate::runtime::capture::screenshot(
            &env,
            &config.capture.screenshot_command,
            timeout,
            config.capture.screenshot_max_bytes,
        )?;
        self.record(
            "capture.screenshot",
            &id,
            scope,
            Origin::Shell,
            None,
            Some(json!({ "bytes": bytes.len(), "width": width, "height": height })),
        );
        if self.scope_definition(scope)?.audit.store_screenshots {
            // URL-less: the bytes are kept for the retention window, never a capability.
            self.media.persist_screenshot(&id, &bytes);
        }
        Ok(json!({
            "capture": "image",
            "mimeType": "image/png",
            "data_b64": crate::runtime::capture::base64_encode(&bytes),
            "bytes": bytes.len(),
            "width": width,
            "height": height,
        }))
    }

    /// `clip_video` -> a minted media id the bridge turns into a `resource_link`.
    fn clip_video(&self, scope: &str, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let (id, _record) = self.capture_target(scope, params, &["session_id", "duration_s"])?;
        let Some(_slot) = self.begin_capture(&id) else {
            return Err(ErrorEnvelope::bad_request(
                crate::strings::CAPTURE_IN_FLIGHT,
            ));
        };
        let duration_s = match params_object(params)?.get("duration_s") {
            None | Some(Value::Null) => 5,
            Some(value) => value
                .as_u64()
                .filter(|seconds| (1..=30).contains(seconds))
                .ok_or_else(|| ErrorEnvelope::bad_request("duration_s must be between 1 and 30"))?,
        };
        let config = self.config.read().expect("config lock").clone();
        let env = self
            .spawner
            .capture_env(&id)
            .ok_or_else(ErrorEnvelope::session_dead)?;
        let timeout = std::time::Duration::from_secs(config.capture.timeout_s);
        let bytes = crate::runtime::capture::clip(
            &env,
            &config.capture.clip_command,
            timeout,
            std::time::Duration::from_secs(duration_s),
            crate::media::SESSION_CAP_BYTES,
        )?;
        self.record(
            "capture.clip",
            &id,
            scope,
            Origin::Shell,
            None,
            Some(json!({ "bytes": bytes.len(), "duration_s": duration_s })),
        );
        let store_clips = self.scope_definition(scope)?.audit.store_clips;
        let media_id = self
            .media
            .mint(&id, "video/webm", bytes.clone(), store_clips);
        Ok(json!({
            "capture": "media",
            "media_id": media_id,
            "mimeType": "video/webm",
            "bytes": bytes.len(),
            "duration_s": duration_s,
            "expires_in_s": config.media.ttl.secs,
        }))
    }

    // -----------------------------------------------------------------------
    // Config (08 reload semantics)
    // -----------------------------------------------------------------------

    /// The config values this manager is using right now — the snapshot a reload replaces
    /// (08: a local `config set` writes the file; a running manager keeps its value until
    /// `config reload` or restart).
    pub fn active_config(&self) -> CoreConfig {
        self.config.read().expect("config lock").clone()
    }

    /// `config reload` (08): re-read the core config file into the live snapshot.
    ///
    /// - The file is re-read strictly: an unreadable or invalid file refuses the reload and
    ///   nothing applies — a half-applied config is worse than a stale one.
    /// - Non-restart-only keys re-read cleanly: changed values are applied to the running
    ///   manager and named in `reloaded`.
    /// - Restart-only items (listener bind addresses, the token — 08) are *not* applied
    ///   while the manager runs: when the file has moved them, reload names them in
    ///   `restart_only` and the running value stays in place.
    fn config_reload(&self) -> Result<Value, ErrorEnvelope> {
        let fresh = CoreConfig::load(&self.paths.core_config())?;
        let mut running = self.config.write().expect("config lock");
        let mut reloaded: Vec<&str> = Vec::new();
        let mut restart_only: Vec<&str> = Vec::new();
        for spec in crate::config::KEYS {
            if spec.authority != Authority::ManagerSide {
                continue;
            }
            let old = running.get(spec.path)?;
            let new = fresh.get(spec.path)?;
            if old == new {
                continue;
            }
            if RESTART_ONLY_KEYS.contains(&spec.path) {
                restart_only.push(spec.path);
            } else {
                running.set(spec.path, new)?;
                reloaded.push(spec.path);
            }
        }
        // The token is restart-only too (07) but not a config key: it is dirty when the
        // file no longer holds the value this start adopted.
        if read_token(&self.paths)? != self.token_at_start {
            restart_only.push("token");
        }
        Ok(json!({ "reloaded": reloaded, "restart_only": restart_only }))
    }

    /// `config get` (08 A2): reads this instance's core config file — the same source the
    /// local cold path reads, so `--remote` and on-box answers agree. The live snapshot is
    /// not the read source; moving it is `config reload`'s job. Client-local keys do not
    /// exist on a manager: refused, never answered.
    fn config_get(&self, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let map = params_object(params)?;
        unknown_params(&map, &["key"])?;
        let key = string_field(&map, "key")?
            .ok_or_else(|| ErrorEnvelope::bad_request("key is required"))?;
        let spec = key_spec(&key)?;
        if spec.authority != Authority::ManagerSide {
            return Err(ErrorEnvelope::bad_request(format!(
                "key {key:?} is client-local; it does not exist on a remote manager"
            )));
        }
        let config = CoreConfig::load_or_default(&self.paths.core_config())?;
        let value = config.get(&key)?;
        Ok(json!({
            "key": key,
            "value": value.to_grammar(),
            "authority": "manager_side",
        }))
    }

    /// `config set` (08 A2): writes this instance's core config file — the file the local
    /// path writes and the next `config reload`/restart reads. The live snapshot is
    /// deliberately untouched: a running manager keeps its value until then (08), so the
    /// caller's output states that instead of pretending the change is live.
    fn config_set(&self, params: Option<&Value>) -> Result<Value, ErrorEnvelope> {
        let map = params_object(params)?;
        unknown_params(&map, &["key", "value"])?;
        let key = string_field(&map, "key")?
            .ok_or_else(|| ErrorEnvelope::bad_request("key is required"))?;
        let raw = string_field(&map, "value")?
            .ok_or_else(|| ErrorEnvelope::bad_request("value is required"))?;
        let spec = key_spec(&key)?;
        if spec.authority != Authority::ManagerSide {
            return Err(ErrorEnvelope::bad_request(format!(
                "key {key:?} is client-local; it does not exist on a remote manager"
            )));
        }
        let value = parse_value(&key, &raw)?;
        crate::config::update_config(&self.paths.core_config(), |config| {
            config.set(&key, value.clone())
        })?;
        Ok(json!({ "key": key, "value": value.to_grammar() }))
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// The caller scope's definition, read per call (08 D4). A missing file means the pinned
    /// defaults: `operator` reads operator-wide (07), every other scope is own-sessions-only
    /// with no quota override. An unreadable or invalid file is a load error, surfaced as
    /// itself.
    fn scope_definition(&self, scope: &str) -> Result<ScopeFile, ErrorEnvelope> {
        let path = self.paths.scope_file(scope);
        if !path.exists() {
            return Ok(ScopeFile {
                sessions: ScopeSessions {
                    max: None,
                    visibility: if scope == OPERATOR_SCOPE {
                        Visibility::All
                    } else {
                        Visibility::Own
                    },
                },
                ..ScopeFile::default()
            });
        }
        ScopeFile::load(&path)
    }

    /// The per-scope session quota (08 D4: read per call).
    fn scope_max(&self, scope: &str) -> Result<Option<u32>, ErrorEnvelope> {
        Ok(self.scope_definition(scope)?.sessions.max)
    }

    /// Is this session inside the caller's visibility? (07: scopes constrain listing and
    /// control; a session outside it does not exist for that caller.)
    fn visible(&self, scope: &str, record: &SessionRecord) -> Result<bool, ErrorEnvelope> {
        Ok(
            self.scope_definition(scope)?.sessions.visibility == Visibility::All
                || record.scope == scope,
        )
    }
}

impl Handler for RuntimeService {
    fn handshake_result(&self) -> HandshakeResult {
        HandshakeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            min_supported: Some(MIN_SUPPORTED.to_string()),
            instance_id: self.registry().instance_id.clone(),
        }
    }

    /// 07: the instance token this start adopted is the transport gate over TCP. A missing
    /// token on the manager side (no network bind ever configured) refuses every network
    /// caller — never "no token configured, so anything goes".
    fn authorize_instance(
        &self,
        token: Option<&str>,
        authority: &str,
    ) -> Result<(), ErrorEnvelope> {
        let expected = self
            .token_at_start
            .as_deref()
            .ok_or_else(|| ErrorEnvelope::token_required(authority))?;
        if !crate::config::secret_matches(token.unwrap_or_default(), expected) {
            return Err(ErrorEnvelope::token_required(authority));
        }
        Ok(())
    }

    /// 07: scope identity over a network surface is authenticated per request; the scope's
    /// secret file is the credential (a claimed string is never authentication). Names are
    /// validated before any path join, so a frame cannot steer the read.
    fn authorize_scope(
        &self,
        token: Option<&str>,
        scope: Option<&str>,
        scope_token: Option<&str>,
        authority: &str,
    ) -> Result<(), ErrorEnvelope> {
        self.authorize_instance(token, authority)?;
        let scope = scope.filter(|value| !value.is_empty()).ok_or_else(|| {
            ErrorEnvelope::new(ErrorCode::ScopeAuthRequired, strings::FRAME_SCOPE_REQUIRED)
        })?;
        if !crate::config::valid_scope_name(scope) {
            return Err(ErrorEnvelope::bad_request(strings::INVALID_SCOPE_NAME));
        }
        // A missing secret is "present your credentials", not "your credentials are wrong":
        // the two get different pinned strings so a caller can tell which fix applies.
        let Some(presented) = scope_token.filter(|value| !value.is_empty()) else {
            return Err(ErrorEnvelope::new(
                ErrorCode::ScopeAuthRequired,
                strings::FRAME_SCOPE_REQUIRED,
            ));
        };
        let secret = crate::config::read_scope_token(&self.paths, scope)?.ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::ScopeAuthRequired,
                strings::scope_has_no_secret(scope),
            )
            .with_hint("create the scope secret file on the manager host")
        })?;
        if !crate::config::secret_matches(presented, &secret) {
            return Err(ErrorEnvelope::new(
                ErrorCode::ScopeAuthRequired,
                strings::frame_scope_auth_required(scope),
            ));
        }
        Ok(())
    }

    fn dispatch(
        &self,
        scope: &str,
        origin: Origin,
        method: &str,
        params: Option<&Value>,
    ) -> Response {
        // B9: control-class calls spend a token before anything else happens. Reads, capture
        // and lifecycle are exempt (07); the bucket is per (scope, session).
        if is_control_tool(method)
            && let Some(session_id) = params
                .and_then(|value| value.get("session_id"))
                .and_then(Value::as_str)
        {
            let rate = self
                .scope_definition(scope)
                .map(|definition| definition.limits.rate)
                .unwrap_or_default();
            if let Err(envelope) = self.rate.acquire(scope, session_id, &rate) {
                return Response::Error(envelope);
            }
            // B8: the call is recorded, its arguments are not (06).
            self.record(
                "tool.call",
                session_id,
                scope,
                origin,
                None,
                Some(json!({ "tool": method })),
            );
        }
        let outcome: Result<Value, ErrorEnvelope> = match method {
            // Instance reads and the two management verbs take no parameters: a stray key is a
            // malformed request (05), refused like any other unknown parameter — silently
            // accepting one answers the wrong question under a typo'd call.
            methods::STATUS | methods::READINESS => {
                no_params(params).map(|()| self.instance_status())
            }
            methods::CREATE_SESSION => self.create_session(scope, origin, params),
            methods::LIST_SESSIONS => self.list_sessions(scope, params),
            methods::SESSION_STATUS => self.session_status(scope, params),
            methods::CLOSE_SESSION => self.close_session(scope, params),
            methods::CONFIG_RELOAD => no_params(params).and_then(|()| self.config_reload()),
            methods::CONFIG_GET => self.config_get(params),
            methods::CONFIG_SET => self.config_set(params),
            methods::DRAIN => no_params(params).and_then(|()| self.drain()),
            // Capture (05 §Capture): the observe-then-act loop's read side.
            methods::SCREENSHOT => self.screenshot(scope, params),
            methods::CLIP_VIDEO => self.clip_video(scope, params),
            // Pinned-surface names this build does not implement yet: the views/query methods
            // and the control tools land later (PLAN.md) — a valid call, an absent build.
            methods::GET_VIEW => self.get_view(scope, params),
            methods::STOP_VIEW => self.stop_view(scope, params),
            methods::LIST_HISTORY => self.list_history(scope, params),
            methods::DAEMON_END => Err(ErrorEnvelope::not_implemented()),
            name if is_known_tool_name(name) => Err(ErrorEnvelope::not_implemented()),
            // Anything else is a name the protocol does not define: a client-side refusal.
            // Dispatch-level — the connection continues (P2 hardening; fixtures/README).
            _ => Err(ErrorEnvelope::unknown_method(method)),
        };
        match outcome {
            Ok(value) => Response::Result(value),
            Err(envelope) => Response::Error(envelope),
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter helpers
// ---------------------------------------------------------------------------

fn params_object(params: Option<&Value>) -> Result<Map<String, Value>, ErrorEnvelope> {
    match params {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map.clone()),
        Some(other) => Err(ErrorEnvelope::bad_request(format!(
            "params must be an object or null, got {other}"
        ))),
    }
}

/// The `get_view` answer (05): the front URL carrying the 07 credential as a query parameter,
/// plus the credential as its own field. Both are request-scoped; neither is ever recorded.
fn view_answer(
    session_id: &str,
    front_port: u16,
    generation: u64,
    token: &str,
    advertise_base: Option<&str>,
) -> Value {
    let credential = crate::runtime::views::view_credential(token, session_id, generation);
    let base = crate::runtime::views::view_base(advertise_base, front_port);
    json!({
        "session_id": session_id,
        "address": format!("{base}/?credential={credential}"),
        "credential": credential,
        "generation": generation,
    })
}

/// The control class (07 B9): the nine input tools are rate-limited; reads, capture and
/// lifecycle are exempt.
fn is_control_tool(method: &str) -> bool {
    matches!(
        method,
        "pointer_move"
            | "pointer_scroll"
            | "pointer_click"
            | "pointer_button_down"
            | "pointer_button_up"
            | "key_tap"
            | "key_down"
            | "key_up"
            | "type_text"
    )
}

/// Releases the session's capture slot on every exit path.
struct CaptureSlot<'a> {
    set: &'a Mutex<std::collections::HashSet<String>>,
    id: String,
}

impl Drop for CaptureSlot<'_> {
    fn drop(&mut self) {
        if let Ok(mut set) = self.set.lock() {
            set.remove(&self.id);
        }
    }
}

/// A method whose pinned surface takes no parameters: `null`, absent and `{}` are all "no
/// parameters"; anything else is a malformed request (05), refused like an unknown parameter.
fn no_params(params: Option<&Value>) -> Result<(), ErrorEnvelope> {
    let map = params_object(params)?;
    match map.keys().next() {
        None => Ok(()),
        Some(key) => Err(ErrorEnvelope::bad_request(
            crate::strings::takes_no_parameters(key),
        )),
    }
}

fn unknown_params(map: &Map<String, Value>, valid: &[&str]) -> Result<(), ErrorEnvelope> {
    for key in map.keys() {
        if !valid.contains(&key.as_str()) {
            return Err(ErrorEnvelope::bad_request(format!(
                "unknown parameter {key:?}; valid: {}",
                valid.join(", ")
            )));
        }
    }
    Ok(())
}

fn string_field(map: &Map<String, Value>, key: &str) -> Result<Option<String>, ErrorEnvelope> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(ErrorEnvelope::bad_request(format!(
            "{key} must be a string, got {other}"
        ))),
    }
}

fn bool_field(map: &Map<String, Value>, key: &str) -> Result<Option<bool>, ErrorEnvelope> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(ErrorEnvelope::bad_request(format!(
            "{key} must be a boolean, got {other}"
        ))),
    }
}

fn required_session_id(params: Option<&Value>) -> Result<String, ErrorEnvelope> {
    let map = params_object(params)?;
    unknown_params(&map, &["session_id"])?;
    match map.get("session_id") {
        Some(Value::String(id)) if !id.is_empty() => Ok(id.clone()),
        Some(Value::String(_)) => Err(ErrorEnvelope::bad_request("session_id must not be empty")),
        Some(other) => Err(ErrorEnvelope::bad_request(format!(
            "session_id must be a string, got {other}"
        ))),
        None => Err(ErrorEnvelope::bad_request("session_id is required")),
    }
}

// ---------------------------------------------------------------------------
// Timestamps (RFC 3339 UTC, dependency-free)
// ---------------------------------------------------------------------------

/// The current UTC time as `YYYY-MM-DDTHH:MM:SSZ` (the pinned `idle_since` form).
pub fn utc_now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    rfc3339_from_unix_secs(secs)
}

/// Days-to-civil (Howard Hinnant's algorithm) — correct for the full range; no dependencies.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn rfc3339_from_unix_secs(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

// ---------------------------------------------------------------------------
// Tests: stub spawner, the whole verb surface, and one real-socket end to end
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigValue, Paths, StartMode, Toggle};
    use crate::ipc::{Address, Client};
    use crate::manager::{Listener, RendererMark, SpawnOutcome};
    use crate::runtime::spawner::{LinuxSpawner, SpawnerConfig};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration as StdDuration;

    struct TestSpawner {
        spawned: Mutex<Vec<SessionSpec>>,
        killed: Mutex<Vec<String>>,
        next_id: AtomicU64,
        fail_next: Mutex<Option<ErrorEnvelope>>,
        /// The creation-time mark every spawn reports (06 D6); `Unverified` by default —
        /// nothing is ever silently `verified`.
        renderer: RendererMark,
        /// What each spawn was told about the live `renderer.allow_software`.
        allow_software_seen: Mutex<Vec<bool>>,
        /// The capture environment `capture_env` answers for any id (tests set one).
        capture_env: Mutex<Option<crate::manager::CaptureEnv>>,
    }

    impl Default for TestSpawner {
        fn default() -> Self {
            Self {
                spawned: Mutex::new(Vec::new()),
                killed: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(0),
                fail_next: Mutex::new(None),
                renderer: RendererMark::Unverified,
                allow_software_seen: Mutex::new(Vec::new()),
                capture_env: Mutex::new(None),
            }
        }
    }

    impl SessionSpawner for TestSpawner {
        fn spawn(
            &self,
            spec: &SessionSpec,
            allow_software: bool,
        ) -> Result<SpawnOutcome, ErrorEnvelope> {
            if let Some(err) = self.fail_next.lock().expect("fail slot").take() {
                return Err(err);
            }
            self.spawned.lock().expect("spawned").push(spec.clone());
            self.allow_software_seen
                .lock()
                .expect("allow_software_seen")
                .push(allow_software);
            let n = self.next_id.fetch_add(1, Ordering::SeqCst);
            Ok(SpawnOutcome {
                id: format!("2f7a4c1e-9b3d-4a6f-8e21-{:012}", n + 1),
                renderer: self.renderer,
            })
        }

        fn kill_tree(&self, id: &str) -> Result<(), ErrorEnvelope> {
            self.killed.lock().expect("killed").push(id.to_string());
            Ok(())
        }

        fn capture_env(&self, _id: &str) -> Option<crate::manager::CaptureEnv> {
            self.capture_env.lock().expect("capture_env").clone()
        }
    }

    /// A spawner whose `spawn` reports the call and then blocks until the test releases it —
    /// the only way to hold a real create inside its critical section while another thread
    /// reaches `drain` (P2 hardening, round 6).
    struct BlockingSpawner {
        entered: std::sync::mpsc::Sender<String>,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        calls: AtomicU64,
    }

    impl BlockingSpawner {
        fn new(
            entered: std::sync::mpsc::Sender<String>,
            release: std::sync::mpsc::Receiver<()>,
        ) -> Self {
            Self {
                entered,
                release: Mutex::new(Some(release)),
                calls: AtomicU64::new(0),
            }
        }
    }

    impl SessionSpawner for BlockingSpawner {
        fn capture_env(&self, _session_id: &str) -> Option<crate::manager::CaptureEnv> {
            None
        }

        fn spawn(
            &self,
            spec: &SessionSpec,
            _allow_software: bool,
        ) -> Result<SpawnOutcome, ErrorEnvelope> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let _ = self
                .entered
                .send(format!("{}@{}", spec.resolution, spec.scale));
            let receiver = self.release.lock().expect("release lock").take();
            if let Some(rx) = receiver {
                let _ = rx.recv();
            }
            Ok(SpawnOutcome {
                id: format!("5c1d2e3f-1111-4222-8333-{:012}", n + 1),
                renderer: RendererMark::Unverified,
            })
        }

        fn kill_tree(&self, _id: &str) -> Result<(), ErrorEnvelope> {
            Ok(())
        }
    }

    fn make_service_with(
        dir: &std::path::Path,
        spawner: Arc<dyn SessionSpawner>,
    ) -> RuntimeService {
        make_service_with_config(dir, spawner, CoreConfig::default(), None)
    }

    fn make_service_with_config(
        dir: &std::path::Path,
        spawner: Arc<dyn SessionSpawner>,
        config: CoreConfig,
        token_at_start: Option<String>,
    ) -> RuntimeService {
        let paths = Paths::for_env(Some(dir), Some(dir), Some(dir), "default");
        let media = Arc::new(crate::media::MediaStore::with_ttl(
            paths.media_dir(),
            crate::media::DEFAULT_TTL,
        ));
        RuntimeService::new(
            ServiceOptions {
                instance_id: "11111111-1111-4111-8111-111111111111".to_string(),
                socket_path: dir.join("manager.sock"),
                mode: InstanceMode::Attached,
                config,
                paths,
                token_at_start,
            },
            spawner,
            media,
        )
    }

    fn make_service(dir: &std::path::Path, spawner: Arc<TestSpawner>) -> RuntimeService {
        make_service_with(dir, spawner)
    }

    /// `make_service_with_config` for tests that keep a typed handle on the spawner for
    /// assertions after construction — the coercion happens here, where the parameter type is
    /// concrete (`Arc<TestSpawner>` → `Arc<dyn SessionSpawner>`).
    fn make_typed_service_with_config(
        dir: &std::path::Path,
        spawner: Arc<TestSpawner>,
        config: CoreConfig,
    ) -> RuntimeService {
        make_service_with_config(dir, spawner, config, None)
    }

    fn result(
        service: &RuntimeService,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, ErrorEnvelope> {
        result_as("operator", Origin::Shell, service, method, params)
    }

    /// One dispatch under an explicit caller identity (scope carriage, 05/07).
    fn result_as(
        scope: &str,
        origin: Origin,
        service: &RuntimeService,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, ErrorEnvelope> {
        match service.dispatch(scope, origin, method, params.as_ref()) {
            Response::Result(value) => Ok(value),
            Response::Error(envelope) => Err(envelope),
        }
    }

    fn create_ok(service: &RuntimeService, params: Option<Value>) -> Value {
        result(service, methods::CREATE_SESSION, params).expect("create succeeds")
    }

    #[test]
    fn create_returns_the_pinned_05_session_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let spawner = Arc::new(TestSpawner::default());
        let service = make_service(tmp.path(), Arc::clone(&spawner));

        let value = create_ok(
            &service,
            Some(json!({
                "backend": "headless",
                "resolution": "1920x1080",
                "scale": 1.0,
                "view": false,
            })),
        );

        let object = value.as_object().expect("a session object");
        assert_eq!(object.len(), 12, "05 shape carries exactly these keys");
        assert_eq!(value["id"], "2f7a4c1e-9b3d-4a6f-8e21-000000000001");
        assert_eq!(value["label"], "");
        assert_eq!(value["state"], "running");
        assert_eq!(value["backend"], "headless");
        assert_eq!(value["resolution"], "1920x1080");
        assert_eq!(value["scale"], 1.0);
        assert_eq!(value["scope"], "operator");
        assert_eq!(value["origin"], "shell");
        assert_eq!(value["view"], Value::Null);
        assert_eq!(value["instance_id"], "11111111-1111-4111-8111-111111111111");
        assert_eq!(value["renderer"], "unverified");
        let idle = value["idle_since"]
            .as_str()
            .expect("idle_since is a string");
        assert_eq!(idle.len(), 20, "{idle:?}");
        assert!(idle.ends_with('Z') && &idle[4..5] == "-" && &idle[10..11] == "T");

        // The spawner saw the exact spec; nothing extra was invented.
        let spawned = spawner.spawned.lock().unwrap();
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].backend, Backend::Headless);
        assert_eq!(spawned[0].resolution, "1920x1080");
        assert_eq!(spawned[0].scale, 1.0);
        assert!(!spawned[0].view);
    }

    #[test]
    fn create_defaults_apply_when_params_are_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        let value = create_ok(&service, None);
        assert_eq!(value["backend"], "headless");
        assert_eq!(value["resolution"], "1920x1080");
        assert_eq!(value["scale"], 1.0);
        assert_eq!(value["label"], "");

        // A non-default label rides through to both the record and the spawner.
        let value = create_ok(&service, Some(json!({ "label": "demo" })));
        assert_eq!(value["label"], "demo");
    }

    #[test]
    fn create_rejects_malformed_params_with_bad_request() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));

        let err = result(
            &service,
            methods::CREATE_SESSION,
            Some(json!({ "backend": "wayland" })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(err.message.contains("headless, nested, host"), "{err:?}");

        let err = result(
            &service,
            methods::CREATE_SESSION,
            Some(json!({ "resolution": "1024" })),
        )
        .unwrap_err();
        assert!(err.message.contains("invalid resolution"), "{err:?}");

        for scale in [json!(0), json!(-2.0), json!("big")] {
            let err = result(
                &service,
                methods::CREATE_SESSION,
                Some(json!({ "scale": scale })),
            )
            .unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRequest, "scale {scale}");
        }

        let err = result(
            &service,
            methods::CREATE_SESSION,
            Some(json!({ "view": "yes" })),
        )
        .unwrap_err();
        assert!(err.message.contains("view must be a boolean"), "{err:?}");

        let err = result(
            &service,
            methods::CREATE_SESSION,
            Some(json!({ "wat": true })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(err.message.contains("unknown parameter"), "{err:?}");
    }

    #[test]
    fn create_with_view_is_refused_provisionally_and_spawns_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let spawner = Arc::new(TestSpawner::default());
        let service = make_service(tmp.path(), Arc::clone(&spawner));

        let err = result(
            &service,
            methods::CREATE_SESSION,
            Some(json!({ "view": true, "label": "wanted-a-view" })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ViewUnavailable);
        assert!(err.message.contains("wanted-a-view"), "{err:?}");
        assert!(spawner.spawned.lock().unwrap().is_empty());
        assert!(
            result(&service, methods::LIST_SESSIONS, None).unwrap()["sessions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn create_propagates_spawner_refusals_and_leaves_no_record() {
        let tmp = tempfile::tempdir().unwrap();
        let spawner = Arc::new(TestSpawner::default());
        *spawner.fail_next.lock().unwrap() = Some(ErrorEnvelope::renderer_refused("llvmpipe"));
        let service = make_service(tmp.path(), Arc::clone(&spawner));

        let err = result(&service, methods::CREATE_SESSION, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::RendererRefused);
        assert!(
            result(&service, methods::LIST_SESSIONS, None).unwrap()["sessions"]
                .as_array()
                .unwrap()
                .is_empty(),
            "a refused spawn must not leave a dead record"
        );
    }

    #[test]
    fn create_records_the_creation_renderer_mark_the_spawner_reports() {
        let tmp = tempfile::tempdir().unwrap();

        // The default test spawner reports `unverified` (no detection) — never silently
        // `verified`.
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        assert_eq!(create_ok(&service, None)["renderer"], "unverified");

        // A detecting spawner's mark reaches the record untouched: `software` (the override
        // was on) and `verified` (a GPU) alike.
        for mark in [RendererMark::Software, RendererMark::Verified] {
            let spawner = Arc::new(TestSpawner {
                renderer: mark,
                ..TestSpawner::default()
            });
            let service = make_service(tmp.path(), Arc::clone(&spawner));
            let value = create_ok(&service, Some(json!({ "label": "marked" })));
            assert_eq!(value["renderer"], mark.as_str(), "{value}");
        }
    }

    #[test]
    fn create_passes_the_live_allow_software_flag_to_the_spawner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_env(
            Some(dir.path()),
            Some(dir.path()),
            Some(dir.path()),
            "default",
        );
        let cfg_path = paths.core_config();
        std::fs::create_dir_all(cfg_path.parent().expect("config dir")).expect("config dir");
        CoreConfig::default().save(&cfg_path).expect("seed config");

        let spawner = Arc::new(TestSpawner::default());
        let service = make_typed_service_with_config(
            dir.path(),
            Arc::clone(&spawner),
            CoreConfig::load(&cfg_path).expect("load seed"),
        );

        // The spec default is off: the spawner sees `false`.
        create_ok(&service, None);
        assert_eq!(*spawner.allow_software_seen.lock().unwrap(), vec![false]);

        // A reload adopts the edited key (08: non-restart-only) and the NEXT create sees the
        // live value — the override is reachable only through the manager's config.
        let mut edited = CoreConfig::load(&cfg_path).expect("reload for edit");
        edited
            .set("renderer.allow_software", ConfigValue::Toggle(Toggle::On))
            .expect("set renderer.allow_software");
        edited.save(&cfg_path).expect("write edit");
        let out = result(&service, methods::CONFIG_RELOAD, None).expect("reload");
        assert_eq!(out["reloaded"], json!(["renderer.allow_software"]));

        create_ok(&service, None);
        assert_eq!(
            *spawner.allow_software_seen.lock().unwrap(),
            vec![false, true]
        );
    }

    #[test]
    fn scope_quota_from_the_scope_file_is_enforced_with_its_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        // scopes/operator.ron with max 2 (the rest of the scope file defaults).
        let paths = Paths::for_env(
            Some(tmp.path()),
            Some(tmp.path()),
            Some(tmp.path()),
            "default",
        );
        let scopes = paths.scopes_dir();
        std::fs::create_dir_all(&scopes).unwrap();
        std::fs::write(
            paths.scope_file("operator"),
            "(sessions: (max: Some(2), visibility: all))",
        )
        .unwrap();

        create_ok(&service, None);
        create_ok(&service, None);
        let err = result(&service, methods::CREATE_SESSION, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::QuotaExceeded);
        assert_eq!(
            err.message,
            "session limit reached: scope 'operator' 2/2; global 2/16. Close a session first."
        );
    }

    #[test]
    fn list_pages_with_limit_cursor_and_the_provisional_default() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        for _ in 0..3 {
            create_ok(&service, None);
        }

        let page = result(
            &service,
            methods::LIST_SESSIONS,
            Some(json!({ "limit": 2 })),
        )
        .unwrap();
        assert_eq!(page["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(page["next_cursor"], "2");

        let page = result(
            &service,
            methods::LIST_SESSIONS,
            Some(json!({ "limit": 2, "cursor": "2" })),
        )
        .unwrap();
        assert_eq!(page["sessions"].as_array().unwrap().len(), 1);
        assert!(page.get("next_cursor").is_none(), "last page has no cursor");

        let page = result(
            &service,
            methods::LIST_SESSIONS,
            Some(json!({ "cursor": "9" })),
        )
        .unwrap();
        assert!(page["sessions"].as_array().unwrap().is_empty());

        let all = result(&service, methods::LIST_SESSIONS, None).unwrap();
        assert_eq!(all["sessions"].as_array().unwrap().len(), 3);

        let err = result(
            &service,
            methods::LIST_SESSIONS,
            Some(json!({ "cursor": "x" })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(err.message.contains("invalid cursor"), "{err:?}");
        let err = result(
            &service,
            methods::LIST_SESSIONS,
            Some(json!({ "limit": 0 })),
        )
        .unwrap_err();
        assert!(err.message.contains("at least 1"), "{err:?}");
    }

    #[test]
    fn session_status_walks_the_registry_gates() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));

        let err = result(
            &service,
            methods::SESSION_STATUS,
            Some(json!({ "session_id": "nope" })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::SessionNotFound);
        assert_eq!(err.hint.as_deref(), Some("list sessions to find one"));

        let value = create_ok(&service, None);
        let id = value["id"].as_str().unwrap().to_string();
        let read = result(
            &service,
            methods::SESSION_STATUS,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert_eq!(read, value, "status reads exactly what create returned");

        // Missing id: a bad request, not a lookup.
        let err = result(&service, methods::SESSION_STATUS, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(err.message.contains("session_id is required"), "{err:?}");
    }

    #[test]
    fn close_kills_then_removes_and_closed_is_final() {
        let tmp = tempfile::tempdir().unwrap();
        let spawner = Arc::new(TestSpawner::default());
        let service = make_service(tmp.path(), Arc::clone(&spawner));

        let value = create_ok(&service, None);
        let id = value["id"].as_str().unwrap().to_string();

        // 08: an audit artifact persisted before the close must still be there afterwards —
        // retention follows the history window, not the close.
        let artifact_dir = service.paths.media_dir().join(&id);
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let artifact = artifact_dir.join("0123456789abcdef0123456789abcdef");
        std::fs::write(&artifact, b"kept-for-retention").unwrap();

        let closed = result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert_eq!(closed, json!({ "id": id, "state": "closed" }));
        assert_eq!(
            spawner.killed.lock().unwrap().as_slice(),
            std::slice::from_ref(&id)
        );

        assert!(
            artifact.exists(),
            "08: closing a session keeps its persisted media for the retention window"
        );

        // Second close: the distinct code, not a silent success.
        let err = result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": id })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::SessionClosed);

        // Unknown stays unknown.
        let err = result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": "ghost" })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::SessionNotFound);
    }

    #[test]
    fn close_of_a_dead_session_is_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        let value = create_ok(&service, None);
        let id = value["id"].as_str().unwrap().to_string();
        service.registry().reap(&id).unwrap();

        // 08: an audit artifact persisted before the close must still be there afterwards —
        // retention follows the history window, not the close.
        let artifact_dir = service.paths.media_dir().join(&id);
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let artifact = artifact_dir.join("0123456789abcdef0123456789abcdef");
        std::fs::write(&artifact, b"kept-for-retention").unwrap();

        let closed = result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert_eq!(closed["state"], "closed");
        assert!(
            artifact.exists(),
            "08: a dead session's persisted media keeps its retention window too"
        );
    }

    #[test]
    fn the_sweep_honors_the_configured_window() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        let value = create_ok(&service, None);
        let id = value["id"].as_str().unwrap().to_string();
        result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        let artifact_dir = service.paths.media_dir().join(&id);
        std::fs::create_dir_all(&artifact_dir).unwrap();
        std::fs::write(
            artifact_dir.join("0123456789abcdef0123456789abcdef"),
            b"png",
        )
        .unwrap();

        // Two days old: inside the default seven-day window, outside a one-hour one.
        let two_days_ago = crate::runtime::retention::now_secs() - 2 * 86_400;
        let marker = service.paths.history_dir().join(&id).join(".ended");
        std::fs::write(
            &marker,
            format!("{}\tcause=closed\n", rfc3339_from_unix_secs(two_days_ago)),
        )
        .unwrap();

        let report = service.sweep_retention();
        assert!(report.removed.is_empty(), "default window: {report:?}");
        assert!(artifact_dir.exists());

        service.config.write().unwrap().history.retention =
            crate::config::Duration::from_secs(3_600);
        let report = service.sweep_retention();
        assert_eq!(report.removed, vec![id.clone()]);
        assert!(!service.paths.history_dir().join(&id).exists());
        assert!(!artifact_dir.exists(), "media follows the same window");
    }

    #[test]
    fn a_running_session_is_kept_even_with_an_old_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        let value = create_ok(&service, None);
        let id = value["id"].as_str().unwrap().to_string();
        let history_dir = service.paths.history_dir().join(&id);
        std::fs::create_dir_all(&history_dir).unwrap();
        std::fs::write(
            history_dir.join(".ended"),
            "2020-01-01T00:00:00Z\tcause=closed\n",
        )
        .unwrap();

        let report = service.sweep_retention();

        assert!(report.removed.is_empty());
        assert!(
            history_dir.exists(),
            "the registry's live session wins over the marker"
        );
    }

    #[test]
    fn the_startup_pass_adopts_an_unmarked_directory_once() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        let orphan = service.paths.history_dir().join("crash-leftover");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("seg-0000.log"), b"{}\n").unwrap();

        let lines = service.startup_retention();

        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("1 unmarked session(s) adopted"),
            "{lines:?}"
        );
        let marker = std::fs::read_to_string(orphan.join(".ended")).unwrap();
        assert!(marker.contains("cause=manager_restart"), "{marker}");
        assert!(
            orphan.exists(),
            "adoption starts the window; it never deletes"
        );

        let again = service.startup_retention();
        assert!(
            again.is_empty(),
            "the marker makes the pass idempotent: {again:?}"
        );
    }

    #[tokio::test]
    async fn the_running_manager_sweeps_on_its_timer() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(make_service(tmp.path(), Arc::new(TestSpawner::default())));
        let value = create_ok(&service, None);
        let id = value["id"].as_str().unwrap().to_string();
        result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        let dir = service.paths.history_dir().join(&id);
        std::fs::write(dir.join(".ended"), "2020-01-01T00:00:00Z\tcause=closed\n").unwrap();

        // A short cadence stands in for the pinned hour: the task must tick on its own and
        // run the pass off the reactor.
        let sweeper = service.spawn_retention_sweeper_every(std::time::Duration::from_millis(20));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while dir.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        sweeper.abort();
        assert!(
            !dir.exists(),
            "the running manager's pass removes an aged session"
        );
    }

    #[test]
    fn status_and_readiness_read_the_live_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        let first = create_ok(&service, None);
        create_ok(&service, None);
        service
            .registry()
            .reap(first["id"].as_str().unwrap())
            .unwrap();

        for method in [methods::STATUS, methods::READINESS] {
            let value = result(&service, method, None).unwrap();
            assert_eq!(value["state"], "running");
            assert_eq!(value["mode"], "attached");
            assert_eq!(value["pid"].as_u64(), Some(std::process::id() as u64));
            assert_eq!(value["sessions"]["running"], 1, "{method}");
            assert_eq!(value["sessions"]["dead"], 1, "{method}");
            assert_eq!(value["instance_id"], "11111111-1111-4111-8111-111111111111");
            assert_eq!(value["versions"]["protocol"], PROTOCOL_VERSION);
        }
    }

    /// P2 hardening: the dispatch answers non-implemented names by class — a pinned-surface
    /// name this build lacks is `internal` (`not implemented yet`); a name outside the pinned
    /// surface refuses as `bad_request` (`unknown method …`).
    #[test]
    fn capture_tools_answer_their_own_validation_now() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        for method in [methods::SCREENSHOT, methods::CLIP_VIDEO] {
            let err = result(&service, method, None).unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRequest, "{method}");
            assert!(err.message.contains("session_id is required"), "{err:?}");
        }
        // Clip's own parameter is accepted; the screenshot surface refuses it.
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        let err = result(
            &service,
            methods::SCREENSHOT,
            Some(json!({ "session_id": id, "duration_s": 1 })),
        )
        .unwrap_err();
        assert!(err.message.contains("unknown parameter"), "{err:?}");
    }

    #[test]
    fn dispatch_refuses_unknown_and_unbuilt_names_by_class() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));

        // Unbuilt, but pinned: the views/query methods, daemon_end, and the control tools.
        for method in [methods::DAEMON_END, "pointer_move", "list_capabilities"] {
            let err = result(&service, method, None).unwrap_err();
            assert_eq!(err.code, ErrorCode::Internal, "{method}");
            assert_eq!(
                err.message,
                crate::strings::NOT_IMPLEMENTED_MESSAGE,
                "{method}"
            );
            assert_eq!(err.hint, None, "{method}");
        }

        // Outside the surface: a client-side refusal, naming the offending method.
        let err = result(&service, "frobnicate", None).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert_eq!(err.message, crate::strings::unknown_method("frobnicate"));
        assert_eq!(err.hint, None);
    }

    /// 05 parity: methods whose pinned surface takes no parameters refuse a stray key rather
    /// than answering the wrong question under a typo'd call. `null`, absent and `{}` stay
    /// valid — the malformed middle is what must refuse.
    #[test]
    fn no_parameter_methods_refuse_stray_parameters() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _paths) = service_with_config_file(tmp.path(), 10);
        for method in [
            methods::STATUS,
            methods::READINESS,
            methods::CONFIG_RELOAD,
            methods::DRAIN,
        ] {
            let err = result(&service, method, Some(json!({ "bogus": 1 }))).unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRequest, "{method}");
            assert_eq!(
                err.message,
                crate::strings::takes_no_parameters("bogus"),
                "{method}"
            );
            assert_eq!(
                err.hint.as_deref(),
                Some(crate::strings::HINT_CHECK_HELP),
                "{method}"
            );
            result(&service, method, None)
                .unwrap_or_else(|err| panic!("{method} with null params: {err:?}"));
            result(&service, method, Some(json!({})))
                .unwrap_or_else(|err| panic!("{method} with empty params: {err:?}"));
        }
    }

    #[tokio::test]
    async fn lifecycle_over_a_real_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("manager.sock");
        let service = Arc::new(make_service(tmp.path(), Arc::new(TestSpawner::default())));
        let handler: Arc<dyn Handler> = service;
        let listener = Listener::bind(&socket).await.unwrap();
        let serving = tokio::spawn(listener.serve(
            handler,
            Arc::new(crate::media::MediaStore::with_ttl(
                tmp.path().to_path_buf(),
                crate::media::DEFAULT_TTL,
            )),
        ));

        let address = Address::Unix(socket.clone());
        let mut client = Client::connect(&address, std::time::Duration::from_secs(2), None)
            .await
            .expect("connects");
        let handshake = client.handshake().await.expect("handshake");
        assert_eq!(
            handshake.instance_id,
            "11111111-1111-4111-8111-111111111111"
        );

        let created = client
            .request(methods::CREATE_SESSION, Some(json!({})), None)
            .await
            .expect("create");
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["state"], "running");

        let listed = client
            .request(methods::LIST_SESSIONS, None, None)
            .await
            .expect("list");
        assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);

        let closed = client
            .request(
                methods::CLOSE_SESSION,
                Some(json!({ "session_id": id })),
                None,
            )
            .await
            .expect("close");
        assert_eq!(closed["state"], "closed");

        let err = client
            .request(
                methods::SESSION_STATUS,
                Some(json!({ "session_id": id })),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::SessionClosed);

        // P2 hardening: dispatch refusals are classified and do not end the connection.
        let err = client.request("frobnicate", None, None).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert_eq!(err.message, crate::strings::unknown_method("frobnicate"));

        // A closed session has no view to start: the tombstone code, not a stale stub.
        let err = client
            .request(methods::GET_VIEW, Some(json!({ "session_id": id })), None)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::SessionClosed);

        // The same connection still answers after both refusals.
        let value = client
            .request(methods::STATUS, None, None)
            .await
            .expect("status after refusals");
        assert_eq!(value["state"], "running");

        serving.abort();
    }

    #[test]
    fn rfc3339_rendering_is_exact() {
        assert_eq!(rfc3339_from_unix_secs(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix_secs(946_684_800), "2000-01-01T00:00:00Z");
        assert_eq!(
            rfc3339_from_unix_secs(1_700_000_000),
            "2023-11-14T22:13:20Z"
        );
        // Leap day: 2024-02-29T12:24:56Z.
        assert_eq!(
            rfc3339_from_unix_secs(1_709_209_496),
            "2024-02-29T12:24:56Z"
        );
    }

    /// Seed a config file and hand back (`service`, paths). The manager starts from
    /// `start_max_global`; the caller edits the file afterwards.
    fn service_with_config_file(
        dir: &std::path::Path,
        start_max_global: u32,
    ) -> (RuntimeService, Paths) {
        let paths = Paths::for_env(Some(dir), Some(dir), Some(dir), "default");
        let cfg_path = paths.core_config();
        std::fs::create_dir_all(cfg_path.parent().expect("config dir")).expect("config dir");
        let mut start = CoreConfig::default();
        start.sessions.max_global = start_max_global;
        start.save(&cfg_path).expect("seed config");
        let service = make_service_with_config(
            dir,
            Arc::new(TestSpawner::default()),
            CoreConfig::load(&cfg_path).expect("load seed"),
            None,
        );
        (service, paths)
    }

    #[test]
    fn config_reload_applies_clean_keys_and_reports_restart_only_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (service, paths) = service_with_config_file(dir.path(), 10);
        let cfg_path = paths.core_config();

        // The edit a running manager has not seen yet: one clean key, the bind
        // (restart-only), and a client-local key (not the manager's to adopt, 08 A2).
        let mut edited = CoreConfig::load(&cfg_path).expect("reload for edit");
        edited
            .set("sessions.max_global", ConfigValue::Int(25))
            .expect("set clean key");
        edited
            .set(
                "manager.bind",
                ConfigValue::Text("127.0.0.1:8756".to_string()),
            )
            .expect("set bind");
        edited
            .set(
                "manager.start_mode",
                ConfigValue::Enum("attached".to_string()),
            )
            .expect("set client-local key");
        edited.save(&cfg_path).expect("write edit");

        let out = result(&service, methods::CONFIG_RELOAD, None).expect("reload");
        assert_eq!(out["reloaded"], json!(["sessions.max_global"]));
        assert_eq!(out["restart_only"], json!(["manager.bind"]));

        // The clean key took effect; the restart-only one and the client-local one did not.
        let active = service.active_config();
        assert_eq!(active.sessions.max_global, 25);
        assert_eq!(active.manager.bind, None);
        assert_eq!(active.manager.start_mode, StartMode::Auto);

        // A second reload: nothing new to adopt, but the bind drift persists —
        // restart-only items stay reported until a restart adopts them.
        let out = result(&service, methods::CONFIG_RELOAD, None).expect("reload again");
        assert_eq!(out["reloaded"], json!([]));
        assert_eq!(out["restart_only"], json!(["manager.bind"]));
    }

    #[test]
    fn config_reload_refuses_an_invalid_or_missing_file_and_applies_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (service, paths) = service_with_config_file(dir.path(), 10);
        let cfg_path = paths.core_config();

        // A typo in the file refuses the reload, nothing half-applies.
        std::fs::write(
            &cfg_path,
            "(version: 1, sessions: (max_global: 25), nope: 1)",
        )
        .expect("write invalid");
        let err = result(&service, methods::CONFIG_RELOAD, None).expect_err("invalid refuses");
        assert!(err.message.contains("config load error"), "got: {err:?}");
        assert_eq!(
            service.active_config().sessions.max_global,
            10,
            "nothing applied"
        );

        // A file deleted under the running manager refuses too — never a silent reset.
        std::fs::remove_file(&cfg_path).expect("remove");
        let err = result(&service, methods::CONFIG_RELOAD, None).expect_err("missing refuses");
        assert!(err.message.contains("cannot read"), "got: {err:?}");
        assert_eq!(
            service.active_config().sessions.max_global,
            10,
            "nothing applied"
        );
    }

    #[test]
    fn config_reload_reports_token_drift_as_restart_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_env(
            Some(dir.path()),
            Some(dir.path()),
            Some(dir.path()),
            "default",
        );
        let cfg_path = paths.core_config();
        std::fs::create_dir_all(cfg_path.parent().expect("config dir")).expect("config dir");
        let mut start = CoreConfig::default();
        start.sessions.max_global = 10;
        start.save(&cfg_path).expect("seed config");
        let token_path = paths.token_file();
        std::fs::create_dir_all(token_path.parent().expect("data dir")).expect("data dir");
        std::fs::write(&token_path, "old-token-value\n").expect("seed token");

        // This manager start adopted the on-disk token.
        let service = make_service_with_config(
            dir.path(),
            Arc::new(TestSpawner::default()),
            CoreConfig::load(&cfg_path).expect("load seed"),
            Some("old-token-value".to_string()),
        );

        // A rotation under the running manager is restart-only, not applied (07/08).
        std::fs::write(&token_path, "new-token-value\n").expect("rotate token");
        let out = result(&service, methods::CONFIG_RELOAD, None).expect("reload");
        assert_eq!(out["reloaded"], json!([]));
        assert_eq!(out["restart_only"], json!(["token"]));

        // The running manager keeps the value it adopted: no drift reported.
        let service = make_service_with_config(
            dir.path(),
            Arc::new(TestSpawner::default()),
            CoreConfig::load(&cfg_path).expect("load seed"),
            Some("new-token-value".to_string()),
        );
        let out = result(&service, methods::CONFIG_RELOAD, None).expect("reload");
        assert_eq!(out["reloaded"], json!([]));
        assert_eq!(out["restart_only"], json!([]));
    }

    #[test]
    fn config_get_reads_the_file_and_refuses_client_local_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (service, paths) = service_with_config_file(dir.path(), 25);
        let cfg_path = paths.core_config();

        // The file is the read source (same as the local cold path): an un-reloaded edit
        // shows up in `get` while the running snapshot still holds the old value.
        let mut edited = CoreConfig::load(&cfg_path).expect("load for edit");
        edited
            .set("sessions.max_global", ConfigValue::Int(40))
            .expect("set");
        edited.save(&cfg_path).expect("save edit");

        let out = result(
            &service,
            methods::CONFIG_GET,
            Some(json!({ "key": "sessions.max_global" })),
        )
        .expect("get");
        assert_eq!(out["key"], json!("sessions.max_global"));
        assert_eq!(out["value"], json!("40"));
        assert_eq!(out["authority"], json!("manager_side"));
        assert_eq!(
            service.active_config().sessions.max_global,
            25,
            "the snapshot is not the read source"
        );

        // Unset optionals answer in the grammar's unset form.
        let out = result(
            &service,
            methods::CONFIG_GET,
            Some(json!({ "key": "manager.bind" })),
        )
        .expect("get bind");
        assert_eq!(out["value"], json!("(unset)"));

        // Client-local keys do not exist on a manager (08 A2) — refused, not answered.
        let err = result(
            &service,
            methods::CONFIG_GET,
            Some(json!({ "key": "remote.default" })),
        )
        .expect_err("client-local refuses");
        assert!(
            err.message.contains("does not exist on a remote manager"),
            "got: {err:?}"
        );

        // Unknown keys name the valid set; `key` is required.
        let err = result(
            &service,
            methods::CONFIG_GET,
            Some(json!({ "key": "nope.key" })),
        )
        .expect_err("unknown refuses");
        assert!(err.message.contains("unknown config key"), "got: {err:?}");
        let err = result(&service, methods::CONFIG_GET, None).expect_err("missing key refuses");
        assert!(err.message.contains("key is required"), "got: {err:?}");

        // A hand-broken file refuses rather than answering defaults.
        std::fs::write(
            &cfg_path,
            "(version: 1, sessions: (max_global: 40), nope: 1)",
        )
        .expect("break file");
        let err = result(
            &service,
            methods::CONFIG_GET,
            Some(json!({ "key": "sessions.max_global" })),
        )
        .expect_err("broken file refuses");
        assert!(err.message.contains("config load error"), "got: {err:?}");
    }

    #[test]
    fn config_set_writes_the_file_and_leaves_the_snapshot_until_reload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (service, paths) = service_with_config_file(dir.path(), 10);
        let cfg_path = paths.core_config();

        let out = result(
            &service,
            methods::CONFIG_SET,
            Some(json!({ "key": "sessions.max_global", "value": "25" })),
        )
        .expect("set");
        assert_eq!(out["key"], json!("sessions.max_global"));
        assert_eq!(out["value"], json!("25"));

        // The file moved; the running snapshot did not (until reload, 08).
        let on_disk = CoreConfig::load(&cfg_path).expect("load after set");
        assert_eq!(
            on_disk
                .get("sessions.max_global")
                .expect("get")
                .to_grammar(),
            "25"
        );
        assert_eq!(service.active_config().sessions.max_global, 10);
        let out = result(&service, methods::CONFIG_RELOAD, None).expect("reload");
        assert_eq!(out["reloaded"], json!(["sessions.max_global"]));
        assert_eq!(service.active_config().sessions.max_global, 25);

        // Restart-only keys are ordinary file writes: reload reports the drift, never applies.
        let out = result(
            &service,
            methods::CONFIG_SET,
            Some(json!({ "key": "manager.bind", "value": "127.0.0.1:8756" })),
        )
        .expect("set bind");
        assert_eq!(out["value"], json!("127.0.0.1:8756"));
        let out = result(&service, methods::CONFIG_RELOAD, None).expect("reload");
        assert_eq!(out["reloaded"], json!([]));
        assert_eq!(out["restart_only"], json!(["manager.bind"]));
    }

    /// P2 hardening: two connections racing `config_set` on distinct keys must both land —
    /// the manager's RMW runs under the config write lock (`update_config`), so neither
    /// writer clobbers the other; pre-fix this lost keys live (fourth stress pass).
    #[test]
    fn concurrent_config_sets_keep_both_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (service, paths) = service_with_config_file(dir.path(), 10);
        let service = Arc::new(service);
        let iters = 60u64;

        let threads: Vec<_> = [("sessions.max_global", 200u64), ("history.segments", 20)]
            .into_iter()
            .map(|(key, base)| {
                let service = Arc::clone(&service);
                std::thread::spawn(move || {
                    for i in 0..iters {
                        let out = result(
                            &service,
                            methods::CONFIG_SET,
                            Some(json!({ "key": key, "value": (base + i).to_string() })),
                        )
                        .expect("set");
                        assert_eq!(out["key"], json!(key));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("writer thread");
        }

        let on_disk = CoreConfig::load(&paths.core_config()).expect("load after concurrent sets");
        assert_eq!(
            on_disk.get("sessions.max_global").unwrap().to_grammar(),
            (200 + iters - 1).to_string()
        );
        assert_eq!(
            on_disk.get("history.segments").unwrap().to_grammar(),
            (20 + iters - 1).to_string()
        );
    }

    #[test]
    fn config_set_refuses_invalid_values_client_local_and_unknown_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (service, paths) = service_with_config_file(dir.path(), 10);

        let err = result(
            &service,
            methods::CONFIG_SET,
            Some(json!({ "key": "sessions.max_global", "value": "abc" })),
        )
        .expect_err("bad int refuses");
        assert!(err.message.contains("invalid integer"), "got: {err:?}");
        let err = result(
            &service,
            methods::CONFIG_SET,
            Some(json!({ "key": "remote.default", "value": "box:8756" })),
        )
        .expect_err("client-local refuses");
        assert!(
            err.message.contains("does not exist on a remote manager"),
            "got: {err:?}"
        );
        let err = result(
            &service,
            methods::CONFIG_SET,
            Some(json!({ "key": "nope.key", "value": "1" })),
        )
        .expect_err("unknown refuses");
        assert!(err.message.contains("unknown config key"), "got: {err:?}");
        let err = result(
            &service,
            methods::CONFIG_SET,
            Some(json!({ "key": "sessions.max_global" })),
        )
        .expect_err("missing value refuses");
        assert!(err.message.contains("value is required"), "got: {err:?}");

        // Every refusal left the file exactly where it was.
        let on_disk = CoreConfig::load(&paths.core_config()).expect("load");
        assert_eq!(
            on_disk
                .get("sessions.max_global")
                .expect("get")
                .to_grammar(),
            "10"
        );
    }

    /// 04/10: drain is entered (idempotently), new sessions are refused with the pinned
    /// string while it holds, and status/readiness carry the live state.
    #[test]
    fn drain_refuses_new_sessions_with_the_pinned_string() {
        let tmp = tempfile::tempdir().unwrap();
        let spawner = Arc::new(TestSpawner::default());
        let service = make_service(tmp.path(), Arc::clone(&spawner));
        create_ok(&service, None);
        create_ok(&service, None);

        // Not draining yet: the state reads inactive/zero while sessions run.
        let status_before = result(&service, methods::STATUS, None).unwrap();
        assert_eq!(
            status_before["drain"],
            json!({ "active": false, "remaining": 0 })
        );

        let state = result(&service, methods::DRAIN, None).expect("drain enters");
        assert_eq!(state, json!({ "active": true, "remaining": 2 }));
        // Idempotent: `--wait` polls the same call; a second drain is the same state.
        let again = result(&service, methods::DRAIN, None).expect("poll");
        assert_eq!(again, state);

        for method in [methods::STATUS, methods::READINESS] {
            let value = result(&service, method, None).unwrap();
            assert_eq!(
                value["drain"],
                json!({ "active": true, "remaining": 2 }),
                "{method}"
            );
        }

        // Refused before anything parses or spawns; the envelope is 04 verbatim.
        let spawned_before = spawner.spawned.lock().unwrap().len();
        let err = result(&service, methods::CREATE_SESSION, Some(json!({}))).expect_err("refused");
        assert_eq!(err.code, ErrorCode::Draining);
        assert_eq!(err.message, crate::strings::draining(2));
        assert_eq!(
            err.hint.as_deref(),
            Some(crate::strings::HINT_RETRY_AFTER_UPGRADE)
        );
        assert_eq!(
            spawner.spawned.lock().unwrap().len(),
            spawned_before,
            "nothing spawned"
        );
    }

    /// The remaining count follows the live registry: it drops as sessions close, reaches
    /// zero, and drain stays active — only a manager stop clears it, so creates stay refused.
    #[test]
    fn drain_remaining_follows_closes_and_stays_active_at_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        let first = create_ok(&service, None);
        let second = create_ok(&service, None);

        let state = result(&service, methods::DRAIN, None).expect("drain enters");
        assert_eq!(state, json!({ "active": true, "remaining": 2 }));

        let id = first["id"].as_str().unwrap().to_string();
        result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": id })),
        )
        .expect("closing while draining is allowed — that is how a drain completes");
        let state = result(&service, methods::DRAIN, None).expect("poll");
        assert_eq!(state, json!({ "active": true, "remaining": 1 }));

        let id = second["id"].as_str().unwrap().to_string();
        result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": id })),
        )
        .expect("close");
        let state = result(&service, methods::DRAIN, None).expect("poll");
        assert_eq!(state, json!({ "active": true, "remaining": 0 }));

        // Zero running does not clear the state: the manager is still going down.
        let err = result(&service, methods::CREATE_SESSION, None).expect_err("still refused");
        assert_eq!(err.code, ErrorCode::Draining);
        assert_eq!(err.message, crate::strings::draining(0));
    }

    /// P2 hardening, round 6: `drain` entered **while** a create is inside its critical
    /// section. The create lock makes the order explicit — drain waits for that spawn to
    /// finish and then counts it, so `remaining` can never under-report a session that is
    /// being created (the state `drain --wait` polls to decide "zero").
    #[test]
    fn drain_waits_for_an_in_flight_create_and_counts_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let spawner = Arc::new(BlockingSpawner::new(entered_tx, release_rx));
        let service = Arc::new(make_service_with(tmp.path(), spawner.clone()));

        let creator = {
            let service = Arc::clone(&service);
            std::thread::spawn(move || result(&service, methods::CREATE_SESSION, None))
        };
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the create reached its spawn");

        let (drain_tx, drain_rx) = std::sync::mpsc::channel();
        let drainer = {
            let service = Arc::clone(&service);
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let out = result(&service, methods::DRAIN, None);
                let _ = drain_tx.send((started.elapsed(), out));
            })
        };
        // Drain may not answer while the spawn is still inside the create's critical section.
        assert!(
            drain_rx
                .recv_timeout(StdDuration::from_millis(300))
                .is_err(),
            "drain answered while a create was still spawning"
        );

        release_tx.send(()).expect("release the spawn");
        let created = creator
            .join()
            .expect("creator thread")
            .expect("create succeeds");
        let (waited, drained) = drain_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("drain answered once the create finished");
        drainer.join().expect("drainer thread");
        assert!(
            waited >= StdDuration::from_millis(250),
            "drain waited for the in-flight create: {waited:?}"
        );
        assert_eq!(created["state"], json!("running"));
        assert_eq!(
            drained.expect("drain enters"),
            json!({ "active": true, "remaining": 1 })
        );
        assert_eq!(spawner.calls.load(Ordering::SeqCst), 1);
    }

    /// P2 hardening, round 6: a create that passed the pre-lock drain check but then queued
    /// behind another create must re-read the flag once it holds the lock — otherwise it would
    /// start spawning after drain was entered. Deterministic interleaving: the test holds the
    /// create lock, the create queues on it, the flag is set exactly as `drain` sets it under
    /// that same lock, then the lock is released.
    #[test]
    fn a_create_that_queued_past_drain_is_refused_before_it_spawns() {
        let tmp = tempfile::tempdir().unwrap();
        let spawner = Arc::new(TestSpawner::default());
        let service = Arc::new(make_service(tmp.path(), Arc::clone(&spawner)));

        let guard = service.create_lock.lock().expect("create lock");
        let creator = {
            let service = Arc::clone(&service);
            std::thread::spawn(move || result(&service, methods::CREATE_SESSION, None))
        };
        std::thread::sleep(StdDuration::from_millis(150));
        // Queued, so it cannot have spawned: it is waiting for the lock this test holds.
        assert_eq!(spawner.spawned.lock().unwrap().len(), 0);
        service.draining.store(true, Ordering::SeqCst);
        drop(guard);

        let err = creator
            .join()
            .expect("creator thread")
            .expect_err("refused once drain holds");
        assert_eq!(err.code, ErrorCode::Draining);
        assert_eq!(err.message, crate::strings::draining(0));
        assert_eq!(
            spawner.spawned.lock().unwrap().len(),
            0,
            "nothing spawned after drain was entered"
        );
    }

    /// The P2 exit run: the lifecycle verbs against the **real** chain (mutter `--headless` →
    /// nested Hyprland on the render node) inside the L1 image. Ignored by default — it needs
    /// the image's compositors and render node; run there like:
    ///
    /// ```text
    /// dbus-run-session -- ./daiorama-tests real_chain --ignored --nocapture --test-threads=1
    /// ```
    #[tokio::test]
    #[ignore = "real chain: needs the L1 image (mutter + Hyprland + render node)"]
    async fn real_chain_lifecycle_verbs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let spawner = Arc::new(LinuxSpawner::new(SpawnerConfig::new(
            tmp.path().join("sessions"),
        )));

        let socket = tmp.path().join("manager.sock");
        let service = Arc::new(make_service_with(tmp.path(), spawner));
        let handler: Arc<dyn Handler> = service;
        let listener = Listener::bind(&socket).await.expect("bind");
        let serving = tokio::spawn(listener.serve(
            handler,
            Arc::new(crate::media::MediaStore::with_ttl(
                tmp.path().to_path_buf(),
                crate::media::DEFAULT_TTL,
            )),
        ));

        let mut client = Client::connect(
            &Address::Unix(socket.clone()),
            std::time::Duration::from_secs(2),
            None,
        )
        .await
        .expect("connect");
        client.handshake().await.expect("handshake");

        // Create: the real chain — mutter, Hyprland, the output at the pinned defaults.
        let first = client
            .request(methods::CREATE_SESSION, Some(json!({})), None)
            .await
            .expect("create 1");
        eprintln!(
            "CHAIN: created {} state={} resolution={}",
            first["id"], first["state"], first["resolution"]
        );
        assert_eq!(first["state"], "running");

        // A second session at a different size and scale (mode = 1024x768 x 1.5).
        let second = client
            .request(
                methods::CREATE_SESSION,
                Some(json!({ "resolution": "1024x768", "scale": 1.5 })),
                None,
            )
            .await
            .expect("create 2");
        eprintln!("CHAIN: created {} state={}", second["id"], second["state"]);

        let listed = client
            .request(methods::LIST_SESSIONS, None, None)
            .await
            .expect("list");
        assert_eq!(listed["sessions"].as_array().unwrap().len(), 2, "{listed}");

        let first_id = first["id"].as_str().unwrap().to_string();
        let read = client
            .request(
                methods::SESSION_STATUS,
                Some(json!({ "session_id": first_id })),
                None,
            )
            .await
            .expect("status");
        assert_eq!(read["state"], "running");

        let closed = client
            .request(
                methods::CLOSE_SESSION,
                Some(json!({ "session_id": first_id })),
                None,
            )
            .await
            .expect("close 1");
        assert_eq!(closed["state"], "closed");
        eprintln!("CHAIN: closed {first_id}");

        let err = client
            .request(
                methods::SESSION_STATUS,
                Some(json!({ "session_id": first_id })),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::SessionClosed);

        let second_id = second["id"].as_str().unwrap().to_string();
        let closed = client
            .request(
                methods::CLOSE_SESSION,
                Some(json!({ "session_id": second_id })),
                None,
            )
            .await
            .expect("close 2");
        assert_eq!(closed["state"], "closed");
        eprintln!("CHAIN: closed {second_id}");

        let listed = client
            .request(methods::LIST_SESSIONS, None, None)
            .await
            .expect("list after closes");
        assert!(
            listed["sessions"].as_array().unwrap().is_empty(),
            "closed sessions leave no live entries: {listed}"
        );

        serving.abort();
        eprintln!("CHAIN: all lifecycle verbs green on the real chain");
    }

    /// Scope carriage (05/07): the identity on the frame decides the record, the quota and
    /// what a listing shows — the CLI's absent field stays operator-wide.
    #[test]
    fn scope_carriage_records_the_caller_and_gates_visibility() {
        let tmp = tempfile::tempdir().unwrap();
        let scopes = tmp.path().join("daiorama/default/scopes");
        std::fs::create_dir_all(&scopes).unwrap();
        std::fs::write(
            scopes.join("agent.ron"),
            "(sessions: (visibility: own), grant: (categories: [\"input\"]))",
        )
        .unwrap();

        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        let created = result_as(
            "agent",
            Origin::Mcp,
            &service,
            methods::CREATE_SESSION,
            Some(json!({})),
        )
        .expect("create as agent");
        assert_eq!(created["scope"], "agent");
        assert_eq!(created["origin"], "mcp");
        let id = created["id"].as_str().unwrap().to_string();

        // A CLI-shaped request (no scope field) is operator and reads operator-wide.
        let operator = result(&service, methods::LIST_SESSIONS, None).unwrap();
        assert_eq!(operator["sessions"].as_array().unwrap().len(), 1);

        // The owning scope sees its session; a different own-scope sees neither the listing
        // entry nor the detail.
        let own = result_as("agent", Origin::Mcp, &service, methods::LIST_SESSIONS, None).unwrap();
        assert_eq!(own["sessions"].as_array().unwrap().len(), 1);
        let stranger =
            result_as("other", Origin::Mcp, &service, methods::LIST_SESSIONS, None).unwrap();
        assert_eq!(stranger["sessions"].as_array().unwrap().len(), 0);
        let err = result_as(
            "other",
            Origin::Mcp,
            &service,
            methods::SESSION_STATUS,
            Some(json!({ "session_id": id })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::SessionNotFound);
    }

    #[test]
    fn per_scope_quota_comes_from_that_scopes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let scopes = tmp.path().join("daiorama/default/scopes");
        std::fs::create_dir_all(&scopes).unwrap();
        std::fs::write(
            scopes.join("agent.ron"),
            "(sessions: (max: Some(1), visibility: own))",
        )
        .unwrap();

        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        result_as(
            "agent",
            Origin::Mcp,
            &service,
            methods::CREATE_SESSION,
            Some(json!({})),
        )
        .expect("first create");
        let err = result_as(
            "agent",
            Origin::Mcp,
            &service,
            methods::CREATE_SESSION,
            Some(json!({})),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::QuotaExceeded);
        assert!(
            err.message.contains("scope 'agent' 1/1"),
            "message names the scope: {}",
            err.message
        );
        // Another scope is unaffected by agent's quota.
        result_as(
            "operator",
            Origin::Shell,
            &service,
            methods::CREATE_SESSION,
            Some(json!({})),
        )
        .expect("operator create");
    }

    /// A 1×1 PNG header — enough to prove the dimension parse and byte transport.
    const TINY_PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00,
    ];

    fn capture_script(dir: &std::path::Path, body: &str) -> Vec<String> {
        let path = dir.join("capture.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        vec![path.display().to_string()]
    }

    fn capture_service(
        dir: &std::path::Path,
        config: CoreConfig,
    ) -> (RuntimeService, Arc<TestSpawner>) {
        let spawner = Arc::new(TestSpawner::default());
        *spawner.capture_env.lock().expect("capture_env") = Some(crate::manager::CaptureEnv {
            rt_dir: dir.to_path_buf(),
            wayland_display: "wayland-1".to_string(),
            signature: "sig".to_string(),
        });
        let service = make_service_with_config(
            dir,
            Arc::clone(&spawner) as Arc<dyn SessionSpawner>,
            config,
            None,
        );
        (service, spawner)
    }

    /// 05 §Capture: `screenshot` answers the typed image payload; the base64 is the file's
    /// exact bytes, and PNG dimensions come from the IHDR header.
    #[test]
    fn screenshot_returns_an_image_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let png = tmp.path().join("fixture.png");
        std::fs::write(&png, TINY_PNG).unwrap();
        let mut config = CoreConfig::default();
        config.capture.screenshot_command =
            capture_script(tmp.path(), &format!("cp {} \"$1\"", png.display()));
        let (service, _spawner) = capture_service(tmp.path(), config);
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();

        let value = result(
            &service,
            methods::SCREENSHOT,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert_eq!(value["capture"], "image");
        assert_eq!(value["mimeType"], "image/png");
        assert_eq!(value["width"], 1);
        assert_eq!(value["height"], 1);
        assert_eq!(value["bytes"], TINY_PNG.len());
        assert_eq!(
            value["data_b64"],
            crate::runtime::capture::base64_encode(TINY_PNG)
        );
        // `audit.store_screenshots` is off by default: nothing persisted.
        assert!(!service.paths.media_dir().join(&id).exists());
    }

    /// The raw PNG is checked against the cap before encoding (05 §Capture).
    #[test]
    fn screenshot_over_the_cap_is_a_bad_request() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CoreConfig::default();
        config.capture.screenshot_max_bytes = 100;
        config.capture.screenshot_command =
            capture_script(tmp.path(), "head -c 200 /dev/zero > \"$1\"");
        let (service, _spawner) = capture_service(tmp.path(), config);
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();

        let err = result(
            &service,
            methods::SCREENSHOT,
            Some(json!({ "session_id": id })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(err.message.starts_with("capture too large:"), "{err:?}");
    }

    /// `clip_video` mints a single-use id; the store serves it once and then answers exactly
    /// like an expired id (05 A8, claim-on-open).
    #[test]
    fn clip_video_mints_a_single_use_id() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CoreConfig::default();
        config.capture.clip_command = capture_script(tmp.path(), "printf x > \"$1\"; sleep 30");
        let (service, _spawner) = capture_service(tmp.path(), config);
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();

        let value = result(
            &service,
            methods::CLIP_VIDEO,
            Some(json!({ "session_id": id, "duration_s": 1 })),
        )
        .unwrap();
        assert_eq!(value["capture"], "media");
        assert_eq!(value["mimeType"], "video/webm");
        let media_id = value["media_id"].as_str().unwrap().to_string();
        assert_eq!(media_id.len(), 32);
        assert!(
            media_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let handle = service
            .media
            .open(&media_id)
            .expect("minted id serves once");
        assert_eq!(&*handle.bytes, b"x");
        assert!(service.media.open(&media_id).is_none(), "claim-on-open");
    }

    #[test]
    fn clip_video_bounds_the_duration() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _spawner) = capture_service(tmp.path(), CoreConfig::default());
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        let err = result(
            &service,
            methods::CLIP_VIDEO,
            Some(json!({ "session_id": id, "duration_s": 31 })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(err.message.contains("between 1 and 30"), "{err:?}");
    }

    /// A visible session whose process tree is gone cannot be captured: the spawner owns
    /// that knowledge, and the answer is the dead-session code, not a fake image.
    #[test]
    fn capture_of_a_session_without_a_live_env_is_session_dead() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(tmp.path(), Arc::new(TestSpawner::default()));
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        let err = result(
            &service,
            methods::SCREENSHOT,
            Some(json!({ "session_id": id })),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::SessionDead);
    }

    /// A fake wayvnc: a shell script that starts a TCP listener on the port the manager
    /// appends (and answers with the RFB banner). Skips the test when python3 is absent.
    fn fake_wayvnc(dir: &std::path::Path) -> Option<Vec<String>> {
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skip: python3 not available for the fake wayvnc");
            return None;
        }
        let script = dir.join("fake-wayvnc.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nPORT=$(sed -n 's/^port=//p' \"$2\")\nexec python3 -c 'import socket,sys\ns=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\ns.bind((\"127.0.0.1\", int(sys.argv[1]))); s.listen(4)\nwhile True:\n    c,_=s.accept(); c.sendall(b\"RFB 003.008\\n\"); c.close()' \"$PORT\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Some(vec!["/bin/sh".to_string(), script.display().to_string()])
    }

    #[test]
    fn get_view_is_idempotent_and_keeps_the_credential_out_of_status() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(wayvnc) = fake_wayvnc(tmp.path()) else {
            return;
        };
        let mut config = CoreConfig::default();
        config.view.wayvnc_command = wayvnc;
        config.view.port_base = crate::runtime::views::allocate_front_port(18080).unwrap();
        let (service, _spawner) = capture_service(tmp.path(), config);
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();

        let first = result(
            &service,
            methods::GET_VIEW,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        let credential = first["credential"].as_str().unwrap().to_string();
        assert_eq!(credential.len(), 64);
        assert!(
            first["address"]
                .as_str()
                .unwrap()
                .starts_with("http://127.0.0.1:")
        );
        assert!(
            first["address"]
                .as_str()
                .unwrap()
                .contains(&format!("credential={credential}"))
        );
        assert_eq!(first["generation"], 1);

        // Session status carries the view state but never the credential.
        let status = result(
            &service,
            methods::SESSION_STATUS,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert_eq!(status["view"]["state"], "running");
        assert_eq!(status["view"]["generation"], 1);
        assert!(
            !status["view"]
                .get("address")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("credential")
        );
        assert!(!status.to_string().contains(&credential));

        // Idempotent: same generation, same credential, same address.
        let second = result(
            &service,
            methods::GET_VIEW,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert_eq!(second["generation"], 1);
        assert_eq!(second["credential"], first["credential"]);
        assert_eq!(second["address"], first["address"]);
        service.views.stop(&id);
    }

    #[test]
    fn stop_view_stops_only_the_view_and_a_restart_moves_the_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(wayvnc) = fake_wayvnc(tmp.path()) else {
            return;
        };
        let mut config = CoreConfig::default();
        config.view.wayvnc_command = wayvnc;
        config.view.port_base = crate::runtime::views::allocate_front_port(18120).unwrap();
        let (service, _spawner) = capture_service(tmp.path(), config);
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();

        let first = result(
            &service,
            methods::GET_VIEW,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        let stopped = result(
            &service,
            methods::STOP_VIEW,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert_eq!(stopped["state"], "stopped");
        assert!(service.views.running(&id).is_none(), "process stopped");

        // The session is untouched; a new get_view starts generation 2.
        let status = result(
            &service,
            methods::SESSION_STATUS,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert_eq!(status["state"], "running");
        assert_eq!(status["view"], Value::Null);
        let second = result(
            &service,
            methods::GET_VIEW,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert_eq!(second["generation"], 2);
        assert_ne!(second["credential"], first["credential"]);
        service.views.stop(&id);
    }

    #[test]
    fn closing_a_session_stops_its_view() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(wayvnc) = fake_wayvnc(tmp.path()) else {
            return;
        };
        let mut config = CoreConfig::default();
        config.view.wayvnc_command = wayvnc;
        config.view.port_base = crate::runtime::views::allocate_front_port(18160).unwrap();
        let (service, _spawner) = capture_service(tmp.path(), config);
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        result(
            &service,
            methods::GET_VIEW,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert!(service.views.running(&id).is_some());
        result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert!(
            service.views.running(&id).is_none(),
            "view never outlives its session"
        );
    }

    /// B9: control-class calls spend bucket tokens; the 41st call at the defaults (40 burst,
    /// 20/s) waits for its token instead of running instantly.
    #[test]
    fn control_calls_are_paced_by_the_default_bucket() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _spawner) = capture_service(tmp.path(), CoreConfig::default());
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        for x in 0..40 {
            let _ = result(
                &service,
                "pointer_move",
                Some(json!({ "session_id": id, "x": x, "y": 0 })),
            );
        }
        let started = std::time::Instant::now();
        let _ = result(
            &service,
            "pointer_move",
            Some(json!({ "session_id": id, "x": 0, "y": 0 })),
        );
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(35),
            "the 41st call was paced: {:?}",
            started.elapsed()
        );
    }

    /// `reject` fails the overflow with a retry hint; reads never touch the bucket.
    #[test]
    fn reject_mode_refuses_with_a_retry_hint_and_reads_are_exempt() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _spawner) = capture_service(tmp.path(), CoreConfig::default());
        std::fs::create_dir_all(service.paths.scopes_dir()).unwrap();
        std::fs::write(
            service.paths.scope_file("operator"),
            "(limits: (rate: (per_sec: 1, burst: 1, mode: reject)))",
        )
        .unwrap();
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        let _ = result(
            &service,
            "pointer_move",
            Some(json!({ "session_id": id, "x": 1, "y": 1 })),
        );
        let error = result(
            &service,
            "pointer_move",
            Some(json!({ "session_id": id, "x": 2, "y": 2 })),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::RateLimited);
        assert!(
            error
                .hint
                .as_deref()
                .unwrap_or_default()
                .starts_with("retry after "),
            "{error:?}"
        );
        assert!(
            result(&service, methods::STATUS, None).is_ok(),
            "reads are exempt from the bucket"
        );
    }

    /// Capture is exempt from the bucket but serialized: one at a time per session.
    #[test]
    fn a_second_concurrent_capture_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let png = tmp.path().join("fixture.png");
        std::fs::write(&png, TINY_PNG).unwrap();
        let mut config = CoreConfig::default();
        config.capture.screenshot_command =
            capture_script(tmp.path(), &format!("sleep 1; cp {} \"$1\"", png.display()));
        let (service, _spawner) = capture_service(tmp.path(), config);
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                result(
                    &service,
                    methods::SCREENSHOT,
                    Some(json!({ "session_id": id })),
                )
            });
            std::thread::sleep(std::time::Duration::from_millis(150));
            let second = result(
                &service,
                methods::SCREENSHOT,
                Some(json!({ "session_id": id })),
            )
            .unwrap_err();
            assert_eq!(second.code, ErrorCode::BadRequest);
            assert!(second.message.contains("already running"), "{second:?}");
            assert!(first.join().unwrap().is_ok(), "the first capture completes");
        });
    }

    /// B8: create/close write records; the close also leaves the durable `.ended` marker the
    /// retention sweep reads (never mtimes).
    #[test]
    fn create_and_close_write_history_and_the_end_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _spawner) = capture_service(tmp.path(), CoreConfig::default());
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        let page = result(
            &service,
            methods::LIST_HISTORY,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        let records = page["records"].as_array().unwrap();
        assert!(
            records
                .iter()
                .any(|record| record["event"] == "session.created"),
            "{page}"
        );
        assert!(
            records
                .iter()
                .all(|record| record["scope"] == "operator" && record["origin"] == "shell"),
            "records carry scope + origin"
        );
        result(
            &service,
            methods::CLOSE_SESSION,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        let page = result(
            &service,
            methods::LIST_HISTORY,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert!(
            page["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["event"] == "session.closed")
        );
        let marker = service.paths.history_dir().join(&id).join(".ended");
        let body = std::fs::read_to_string(&marker).unwrap();
        assert!(body.contains("cause=closed"), "{body}");
    }

    /// History follows session visibility, and survives a manager restart through the recorded
    /// scope (the live registry does not).
    #[test]
    fn history_follows_visibility_and_survives_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _spawner) = capture_service(tmp.path(), CoreConfig::default());
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();

        // A stranger's session id is the same non-leak answer as everywhere.
        let error = result_as(
            "agent1",
            Origin::Mcp,
            &service,
            methods::LIST_HISTORY,
            Some(json!({ "session_id": id })),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SessionNotFound);

        // A fresh manager on the same data dir (a restart): the live record is gone, the
        // history's recorded scope still answers for the owner scope.
        drop(service);
        let (reopened, _spawner) = capture_service(tmp.path(), CoreConfig::default());
        let page = result(
            &reopened,
            methods::LIST_HISTORY,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        assert!(!page["records"].as_array().unwrap().is_empty());
    }

    /// Cursors are opaque, session-scoped and never silently ignored.
    #[test]
    fn history_pages_with_a_cursor_and_rejects_cross_session_cursors() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _spawner) = capture_service(tmp.path(), CoreConfig::default());
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        // Distinct events, written directly: the cursor must page by position, not by content.
        for n in 0..5 {
            let _ = service.history.append(&crate::history::Record {
                at: format!("2026-10-17T00:00:0{n}Z"),
                event: format!("test.{n}"),
                session_id: id.clone(),
                scope: "operator".to_string(),
                origin: "shell".to_string(),
                cause: None,
                detail: None,
            });
        }
        let first = result(
            &service,
            methods::LIST_HISTORY,
            Some(json!({ "session_id": id, "limit": 2 })),
        )
        .unwrap();
        assert_eq!(first["records"].as_array().unwrap().len(), 2);
        let cursor = first["next_cursor"].as_str().unwrap().to_string();
        let second = result(
            &service,
            methods::LIST_HISTORY,
            Some(json!({ "session_id": id, "limit": 2, "cursor": cursor })),
        )
        .unwrap();
        let events = |page: &Value| -> Vec<String> {
            page["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|record| record["event"].as_str().unwrap_or_default().to_string())
                .collect()
        };
        assert_ne!(events(&first), events(&second), "pages advance");
        assert_eq!(events(&first), vec!["test.4", "test.3"]);
        assert_eq!(events(&second), vec!["test.2", "test.1"]);
        let error = result(
            &service,
            methods::LIST_HISTORY,
            Some(json!({ "cursor": second["next_cursor"] })),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(error.message.contains("session_id"), "{error:?}");
        // Control args are never recorded.
        let page = result(
            &service,
            methods::LIST_HISTORY,
            Some(json!({ "session_id": id, "limit": 50 })),
        )
        .unwrap();
        assert!(!page.to_string().contains("\"x\""), "no control arguments");
    }

    /// Hostile ids and garbage cursors answer the same non-leak class as any unknown id.
    #[test]
    fn history_refuses_hostile_ids_and_garbage_cursors() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _spawner) = capture_service(tmp.path(), CoreConfig::default());
        for hostile in ["../../etc", "s-1/../../s-2", ".."] {
            let error = result(
                &service,
                methods::LIST_HISTORY,
                Some(json!({ "session_id": hostile })),
            )
            .unwrap_err();
            assert_eq!(error.code, ErrorCode::SessionNotFound, "{hostile:?}");
        }
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        let error = result(
            &service,
            methods::LIST_HISTORY,
            Some(json!({ "session_id": id, "cursor": "!!!not-base64!!!" })),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert_eq!(error.message, "invalid cursor");
    }

    /// View transitions are history too.
    #[test]
    fn view_start_and_stop_are_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(wayvnc) = fake_wayvnc(tmp.path()) else {
            return;
        };
        let mut config = CoreConfig::default();
        config.view.wayvnc_command = wayvnc;
        config.view.port_base = crate::runtime::views::allocate_front_port(18300).unwrap();
        let (service, _spawner) = capture_service(tmp.path(), config);
        let created = create_ok(&service, None);
        let id = created["id"].as_str().unwrap().to_string();
        result(
            &service,
            methods::GET_VIEW,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        result(
            &service,
            methods::STOP_VIEW,
            Some(json!({ "session_id": id })),
        )
        .unwrap();
        let page = result(
            &service,
            methods::LIST_HISTORY,
            Some(json!({ "session_id": id, "limit": 50 })),
        )
        .unwrap();
        let events: Vec<&str> = page["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["event"].as_str().unwrap_or_default())
            .collect();
        assert!(events.contains(&"view.started"), "{events:?}");
        assert!(events.contains(&"view.stopped"), "{events:?}");
        service.views.stop(&id);
    }

    // -----------------------------------------------------------------------
    // P4-ENDPOINT: the TCP manager surface and its credential gate
    // -----------------------------------------------------------------------

    fn seed_scope_secret(dir: &std::path::Path, scope: &str, secret: &str) {
        let paths = Paths::for_env(Some(dir), Some(dir), Some(dir), "default");
        std::fs::create_dir_all(paths.scope_tokens_dir()).expect("scope-tokens dir");
        std::fs::write(paths.scope_token_file(scope), format!("{secret}\n")).expect("scope secret");
    }

    /// One frame written to a raw TCP connection, one response read back. Raw sockets, not
    /// `Client`: the client refuses a missing token up front, and the point here is the
    /// server's answer to each credential class.
    struct Gated {
        reader: tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
        writer: tokio::net::tcp::OwnedWriteHalf,
    }

    impl Gated {
        /// Connect and complete the tokenless handshake every gated connection needs.
        async fn connect(authority: &str) -> Self {
            let stream = tokio::net::TcpStream::connect(authority)
                .await
                .expect("connect");
            let (reader, writer) = stream.into_split();
            let mut gated = Self {
                reader: tokio::io::BufReader::new(reader),
                writer,
            };
            let response = gated.exchange(&handshake_frame()).await;
            assert!(
                matches!(response, crate::ipc::Response::Result(_)),
                "handshake answers: {response:?}"
            );
            gated
        }

        async fn exchange(&mut self, request: &crate::ipc::Request) -> crate::ipc::Response {
            crate::ipc::write_frame(&mut self.writer, request)
                .await
                .expect("frame writes");
            crate::ipc::read_frame(&mut self.reader)
                .await
                .expect("frame reads")
        }

        /// `None` on a clean EOF — the shape a transport-gate refusal leaves behind.
        async fn exchange_or_eof(
            &mut self,
            request: &crate::ipc::Request,
        ) -> Option<crate::ipc::Response> {
            crate::ipc::write_frame(&mut self.writer, request)
                .await
                .expect("frame writes");
            crate::ipc::read_frame(&mut self.reader).await.ok()
        }
    }

    fn handshake_frame() -> crate::ipc::Request {
        crate::ipc::Request::new(
            methods::HANDSHAKE,
            Some(json!({
                "protocol_version": crate::ipc::PROTOCOL_VERSION,
                "min_supported": crate::ipc::MIN_SUPPORTED,
            })),
        )
    }

    fn gated_status(
        token: Option<&str>,
        scope: Option<&str>,
        secret: Option<&str>,
    ) -> crate::ipc::Request {
        let mut request = crate::ipc::Request::new(methods::STATUS, None);
        request.token = token.map(str::to_string);
        request.scope = scope.map(str::to_string);
        request.scope_token = secret.map(str::to_string);
        request
    }

    async fn tcp_service(
        dir: &std::path::Path,
        token_at_start: Option<&str>,
    ) -> (String, Arc<crate::media::MediaStore>) {
        let service = Arc::new(make_service_with_config(
            dir,
            Arc::new(TestSpawner::default()),
            CoreConfig::default(),
            token_at_start.map(str::to_string),
        ));
        let handler: Arc<dyn Handler> = service;
        let listener = crate::manager::NetworkListener::bind("127.0.0.1", 0)
            .await
            .expect("bind");
        let authority = listener.authority().to_string();
        let paths = Paths::for_env(Some(dir), Some(dir), Some(dir), "default");
        let media = Arc::new(crate::media::MediaStore::with_ttl(
            paths.media_dir(),
            crate::media::DEFAULT_TTL,
        ));
        tokio::spawn(listener.serve(handler, Arc::clone(&media)));
        (authority, media)
    }

    #[tokio::test]
    async fn tcp_surface_gates_token_then_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed_scope_secret(tmp.path(), "operator", "scope-secret");
        let (authority, _media) = tcp_service(tmp.path(), Some("instance-token")).await;

        // No token: the transport gate refuses and the connection ends.
        let mut gated = Gated::connect(&authority).await;
        match gated.exchange_or_eof(&gated_status(None, None, None)).await {
            Some(crate::ipc::Response::Error(envelope)) => {
                assert_eq!(envelope.code, ErrorCode::TokenRequired);
                assert_eq!(envelope.message, strings::token_required(&authority));
            }
            other => panic!("expected token_required, got {other:?}"),
        }
        assert!(
            gated
                .exchange_or_eof(&gated_status(
                    Some("instance-token"),
                    Some("operator"),
                    Some("scope-secret")
                ))
                .await
                .is_none(),
            "nothing follows the transport-gate refusal"
        );

        // Wrong token: same class.
        let mut gated = Gated::connect(&authority).await;
        match gated
            .exchange(&gated_status(
                Some("wrong"),
                Some("operator"),
                Some("scope-secret"),
            ))
            .await
        {
            crate::ipc::Response::Error(envelope) => {
                assert_eq!(envelope.code, ErrorCode::TokenRequired);
            }
            other => panic!("expected token_required, got {other:?}"),
        }

        // Right token, no scope: scope auth refuses, the connection continues.
        let mut gated = Gated::connect(&authority).await;
        match gated
            .exchange(&gated_status(Some("instance-token"), None, None))
            .await
        {
            crate::ipc::Response::Error(envelope) => {
                assert_eq!(envelope.code, ErrorCode::ScopeAuthRequired);
                assert_eq!(envelope.message, strings::FRAME_SCOPE_REQUIRED);
            }
            other => panic!("expected scope_required, got {other:?}"),
        }

        // Wrong scope secret: refused, and the connection is still usable.
        match gated
            .exchange(&gated_status(
                Some("instance-token"),
                Some("operator"),
                Some("wrong"),
            ))
            .await
        {
            crate::ipc::Response::Error(envelope) => {
                assert_eq!(envelope.code, ErrorCode::ScopeAuthRequired);
                assert_eq!(
                    envelope.message,
                    strings::frame_scope_auth_required("operator")
                );
            }
            other => panic!("expected scope_auth_required, got {other:?}"),
        }

        // A traversal-shaped name never reaches a path join.
        match gated
            .exchange(&gated_status(
                Some("instance-token"),
                Some("../../etc"),
                Some("x"),
            ))
            .await
        {
            crate::ipc::Response::Error(envelope) => {
                assert_eq!(envelope.code, ErrorCode::BadRequest);
                assert_eq!(envelope.message, strings::INVALID_SCOPE_NAME);
            }
            other => panic!("expected invalid scope name, got {other:?}"),
        }

        // Unknown but well-formed scope: the secret file is the credential, so the answer is
        // the same `scope_auth_required` class as a secretless scope — existence is not leaked.
        match gated
            .exchange(&gated_status(
                Some("instance-token"),
                Some("ghost"),
                Some("x"),
            ))
            .await
        {
            crate::ipc::Response::Error(envelope) => {
                assert_eq!(envelope.code, ErrorCode::ScopeAuthRequired);
                assert_eq!(envelope.message, strings::scope_has_no_secret("ghost"));
            }
            other => panic!("expected scope_auth_required, got {other:?}"),
        }

        // Authenticated: dispatched.
        match gated
            .exchange(&gated_status(
                Some("instance-token"),
                Some("operator"),
                Some("scope-secret"),
            ))
            .await
        {
            crate::ipc::Response::Result(value) => {
                assert_eq!(value["state"], "running");
            }
            other => panic!("expected a dispatched status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_rotation_window_is_enforced_on_the_wire() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed_scope_secret(tmp.path(), "operator", "scope-secret");
        let paths = Paths::for_env(
            Some(tmp.path()),
            Some(tmp.path()),
            Some(tmp.path()),
            "default",
        );
        std::fs::write(paths.token_file(), "old-token\n").expect("token file");

        // This start adopted the old value; the file now holds a new one.
        std::fs::write(paths.token_file(), "new-token\n").expect("rotate");
        let (authority, _media) = tcp_service(tmp.path(), Some("old-token")).await;

        let mut gated = Gated::connect(&authority).await;
        assert!(
            matches!(
                gated
                    .exchange(&gated_status(
                        Some("old-token"),
                        Some("operator"),
                        Some("scope-secret")
                    ))
                    .await,
                crate::ipc::Response::Result(_)
            ),
            "the adopted token keeps working until restart"
        );
        match gated
            .exchange(&gated_status(
                Some("new-token"),
                Some("operator"),
                Some("scope-secret"),
            ))
            .await
        {
            crate::ipc::Response::Error(envelope) => {
                assert_eq!(envelope.code, ErrorCode::TokenRequired);
            }
            other => panic!("expected the rotated token to be refused, got {other:?}"),
        }

        // A manager started after the rotation adopts the new value and refuses the old one.
        let (authority, _media) = tcp_service(tmp.path(), Some("new-token")).await;
        let mut gated = Gated::connect(&authority).await;
        assert!(matches!(
            gated
                .exchange(&gated_status(
                    Some("new-token"),
                    Some("operator"),
                    Some("scope-secret")
                ))
                .await,
            crate::ipc::Response::Result(_)
        ));
        match gated
            .exchange(&gated_status(
                Some("old-token"),
                Some("operator"),
                Some("scope-secret"),
            ))
            .await
        {
            crate::ipc::Response::Error(envelope) => {
                assert_eq!(envelope.code, ErrorCode::TokenRequired);
            }
            other => panic!("expected the old token to be refused after restart, got {other:?}"),
        }
    }
}
