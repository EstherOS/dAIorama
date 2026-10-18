//! Views (06 B2, 07): one wayvnc per session plus the manager-owned front that serves the
//! noVNC page and proxies RFB-over-WebSocket, with the 07 credential as the only gate.
//!
//! The front runs on its own loopback port and speaks nothing but static assets and
//! `/websockify` — no MCP, no media, no second surface that could drift. Blocking threads on
//! purpose: dispatch is synchronous, the workload is a handful of browsers, and the relay gets
//! explicit bounds (32 connections, 10 s per write, 64 KiB chunks) instead of a runtime
//! dependency.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::crypto;
use crate::ipc::ErrorEnvelope;
use crate::manager::CaptureEnv;

/// Bounded concurrent front connections (07 / the in-process front pin).
pub const FRONT_MAX_CONNECTIONS: usize = 32;
/// Per-write bound on both relay directions: a browser that stops reading cannot wedge a task.
pub const FRONT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Strict, bounded request head — the same posture as the MCP listener.
pub const FRONT_HEAD_LIMIT: usize = 16 * 1024;
/// How long wayvnc gets to open its VNC socket before the start is refused.
pub const VIEW_READY_TIMEOUT: Duration = Duration::from_secs(5);
/// The asset tree under `<data>` (packaging ships it in P5).
pub const ASSETS_DIR: &str = "novnc";
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// 07's pin: `HMAC-SHA256(instance_token, "view:" || session_id || ":" || generation)`.
pub fn view_credential(instance_token: &str, session_id: &str, generation: u64) -> String {
    let message = format!("view:{session_id}:{generation}");
    crypto::hex(&crypto::hmac_sha256(
        instance_token.as_bytes(),
        message.as_bytes(),
    ))
}

/// The view's advertised base: `view.advertise_base` when set, else loopback.
pub fn view_base(advertise_base: Option<&str>, front_port: u16) -> String {
    advertise_base
        .map(|base| base.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("http://127.0.0.1:{front_port}"))
}

// ---------------------------------------------------------------------------
// Port allocation
// ---------------------------------------------------------------------------

/// First free loopback port at or above `base` (06: `front_port` allocates from
/// `view.port_base`, released on stop/death).
pub fn allocate_front_port(base: u16) -> Option<u16> {
    for offset in 0..200u16 {
        let port = base.saturating_add(offset);
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Some(port);
        }
    }
    None
}

/// An ephemeral loopback port for wayvnc (manager-known, not part of the address contract).
fn ephemeral_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.local_addr().map(|address| address.port())
}

// ---------------------------------------------------------------------------
// wayvnc
// ---------------------------------------------------------------------------

/// Spawn wayvnc against the session's Wayland env, with PDEATHSIG so manager death takes it.
/// The command is an argv array; the manager appends `127.0.0.1 <port>`.
pub fn spawn_wayvnc(
    session_id: &str,
    argv: &[String],
    env: &CaptureEnv,
    port: u16,
) -> Result<Child, ErrorEnvelope> {
    let (program, args) = argv.split_first().ok_or_else(|| {
        ErrorEnvelope::view_unavailable(session_id, "view.wayvnc_command is empty")
    })?;
    // wayvnc 0.10 has no port argv: the manager writes a per-view config and appends `-C`.
    let config_path = env.rt_dir.join(format!("wayvnc-{port}.conf"));
    // 0600 like every other instance file: no secret in it today, but a future wayvnc
    // option may add one, and uniform modes age better than a surprise.
    let write_conf = (|| {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&config_path)?;
        file.write_all(format!("address=127.0.0.1\nport={port}\n").as_bytes())
    })();
    if let Err(err) = write_conf {
        return Err(ErrorEnvelope::view_unavailable(
            session_id,
            &format!("view config: {err}"),
        ));
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .arg("-C")
        .arg(&config_path)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_RUNTIME_DIR", &env.rt_dir)
        .env("WAYLAND_DISPLAY", &env.wayland_display)
        .env("HYPRLAND_INSTANCE_SIGNATURE", &env.signature)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            // 03: parent-death guarantee for the view process.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != libc::getpid() && libc::getppid() == 1 {
                // Reparented before pre_exec: the manager is already gone.
                libc::_exit(101);
            }
            Ok(())
        });
    }
    command.spawn().map_err(|err| {
        ErrorEnvelope::view_unavailable(
            session_id,
            &format!("view process failed to start: {program}: {err}"),
        )
    })
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().expect("loopback addr"),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

