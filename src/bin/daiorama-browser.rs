//! `daiorama-browser` — the session launcher 06 D6 pins at `/usr/local/bin/daiorama-browser`
//! (desktop image only). It launches the real browser on a probe page, reads the renderer string
//! the browser reports through the compositor's window title (`hyprctl clients`, measured in
//! `p5/browser/evidence/channel-probe.txt` — Chromium's stderr carries nothing unmasked), writes
//! `<runtime>/browser-renderer` (one line, 0600, atomic) and then supervises the browser.
//!
//! No automation protocol, no headless mode, no CDP: the browser is the same one the session
//! shows. Capture failure is not fatal — no file, one warning, session stays `renderer: unverified`.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;

const TITLE_PREFIX: &str = "daiorama-renderer:";
const PROBE_FILENAME: &str = "browser-probe.html";
const RENDERER_FILENAME: &str = "browser-renderer";

#[derive(Debug, Parser)]
#[command(
    name = "daiorama-browser",
    about = "Launch the session browser and record its rendered-GL renderer (06 D6)"
)]
struct Args {
    /// Browser binary to launch.
    #[arg(long)]
    browser: Option<String>,
    /// Persisted browser profile directory.
    #[arg(long)]
    profile: Option<PathBuf>,
    /// Where the renderer line is written.
    #[arg(long)]
    renderer_file: Option<PathBuf>,
    /// Seconds to wait for the compositor to report the probe title.
    #[arg(long, default_value_t = 10)]
    capture_timeout: u64,
    /// Page to navigate to after the renderer is captured.
    url: String,
    /// Extra arguments passed to the browser verbatim (after `--`).
    #[arg(last = true)]
    browser_args: Vec<String>,
}

/// PID of the browser, for the signal handler. 0 means "nothing to forward".
static CHILD: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward_signal(signal: libc::c_int) {
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, signal);
        }
    }
}

fn main() {
    let args = Args::parse();
    match run(args) {
        Ok(code) => std::process::exit(code),
        Err((code, message)) => {
            eprintln!("daiorama-browser: {message}");
            std::process::exit(code);
        }
    }
}

fn run(args: Args) -> Result<i32, (i32, String)> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map_err(|_| (127, "XDG_RUNTIME_DIR is not set".to_string()))?;
    let browser = args
        .browser
        .or_else(|| std::env::var("DAIORAMA_BROWSER").ok())
        .unwrap_or_else(|| "chromium".to_string());
    let data_home = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share")
        });
    let profile = args.profile.unwrap_or_else(|| data_home.join("browser"));
    let renderer_file = args
        .renderer_file
        .unwrap_or_else(|| runtime.join(RENDERER_FILENAME));

    if let Err(err) = std::fs::create_dir_all(&profile) {
        return Err((
            127,
            format!("cannot create profile {}: {err}", profile.display()),
        ));
    }
    if let Err(err) = std::fs::create_dir_all(&runtime) {
        return Err((
            127,
            format!("cannot create runtime dir {}: {err}", runtime.display()),
        ));
    }

    let probe = runtime.join(PROBE_FILENAME);
    write_private(&probe, &probe_html(&args.url)).map_err(|err| (127, err))?;

    let mut command = Command::new(&browser);
    command
        .arg(format!("--app=file://{}", probe.display()))
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .args(&args.browser_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            // Parent death takes the browser with it (contract: no orphan).
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|err| {
        let _ = std::fs::remove_file(&probe);
        (127, format!("cannot launch {browser}: {err}"))
    })?;
    CHILD.store(child.id() as i32, Ordering::SeqCst);

    // Signal forwarding: SIGINT/SIGTERM go to the browser; the launcher keeps the child's status.
    unsafe {
        libc::signal(
            libc::SIGINT,
            forward_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            forward_signal as *const () as libc::sighandler_t,
        );
    }

    let capture = capture_renderer(&runtime, args.capture_timeout);
    match capture {
        Ok(Some(renderer)) => {
            let line = format!("renderer={renderer}\n");
            if let Err(err) = write_atomic_private(&renderer_file, &line) {
                eprintln!(
                    "daiorama-browser: cannot write {}: {err}",
                    renderer_file.display()
                );
            }
        }
        Ok(None) => {
            eprintln!(
                "daiorama-browser: no renderer title within {}s; the session stays renderer: unverified",
                args.capture_timeout
            );
        }
        Err(err) => {
            let _ = std::fs::remove_file(&probe);
            let _ = child.kill();
            let _ = child.wait();
            return Err((127, err));
        }
    }
    let _ = std::fs::remove_file(&probe);

    let status = child
        .wait()
        .map_err(|err| (127, format!("waiting for browser: {err}")))?;
    CHILD.store(0, Ordering::SeqCst);
    Ok(match status.code() {
        Some(code) => code,
        None => 128 + status.signal().unwrap_or(0),
    })
}

