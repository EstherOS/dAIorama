//! CLI: the full `docs/04-cli.md` surface, and its implementations.
//!
//! Two rules shape this module:
//!
//! - **Cold vs hot (04 §Cold vs hot).** A cold command never starts a local manager: with no
//!   manager it answers `no manager running` on stdout and exits 0 (an answer, not an error).
//!   `new` is the only hot command: it may lazily start a local manager, and the start is
//!   reported on the pinned line. With `--remote`, nothing local is ever started and an
//!   unreachable remote is the `manager unreachable at {addr}` error — never a local fallback.
//! - **Exit codes.** `0` for answers (including the cold answer), `1` for an error envelope.
//!   `--json` prints the RPC response payload unmodified on stdout, with errors as the
//!   `{code,message,hint?}` envelope verbatim on stderr.
//!
//! Commands whose behaviour needs the runtime: `serve` and `daemon start|end` are live (W3) —
//! the real manager behind the pinned surface; `doctor` is live (W3, `doctor` module: the
//! host-runnable environment check, 04); `mcp` (stdio) is live (P3 W1: `mcp::server`), and
//! `mcp --listen` plus `dashboard *` answer the not-implemented envelope until their slices
//! land.

use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use serde_json::{Map, Value, json};

use crate::config::{self, CoreConfig, Paths, ScopeFile, effective_view, parse_value};
use crate::ipc::{
    Address, BINARY_VERSION, Client, DEFAULT_INSTANCE, EnvView, ErrorEnvelope, MIN_SUPPORTED,
    PROTOCOL_VERSION, json_str, methods, params_object,
};
use crate::strings;

mod daemon;
mod doctor;
mod serve;

// ---------------------------------------------------------------------------
// clap surface (04, complete)
// ---------------------------------------------------------------------------

/// How long a hot command waits for a lazily started manager to answer.
const START_DEADLINE: Duration = Duration::from_secs(10);
/// Poll interval while waiting for a started manager.
const START_POLL: Duration = Duration::from_millis(50);
/// Connect timeout for one attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// 04 pinned default for `drain --timeout`.
const DRAIN_TIMEOUT_DEFAULT: u64 = 300;

#[derive(Debug, Parser)]
#[command(
    name = "daiorama",
    version = BINARY_VERSION,
    about = "Headless desktop sessions over a single manager per instance.",
    long_about = "dAIorama runs headless desktop sessions behind one manager per instance.\n\n\
                  Cold commands answer `no manager running` when nothing is running; `new` may \
                  lazily start one. `--remote` never starts anything locally."
)]
pub struct Cli {
    /// Which local instance to target.
    #[arg(long, global = true, default_value = DEFAULT_INSTANCE)]
    pub instance: String,

    /// Which manager to talk to: host:port or a literal unix socket path.
    #[arg(long, global = true)]
    pub remote: Option<String>,

    /// Machine-readable output on read/status commands.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a session and print its id.
    New(NewArgs),
    /// Explicit teardown of a session. Always explicit; never a side effect.
    Close { session: String },
    /// List sessions with status, scope, origin and idle-since.
    List(ListArgs),
    /// Instance status, or session detail with an id.
    Status { session: Option<String> },
    /// One control action on a session.
    Control(ControlArgs),
    /// Print the view address (starting it if needed), or stop it.
    View(ViewArgs),
    /// Read the history record.
    History(HistoryArgs),
    /// Fetch a captured media artifact by id.
    Media(MediaArgs),
    /// Run the manager in the foreground.
    Serve,
    /// Start or stop the detached manager.
    Daemon(DaemonArgs),
    /// Enter drain: new sessions refused, running sessions reported.
    Drain(DrainArgs),
    /// Read and write config.
    Config(ConfigArgs),
    /// Instance token operations.
    Token(TokenArgs),
    /// Run the MCP bridge.
    Mcp(McpArgs),
    /// Host-runnable environment check.
    Doctor,
    /// Dashboard module operations.
    Dashboard(DashboardArgs),
    /// Core version, protocol version and min_supported.
    Version,
}

#[derive(Debug, Args)]
pub struct MediaArgs {
    #[command(subcommand)]
    pub action: MediaAction,
}

#[derive(Debug, Subcommand)]
pub enum MediaAction {
    /// Fetch one media id to a file (claim-on-open: this fetch consumes the id).
    Fetch {
        /// 32 lowercase hex artifact id.
        id: String,
        /// Where to write the artifact (required; stdout stays clean for harnesses).
        #[arg(long)]
        out: std::path::PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// headless | nested | host
    #[arg(long, default_value = "headless")]
    pub backend: String,
    /// Creation resolution, WxH.
    #[arg(long, default_value = "1920x1080")]
    pub resolution: String,
    /// Creation scale.
    #[arg(long, default_value_t = crate::manager::DEFAULT_SCALE)]
    pub scale: f64,
    /// Start a view for the new session.
    #[arg(long)]
    pub view: bool,
    /// Mutable label; never an identifier.
    #[arg(long)]
    pub label: Option<String>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(long)]
    pub limit: Option<u32>,
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Debug, Args)]
pub struct ControlArgs {
    /// Session id.
    pub session: String,
    /// An MCP control-tool name.
    pub action: String,
    /// That tool's named arguments, as key=value.
    #[arg(value_name = "key=value")]
    pub args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    pub session: String,
    /// Stop the view instead of printing its address.
    #[arg(long)]
    pub stop: bool,
}

#[derive(Debug, Args)]
pub struct HistoryArgs {
    pub session: Option<String>,
    #[arg(long)]
    pub scope: Option<String>,
    #[arg(long)]
    pub limit: Option<u32>,
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub action: DaemonAction,
}