// ---------------------------------------------------------------------------
// The front
// ---------------------------------------------------------------------------

struct FrontState {
    stop: AtomicBool,
    connections: AtomicUsize,
}

/// The manager-owned front for one view: a loopback HTTP server that serves the noVNC tree
/// and relays `/websockify` to wayvnc, gating every request on the 07 credential.
pub struct Front {
    pub front_port: u16,
    pub vnc_port: u16,
    state: Arc<FrontState>,
}

impl Front {
    pub fn start(
        front_port: u16,
        vnc_port: u16,
        credential: String,
        assets: PathBuf,
        advertise_base: Option<&str>,
    ) -> std::io::Result<Front> {
        let listener = TcpListener::bind(("127.0.0.1", front_port))?;
        Self::start_with_listener(
            listener,
            vnc_port,
            credential,
            assets,
            advertise_base.map(str::to_string),
        )
    }

    /// The Origin authorities the upgrade accepts: loopback always, plus
    /// `view.advertise_base`'s authority when it is set (a proxied browser sends that one).
    pub fn allowed_origins(front_port: u16, advertise_base: Option<&str>) -> Vec<String> {
        let mut origins = vec![
            format!("http://127.0.0.1:{front_port}"),
            format!("http://localhost:{front_port}"),
            format!("http://[::1]:{front_port}"),
        ];
        if let Some(base) = advertise_base {
            origins.push(base.trim_end_matches('/').to_string());
        }
        origins
    }

    /// Test-only: bind an ephemeral port so parallel tests never race on a fixed base.
    #[cfg(test)]
    pub fn start_ephemeral(
        vnc_port: u16,
        credential: String,
        assets: PathBuf,
    ) -> std::io::Result<Front> {
        Self::start_ephemeral_with_advertise(vnc_port, credential, assets, None)
    }

