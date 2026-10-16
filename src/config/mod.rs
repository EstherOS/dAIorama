//! Config: RON core config + scope-file types, dot-path get/set, validation, paths (08).
//!
//! Shape rules pinned by `docs/08-config.md` and `docs/04-cli.md`:
//!
//! - RON, core file `${XDG_CONFIG_HOME:-~/.config}/daiorama/<instance>/config.ron`,
//!   scope files `scopes/<scope>.ron`, data root `${XDG_DATA_HOME:-~/.local/share}/daiorama/<instance>/`.
//! - Dot-path keys, **one nesting level max**. `get`/`set` deliberately do not address
//!   scopes (08 D4): scopes are the one list-tolerant shape and are edited as files.
//! - Value grammar (04): integers; durations `<n>s|m|h`; enum literals; booleans `on|off`.
//! - Unknown keys / unknown tool or category names are **load errors listing the valid
//!   names** — a typo must never silently shrink a manifest (08 D4).
//!
//! Dependency position: sits directly above [`crate::ipc`] (error envelopes) and
//! [`crate::strings`]; nothing below depends on `cli`, `manager` or `mcp`.
//!
//! Note on lints: this crate is the P2 phase-1 **contract layer** — the full validated key
//! set, the scope-file types and the starter-file writers are consumed by the runtime half
//! (W2/W3) as well as by today's CLI, and every one of them is exercised by the unit tests
//! below until then. `dead_code` is allowed module-wide with that reason in mind; it goes away
//! as the consumers land.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ipc::ErrorEnvelope;

// ---------------------------------------------------------------------------
// Paths (08 filesystem contract)
// ---------------------------------------------------------------------------

/// Per-instance filesystem contract (08). Pure path arithmetic, built from an injected
/// environment so tests never touch the process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    /// `${XDG_CONFIG_HOME:-~/.config}/daiorama/<instance>` and
    /// `${XDG_DATA_HOME:-~/.local/share}/daiorama/<instance>`.
    pub fn for_env(
        config_home: Option<&Path>,
        data_home: Option<&Path>,
        home: Option<&Path>,
        instance: &str,
    ) -> Self {
        let home_dir = home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let config_home = config_home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home_dir.join(".config"));
        let data_home = data_home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home_dir.join(".local/share"));
        Self {
            config_dir: config_home.join("daiorama").join(instance),
            data_dir: data_home.join("daiorama").join(instance),
        }
    }

    /// The real process environment.
    pub fn from_process(instance: &str) -> Self {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let config_home = var("XDG_CONFIG_HOME");
        let data_home = var("XDG_DATA_HOME");
        let home = var("HOME");
        Self::for_env(
            config_home.as_deref(),
            data_home.as_deref(),
            home.as_deref(),
            instance,
        )
    }

    /// `<config-dir>/config.ron`
    pub fn core_config(&self) -> PathBuf {
        self.config_dir.join("config.ron")
    }

    /// `<config-dir>/scopes`
    pub fn scopes_dir(&self) -> PathBuf {
        self.config_dir.join("scopes")
    }

    /// `<config-dir>/scopes/<scope>.ron` — filename = scope name (08).
    pub fn scope_file(&self, scope: &str) -> PathBuf {
        self.scopes_dir().join(format!("{scope}.ron"))
    }

    /// `<data-root>/token` — mode 0600, its own file (07).
    pub fn token_file(&self) -> PathBuf {
        self.data_dir.join("token")
    }

    /// `<data-root>/history/`
    pub fn history_dir(&self) -> PathBuf {
        self.data_dir.join("history")
    }

    /// `<data-root>/media/`
    pub fn media_dir(&self) -> PathBuf {
        self.data_dir.join("media")
    }

    /// `<data-root>/modules/`
    pub fn modules_dir(&self) -> PathBuf {
        self.data_dir.join("modules")
    }

    /// Existing scope-file names, sorted (filename = scope name).
    pub fn scope_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.scopes_dir())
            .map(|entries| {
                entries
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
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

// ---------------------------------------------------------------------------
// Core config
// ---------------------------------------------------------------------------

/// Checked at startup with a clear migration path if ever bumped (08).
pub const CONFIG_VERSION: u32 = 1;

/// The pinned default for `sessions.max_global`. Not in 04's default table; carried as a
/// documented provisional (see the P2 report's open questions) — the runtime side may pin a
/// different number without touching the contract shapes.
pub const DEFAULT_SESSIONS_MAX_GLOBAL: u32 = 16;

/// `manager.start_mode` (04): client-local, `auto` unless pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartMode {
    Auto,
    Attached,
    Background,
}

impl StartMode {
    pub const LITERALS: [&'static str; 3] = ["auto", "attached", "background"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Attached => "attached",
            Self::Background => "background",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ErrorEnvelope> {
        match raw {
            "auto" => Ok(Self::Auto),
            "attached" => Ok(Self::Attached),
            "background" => Ok(Self::Background),
            other => Err(unknown_literal(other, &Self::LITERALS)),
        }
    }
}

/// `on|off` (04 value grammar). The same enum serves `limits.mode`, `reaper.sweep_dead` and
/// `renderer.allow_software`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Toggle {
    On,
    Off,
}

impl Toggle {
    pub fn is_on(self) -> bool {
        matches!(self, Self::On)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ErrorEnvelope> {
        match raw {
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            other => Err(unknown_literal(other, &["on", "off"])),
        }
    }
}

/// A duration-or-off reaper setting (04: `reaper.idle`/`reaper.view_idle`, default off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Idle {
    Off,
    Every(Duration),
}

impl Idle {
    pub fn as_seconds(self) -> Option<u64> {
        match self {
            Self::Off => None,
            Self::Every(duration) => Some(duration.secs),
        }
    }
}

/// A duration held as whole seconds — the file keeps seconds (04's table names seconds:
/// `media.ttl = 600 (seconds)`), the CLI grammar accepts `<n>s|m|h` (and a bare integer,
/// read as seconds) and prints back the canonical `s` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Duration {
    pub secs: u64,
}

impl Duration {
    pub const fn from_secs(secs: u64) -> Self {
        Self { secs }
    }

    pub const fn from_minutes(minutes: u64) -> Self {
        Self { secs: minutes * 60 }
    }

    pub const fn from_hours(hours: u64) -> Self {
        Self { secs: hours * 3600 }
    }

