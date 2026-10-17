//! `daiorama mcp --listen` — the network bridge (P3 W1b, 05 §The bridge, 07 §Scope model).
//!
//! One long-lived bridge, many connections, MCP over HTTP on one endpoint (`POST /mcp`). The
//! bridge is stateless with respect to sessions and scope: every request authenticates itself
//! with the pinned headers — `Daiorama-Token` (the instance token, the transport gate),
//! `Daiorama-Scope`, `Daiorama-Scope-Token` (the application-layer identity). No anonymous
//! calls, no default scope. The bridge is a presentation relay, not a token authority: it
//! checks the credentials it was given, then relays the call with the scope on the frame for
//! the manager to enforce (05, 07).
//!
//! Failures are MCP-level errors — a `{code, message, hint?}` envelope in the JSON-RPC error's
//! `data` — never an HTTP OAuth challenge or discovery metadata (05 §The bridge).
//!
//! HTTP is deliberately small: `POST /mcp`, `Content-Length` bodies, keep-alive, `Host`
//! required, and an `Origin` (when present) must name the same authority as `Host` (07's
//! browser-validation rule). Bodies are bounded exactly like a stdio frame.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};

use crate::config::{self, Paths, ScopeFile};
use crate::ipc::{Address, Client, ErrorCode, ErrorEnvelope, MAX_FRAME_BYTES};
use crate::mcp::server::{Bridge, json_rpc_envelope_error, json_rpc_error};
use crate::strings;

/// HTTP request heads are small; an over-cap head is a malformed client, not a payload.
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// Every socket read must make progress within this bound — head lines, body chunks, and the
/// idle wait for the next request on a kept-alive connection. It is a progress timeout, not a
/// total-body deadline: an 8 MiB body stays legal as long as bytes keep flowing. Five times
/// the manager connect timeout, and far above any real pause; an idle keep-alive close is
/// invisible to HTTP clients, they reconnect.
const PROGRESS_TIMEOUT: Duration = Duration::from_secs(10);
/// Once the request line has arrived, the rest of the head must arrive within this window.
/// A per-line bound alone lets a client that writes one short header line every few seconds
/// hold a connection slot indefinitely: every line makes progress, so `PROGRESS_TIMEOUT` never
/// fires. A head is at most `MAX_HEAD_BYTES` and a real client writes it in one go; 15 s is
/// 1.5× the progress bound and far above any real pause. The clock starts after the request
/// line (itself bounded by the progress timeout), so an idle kept-alive connection waits on
/// the progress bound alone.
const HEAD_DEADLINE: Duration = Duration::from_secs(15);
/// Bound on concurrent connections, so the fd set stays bounded no matter how fast a caller
/// opens sockets. Over-cap connections are closed on accept.
const MAX_CONNECTIONS: usize = 64;
/// After a refusal, a client already sending a body gets a bounded lingering close: the socket
/// drains a small slice (time-boxed) before the FIN, so the refusal is not lost to a reset. A
/// client that waited on `Expect: 100-continue` never sent a body and needs no drain.
const LINGER_BYTES: usize = 64 * 1024;
const LINGER_WINDOW: Duration = Duration::from_millis(100);
/// `/media/<id>` pass-through (05 A8): the artifact id is the capability and the instance
/// token is this listener's transport gate; the bridge relays the GET to the manager's media
/// surface without inspecting or validating the artifact itself.
const MEDIA_PREFIX: &str = "/media/";
/// The capability id: exactly 32 lowercase hex characters (128 bits). Any other shape is
/// refused at the bridge without touching the manager (05 A8's parsing rule).
const MEDIA_ID_LEN: usize = 32;
/// Upstream media fetch: connect bound, per-chunk copy size.
const MEDIA_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const MEDIA_CHUNK: usize = 64 * 1024;
/// JSON-RPC parse error (malformed JSON), same as the stdio bridge.
const PARSE_ERROR: i32 = -32700;

/// The network endpoint: one per bridge process, shared by every connection.
struct Network {
    instance: String,
    /// The instance token inbound callers must present.
    token: String,
    /// Where the token was required from, for the pinned `token_required` message.
    display: String,
    /// One manager link shared across requests; the caller identity rides each frame.
    client: Mutex<Option<Client>>,
}