    #[cfg(test)]
    pub fn start_ephemeral_with_advertise(
        vnc_port: u16,
        credential: String,
        assets: PathBuf,
        advertise_base: Option<&str>,
    ) -> std::io::Result<Front> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        Self::start_with_listener(
            listener,
            vnc_port,
            credential,
            assets,
            advertise_base.map(str::to_string),
        )
    }

    fn start_with_listener(
        listener: TcpListener,
        vnc_port: u16,
        credential: String,
        assets: PathBuf,
        advertise_base: Option<String>,
    ) -> std::io::Result<Front> {
        let front_port = listener.local_addr()?.port();
        let origins = Self::allowed_origins(front_port, advertise_base.as_deref());
        listener.set_nonblocking(true)?;
        let state = Arc::new(FrontState {
            stop: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
        });
        let state_for_thread = Arc::clone(&state);
        thread::spawn(move || {
            while !state_for_thread.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let state = Arc::clone(&state_for_thread);
                        if state.connections.fetch_add(1, Ordering::SeqCst) >= FRONT_MAX_CONNECTIONS
                        {
                            state.connections.fetch_sub(1, Ordering::SeqCst);
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                            continue;
                        }
                        let credential = credential.clone();
                        let assets = assets.clone();
                        let origins = origins.clone();
                        thread::spawn(move || {
                            let _ =
                                handle_connection(stream, vnc_port, &credential, &assets, &origins);
                            state.connections.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Front {
            front_port,
            vnc_port,
            state,
        })
    }

    pub fn stop(&self) {
        self.state.stop.store(true, Ordering::SeqCst);
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: (u16, &str),
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", status.0, status.1);
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\nReferrer-Policy: no-referrer\r\n");
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.set_write_timeout(Some(FRONT_WRITE_TIMEOUT))?;
    stream.write_all(head.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body)?;
    }
    stream.flush()
}

/// The pinned `view_unavailable` body for a missing asset tree (the message is exactly the
/// 05 pin — no session id in this path).
fn assets_missing() -> Vec<u8> {
    let envelope = ErrorEnvelope {
        code: crate::ipc::ErrorCode::ViewUnavailable,
        message: crate::strings::VIEW_ASSETS_MISSING.to_string(),
        hint: None,
    };
    serde_json::to_vec(&envelope).expect("envelope serializes")
}

type Headers = Vec<(String, String)>;

fn parse_head(raw: &[u8]) -> Option<(String, String, Headers)> {
    let text = std::str::from_utf8(raw).ok()?;
    let mut lines = text.split("\r\n");
    let request = lines.next()?;
    let mut parts = request.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let version = parts.next()?;
    if !version.starts_with("HTTP/1.") {
        return None;
    }
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        if name.trim().is_empty()
            || name.ends_with(' ')
            || value.starts_with(' ') && value.is_empty()
        {
            return None;
        }
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    Some((method, target, headers))
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn read_head(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            return Some(buffer);
        }
        if buffer.len() > FRONT_HEAD_LIMIT {
            return None;
        }
    }
}

/// One front request: gate, then serve static or relay a WebSocket.
fn handle_connection(
    mut stream: TcpStream,
    vnc_port: u16,
    credential: &str,
    assets: &Path,
    allowed_origins: &[String],
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let Some(raw) = read_head(&mut stream) else {
        let body = b"malformed request\n";
        return write_response(&mut stream, (400, "Bad Request"), &[], body);
    };
    let Some((method, target, headers)) = parse_head(&raw) else {
        let body = b"malformed request\n";
        return write_response(&mut stream, (400, "Bad Request"), &[], body);
    };
    if method != "GET" {
        return write_response(
            &mut stream,
            (405, "Method Not Allowed"),
            &[("Allow", "GET".to_string())],
            b"GET only\n",
        );
    }
    // Strict head, same posture as the MCP listener: exactly one Host, no chunked framing.
    let hosts: Vec<&str> = headers
        .iter()
        .filter(|(key, _)| key == "host")
        .map(|(_, value)| value.as_str())
        .collect();
    if hosts.len() != 1 || hosts[0].is_empty() {
        return write_response(&mut stream, (400, "Bad Request"), &[], b"host required\n");
    }
    if header(&headers, "transfer-encoding").is_some() {
        return write_response(&mut stream, (400, "Bad Request"), &[], b"chunked refused\n");
    }

    // The 07 gate: query parameter (noVNC bootstrap), header, or cookie — one verifier.
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let query_credential = query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "credential").then_some(value)
    });
    let cookie_credential = header(&headers, "cookie").and_then(|cookies| {
        cookies.split(';').find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == "daiorama_view").then_some(value)
        })
    });
    let presented = query_credential
        .or_else(|| header(&headers, "x-daiorama-view"))
        .or(cookie_credential);
    let authorized = presented
        .map(|value| crypto::eq_constant_time(value.as_bytes(), credential.as_bytes()))
        .unwrap_or(false);
    if !authorized {
        return write_response(&mut stream, (403, "Forbidden"), &[], b"forbidden\n");
    }

    if path == "/websockify" {
        // Origin is required on the upgrade: browsers always send it; a script without one
        // uses a credential header instead (07, amended).
        let origin = header(&headers, "origin").unwrap_or_default();
        if !allowed_origins.iter().any(|allowed| allowed == origin) {
            return write_response(&mut stream, (403, "Forbidden"), &[], b"origin refused\n");
        }
        let Some(key) = header(&headers, "sec-websocket-key") else {
            return write_response(&mut stream, (400, "Bad Request"), &[], b"key required\n");
        };
        if !header(&headers, "upgrade")
            .map(|value| value.eq_ignore_ascii_case("websocket"))
            .unwrap_or(false)
        {
            return write_response(
                &mut stream,
                (400, "Bad Request"),
                &[],
                b"upgrade required\n",
            );
        }
        return upgrade_and_relay(stream, key, vnc_port);
    }

    // Static: only inside the asset tree, no traversal, no encoded tricks.
    if path.contains("..") || path.starts_with("//") || path.contains('%') || path.contains('\\') {
        return write_response(&mut stream, (404, "Not Found"), &[], b"not found\n");
    }
    let relative = if path == "/" || path.is_empty() {
        "vnc.html"
    } else {
        path.trim_start_matches('/')
    };
    let mut file = assets.join(relative);
    if !file.exists() && relative == "vnc.html" {
        file = assets.join("index.html");
    }
    let body = match std::fs::read(&file) {
        Ok(body) => body,
        Err(_) => {
            let body = assets_missing();
            let mut headers = vec![("Content-Type", "application/json".to_string())];
            if relative == "vnc.html" || relative == "index.html" {
                // The credential passed the gate on this request: bind the cookie here only.
                headers.push(("Set-Cookie", cookie_header(credential)));
            }
            return write_response(&mut stream, (503, "Service Unavailable"), &headers, &body);
        }
    };
    let content_type = content_type_for(relative);
    let mut headers = vec![
        ("Content-Type", content_type.to_string()),
        ("Cache-Control", "no-store".to_string()),
    ];
    if relative == "vnc.html" || relative == "index.html" {
        headers.push(("Set-Cookie", cookie_header(credential)));
    }
    write_response(&mut stream, (200, "OK"), &headers, &body)
}

