//! The live manager behind `daiorama serve` (P2/W3): lock → containment plan → spawner →
//! service → bind → serve.
//!
//! Startup order is normative (03/10): the flock is taken **before anything binds**, the
//! containment decision runs after it and before the socket exists, and a startup failure is
//! reported as **one JSON envelope line on stderr** — the form the lazy-start launcher parses
//! (`fixtures/ipc/manager-start-limits-unavailable.ron`). Signals end the loop; everything the
//! manager owns dies with it: the spawner's `Drop` kills the session trees, the socket file is
//! removed, and the flock is released by the kernel (10).
//!
//! Containment-on runs wire the C6 seam here: the instance's session subtree (under the
//! manager cgroup dir the class decision acted on) is resolved and handed to the spawner as
//! the scope root sessions attach under, prepared before the service can spawn anything, and
//! swept before the socket binds (03 — session cgroups left by a previous manager run are
//! `cgroup.kill`ed). Off-mode runs never touch the cgroupfs.
//!
//! Provisional shapes (additive, flagged as such — none is observable on the wire yet): the
//! lock file name (`manager.lock` beside the socket), the loser's notice line, the session
//! scratch root (`<data-root>/sessions/`), the instance-id file (`<data-root>/instance-id`,
//! created once, 0600), and the `DAIORAMA_MODE` env channel the launcher uses to hand the
//! chosen mode to the child it starts.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::config::{CoreConfig, Paths, StartMode, read_token};
use crate::ipc::{self, Address, Client, EnvView, ErrorEnvelope};
use crate::manager::{
    self, Handler, InstanceMode, Listener, NetworkListener, SessionSpawner,
    startup::{ContainmentProbe, LimitsPlan, StartupDecision, StartupPlan, plan_startup},
};
use crate::runtime::containment::{
    LinuxCgroupProbe, ensure_session_subtree, session_subtree, sweep_session_subtree,
};
use crate::runtime::service::{RuntimeService, ServiceOptions};
use crate::runtime::spawner::{Containment, LinuxSpawner, SpawnerConfig};
use crate::strings;

/// How long a second `serve` (the lock loser) waits for the winner's socket before deciding
/// the winner died pre-bind (10: "waits a bounded time and connects instead").
pub const LOSER_WAIT: Duration = Duration::from_secs(5);

/// The one line a started manager reports a startup failure as: the frame's error form, on
/// stderr. `start_failure()` in the CLI parses exactly this shape back into the envelope.
pub fn startup_failure_line(envelope: &ErrorEnvelope) -> String {
    json!({ "error": envelope }).to_string()
}

/// Everything one `serve` run needs; constructed for real in [`serve_instance`], injected in
/// tests.
pub struct ServeDeps {
    pub paths: Paths,
    pub config: CoreConfig,
    pub spawner: Arc<dyn SessionSpawner>,
    /// The containment precondition probe. `prepare` reads it when no decision was taken
    /// ahead (tests, embedders); production takes the decision once in `run()` — one probe
    /// pass — because the plan selects the spawner's containment, and replays it via `plan`.
    pub probe: Box<dyn ContainmentProbe>,
    /// A containment decision taken ahead of the spawner's construction (production `run`
    /// replays it here). `None`: `prepare` takes the decision itself — the tests' shape.
    pub plan: Option<StartupPlan>,
    /// The instance session subtree (06 C6 seam) for an enabled plan, resolved by the
    /// caller: production via `LinuxCgroupProbe::cgroup_dir`; tests that exercise the seam
    /// inject a tempdir. When set with the plan enabled, `prepare` prepares it (idempotent)
    /// and startup sweeps it before binding. `None` skips both.
    pub session_subtree: Option<PathBuf>,
    /// The manager socket. Production derives it from the environment (08,
    /// `ipc::socket_path`); tests inject a temp path so no test touches the real runtime dir.
    pub socket: PathBuf,
    pub lock_wait: Duration,
}

