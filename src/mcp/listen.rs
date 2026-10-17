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

/// One HTTP response: a status, an optional JSON-RPC body, and whether the connection ends.
struct Reply {
    status: u16,
    body: Option<serde_json::Value>,
    close: bool,
}

impl Reply {
    fn mcp(body: serde_json::Value) -> Self {
        Self {
            status: 200,
            body: Some(body),
            close: false,
        }
    }

    fn refusal(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            body: Some(body),
            close: true,
        }
    }

    fn accepted() -> Self {
        Self {
            status: 202,
            body: None,
            close: false,
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
                write_reply(input.get_mut(), &reply).await?;
                return Ok(());
            }
            HeadRead::Lines(lines) => lines,
        };

        let Some(request) = RequestHead::parse(&head) else {
            let reply = envelope_reply(
                400,
                ErrorEnvelope::bad_request(strings::MALFORMED_HTTP_REQUEST),
            );
            write_reply(input.get_mut(), &reply).await?;
            return Ok(());
        };

        let Some(host) = request.header("host") else {
            let reply = envelope_reply(400, ErrorEnvelope::bad_request(strings::MISSING_HOST));
            write_reply(input.get_mut(), &reply).await?;
            return Ok(());
        };
        if let Some(origin) = request.header("origin")
            && !origin_matches(origin, host)
        {
            let reply = envelope_reply(
                403,
                ErrorEnvelope::bad_request(strings::origin_not_allowed(origin)),
            );
            write_reply(input.get_mut(), &reply).await?;
            return Ok(());
        }
        if request.method != "POST" || request.target != "/mcp" {
            let status = if request.target == "/mcp" { 405 } else { 404 };
            let reply = envelope_reply(
                status,
                ErrorEnvelope::bad_request(strings::NETWORK_POST_ONLY),
            );
            write_reply(input.get_mut(), &reply).await?;
            return Ok(());
        }
        if request.header("transfer-encoding").is_some() {
            let reply = envelope_reply(
                400,
                ErrorEnvelope::bad_request(strings::CHUNKED_UNSUPPORTED),
            );
            write_reply(input.get_mut(), &reply).await?;
            return Ok(());
        }

        if request.duplicate_content_length {
            let reply = envelope_reply(
                400,
                ErrorEnvelope::bad_request(strings::DUPLICATE_CONTENT_LENGTH),
            );
            write_reply(input.get_mut(), &reply).await?;
            return Ok(());
        }

        let length = match request.content_length() {
            Ok(length) => length,
            Err(message) => {
                let reply = envelope_reply(400, ErrorEnvelope::bad_request(message));
                write_reply(input.get_mut(), &reply).await?;
                return Ok(());
            }
        };
        // The identity gate answers from the head alone, before any body byte is read or
        // parsed: an unauthenticated caller must not exercise the JSON parser or learn
        // anything from its errors.
        let (scope, definition) = match authenticate(network, &request).await {
            Ok(caller) => caller,
            Err(envelope) => {
                let reply = Reply::refusal(200, json_rpc_envelope_error(None, &envelope));
                write_reply(input.get_mut(), &reply).await?;
                // A refused gate closes: answering keep-alive would leave a body this request
                // never sent — an `Expect: 100-continue` client stops on the refusal — to be
                // read as the next request's head. Keep-alive after a refused auth buys
                // nothing; a fresh connection starts clean.
                return Ok(());
            }
        };

        if length > MAX_FRAME_BYTES {
            let reply = Reply::refusal(
                200,
                json_rpc_envelope_error(
                    None,
                    &ErrorEnvelope::bad_request(strings::frame_too_large(MAX_FRAME_BYTES)),
                ),
            );
            write_reply(input.get_mut(), &reply).await?;
            return Ok(());
        }
        if request
            .header("expect")
            .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
        {
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
                let reply = Reply::mcp(json_rpc_error(
                    serde_json::Value::Null,
                    PARSE_ERROR,
                    &format!("parse error: {err}"),
                ));
                write_reply(input.get_mut(), &reply).await?;
                continue;
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

async fn write_reply(stream: &mut TcpStream, reply: &Reply) -> Result<(), std::io::Error> {
    let (body, length) = match &reply.body {
        Some(value) => {
            let body = serde_json::to_vec(value).expect("replies serialize");
            let length = body.len();
            (Some(body), length)
        }
        None => (None, 0),
    };
    let reason = match reply.status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {length}\r\n\
         Connection: {}\r\n",
        reply.status,
        if reply.close { "close" } else { "keep-alive" }
    );
    if reply.status == 405 {
        head.push_str("Allow: POST\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    if let Some(body) = body {
        stream.write_all(&body).await?;
    }
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    const README: &str = include_str!("../../fixtures/README.md");

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
