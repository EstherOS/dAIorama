//! Session capture (05 §Capture): run one configured argv command against a session's own
//! Wayland environment, bounded in time and size, and hand back what it wrote.
//!
//! The command is configuration, not code: it is an argv array (never a shell string), the
//! manager appends the output path and interpolates nothing else, and a fixture script can
//! stand in for grim/wf-recorder in every test — no compositor is needed to exercise the
//! whole slice.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::ipc::ErrorEnvelope;
use crate::manager::CaptureEnv;

/// Build a throwaway output path inside the session's runtime dir (0700, compositor-owned).
fn scratch_path(env: &CaptureEnv, extension: &str) -> PathBuf {
    env.rt_dir
        .join(format!("capture-{}", crate::config::generate_token()))
        .with_extension(extension)
}

/// Run `argv` with `out` appended, bounded by `budget`. `stop_after` sends SIGINT after that
/// long (a recorder finalizes its container) instead of expecting a natural exit.
fn run(
    env: &CaptureEnv,
    argv: &[String],
    out: &Path,
    budget: Duration,
    stop_after: Option<Duration>,
) -> Result<(), ErrorEnvelope> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| ErrorEnvelope::capture_failed("capture failed: no command configured"))?;
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(program)
        .args(args)
        .arg(out)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_RUNTIME_DIR", &env.rt_dir)
        .env("WAYLAND_DISPLAY", &env.wayland_display)
        .env("HYPRLAND_INSTANCE_SIGNATURE", &env.signature)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group, so a SIGINT reaches the recorder **and** its children.
        .process_group(0)
        .spawn()
        .map_err(|err| {
            ErrorEnvelope::capture_failed(format!("capture failed: cannot start {program}: {err}"))
        })?;

    let started = Instant::now();
    let deadline = started + budget;
    let mut signalled = false;
    // SIGINT is a finalize request, not a start pistol: a recorder that has not written its
    // first byte yet (slow exec/scheduler under load) must get that byte out first, or the
    // finalize produces an empty container. The grace keeps the stop bound honest for a
    // recorder that never writes at all.
    const FIRST_BYTE_GRACE: Duration = Duration::from_secs(1);
    loop {
        if let Some(stop_after) = stop_after
            && !signalled
            && started.elapsed() >= stop_after
            && (out_has_bytes(out) || started.elapsed() >= stop_after + FIRST_BYTE_GRACE)
        {
            // SIGINT lets a recorder flush its container; the exit status afterwards is not
            // the capture's verdict (the bytes are).
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGINT);
            }
            signalled = true;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !signalled && !status.success() {
                    let detail = status
                        .code()
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "signal".to_string());
                    return Err(ErrorEnvelope::capture_failed(format!(
                        "capture failed: {program} exited {detail}"
                    )));
                }
                return Ok(());
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ErrorEnvelope::capture_failed(format!(
                        "capture failed: {program} timed out after {}s",
                        budget.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => {
                return Err(ErrorEnvelope::capture_failed(format!(
                    "capture failed: cannot wait for {program}: {err}"
                )));
            }
        }
    }
}