/// The prepared, not-yet-bound manager: the startup decision already made.
pub struct Prepared {
    pub handler: Arc<dyn Handler>,
    /// The manager-side media store (05 A8): serves `GET /media/<id>` on the socket.
    pub media: Arc<crate::media::MediaStore>,
    pub socket: PathBuf,
    /// The `manager.bind` TCP surface (07), parsed: `Some` binds in addition to the unix
    /// socket, with the instance-token and scope-secret gate on every call.
    pub bind: Option<(String, u16)>,
    pub mode: InstanceMode,
    /// The recorded-downgrade notice when `limits.mode = off` (06); `None` otherwise.
    pub notice: Option<&'static str>,
    pub lock_wait: Duration,
    /// The instance session subtree startup must sweep before binding (03): `Some` when the
    /// plan enables the sweep and the subtree was resolved (production). `None` leaves the
    /// sweep off.
    pub sweep_root: Option<PathBuf>,
}

/// `daiorama serve` — the real thing. Startup failures are printed in the launcher's contract
/// form and returned as the error (exit 1).
pub async fn serve_instance(instance: &str) -> Result<(), ErrorEnvelope> {
    let env = EnvView::from_process();
    // 08: the in-binary runtime-dir ensure is the only code path that creates it.
    let result = run(instance, &env).await;
    if let Err(envelope) = &result {
        eprintln!("{}", startup_failure_line(envelope));
    }
    result
}

async fn run(instance: &str, env: &EnvView) -> Result<(), ErrorEnvelope> {
    crate::ipc::ensure_runtime_dir(env, instance)
        .map_err(|error| ErrorEnvelope::internal(format!("runtime dir: {error}")))?;
    let paths = Paths::from_process(instance);
    let config = CoreConfig::load_or_default(&paths.core_config())?;
    // The containment decision must precede the spawner: the plan's scope root is part of
    // the spawner's config (sessions attach their scopes beneath it), and the same root is
    // what the startup sweep works on. Take the plan once, here — one probe pass — and
    // replay it through `prepare` rather than taking it a second time.
    let cgroup_probe = LinuxCgroupProbe::new();
    let plan = match plan_startup(&config, &cgroup_probe) {
        StartupDecision::Refuse(envelope) => return Err(envelope),
        StartupDecision::Proceed(plan) => plan,
    };
    let session_subtree = match &plan.limits {
        LimitsPlan::Enabled { .. } => Some(resolve_session_subtree(&cgroup_probe)?),
        LimitsPlan::Disabled => None,
    };
    let mut spawner_config = SpawnerConfig::new(paths.data_dir.join("sessions"));
    if let Some(subtree) = &session_subtree {
        spawner_config.containment = Containment::On {
            subtree: subtree.clone(),
        };
    }
    let spawner: Arc<dyn SessionSpawner> = Arc::new(LinuxSpawner::new(spawner_config));
    let deps = ServeDeps {
        paths,
        config,
        spawner,
        probe: Box::new(cgroup_probe),
        plan: Some(plan),
        session_subtree,
        socket: ipc::socket_path(instance),
        lock_wait: LOSER_WAIT,
    };
    let prepared = prepare(&deps)?;
    run_until(prepared, shutdown_signal()).await
}

/// The startup decision, made concrete: containment plan → service → socket path. No binding,
/// no spawning — everything here is reversible and unit-testable.
///
/// The decision is taken here unless the caller already had to take it earlier (`deps.plan`,
/// production `run` — the plan selects the spawner's containment, which must precede this
/// call; replaying avoids a second probe pass). With the plan enabled and a caller-resolved
/// session subtree in hand, that subtree is prepared (idempotent: mkdir + controller enable,
/// read back) and startup sweeps it before binding (03). The injected spawner is always used
/// as-is.
pub fn prepare(deps: &ServeDeps) -> Result<Prepared, ErrorEnvelope> {
    let plan = match &deps.plan {
        Some(plan) => plan.clone(),
        None => match plan_startup(&deps.config, deps.probe.as_ref()) {
            StartupDecision::Refuse(envelope) => return Err(envelope),
            StartupDecision::Proceed(plan) => plan,
        },
    };
    let sweep_root = match (&plan.limits, &deps.session_subtree) {
        (LimitsPlan::Enabled { .. }, Some(subtree)) => {
            ensure_session_subtree(subtree).map_err(|error| {
                ErrorEnvelope::internal(format!("session subtree {}: {error}", subtree.display()))
            })?;
            plan.sweep.then_some(subtree.clone())
        }
        _ => None,
    };
    let notice = manager::limits_disabled_notice(&plan);
    let mode = effective_mode(&deps.config);
    let instance_id = read_or_create_instance_id(&deps.paths)?;
    let socket = deps.socket.clone();
    // 07: `manager.bind` is the TCP surface. Parse it at startup (a malformed value refuses
    // the start, never accept-time), and make the network credentials exist before the
    // listener can accept a caller.
    let bind = match deps
        .config
        .manager
        .bind
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
    {
        Some(raw) => Some(crate::config::parse_bind(raw)?),
        None => None,
    };
    if bind.is_some() {
        for line in crate::config::ensure_network_credentials(&deps.paths)? {
            eprintln!("{line}");
        }
    }
    let media = Arc::new(crate::media::MediaStore::for_instance(&deps.paths));
    let service = RuntimeService::new(
        ServiceOptions {
            instance_id,
            socket_path: socket.clone(),
            mode,
            config: deps.config.clone(),
            paths: deps.paths.clone(),
            // The token this start adopts (07): restart-only on reload — an unreadable
            // token file fails the start rather than reloading blind.
            token_at_start: read_token(&deps.paths)?,
        },
        Arc::clone(&deps.spawner),
        Arc::clone(&media),
    );
    Ok(Prepared {
        media,
        handler: Arc::new(service),
        socket,
        bind,
        mode,
        notice,
        lock_wait: deps.lock_wait,
        sweep_root,
    })
}