#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Start the manager detached (survives shell exit).
    Start,
    /// Stop the manager.
    End {
        /// Stop the manager and everything it owns (a session drop, stated in output).
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Args)]
pub struct DrainArgs {
    /// Block until zero running sessions.
    #[arg(long)]
    pub wait: bool,
    /// Bound for --wait.
    #[arg(long, default_value_t = DRAIN_TIMEOUT_DEFAULT)]
    pub timeout: u64,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Read one dot-path key.
    Get { key: String },
    /// Write one dot-path key.
    Set { key: String, value: String },
    /// $EDITOR on the core config file (local only).
    Edit,
    /// First-run wizard: write a commented starter config and scope file.
    Setup,
    /// Re-read core config on a running manager (never starts one).
    Reload,
    /// Validate core config and/or scope files.
    Validate { path: Option<PathBuf> },
    /// Scope definitions: read them, and create one from a template.
    Scope(ScopeArgs),
    /// Resolved view: which values apply where.
    Effective {
        #[arg(long)]
        scope: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct ScopeArgs {
    #[command(subcommand)]
    pub action: ScopeAction,
}

#[derive(Debug, Subcommand)]
pub enum ScopeAction {
    /// List scope definitions.
    List,
    /// Print one scope definition.
    Get { name: String },
    /// Validate one scope definition.
    Validate { name: String },
    /// Write a new scope definition from a template (prints its path).
    Create {
        name: String,
        /// own (default) | all | dashboard
        #[arg(long, default_value = "own")]
        template: String,
    },
}

#[derive(Debug, Args)]
pub struct TokenArgs {
    #[command(subcommand)]
    pub action: TokenAction,
}

#[derive(Debug, Subcommand)]
pub enum TokenAction {
    /// Print the local instance token and its file path.
    Show,
    /// Regenerate the token file.
    Rotate,
}

#[derive(Debug, Args)]
pub struct McpArgs {
    /// Run as a network service on this address instead of stdio.
    #[arg(long)]
    pub listen: Option<String>,
}

#[derive(Debug, Args)]
pub struct DashboardArgs {
    #[command(subcommand)]
    pub action: DashboardAction,
}

#[derive(Debug, Subcommand)]
pub enum DashboardAction {
    /// Serve the dashboard (module must be installed).
    Serve,
    /// Install the dashboard module.
    Install {
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        from: Option<String>,
    },
    /// Add a manager connection.
    Connect { address: String },
    /// Manage manager connections.
    Connections(ConnectionsArgs),
    /// Store or remove per-manager tokens.
    Token(TokenConnectionArgs),
}

#[derive(Debug, Args)]
pub struct ConnectionsArgs {
    #[command(subcommand)]
    pub action: ConnectionsAction,
}

#[derive(Debug, Subcommand)]
pub enum ConnectionsAction {
    List,
    Remove { name: String },
}

#[derive(Debug, Args)]
pub struct TokenConnectionArgs {
    #[command(subcommand)]
    pub action: TokenConnectionAction,
}

#[derive(Debug, Subcommand)]
pub enum TokenConnectionAction {
    Set { connection: String },
    Remove { connection: String },
}

// ---------------------------------------------------------------------------
// Answers and output
// ---------------------------------------------------------------------------

/// One command's answer: the RPC payload (`--json`) and the human rendering.
#[derive(Debug, Clone)]
pub struct Answer {
    pub payload: Value,
    pub human: Vec<String>,
}

impl Answer {
    fn new(payload: Value, human: Vec<String>) -> Self {
        Self { payload, human }
    }

    fn from_lines(human: Vec<String>) -> Self {
        Self::new(Value::Null, human)
    }

    /// The cold answer (04): an answer, not an error — the two-field envelope and exit 0.
    fn cold_no_manager() -> Self {
        Self::new(
            json!({"code": "no_manager_running", "message": strings::NO_MANAGER_RUNNING}),
            vec![strings::NO_MANAGER_RUNNING.to_string()],
        )
    }
}

/// Which class a command belongs to (04 §Cold vs hot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Never starts a local manager.
    Cold,
    /// May lazily start a local manager when none is reachable.
    Hot,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the CLI and return its process exit code.
pub async fn run() -> ExitCode {
    let cli = Cli::parse();
    let app = App::new(&cli);
    match app.dispatch(cli.command).await {
        Ok(answer) => {
            app.print_answer(&answer);
            ExitCode::SUCCESS
        }
        Err(envelope) => {
            app.print_error(&envelope);
            ExitCode::from(1)
        }
    }
}

struct App {
    instance: String,
    remote: Option<String>,
    json: bool,
}

impl App {
    fn new(cli: &Cli) -> Self {
        Self {
            instance: cli.instance.clone(),
            remote: cli.remote.clone(),
            json: cli.json,
        }
    }

    fn paths(&self) -> Paths {
        Paths::from_process(&self.instance)
    }

    /// 04: `--json` prints the response payload unmodified; errors emit the envelope verbatim
    /// on stderr so stdout stays machine-readable.
    fn print_answer(&self, answer: &Answer) {
        if self.json {
            if !answer.payload.is_null() {
                println!(
                    "{}",
                    serde_json::to_string(&answer.payload).expect("payloads serialize")
                );
            }
        } else {
            for line in &answer.human {
                println!("{line}");
            }
        }
    }

    fn print_error(&self, envelope: &ErrorEnvelope) {
        if self.json {
            eprintln!(
                "{}",
                serde_json::to_string(envelope).expect("envelopes serialize")
            );
        } else {
            eprintln!("{}", strings::error_line(&envelope.message, envelope.code));
            if let Some(hint) = &envelope.hint {
                eprintln!("{}", strings::hint_line(hint));
            }
        }
    }

    /// The client token, exact env name `DAIORAMA_TOKEN` (04).
    fn token() -> Option<String> {
        std::env::var("DAIORAMA_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
    }

    /// The operator scope's network secret, exact env name `DAIORAMA_SCOPE_TOKEN` (07). Only
    /// the TCP transport presents it; the unix socket's frames stay credential-free.
    fn scope_token() -> Option<String> {
        std::env::var("DAIORAMA_SCOPE_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
    }

    /// Resolve `--remote` to an address (04: one flag, one meaning, everywhere).
    fn address(&self) -> Result<Option<Address>, ErrorEnvelope> {
        match &self.remote {
            Some(raw) => Address::parse(raw).map(Some),
            None => Ok(None),
        }
    }

    /// Connect for a command of the given class. `Ok(None)` is the cold answer
    /// (`no manager running`); `Err` is an error envelope.
    async fn connect(&self, class: Class) -> Result<Option<Client>, ErrorEnvelope> {
        let token = Self::token();
        match self.address()? {
            Some(address) => {
                let mut client =
                    match Client::connect(&address, CONNECT_TIMEOUT, token.as_deref()).await {
                        Ok(client) => client,
                        // 04: with --remote, unreachable is an error — never a local fallback.
                        Err(envelope) => return Err(envelope),
                    };
                if client.is_gated() {
                    // 07: over TCP the operator scope authenticates with its secret; the CLI
                    // never silently claims `operator` without one.
                    client.set_scope_token(Self::scope_token());
                }
                Ok(Some(client))
            }
            None => match Client::connect_local(&self.instance).await {
                Ok(client) => Ok(Some(client)),
                Err(_) => {
                    if class == Class::Hot {
                        self.start_local_manager().await?;
                        match Client::connect_local(&self.instance).await {
                            Ok(client) => Ok(Some(client)),
                            Err(envelope) => Err(envelope),
                        }
                    } else {
                        Ok(None)
                    }
                }
            },
        }
    }

    /// Connect and handshake, or return the cold answer.
    async fn session(&self, class: Class) -> Result<Option<Client>, ErrorEnvelope> {
        match self.connect(class).await? {
            Some(mut client) => {
                client.handshake().await?;
                Ok(Some(client))
            }
            None => Ok(None),
        }
    }

    /// The lazy start (04/10): spawn `serve` for this instance, wait for it to answer, and
    /// surface a startup failure as the envelope the child reported. The start is reported on
    /// the pinned line — written to stderr so `--json` stdout stays machine-readable.
    async fn start_local_manager(&self) -> Result<(), ErrorEnvelope> {
        let env = EnvView::from_process();
        let socket = env.socket_path(&self.instance);
        let address = Address::Unix(socket.clone());
        if Client::probe(&address, Duration::from_millis(200)).await {
            return Ok(());
        }
        let paths = self.paths();
        let config = CoreConfig::load_or_default(&paths.core_config())?;
        // 10: auto picks attached for a supervised/interactive process, background otherwise.
        // W3 owns the full heuristic; the contract pinned here is only the report line.
        let mode = match config.manager.start_mode {
            config::StartMode::Auto => {
                if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                    "attached"
                } else {
                    "background"
                }
            }
            config::StartMode::Attached => "attached",
            config::StartMode::Background => "background",
        };
        let program = manager_program();
        let mut child = std::process::Command::new(&program)
            .arg("serve")
            .arg("--instance")
            .arg(&self.instance)
            // Hand the chosen mode down: the child has no terminal to infer it from (04/10).
            // Provisional env channel, additive.
            .env("DAIORAMA_MODE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                ErrorEnvelope::internal(format!("cannot start {}: {err}", program.display()))
            })?;
        let pid = child.id();

        let deadline = std::time::Instant::now() + START_DEADLINE;
        loop {
            if Client::probe(&address, Duration::from_millis(200)).await {
                eprintln!(
                    "{}",
                    strings::started_manager(mode, pid, &self.instance_socket_display(&socket))
                );
                return Ok(());
            }
            if let Ok(Some(status)) = child.try_wait() {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read;
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return Err(match start_failure(&stderr) {
                    Some(envelope) => envelope,
                    None => ErrorEnvelope::internal(format!(
                        "manager start failed ({}): {}",
                        status
                            .code()
                            .map_or_else(|| "signal".to_string(), |code| code.to_string()),
                        stderr.trim()
                    )),
                });
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                return Err(ErrorEnvelope::internal(format!(
                    "manager did not answer within {}s of start; see its diagnostics",
                    START_DEADLINE.as_secs()
                )));
            }
            tokio::time::sleep(START_POLL).await;
        }
    }

    fn instance_socket_display(&self, socket: &std::path::Path) -> String {
        socket.display().to_string()
    }

    /// One RPC that answers with a read shape.
    async fn call(
        &self,
        class: Class,
        method: &str,
        params: Option<Value>,
    ) -> Result<Option<Value>, ErrorEnvelope> {
        match self.session(class).await? {
            Some(mut client) => Ok(Some(client.request(method, params, None).await?)),
            None => Ok(None),
        }
    }

    // -- dispatch ---------------------------------------------------------

    async fn dispatch(&self, command: Command) -> Result<Answer, ErrorEnvelope> {
        match command {
            Command::New(args) => self.cmd_new(args).await,
            Command::Close { session } => self.cmd_close(session).await,
            Command::List(args) => self.cmd_list(args).await,
            Command::Status { session } => self.cmd_status(session).await,
            Command::Control(args) => self.cmd_control(args).await,
            Command::View(args) => self.cmd_view(args).await,
            Command::History(args) => self.cmd_history(args).await,
            Command::Media(args) => self.cmd_media(args).await,
            Command::Drain(args) => self.cmd_drain(args).await,
            Command::Config(args) => self.cmd_config(args).await,
            Command::Token(args) => self.cmd_token(args).await,
            Command::Version => Ok(Self::cmd_version()),
            Command::Serve => {
                serve::serve_instance(&self.instance).await?;
                Ok(Answer::from_lines(Vec::new()))
            }
            Command::Daemon(args) => match args.action {
                DaemonAction::Start => daemon::start(self).await,
                DaemonAction::End { force } => daemon::end(self, force).await,
            },
            Command::Doctor => Ok(doctor::report()),
            Command::Mcp(args) => match args.listen {
                Some(addr) => {
                    crate::mcp::listen::run_listen(&self.instance, &addr, self.remote.clone())
                        .await?;
                    Ok(Answer::from_lines(Vec::new()))
                }
                None => {
                    crate::mcp::server::run_stdio(&self.instance, self.remote.clone()).await?;
                    Ok(Answer::from_lines(Vec::new()))
                }
            },
            Command::Dashboard(_) => Err(ErrorEnvelope::not_implemented()),
        }
    }

    // -- sessions ---------------------------------------------------------

    async fn cmd_new(&self, args: NewArgs) -> Result<Answer, ErrorEnvelope> {
        let backend = crate::manager::Backend::parse(&args.backend)?;
        let resolution = crate::manager::Resolution::parse(&args.resolution)?;
        if !(args.scale.is_finite() && args.scale > 0.0) {
            return Err(ErrorEnvelope::bad_request(format!(
                "invalid scale {}; expected a positive number",
                args.scale
            )));
        }
        let spec = json!({
            "backend": backend.as_str(),
            "resolution": resolution.as_str(),
            "scale": args.scale,
            "view": args.view,
            "label": args.label,
        });
        let params = match &args.label {
            Some(_) => spec,
            None => {
                let mut map = spec.as_object().cloned().unwrap_or_default();
                map.remove("label");
                Value::Object(map)
            }
        };
        match self
            .call(Class::Hot, methods::CREATE_SESSION, Some(params))
            .await?
        {
            Some(result) => {
                let id = json_str(&result, "id").ok_or_else(|| {
                    ErrorEnvelope::internal("create_session result carries no id")
                })?;
                let mut human = vec![id];
                if args.view
                    && let Some(address) = result
                        .get("view")
                        .and_then(|view| view.get("address"))
                        .and_then(Value::as_str)
                {
                    human.push(address.to_string());
                }
                Ok(Answer::new(result, human))
            }
            None => Ok(Answer::cold_no_manager()),
        }
    }

    async fn cmd_close(&self, session: String) -> Result<Answer, ErrorEnvelope> {
        let params = json!({"session_id": session});
        match self
            .call(Class::Cold, methods::CLOSE_SESSION, Some(params))
            .await?
        {
            Some(result) => {
                let id = json_str(&result, "id").unwrap_or(session);
                Ok(Answer::new(result, vec![format!("closed session {id}")]))
            }
            None => Ok(Answer::cold_no_manager()),
        }
    }

    async fn cmd_list(&self, args: ListArgs) -> Result<Answer, ErrorEnvelope> {
        let params = params_object(vec![
            (
                "limit",
                args.limit.map(|limit| json!(limit)).unwrap_or(Value::Null),
            ),
            (
                "cursor",
                args.cursor.clone().map_or(Value::Null, Value::String),
            ),
        ]);
        match self
            .call(Class::Cold, methods::LIST_SESSIONS, params)
            .await?
        {
            Some(result) => Ok(Answer::new(result.clone(), render_list(&result))),
            None => Ok(Answer::cold_no_manager()),
        }
    }

    async fn cmd_status(&self, session: Option<String>) -> Result<Answer, ErrorEnvelope> {
        match session {
            Some(session) => {
                let params = json!({"session_id": session});
                match self
                    .call(Class::Cold, methods::SESSION_STATUS, Some(params))
                    .await?
                {
                    Some(result) => Ok(Answer::new(result.clone(), render_session(&result))),
                    None => Ok(Answer::cold_no_manager()),
                }
            }
            None => match self.call(Class::Cold, methods::STATUS, None).await? {
                Some(result) => Ok(Answer::new(result.clone(), render_instance_status(&result))),
                None => Ok(Answer::cold_no_manager()),
            },
        }
    }

    async fn cmd_control(&self, args: ControlArgs) -> Result<Answer, ErrorEnvelope> {
        // Unknown action ⇒ bad_request, message lists the valid names (04). Validated before
        // dialing so the refusal is identical whether a manager is running or not.
        if !config::is_control_action(&args.action) {
            return Err(ErrorEnvelope::bad_request(format!(
                "unknown control action {:?}; valid actions: {}",
                args.action,
                config::control_actions_listing()
            )));
        }
        let mut params = Map::new();
        params.insert("session_id".to_string(), Value::String(args.session));
        for pair in &args.args {
            let (key, raw) = pair.split_once('=').ok_or_else(|| {
                ErrorEnvelope::bad_request(format!("invalid argument {pair:?}; expected key=value"))
            })?;
            params.insert(key.to_string(), coerce_argument(&args.action, key, raw)?);
        }
        let params = Value::Object(params);
        match self.call(Class::Cold, &args.action, Some(params)).await? {
            Some(result) => Ok(Answer::new(result.clone(), render_object(&result))),
            None => Ok(Answer::cold_no_manager()),
        }
    }

    async fn cmd_view(&self, args: ViewArgs) -> Result<Answer, ErrorEnvelope> {
        let params = json!({"session_id": args.session});
        if args.stop {
            match self
                .call(Class::Cold, methods::STOP_VIEW, Some(params))
                .await?
            {
                Some(_) => Ok(Answer::from_lines(vec!["view stopped".to_string()])),
                None => Ok(Answer::cold_no_manager()),
            }
        } else {
            match self
                .call(Class::Cold, methods::GET_VIEW, Some(params))
                .await?
            {
                Some(result) => {
                    let human = json_str(&result, "address")
                        .map(|address| vec![address])
                        .unwrap_or_else(|| render_object(&result));
                    Ok(Answer::new(result, human))
                }
                None => Ok(Answer::cold_no_manager()),
            }
        }
    }

    /// `daiorama media fetch <id> --out <path>` (05 §Capture): the stdio bridge's fetcher.
    /// It speaks the manager socket's HTTP media surface directly — peer-credential trust,
    /// no new credential — and claim-on-open means the CLI is just another consumer.
    async fn cmd_media(&self, args: MediaArgs) -> Result<Answer, ErrorEnvelope> {
        match args.action {
            MediaAction::Fetch { id, out } => {
                let shaped = id.len() == 32
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
                if !shaped {
                    return Err(ErrorEnvelope::bad_request(
                        "media id must be 32 lowercase hex characters",
                    ));
                }
                use std::io::{Read, Write};
                let socket = EnvView::from_process().socket_path(&self.instance);
                let mut stream = std::os::unix::net::UnixStream::connect(&socket)
                    .map_err(|_| ErrorEnvelope::no_manager_running())?;
                let request = format!(
                    "GET /media/{id} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
                );
                stream
                    .write_all(request.as_bytes())
                    .map_err(|err| ErrorEnvelope::internal(format!("media fetch: {err}")))?;
                let mut response = Vec::new();
                stream
                    .read_to_end(&mut response)
                    .map_err(|err| ErrorEnvelope::internal(format!("media fetch: {err}")))?;
                let split = response
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .ok_or_else(|| ErrorEnvelope::internal("media fetch: no response head"))?;
                let head = std::str::from_utf8(&response[..split]).unwrap_or_default();
                let status = head
                    .split_whitespace()
                    .nth(1)
                    .and_then(|value| value.parse::<u16>().ok())
                    .unwrap_or(0);
                let body = &response[split + 4..];
                if status != 200 {
                    if let Ok(envelope) = serde_json::from_slice::<ErrorEnvelope>(body) {
                        return Err(envelope);
                    }
                    return Err(ErrorEnvelope::internal(format!(
                        "media fetch answered {status}"
                    )));
                }
                if let Some(parent) = out.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent).map_err(|err| {
                        ErrorEnvelope::internal(format!(
                            "cannot create {}: {err}",
                            parent.display()
                        ))
                    })?;
                }
                use std::os::unix::fs::OpenOptionsExt;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&out)
                    .map_err(|err| {
                        ErrorEnvelope::internal(format!("cannot write {}: {err}", out.display()))
                    })?;
                file.write_all(body).map_err(|err| {
                    ErrorEnvelope::internal(format!("cannot write {}: {err}", out.display()))
                })?;
                Ok(Answer::new(
                    json!({"path": out.display().to_string(), "bytes": body.len()}),
                    vec![format!("wrote {} bytes to {}", body.len(), out.display())],
                ))
            }
        }
    }

    async fn cmd_history(&self, args: HistoryArgs) -> Result<Answer, ErrorEnvelope> {
        let params = params_object(vec![
            (
                "session_id",
                args.session.map_or(Value::Null, Value::String),
            ),
            ("scope", args.scope.map_or(Value::Null, Value::String)),
            (
                "limit",
                args.limit.map(|limit| json!(limit)).unwrap_or(Value::Null),
            ),
            ("cursor", args.cursor.map_or(Value::Null, Value::String)),
        ]);
        match self
            .call(Class::Cold, methods::LIST_HISTORY, params)
            .await?
        {
            Some(result) => Ok(Answer::new(result.clone(), render_history(&result))),
            None => Ok(Answer::cold_no_manager()),
        }
    }

    async fn cmd_drain(&self, args: DrainArgs) -> Result<Answer, ErrorEnvelope> {
        // One connection for the whole command: `--wait` polls the drain state over the same
        // session rather than reopening (and re-handshaking) every pass.
        let Some(mut client) = self.session(Class::Cold).await? else {
            return Ok(Answer::cold_no_manager());
        };
        let first = client.request(methods::DRAIN, None, None).await?;
        if !args.wait {
            return Ok(Answer::new(first.clone(), render_drain(&first)));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(args.timeout);
        let mut latest = first;
        loop {
            let remaining = latest
                .get("remaining")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            if remaining == 0 {
                return Ok(Answer::new(latest, vec!["drain complete".to_string()]));
            }
            if std::time::Instant::now() >= deadline {
                return Err(ErrorEnvelope::internal(format!(
                    "drain did not reach zero running sessions within {}s ({remaining} still running)",
                    args.timeout
                )));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            latest = client.request(methods::DRAIN, None, None).await?;
        }
    }

    // -- config -----------------------------------------------------------

    async fn cmd_config(&self, args: ConfigArgs) -> Result<Answer, ErrorEnvelope> {
        let paths = self.paths();
        match args.action {
            ConfigAction::Get { key } => self.config_get(&paths, key).await,
            ConfigAction::Set { key, value } => self.config_set(&paths, key, value).await,
            ConfigAction::Edit => self.config_edit(&paths),
            ConfigAction::Setup => self.config_setup(&paths),
            ConfigAction::Reload => self.config_reload().await,
            ConfigAction::Validate { path } => self.config_validate(&paths, path),
            ConfigAction::Scope(scope) => self.config_scope(&paths, scope.action),
            ConfigAction::Effective { scope } => self.config_effective(&paths, scope),
        }
    }

    async fn config_get(&self, paths: &Paths, key: String) -> Result<Answer, ErrorEnvelope> {
        let spec = config::key_spec(&key)?;
        // 08 A2: a client-local key has no value on a remote.
        if self.remote.is_some() && spec.authority == config::Authority::ClientLocal {
            return Err(ErrorEnvelope::bad_request(format!(
                "key {key:?} is client-local; it does not exist on a remote manager"
            )));
        }
        if self.remote.is_some() {
            let params = json!({"key": key});
            return match self
                .call(Class::Cold, methods::CONFIG_GET, Some(params))
                .await?
            {
                Some(result) => {
                    let value = json_str(&result, "value").unwrap_or_else(|| "(unset)".to_string());
                    Ok(Answer::new(result, vec![value]))
                }
                None => Ok(Answer::cold_no_manager()),
            };
        }
        let config = CoreConfig::load_or_default(&paths.core_config())?;
        let value = config.get(&key)?;
        let authority = match spec.authority {
            config::Authority::ClientLocal => "client_local",
            config::Authority::ManagerSide => "manager_side",
        };
        Ok(Answer::new(
            json!({"key": key, "value": value.to_grammar(), "authority": authority}),
            vec![value.to_grammar()],
        ))
    }

    async fn config_set(
        &self,
        paths: &Paths,
        key: String,
        raw: String,
    ) -> Result<Answer, ErrorEnvelope> {
        let spec = config::key_spec(&key)?;
        let value = parse_value(&key, &raw)?;
        let rendered = value.to_grammar();
        let restart_only = config::RESTART_ONLY_KEYS.contains(&spec.path);
        if self.remote.is_some() {
            if spec.authority == config::Authority::ClientLocal {
                return Err(ErrorEnvelope::bad_request(format!(
                    "key {key:?} is client-local; it does not exist on a remote manager"
                )));
            }
            let params = json!({"key": key, "value": rendered.clone()});
            return match self
                .call(Class::Cold, methods::CONFIG_SET, Some(params))
                .await?
            {
                Some(result) => Ok(Answer::new(
                    result,
                    vec![config_set_line(&key, &rendered, true, restart_only)],
                )),
                None => Ok(Answer::cold_no_manager()),
            };
        }

        // P2 hardening: the whole read-modify-write runs under the config write lock so a
        // concurrent writer (a manager's RPC path, another local set) cannot interleave.
        config::update_config(&paths.core_config(), |config| {
            config.set(&key, value.clone())
        })?;
        // 04/08: output states which of the two applies.
        let env = EnvView::from_process();
        let address = Address::Unix(env.socket_path(&self.instance));
        let running = Client::probe(&address, Duration::from_millis(200)).await;
        let line = config_set_line(&key, &rendered, running, restart_only);
        Ok(Answer::from_lines(vec![line]))
    }

    fn config_edit(&self, paths: &Paths) -> Result<Answer, ErrorEnvelope> {
        if self.remote.is_some() {
            return Err(ErrorEnvelope::bad_request(
                "config edit is local-instance only; it cannot target a remote manager",
            ));
        }
        let path = paths.core_config();
        if !path.exists() {
            // First-run create under the same write lock (another writer may have raced us
            // here; re-check once inside). P2 hardening.
            let _lock = config::lock_config_write(&path)?;
            if !path.exists() {
                CoreConfig::default().save(&path)?;
            }
        }
        let editor = std::env::var("EDITOR")
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ErrorEnvelope::bad_request_local(
                    "$EDITOR is not set; set it to edit the core config file",
                )
            })?;
        let status = std::process::Command::new(&editor)
            .arg(&path)
            .status()
            .map_err(|err| ErrorEnvelope::internal(format!("cannot run {editor}: {err}")))?;
        if !status.success() {
            return Err(ErrorEnvelope::internal(format!(
                "{editor} exited with {status}"
            )));
        }
        Ok(Answer::from_lines(vec![format!(
            "edited {}",
            path.display()
        )]))
    }

    fn config_setup(&self, paths: &Paths) -> Result<Answer, ErrorEnvelope> {
        let core = paths.core_config();
        let mut lines = Vec::new();
        if core.exists() {
            lines.push(format!(
                "config already exists at {}; edit it with 'daiorama config edit'",
                core.display()
            ));
        } else {
            // First-run create under the config write lock (re-check inside; a concurrent
            // writer may have created it since our check). P2 hardening.
            let _lock = config::lock_config_write(&core)?;
            if !core.exists() {
                CoreConfig::default().save(&core)?;
                std::fs::write(&core, config::starter_config()).map_err(|err| {
                    ErrorEnvelope::bad_request(format!("cannot write {}: {err}", core.display()))
                })?;
                lines.push(format!("wrote {}", core.display()));
            } else {
                lines.push(format!(
                    "config already exists at {}; edit it with 'daiorama config edit'",
                    core.display()
                ));
            }
        }
        let scope = paths.scope_file("operator");
        if scope.exists() {
            lines.push(format!("scope file already exists at {}", scope.display()));
        } else {
            if let Some(parent) = scope.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    ErrorEnvelope::bad_request(format!("cannot create {}: {err}", parent.display()))
                })?;
            }
            std::fs::write(&scope, config::starter_scope_operator()).map_err(|err| {
                ErrorEnvelope::bad_request(format!("cannot write {}: {err}", scope.display()))
            })?;
            lines.push(format!("wrote {}", scope.display()));
        }
        // Scope secrets make a scope network-usable (07); the starter scope gets one here so
        // the first network bridge start does not have to mint its own.
        for name in paths.scope_names() {
            if config::read_scope_token(paths, &name)?.is_none() {
                let secret = config::generate_token();
                let path = config::write_scope_token(paths, &name, &secret)?;
                lines.push(format!(
                    "wrote {} (scope secret, mode 0600)",
                    path.display()
                ));
            }
        }
        lines.push(
            "next: 'daiorama doctor' for the environment check, then 'daiorama new'".to_string(),
        );
        Ok(Answer::from_lines(lines))
    }

    async fn config_reload(&self) -> Result<Answer, ErrorEnvelope> {
        // Cold class: RPCs a running manager, never starts one (04).
        match self.call(Class::Cold, methods::CONFIG_RELOAD, None).await? {
            Some(result) => {
                let mut human = vec![config_reload_line(&result)];
                if let Some(restart_only) = result.get("restart_only").and_then(Value::as_array) {
                    let names: Vec<String> = restart_only
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    if !names.is_empty() {
                        human.push(format!(
                            "restart-only (takes effect at next manager start): {}",
                            names.join(", ")
                        ));
                    }
                }
                Ok(Answer::new(result, human))
            }
            None => Ok(Answer::cold_no_manager()),
        }
    }

    fn config_validate(
        &self,
        paths: &Paths,
        path: Option<PathBuf>,
    ) -> Result<Answer, ErrorEnvelope> {
        let mut lines = Vec::new();
        let core_path = path.clone().unwrap_or_else(|| paths.core_config());
        if core_path.exists() {
            CoreConfig::load(&core_path)?;
            lines.push(format!("config valid: {}", core_path.display()));
        } else if path.is_some() {
            CoreConfig::load(&core_path)?;
        } else {
            // No file yet: defaults are the valid answer (08: first run is not an error).
            CoreConfig::default().validate()?;
            lines.push(format!(
                "no config at {}; defaults are valid",
                core_path.display()
            ));
        }
        for name in paths.scope_names() {
            let scope_path = paths.scope_file(&name);
            let scope = ScopeFile::load(&scope_path)?;
            lines.push(format!(
                "scope {name:?} valid ({} tool grants, {} category grants)",
                scope.grant.tools.len(),
                scope.grant.categories.len()
            ));
        }
        Ok(Answer::from_lines(lines))
    }

    fn config_scope(&self, paths: &Paths, action: ScopeAction) -> Result<Answer, ErrorEnvelope> {
        match action {
            ScopeAction::List => {
                let names = paths.scope_names();
                if names.is_empty() {
                    return Ok(Answer::from_lines(vec![format!(
                        "no scope files at {}",
                        paths.scopes_dir().display()
                    )]));
                }
                Ok(Answer::from_lines(names))
            }
            ScopeAction::Get { name } => {
                let path = paths.scope_file(&name);
                let scope = ScopeFile::load(&path)?;
                let mut lines = vec![format!("scope {name} ({})", path.display())];
                lines.push(format!(
                    "sessions.max: {}",
                    scope
                        .sessions
                        .max
                        .map_or_else(|| "(unset)".to_string(), |max| max.to_string())
                ));
                lines.push(format!(
                    "sessions.visibility: {}",
                    scope.sessions.visibility.as_str()
                ));
                lines.push(format!(
                    "limits.rate: {} per_sec, burst {}, mode {}",
                    scope.limits.rate.per_sec,
                    scope.limits.rate.burst,
                    scope.limits.rate.mode.as_str()
                ));
                lines.push(format!(
                    "audit: store_clips {}, store_screenshots {}",
                    on_off(scope.audit.store_clips),
                    on_off(scope.audit.store_screenshots)
                ));
                lines.push(format!(
                    "grant: categories [{}], tools [{}]",
                    scope.grant.categories.join(", "),
                    scope.grant.tools.join(", ")
                ));
                Ok(Answer::from_lines(lines))
            }
            ScopeAction::Validate { name } => {
                let path = paths.scope_file(&name);
                ScopeFile::load(&path)?;
                Ok(Answer::from_lines(vec![format!(
                    "scope {name:?} valid: {}",
                    path.display()
                )]))
            }
            ScopeAction::Create { name, template } => {
                if !config::valid_scope_name(&name) {
                    return Err(ErrorEnvelope::bad_request(
                        crate::strings::INVALID_SCOPE_NAME,
                    ));
                }
                let template = config::ScopeTemplate::parse(&template)?;
                let path = paths.scope_file(&name);
                // Render → validate → write: a template that would not load never lands.
                let text = config::starter_scope(template);
                ScopeFile::from_ron(&text)?;
                config::write_scope_new(&path, &name, &text)?;
                // The path only (04): the file is the answer, not a copy of its contents.
                Ok(Answer::from_lines(vec![path.display().to_string()]))
            }
        }
    }

    fn config_effective(
        &self,
        paths: &Paths,
        scope: Option<String>,
    ) -> Result<Answer, ErrorEnvelope> {
        let config = CoreConfig::load_or_default(&paths.core_config())?;
        let mut scopes: Vec<(String, ScopeFile)> = Vec::new();
        match scope {
            Some(name) => {
                scopes.push((name.clone(), ScopeFile::load(&paths.scope_file(&name))?));
            }
            None => {
                for name in paths.scope_names() {
                    scopes.push((name.clone(), ScopeFile::load(&paths.scope_file(&name))?));
                }
            }
        }
        let view = effective_view(&config, scopes);
        let mut lines: Vec<String> = view
            .core
            .iter()
            .map(|(key, value)| format!("{key} = {value}"))
            .collect();
        let mut payload_scopes = Map::new();
        for (name, scope) in &view.scopes {
            lines.push(format!("--- scope {name} ---"));
            lines.push(format!(
                "sessions.max = {}",
                scope
                    .sessions
                    .max
                    .map_or_else(|| "(unset)".to_string(), |max| max.to_string())
            ));
            lines.push(format!(
                "sessions.visibility = {}",
                scope.sessions.visibility.as_str()
            ));
            lines.push(format!(
                "limits.rate = {} per_sec, burst {}, mode {}",
                scope.limits.rate.per_sec,
                scope.limits.rate.burst,
                scope.limits.rate.mode.as_str()
            ));
            lines.push(format!(
                "audit.store_clips = {}",
                on_off(scope.audit.store_clips)
            ));
            lines.push(format!(
                "audit.store_screenshots = {}",
                on_off(scope.audit.store_screenshots)
            ));
            lines.push(format!(
                "grant.categories = [{}]",
                scope.grant.categories.join(", ")
            ));
            lines.push(format!("grant.tools = [{}]", scope.grant.tools.join(", ")));
            payload_scopes.insert(
                name.clone(),
                serde_json::to_value(scope).unwrap_or(Value::Null),
            );
        }
        let payload = json!({
            "core": view.core.iter().map(|(key, value)| json!({"key": key, "value": value})).collect::<Vec<_>>(),
            "scopes": Value::Object(payload_scopes),
        });
        Ok(Answer::new(payload, lines))
    }

    // -- token ------------------------------------------------------------

    async fn cmd_token(&self, args: TokenArgs) -> Result<Answer, ErrorEnvelope> {
        if self.remote.is_some() {
            return Err(ErrorEnvelope::bad_request(
                "token operations are local-instance only; they cannot target a remote manager",
            ));
        }
        let paths = self.paths();
        match args.action {
            TokenAction::Show => {
                let token = config::read_token(&paths)?.ok_or_else(|| {
                    ErrorEnvelope::bad_request_local(format!(
                        "no token file at {}; it is generated on the first network listen bind, or by 'daiorama token rotate'",
                        paths.token_file().display()
                    ))
                })?;
                if let Some(mode) = config::token_file_mode(&paths)
                    && mode != 0o600
                {
                    eprintln!(
                        "warning: {} is mode {mode:04o}; it should be 0600",
                        paths.token_file().display()
                    );
                }
                Ok(Answer::new(
                    json!({"token": token, "path": paths.token_file().display().to_string()}),
                    vec![token, format!("path: {}", paths.token_file().display())],
                ))
            }
            TokenAction::Rotate => {
                let token = config::generate_token();
                config::write_token(&paths, &token)?;
                let running = Client::probe(
                    &Address::Unix(EnvView::from_process().socket_path(&self.instance)),
                    Duration::from_millis(200),
                )
                .await;
                let mut lines = vec![token.clone()];
                lines.push(format!("path: {}", paths.token_file().display()));
                if running {
                    lines.push(
                        "the file holds the new value while the running manager still accepts the old one until it restarts"
                            .to_string(),
                    );
                    lines.push(
                        "restarting the manager drops every running session (there is no running state to migrate)"
                            .to_string(),
                    );
                } else {
                    lines.push("takes effect at next manager start".to_string());
                }
                Ok(Answer::new(
                    json!({"token": token, "path": paths.token_file().display().to_string()}),
                    lines,
                ))
            }
        }
    }

    // -- version ----------------------------------------------------------

    fn cmd_version() -> Answer {
        Answer::new(
            json!({
                "binary": BINARY_VERSION,
                "protocol": PROTOCOL_VERSION,
                "min_supported": MIN_SUPPORTED,
            }),
            vec![
                format!("daiorama {BINARY_VERSION}"),
                format!("protocol {PROTOCOL_VERSION} (min {MIN_SUPPORTED})"),
            ],
        )
    }
}