/// Run the network bridge until killed. Startup is loud about generated credentials — the
/// token (07) and any per-scope secrets a scope file needs to be network-usable.
pub async fn run_listen(
    instance: &str,
    raw_addr: &str,
    remote: Option<String>,
) -> Result<(), ErrorEnvelope> {
    // Network mode serves the local instance: the scope files, the token and the manager link
    // are all local. A bridge in front of another manager would need that box's scope files to
    // gate and manifest correctly — the stdio bridge with `--remote` is the supported shape.
    if remote.is_some() {
        return Err(ErrorEnvelope::bad_request(strings::NETWORK_LOCAL_ONLY));
    }
    let paths = Paths::from_process(instance);
    let bind = match Address::parse(raw_addr)? {
        Address::Tcp { host, port } => (host, port),
        Address::Unix(_) => {
            return Err(ErrorEnvelope::bad_request(
                "--listen takes a host:port address; the unix socket is the default transport",
            ));
        }
    };
    let display = format!("{}:{}", bind.0, bind.1);

    // A network surface never runs tokenless: mint the instance token on any listen bind,
    // loopback included (07).
    let token = match config::read_token(&paths)? {
        Some(token) => token,
        None => {
            let token = config::generate_token();
            let path = config::write_token(&paths, &token)?;
            eprintln!(
                "wrote {} (mode 0600); 'daiorama token show' prints the value",
                path.display()
            );
            token
        }
    };

    // A scope without a secret cannot be used over the network (07); the first network start
    // makes every scope file present usable rather than leaving a silent dead end. Deleting a
    // scope's secret file takes it off the network again.
    for scope in paths.scope_names() {
        if config::read_scope_token(&paths, &scope)?.is_none() {
            let secret = config::generate_token();
            let path = config::write_scope_token(&paths, &scope, &secret)?;
            eprintln!("wrote {} (scope secret, mode 0600)", path.display());
        }
    }

    let listener = TcpListener::bind((bind.0.as_str(), bind.1))
        .await
        .map_err(|err| ErrorEnvelope::bad_request(format!("cannot listen on {display}: {err}")))?;
    let local = listener
        .local_addr()
        .map_err(|err| ErrorEnvelope::internal(format!("cannot read listener address: {err}")))?;
    eprintln!("listening on {local}");

    let network = Arc::new(Network {
        instance: instance.to_string(),
        token,
        display,
        client: Mutex::new(None),
    });
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                eprintln!("accept failed: {err}");
                continue;
            }
        };
        // Over the cap: close on accept. The fd set stays bounded no matter how fast a caller
        // opens sockets; a client that retries gets in as soon as a slot frees.
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let network = Arc::clone(&network);
        tokio::spawn(async move {
            let _permit = permit;
            let _ = serve_connection(&network, stream).await;
        });
    }
}

// ---------------------------------------------------------------------------
// One connection: HTTP in, JSON-RPC out
// ---------------------------------------------------------------------------

enum HeadRead {
    Eof,
    /// No progress within `PROGRESS_TIMEOUT`: close the connection, nothing follows.
    Timeout,
    TooLarge,
    Lines(Vec<String>),
}

async fn read_head<R>(input: &mut R) -> std::io::Result<HeadRead>
where
    R: AsyncBufRead + Unpin,
{
    let mut lines = Vec::new();
    let mut total = 0usize;
    // The head's clock starts once the request line has arrived: an idle kept-alive
    // connection is the progress bound's business, and the request line is bounded by it too.
    let mut started: Option<tokio::time::Instant> = None;
    loop {
        let mut line = Vec::new();
        let budget = match started {
            None => PROGRESS_TIMEOUT,
            Some(started) => {
                let remaining = HEAD_DEADLINE.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Ok(HeadRead::Timeout);
                }
                PROGRESS_TIMEOUT.min(remaining)
            }
        };
        let read = match tokio::time::timeout(budget, input.read_until(b'\n', &mut line)).await {
            Ok(result) => result?,
            // A head that stalls mid-line is a dead peer, not a slow one: close.
            Err(_elapsed) => return Ok(HeadRead::Timeout),
        };
        if read == 0 {
            return Ok(HeadRead::Eof);
        }
        started.get_or_insert_with(tokio::time::Instant::now);
        total += read;
        if total > MAX_HEAD_BYTES {
            return Ok(HeadRead::TooLarge);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            return Ok(HeadRead::Lines(lines));
        }
        lines.push(text.to_string());
    }
}

/// One HTTP response: a status, an optional JSON-RPC body, whether the connection ends, and
/// the `Allow` value a 405 carries (`POST` on the MCP target, `GET` on the media surface).
struct Reply {
    status: u16,
    body: Option<serde_json::Value>,
    close: bool,
    allow: Option<&'static str>,
}

impl Reply {
    fn mcp(body: serde_json::Value) -> Self {
        Self {
            status: 200,
            body: Some(body),
            close: false,
            allow: None,
        }
    }

    fn refusal(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            body: Some(body),
            close: true,
            allow: None,
        }
    }

    fn with_allow(mut self, allow: &'static str) -> Self {
        self.allow = Some(allow);
        self
    }

    fn accepted() -> Self {
        Self {
            status: 202,
            body: None,
            close: false,
            allow: None,
        }
    }
}

/// A transport-level refusal that still carries the canonical envelope in `data`: the caller
/// sees an MCP error with a stable code, never an HTTP auth challenge (05 §The bridge).
fn envelope_reply(status: u16, envelope: ErrorEnvelope) -> Reply {
    Reply::refusal(status, json_rpc_envelope_error(None, &envelope))
}