/// The C6 seam's path step: the manager's own cgroup dir — the dir the class decision acted
/// on, read from the probe instance that took the decision — and the instance session
/// subtree beneath it. Isolated so tests drive it exactly as the containment tests do
/// (`LinuxCgroupProbe::with_paths` against a tempdir).
fn resolve_session_subtree(probe: &LinuxCgroupProbe) -> Result<PathBuf, ErrorEnvelope> {
    let manager_cgroup = probe
        .cgroup_dir()
        .map_err(|detail| ErrorEnvelope::internal(format!("cgroup dir: {detail}")))?;
    Ok(session_subtree(&manager_cgroup))
}

/// The mode this process presents (04: attached/background). The launcher hands its choice
/// down via `DAIORAMA_MODE`; a pinned `manager.start_mode` is authoritative; `auto` falls back
/// to the terminal heuristic (10).
fn effective_mode(config: &CoreConfig) -> InstanceMode {
    if let Ok(raw) = std::env::var("DAIORAMA_MODE") {
        match raw.as_str() {
            "attached" => return InstanceMode::Attached,
            "background" => return InstanceMode::Background,
            _ => {}
        }
    }
    match config.manager.start_mode {
        StartMode::Attached => InstanceMode::Attached,
        StartMode::Background => InstanceMode::Background,
        StartMode::Auto => {
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                InstanceMode::Attached
            } else {
                InstanceMode::Background
            }
        }
    }
}

/// The instance id: persisted at first start, stable across restarts (provisional location,
/// `<data-root>/instance-id`, 0600).
fn read_or_create_instance_id(paths: &Paths) -> Result<String, ErrorEnvelope> {
    use std::os::unix::fs::PermissionsExt;
    let file = paths.data_dir.join("instance-id");
    if let Ok(text) = std::fs::read_to_string(&file) {
        let id = text.trim();
        if !id.is_empty() {
            return Ok(id.to_string());
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::create_dir_all(&paths.data_dir)
        .map_err(|error| ErrorEnvelope::internal(format!("data dir: {error}")))?;
    std::fs::write(&file, format!("{id}\n"))
        .map_err(|error| ErrorEnvelope::internal(format!("instance id: {error}")))?;
    let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    Ok(id)
}

// ---------------------------------------------------------------------------
// The runtime lock (10: exactly one manager per runtime path)
// ---------------------------------------------------------------------------

/// Held for this process's lifetime; the kernel releases the flock when the process dies, so
/// there is nothing to clean up after a crash (10 — explicitly not an `O_EXCL` lockfile).
pub struct RuntimeLock {
    _file: std::fs::File,
}

/// The lock's outcome: this process won (holds it) or another manager is already serving.
pub enum Lock {
    Holder(RuntimeLock),
    Loser,
}

fn try_flock(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EWOULDBLOCK) => Ok(false),
        _ => Err(error),
    }
}

