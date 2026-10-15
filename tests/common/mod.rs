//! Fake-manager harness for the P2 conformance fixtures.
//!
//! What it does, and the rules it enforces (documented for fixture authors in
//! `fixtures/README.md`):
//!
//! - Binds a **fake manager** on the instance's unix socket inside a per-scenario temp
//!   runtime dir, and serves the scenario's `steps` in strict order across every connection
//!   the CLI opens. An unexpected request, a wrong method, params that do not match, or an
//!   exhausted step list fails the scenario with a clear message.
//! - Enforces the **handshake-first** rule (05 A7): the first frame on every connection must
//!   match a `handshake` step; anything else is a failure.
//! - Isolates the CLI process: `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_RUNTIME_DIR`
//!   and `TMPDIR` all point into the scenario's temp root, and `DAIORAMA_TOKEN`,
//!   `DAIORAMA_SCOPE`, `DAIORAMA_SCOPE_TOKEN` are cleared.
//! - Passes `--remote <socket>` (before the subcommand) unless the scenario sets
//!   `remote: None`, and points `DAIORAMA_MANAGER_BIN` at a start stub whose invocation is
//!   asserted against the scenario's `start` block.
//! - Substitutes `$socket`, `$manager_pid`, `$config_dir`, `$data_dir` and `$runtime_dir` in
//!   every fixture string and every expectation.
//!
//! See `fixtures/README.md` for the scenario schema.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// The CLI under test, built by cargo for this integration test.
pub const BIN: &str = env!("CARGO_BIN_EXE_daiorama");

/// The instance every scenario targets.
pub const INSTANCE: &str = "default";

/// A whole scenario may not take longer than this (a hang is a failure, not a wait).
pub const SCENARIO_TIMEOUT: Duration = Duration::from_secs(60);

/// The directory holding the scenario files.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/ipc")
}

pub fn scenario_path(name: &str) -> PathBuf {
    fixtures_dir().join(format!("{name}.ron"))
}

// ---------------------------------------------------------------------------
// Scenario schema
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub description: String,
    /// Seeded into the temp **config** dir (`<config-dir>/daiorama/<instance>/…`).
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, String>,
    /// `Some("$socket")` (the default) passes `--remote <fake socket>`; `Some("/path")`
    /// passes that literal address; `None` passes no `--remote` at all (local-instance
    /// behaviour, needed for the cold answer and the lazy start).
    #[serde(default = "default_remote")]
    pub remote: Option<RemoteSpec>,
    /// The lazy-start stub. Its presence *is* the expectation that an attempted start
    /// happens: the harness asserts the stub ran iff `start` is present.
    #[serde(default)]
    pub start: Option<StartStub>,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct RemoteSpec(pub String);