// ---------------------------------------------------------------------------
// Rendering (human output; `--json` prints the payload instead)
// ---------------------------------------------------------------------------

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// 04/08: the `config set` line states which applies — a reload, or a restart.
fn config_set_line(key: &str, rendered: &str, running: bool, restart_only: bool) -> String {
    if !running {
        return format!("set {key} = {rendered}; takes effect at next manager start");
    }
    if restart_only {
        return format!(
            "set {key} = {rendered}; restart-only: a running manager keeps the old value until it restarts ('daiorama config reload' will not apply it)"
        );
    }
    format!(
        "set {key} = {rendered}; a running manager keeps the old value until 'daiorama config reload' (or restart)"
    )
}

/// 04/08: name the keys a reload applied — a bare "config reloaded" reads as a no-op.
fn config_reload_line(result: &Value) -> String {
    let names: Vec<&str> = result
        .get("reloaded")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if names.is_empty() {
        "config reloaded".to_string()
    } else {
        format!("config reloaded: {}", names.join(", "))
    }
}

/// Generic object rendering: `key: value` per field, nested values as compact JSON.
fn render_object(value: &Value) -> Vec<String> {
    match value.as_object() {
        Some(map) if !map.is_empty() => map
            .iter()
            .map(|(key, value)| match value {
                Value::String(text) => format!("{key}: {text}"),
                Value::Null => format!("{key}: -"),
                other => format!("{key}: {other}"),
            })
            .collect(),
        Some(_) => Vec::new(),
        None => vec![value.to_string()],
    }
}