/// Take the runtime lock, or lose to the manager that holds it. The loser waits a bounded time
/// and connects instead; if the winner died pre-bind (socket never answered), the loser
/// retries exactly once as a starter (10).
pub async fn acquire_lock(
    dir: &Path,
    socket: &Path,
    wait: Duration,
) -> Result<Lock, ErrorEnvelope> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)
        .map_err(|error| ErrorEnvelope::internal(format!("runtime dir: {error}")))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("manager.lock"))
        .map_err(|error| ErrorEnvelope::internal(format!("runtime lock: {error}")))?;
    let _ = std::fs::set_permissions(
        dir.join("manager.lock"),
        std::fs::Permissions::from_mode(0o600),
    );
    if try_flock(&file)
        .map_err(|error| ErrorEnvelope::internal(format!("runtime lock: {error}")))?
    {
        return Ok(Lock::Holder(RuntimeLock { _file: file }));
    }
    let address = Address::Unix(socket.to_path_buf());
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if Client::probe(&address, Duration::from_millis(200)).await {
            return Ok(Lock::Loser);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The winner died pre-bind: retry exactly once as a starter.
    if try_flock(&file)
        .map_err(|error| ErrorEnvelope::internal(format!("runtime lock: {error}")))?
    {
        return Ok(Lock::Holder(RuntimeLock { _file: file }));
    }
    Err(ErrorEnvelope::internal(format!(
        "another manager holds the runtime lock and no socket answers at {}",
        socket.display()
    )))
}

// ---------------------------------------------------------------------------
// The serve loop
// ---------------------------------------------------------------------------