    /// 04 grammar: `<n>s|m|h`, plus a bare integer read as seconds.
    pub fn parse(raw: &str) -> Result<Self, ErrorEnvelope> {
        let invalid = || {
            ErrorEnvelope::bad_request(format!(
                "invalid duration {raw:?}; expected <n>s, <n>m or <n>h"
            ))
        };
        if raw.is_empty() {
            return Err(invalid());
        }
        let (digits, unit) = match raw.chars().last() {
            Some('s') => (&raw[..raw.len() - 1], 1u64),
            Some('m') => (&raw[..raw.len() - 1], 60),
            Some('h') => (&raw[..raw.len() - 1], 3600),
            Some(c) if c.is_ascii_digit() => (raw, 1),
            _ => return Err(invalid()),
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(invalid());
        }
        let value: u64 = digits.parse().map_err(|_| invalid())?;
        value
            .checked_mul(unit)
            .map(Self::from_secs)
            .ok_or_else(invalid)
    }

    /// The canonical grammar form (`600s`).
    pub fn to_grammar(self) -> String {
        format!("{}s", self.secs)
    }
}

/// `remote.*` — client routing values, read before any manager is located (08 A2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    #[serde(default)]
    pub default: Option<String>,
}

/// `manager.*`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagerConfig {
    pub start_mode: StartMode,
    /// `None` = unix socket only. A TCP bind here means 8756 unless a port is written
    /// (04 pinned default port).
    #[serde(default)]
    pub bind: Option<String>,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            start_mode: StartMode::Auto,
            bind: None,
        }
    }
}

/// `sessions.*`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsConfig {
    pub max_global: u32,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
            max_global: DEFAULT_SESSIONS_MAX_GLOBAL,
        }
    }
}

/// `view.*`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewConfig {
    pub port_base: u16,
    #[serde(default)]
    pub advertise_base: Option<String>,
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            port_base: 6080,
            advertise_base: None,
        }
    }
}

/// `reaper.*` — idle off by default, dead-session sweep on and independent (04, 06 B1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaperConfig {
    pub idle: Idle,
    pub view_idle: Idle,
    pub sweep_dead: Toggle,
}

impl Default for ReaperConfig {
    fn default() -> Self {
        Self {
            idle: Idle::Off,
            view_idle: Idle::Off,
            sweep_dead: Toggle::On,
        }
    }
}

/// `history.*` — 8 segments × 4 MiB per session, 7-day retention from session end (04).
///
/// `segment_size` is an **integer of bytes**: 04 §Value grammar has ints, durations, enums and
/// booleans and no size literal, and 08's defaults defer to 04 — so the docs' reading is
/// bytes-as-int. Noted in the P2 report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryConfig {
    pub segments: u32,
    pub segment_size: u64,
    pub retention: Duration,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            segments: 8,
            segment_size: 4 * 1024 * 1024,
            retention: Duration::from_hours(24 * 7),
        }
    }
}

/// `media.*` — TTL 600 s or fetch-once, whichever first (04, 05 A8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    pub ttl: Duration,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(600),
        }
    }
}

/// `limits.*` — `on` unless explicitly downgraded (06 C6: the downgrade is recorded, never
/// silent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    pub mode: Toggle,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self { mode: Toggle::On }
    }
}

/// `renderer.*` — software override off; config-only, never reachable at runtime (06 D6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererConfig {
    pub allow_software: Toggle,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            allow_software: Toggle::Off,
        }
    }
}

/// The core config file (`config.ron`). `deny_unknown_fields` is what makes a typo a load
/// error instead of a silently ignored line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreConfig {
    pub version: u32,
    #[serde(default)]
    pub remote: RemoteConfig,
    #[serde(default)]
    pub manager: ManagerConfig,
    #[serde(default)]
    pub sessions: SessionsConfig,
    #[serde(default)]
    pub view: ViewConfig,
    #[serde(default)]
    pub reaper: ReaperConfig,
    #[serde(default)]
    pub history: HistoryConfig,
    #[serde(default)]
    pub media: MediaConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub renderer: RendererConfig,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            remote: RemoteConfig::default(),
            manager: ManagerConfig::default(),
            sessions: SessionsConfig::default(),
            view: ViewConfig::default(),
            reaper: ReaperConfig::default(),
            history: HistoryConfig::default(),
            media: MediaConfig::default(),
            limits: LimitsConfig::default(),
            renderer: RendererConfig::default(),
        }
    }
}

impl CoreConfig {
    /// Parse a core config file, turning RON and validation failures into a load error that
    /// names the problem (unknown keys included).
    pub fn from_ron(text: &str) -> Result<Self, ErrorEnvelope> {
        let config: Self = ron::from_str(text)
            .map_err(|err| ErrorEnvelope::bad_request(format!("config load error: {err}")))?;
        config.validate()?;
        Ok(config)
    }

    /// Load from disk; a missing file is a load error, not a default (callers decide).
    pub fn load(path: &Path) -> Result<Self, ErrorEnvelope> {
        let text = std::fs::read_to_string(path).map_err(|err| {
            ErrorEnvelope::bad_request(format!("cannot read {}: {err}", path.display()))
        })?;
        Self::from_ron(&text)
    }

    /// Load, or fall back to defaults when the file does not exist yet (cold first run).
    pub fn load_or_default(path: &Path) -> Result<Self, ErrorEnvelope> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::default())
        }
    }

    /// Serialize to RON. The struct is the schema, so what is written is always loadable.
    pub fn to_ron(&self) -> Result<String, ErrorEnvelope> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|err| ErrorEnvelope::internal(format!("cannot encode config: {err}")))
    }

    /// Write the file (creating parents), 0600 like the rest of the per-instance state.
    pub fn save(&self, path: &Path) -> Result<(), ErrorEnvelope> {
        use std::os::unix::fs::PermissionsExt;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                ErrorEnvelope::bad_request(format!("cannot create {}: {err}", parent.display()))
            })?;
        }
        let text = self.to_ron()?;
        std::fs::write(path, text).map_err(|err| {
            ErrorEnvelope::bad_request(format!("cannot write {}: {err}", path.display()))
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|err| {
            ErrorEnvelope::bad_request(format!("cannot set mode on {}: {err}", path.display()))
        })
    }

    /// `config.version` is checked at startup with a clear migration path (08).
    pub fn validate(&self) -> Result<(), ErrorEnvelope> {
        if self.version != CONFIG_VERSION {
            return Err(ErrorEnvelope::bad_request(format!(
                "config version {} is not supported by this build (expected {}); migrate the file before starting",
                self.version, CONFIG_VERSION
            )));
        }
        if let Some(base) = &self.view.advertise_base
            && base.is_empty()
        {
            return Err(ErrorEnvelope::bad_request(
                "view.advertise_base must not be empty; unset it instead",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Dot-path keys (one nesting level max)
// ---------------------------------------------------------------------------

/// Where a key's authority lives (08 A2) — `config get/set` respects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// `remote.default`, `manager.start_mode` (and the file's own `config.version`): the
    /// local file, direct; `--remote` on a client-local key is an error — these keys do not
    /// exist on a remote.
    ClientLocal,
    /// This instance's files, direct locally; RPC when `--remote` is given.
    ManagerSide,
}

/// What kind of value a key holds, and therefore which grammar applies (04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Int,
    Duration,
    DurationOrOff,
    Toggle,
    Text,
    /// Optional text (`None` when unset).
    TextOptional,
    Enum(&'static [&'static str]),
}

/// One entry of the validated key set (as pinned by 04/08).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub path: &'static str,
    pub kind: ValueKind,
    pub authority: Authority,
}

