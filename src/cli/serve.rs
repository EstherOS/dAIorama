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
//! Provisional shapes (additive, flagged as such — none is observable on the wire yet): the
//! lock file name (`manager.lock` beside the socket), the loser's notice line, the session
//! scratch root (`<data-root>/sessions/`), the instance-id file (`<data-root>/instance-id`,
//! created once, 0600), and the `DAIORAMA_MODE` env channel the launcher uses to hand the
//! chosen mode to the child it starts.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::config::{CoreConfig, Paths, StartMode};
use crate::ipc::{self, Address, Client, EnvView, ErrorEnvelope};
use crate::manager::{
    self, Handler, InstanceMode, Listener, SessionSpawner,
    startup::{ContainmentProbe, StartupDecision, plan_startup},
};
use crate::runtime::containment::LinuxCgroupProbe;
use crate::runtime::service::{RuntimeService, ServiceOptions};
use crate::runtime::spawner::{LinuxSpawner, SpawnerConfig};
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
    pub probe: Box<dyn ContainmentProbe>,
    /// The manager socket. Production derives it from the environment (08,
    /// `ipc::socket_path`); tests inject a temp path so no test touches the real runtime dir.
    pub socket: PathBuf,
    pub lock_wait: Duration,
}

/// The prepared, not-yet-bound manager: the startup decision already made.
pub struct Prepared {
    pub handler: Arc<dyn Handler>,
    pub socket: PathBuf,
    pub mode: InstanceMode,
    /// The recorded-downgrade notice when `limits.mode = off` (06); `None` otherwise.
    pub notice: Option<&'static str>,
    pub lock_wait: Duration,
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
    let spawner: Arc<dyn SessionSpawner> = Arc::new(LinuxSpawner::new(SpawnerConfig::new(
        paths.data_dir.join("sessions"),
    )));
    let deps = ServeDeps {
        paths,
        config,
        spawner,
        probe: Box::new(LinuxCgroupProbe::new()),
        socket: ipc::socket_path(instance),
        lock_wait: LOSER_WAIT,
    };
    let prepared = prepare(&deps)?;
    run_until(prepared, shutdown_signal()).await
}

/// The startup decision, made concrete: containment plan → service → socket path. No binding,
/// no spawning — everything here is reversible and unit-testable.
pub fn prepare(deps: &ServeDeps) -> Result<Prepared, ErrorEnvelope> {
    let plan = match plan_startup(&deps.config, deps.probe.as_ref()) {
        StartupDecision::Refuse(envelope) => return Err(envelope),
        StartupDecision::Proceed(plan) => plan,
    };
    // `plan.sweep` is recorded but vacuous here: sessions do not create cgroup scopes until
    // the containment-scope slice attaches (W2), so there is no subtree to sweep yet. When
    // that slice lands, a stale instance subtree is exactly what startup sweeps (03).
    let notice = manager::limits_disabled_notice(&plan);
    let mode = effective_mode(&deps.config);
    let instance_id = read_or_create_instance_id(&deps.paths)?;
    let socket = deps.socket.clone();
    let service = RuntimeService::new(
        ServiceOptions {
            instance_id,
            socket_path: socket.clone(),
            mode,
            config: deps.config.clone(),
            paths: deps.paths.clone(),
        },
        Arc::clone(&deps.spawner),
    );
    Ok(Prepared {
        handler: Arc::new(service),
        socket,
        mode,
        notice,
        lock_wait: deps.lock_wait,
    })
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

    let listener = Listener::bind(&prepared.socket)
        .await
        .map_err(|error| ErrorEnvelope::internal(format!("manager socket: {error}")))?;
    // The pinned startup report (04); stderr, so `--json` stdout stays machine-readable.
    eprintln!(
        "{}",
        strings::started_manager(
            prepared.mode.as_str(),
            std::process::id(),
            &prepared.socket.display().to_string(),
        )
    );
    if let Some(notice) = prepared.notice {
        eprintln!("{notice}");
    }

    tokio::select! {
        _ = listener.serve(prepared.handler) => {}
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
    use crate::manager::SessionSpec;
    use crate::manager::startup::Delegation;
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
        fn spawn(&self, _spec: &SessionSpec) -> Result<String, ErrorEnvelope> {
            let id = uuid::Uuid::new_v4().to_string();
            self.0.lock().unwrap().push(id.clone());
            Ok(id)
        }

        fn kill_tree(&self, _session_id: &str) -> Result<(), ErrorEnvelope> {
            Ok(())
        }
    }

    fn deps(tmp: &Path, config: CoreConfig, probe: Box<dyn ContainmentProbe>) -> ServeDeps {
        ServeDeps {
            paths: Paths::for_env(Some(tmp), Some(tmp), Some(tmp), "default"),
            config,
            spawner: Arc::new(StubSpawner::default()),
            probe,
            socket: tmp.join("manager.sock"),
            lock_wait: Duration::from_millis(300),
        }
    }

    fn off_mode_config() -> CoreConfig {
        let mut config = CoreConfig::default();
        config.limits.mode = Toggle::Off;
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
}