/// Poll the compositor's client list for the probe title. `Ok(None)` is a timeout (not an
/// error); `Err` means the compositor IPC itself is unavailable (contract: exit 127).
fn capture_renderer(runtime: &Path, timeout: u64) -> Result<Option<String>, String> {
    if !command_available("hyprctl") {
        return Err("hyprctl is not available; no compositor IPC".to_string());
    }
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        let output = Command::new("hyprctl")
            .arg("clients")
            .arg("-j")
            .env("XDG_RUNTIME_DIR", runtime)
            .output()
            .map_err(|err| format!("hyprctl: {err}"))?;
        if output.status.success()
            && let Some(renderer) = title_from_clients(&output.stdout)
        {
            return Ok(Some(renderer));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn command_available(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// First client title carrying the prefix yields the renderer string. `hyprctl clients -j`
/// returns a JSON array of objects with a `title` field.
pub fn title_from_clients(raw: &[u8]) -> Option<String> {
    let clients: serde_json::Value = serde_json::from_slice(raw).ok()?;
    for client in clients.as_array()? {
        let title = client.get("title")?.as_str()?;
        if let Some(rest) = title.strip_prefix(TITLE_PREFIX) {
            let renderer = rest.trim();
            if !renderer.is_empty() {
                return Some(renderer.to_string());
            }
        }
    }
    None
}

/// The probe holds the title for a bounded window (2 s) before navigating: the launcher's poll
/// must get a chance to read it, which is the race the acceptance leg measures.
pub fn probe_html(url: &str) -> String {
    let target = serde_json::to_string(url).expect("URL serializes");
    format!(
        r#"<!doctype html><meta charset="utf-8"><title>daiorama probe</title>
<body><script>
(function () {{
  var renderer = "unavailable";
  try {{
    var canvas = document.createElement("canvas");
    var gl = canvas.getContext("webgl");
    if (gl) {{
      var ext = gl.getExtension("WEBGL_debug_renderer_info");
      renderer = ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER);
    }}
  }} catch (e) {{ renderer = "error:" + e; }}
  document.title = "daiorama-renderer: " + renderer;
  setTimeout(function () {{ window.location.href = {target}; }}, 2000);
}})();
</script></body>"#
    )
}

fn write_private(path: &Path, body: &str) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    file.write_all(body.as_bytes())
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("cannot chmod {}: {err}", path.display()))?;
    Ok(())
}

/// Temp file + rename in the same directory, 0600: a reader never sees a partial line.
fn write_atomic_private(path: &Path, body: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "renderer file has no parent".to_string())?;
    let temp = dir.join(format!(".{}.tmp.{}", RENDERER_FILENAME, std::process::id()));
    write_private(&temp, body)?;
    std::fs::rename(&temp, path).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot rename {}: {err}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, mode: u32) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .unwrap();
        file.write_all(b"x").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn title_parser_takes_only_the_prefixed_title() {
        let raw = br#"[
            {"title":"something else"},
            {"title":"daiorama-renderer: ANGLE (Mesa, NV117, OpenGL ES 3.2)"},
            {"title":"daiorama-renderer: later"}
        ]"#;
        assert_eq!(
            title_from_clients(raw).as_deref(),
            Some("ANGLE (Mesa, NV117, OpenGL ES 3.2)")
        );
        assert!(title_from_clients(br#"{"not":"an array"}"#).is_none());
        assert!(title_from_clients(br#"[]"#).is_none());
    }

    #[test]
    fn probe_page_holds_the_title_before_navigating() {
        let html = probe_html("https://example.test/a?b=1");
        assert!(html.contains(TITLE_PREFIX));
        assert!(html.contains("setTimeout"));
        assert!(html.contains("2000"));
        assert!(html.contains("UNMASKED_RENDERER_WEBGL"));
        assert!(html.contains("\"https://example.test/a?b=1\""));
    }

    #[test]
    fn renderer_file_is_private_and_atomic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("browser-renderer");
        write_atomic_private(&path, "renderer=llvmpipe (LLVM 17.0, 256 bits)\n").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, "renderer=llvmpipe (LLVM 17.0, 256 bits)\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let entries: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1, "no temp file survives the rename");
    }

    #[test]
    fn private_write_is_0600_even_when_the_file_existed_wider() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("probe.html");
        write(&path, 0o644);
        write_private(&path, "body").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