/// The complete validated key set. Additive-only in the same spirit as tool names.
pub const KEYS: &[KeySpec] = &[
    KeySpec {
        path: "config.version",
        kind: ValueKind::Int,
        // The file's own version field: it has no remote counterpart either (08 A2).
        authority: Authority::ClientLocal,
    },
    KeySpec {
        path: "remote.default",
        kind: ValueKind::TextOptional,
        authority: Authority::ClientLocal,
    },
    KeySpec {
        path: "manager.start_mode",
        kind: ValueKind::Enum(&StartMode::LITERALS),
        authority: Authority::ClientLocal,
    },
    KeySpec {
        path: "manager.bind",
        kind: ValueKind::TextOptional,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "sessions.max_global",
        kind: ValueKind::Int,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "view.port_base",
        kind: ValueKind::Int,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "view.advertise_base",
        kind: ValueKind::TextOptional,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "reaper.idle",
        kind: ValueKind::DurationOrOff,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "reaper.view_idle",
        kind: ValueKind::DurationOrOff,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "reaper.sweep_dead",
        kind: ValueKind::Toggle,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "history.segments",
        kind: ValueKind::Int,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "history.segment_size",
        kind: ValueKind::Int,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "history.retention",
        kind: ValueKind::Duration,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "media.ttl",
        kind: ValueKind::Duration,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "limits.mode",
        kind: ValueKind::Toggle,
        authority: Authority::ManagerSide,
    },
    KeySpec {
        path: "renderer.allow_software",
        kind: ValueKind::Toggle,
        authority: Authority::ManagerSide,
    },
];

/// Manager-side keys a running manager cannot adopt on `config reload`: listener bind
/// addresses are restart-only (08) — reload reports them as dirty and leaves the running
/// value in place. The token is the same class but is not a config key (07); the manager
/// tracks it separately.
pub const RESTART_ONLY_KEYS: &[&str] = &["manager.bind"];

/// A parsed config value in its grammar form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValue {
    Int(u64),
    Duration(Duration),
    Idle(Idle),
    Toggle(Toggle),
    Enum(String),
    Text(String),
    /// An unset optional (`manager.bind`, `view.advertise_base`, `remote.default`).
    Unset,
}

impl ConfigValue {
    /// The canonical grammar rendering (`config get` prints this).
    pub fn to_grammar(&self) -> String {
        match self {
            Self::Int(value) => value.to_string(),
            Self::Duration(duration) => duration.to_grammar(),
            Self::Idle(idle) => match idle.as_seconds() {
                Some(secs) => format!("{secs}s"),
                None => "off".to_string(),
            },
            Self::Toggle(toggle) => toggle.as_str().to_string(),
            Self::Enum(literal) => literal.clone(),
            Self::Text(text) => text.clone(),
            Self::Unset => "(unset)".to_string(),
        }
    }
}

/// Every valid key, comma-separated — the tail of every unknown-key load error (08 D4).
pub fn known_keys_listing() -> String {
    KEYS.iter()
        .map(|spec| spec.path)
        .collect::<Vec<_>>()
        .join(", ")
}

fn unknown_literal(raw: &str, valid: &[&str]) -> ErrorEnvelope {
    ErrorEnvelope::bad_request(format!(
        "invalid value {raw:?}; valid values: {}",
        valid.join(", ")
    ))
}

/// Find a key spec, or an error listing every valid key.
pub fn key_spec(path: &str) -> Result<&'static KeySpec, ErrorEnvelope> {
    KEYS.iter().find(|spec| spec.path == path).ok_or_else(|| {
        ErrorEnvelope::bad_request(format!(
            "unknown config key {path:?}; valid keys: {}",
            known_keys_listing()
        ))
    })
}

/// Parse a raw CLI value against a key's grammar (04 §Value grammar).
pub fn parse_value(path: &str, raw: &str) -> Result<ConfigValue, ErrorEnvelope> {
    let spec = key_spec(path)?;
    match spec.kind {
        ValueKind::Int => raw
            .parse::<u64>()
            .map(ConfigValue::Int)
            .map_err(|_| ErrorEnvelope::bad_request(format!("invalid integer {raw:?}"))),
        ValueKind::Duration => Duration::parse(raw).map(ConfigValue::Duration),
        ValueKind::DurationOrOff => {
            if raw == "off" {
                Ok(ConfigValue::Idle(Idle::Off))
            } else {
                Duration::parse(raw).map(|duration| ConfigValue::Idle(Idle::Every(duration)))
            }
        }
        ValueKind::Toggle => Toggle::parse(raw).map(ConfigValue::Toggle),
        ValueKind::Text => {
            if raw.is_empty() {
                Err(ErrorEnvelope::bad_request(format!(
                    "invalid value for {path}: expected a non-empty string"
                )))
            } else {
                Ok(ConfigValue::Text(raw.to_string()))
            }
        }
        ValueKind::TextOptional => {
            if raw == "off" || raw == "unset" {
                Ok(ConfigValue::Unset)
            } else if raw.is_empty() {
                Err(ErrorEnvelope::bad_request(format!(
                    "invalid value for {path}: expected a string, 'unset' or 'off'"
                )))
            } else {
                Ok(ConfigValue::Text(raw.to_string()))
            }
        }
        ValueKind::Enum(literals) => {
            if literals.contains(&raw) {
                Ok(ConfigValue::Enum(raw.to_string()))
            } else {
                Err(unknown_literal(raw, literals))
            }
        }
    }
}