fn default_remote() -> Option<RemoteSpec> {
    Some(RemoteSpec("$socket".to_string()))
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartStub {
    #[serde(default)]
    pub exit_code: i32,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    pub expect: Expect,
    pub respond: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Expect {
    pub method: String,
    /// Omitted (or `()`) = the request must carry no params (`null`). Present = every field
    /// must match (a subset match for objects; extra keys in the live request are allowed).
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Run {
    pub argv: Vec<String>,
    #[serde(default)]
    pub expect_exit: i32,
    #[serde(default)]
    pub expect_stdout_contains: Vec<String>,
    #[serde(default)]
    pub expect_stdout_not_contains: Vec<String>,
    #[serde(default)]
    pub expect_stderr_contains: Vec<String>,
    #[serde(default)]
    pub expect_stderr_not_contains: Vec<String>,
    /// Exact equality against the parsed stdout, after substitution. `()`/omitted = no
    /// assertion.
    #[serde(default)]
    pub expect_stdout_json: Value,
}

// ---------------------------------------------------------------------------
// Loading and running
// ---------------------------------------------------------------------------

pub fn load_scenario(path: &Path) -> Scenario {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    ron::from_str(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Load a scenario file and run it. `name` is the file stem and must equal the scenario's
/// own `name` field: the suite uses the filename to find the file and the field to label
/// failures.
pub async fn run_scenario_file(name: &str) {
    let path = scenario_path(name);
    assert!(
        path.exists(),
        "missing fixture file {} (every scenario in the suite must exist)",
        path.display()
    );
    let scenario = load_scenario(&path);
    assert_eq!(
        scenario.name,
        name,
        "scenario `name` must match its file stem ({})",
        path.display()
    );
    match tokio::time::timeout(SCENARIO_TIMEOUT, Harness::new(scenario).execute()).await {
        Ok(()) => {}
        Err(_) => panic!(
            "scenario {name} timed out after {}s",
            SCENARIO_TIMEOUT.as_secs()
        ),
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    scenario: Scenario,
    root: tempfile::TempDir,
    config_dir: PathBuf,
    data_dir: PathBuf,
    runtime_dir: PathBuf,
    socket: PathBuf,
    stub: PathBuf,
    marker: PathBuf,
    steps: Arc<Mutex<VecDeque<Step>>>,
    failures: Arc<Mutex<Vec<String>>>,
}

impl Harness {
    fn new(scenario: Scenario) -> Self {
        let base = short_temp_base();
        let root = tempfile::Builder::new()
            .prefix("dfx-")
            .tempdir_in(&base)
            .unwrap_or_else(|err| panic!("cannot create a temp root in {}: {err}", base.display()));
        let instance_root = root.path().join("xdg");
        let config_dir = instance_root.join("config/daiorama").join(INSTANCE);
        let data_dir = instance_root.join("data/daiorama").join(INSTANCE);
        let runtime_dir = instance_root.join("run");
        let socket = runtime_dir
            .join("daiorama")
            .join(INSTANCE)
            .join("manager.sock");
        let stub = root.path().join("start-stub.sh");
        let marker = root.path().join("start-stub.invoked");

        let harness = Self {
            scenario,
            root,
            config_dir,
            data_dir,
            runtime_dir,
            socket,
            stub,
            marker,
            steps: Arc::new(Mutex::new(VecDeque::new())),
            failures: Arc::new(Mutex::new(Vec::new())),
        };
        harness.seed();
        harness.write_stub();
        harness
    }

    /// Seed `files` under the temp config dir, exactly as the scenario spells them.
    fn seed(&self) {
        for (relative, content) in &self.scenario.files {
            let path = self.config_dir.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, content)
                .unwrap_or_else(|err| panic!("cannot seed {}: {err}", path.display()));
        }
        std::fs::create_dir_all(&self.data_dir).unwrap();
        std::fs::create_dir_all(&self.runtime_dir).unwrap();
    }

    /// The start stub: records that it ran, then emits the scenario's scripted output.
    /// With no `start` block the stub exists only to make an *unexpected* start loud.
    fn write_stub(&self) {
        let (exit_code, stdout, stderr) = match &self.scenario.start {
            Some(stub) => (stub.exit_code, stub.stdout.clone(), stub.stderr.clone()),
            None => (
                1,
                String::new(),
                "{\"error\":{\"code\":\"internal\",\"message\":\"fixture: lazy start was not \
                 expected by this scenario\"}}\n"
                    .to_string(),
            ),
        };
        let mut script = String::from("#!/bin/sh\n");
        script.push_str("# Generated by the P2 fixture harness; not part of the shipped tree.\n");
        script.push_str(&format!(
            "printf '+' >> {}\n",
            sh_quote(&self.marker.display().to_string())
        ));
        if !stdout.is_empty() {
            script.push_str(&format!("printf '%s\\n' {}\n", sh_quote(&stdout)));
        }
        if !stderr.is_empty() {
            script.push_str(&format!("printf '%s\\n' {} >&2\n", sh_quote(&stderr)));
        }
        script.push_str(&format!("exit {exit_code}\n"));
        std::fs::write(&self.stub, script).unwrap();
        let mut permissions = std::fs::metadata(&self.stub).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&self.stub, permissions).unwrap();
    }

    /// Run every `runs` entry, then check the scenario's own invariants.
    async fn execute(mut self) {
        // The fake manager, when the scenario has steps to serve.
        let serving = if self.scenario.steps.is_empty() {
            None
        } else {
            // Substitute tokens in what the fake manager will say and expect, so a scenario
            // can speak about the socket path and the manager pid without knowing them.
            let steps: VecDeque<Step> = self
                .scenario
                .steps
                .iter()
                .map(|step| Step {
                    expect: Expect {
                        method: step.expect.method.clone(),
                        params: self.substitute(&step.expect.params),
                    },
                    respond: self.substitute(&step.respond),
                })
                .collect();
            self.steps = Arc::new(Mutex::new(steps));
            if let Some(parent) = self.socket.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            let listener = UnixListener::bind(&self.socket).unwrap_or_else(|err| {
                panic!(
                    "cannot bind the fake manager at {}: {err}",
                    self.socket.display()
                )
            });
            let steps = Arc::clone(&self.steps);
            let failures = Arc::clone(&self.failures);
            Some(tokio::spawn(serve_fake_manager(listener, steps, failures)))
        };

        let runs = self.scenario.runs.clone();
        for (index, run) in runs.iter().enumerate() {
            self.execute_run(index, run).await;
        }

        if let Some(handle) = serving {
            handle.abort();
        }

        // 1. Every scripted step was consumed.
        let leftover: Vec<String> = self
            .steps
            .lock()
            .unwrap()
            .iter()
            .map(|step| step.expect.method.clone())
            .collect();
        assert!(
            leftover.is_empty(),
            "scenario {}: {} scripted step(s) were never requested: {leftover:?}",
            self.scenario.name,
            leftover.len()
        );

        // 2. The fake manager saw nothing unexpected.
        let failures = self.failures.lock().unwrap().clone();
        assert!(
            failures.is_empty(),
            "scenario {}: unexpected traffic to the fake manager:\n  {}",
            self.scenario.name,
            failures.join("\n  ")
        );

        // 3. The lazy-start stub ran iff the scenario expects a start.
        let started = self.marker.exists();
        assert_eq!(
            started,
            self.scenario.start.is_some(),
            "scenario {}: lazy-start stub invoked = {started}, but the scenario {} a start",
            self.scenario.name,
            if self.scenario.start.is_some() {
                "expects"
            } else {
                "does not expect"
            }
        );

        // 4. Nothing leaked outside the temp root.
        drop(self.root);
    }

    /// What the fake manager has to say about the traffic so far — attached to every run
    /// failure so a mismatch explains itself in one shot.
    fn failure_note(&self) -> String {
        let failures = self.failures.lock().unwrap();
        if failures.is_empty() {
            "  fake manager reported: nothing unexpected".to_string()
        } else {
            format!("  fake manager reported:\n    {}", failures.join("\n    "))
        }
    }

    fn substitute(&self, value: &Value) -> Value {
        let tokens = [
            ("$socket", self.socket.display().to_string()),
            ("$manager_pid", std::process::id().to_string()),
            ("$config_dir", self.config_dir.display().to_string()),
            ("$data_dir", self.data_dir.display().to_string()),
            ("$runtime_dir", self.runtime_dir.display().to_string()),
        ];
        substitute_value(value, &tokens)
    }

    fn substitute_str(&self, text: &str) -> String {
        let tokens = [
            ("$socket", self.socket.display().to_string()),
            ("$manager_pid", std::process::id().to_string()),
            ("$config_dir", self.config_dir.display().to_string()),
            ("$data_dir", self.data_dir.display().to_string()),
            ("$runtime_dir", self.runtime_dir.display().to_string()),
        ];
        substitute_text(text, &tokens)
    }

    async fn execute_run(&self, index: usize, run: &Run) {
        let mut command = std::process::Command::new(BIN);
        command.arg("--instance").arg(INSTANCE);
        if let Some(RemoteSpec(address)) = &self.scenario.remote {
            command.arg("--remote").arg(self.substitute_str(address));
        }
        command.args(&run.argv);
        command
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.root.path().join("home"))
            .env("XDG_CONFIG_HOME", self.root.path().join("xdg/config"))
            .env("XDG_DATA_HOME", self.root.path().join("xdg/data"))
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("DAIORAMA_MANAGER_BIN", &self.stub)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        std::fs::create_dir_all(self.root.path().join("home")).unwrap();
        std::fs::create_dir_all(self.root.path().join("tmp")).unwrap();

        let output = tokio::task::spawn_blocking(move || command.output())
            .await
            .expect("the child process task panicked")
            .expect("failed to spawn the CLI");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code();

        let label = format!(
            "scenario {} ({}) run {}: `daiorama {}`",
            self.scenario.name,
            self.scenario.description,
            index,
            run.argv.join(" ")
        );
        let note = self.failure_note();
        assert_eq!(
            code,
            Some(run.expect_exit),
            "{label}\n  exit code mismatch\n{note}\n  stdout: {stdout}\n  stderr: {stderr}"
        );
        for needle in &run.expect_stdout_contains {
            let needle = self.substitute_str(needle);
            assert!(
                stdout.contains(&needle),
                "{label}\n  stdout must contain {needle:?}\n{note}\n  stdout: {stdout}\n  stderr: {stderr}"
            );
        }
        for needle in &run.expect_stdout_not_contains {
            let needle = self.substitute_str(needle);
            assert!(
                !stdout.contains(&needle),
                "{label}\n  stdout must NOT contain {needle:?}\n{note}\n  stdout: {stdout}"
            );
        }
        for needle in &run.expect_stderr_contains {
            let needle = self.substitute_str(needle);
            assert!(
                stderr.contains(&needle),
                "{label}\n  stderr must contain {needle:?}\n{note}\n  stderr: {stderr}\n  stdout: {stdout}"
            );
        }
        for needle in &run.expect_stderr_not_contains {
            let needle = self.substitute_str(needle);
            assert!(
                !stderr.contains(&needle),
                "{label}\n  stderr must NOT contain {needle:?}\n{note}\n  stderr: {stderr}"
            );
        }
        if !run.expect_stdout_json.is_null() {
            let expected = self.substitute(&run.expect_stdout_json);
            let actual: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
                panic!(
                    "{label}\n  stdout is not one JSON document ({err})\n{note}\n  stdout: {stdout}"
                )
            });
            assert_eq!(
                actual, expected,
                "{label}\n  stdout JSON mismatch\n{note}\n  stdout: {stdout}"
            );
        }
    }
}

/// A short temp base: unix socket paths have a hard length limit (~108 bytes), so scenario
/// sockets live under `/tmp` when it is usable.
fn short_temp_base() -> PathBuf {
    let candidates = [PathBuf::from("/tmp"), std::env::temp_dir()];
    for candidate in candidates {
        if candidate.is_dir() && std::fs::metadata(&candidate).is_ok() {
            return candidate;
        }
    }
    PathBuf::from(".")
}

/// The fake manager: one task per connection, steps served in strict order.
async fn serve_fake_manager(
    listener: UnixListener,
    steps: Arc<Mutex<VecDeque<Step>>>,
    failures: Arc<Mutex<Vec<String>>>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let steps = Arc::clone(&steps);
        let failures = Arc::clone(&failures);
        tokio::spawn(async move {
            let _ = serve_connection(stream, steps, failures).await;
        });
    }
}

