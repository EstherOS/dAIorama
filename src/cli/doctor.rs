//! `daiorama doctor` — the host-runnable environment check (04 §Diagnostics).
//!
//! Live since W3. Three sections, plain greppable labels; doctor is a cold command — it
//! never touches a manager and answers from this machine's environment alone:
//!
//! - `tools:` — Hyprland and wayvnc presence + version, the 04 strings verbatim when a tool
//!   is missing or out of range, and the install command resolved from the maintained
//!   table's in-binary rows (09).
//! - `wayland:` — `WAYLAND_DISPLAY` (nested-backend detection, 03) and `XDG_RUNTIME_DIR`
//!   facts (the manager socket's pinned home, 08).
//! - `containment:` — the same delegation check the manager start runs before binding (06
//!   C6), through the existing probe; a failing check carries the pinned fix string.
//! - `assets:` — the view asset tree at `<data>/novnc` (07/09): present, or the front's pinned
//!   `view_unavailable` message plus the staging command that fixes it. The check reads the
//!   entry point the front actually looks up (`vnc.html`), not merely the directory.
//!
//! Out of this layer's remit by design: the in-container renderer verification and the
//! GPU-family detection stay runtime-side (03/09; `fixtures/README.md` §Containment classes).
//!
//! Provisional shapes where the specs pin none (additive, flagged): the section labels, the
//! `--json` key set, the unknown-version and wayvnc-missing renderings, and the in-binary
//! install-table rows (fedora/arch carried; the generic row renders the no-package string and
//! never a command that cannot succeed). Version probing is best-effort — `Hyprland
//! --version`, falling back to `hyprctl version` (the P1-practiced read), `wayvnc --version`;
//! each probe is time-bounded and killed on timeout.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::manager::startup::ContainmentProbe;
use crate::runtime::containment::LinuxCgroupProbe;
use crate::strings;

use super::Answer;

/// How long one best-effort tool version probe may run before it is killed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Poll interval while a probe runs.
const PROBE_POLL: Duration = Duration::from_millis(20);

// ---------------------------------------------------------------------------
// Facts (probed by `gather()`, injected synthetically by tests)
// ---------------------------------------------------------------------------

/// One tool's presence (path) and best-effort version.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolFacts {
    path: PathBuf,
    version: Option<String>,
}

/// `XDG_RUNTIME_DIR` facts: unset, or set with its writability.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeDirFacts {
    Unset,
    Present { path: PathBuf, writable: bool },
}

/// `/etc/os-release` facts the install rows key on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OsFacts {
    id: String,
}

impl OsFacts {
    fn detect() -> Self {
        let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        Self::parse(&text)
    }

    fn parse(text: &str) -> Self {
        let id = text
            .lines()
            .find_map(|line| line.strip_prefix("ID="))
            .map(|value| value.trim().trim_matches('"').to_string())
            .unwrap_or_default();
        Self { id }
    }
}

/// Everything the report renders, gathered once.
#[derive(Debug, Clone)]
struct DoctorFacts {
    hyprland: Option<ToolFacts>,
    wayvnc: Option<ToolFacts>,
    wayland_display: Option<String>,
    runtime_dir: RuntimeDirFacts,
    containment: Result<String, String>,
    assets: AssetFacts,
    os: OsFacts,
}

/// The view asset tree (07): the front serves `<data>/novnc` and looks up `vnc.html`; 09 owns
/// staging it. `installed` is the front's own test — the entry point, not the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AssetFacts {
    path: PathBuf,
    installed: bool,
}

impl AssetFacts {
    fn probe(path: PathBuf) -> Self {
        let installed = path.join("vnc.html").is_file();
        Self { path, installed }
    }
}

impl DoctorFacts {
    /// Probe the real machine. The asset root follows 08 like every other data path, so
    /// `--instance` selects the tree the matching manager would serve.
    fn gather(instance: &str) -> Self {
        let hyprland = find_tool("Hyprland").map(|path| {
            let version = hyprland_version(&path, find_tool("hyprctl").as_deref());
            ToolFacts { path, version }
        });
        let wayvnc = find_tool("wayvnc").map(|path| {
            let version = probe_output(&path, &["--version"]).and_then(|t| first_version_token(&t));
            ToolFacts { path, version }
        });
        let wayland_display = std::env::var("WAYLAND_DISPLAY")
            .ok()
            .filter(|value| !value.is_empty());
        let runtime_dir = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) if !dir.is_empty() => {
                let path = PathBuf::from(dir);
                let writable = is_writable_dir(&path);
                RuntimeDirFacts::Present { path, writable }
            }
            _ => RuntimeDirFacts::Unset,
        };
        let containment = LinuxCgroupProbe::new().probe().map(|d| d.detail);
        let assets = AssetFacts::probe(crate::config::Paths::from_process(instance).novnc_dir());
        Self {
            hyprland,
            wayvnc,
            wayland_display,
            runtime_dir,
            containment,
            assets,
            os: OsFacts::detect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

/// Find a binary on `PATH` (executable file).
fn find_tool(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(name);
        match std::fs::metadata(&candidate) {
            Ok(meta) if meta.is_file() && (meta.permissions().mode() & 0o111 != 0) => {
                Some(candidate)
            }
            _ => None,
        }
    })
}