/// Read the capture file with a hard size bound; an absent or empty file is "no output".
fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ErrorEnvelope> {
    let metadata = std::fs::metadata(path)
        .map_err(|_| ErrorEnvelope::capture_failed("capture failed: no output"))?;
    if metadata.len() == 0 {
        return Err(ErrorEnvelope::capture_failed("capture failed: no output"));
    }
    if metadata.len() > max_bytes {
        return Err(ErrorEnvelope::bad_request(format!(
            "capture too large: {} bytes (max {})",
            metadata.len(),
            max_bytes
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::fs::File::open(path)
        .and_then(|file| file.take(max_bytes + 1).read_to_end(&mut bytes).map(|_| ()))
        .map_err(|err| ErrorEnvelope::capture_failed(format!("capture failed: {err}")))?;
    if bytes.len() as u64 > max_bytes {
        return Err(ErrorEnvelope::bad_request(format!(
            "capture too large: {} bytes (max {})",
            bytes.len(),
            max_bytes
        )));
    }
    Ok(bytes)
}

/// PNG dimensions from the IHDR chunk (no image crate: 8-byte signature, then the fixed chunk).
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// One screenshot: PNG bytes plus their dimensions.
pub fn screenshot(
    env: &CaptureEnv,
    argv: &[String],
    timeout: Duration,
    max_bytes: u64,
) -> Result<(Vec<u8>, u32, u32), ErrorEnvelope> {
    let out = scratch_path(env, "png");
    let result = run(env, argv, &out, timeout, None);
    if let Err(error) = result {
        let _ = std::fs::remove_file(&out);
        return Err(error);
    }
    let bytes = read_bounded(&out, max_bytes);
    let _ = std::fs::remove_file(&out);
    let bytes = bytes?;
    let (width, height) = png_dimensions(&bytes)
        .ok_or_else(|| ErrorEnvelope::capture_failed("capture failed: not a PNG"))?;
    Ok((bytes, width, height))
}

fn out_has_bytes(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
}

/// One clip: the recorder runs for `duration`, then SIGINT finalizes the container.
pub fn clip(
    env: &CaptureEnv,
    argv: &[String],
    timeout: Duration,
    duration: Duration,
    max_bytes: u64,
) -> Result<Vec<u8>, ErrorEnvelope> {
    let out = scratch_path(env, "webm");
    // The finalize window is the same configured bound, on top of the recording itself.
    let budget = duration + timeout;
    let result = run(env, argv, &out, budget, Some(duration));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&out);
        return Err(error);
    }
    let bytes = read_bounded(&out, max_bytes);
    let _ = std::fs::remove_file(&out);
    bytes
}

/// Standard base64 with padding — the encoding an MCP `image` block carries.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((triple[0] as u32) << 16) | ((triple[1] as u32) << 8) | triple[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(dir: &Path) -> CaptureEnv {
        CaptureEnv {
            rt_dir: dir.to_path_buf(),
            wayland_display: "wayland-1".to_string(),
            signature: "sig".to_string(),
        }
    }

    /// A 1×1 PNG (the smallest valid file with an IHDR header).
    const TINY_PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00,
    ];

    /// A configured capture argv that runs `body` under `/bin/sh`, with the output path as
    /// `$1` (the runner appends it last). The body runs as `-c` text, never as a script file
    /// this process wrote: exec'ing a file written here races every other thread's fork (the
    /// child inherits the open write handle), which surfaces as `Text file busy (os error 26)`
    /// on a loaded machine. Nothing in these legs depends on the argv being a file.
    fn fixture(body: &str) -> Vec<String> {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            body.to_string(),
            "capture-fixture".to_string(),
        ]
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn screenshot_reads_a_png_and_its_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("tiny.png");
        std::fs::write(&out, TINY_PNG).unwrap();
        let script = fixture(&format!("cp {} \"$1\"", out.display()));
        let (bytes, width, height) =
            screenshot(&env(dir.path()), &script, Duration::from_secs(5), 1024).unwrap();
        assert_eq!(bytes, TINY_PNG);
        assert_eq!((width, height), (1, 1));
    }

    #[test]
    fn a_nonzero_exit_is_capture_failed() {
        let dir = tempfile::tempdir().unwrap();
        let script = fixture("exit 3");
        let error =
            screenshot(&env(dir.path()), &script, Duration::from_secs(5), 1024).unwrap_err();
        assert_eq!(error.code, crate::ipc::ErrorCode::CaptureFailed);
        assert!(error.message.contains("exited 3"), "{error:?}");
    }

    #[test]
    fn an_empty_file_is_no_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = fixture("touch \"$1\"");
        let error =
            screenshot(&env(dir.path()), &script, Duration::from_secs(5), 1024).unwrap_err();
        assert!(error.message.contains("no output"), "{error:?}");
    }

    #[test]
    fn a_big_file_is_refused_before_encoding() {
        let dir = tempfile::tempdir().unwrap();
        let script = fixture("head -c 2048 /dev/zero > \"$1\"");
        let error =
            screenshot(&env(dir.path()), &script, Duration::from_secs(5), 1000).unwrap_err();
        assert_eq!(error.code, crate::ipc::ErrorCode::BadRequest);
        assert!(error.message.starts_with("capture too large:"), "{error:?}");
    }

    #[test]
    fn a_slow_command_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let script = fixture("sleep 30");
        let error =
            screenshot(&env(dir.path()), &script, Duration::from_secs(1), 1024).unwrap_err();
        assert!(error.message.contains("timed out after 1s"), "{error:?}");
    }

    #[test]
    fn a_slow_starting_recorder_writes_its_first_byte_before_the_interrupt() {
        let dir = tempfile::tempdir().unwrap();
        // The write lands well after the nominal stop; the harness waits for the first byte
        // instead of racing the child's startup with SIGINT.
        let script = fixture("sleep 0.15; printf x > \"$1\"; sleep 30");
        let bytes = clip(
            &env(dir.path()),
            &script,
            Duration::from_secs(5),
            Duration::from_millis(50),
            1024,
        )
        .unwrap();
        assert_eq!(bytes, b"x");
    }

    #[test]
    fn a_clip_stops_at_the_duration_and_keeps_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        // Write a byte, then sleep: the SIGINT after 200 ms ends it; the file is the artifact.
        let script = fixture("printf x > \"$1\"; sleep 30");
        let bytes = clip(
            &env(dir.path()),
            &script,
            Duration::from_secs(5),
            Duration::from_millis(200),
            1024,
        )
        .unwrap();
        assert_eq!(bytes, b"x");
    }
}