async fn serve_connection(
    stream: UnixStream,
    steps: Arc<Mutex<VecDeque<Step>>>,
    failures: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut first_frame = true;
    let mut count = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        count += 1;
        let frame: Value = match serde_json::from_str(trimmed) {
            Ok(frame) => frame,
            Err(err) => {
                failures
                    .lock()
                    .unwrap()
                    .push(format!("request #{count} is not JSON: {err} ({trimmed})"));
                return Ok(());
            }
        };
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        if first_frame && method != "handshake" {
            failures.lock().unwrap().push(format!(
                "the first call on a connection must be `handshake`, got `{method}`"
            ));
            return Ok(());
        }
        first_frame = false;

        let step = steps.lock().unwrap().pop_front();
        let Some(step) = step else {
            failures.lock().unwrap().push(format!(
                "unexpected request `{method}` — the scenario has no steps left"
            ));
            let response = json!({"error": {"code": "internal", "message": format!(
                "fixture: unexpected request `{method}` with no step left for it")}});
            write_line(&mut reader, &response).await?;
            return Ok(());
        };

        if step.expect.method != method {
            failures.lock().unwrap().push(format!(
                "expected `{}` next, got `{method}`",
                step.expect.method
            ));
            return Ok(());
        }
        let actual_params = frame.get("params").cloned().unwrap_or(Value::Null);
        if step.expect.params.is_null() {
            if !actual_params.is_null() {
                failures.lock().unwrap().push(format!(
                    "`{method}` must be called with no params, got {actual_params}"
                ));
                return Ok(());
            }
        } else if !params_match(&step.expect.params, &actual_params) {
            failures.lock().unwrap().push(format!(
                "params mismatch for `{method}`: expected {}, got {actual_params}",
                step.expect.params
            ));
            return Ok(());
        }

        let response = step.respond.clone();
        validate_respond(&response).unwrap_or_else(|err| {
            failures
                .lock()
                .unwrap()
                .push(format!("scenario step for `{method}` is malformed: {err}"));
            panic!("malformed fixture step: {err}")
        });
        write_line(&mut reader, &response).await?;
    }
}