async fn serve_connection(network: &Arc<Network>, stream: TcpStream) -> Result<(), std::io::Error> {
    let mut input = BufReader::new(stream);
    loop {
        let head = match read_head(&mut input).await? {
            HeadRead::Eof => return Ok(()),
            HeadRead::Timeout => return Ok(()),
            HeadRead::TooLarge => {
                let reply =
                    envelope_reply(400, ErrorEnvelope::bad_request(strings::HEAD_TOO_LARGE));
                refuse(&mut input, &reply, None).await?;
                return Ok(());
            }
            HeadRead::Lines(lines) => lines,
        };

        let Some(request) = RequestHead::parse(&head) else {
            let reply = envelope_reply(
                400,
                ErrorEnvelope::bad_request(strings::MALFORMED_HTTP_REQUEST),
            );
            refuse(&mut input, &reply, None).await?;
            return Ok(());
        };

        let Some(host) = request.header("host") else {
            let reply = envelope_reply(400, ErrorEnvelope::bad_request(strings::MISSING_HOST));
            refuse(&mut input, &reply, Some(&request)).await?;
            return Ok(());
        };
        if let Some(origin) = request.header("origin")
            && !origin_matches(origin, host)
        {
            let reply = envelope_reply(
                403,
                ErrorEnvelope::bad_request(strings::origin_not_allowed(origin)),
            );
            refuse(&mut input, &reply, Some(&request)).await?;
            return Ok(());
        }
        // `/media/<id>` is the second target on this listener (05 A8): the manager's media
        // surface relayed, GET only, token-gated, never logged (the id is the capability).
        // It carries no body and closes after one fetch. The MCP target below stays
        // `POST /mcp` only.
        if request.target == "/media" || request.target.starts_with(MEDIA_PREFIX) {
            serve_media(network, &request, &mut input).await?;
            return Ok(());
        }
        if request.method != "POST" || request.target != "/mcp" {
            let status = if request.target == "/mcp" { 405 } else { 404 };
            let mut reply = envelope_reply(
                status,
                ErrorEnvelope::bad_request(strings::NETWORK_POST_ONLY),
            );
            if status == 405 {
                reply = reply.with_allow("POST");
            }
            refuse(&mut input, &reply, Some(&request)).await?;
            return Ok(());
        }
        if request.header("transfer-encoding").is_some() {
            let reply = envelope_reply(
                400,
                ErrorEnvelope::bad_request(strings::CHUNKED_UNSUPPORTED),
            );
            refuse(&mut input, &reply, Some(&request)).await?;
            return Ok(());
        }

        if request.duplicate_content_length {
            let reply = envelope_reply(
                400,
                ErrorEnvelope::bad_request(strings::DUPLICATE_CONTENT_LENGTH),
            );
            refuse(&mut input, &reply, Some(&request)).await?;
            return Ok(());
        }

        let length = match request.content_length() {
            Ok(length) => length,
            Err(message) => {
                let reply = envelope_reply(400, ErrorEnvelope::bad_request(message));
                refuse(&mut input, &reply, None).await?;
                return Ok(());
            }
        };
        // The identity gate answers from the head alone, before any body byte is read or
        // parsed: an unauthenticated caller must not exercise the JSON parser or learn
        // anything from its errors.
        let (scope, definition) = match authenticate(network, &request).await {
            Ok(caller) => caller,
            Err(envelope) => {
                // An id-less refusal answers its HTTP status, not a 200 envelope: a request/
                // response client matches by id, cannot match `null`, and would wait out its
                // read timeout — the refusal text would never reach it (11). 403, never 401
                // (the OAuth-discovery trigger).
                let reply = Reply::refusal(403, json_rpc_envelope_error(None, &envelope));
                // A refused gate closes: keep-alive would leave a body this request never sent
                // to be read as the next request's head. The refusal half-closes and lingers
                // (see `refuse`), so a body already in flight does not reset it away.
                refuse(&mut input, &reply, Some(&request)).await?;
                return Ok(());
            }
        };

        if length > MAX_FRAME_BYTES {
            // A head-decided size refusal cannot name an id either: 413, same envelope, close.
            let reply = Reply::refusal(
                413,
                json_rpc_envelope_error(
                    None,
                    &ErrorEnvelope::bad_request(strings::frame_too_large(MAX_FRAME_BYTES)),
                ),
            );
            refuse(&mut input, &reply, Some(&request)).await?;
            return Ok(());
        }
        if expects_continue(&request) {
            input
                .get_mut()
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await?;
        }
        let Some(body) = read_body(&mut input, length).await? else {
            return Ok(());
        };

        let parsed: Result<crate::mcp::JsonRpcRequest, _> = serde_json::from_slice(&body);
        let request_json = match parsed {
            Ok(request) => request,
            Err(err) => {
                // A body that does not parse has no id to name: the refusal answers 400 with
                // the same envelope and closes (11's id-less rule).
                let reply = Reply::refusal(
                    400,
                    json_rpc_error(
                        serde_json::Value::Null,
                        PARSE_ERROR,
                        &format!("parse error: {err}"),
                    ),
                );
                write_reply(input.get_mut(), &reply).await?;
                return Ok(());
            }
        };

        // Gate first, then dispatch. A notification is authenticated like a call but never
        // executes and never gets a response body (JSON-RPC; same rule as stdio).
        if request_json.id.is_none() {
            write_reply(input.get_mut(), &Reply::accepted()).await?;
            continue;
        }

        let mut bridge = Bridge::new(&network.instance, scope, definition, None);
        let mut client = network.client.lock().await;
        let Some(response) = bridge.handle_request(request_json, &mut client).await else {
            continue;
        };
        drop(client);
        write_reply(input.get_mut(), &Reply::mcp(response)).await?;

        if request
            .header("connection")
            .is_some_and(|value| value.eq_ignore_ascii_case("close"))
        {
            return Ok(());
        }
    }
}