impl CoreConfig {
    /// Dot-path read, one nesting level max.
    pub fn get(&self, path: &str) -> Result<ConfigValue, ErrorEnvelope> {
        let spec = key_spec(path)?;
        let value = match spec.path {
            "config.version" => ConfigValue::Int(u64::from(self.version)),
            "remote.default" => match &self.remote.default {
                Some(value) => ConfigValue::Text(value.clone()),
                None => ConfigValue::Unset,
            },
            "manager.start_mode" => ConfigValue::Enum(self.manager.start_mode.as_str().to_string()),
            "manager.bind" => match &self.manager.bind {
                Some(value) => ConfigValue::Text(value.clone()),
                None => ConfigValue::Unset,
            },
            "sessions.max_global" => ConfigValue::Int(u64::from(self.sessions.max_global)),
            "view.port_base" => ConfigValue::Int(u64::from(self.view.port_base)),
            "view.advertise_base" => match &self.view.advertise_base {
                Some(value) => ConfigValue::Text(value.clone()),
                None => ConfigValue::Unset,
            },
            "reaper.idle" => ConfigValue::Idle(self.reaper.idle),
            "reaper.view_idle" => ConfigValue::Idle(self.reaper.view_idle),
            "reaper.sweep_dead" => ConfigValue::Toggle(self.reaper.sweep_dead),
            "history.segments" => ConfigValue::Int(u64::from(self.history.segments)),
            "history.segment_size" => ConfigValue::Int(self.history.segment_size),
            "history.retention" => ConfigValue::Duration(self.history.retention),
            "media.ttl" => ConfigValue::Duration(self.media.ttl),
            "limits.mode" => ConfigValue::Toggle(self.limits.mode),
            "renderer.allow_software" => ConfigValue::Toggle(self.renderer.allow_software),
            other => {
                return Err(ErrorEnvelope::internal(format!(
                    "key {other:?} has a spec but no accessor; this is a contract bug"
                )));
            }
        };
        Ok(value)
    }

    /// Dot-path write, one nesting level max.
    pub fn set(&mut self, path: &str, value: ConfigValue) -> Result<(), ErrorEnvelope> {
        let spec = key_spec(path)?;
        let mismatch = || {
            ErrorEnvelope::bad_request(format!(
                "value for {path} does not match its kind ({:?})",
                spec.kind
            ))
        };
        match (spec.path, value) {
            ("config.version", ConfigValue::Int(value)) => {
                self.version = u32::try_from(value).map_err(|_| mismatch())?;
            }
            ("remote.default", ConfigValue::Unset) => self.remote.default = None,
            ("remote.default", ConfigValue::Text(value)) => self.remote.default = Some(value),
            ("manager.start_mode", ConfigValue::Enum(value)) => {
                self.manager.start_mode = StartMode::parse(&value)?;
            }
            ("manager.bind", ConfigValue::Unset) => self.manager.bind = None,
            ("manager.bind", ConfigValue::Text(value)) => self.manager.bind = Some(value),
            ("sessions.max_global", ConfigValue::Int(value)) => {
                self.sessions.max_global = u32::try_from(value).map_err(|_| mismatch())?;
            }
            ("view.port_base", ConfigValue::Int(value)) => {
                self.view.port_base = u16::try_from(value).map_err(|_| mismatch())?;
            }
            ("view.advertise_base", ConfigValue::Unset) => self.view.advertise_base = None,
            ("view.advertise_base", ConfigValue::Text(value)) => {
                self.view.advertise_base = Some(value);
            }
            ("reaper.idle", ConfigValue::Idle(value)) => self.reaper.idle = value,
            ("reaper.view_idle", ConfigValue::Idle(value)) => self.reaper.view_idle = value,
            ("reaper.sweep_dead", ConfigValue::Toggle(value)) => self.reaper.sweep_dead = value,
            ("history.segments", ConfigValue::Int(value)) => {
                self.history.segments = u32::try_from(value).map_err(|_| mismatch())?;
            }
            ("history.segment_size", ConfigValue::Int(value)) => self.history.segment_size = value,
            ("history.retention", ConfigValue::Duration(value)) => self.history.retention = value,
            ("media.ttl", ConfigValue::Duration(value)) => self.media.ttl = value,
            ("limits.mode", ConfigValue::Toggle(value)) => self.limits.mode = value,
            ("renderer.allow_software", ConfigValue::Toggle(value)) => {
                self.renderer.allow_software = value;
            }
            _ => return Err(mismatch()),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Scope files (D4)
// ---------------------------------------------------------------------------

/// `sessions.visibility` (04 grammar: `own|all`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Own,
    All,
}

impl Visibility {
    pub const LITERALS: [&'static str; 2] = ["own", "all"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::All => "all",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ErrorEnvelope> {
        match raw {
            "own" => Ok(Self::Own),
            "all" => Ok(Self::All),
            other => Err(unknown_literal(other, &Self::LITERALS)),
        }
    }
}

/// `limits.rate.mode` (04 grammar: `pace|reject`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateMode {
    Pace,
    Reject,
}

impl RateMode {
    pub const LITERALS: [&'static str; 2] = ["pace", "reject"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pace => "pace",
            Self::Reject => "reject",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ErrorEnvelope> {
        match raw {
            "pace" => Ok(Self::Pace),
            "reject" => Ok(Self::Reject),
            other => Err(unknown_literal(other, &Self::LITERALS)),
        }
    }
}

/// Scope quota fields (08 D4): `sessions.max`, `sessions.visibility`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeSessions {
    /// Per-scope session quota. Unset = limited only by the global ceiling.
    #[serde(default)]
    pub max: Option<u32>,
    pub visibility: Visibility,
}

impl Default for ScopeSessions {
    fn default() -> Self {
        Self {
            max: None,
            visibility: Visibility::Own,
        }
    }
}

/// `limits.rate { per_sec, burst, mode }` (08 D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimit {
    pub per_sec: u32,
    pub burst: u32,
    pub mode: RateMode,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            per_sec: 20,
            burst: 40,
            mode: RateMode::Pace,
        }
    }
}

/// `limits` — the per-scope rate limiter. Numbers are provisional until P4 verifies them
/// (the doc's own carried proof: "rate numbers (B9) -> P4 with 07").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeLimits {
    #[serde(default)]
    pub rate: RateLimit,
}

/// Audit flags (05 A8, 08 D4): persistence of already-captured artifacts, never a second
/// capture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeAudit {
    pub store_clips: bool,
    pub store_screenshots: bool,
}

/// The exposed manifest: deny-by-default, granted per category or per individual tool (05).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeGrant {
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
}

/// A scope definition (`scopes/<scope>.ron`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeFile {
    #[serde(default)]
    pub sessions: ScopeSessions,
    #[serde(default)]
    pub limits: ScopeLimits,
    #[serde(default)]
    pub audit: ScopeAudit,
    #[serde(default)]
    pub grant: ScopeGrant,
}