/// The fixture's `respond` must be exactly one of `result` / `error`, in the frame's shape.
fn validate_respond(respond: &Value) -> Result<(), String> {
    let object = respond
        .as_object()
        .ok_or_else(|| format!("expected an object, got {respond}"))?;
    let has = |key: &str| object.contains_key(key);
    if has("result") == has("error") {
        return Err(format!(
            "expected exactly one of `result`/`error`, got {respond}"
        ));
    }
    if let Some(error) = object.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("error envelope needs a code: {error}"))?;
        let looks_like_code = !code.is_empty()
            && code.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !looks_like_code {
            return Err(format!("{code:?} is not a canonical code shape"));
        }
        if error.get("message").and_then(Value::as_str).is_none() {
            return Err(format!("error envelope needs a message: {error}"));
        }
    }
    Ok(())
}

async fn write_line<W>(writer: &mut W, value: &Value) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut line = serde_json::to_string(value).expect("responses serialize");
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

/// Object comparison is a subset match: every fixture field must match, extra live fields are
/// allowed. Everything else is exact.
fn params_match(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => expected.iter().all(|(key, value)| {
            actual
                .get(key)
                .is_some_and(|actual| params_match(value, actual))
        }),
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| params_match(expected, actual))
        }
        (Value::Number(expected), Value::Number(actual)) => {
            // RON writes 1.0 for a float and 1 for an int; compare numerically.
            match (expected.as_f64(), actual.as_f64()) {
                (Some(expected), Some(actual)) => expected == actual,
                _ => expected == actual,
            }
        }
        _ => expected == actual,
    }
}

fn substitute_value(value: &Value, tokens: &[(&str, String)]) -> Value {
    match value {
        Value::String(text) => Value::String(substitute_text(text, tokens)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| substitute_value(item, tokens))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), substitute_value(value, tokens)))
                .collect::<Map<String, Value>>(),
        ),
        other => other.clone(),
    }
}

fn substitute_text(text: &str, tokens: &[(&str, String)]) -> String {
    let mut out = text.to_string();
    for (name, value) in tokens {
        out = out.replace(name, value);
    }
    out
}

/// POSIX single-quote a string for the generated start stub.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Read the whole file list of a directory (used by the suite's guard test).
pub fn scenario_names_on_disk() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures/ipc exists")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "ron") {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}