/// Bind and serve until `shutdown` resolves. Winner-only: a lock loser exits after confirming
/// the winner answers.
pub async fn run_until(
    prepared: Prepared,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<(), ErrorEnvelope> {
    let dir = prepared
        .socket
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let lock = acquire_lock(&dir, &prepared.socket, prepared.lock_wait).await?;
    let _guard = match lock {
        Lock::Loser => {
            // Provisional wording (additive; not on the wire).
            eprintln!(
                "manager already serving this runtime path at {}",
                prepared.socket.display()
            );
            return Ok(());
        }
        Lock::Holder(guard) => guard,
    };

    // A manager that died leaves its socket file behind; a dead path must not refuse the bind.
    if prepared.socket.exists()
        && !Client::probe(
            &Address::Unix(prepared.socket.clone()),
            Duration::from_millis(200),
        )
        .await
    {
        let _ = std::fs::remove_file(&prepared.socket);
    }

    // 03: the one thing that is cleaned — session cgroups left under this instance's subtree
    // by a previous manager run are `cgroup.kill`ed before anything binds. An absent subtree
    // (off mode) is a no-op. A sweep failure is reported, not fatal: leftover scopes cannot
    // collide with new sessions (fresh ids), so the honest line beats a refused start.
    if let Some(root) = &prepared.sweep_root {
        match sweep_session_subtree(root) {
            Ok(count) => eprintln!("startup sweep: {count} stale session scope(s) reaped"),
            Err(error) => eprintln!("startup sweep failed under {}: {error}", root.display()),
        }
    }

    let listener = Listener::bind(&prepared.socket)
        .await
        .map_err(|error| ErrorEnvelope::internal(format!("manager socket: {error}")))?;
    // 07: `manager.bind` adds the TCP surface beside the unix socket. A bind failure is a
    // startup failure (the operator asked for the surface explicitly) — never a silent
    // unix-only start.
    let network = match &prepared.bind {
        Some((host, port)) => Some(NetworkListener::bind(host, *port).await.map_err(|error| {
            ErrorEnvelope::internal(format!("manager bind {host}:{port}: {error}"))
        })?),
        None => None,
    };
    // The pinned startup report (04); stderr, so `--json` stdout stays machine-readable.
    eprintln!(
        "{}",
        strings::started_manager(
            prepared.mode.as_str(),
            std::process::id(),
            &prepared.socket.display().to_string(),
        )
    );
    if let Some(network) = &network {
        // 04: the network surface reports the address it actually bound (`:0` resolves).
        eprintln!("listening on {}", network.authority());
    }
    if let Some(notice) = prepared.notice {
        eprintln!("{notice}");
    }

    let handler = prepared.handler;
    let media = prepared.media;
    tokio::select! {
        _ = listener.serve(Arc::clone(&handler), Arc::clone(&media)) => {}
        _ = async {
            match network {
                Some(network) => network.serve(handler, media).await,
                None => std::future::pending::<()>().await,
            }
        } => {}
        _ = shutdown => {}
    }
    let _ = std::fs::remove_file(&prepared.socket);
    Ok(())
}

/// SIGINT and SIGTERM both end the loop; cleanup runs after (`Drop` of the spawner kills the
/// session trees, "signals cascade to everything it owns" — 04).
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Toggle;
    use crate::ipc::methods;
    use crate::manager::startup::{DelegatedProbe, Delegation};
    use crate::manager::{RendererMark, SessionSpec, SpawnOutcome};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailingProbe(&'static str);

    impl ContainmentProbe for FailingProbe {
        fn probe(&self) -> Result<Delegation, String> {
            Err(self.0.to_string())
        }
    }

    #[derive(Default)]
    struct CountingProbe(AtomicUsize);

    impl ContainmentProbe for CountingProbe {
        fn probe(&self) -> Result<Delegation, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err("probe must not be consulted".to_string())
        }
    }

    #[derive(Default)]
    struct StubSpawner(StdMutex<Vec<String>>);

    impl SessionSpawner for StubSpawner {
        fn spawn(
            &self,
            _spec: &SessionSpec,
            _allow_software: bool,
        ) -> Result<SpawnOutcome, ErrorEnvelope> {
            let id = uuid::Uuid::new_v4().to_string();
            self.0.lock().unwrap().push(id.clone());
            Ok(SpawnOutcome {
                id,
                renderer: RendererMark::Unverified,
            })
        }

        fn kill_tree(&self, _session_id: &str) -> Result<(), ErrorEnvelope> {
            Ok(())
        }

        fn capture_env(&self, _session_id: &str) -> Option<crate::manager::CaptureEnv> {
            None
        }
    }

    fn deps(tmp: &Path, config: CoreConfig, probe: Box<dyn ContainmentProbe>) -> ServeDeps {
        ServeDeps {
            paths: Paths::for_env(Some(tmp), Some(tmp), Some(tmp), "default"),
            config,
            spawner: Arc::new(StubSpawner::default()),
            probe,
            plan: None,
            session_subtree: None,
            socket: tmp.join("manager.sock"),
            lock_wait: Duration::from_millis(300),
        }
    }

    fn off_mode_config() -> CoreConfig {
        let mut config = CoreConfig::default();
        config.limits.mode = Toggle::Off;
        config
    }

    fn on_mode_config() -> CoreConfig {
        let mut config = CoreConfig::default();
        config.limits.mode = Toggle::On;
        config
    }

    #[test]
    fn refusal_carries_the_fixture_contract_exactly() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CoreConfig::default();
        config.limits.mode = Toggle::On;
        let deps = deps(
            tmp.path(),
            config,
            Box::new(FailingProbe("cgroupfs is mounted read-only")),
        );
        let Err(envelope) = prepare(&deps) else {
            panic!("the on-mode start must refuse without delegation");
        };
        assert_eq!(envelope.code, crate::ipc::ErrorCode::LimitsUnavailable);
        // The same message the `manager-start-limits-unavailable` fixture pins, verbatim.
        assert_eq!(
            envelope.message,
            crate::strings::containment_unavailable("cgroupfs is mounted read-only")
        );
        assert_eq!(
            envelope.hint.as_deref(),
            Some("set limits.mode = off to accept the documented downgrade")
        );
        // And the line the launcher parses back: one JSON envelope in the frame's error form.
        let line = startup_failure_line(&envelope);
        let value: serde_json::Value = serde_json::from_str(&line).expect("one JSON line");
        assert_eq!(value["error"]["code"], "limits_unavailable");
        assert_eq!(value["error"]["message"], envelope.message);
    }

    #[test]
    fn off_mode_proceeds_with_the_notice_and_never_probes() {
        let tmp = tempfile::tempdir().unwrap();
        let probe = Arc::new(CountingProbe::default());
        struct SharedProbe(Arc<CountingProbe>);
        impl ContainmentProbe for SharedProbe {
            fn probe(&self) -> Result<Delegation, String> {
                self.0.probe()
            }
        }
        let deps = deps(
            tmp.path(),
            off_mode_config(),
            Box::new(SharedProbe(Arc::clone(&probe))),
        );
        let prepared = prepare(&deps).expect("off-mode proceeds");
        assert_eq!(
            probe.0.load(Ordering::SeqCst),
            0,
            "off-mode must not touch the cgroupfs"
        );
        assert!(prepared.notice.is_some(), "the downgrade is announced");
        assert!(prepared.notice.unwrap().contains("no per-session limits"));
    }

    #[test]
    fn instance_id_persists_at_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::for_env(
            Some(tmp.path()),
            Some(tmp.path()),
            Some(tmp.path()),
            "default",
        );
        let first = read_or_create_instance_id(&paths).unwrap();
        let second = read_or_create_instance_id(&paths).unwrap();
        assert_eq!(first, second);
        let mode = std::fs::metadata(paths.data_dir.join("instance-id"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn lock_is_exclusive_and_released_by_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("manager.sock");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (guard, loser) = runtime.block_on(async {
            let holder = acquire_lock(tmp.path(), &socket, Duration::from_millis(100))
                .await
                .unwrap();
            assert!(matches!(holder, Lock::Holder(_)));
            // No socket answers; the second attempt exhausts its wait, retries once, and fails
            // against the guard still held in this process.
            let loser = acquire_lock(tmp.path(), &socket, Duration::from_millis(100)).await;
            (holder, loser)
        });
        assert!(loser.is_err(), "second acquire must not win");
        drop(guard);
        let again = runtime.block_on(acquire_lock(
            tmp.path(),
            &socket,
            Duration::from_millis(100),
        ));
        assert!(
            matches!(again, Ok(Lock::Holder(_))),
            "kernel released the lock"
        );
    }

    #[tokio::test]
    async fn run_serves_the_whole_stack_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let prepared = prepare(&deps(
            tmp.path(),
            off_mode_config(),
            Box::new(FailingProbe("never consulted in off mode")),
        ))
        .unwrap();
        let socket = prepared.socket.clone();

        let serving = tokio::spawn(run_until(
            prepared,
            tokio::time::sleep(Duration::from_millis(600)),
        ));

        // Wait for the socket to answer, then drive the real wire path.
        let address = Address::Unix(socket.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !Client::probe(&address, Duration::from_millis(100)).await {
            assert!(Instant::now() < deadline, "manager never came up");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let mut client = Client::connect(&address, Duration::from_secs(2), None)
            .await
            .expect("connect");
        let handshake = client.handshake().await.expect("handshake");
        assert_eq!(handshake.protocol_version, crate::ipc::PROTOCOL_VERSION);

        let status = client
            .request(methods::STATUS, None, None)
            .await
            .expect("status");
        assert_eq!(status["state"], "running");
        assert_eq!(status["mode"], "background", "no tty in tests → background");

        let created = client
            .request(methods::CREATE_SESSION, Some(json!({})), None)
            .await
            .expect("create");
        assert_eq!(created["state"], "running");
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["instance_id"], status["instance_id"]);

        let closed = client
            .request(
                methods::CLOSE_SESSION,
                Some(json!({"session_id": id})),
                None,
            )
            .await
            .expect("close");
        assert_eq!(closed["state"], "closed");

        serving.await.expect("serve task").expect("clean shutdown");
        assert!(!socket.exists(), "socket file removed on clean shutdown");
    }

    #[test]
    fn loser_exits_cleanly_when_a_manager_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("manager.sock");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            // A live manager: someone else holds the lock and answers on the socket.
            let _squatter = tokio::net::UnixListener::bind(&socket).unwrap();
            let lock_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(tmp.path().join("manager.lock"))
                .unwrap();
            let lock_fd = std::os::unix::io::AsRawFd::as_raw_fd(&lock_file);
            let rc = unsafe { libc::flock(lock_fd, libc::LOCK_EX) };
            assert_eq!(rc, 0, "test holds the runtime lock");
            let prepared = prepare(&deps(
                tmp.path(),
                off_mode_config(),
                Box::new(FailingProbe("never consulted in off mode")),
            ))
            .unwrap();
            let outcome = run_until(prepared, std::future::pending()).await;
            assert!(outcome.is_ok(), "loser exits cleanly: {outcome:?}");
            assert!(socket.exists(), "the winner's socket is untouched");
            drop(lock_file);
        });
    }

    #[tokio::test]
    async fn stale_socket_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        // A dead manager leaves its socket file behind (kernel unlinks nothing on crash).
        std::fs::write(tmp.path().join("manager.sock"), b"").unwrap();
        let prepared = prepare(&deps(
            tmp.path(),
            off_mode_config(),
            Box::new(FailingProbe("never consulted in off mode")),
        ))
        .unwrap();
        let socket = prepared.socket.clone();
        let serving = tokio::spawn(run_until(
            prepared,
            tokio::time::sleep(Duration::from_millis(400)),
        ));
        let address = Address::Unix(socket.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !Client::probe(&address, Duration::from_millis(100)).await {
            assert!(
                Instant::now() < deadline,
                "manager never came up past the stale socket"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        serving.await.unwrap().unwrap();
    }

    #[test]
    fn session_subtree_resolves_under_the_probes_cgroup_dir() {
        // Exactly the containment tests' pattern: a fake cgroupfs rooted at a tempdir.
        let tmp = tempfile::tempdir().unwrap();
        let root = LinuxCgroupProbe::with_paths(tmp.path(), "0::/\n");
        assert_eq!(
            resolve_session_subtree(&root).unwrap(),
            tmp.path()
                .join(crate::runtime::containment::SESSION_SUBTREE)
        );
        // A delegated scope: the base plus the delegated path (06 class 3).
        let scope =
            LinuxCgroupProbe::with_paths(tmp.path(), "0::/user.slice/user-4242.slice/run.scope\n");
        assert_eq!(
            resolve_session_subtree(&scope).unwrap(),
            tmp.path()
                .join("user.slice/user-4242.slice/run.scope")
                .join(crate::runtime::containment::SESSION_SUBTREE)
        );
    }

    #[test]
    fn enabled_plan_prepares_the_resolved_subtree_and_arms_the_sweep() {
        let tmp = tempfile::tempdir().unwrap();
        let subtree = tmp.path().join("daiorama-sessions");
        let mut deps = deps(
            tmp.path(),
            on_mode_config(),
            Box::new(DelegatedProbe::default()),
        );
        deps.session_subtree = Some(subtree.clone());
        let prepared = prepare(&deps).expect("an enabled plan proceeds");
        let control = std::fs::read_to_string(subtree.join("cgroup.subtree_control"))
            .expect("the subtree is prepared with its control file");
        for controller in ["+cpu", "+memory", "+pids"] {
            assert!(
                control.contains(controller),
                "subtree enables {controller}: {control}"
            );
        }
        assert_eq!(
            prepared.sweep_root,
            Some(subtree),
            "startup sweeps the subtree it prepared"
        );
        assert!(prepared.notice.is_none(), "no downgrade notice when on");
    }

    #[test]
    fn enabled_plan_without_a_resolved_subtree_skips_the_cgroupfs() {
        // The embedder shape: delegation present, no subtree resolved — nothing on the
        // cgroupfs is touched and the sweep stays off.
        let tmp = tempfile::tempdir().unwrap();
        let deps = deps(
            tmp.path(),
            on_mode_config(),
            Box::new(DelegatedProbe::default()),
        );
        let prepared = prepare(&deps).expect("an enabled plan proceeds");
        assert!(prepared.sweep_root.is_none());
        assert!(prepared.notice.is_none());
    }

    #[tokio::test]
    async fn startup_sweep_clears_stale_scopes_before_binding() {
        let tmp = tempfile::tempdir().unwrap();
        // A stale scope a previous manager run left behind (empty here; the privileged
        // real-box run exercises the `cgroup.kill` leg against a live process tree).
        let subtree = tmp.path().join("daiorama-sessions");
        std::fs::create_dir_all(subtree.join("deadbeef")).unwrap();
        let mut prepared = prepare(&deps(
            tmp.path(),
            off_mode_config(),
            Box::new(FailingProbe("never consulted in off mode")),
        ))
        .unwrap();
        prepared.sweep_root = Some(subtree.clone());
        let serving = tokio::spawn(run_until(
            prepared,
            tokio::time::sleep(Duration::from_millis(300)),
        ));
        serving.await.unwrap().expect("clean shutdown");
        assert!(
            !subtree.join("deadbeef").exists(),
            "stale session scope swept before binding"
        );
    }
}