// ---------------------------------------------------------------------------
// Authentication (07: token gate, then per-connection scope)
// ---------------------------------------------------------------------------

/// Read exactly `length` bytes of body with the per-chunk progress timeout. `Ok(None)` means
/// the peer stalled or closed mid-body: the caller ends the connection, nothing follows.
async fn read_body<R>(input: &mut R, length: usize) -> std::io::Result<Option<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut body = vec![0u8; length];
    let mut filled = 0usize;
    while filled < length {
        match tokio::time::timeout(PROGRESS_TIMEOUT, input.read(&mut body[filled..])).await {
            Ok(Ok(0)) => return Ok(None),
            Ok(Ok(n)) => filled += n,
            Ok(Err(err)) => return Err(err),
            // A stalled body is a dead peer: close, nothing follows.
            Err(_elapsed) => return Ok(None),
        }
    }
    Ok(Some(body))
}

/// Whether the client asked to hold the body back until a `100 Continue`.
fn expects_continue(request: &RequestHead) -> bool {
    request
        .header("expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
}

/// Read and discard up to `limit` bytes; the caller bounds the time. Errors and EOF end it —
/// courtesy to a client that is still sending, never a correctness path.
async fn drain_bounded<R>(input: &mut R, limit: usize) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = [0u8; 8192];
    let mut total = 0usize;
    while total < limit {
        let want = (limit - total).min(buf.len());
        let n = input.read(&mut buf[..want]).await?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(())
}

/// Refuse and end the connection: write the reply, half-close so the FIN rides out with the
/// response, then drain a bounded slice of a body that is already in flight — unless the client
/// waited on `Expect: 100-continue` (it sent no body). Every refusal site routes through here,
/// so no refusal loses its body to a reset.
async fn refuse(
    input: &mut BufReader<TcpStream>,
    reply: &Reply,
    request: Option<&RequestHead>,
) -> std::io::Result<()> {
    write_reply(input.get_mut(), reply).await?;
    input.get_mut().shutdown().await?;
    if request.is_none_or(|request| !expects_continue(request)) {
        let _ = tokio::time::timeout(LINGER_WINDOW, drain_bounded(input, LINGER_BYTES)).await;
    }
    Ok(())
}

/// One media fetch (05 A8): `GET /media/<id>` → the manager's media surface, streamed back.
/// The bridge validates no artifact — the 128-bit id is the credential; the instance token is
/// this listener's transport gate. The request target is never logged.
async fn serve_media(
    network: &Network,
    request: &RequestHead,
    input: &mut BufReader<TcpStream>,
) -> std::io::Result<()> {
    if request.method != "GET" {
        let reply = media_reply(
            405,
            ErrorEnvelope::bad_request(strings::MEDIA_GET_ONLY),
            Some("GET"),
        );
        return refuse(input, &reply, Some(request)).await;
    }
    let Some(id) = media_id(&request.target) else {
        let reply = media_reply(404, ErrorEnvelope::media_expired(), None);
        return refuse(input, &reply, Some(request)).await;
    };
    let presented = request.header("daiorama-token").unwrap_or_default();
    if !secret_matches(presented, &network.token) {
        let reply = media_reply(403, ErrorEnvelope::token_required(&network.display), None);
        return refuse(input, &reply, Some(request)).await;
    }
    match fetch_media(network, id).await {
        Ok(upstream) => stream_media(input.get_mut(), upstream).await,
        Err(envelope) => {
            let reply = media_reply(502, envelope, None);
            refuse(input, &reply, Some(request)).await
        }
    }
}

/// The request path must be exactly `/media/<32 lowercase hex>`; anything else is the pinned
/// `media_expired` 404 and never becomes an upstream request (05 A8's parsing rule).
fn media_id(target: &str) -> Option<&str> {
    let id = target.strip_prefix(MEDIA_PREFIX)?;
    let hex = id
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if id.len() == MEDIA_ID_LEN && hex {
        Some(id)
    } else {
        None
    }
}

/// A media refusal is a `Reply` like every other: the body is the canonical envelope (a media
/// fetch is a plain HTTP GET, not a JSON-RPC request) and the close rides the one `refuse`
/// path, so a request whose body is already in flight does not lose the refusal to a reset.
fn media_reply(status: u16, envelope: ErrorEnvelope, allow: Option<&'static str>) -> Reply {
    let body = serde_json::to_value(&envelope).expect("media envelopes serialize");
    let reply = Reply::refusal(status, body);
    match allow {
        Some(allow) => reply.with_allow(allow),
        None => reply,
    }
}

/// A manager media answer, already validated: a 200 or 404 with a declared length.
struct MediaUpstream {
    status: u16,
    content_type: String,
    length: usize,
    reader: BufReader<tokio::net::UnixStream>,
}

/// Fetch `/media/<id>` from the manager's own surface (the local unix socket: tokenless, 07).
/// The bridge refuses to guess a body's end: a missing `Content-Length` or any
/// `Transfer-Encoding` is a malformed upstream answer, never something to reinterpret.
async fn fetch_media(network: &Network, id: &str) -> Result<MediaUpstream, ErrorEnvelope> {
    let path = crate::ipc::socket_path(&network.instance);
    let display = path.display().to_string();
    let connect = tokio::time::timeout(
        MEDIA_CONNECT_TIMEOUT,
        tokio::net::UnixStream::connect(&path),
    )
    .await
    .map_err(|_| ErrorEnvelope::manager_unreachable(&display))?
    .map_err(|_| ErrorEnvelope::manager_unreachable(&display))?;
    let mut stream = connect;
    let request =
        format!("GET {MEDIA_PREFIX}{id} HTTP/1.1\r\nHost: daiorama\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| ErrorEnvelope::manager_unreachable(&display))?;
    let mut reader = BufReader::new(stream);
    let head = match read_head(&mut reader).await {
        Ok(HeadRead::Lines(lines)) => lines,
        _ => return Err(media_malformed()),
    };
    let mut lines = head.iter();
    let Some(status_line) = lines.next() else {
        return Err(media_malformed());
    };
    let mut parts = status_line.split_whitespace();
    let status = match (
        parts.next(),
        parts.next().and_then(|code| code.parse::<u16>().ok()),
    ) {
        (Some(version), Some(status)) if version.starts_with("HTTP/1.") => status,
        _ => return Err(media_malformed()),
    };
    if status != 200 && status != 404 {
        return Err(media_malformed());
    }
    let mut content_type = None;
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(media_malformed());
        };
        let name = name.to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "transfer-encoding" => return Err(media_malformed()),
            "content-length" => {
                length = value.parse::<usize>().ok();
                if length.is_none() {
                    return Err(media_malformed());
                }
            }
            "content-type" => content_type = Some(value.to_string()),
            _ => {}
        }
    }
    let Some(length) = length else {
        return Err(media_malformed());
    };
    Ok(MediaUpstream {
        status,
        content_type: content_type.unwrap_or_else(|| "application/octet-stream".to_string()),
        length,
        reader,
    })
}