impl ScopeFile {
    pub fn from_ron(text: &str) -> Result<Self, ErrorEnvelope> {
        let scope: Self = ron::from_str(text)
            .map_err(|err| ErrorEnvelope::bad_request(format!("scope load error: {err}")))?;
        scope.validate()?;
        Ok(scope)
    }

    pub fn load(path: &Path) -> Result<Self, ErrorEnvelope> {
        let text = std::fs::read_to_string(path).map_err(|err| {
            ErrorEnvelope::bad_request(format!("cannot read {}: {err}", path.display()))
        })?;
        Self::from_ron(&text)
    }

    pub fn to_ron(&self) -> Result<String, ErrorEnvelope> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|err| ErrorEnvelope::internal(format!("cannot encode scope file: {err}")))
    }

    /// 08 D4: unknown tool or category names are load errors listing the valid names.
    pub fn validate(&self) -> Result<(), ErrorEnvelope> {
        let reported = self
            .unknown_names()
            .into_iter()
            .map(|(kind, name)| format!("{kind} {name:?}"))
            .collect::<Vec<_>>();
        if reported.is_empty() {
            return Ok(());
        }
        Err(ErrorEnvelope::bad_request(format!(
            "unknown {} in scope manifest; valid categories: {}; valid tools: {}",
            reported.join(", "),
            known_categories_listing(),
            known_tools_listing()
        )))
    }

    /// Names in this scope file that are not in the registry.
    pub fn unknown_names(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        for category in &self.grant.categories {
            if !KNOWN_CATEGORIES.contains(&category.as_str()) {
                out.push(("category", category.clone()));
            }
        }
        for tool in &self.grant.tools {
            if !KNOWN_TOOLS.iter().any(|known| known.name == tool) {
                out.push(("tool", tool.clone()));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Known names (the valid-name lists scope validation reports)
// ---------------------------------------------------------------------------

/// One known tool: its pinned name, its category, and whether it is in the always-on
/// session-lifecycle set (05: the one named exception to deny-by-default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub category: &'static str,
    pub always_on: bool,
}

/// Categories: the starter essential set from 05 plus the views family (added by this layer,
/// additive-only like tool names; noted in the P2 report).
pub const KNOWN_CATEGORIES: [&str; 6] = ["window", "input", "capture", "query", "launch", "views"];

/// Known tool names: core pinned names (05) and the first landing category (input, nine
/// tools). Additive-only.
pub const KNOWN_TOOLS: [ToolSpec; 20] = [
    // Always-on session-lifecycle set (05).
    ToolSpec {
        name: "create_session",
        category: "window",
        always_on: true,
    },
    ToolSpec {
        name: "close_session",
        category: "window",
        always_on: true,
    },
    ToolSpec {
        name: "list_sessions",
        category: "window",
        always_on: true,
    },
    ToolSpec {
        name: "session_status",
        category: "window",
        always_on: true,
    },
    ToolSpec {
        name: "list_capabilities",
        category: "window",
        always_on: true,
    },
    // Screen observation / capture.
    ToolSpec {
        name: "screenshot",
        category: "capture",
        always_on: false,
    },
    ToolSpec {
        name: "clip_video",
        category: "capture",
        always_on: false,
    },
    ToolSpec {
        name: "clip_audio",
        category: "capture",
        always_on: false,
    },
    // Views (deny-by-default, like everything that touches a running session).
    ToolSpec {
        name: "get_view",
        category: "views",
        always_on: false,
    },
    ToolSpec {
        name: "stop_view",
        category: "views",
        always_on: false,
    },
    // Query.
    ToolSpec {
        name: "list_history",
        category: "query",
        always_on: false,
    },
    // Input: the first landing category, nine tools (05).
    ToolSpec {
        name: "pointer_move",
        category: "input",
        always_on: false,
    },
    ToolSpec {
        name: "pointer_click",
        category: "input",
        always_on: false,
    },
    ToolSpec {
        name: "pointer_button_down",
        category: "input",
        always_on: false,
    },
    ToolSpec {
        name: "pointer_button_up",
        category: "input",
        always_on: false,
    },
    ToolSpec {
        name: "pointer_scroll",
        category: "input",
        always_on: false,
    },
    ToolSpec {
        name: "key_tap",
        category: "input",
        always_on: false,
    },
    ToolSpec {
        name: "key_down",
        category: "input",
        always_on: false,
    },
    ToolSpec {
        name: "key_up",
        category: "input",
        always_on: false,
    },
    ToolSpec {
        name: "type_text",
        category: "input",
        always_on: false,
    },
];

pub fn known_tools_listing() -> String {
    KNOWN_TOOLS
        .iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn known_categories_listing() -> String {
    KNOWN_CATEGORIES.join(", ")
}

/// The names a control action may take: the control tools of 05 (the input set plus the
/// session-scoped observation tools). `daiorama control <session> <action>` validates against
/// this list and refuses with a message listing it (`bad_request`, 04).
pub fn control_action_names() -> Vec<&'static str> {
    KNOWN_TOOLS
        .iter()
        .filter(|tool| !tool.always_on)
        .map(|tool| tool.name)
        .collect()
}

pub fn control_actions_listing() -> String {
    control_action_names().join(", ")
}

/// Is this a known control action?
pub fn is_control_action(name: &str) -> bool {
    KNOWN_TOOLS
        .iter()
        .any(|tool| !tool.always_on && tool.name == name)
}

// ---------------------------------------------------------------------------
// Starter files (config setup, D1)
// ---------------------------------------------------------------------------

/// The commented starter config `config setup` writes (04, 08 D4: first-run produces
/// something valid and editable, not an empty page).
pub fn starter_config() -> String {
    let mut text = String::new();
    text.push_str("// daiorama instance config (RON). Every key is optional except version.\n");
    text.push_str("// Value grammar: integers; durations <n>s|<n>m|<n>h; on|off; enum literals.\n");
    text.push_str("// This file is local to the instance; a running manager keeps old values\n");
    text.push_str("// until `daiorama config reload` (or restart).\n");
    text.push_str("(\n");
    text.push_str(&format!(
        "  version: {CONFIG_VERSION},   // checked at startup; migrate before bumping\n"
    ));
    text.push_str("  remote: (\n");
    text.push_str("    // Client-local routing value, read before any manager is located.\n");
    text.push_str("    default: None,   // Some(\"host:8756\") or Some(\"/run/user/1000/daiorama/default/manager.sock\")\n");
    text.push_str("  ),\n");
    text.push_str("  manager: (\n");
    text.push_str("    start_mode: auto,   // auto | attached | background (client-local)\n");
    text.push_str("    bind: None,         // Some(\"0.0.0.0:8756\") for TCP; a TCP bind requires the instance token\n");
    text.push_str("  ),\n");
    text.push_str(&format!(
        "  sessions: (\n    max_global: {DEFAULT_SESSIONS_MAX_GLOBAL},   // global session ceiling (dead sessions do not count)\n  ),\n"
    ));
    text.push_str("  view: (\n");
    text.push_str("    port_base: 6080,      // front ports allocate upward from here\n");
    text.push_str("    advertise_base: None, // Some(\"https://...\") to serve view addresses outside this namespace\n");
    text.push_str("  ),\n");
    text.push_str("  reaper: (\n");
    text.push_str(
        "    idle: off,          // off | every(3600) — session idle reaper, off by default\n",
    );
    text.push_str("    view_idle: off,     // applies to watched-only sessions\n");
    text.push_str(
        "    sweep_dead: on,     // dead-session sweep, independent of the idle reaper\n",
    );
    text.push_str("  ),\n");
    text.push_str("  history: (\n");
    text.push_str("    segments: 8,          // segments per session\n");
    text.push_str("    segment_size: 4194304,// bytes per segment (4 MiB)\n");
    text.push_str("    retention: 604800,    // seconds from session end (7 days)\n");
    text.push_str("  ),\n");
    text.push_str("  media: (\n");
    text.push_str("    ttl: 600,   // seconds; clip links are also fetch-once\n");
    text.push_str("  ),\n");
    text.push_str("  limits: (\n");
    text.push_str("    mode: on,   // off = no per-session cgroups, no crash-time cleanup (recorded downgrade)\n");
    text.push_str("  ),\n");
    text.push_str("  renderer: (\n");
    text.push_str("    allow_software: off,   // config-only override for protocol-test lanes; never a CLI flag\n");
    text.push_str("  ),\n");
    text.push_str(")\n");
    text
}

/// The commented starter scope file `config setup` offers — the CLI's own `operator` scope,
/// operator-wide visibility (04 `list`).
pub fn starter_scope_operator() -> String {
    let mut text = String::new();
    text.push_str(
        "// daiorama scope definition (RON). One file per scope; the filename is the scope name.\n",
    );
    text.push_str(
        "// Deny-by-default: nothing is exposed until a scope grants it. The always-on\n",
    );
    text.push_str("// session-lifecycle set (create_session, close_session, list_sessions,\n");
    text.push_str(
        "// session_status, list_capabilities) is not listed here — it is always available.\n",
    );
    text.push_str("// Editing this file affects the next call (scopes are read per call).\n");
    text.push_str("(\n");
    text.push_str("  sessions: (\n");
    text.push_str(
        "    max: None,          // per-scope session quota; unset = global ceiling only\n",
    );
    text.push_str("    visibility: all,    // 'operator' sees every scope's sessions (04)\n");
    text.push_str("  ),\n");
    text.push_str("  limits: (\n");
    text.push_str("    rate: (\n");
    text.push_str("      per_sec: 20,\n");
    text.push_str("      burst: 40,\n");
    text.push_str("      mode: pace,      // pace | reject\n");
    text.push_str("    ),\n");
    text.push_str("  ),\n");
    text.push_str("  audit: (\n");
    text.push_str("    store_clips: false,\n");
    text.push_str("    store_screenshots: false,\n");
    text.push_str("  ),\n");
    text.push_str("  grant: (\n");
    text.push_str("    // Deny-by-default: grant categories or individual tools.\n");
    text.push_str(&format!(
        "    categories: [],   // {}\n",
        known_categories_listing()
    ));
    text.push_str("    tools: [],\n");
    text.push_str("  ),\n");
    text.push_str(")\n");
    text
}

// ---------------------------------------------------------------------------
// Effective view (config effective, D4)
// ---------------------------------------------------------------------------

/// The resolved view `config effective` prints: which values apply where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveView {
    pub core: Vec<(&'static str, String)>,
    pub scopes: Vec<(String, ScopeFile)>,
}

pub fn effective_view(config: &CoreConfig, scopes: Vec<(String, ScopeFile)>) -> EffectiveView {
    let core = KEYS
        .iter()
        .filter_map(|spec| {
            config
                .get(spec.path)
                .ok()
                .map(|value| (spec.path, value.to_grammar()))
        })
        .collect();
    EffectiveView { core, scopes }
}

// ---------------------------------------------------------------------------
// Token file (07, 08)
// ---------------------------------------------------------------------------

/// Generate an instance token: 122 bits of v4 randomness, never written to logs (07).
pub fn generate_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Read the token file; `None` when it does not exist yet.
pub fn read_token(paths: &Paths) -> Result<Option<String>, ErrorEnvelope> {
    let path = paths.token_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text.trim().to_string())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ErrorEnvelope::bad_request(format!(
            "cannot read {}: {err}",
            path.display()
        ))),
    }
}