fn cookie_header(credential: &str) -> String {
    format!("daiorama_view={credential}; HttpOnly; SameSite=Strict; Path=/; Max-Age=3600")
}

fn content_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn upgrade_and_relay(mut stream: TcpStream, key: &str, vnc_port: u16) -> std::io::Result<()> {
    let mut accept_material = Vec::new();
    accept_material.extend_from_slice(key.as_bytes());
    accept_material.extend_from_slice(WS_GUID.as_bytes());
    let accept = crypto::base64(&crypto::sha1(&accept_material));
    let mut head = String::new();
    head.push_str("HTTP/1.1 101 Switching Protocols\r\n");
    head.push_str("Upgrade: websocket\r\nConnection: Upgrade\r\n");
    head.push_str(&format!("Sec-WebSocket-Accept: {accept}\r\n\r\n"));
    stream.set_write_timeout(Some(FRONT_WRITE_TIMEOUT))?;
    stream.write_all(head.as_bytes())?;
    stream.flush()?;

    let backend = TcpStream::connect_timeout(
        &format!("127.0.0.1:{vnc_port}")
            .parse()
            .expect("loopback addr"),
        Duration::from_secs(2),
    )?;
    backend.set_nodelay(true).ok();
    stream.set_nodelay(true).ok();
    let mut client_read = stream.try_clone()?;
    let mut client_write = stream;
    let mut down_write = client_write.try_clone()?;
    let mut backend_write = backend.try_clone()?;
    let mut backend_read = backend;

    // vnc -> browser: raw bytes wrapped in unmasked binary frames.
    let downlink = thread::spawn(move || {
        let mut buffer = [0u8; 64 * 1024];
        loop {
            match backend_read.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if write_frame(&mut down_write, 0x2, &buffer[..read]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = down_write.shutdown(std::net::Shutdown::Both);
    });

    // browser -> vnc: unmask frames; ping answers pong; close ends the relay.
    let mut header = [0u8; 2];
    loop {
        if client_read.read_exact(&mut header).is_err() {
            break;
        }
        let fin = header[0] & 0x80 != 0;
        let opcode = header[0] & 0x0f;
        let masked = header[1] & 0x80 != 0;
        let mut length = (header[1] & 0x7f) as u64;
        if length == 126 {
            let mut extended = [0u8; 2];
            if client_read.read_exact(&mut extended).is_err() {
                break;
            }
            length = u16::from_be_bytes(extended) as u64;
        } else if length == 127 {
            let mut extended = [0u8; 8];
            if client_read.read_exact(&mut extended).is_err() {
                break;
            }
            length = u64::from_be_bytes(extended);
        }
        if length > 4 * 1024 * 1024 {
            break;
        }
        let mut mask = [0u8; 4];
        if masked && client_read.read_exact(&mut mask).is_err() {
            break;
        }
        let mut payload = vec![0u8; length as usize];
        if client_read.read_exact(&mut payload).is_err() {
            break;
        }
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        match opcode {
            0x8 => break, // close
            0x9 => {
                if write_frame(&mut client_write, 0xA, &payload).is_err() {
                    break;
                }
            }
            0x2 | 0x0 if backend_write.write_all(&payload).is_err() || !fin => break,
            0x2 | 0x0 => {}
            _ => {} // text/pong: not part of this relay
        }
    }
    let _ = backend_write.shutdown(std::net::Shutdown::Both);
    let _ = client_read.shutdown(std::net::Shutdown::Both);
    let _ = downlink.join();
    Ok(())
}

fn write_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | opcode);
    let len = payload.len();
    if len < 126 {
        frame.push(len as u8);
    } else if len <= u16::MAX as usize {
        frame.push(126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.set_write_timeout(Some(FRONT_WRITE_TIMEOUT))?;
    stream.write_all(&frame)?;
    stream.flush()
}

// ---------------------------------------------------------------------------
// View manager (per-instance)
// ---------------------------------------------------------------------------

struct RunningView {
    front: Front,
    child: Child,
    generation: u64,
}

/// All live views of one manager. Starts are serialized per session by the registry lock in
/// `RuntimeService` (one start path, 06).
#[derive(Default)]
pub struct ViewManager {
    views: Mutex<HashMap<String, RunningView>>,
}

/// The process/asset half of a view start (kept apart so `start` takes one launch value).
pub struct ViewLaunch<'a> {
    pub argv: &'a [String],
    pub port_base: u16,
    pub assets: &'a Path,
    /// `view.advertise_base`: display-only for the address, but its authority is accepted as
    /// an upgrade `Origin` (a browser behind a reverse proxy sends it).
    pub advertise_base: Option<&'a str>,
}

/// What `get_view` needs to build its answer.
pub struct StartedView {
    pub front_port: u16,
    pub vnc_port: u16,
    pub generation: u64,
}

impl ViewManager {
    /// The live view for a session, if any. A view whose wayvnc has exited is **not** live:
    /// 06 says view death marks the view `down`, leaves the session running, and the next
    /// `get_view` starts a fresh one with a new generation — so an exited child is reaped here
    /// (front stopped, entry dropped) rather than handed back as a working address. Without
    /// this, a crashed wayvnc leaves the front serving the page and `get_view` answering the
    /// same generation forever; the operator's reload cannot recover without a manual stop.
    pub fn running(&self, session_id: &str) -> Option<(u16, u16, u64)> {
        let mut views = self.views.lock().expect("view lock");
        let exit = views
            .get_mut(session_id)
            // `try_wait` errors only when the child cannot be interrogated; keep the view then.
            .map(|view| matches!(view.child.try_wait(), Ok(Some(_))))?;
        if exit {
            if let Some(view) = views.remove(session_id) {
                view.front.stop();
            }
            return None;
        }
        views
            .get(session_id)
            .map(|view| (view.front.front_port, view.front.vnc_port, view.generation))
    }

    /// Start a view: allocate ports, spawn wayvnc, wait for its socket, start the front.
    pub fn start(
        &self,
        session_id: &str,
        generation: u64,
        credential: &str,
        env: &CaptureEnv,
        launch: ViewLaunch<'_>,
    ) -> Result<StartedView, ErrorEnvelope> {
        let mut views = self.views.lock().expect("view lock");
        if let Some(existing) = views.get(session_id) {
            return Ok(StartedView {
                front_port: existing.front.front_port,
                vnc_port: existing.front.vnc_port,
                generation: existing.generation,
            });
        }
        let front_port = allocate_front_port(launch.port_base)
            .ok_or_else(|| ErrorEnvelope::backend_busy(session_id))?;
        let vnc_port = ephemeral_port().map_err(|err| {
            ErrorEnvelope::view_unavailable(session_id, &format!("view port: {err}"))
        })?;
        let mut child = spawn_wayvnc(session_id, launch.argv, env, vnc_port)?;
        if !wait_for_port(vnc_port, VIEW_READY_TIMEOUT) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ErrorEnvelope::view_unavailable(
                session_id,
                &format!(
                    "view process failed to start: {} did not open 127.0.0.1:{vnc_port}",
                    launch.argv.first().map(String::as_str).unwrap_or("wayvnc")
                ),
            ));
        }
        let front = Front::start(
            front_port,
            vnc_port,
            credential.to_string(),
            launch.assets.to_path_buf(),
            launch.advertise_base,
        )
        .map_err(|err| {
            let _ = child.kill();
            ErrorEnvelope::view_unavailable(
                session_id,
                &format!("view front failed to start: {err}"),
            )
        })?;
        views.insert(
            session_id.to_string(),
            RunningView {
                front,
                child,
                generation,
            },
        );
        Ok(StartedView {
            front_port,
            vnc_port,
            generation,
        })
    }

    /// Stop a view: front first, then wayvnc. Absent is fine (idempotent stop).
    pub fn stop(&self, session_id: &str) -> Option<u16> {
        let mut views = self.views.lock().expect("view lock");
        let mut view = views.remove(session_id)?;
        view.front.stop();
        let _ = view.child.kill();
        let _ = view.child.wait();
        Some(view.front.front_port)
    }

    pub fn shutdown_all(&self) {
        let mut views = self.views.lock().expect("view lock");
        for (_, mut view) in views.drain() {
            view.front.stop();
            let _ = view.child.kill();
            let _ = view.child.wait();
        }
    }
}