fn media_malformed() -> ErrorEnvelope {
    ErrorEnvelope::internal(strings::MEDIA_UPSTREAM_MALFORMED)
}

/// Copy the upstream answer to the client as it arrives: head first, then `length` bytes in
/// chunks. No whole-body buffering — the manager's single-use lifetime relies on the fetch
/// completing, not on the bridge hoarding it.
async fn stream_media(stream: &mut TcpStream, upstream: MediaUpstream) -> std::io::Result<()> {
    let head = reply_head(
        upstream.status,
        &upstream.content_type,
        upstream.length,
        true,
        None,
    );
    write_bounded(stream, head.as_bytes()).await?;
    let MediaUpstream {
        length, mut reader, ..
    } = upstream;
    let mut remaining = length;
    let mut buffer = vec![0u8; MEDIA_CHUNK.min(remaining.max(1))];
    while remaining > 0 {
        let want = remaining.min(buffer.len());
        let read =
            match tokio::time::timeout(PROGRESS_TIMEOUT, reader.read(&mut buffer[..want])).await {
                Ok(Ok(0)) => break,
                Ok(Ok(read)) => read,
                Ok(Err(err)) => return Err(err),
                // Upstream stalled: the client keeps the head and a short body; there is no
                // repair for a truncated artifact (the manager owns its lifetime).
                Err(_) => break,
            };
        write_bounded(stream, &buffer[..read]).await?;
        remaining -= read;
    }
    stream.flush().await
}

async fn authenticate(
    network: &Network,
    request: &RequestHead,
) -> Result<(String, ScopeFile), ErrorEnvelope> {
    // Transport gate first: a caller without the instance token learns nothing about scopes.
    let presented = request.header("daiorama-token").unwrap_or_default();
    if !secret_matches(presented, &network.token) {
        return Err(ErrorEnvelope::token_required(&network.display));
    }

    let scope = request
        .header("daiorama-scope")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::ScopeAuthRequired,
                strings::NETWORK_SCOPE_REQUIRED,
            )
        })?;

    let definition = crate::mcp::server::load_scope(&network.instance, scope)?;
    let paths = Paths::from_process(&network.instance);
    let secret = config::read_scope_token(&paths, scope)?.ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::ScopeAuthRequired,
            strings::scope_has_no_secret(scope),
        )
        .with_hint("create the scope secret file on the bridge host")
    })?;
    let presented = request.header("daiorama-scope-token").unwrap_or_default();
    if !secret_matches(presented, &secret) {
        return Err(ErrorEnvelope::new(
            ErrorCode::ScopeAuthRequired,
            strings::network_scope_auth_required(scope),
        ));
    }
    Ok((scope.to_string(), definition))
}