/// Hyprland's version: `Hyprland --version` (the binary flag), falling back to
/// `hyprctl version` — the read P1 practiced inside a live instance.
fn hyprland_version(hyprland: &Path, hyprctl: Option<&Path>) -> Option<String> {
    probe_output(hyprland, &["--version"])
        .and_then(|text| first_version_token(&text))
        .or_else(|| {
            hyprctl
                .and_then(|ctl| probe_output(ctl, &["version"]))
                .and_then(|text| first_version_token(&text))
        })
}

/// Run one bounded probe and return its combined output as text. Spawn failure or timeout is
/// `None`; a version tool that exits non-zero but prints anyway still yields its output.
fn probe_output(bin: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(PROBE_POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut text = String::new();
    if let Some(stdout) = child.stdout.take() {
        text.push_str(&read_pipe(stdout));
    }
    if let Some(stderr) = child.stderr.take() {
        text.push_str(&read_pipe(stderr));
    }
    Some(text)
}

fn read_pipe(pipe: impl Read) -> String {
    let mut buffer = Vec::new();
    let _ = pipe.take(64 * 1024).read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).into_owned()
}

/// The first `major.minor[.patch]`-shaped token in `text` (scans runs of digits and dots, so
/// `v0.56.2-1-gabc` yields `0.56.2`).
fn first_version_token(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                j += 1;
            }
            let candidate = text[start..j].trim_end_matches('.');
            if version_tuple(candidate).is_some() {
                return Some(candidate.to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

/// `major.minor[.patch]` as a comparable triple; single-component runs are not versions.
fn version_tuple(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = match parts.next() {
        Some(part) => part.parse::<u64>().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn version_in_range(found: &str, min: &str, max: &str) -> Option<bool> {
    let found = version_tuple(found)?;
    let min = version_tuple(min)?;
    let max = version_tuple(max)?;
    Some(found >= min && found <= max)
}

/// Directory exists and this process may write it (`access(2)`, the same right a bind needs).
fn is_writable_dir(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    if !path.is_dir() {
        return false;
    }
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

// ---------------------------------------------------------------------------
// Install rows (09's maintained table, in-binary subset)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Package {
    Hyprland,
    Wayvnc,
}

impl Package {
    fn name(self) -> &'static str {
        match self {
            Package::Hyprland => "hyprland",
            Package::Wayvnc => "wayvnc",
        }
    }
}

/// Resolve the install command for the detected system. Fedora's hyprland is orphaned (no
/// f43+ builds; the f41/f42-era package is out of the supported range) and the maintained
/// table routes it to a COPR/upstream build — the same no-package rendering the generic row
/// uses; a command that cannot succeed is never printed (09, audit F9).
fn resolve_install(package: Package, os: &OsFacts) -> String {
    match (os.id.as_str(), package) {
        ("arch", _) => format!("sudo pacman -S {}", package.name()),
        ("fedora", Package::Wayvnc) => "sudo dnf install wayvnc".to_string(),
        _ => strings::HYPRLAND_NOT_PACKAGED.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Probe this machine and render the report.
pub fn report(instance: &str) -> Answer {
    render(&DoctorFacts::gather(instance))
}

fn render(facts: &DoctorFacts) -> Answer {
    let mut lines = vec![strings::DOCTOR_HEADER.to_string()];
    lines.push(strings::DOCTOR_TOOLS_LABEL.to_string());
    lines.extend(tools_lines(facts));
    lines.push(strings::DOCTOR_WAYLAND_LABEL.to_string());
    lines.extend(wayland_lines(facts));
    lines.push(strings::DOCTOR_CONTAINMENT_LABEL.to_string());
    lines.push(containment_line(facts));
    lines.push(strings::DOCTOR_ASSETS_LABEL.to_string());
    lines.extend(assets_lines(facts));
    let payload = json!({
        "tools": {
            "hyprland": tool_json(
                facts.hyprland.as_ref(),
                supported_hyprland(facts),
                resolve_install(Package::Hyprland, &facts.os),
            ),
            "wayvnc": tool_json(
                facts.wayvnc.as_ref(),
                None,
                resolve_install(Package::Wayvnc, &facts.os),
            ),
        },
        "wayland": wayland_json(facts),
        "containment": match &facts.containment {
            Ok(detail) => json!({"ok": true, "detail": detail}),
            Err(detail) => json!({"ok": false, "detail": detail}),
        },
        "assets": assets_json(facts),
    });
    Answer::new(payload, lines)
}

fn tools_lines(facts: &DoctorFacts) -> Vec<String> {
    let mut lines = Vec::new();
    match &facts.hyprland {
        None => lines.push(format!(
            "  {}",
            strings::hyprland_missing(&resolve_install(Package::Hyprland, &facts.os))
        )),
        Some(tool) => {
            let path = tool.path.display();
            match &tool.version {
                Some(version) => match supported_hyprland(facts) {
                    Some(false) => lines.push(format!(
                        "  {}",
                        strings::hyprland_unsupported(
                            version,
                            strings::HYPRLAND_MIN_VERSION,
                            strings::HYPRLAND_MAX_VERSION
                        )
                    )),
                    _ => lines.push(format!("  hyprland: {version} ({path})")),
                },
                None => lines.push(format!("  hyprland: version unknown ({path})")),
            }
        }
    }
    match &facts.wayvnc {
        None => lines.push(format!(
            "  wayvnc: missing — install it: {}",
            resolve_install(Package::Wayvnc, &facts.os)
        )),
        Some(tool) => {
            let path = tool.path.display();
            match &tool.version {
                Some(version) => lines.push(format!("  wayvnc: {version} ({path})")),
                None => lines.push(format!("  wayvnc: version unknown ({path})")),
            }
        }
    }
    lines
}

/// `Some(true)` in range, `Some(false)` out of range, `None` when no version was readable.
fn supported_hyprland(facts: &DoctorFacts) -> Option<bool> {
    let version = facts.hyprland.as_ref()?.version.as_deref()?;
    version_in_range(
        version,
        strings::HYPRLAND_MIN_VERSION,
        strings::HYPRLAND_MAX_VERSION,
    )
}

fn wayland_lines(facts: &DoctorFacts) -> Vec<String> {
    let mut lines = Vec::new();
    match &facts.wayland_display {
        Some(display) => lines.push(format!("  WAYLAND_DISPLAY: {display}")),
        None => lines.push(
            "  WAYLAND_DISPLAY: unset (nested backend unavailable; headless unaffected)"
                .to_string(),
        ),
    }
    match &facts.runtime_dir {
        RuntimeDirFacts::Unset => lines.push("  XDG_RUNTIME_DIR: unset".to_string()),
        RuntimeDirFacts::Present { path, writable } => lines.push(format!(
            "  XDG_RUNTIME_DIR: {} ({})",
            path.display(),
            if *writable {
                "writable"
            } else {
                "not writable"
            }
        )),
    }
    lines
}

fn containment_line(facts: &DoctorFacts) -> String {
    match &facts.containment {
        Ok(detail) => format!("  ok — {detail}"),
        Err(detail) => format!("  {}", strings::containment_unavailable(detail)),
    }
}

/// The asset section's lines: installed names the tree the front will serve; missing carries
/// the front's own refusal verbatim (the operator greps the same string in the 503) and the
/// staging command that resolves it.
fn assets_lines(facts: &DoctorFacts) -> Vec<String> {
    let path = facts.assets.path.display().to_string();
    if facts.assets.installed {
        vec![format!("  noVNC: installed at {path}")]
    } else {
        vec![
            format!("  {}", strings::VIEW_ASSETS_MISSING),
            format!("  {}", strings::novnc_install_hint(&path)),
        ]
    }
}

fn assets_json(facts: &DoctorFacts) -> Value {
    let path = facts.assets.path.display().to_string();
    json!({
        "path": path,
        "installed": facts.assets.installed,
        "install": if facts.assets.installed {
            Value::Null
        } else {
            json!(strings::novnc_install_hint(&path))
        },
    })
}

fn tool_json(tool: Option<&ToolFacts>, supported: Option<bool>, install: String) -> Value {
    match tool {
        Some(tool) => json!({
            "path": tool.path.display().to_string(),
            "version": tool.version,
            "supported": supported,
            "install": Value::Null,
        }),
        None => json!({
            "path": Value::Null,
            "version": Value::Null,
            "supported": Value::Null,
            "install": install,
        }),
    }
}

fn wayland_json(facts: &DoctorFacts) -> Value {
    match &facts.runtime_dir {
        RuntimeDirFacts::Unset => json!({
            "wayland_display": facts.wayland_display,
            "runtime_dir": Value::Null,
            "runtime_dir_writable": Value::Null,
        }),
        RuntimeDirFacts::Present { path, writable } => json!({
            "wayland_display": facts.wayland_display,
            "runtime_dir": path.display().to_string(),
            "runtime_dir_writable": writable,
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests (synthetic facts; the probes themselves are covered by the CLI integration test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(path: &str, version: Option<&str>) -> Option<ToolFacts> {
        Some(ToolFacts {
            path: PathBuf::from(path),
            version: version.map(str::to_string),
        })
    }

    fn base_facts() -> DoctorFacts {
        DoctorFacts {
            hyprland: tool("/usr/bin/Hyprland", Some("0.56.2")),
            wayvnc: tool("/usr/bin/wayvnc", Some("0.9.1")),
            wayland_display: Some("wayland-1".to_string()),
            runtime_dir: RuntimeDirFacts::Present {
                path: PathBuf::from("/run/user/1000"),
                writable: true,
            },
            containment: Ok("delegated: cpu memory pids already enabled".to_string()),
            assets: AssetFacts {
                path: PathBuf::from("/home/agent/.local/share/daiorama/default/novnc"),
                installed: true,
            },
            os: OsFacts {
                id: "arch".to_string(),
            },
        }
    }

    #[test]
    fn version_token_scanning() {
        assert_eq!(
            first_version_token(
                "Hyprland, built from branch main at commit abc123 (version 0.56.2)"
            ),
            Some("0.56.2".to_string())
        );
        assert_eq!(
            first_version_token("Tag: v0.56.2-1-gdeadbeef"),
            Some("0.56.2".to_string())
        );
        assert_eq!(
            first_version_token("wayvnc: version 0.9.1\nneatvnc 0.8.0"),
            Some("0.9.1".to_string())
        );
        assert_eq!(first_version_token("no digits here"), None);
        assert_eq!(
            first_version_token("commit 1234abcd released 2026-10-16"),
            None
        );
    }

    #[test]
    fn range_check_semantics() {
        let (min, max) = (strings::HYPRLAND_MIN_VERSION, strings::HYPRLAND_MAX_VERSION);
        assert_eq!(version_in_range("0.56.2", min, max), Some(true));
        assert_eq!(version_in_range("0.56.0", min, max), Some(true));
        assert_eq!(version_in_range("0.55.9", min, max), Some(false));
        assert_eq!(version_in_range("0.57.0", min, max), Some(false));
        assert_eq!(version_in_range("0.56", min, max), Some(true));
    }

    #[test]
    fn install_rows_resolve_per_system() {
        let arch = OsFacts {
            id: "arch".to_string(),
        };
        let fedora = OsFacts {
            id: "fedora".to_string(),
        };
        let other = OsFacts {
            id: "ubuntu".to_string(),
        };
        assert_eq!(
            resolve_install(Package::Hyprland, &arch),
            "sudo pacman -S hyprland"
        );
        assert_eq!(
            resolve_install(Package::Wayvnc, &arch),
            "sudo pacman -S wayvnc"
        );
        assert_eq!(
            resolve_install(Package::Hyprland, &fedora),
            strings::HYPRLAND_NOT_PACKAGED
        );
        assert_eq!(
            resolve_install(Package::Wayvnc, &fedora),
            "sudo dnf install wayvnc"
        );
        assert_eq!(
            resolve_install(Package::Hyprland, &other),
            strings::HYPRLAND_NOT_PACKAGED
        );
    }

    #[test]
    fn report_renders_the_healthy_shape() {
        let answer = render(&base_facts());
        assert_eq!(answer.human[0], strings::DOCTOR_HEADER);
        assert_eq!(answer.human[1], strings::DOCTOR_TOOLS_LABEL);
        assert_eq!(answer.human[2], "  hyprland: 0.56.2 (/usr/bin/Hyprland)");
        assert_eq!(answer.human[3], "  wayvnc: 0.9.1 (/usr/bin/wayvnc)");
        assert_eq!(answer.human[4], strings::DOCTOR_WAYLAND_LABEL);
        assert_eq!(answer.human[5], "  WAYLAND_DISPLAY: wayland-1");
        assert_eq!(
            answer.human[6],
            "  XDG_RUNTIME_DIR: /run/user/1000 (writable)"
        );
        assert_eq!(answer.human[7], strings::DOCTOR_CONTAINMENT_LABEL);
        assert_eq!(
            answer.human[8],
            "  ok — delegated: cpu memory pids already enabled"
        );
        assert_eq!(answer.human[9], strings::DOCTOR_ASSETS_LABEL);
        assert_eq!(
            answer.human[10],
            "  noVNC: installed at /home/agent/.local/share/daiorama/default/novnc"
        );
        assert_eq!(
            answer.payload["tools"]["hyprland"]["supported"],
            json!(true)
        );
        assert_eq!(
            answer.payload["containment"],
            json!({"ok": true, "detail": "delegated: cpu memory pids already enabled"})
        );
        assert_eq!(answer.payload["assets"]["installed"], json!(true));
        assert_eq!(answer.payload["assets"]["install"], Value::Null);
    }

    #[test]
    fn report_names_the_staging_step_when_assets_are_missing() {
        let mut facts = base_facts();
        facts.assets.installed = false;
        let answer = render(&facts);
        let lines = &answer.human;
        assert_eq!(lines[9], strings::DOCTOR_ASSETS_LABEL);
        // The front's refusal verbatim: the operator sees the same string in the 503.
        assert_eq!(lines[10], format!("  {}", strings::VIEW_ASSETS_MISSING));
        assert_eq!(
            lines[11],
            "  install: packaging/novnc/fetch.sh \
             /home/agent/.local/share/daiorama/default/novnc"
        );
        assert_eq!(answer.payload["assets"]["installed"], json!(false));
        assert_eq!(
            answer.payload["assets"]["install"],
            json!(
                "install: packaging/novnc/fetch.sh \
                 /home/agent/.local/share/daiorama/default/novnc"
            )
        );
    }

    #[test]
    fn report_renders_missing_and_out_of_range() {
        let mut facts = base_facts();
        facts.hyprland = None;
        facts.wayvnc = None;
        facts.os = OsFacts {
            id: "fedora".to_string(),
        };
        let answer = render(&facts);
        assert_eq!(
            answer.human[2],
            "  Hyprland not found. Install it: not packaged for this distribution; see the \
             release notes for a COPR/upstream build, or use the container image."
        );
        assert_eq!(
            answer.human[3],
            "  wayvnc: missing — install it: sudo dnf install wayvnc"
        );
        assert_eq!(
            answer.payload["tools"]["hyprland"]["install"],
            json!(strings::HYPRLAND_NOT_PACKAGED)
        );

        let mut facts = base_facts();
        facts.hyprland = tool("/opt/Hyprland", Some("0.99.9"));
        let answer = render(&facts);
        assert_eq!(
            answer.human[2],
            format!(
                "  {}",
                strings::hyprland_unsupported("0.99.9", "0.56.0", "0.56.9")
            )
        );
        assert_eq!(
            answer.payload["tools"]["hyprland"]["supported"],
            json!(false)
        );

        let mut facts = base_facts();
        facts.hyprland = tool("/opt/Hyprland", None);
        let answer = render(&facts);
        assert_eq!(
            answer.human[2],
            "  hyprland: version unknown (/opt/Hyprland)"
        );
        assert_eq!(
            answer.payload["tools"]["hyprland"]["supported"],
            Value::Null
        );
    }

    #[test]
    fn report_renders_wayland_and_containment_edge_shapes() {
        let mut facts = base_facts();
        facts.wayland_display = None;
        facts.runtime_dir = RuntimeDirFacts::Unset;
        facts.containment = Err("no cgroup v2 hierarchy at /sys/fs/cgroup".to_string());
        let answer = render(&facts);
        assert_eq!(
            answer.human[5],
            "  WAYLAND_DISPLAY: unset (nested backend unavailable; headless unaffected)"
        );
        assert_eq!(answer.human[6], "  XDG_RUNTIME_DIR: unset");
        assert_eq!(
            answer.human[8],
            format!(
                "  {}",
                strings::containment_unavailable("no cgroup v2 hierarchy at /sys/fs/cgroup")
            )
        );
        assert_eq!(answer.payload["containment"]["ok"], json!(false));

        let mut facts = base_facts();
        facts.runtime_dir = RuntimeDirFacts::Present {
            path: PathBuf::from("/tmp/run"),
            writable: false,
        };
        let answer = render(&facts);
        assert_eq!(
            answer.human[6],
            "  XDG_RUNTIME_DIR: /tmp/run (not writable)"
        );
    }
}