/// Write the token file at mode 0600 (its own file, never in ordinary config).
pub fn write_token(paths: &Paths, token: &str) -> Result<PathBuf, ErrorEnvelope> {
    use std::os::unix::fs::PermissionsExt;
    let path = paths.token_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            ErrorEnvelope::bad_request(format!("cannot create {}: {err}", parent.display()))
        })?;
    }
    std::fs::write(&path, format!("{token}\n")).map_err(|err| {
        ErrorEnvelope::bad_request(format!("cannot write {}: {err}", path.display()))
    })?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|err| {
        ErrorEnvelope::bad_request(format!("cannot set mode on {}: {err}", path.display()))
    })?;
    Ok(path)
}

/// The mode of the token file, for `token show`'s 0600 heads-up.
pub fn token_file_mode(paths: &Paths) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(paths.token_file())
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o777)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(root: &Path) -> Paths {
        Paths::for_env(
            Some(&root.join("config")),
            Some(&root.join("data")),
            None,
            "default",
        )
    }

    /// 04's pinned defaults are what `CoreConfig::default()` carries.
    #[test]
    fn defaults_match_the_pinned_04_table() {
        let config = CoreConfig::default();
        assert_eq!(config.version, 1);
        assert_eq!(config.remote.default, None);
        assert_eq!(config.manager.start_mode, StartMode::Auto);
        assert_eq!(config.manager.bind, None);
        assert_eq!(config.view.port_base, 6080);
        assert_eq!(config.view.advertise_base, None);
        assert_eq!(config.reaper.idle, Idle::Off, "idle reaper off by default");
        assert_eq!(config.reaper.view_idle, Idle::Off);
        assert_eq!(
            config.reaper.sweep_dead,
            Toggle::On,
            "dead sweep on, independent"
        );
        assert_eq!(config.history.segments, 8);
        assert_eq!(config.history.segment_size, 4 * 1024 * 1024);
        assert_eq!(config.history.retention, Duration::from_hours(24 * 7));
        assert_eq!(config.media.ttl, Duration::from_secs(600));
        assert_eq!(config.limits.mode, Toggle::On);
        assert_eq!(config.renderer.allow_software, Toggle::Off);
    }

    #[test]
    fn config_round_trips_through_ron() {
        let config = CoreConfig::default();
        let text = config.to_ron().unwrap();
        let parsed = CoreConfig::from_ron(&text).unwrap();
        assert_eq!(parsed, config);

        // A hand-written minimal file loads against the defaults.
        let parsed = CoreConfig::from_ron("(version: 1)").unwrap();
        assert_eq!(parsed, CoreConfig::default());
    }

    #[test]
    fn unknown_keys_are_load_errors_listing_the_valid_names() {
        let err = CoreConfig::from_ron("(version: 1, manager: (start_mod: auto))").unwrap_err();
        assert!(err.message.contains("start_mod"), "{}", err.message);
        assert!(err.message.contains("config load error"), "{}", err.message);

        let err = key_spec("manager.nope").unwrap_err();
        assert!(err.message.contains("manager.nope"));
        for spec in KEYS {
            assert!(
                err.message.contains(spec.path),
                "listing omits {}",
                spec.path
            );
        }
    }

    #[test]
    fn value_grammar_parses_durations_enums_toggles_and_ints() {
        assert_eq!(
            parse_value("media.ttl", "600").unwrap(),
            ConfigValue::Duration(Duration::from_secs(600))
        );
        assert_eq!(
            parse_value("media.ttl", "600s").unwrap(),
            ConfigValue::Duration(Duration::from_secs(600))
        );
        assert_eq!(
            parse_value("media.ttl", "10m").unwrap(),
            ConfigValue::Duration(Duration::from_secs(600))
        );
        assert_eq!(
            parse_value("media.ttl", "1h").unwrap(),
            ConfigValue::Duration(Duration::from_secs(3600))
        );
        assert_eq!(
            parse_value("history.retention", "168h").unwrap(),
            ConfigValue::Duration(Duration::from_hours(168))
        );
        assert!(
            parse_value("media.ttl", "7d").is_err(),
            "04 grammar has no days"
        );
        assert!(parse_value("media.ttl", "m").is_err());
        assert!(parse_value("media.ttl", "10x").is_err());
        assert!(parse_value("media.ttl", "-5s").is_err());

        assert_eq!(
            parse_value("limits.mode", "off").unwrap(),
            ConfigValue::Toggle(Toggle::Off)
        );
        assert_eq!(
            parse_value("limits.mode", "on").unwrap(),
            ConfigValue::Toggle(Toggle::On)
        );
        assert_eq!(
            parse_value("reaper.sweep_dead", "on").unwrap(),
            ConfigValue::Toggle(Toggle::On)
        );
        assert!(parse_value("limits.mode", "true").is_err());
        assert!(
            parse_value("limits.mode", "On").is_err(),
            "enum literals are exact"
        );

        assert_eq!(
            parse_value("reaper.idle", "off").unwrap(),
            ConfigValue::Idle(Idle::Off)
        );
        assert_eq!(
            parse_value("reaper.idle", "1h").unwrap(),
            ConfigValue::Idle(Idle::Every(Duration::from_hours(1)))
        );

        assert_eq!(
            parse_value("manager.start_mode", "background").unwrap(),
            ConfigValue::Enum("background".to_string())
        );
        assert!(parse_value("manager.start_mode", "detached").is_err());

        assert_eq!(
            parse_value("view.port_base", "6081").unwrap(),
            ConfigValue::Int(6081)
        );
        assert!(parse_value("view.port_base", "6081x").is_err());
        assert_eq!(
            parse_value("view.advertise_base", "unset").unwrap(),
            ConfigValue::Unset
        );
        assert_eq!(
            parse_value("view.advertise_base", "https://example.invalid:6080").unwrap(),
            ConfigValue::Text("https://example.invalid:6080".to_string())
        );
    }

    #[test]
    fn get_set_round_trip_for_every_key() {
        let mut config = CoreConfig::default();
        let cases: [(&str, &str); 16] = [
            ("config.version", "1"),
            ("remote.default", "127.0.0.1:8756"),
            ("manager.start_mode", "attached"),
            ("manager.bind", "0.0.0.0:8756"),
            ("sessions.max_global", "9"),
            ("view.port_base", "7000"),
            ("view.advertise_base", "https://view.example.invalid"),
            ("reaper.idle", "1h"),
            ("reaper.view_idle", "30m"),
            ("reaper.sweep_dead", "off"),
            ("history.segments", "4"),
            ("history.segment_size", "1048576"),
            ("history.retention", "24h"),
            ("media.ttl", "120s"),
            ("limits.mode", "off"),
            ("renderer.allow_software", "on"),
        ];
        for (path, raw) in cases {
            let value = parse_value(path, raw).unwrap();
            config.set(path, value.clone()).unwrap();
            assert_eq!(config.get(path).unwrap(), value, "round trip for {path}");
        }
        // And the whole thing still round-trips through RON.
        let parsed = CoreConfig::from_ron(&config.to_ron().unwrap()).unwrap();
        assert_eq!(parsed, config);

        // Grammar renderings.
        assert_eq!(config.get("media.ttl").unwrap().to_grammar(), "120s");
        assert_eq!(config.get("limits.mode").unwrap().to_grammar(), "off");
        assert_eq!(
            config.get("manager.start_mode").unwrap().to_grammar(),
            "attached"
        );
        assert_eq!(config.get("sessions.max_global").unwrap().to_grammar(), "9");
    }

    #[test]
    fn version_is_checked_with_a_migration_message() {
        let err = CoreConfig::from_ron("(version: 2)").unwrap_err();
        assert!(err.message.contains("config version 2"), "{}", err.message);
        assert!(err.message.contains("migrate"), "{}", err.message);
    }

    #[test]
    fn authority_is_client_local_for_client_keys_only() {
        let authority = |path: &str| key_spec(path).unwrap().authority;
        assert_eq!(authority("remote.default"), Authority::ClientLocal);
        assert_eq!(authority("manager.start_mode"), Authority::ClientLocal);
        assert_eq!(authority("media.ttl"), Authority::ManagerSide);
        assert_eq!(authority("limits.mode"), Authority::ManagerSide);
        assert_eq!(authority("view.advertise_base"), Authority::ManagerSide);
    }

    #[test]
    fn scope_files_validate_names_and_quotas() {
        let good = ScopeFile::from_ron(
            r#"(
              sessions: (max: Some(4), visibility: all),
              limits: (rate: (per_sec: 10, burst: 20, mode: reject)),
              audit: (store_clips: true, store_screenshots: false),
              grant: (categories: ["input"], tools: ["screenshot"]),
            )"#,
        )
        .unwrap();
        assert_eq!(good.sessions.visibility, Visibility::All);
        assert_eq!(good.sessions.max, Some(4));
        assert_eq!(good.limits.rate.mode, RateMode::Reject);
        assert!(good.audit.store_clips);
        assert!(good.unknown_names().is_empty());

        let bad = ScopeFile::from_ron(
            r#"( grant: (categories: ["input", "telepathy"], tools: ["screenshot", "make_coffee"]) )"#,
        )
        .unwrap_err();
        assert!(bad.message.contains("\"telepathy\""), "{}", bad.message);
        assert!(bad.message.contains("\"make_coffee\""), "{}", bad.message);
        assert!(
            bad.message.contains("input"),
            "valid names are listed: {}",
            bad.message
        );
        assert!(
            bad.message.contains("screenshot"),
            "valid names are listed: {}",
            bad.message
        );

        // Unknown fields inside a scope are load errors too.
        let bad = ScopeFile::from_ron("( grantt: () )").unwrap_err();
        assert!(bad.message.contains("scope load error"), "{}", bad.message);

        // Visibility literals are exact.
        assert!(ScopeFile::from_ron("(sessions: (visibility: everything))").is_err());
    }

    /// The nine input tools are pinned by 05 §Tool exposure model and must all be known.
    #[test]
    fn the_pinned_input_category_names_are_registered() {
        let nine = [
            "pointer_move",
            "pointer_click",
            "pointer_button_down",
            "pointer_button_up",
            "pointer_scroll",
            "key_tap",
            "key_down",
            "key_up",
            "type_text",
        ];
        for name in nine {
            assert!(
                is_control_action(name),
                "{name} must be a known control action"
            );
        }
        assert_eq!(
            control_action_names().len(),
            KNOWN_TOOLS.iter().filter(|tool| !tool.always_on).count()
        );
        // The always-on lifecycle set is never a control action.
        for name in [
            "create_session",
            "close_session",
            "list_sessions",
            "session_status",
            "list_capabilities",
        ] {
            assert!(
                !is_control_action(name),
                "{name} is always-on, not a control action"
            );
        }
        assert!(control_actions_listing().contains("pointer_move"));
    }

    #[test]
    fn starter_files_are_valid_and_commented() {
        let config_text = starter_config();
        assert!(config_text.contains("//"), "starter config is commented");
        let parsed = CoreConfig::from_ron(&config_text).unwrap();
        assert_eq!(parsed, CoreConfig::default());

        let scope_text = starter_scope_operator();
        assert!(scope_text.contains("//"));
        let scope = ScopeFile::from_ron(&scope_text).unwrap();
        assert_eq!(
            scope.sessions.visibility,
            Visibility::All,
            "the operator scope is operator-wide"
        );
        assert!(
            scope.grant.tools.is_empty(),
            "deny-by-default starter manifest"
        );
    }

    #[test]
    fn paths_follow_the_08_table() {
        let root = Path::new("/scratch/root");
        let paths = paths_in(root);
        assert_eq!(
            paths.core_config(),
            Path::new("/scratch/root/config/daiorama/default/config.ron")
        );
        assert_eq!(
            paths.scope_file("operator"),
            Path::new("/scratch/root/config/daiorama/default/scopes/operator.ron")
        );
        assert_eq!(
            paths.token_file(),
            Path::new("/scratch/root/data/daiorama/default/token")
        );
        assert_eq!(
            paths.history_dir(),
            Path::new("/scratch/root/data/daiorama/default/history")
        );
        assert_eq!(
            paths.media_dir(),
            Path::new("/scratch/root/data/daiorama/default/media")
        );
        assert_eq!(
            paths.modules_dir(),
            Path::new("/scratch/root/data/daiorama/default/modules")
        );

        // XDG unset: ~/.config and ~/.local/share.
        let paths = Paths::for_env(None, None, Some(Path::new("/home/operator")), "beta");
        assert_eq!(
            paths.core_config(),
            Path::new("/home/operator/.config/daiorama/beta/config.ron")
        );
        assert_eq!(
            paths.token_file(),
            Path::new("/home/operator/.local/share/daiorama/beta/token")
        );
    }

    #[test]
    fn token_file_is_0600_and_rotatable() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        assert_eq!(read_token(&paths).unwrap(), None);

        let token = generate_token();
        assert_eq!(token.len(), 32, "122 bits of randomness, hex encoded");
        let path = write_token(&paths, &token).unwrap();
        assert_eq!(path, paths.token_file());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(read_token(&paths).unwrap().as_deref(), Some(token.as_str()));
        assert_eq!(token_file_mode(&paths), Some(0o600));

        // Rotation writes a new file with a new value.
        let rotated = generate_token();
        assert_ne!(rotated, token);
        write_token(&paths, &rotated).unwrap();
        assert_eq!(
            read_token(&paths).unwrap().as_deref(),
            Some(rotated.as_str())
        );
    }

    #[test]
    fn config_files_are_written_0600_and_load_back() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        let mut config = CoreConfig::default();
        config
            .set("media.ttl", ConfigValue::Duration(Duration::from_secs(120)))
            .unwrap();
        config.save(&paths.core_config()).unwrap();
        let mode = std::fs::metadata(paths.core_config())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(CoreConfig::load(&paths.core_config()).unwrap(), config);
        assert_eq!(
            CoreConfig::load_or_default(&paths.scope_file("operator")).unwrap(),
            CoreConfig::default()
        );
    }

    #[test]
    fn effective_view_lists_every_key_and_each_scope() {
        let config = CoreConfig::default();
        let scope = ScopeFile::from_ron("(sessions: (visibility: all))").unwrap();
        let view = effective_view(&config, vec![("operator".to_string(), scope)]);
        assert_eq!(view.core.len(), KEYS.len());
        assert_eq!(view.core[0].0, "config.version");
        assert_eq!(view.scopes[0].0, "operator");
    }
}