/// Length-checked, value-constant-time comparison (the token and scope secrets are secrets;
/// length is not).
fn secret_matches(presented: &str, expected: &str) -> bool {
    let presented = presented.as_bytes();
    let expected = expected.as_bytes();
    if presented.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in presented.iter().zip(expected) {
        diff |= a ^ b;
    }
    diff == 0
}

/// `Origin` must name the same authority the request was sent to (`Host`): the bridge has one
/// origin per listener, so any cross-origin browser shape is refused (07).
fn origin_matches(origin: &str, host: &str) -> bool {
    let Some((_, authority)) = origin.split_once("://") else {
        return false;
    };
    let authority = authority.split('/').next().unwrap_or(authority);
    authority.eq_ignore_ascii_case(host)
}

// ---------------------------------------------------------------------------
// HTTP request head
// ---------------------------------------------------------------------------

struct RequestHead {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    /// More than one `Content-Length` header: last-wins would let the two lines disagree
    /// about the body size (a request-smuggling shape), so the request is refused like the
    /// chunked form (05 §The bridge).
    duplicate_content_length: bool,
}

impl RequestHead {
    fn parse(lines: &[String]) -> Option<Self> {
        let mut lines = lines.iter();
        let (method, target) = {
            let request_line = lines.next()?;
            let mut parts = request_line.split_whitespace();
            let method = parts.next()?.to_string();
            let target = parts.next()?.to_string();
            let version = parts.next()?;
            if !version.starts_with("HTTP/1.") {
                return None;
            }
            (method, target)
        };
        let mut headers = HashMap::new();
        let mut duplicate_content_length = false;
        for line in lines {
            let (name, value) = line.split_once(':')?;
            // RFC 7230 §3.2.4: no whitespace between field-name and colon. Trimming it would
            // paper over the exact bytes a fronting proxy rejects — the smuggling disagreement
            // the duplicate-`Content-Length` refusal already guards against. A leading space is
            // an obs-fold continuation, also refused (a folded line is not a field).
            if name.is_empty() || name.trim() != name {
                return None;
            }
            let name = name.to_ascii_lowercase();
            // RFC 7230 §5.4: a request with more than one `Host`, or one with an empty value,
            // is a 400. Last-wins would let two readers of the same head disagree about which
            // authority the request is for — and the `Origin` check reads the same field.
            if name == "host" && (headers.contains_key("host") || value.trim().is_empty()) {
                return None;
            }
            if name == "content-length" && headers.contains_key("content-length") {
                duplicate_content_length = true;
            }
            headers.insert(name, value.trim().to_string());
        }
        Some(Self {
            method,
            target,
            headers,
            duplicate_content_length,
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    fn content_length(&self) -> Result<usize, String> {
        match self.header("content-length") {
            None => Ok(0),
            // RFC 7230 §3.3.2: `Content-Length` is `1*DIGIT`. Rust's `parse` also accepts a
            // leading `+` (and an empty value would read as no body); a proxy that follows the
            // grammar rejects those bytes while this side would frame a body — a disagreement
            // about where the next request starts. Only digits here.
            Some(raw) if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) => {
                Err(strings::invalid_content_length(raw))
            }
            Some(raw) => raw
                .parse::<usize>()
                .map_err(|_| strings::invalid_content_length(raw)),
        }
    }
}

/// A media answer is streamed to a client that keeps reading; a client that accepts nothing
/// for `PROGRESS_TIMEOUT` is gone, not slow, and is cut off — the same progress bound the read
/// side runs on. Without it the task sits in an unbounded write and the slot is held for as
/// long as the client stays silent (`p3/media/evidence/probe-media-stall.py`, leg C).
async fn write_bounded<W>(writer: &mut W, bytes: &[u8]) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut written = 0;
    while written < bytes.len() {
        let progress =
            match tokio::time::timeout(PROGRESS_TIMEOUT, writer.write(&bytes[written..])).await {
                Ok(Ok(0)) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
                Ok(Ok(progress)) => progress,
                Ok(Err(err)) => return Err(err),
                Err(_) => return Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
            };
        written += progress;
    }
    Ok(())
}

async fn write_reply(stream: &mut TcpStream, reply: &Reply) -> Result<(), std::io::Error> {
    let (body, length) = match &reply.body {
        Some(value) => {
            let body = serde_json::to_vec(value).expect("replies serialize");
            let length = body.len();
            (Some(body), length)
        }
        None => (None, 0),
    };
    let head = reply_head(
        reply.status,
        "application/json",
        length,
        reply.close,
        reply.allow,
    );
    write_bounded(stream, head.as_bytes()).await?;
    if let Some(body) = body {
        write_bounded(stream, &body).await?;
    }
    stream.flush().await
}

/// The response head, shared by every reply on this listener — MCP answers, media successes and
/// refusals — so a status, a content type, a close and an `Allow` value cannot drift between
/// them. A 405 names the method it does allow: `POST` on `/mcp`, `GET` on `/media/<id>`.
fn reply_head(
    status: u16,
    content_type: &str,
    length: usize,
    close: bool,
    allow: Option<&str>,
) -> String {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\n\
         Connection: {}\r\n",
        if close { "close" } else { "keep-alive" }
    );
    if let Some(allow) = allow {
        head.push_str(&format!("Allow: {allow}\r\n"));
    }
    head.push_str("\r\n");
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    const README: &str = include_str!("../../fixtures/README.md");