impl Drop for ViewManager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential() -> String {
        view_credential("instance-token", "session-1", 7)
    }

    /// A fake wayvnc: a loopback echo server that answers with the RFB banner first.
    fn fake_backend() -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let Ok(mut stream) = stream else { continue };
                thread::spawn(move || {
                    let _ = stream.write_all(b"RFB 003.008\n");
                    let mut buffer = [0u8; 1024];
                    while let Ok(read) = stream.read(&mut buffer) {
                        if read == 0 {
                            break;
                        }
                        let _ = stream.write_all(&buffer[..read]);
                    }
                });
            }
        });
        (port, handle)
    }

    fn start_front() -> (Front, u16) {
        let (backend_port, _handle) = fake_backend();
        let assets = std::env::temp_dir();
        let front = Front::start_ephemeral(backend_port, credential(), assets).unwrap();
        let port = front.front_port;
        (front, port)
    }

    fn request(port: u16, raw: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        String::from_utf8_lossy(&response).to_string()
    }

    #[test]
    fn the_gate_refuses_without_and_with_a_wrong_credential() {
        let (front, port) = start_front();
        let missing = request(port, "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        assert!(missing.starts_with("HTTP/1.1 403"), "{missing}");
        let wrong = request(
            port,
            "GET /?credential=deadbeef HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        assert!(wrong.starts_with("HTTP/1.1 403"), "{wrong}");
        front.stop();
    }

    #[test]
    fn a_valid_credential_serves_a_page_and_sets_the_cookie() {
        let (front, port) = start_front();
        // No vnc.html in the asset tree -> 503 view_unavailable, but the gate passed and the
        // cookie is bound on this response.
        let response = request(
            port,
            &format!(
                "GET /?credential={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
                credential()
            ),
        );
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
        assert!(
            response.contains("Set-Cookie: daiorama_view="),
            "{response}"
        );
        assert!(response.contains("HttpOnly"), "{response}");
        assert!(
            response.contains("Referrer-Policy: no-referrer"),
            "{response}"
        );
        front.stop();
    }

    #[test]
    fn traversal_and_encoded_paths_are_refused() {
        let (front, port) = start_front();
        for path in ["/../token", "/%2e%2e/token", "/a/../../b"] {
            let response = request(
                port,
                &format!(
                    "GET {path}?credential={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
                    credential()
                ),
            );
            assert!(response.starts_with("HTTP/1.1 404"), "{path}: {response}");
        }
        front.stop();
    }

    #[test]
    fn the_websocket_upgrade_requires_origin_and_relays_rfb() {
        let (front, port) = start_front();
        let no_origin = request(
            port,
            &format!(
                "GET /websockify?credential={} HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
                credential()
            ),
        );
        assert!(no_origin.starts_with("HTTP/1.1 403"), "{no_origin}");

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let handshake = format!(
            "GET /websockify?credential={} HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
            credential()
        );
        stream.write_all(handshake.as_bytes()).unwrap();
        let mut head = [0u8; 1024];
        let read = stream.read(&mut head).unwrap();
        let head = String::from_utf8_lossy(&head[..read]).to_string();
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        assert!(
            head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            "{head}"
        );
        // First server frame is the RFB banner.
        let mut banner = [0u8; 14];
        stream.read_exact(&mut banner).unwrap();
        assert_eq!(&banner, b"\x82\x0cRFB 003.008\n");
        // A real client masks its payload: "hello" XOR the mask, echoed back unmasked.
        let mask = [1u8, 2, 3, 4];
        let mut frame = vec![0x82, 0x80 | 5];
        frame.extend_from_slice(&mask);
        frame.extend(
            b"hello"
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        stream.write_all(&frame).unwrap();
        let mut echo = [0u8; 7];
        stream.read_exact(&mut echo).unwrap();
        assert_eq!(&echo, b"\x82\x05hello");
        let _ = stream.shutdown(std::net::Shutdown::Both);
        front.stop();
    }

    #[test]
    fn an_advertise_base_authority_is_accepted_as_origin() {
        let (backend_port, _handle) = fake_backend();
        let front = Front::start_ephemeral_with_advertise(
            backend_port,
            credential(),
            std::env::temp_dir(),
            Some("http://proxy.example:8080"),
        )
        .unwrap();
        let port = front.front_port;
        assert!(
            Front::allowed_origins(port, Some("http://proxy.example:8080/"))
                .contains(&"http://proxy.example:8080".to_string())
        );
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let handshake = format!(
            "GET /websockify?credential={} HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://proxy.example:8080\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
            credential()
        );
        stream.write_all(handshake.as_bytes()).unwrap();
        let mut head = [0u8; 512];
        let read = stream.read(&mut head).unwrap();
        let head = String::from_utf8_lossy(&head[..read]).to_string();
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        let _ = stream.shutdown(std::net::Shutdown::Both);
        front.stop();
    }

    #[test]
    fn the_wayvnc_conf_is_written_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let env = CaptureEnv {
            rt_dir: dir.path().to_path_buf(),
            wayland_display: "wayland-1".to_string(),
            signature: "sig".to_string(),
        };
        let mut child = spawn_wayvnc("s", &["/bin/true".to_string()], &env, 5999).unwrap();
        let _ = child.wait();
        let conf = dir.path().join("wayvnc-5999.conf");
        let mode = std::fs::metadata(&conf).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "conf mode");
        assert!(
            std::fs::read_to_string(&conf)
                .unwrap()
                .contains("port=5999")
        );
    }

    #[test]
    fn view_port_allocation_moves_up_from_the_base() {
        let first = allocate_front_port(17080).unwrap();
        let held = TcpListener::bind(("127.0.0.1", first)).unwrap();
        assert!(allocate_front_port(first).unwrap() > first);
        drop(held);
    }
}