fn render_list(page: &Value) -> Vec<String> {
    let sessions = page
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut lines = Vec::new();
    if sessions.is_empty() {
        lines.push("no sessions".to_string());
    } else {
        lines.push("ID  STATE  BACKEND  SCOPE  ORIGIN  IDLE-SINCE".to_string());
        for session in &sessions {
            lines.push(format!(
                "{}  {}  {}  {}  {}  {}",
                json_str(session, "id").unwrap_or_else(|| "-".to_string()),
                json_str(session, "state").unwrap_or_else(|| "-".to_string()),
                json_str(session, "backend").unwrap_or_else(|| "-".to_string()),
                json_str(session, "scope").unwrap_or_else(|| "-".to_string()),
                json_str(session, "origin").unwrap_or_else(|| "-".to_string()),
                json_str(session, "idle_since").unwrap_or_else(|| "-".to_string()),
            ));
        }
    }
    if let Some(cursor) = page
        .get("next_cursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
    {
        lines.push(format!("next_cursor: {cursor}"));
    }
    lines
}

fn render_instance_status(status: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for key in ["state", "mode", "pid", "socket"] {
        if let Some(value) = status.get(key) {
            let rendered = match value {
                Value::Null => "-".to_string(),
                other => other.to_string(),
            };
            lines.push(format!("{key}: {}", rendered.trim_matches('"')));
        }
    }
    if let Some(versions) = status.get("versions").and_then(Value::as_object) {
        for (key, value) in versions {
            lines.push(format!("{key}: {}", value.to_string().trim_matches('"')));
        }
    }
    if let Some(sessions) = status.get("sessions") {
        let running = sessions
            .get("running")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let dead = sessions
            .get("dead")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        lines.push(format!("sessions: {running} running, {dead} dead"));
    }
    if let Some(drain) = status.get("drain") {
        let active = drain
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let remaining = drain
            .get("remaining")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        lines.push(if active {
            format!("drain: active, {remaining} still running")
        } else {
            "drain: inactive".to_string()
        });
    }
    if let Some(instance_id) = status.get("instance_id") {
        lines.push(format!(
            "instance_id: {}",
            instance_id.to_string().trim_matches('"')
        ));
    }
    lines
}

fn render_session(session: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for key in [
        "id",
        "label",
        "state",
        "backend",
        "resolution",
        "scale",
        "scope",
        "origin",
        "idle_since",
    ] {
        if let Some(value) = session.get(key) {
            let rendered = match value {
                Value::Null => "-".to_string(),
                other => other.to_string().trim_matches('"').to_string(),
            };
            lines.push(format!("{key}: {rendered}"));
        }
    }
    match session.get("view") {
        None | Some(Value::Null) => lines.push("view: none".to_string()),
        Some(view) => {
            let state = json_str(view, "state").unwrap_or_else(|| "-".to_string());
            let address = json_str(view, "address").unwrap_or_else(|| "-".to_string());
            let generation = view
                .get("generation")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            lines.push(format!("view: {state} {address} (generation {generation})"));
        }
    }
    if let Some(renderer) = session.get("renderer") {
        lines.push(format!(
            "renderer: {}",
            renderer.to_string().trim_matches('"')
        ));
    }
    if let Some(instance_id) = session.get("instance_id") {
        lines.push(format!(
            "instance_id: {}",
            instance_id.to_string().trim_matches('"')
        ));
    }
    lines
}

fn render_history(page: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let records = page
        .get("records")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if records.is_empty() {
        lines.push("no history records".to_string());
    }
    for record in &records {
        let at = json_str(record, "at").unwrap_or_else(|| "-".to_string());
        let event = json_str(record, "event").unwrap_or_else(|| "-".to_string());
        let scope = json_str(record, "scope").unwrap_or_else(|| "-".to_string());
        let origin = json_str(record, "origin").unwrap_or_else(|| "-".to_string());
        let mut line = format!("{at}  {event}  scope={scope}  origin={origin}");
        if let Some(session) = json_str(record, "session_id") {
            line.push_str(&format!("  session={session}"));
        }
        if let Some(cause) = json_str(record, "cause") {
            line.push_str(&format!("  cause={cause}"));
        }
        lines.push(line);
    }
    if let Some(cursor) = page
        .get("next_cursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
    {
        lines.push(format!("next_cursor: {cursor}"));
    }
    lines
}

fn render_drain(state: &Value) -> Vec<String> {
    let remaining = state
        .get("remaining")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if remaining == 0 {
        vec!["drain complete".to_string()]
    } else {
        vec![format!("draining; {remaining} still running")]
    }
}

/// Control arguments are typed per the tool's inputSchema (04): integers for the pointer and
/// scroll coordinates, strings everywhere else.
fn coerce_argument(action: &str, key: &str, raw: &str) -> Result<Value, ErrorEnvelope> {
    let integer = matches!(
        (action, key),
        ("pointer_move", "x" | "y") | ("pointer_scroll", "dx" | "dy")
    );
    if integer {
        raw.parse::<i64>().map(|value| json!(value)).map_err(|_| {
            ErrorEnvelope::bad_request(format!(
                "argument {key}={raw} is not an integer (per {action}'s inputSchema)"
            ))
        })
    } else {
        Ok(Value::String(raw.to_string()))
    }
}

/// The program the lazy start spawns: this binary, unless overridden. `DAIORAMA_MANAGER_BIN`
/// exists for out-of-tree manager builds and for the fixture harness (documented in
/// `fixtures/README.md`); the default is the sibling `daiorama`.
fn manager_program() -> PathBuf {
    match std::env::var_os("DAIORAMA_MANAGER_BIN") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => std::env::current_exe().unwrap_or_else(|_| PathBuf::from("daiorama")),
    }
}

/// A started manager reports a startup failure as one line of the frame's error form on
/// stderr; the launcher surfaces that envelope verbatim (contract for the runtime side).
fn start_failure(stderr: &str) -> Option<ErrorEnvelope> {
    stderr.lines().rev().find_map(|line| {
        serde_json::from_str::<Value>(line.trim())
            .ok()
            .and_then(|value| value.get("error").cloned())
            .and_then(|error| serde_json::from_value::<ErrorEnvelope>(error).ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::ErrorCode;
    use clap::CommandFactory;

    #[test]
    fn clap_surface_parses_every_pinned_command() {
        Cli::command().debug_assert();

        let parse = |args: &[&str]| Cli::try_parse_from(args).unwrap();

        let cli = parse(&[
            "daiorama",
            "new",
            "--backend",
            "nested",
            "--resolution",
            "1280x720",
            "--scale",
            "1.5",
            "--view",
            "--label",
            "demo",
        ]);
        match cli.command {
            Command::New(args) => {
                assert_eq!(args.backend, "nested");
                assert_eq!(args.resolution, "1280x720");
                assert_eq!(args.scale, 1.5);
                assert!(args.view);
                assert_eq!(args.label.as_deref(), Some("demo"));
            }
            other => panic!("expected new, got {other:?}"),
        }

        // Pinned defaults.
        let cli = parse(&["daiorama", "new"]);
        match cli.command {
            Command::New(args) => {
                assert_eq!(args.backend, "headless");
                assert_eq!(args.resolution, "1920x1080");
                assert_eq!(args.scale, 1.0);
                assert!(!args.view);
                assert_eq!(args.label, None);
            }
            other => panic!("expected new, got {other:?}"),
        }
        assert_eq!(parse(&["daiorama", "status"]).instance, "default");

        // The rest of the surface exists with the pinned names.
        let commands: [&[&str]; 24] = [
            &["daiorama", "close", "s-1"],
            &["daiorama", "list", "--limit", "10", "--cursor", "c1"],
            &["daiorama", "status", "s-1"],
            &["daiorama", "control", "s-1", "pointer_move", "x=1", "y=2"],
            &["daiorama", "view", "s-1"],
            &["daiorama", "view", "s-1", "--stop"],
            &[
                "daiorama", "history", "s-1", "--scope", "operator", "--limit", "5",
            ],
            &["daiorama", "serve"],
            &["daiorama", "daemon", "start"],
            &["daiorama", "daemon", "end", "--force"],
            &["daiorama", "drain", "--wait", "--timeout", "10"],
            &["daiorama", "config", "get", "media.ttl"],
            &["daiorama", "config", "set", "media.ttl", "120s"],
            &["daiorama", "config", "edit"],
            &["daiorama", "config", "setup"],
            &["daiorama", "config", "reload"],
            &["daiorama", "config", "validate"],
            &["daiorama", "config", "scope", "list"],
            &["daiorama", "config", "scope", "get", "operator"],
            &["daiorama", "config", "scope", "validate", "operator"],
            &["daiorama", "config", "effective", "--scope", "operator"],
            &["daiorama", "token", "show"],
            &["daiorama", "token", "rotate"],
            &["daiorama", "mcp", "--listen", "127.0.0.1:8757"],
        ];
        for argv in commands {
            Cli::try_parse_from(argv).unwrap_or_else(|err| panic!("{argv:?}: {err}"));
        }
        for argv in [
            &["daiorama", "doctor"][..],
            &["daiorama", "dashboard", "serve"][..],
            &[
                "daiorama",
                "dashboard",
                "install",
                "--version",
                "1.2.3",
                "--from",
                "/tmp/mod",
            ][..],
            &["daiorama", "dashboard", "connect", "host:8756"][..],
            &["daiorama", "dashboard", "connections", "list"][..],
            &["daiorama", "dashboard", "connections", "remove", "prod"][..],
            &["daiorama", "dashboard", "token", "set", "prod"][..],
            &["daiorama", "dashboard", "token", "remove", "prod"][..],
            &["daiorama", "version"][..],
            &["daiorama", "mcp"][..],
        ] {
            Cli::try_parse_from(argv).unwrap_or_else(|err| panic!("{argv:?}: {err}"));
        }

        // Global flags are accepted anywhere and default as pinned.
        let cli = parse(&[
            "daiorama",
            "--instance",
            "beta",
            "--remote",
            "host:8756",
            "--json",
            "status",
        ]);
        assert_eq!(cli.instance, "beta");
        assert_eq!(cli.remote.as_deref(), Some("host:8756"));
        assert!(cli.json);
    }

    #[test]
    fn control_arguments_are_typed_per_the_schema() {
        assert_eq!(
            coerce_argument("pointer_move", "x", "12").unwrap(),
            json!(12)
        );
        assert_eq!(
            coerce_argument("pointer_scroll", "dy", "-3").unwrap(),
            json!(-3)
        );
        assert_eq!(
            coerce_argument("pointer_click", "button", "left").unwrap(),
            json!("left")
        );
        assert!(coerce_argument("pointer_move", "x", "left").is_err());
        assert_eq!(
            coerce_argument("type_text", "text", "hello world").unwrap(),
            json!("hello world")
        );
    }

    #[test]
    fn start_failure_reads_the_last_envelope_line() {
        let stderr = "starting\n\
                      {\"error\":{\"code\":\"limits_unavailable\",\"message\":\"containment unavailable: no delegation (x).\",\"hint\":\"set limits.mode = off to accept the documented downgrade\"}}\n";
        let envelope = start_failure(stderr).expect("envelope found");
        assert_eq!(envelope.code, ErrorCode::LimitsUnavailable);
        assert!(envelope.message.contains("containment unavailable"));

        assert!(start_failure("just noise\n").is_none());
        assert!(start_failure("{\"result\":1}\n").is_none());

        // The last error line wins when the child logged more than one.
        let two = "{\"error\":{\"code\":\"internal\",\"message\":\"first\"}}\n{\"error\":{\"code\":\"limits_unavailable\",\"message\":\"second\"}}\n";
        assert_eq!(start_failure(two).unwrap().message, "second");
    }

    #[test]
    fn cold_answer_shape_is_pinned() {
        let answer = Answer::cold_no_manager();
        assert_eq!(
            answer.payload,
            json!({"code": "no_manager_running", "message": "no manager running"})
        );
        assert_eq!(answer.human, vec!["no manager running".to_string()]);
        assert_eq!(strings::NO_MANAGER_RUNNING, "no manager running");
    }

    #[test]
    fn renderers_cover_the_pinned_shapes() {
        let page = json!({
            "sessions": [
                {"id": "s-1", "state": "running", "backend": "headless", "scope": "operator", "origin": "shell", "idle_since": "t0"},
                {"id": "s-2", "state": "dead", "backend": "headless", "scope": "operator", "origin": "shell", "idle_since": "t0"}
            ],
            "next_cursor": "c2"
        });
        let lines = render_list(&page);
        assert_eq!(lines[0], "ID  STATE  BACKEND  SCOPE  ORIGIN  IDLE-SINCE");
        assert!(lines[1].starts_with("s-1  running"));
        assert_eq!(lines.last().unwrap(), "next_cursor: c2");
        assert_eq!(
            render_list(&json!({"sessions": [], "next_cursor": null})),
            vec!["no sessions"]
        );
        assert_eq!(
            render_list(&json!({"sessions": [{"id": "s-1"}]})).len(),
            2,
            "no cursor line when the page is not truncated"
        );

        let status = json!({
            "state": "running", "mode": "attached", "pid": 4242, "socket": "/tmp/s.sock",
            "versions": {"binary": "0.1.0", "protocol": "0.1.0", "min_supported": "0.1.0"},
            "sessions": {"running": 1, "dead": 2}, "drain": {"active": false, "remaining": 0},
            "instance_id": "abc"
        });
        let lines = render_instance_status(&status);
        assert!(lines.contains(&"state: running".to_string()));
        assert!(lines.contains(&"mode: attached".to_string()));
        assert!(lines.contains(&"pid: 4242".to_string()));
        assert!(lines.contains(&"sessions: 1 running, 2 dead".to_string()));
        assert!(lines.contains(&"drain: inactive".to_string()));
        assert!(lines.contains(&"instance_id: abc".to_string()));

        let session = json!({
            "id": "s-1", "state": "running", "backend": "headless", "resolution": "1920x1080",
            "scale": 1.0, "scope": "operator", "origin": "shell", "idle_since": "t0",
            "view": {"state": "down", "address": "http://127.0.0.1:6080/", "generation": 1},
            "instance_id": "abc", "renderer": "verified"
        });
        let lines = render_session(&session);
        assert!(lines.contains(&"view: down http://127.0.0.1:6080/ (generation 1)".to_string()));
        let mut no_view = session.clone();
        no_view["view"] = Value::Null;
        assert!(render_session(&no_view).contains(&"view: none".to_string()));

        let history = json!({
            "records": [{"at": "t0", "event": "session_end", "scope": "operator", "origin": "shell", "session_id": "s-1", "cause": "dead"}],
            "next_cursor": null
        });
        let lines = render_history(&history);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("cause=dead"));
        assert_eq!(
            render_history(&json!({"records": []})),
            vec!["no history records"]
        );

        assert_eq!(
            render_drain(&json!({"active": true, "remaining": 0})),
            vec!["drain complete"]
        );
        assert_eq!(
            render_drain(&json!({"active": true, "remaining": 3})),
            vec!["draining; 3 still running"]
        );
    }

    #[test]
    fn manager_program_defaults_to_this_binary() {
        // No override set in the test environment: the lazy start re-execs `serve` from the
        // same binary (one binary, one process per instance). Under `cargo test` the current
        // executable is the test harness built from this crate, which is what we assert.
        let program = manager_program();
        assert!(
            program
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains("daiorama")),
            "unexpected program {}",
            program.display()
        );
    }

    #[test]
    fn config_set_line_states_reload_or_restart() {
        // 04/08: the line states which of the two applies — never both for a restart-only key.
        assert_eq!(
            config_set_line("sessions.max_global", "25", true, false),
            "set sessions.max_global = 25; a running manager keeps the old value until \
             'daiorama config reload' (or restart)"
        );
        assert_eq!(
            config_set_line("manager.bind", "127.0.0.1:9", true, true),
            "set manager.bind = 127.0.0.1:9; restart-only: a running manager keeps the old \
             value until it restarts ('daiorama config reload' will not apply it)"
        );
        assert_eq!(
            config_set_line("manager.bind", "", false, true),
            "set manager.bind = ; takes effect at next manager start"
        );
    }

    #[test]
    fn config_reload_line_names_applied_keys() {
        assert_eq!(
            config_reload_line(&json!({"reloaded": [], "restart_only": []})),
            "config reloaded"
        );
        assert_eq!(
            config_reload_line(&json!({"reloaded": ["sessions.max_global"]})),
            "config reloaded: sessions.max_global"
        );
        assert_eq!(
            config_reload_line(&json!({"reloaded": ["a", "b"]})),
            "config reloaded: a, b"
        );
    }
}