    /// A writer that accepts nothing: the response bound must cut it, not wait it out.
    struct Stalled;

    impl tokio::io::AsyncWrite for Stalled {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Pending
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A writer that takes one byte per call: slow, but every call makes progress.
    struct Trickle(usize);

    impl tokio::io::AsyncWrite for Trickle {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            assert!(
                !buf.is_empty(),
                "a bounded write never passes an empty slice"
            );
            self.get_mut().0 += 1;
            std::task::Poll::Ready(Ok(1))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// The write side gets the read side's bound: a media client that stops reading is cut
    /// off, so it cannot pin a connection slot by staying silent.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_writer_is_cut_at_the_progress_bound() {
        let started = tokio::time::Instant::now();
        let error = write_bounded(&mut Stalled, b"0123456789")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(started.elapsed(), PROGRESS_TIMEOUT);
    }

    /// The bound is per write, not a total-transfer deadline: a client that keeps taking
    /// bytes finishes the body however long it takes.
    #[tokio::test(start_paused = true)]
    async fn a_slow_writer_still_finishes_the_body() {
        let mut trickle = Trickle(0);
        write_bounded(&mut trickle, b"0123456789").await.unwrap();
        assert_eq!(trickle.0, 10);
    }

    #[test]
    fn response_heads_share_one_shape_and_name_their_allow() {
        // A 405 names the method the target does serve: POST on `/mcp`, GET on `/media/<id>`.
        let mcp = reply_head(405, "application/json", 7, true, Some("POST"));
        assert!(mcp.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        assert!(mcp.contains("Allow: POST\r\n"));
        assert!(!mcp.contains("Allow: GET"));
        let media = media_reply(
            405,
            ErrorEnvelope::bad_request("media fetches are GET only"),
            Some("GET"),
        );
        let head = reply_head(media.status, "image/png", 2, media.close, media.allow);
        assert!(head.contains("Allow: GET\r\n"));
        assert!(head.contains("Connection: close\r\n"));
        // The media statuses carry their own reason phrases and a close; no `Allow` elsewhere.
        for (status, reason) in [(403, "Forbidden"), (404, "Not Found"), (502, "Bad Gateway")] {
            let reply = media_reply(status, ErrorEnvelope::bad_request("x"), None);
            assert!(reply.close);
            assert!(reply.allow.is_none());
            let head = reply_head(
                reply.status,
                "application/json",
                0,
                reply.close,
                reply.allow,
            );
            assert!(head.starts_with(&format!("HTTP/1.1 {status} {reason}\r\n")));
            assert!(!head.contains("Allow:"));
        }
    }

    #[test]
    fn network_refusals_are_pinned_in_the_fixture_readme() {
        for pinned in [
            strings::NETWORK_SCOPE_REQUIRED,
            strings::NETWORK_POST_ONLY,
            strings::NETWORK_LOCAL_ONLY,
            strings::MISSING_HOST,
            strings::CHUNKED_UNSUPPORTED,
            strings::HEAD_TOO_LARGE,
            strings::MALFORMED_HTTP_REQUEST,
            strings::DUPLICATE_CONTENT_LENGTH,
            strings::MEDIA_GET_ONLY,
            strings::MEDIA_UPSTREAM_MALFORMED,
        ] {
            assert!(
                README.contains(pinned),
                "fixtures/README pins {pinned:?} (the network contract)"
            );
        }
        let with_values = [
            strings::network_scope_auth_required("agent1"),
            strings::scope_has_no_secret("agent1"),
            strings::origin_not_allowed("http://evil.example"),
            strings::invalid_content_length("abc"),
        ];
        let templates = [
            "scope {scope} requires its scope secret (Daiorama-Scope-Token header)",
            "scope {scope} has no secret; it cannot be used over the network",
            "origin not allowed: {origin}",
            "invalid Content-Length: {raw}",
        ];
        for (value, template) in with_values.iter().zip(templates) {
            let shape = template
                .replace("{scope}", "\"agent1\"")
                .replace("{origin}", "\"http://evil.example\"")
                .replace("{raw}", "\"abc\"");
            assert_eq!(value, &shape, "the network string template holds");
            assert!(README.contains(template), "README pins {template:?}");
        }
    }

    #[test]
    fn origin_must_match_the_host_authority() {
        assert!(origin_matches("http://127.0.0.1:8757", "127.0.0.1:8757"));
        assert!(origin_matches("https://Example.test", "example.test"));
        assert!(!origin_matches("http://evil.example", "127.0.0.1:8757"));
        assert!(!origin_matches("null", "127.0.0.1:8757"));
        assert!(!origin_matches("http://127.0.0.1:9999", "127.0.0.1:8757"));
    }

    /// 05 §The bridge: the HTTP transport refusals are pinned — a harness can assert them.
    #[test]
    fn transport_refusals_are_pinned() {
        assert_eq!(strings::HEAD_TOO_LARGE, "request head too large");
        assert_eq!(strings::MALFORMED_HTTP_REQUEST, "malformed HTTP request");
        assert_eq!(
            strings::DUPLICATE_CONTENT_LENGTH,
            "duplicate Content-Length"
        );
        assert_eq!(
            strings::invalid_content_length("abc"),
            "invalid Content-Length: \"abc\""
        );
    }

    /// The head shapes a fronting proxy refuses: two readers of one head must not disagree
    /// about the authority or about where the body ends (RFC 7230 §3.2.4, §3.3.2, §5.4).
    #[test]
    fn smuggling_head_shapes_are_malformed() {
        let parse = |lines: &[&str]| {
            RequestHead::parse(
                &lines
                    .iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>(),
            )
        };
        assert!(
            parse(&["POST /mcp HTTP/1.1", "Host: a", "Host: b"]).is_none(),
            "duplicate Host is refused"
        );
        assert!(
            parse(&["POST /mcp HTTP/1.1", "Host: a", "Host: a"]).is_none(),
            "duplicate Host is refused even when the values agree"
        );
        assert!(
            parse(&["POST /mcp HTTP/1.1", "Host:"]).is_none(),
            "an empty Host is not an authority"
        );
        assert!(
            parse(&["POST /mcp HTTP/1.1", "Host : a"]).is_none(),
            "whitespace before the colon is refused"
        );
        assert!(
            parse(&["POST /mcp HTTP/1.1", "Host: a", " folded"]).is_none(),
            "a folded continuation line is not a field"
        );
        assert!(parse(&["POST /mcp HTTP/1.1", "Host: a"]).is_some());
    }

    #[test]
    fn content_length_is_one_star_digit() {
        let length = |raw: &str| {
            RequestHead::parse(&[
                "POST /mcp HTTP/1.1".to_string(),
                "Host: a".to_string(),
                format!("Content-Length: {raw}"),
            ])
            .expect("head parses")
            .content_length()
        };
        assert_eq!(length("5").unwrap(), 5);
        assert_eq!(length("0005").unwrap(), 5, "leading zeros are digits");
        assert!(length("+5").is_err(), "a leading plus is not a digit");
        assert!(length("").is_err(), "an empty value is not a length");
        assert!(length("-1").is_err());
        assert!(length("5, 5").is_err());
        assert!(length("0x10").is_err());
    }

    #[test]
    fn secrets_compare_exactly_and_bound() {
        assert!(secret_matches("abc", "abc"));
        assert!(!secret_matches("abc", "abd"));
        assert!(!secret_matches("abc", "abcd"));
        assert!(!secret_matches("", "abc"));
    }

    /// A head dribbled one line at a time never stalls long enough for the per-line progress
    /// bound, so it used to hold a connection slot until the client chose to finish. The head
    /// now has its own total window, counted from the request line.
    #[tokio::test(start_paused = true)]
    async fn a_dribbled_head_dies_at_the_head_deadline() {
        let (mut peer, server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            if peer.write_all(b"POST /mcp HTTP/1.1\r\n").await.is_err() {
                return;
            }
            for i in 0..12 {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let line = format!("X-Pad-{i:02}: 1\r\n");
                if peer.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
            let _ = peer.write_all(b"\r\n").await;
        });
        let mut input = tokio::io::BufReader::new(server);
        let started = tokio::time::Instant::now();
        let outcome = read_head(&mut input).await.unwrap();
        assert!(
            matches!(outcome, HeadRead::Timeout),
            "a dribbled head is cut off, not waited out"
        );
        assert_eq!(
            started.elapsed(),
            HEAD_DEADLINE,
            "the cut lands exactly on the pinned window"
        );
    }

    /// The head clock starts after the request line: a kept-alive connection with nothing to
    /// say is still closed by the progress bound, not by the head window.
    #[tokio::test(start_paused = true)]
    async fn an_idle_connection_waits_on_the_progress_bound() {
        let (_peer, server) = tokio::io::duplex(1024);
        let mut input = tokio::io::BufReader::new(server);
        let started = tokio::time::Instant::now();
        let outcome = read_head(&mut input).await.unwrap();
        assert!(matches!(outcome, HeadRead::Timeout));
        assert_eq!(started.elapsed(), PROGRESS_TIMEOUT);
    }

    #[test]
    fn request_heads_parse_and_reject_non_http() {
        let head = vec![
            "POST /mcp HTTP/1.1".to_string(),
            "Host: 127.0.0.1:8757".to_string(),
            "Daiorama-Scope: agent1".to_string(),
        ];
        let parsed = RequestHead::parse(&head).unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.header("daiorama-scope"), Some("agent1"));

        let broken = vec!["POST /mcp HTTP/2".to_string()];
        assert!(RequestHead::parse(&broken).is_none());
        let broken = vec!["POST /mcp HTTP/1.1".to_string(), "no colon".to_string()];
        assert!(RequestHead::parse(&broken).is_none());
    }
}
