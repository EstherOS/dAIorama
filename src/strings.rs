//! Every operator-visible string pinned by `docs/04-cli.md` and `docs/05-mcp-bridge.md`,
//! defined exactly once.
//!
//! Operators, agents and scripts string-match these; wording changes are spec changes.
//! The unit tests at the bottom of this module render every 04 §"Refusal and error strings"
//! bullet from the frozen document and compare it byte-for-byte against the functions here,
//! so drift between docs and code fails the build instead of shipping.
//!
//! Dependency position: text only, no logic, no imports — the module every other module may
//! use without creating a cycle.
//!
//! Note on lints: this crate is the P2 phase-1 **contract layer** — several items here are
//! constructed by the runtime half (W2/W3) rather than by today's CLI, and are exercised by
//! the module's unit tests until then. `dead_code` is allowed module-wide with that reason in
//! mind; it goes away as the consumers land.
#![allow(dead_code)]

// ---------------------------------------------------------------------------
// Answers (not errors)
// ---------------------------------------------------------------------------

/// The cold commands' honest answer (04). Printed on stdout, exit code 0.
pub const NO_MANAGER_RUNNING: &str = "no manager running";

// ---------------------------------------------------------------------------
// 04 — refusal and error strings (canonical, shared verbatim by CLI, bridge, dashboard)
// ---------------------------------------------------------------------------

/// 04: `manager unreachable at {addr}` — the shared reachability string.
pub fn manager_unreachable(addr: &str) -> String {
    format!("manager unreachable at {addr}")
}

/// 04: Hyprland missing on a bare host. `{resolved command}` comes from the maintained
/// install table (09); [`HYPRLAND_NOT_PACKAGED`] is what it renders where no package exists.
pub fn hyprland_missing(resolved_command: &str) -> String {
    format!("Hyprland not found. Install it: {resolved_command}, or use the container image.")
}

/// 04: the `{resolved command}` rendering where the distribution has no package.
pub const HYPRLAND_NOT_PACKAGED: &str =
    "not packaged for this distribution; see the release notes for a COPR/upstream build";

/// 04: Hyprland outside the supported version range.
pub fn hyprland_unsupported(found: &str, min: &str, max: &str) -> String {
    format!("Hyprland {found} is outside the supported range {min}-{max}.")
}

/// 09/04: `daiorama doctor`'s supported Hyprland range — the spike-verified series; re-pin
/// per re-verified release (one line, deliberately cheap to flip).
pub const HYPRLAND_MIN_VERSION: &str = "0.56.0";
pub const HYPRLAND_MAX_VERSION: &str = "0.56.9";

// ---------------------------------------------------------------------------
// 04 — doctor presentation (W3; section labels are provisional — the specs pin no label —
// kept here so the tests catch drift like every other operator-visible string)
// ---------------------------------------------------------------------------

/// The report's first line.
pub const DOCTOR_HEADER: &str = "daiorama doctor — environment check";
/// Section label: tool presence + versions.
pub const DOCTOR_TOOLS_LABEL: &str = "tools:";
/// Section label: Wayland environment facts.
pub const DOCTOR_WAYLAND_LABEL: &str = "wayland:";
/// Section label: the delegation check (06 C6).
pub const DOCTOR_CONTAINMENT_LABEL: &str = "containment:";

/// 04: software renderer detected at session creation (06 renderer policy).
pub fn renderer_refused(renderer: &str) -> String {
    format!(
        "session refused: active renderer is {renderer} (software). This session would carry a \
         known automation fingerprint. Run 'daiorama doctor'."
    )
}

/// 04: non-loopback bind requested and no token could be generated (07).
pub fn bind_refused(addr: &str, path: &str) -> String {
    format!(
        "refusing to bind {addr}: non-loopback binds require a token; none could be generated at {path}."
    )
}

/// 04: cgroup v2 delegation preconditions missing at manager startup (06).
pub fn containment_unavailable(detail: &str) -> String {
    format!(
        "containment unavailable: cgroup v2 delegation is not available here ({detail}). Fix: run \
         with a writable/delegated cgroupfs (see the compose notes), or set limits.mode = off and \
         accept losing per-session limits and crash-time cleanup."
    )
}

/// 04: client-side missing token for a token-gated address (07).
pub fn token_required(addr: &str) -> String {
    format!(
        "token required for {addr}: set DAIORAMA_TOKEN (value from 'daiorama token show' on the manager host)."
    )
}

/// 04: scope + global session quota exhausted (05 `quota_exceeded`).
pub fn session_limit_reached(
    scope: &str,
    n: u32,
    max: u32,
    global: u32,
    global_max: u32,
) -> String {
    format!(
        "session limit reached: scope '{scope}' {n}/{max}; global {global}/{global_max}. Close a session first."
    )
}

/// 04: `host` backend exclusivity (05 `backend_busy`; 06 B6).
pub fn backend_busy(id: &str) -> String {
    format!(
        "backend 'host' is exclusive; in use by session {id}. Close it or use --backend=headless."
    )
}

/// 04: new sessions refused while draining (05 `draining`).
pub fn draining(n: u32) -> String {
    format!("manager is draining for upgrade; new sessions are refused ({n} still running).")
}

/// P2 hardening: a request frame over the read-side byte cap (fixtures/README frame
/// contract). The refusal ends the connection, like a refused handshake.
pub fn frame_too_large(max: usize) -> String {
    format!("request frame must be at most {max} bytes")
}

/// P2 hardening: a line that is not valid UTF-8 cannot be a frame (fixtures/README frame
/// contract). Refused like an over-cap frame; the refusal ends the connection.
pub const LINE_NOT_UTF8: &str = "request line is not valid UTF-8";

/// P2 hardening: a second `handshake` on one connection is a gate violation (05 A7: the
/// handshake is the first call, once per connection). Refused; ends the connection.
pub const DUPLICATE_HANDSHAKE: &str = "handshake may only be the first call on a connection";

/// 04: protocol ranges do not overlap (05 §Version handshake; 05 `version_incompatible`).
pub fn protocol_mismatch(
    client_version: &str,
    client_min: &str,
    manager_version: &str,
    manager_min: &str,
) -> String {
    format!(
        "protocol mismatch: this client speaks {client_version} (min {client_min}); manager speaks \
         {manager_version} (min {manager_min})."
    )
}

/// 04: stdio bridge started without a scope (05).
pub const SCOPE_REQUIRED: &str = "scope required: set DAIORAMA_SCOPE.";

/// 04: dashboard module absent when a dashboard command needs it (09).
pub const MODULE_NOT_INSTALLED: &str = "module not installed; run 'daiorama dashboard install'.";

/// 04: view start refused — wayvnc unusable, port range exhausted, or front bind refused (05).
pub fn view_unavailable(session: &str, reason: &str) -> String {
    format!("view unavailable for {session}: {reason}.")
}

/// 04: the MCP-side form of the missing-ID error (05 `no_session_specified`).
pub const NO_SESSION_SPECIFIED: &str =
    "no session specified; create one first (tool: create_session).";

// ---------------------------------------------------------------------------
// 04 — lifecycle report line (B5)
// ---------------------------------------------------------------------------

/// 04: printed on the single line that reports a lazy start.
/// `mode` is `attached` or `background`.
pub fn started_manager(mode: &str, pid: u32, socket: &str) -> String {
    format!("started manager (mode={mode}, pid={pid}) socket={socket}")
}

// ---------------------------------------------------------------------------
// 05 — hints (corrective next action; `hint` is optional in the envelope)
// ---------------------------------------------------------------------------

/// 05: the pinned hint example for `no_session_specified`.
pub const HINT_CREATE_A_SESSION_FIRST: &str = "create a session first";
/// 05: the pinned hint example for `quota_exceeded`.
pub const HINT_CLOSE_OR_RAISE_QUOTA: &str = "close a session or raise the scope quota";
/// 05 `session_not_found`: hint pinned by the contract-layer fixture (`close-unknown`).
pub const HINT_LIST_SESSIONS: &str = "list sessions to find one";
/// 05 `session_closed` / `session_reaped` / `session_dead`: the session is gone for good.
pub const HINT_CREATE_A_NEW_SESSION: &str = "create a new session";
/// 05 `tool_not_granted`: how a denied caller fixes the denial.
pub const HINT_GRANT_IN_SCOPE: &str = "grant the tool in the scope's manifest";
/// 05 `renderer_refused`: the diagnostic the refusal points at.
pub const HINT_RUN_DOCTOR: &str = "run 'daiorama doctor'";
/// 05 `backend_busy`: the documented alternative.
pub const HINT_USE_HEADLESS: &str = "use --backend=headless";
/// 05 `draining`: what the caller waits for.
pub const HINT_RETRY_AFTER_UPGRADE: &str = "retry after the upgrade completes";
/// 05 `version_incompatible`: overlap the ranges by upgrading the older side.
pub const HINT_UPGRADE_PROTOCOL: &str =
    "upgrade the older side to a protocol version whose range overlaps";
/// 05 `limits_unavailable`: the explicit, recorded downgrade (06).
pub const HINT_LIMITS_OFF: &str = "set limits.mode = off to accept the documented downgrade";
/// 05 `bad_request`: where the valid surface is documented in place.
pub const HINT_CHECK_HELP: &str = "check the request against 'daiorama <command> --help'";
/// 05 `rate_limited`: the documented client behaviour.
pub const HINT_BACK_OFF: &str = "back off and retry";
/// 05 `media_expired`: capability URLs are single-use and TTL-bounded (05 A8).
pub const HINT_CAPTURE_AGAIN: &str = "capture again; media links are single-use";
/// 05 `view_unavailable`: the corrective step where one exists.
pub const HINT_VIEW_RETRY: &str = "retry 'daiorama view'; run 'daiorama doctor' if it persists";
/// 05 `scope_required`: the exact environment variable.
pub const HINT_SET_SCOPE_ENV: &str = "set DAIORAMA_SCOPE";
/// 05 `token_required`: where the token comes from.
pub const HINT_TOKEN_SHOW: &str =
    "run 'daiorama token show' on the manager host and set DAIORAMA_TOKEN";

// ---------------------------------------------------------------------------
// CLI presentation (04 §Machine-readable output)
// ---------------------------------------------------------------------------

/// Non-JSON errors print the message with this suffix form: `<message> (<code>)`.
/// One clean form, uniform across every command (04).
pub fn error_line(message: &str, code: impl std::fmt::Display) -> String {
    format!("{message} ({code})")
}

/// Prefix used when a non-JSON error carries a hint on its own stderr line.
pub fn hint_line(hint: &str) -> String {
    format!("hint: {hint}")
}

// ---------------------------------------------------------------------------
// Placeholder strings (excluded from the frozen-string tests: they are scaffolding, not
// contract)
// ---------------------------------------------------------------------------

/// Everything the runtime side implements in `p2/session-*` answers this envelope for now.
pub const NOT_IMPLEMENTED_MESSAGE: &str = "not implemented yet (P2 phase 1: contract layer)";
/// The stub envelope's hint.
pub const NOT_IMPLEMENTED_HINT: &str = "the runtime side lands in p2/session-*";

#[cfg(test)]
mod tests {
    use super::*;

    /// The frozen document, read at test time: cargo runs unit tests with the crate root
    /// as the working directory.
    const DOC_04: &str = include_str!("../docs/04-cli.md");

    /// Pull one bullet from 04's "Refusal and error strings" section, verbatim, without the
    /// markdown decoration.
    fn bullet(fragment: &str) -> String {
        let section = DOC_04
            .split("## Refusal and error strings")
            .nth(1)
            .expect("04 keeps a 'Refusal and error strings' section");
        let line = section
            .lines()
            .find(|l| l.starts_with("- ") && l.contains(fragment))
            .unwrap_or_else(|| panic!("04 pins a bullet containing {fragment:?}"));
        let raw = line.trim_start_matches("- ").trim();
        let start = raw.find('`').expect("pinned bullets are backticked") + 1;
        let end = start
            + raw[start..]
                .find('`')
                .expect("pinned bullets close their backticks");
        raw[start..end].to_string()
    }

    /// Fill `{placeholder}` slots in a doc bullet with sample values.
    fn fill(template: &str, values: &[(&str, &str)]) -> String {
        let mut out = template.to_string();
        for (name, value) in values {
            out = out.replace(&format!("{{{name}}}"), value);
        }
        assert!(!out.contains('{'), "unfilled placeholder in {out:?}");
        out
    }

    /// Byte-for-byte: every pinned refusal string in 04 is what the code produces.
    #[test]
    fn every_pinned_04_string_matches_the_frozen_doc() {
        let cases: Vec<(String, String)> = vec![
            (
                fill(
                    &bullet("manager unreachable at"),
                    &[("addr", "127.0.0.1:8756")],
                ),
                manager_unreachable("127.0.0.1:8756"),
            ),
            (
                fill(
                    &bullet("Hyprland not found"),
                    &[("resolved command", "dnf install hyprland")],
                ),
                hyprland_missing("dnf install hyprland"),
            ),
            (
                fill(
                    &bullet("is outside the supported range"),
                    &[("found", "0.41.0"), ("min", "0.40.0"), ("max", "0.43.0")],
                ),
                hyprland_unsupported("0.41.0", "0.40.0", "0.43.0"),
            ),
            (
                fill(&bullet("session refused"), &[("renderer", "llvmpipe")]),
                renderer_refused("llvmpipe"),
            ),
            (
                fill(
                    &bullet("refusing to bind"),
                    &[
                        ("addr", "0.0.0.0:8756"),
                        ("path", "/data/daiorama/default/token"),
                    ],
                ),
                bind_refused("0.0.0.0:8756", "/data/daiorama/default/token"),
            ),
            (
                fill(
                    &bullet("containment unavailable"),
                    &[("detail", "no cgroup2 fs")],
                ),
                containment_unavailable("no cgroup2 fs"),
            ),
            (
                fill(&bullet("token required for"), &[("addr", "10.0.0.9:8756")]),
                token_required("10.0.0.9:8756"),
            ),
            (
                fill(
                    &bullet("session limit reached"),
                    &[
                        ("scope", "operator"),
                        ("n", "4"),
                        ("max", "4"),
                        ("g", "16"),
                        ("gmax", "16"),
                    ],
                ),
                session_limit_reached("operator", 4, 4, 16, 16),
            ),
            (
                fill(
                    &bullet("is exclusive"),
                    &[("id", "0f8d9a34-1c2b-4e5f-8a90-112233445566")],
                ),
                backend_busy("0f8d9a34-1c2b-4e5f-8a90-112233445566"),
            ),
            (
                fill(&bullet("draining for upgrade"), &[("n", "2")]),
                draining(2),
            ),
            (
                fill(
                    &bullet("protocol mismatch"),
                    &[
                        ("cv", "0.1.0"),
                        ("cm", "0.1.0"),
                        ("mv", "0.2.0"),
                        ("mm", "0.2.0"),
                    ],
                ),
                protocol_mismatch("0.1.0", "0.1.0", "0.2.0", "0.2.0"),
            ),
            (
                fill(&bullet("scope required"), &[]),
                SCOPE_REQUIRED.to_string(),
            ),
            (
                fill(&bullet("module not installed"), &[]),
                MODULE_NOT_INSTALLED.to_string(),
            ),
            (
                fill(
                    &bullet("view unavailable for"),
                    &[("session", "7f2b"), ("reason", "wayvnc is not usable")],
                ),
                view_unavailable("7f2b", "wayvnc is not usable"),
            ),
            (
                fill(&bullet("no session specified"), &[]),
                NO_SESSION_SPECIFIED.to_string(),
            ),
        ];

        for (expected, produced) in cases {
            assert_eq!(expected, produced, "pinned 04 string drifted");
        }
    }

    /// 04 wraps prose across lines; whitespace-collapsed comparison is the honest one.
    fn doc_flat() -> String {
        DOC_04.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The cold answer and the not-packaged Hyprland rendering are pinned too.
    #[test]
    fn answers_and_variants_match_the_doc() {
        let doc = doc_flat();
        assert!(doc.contains("`no manager running` — cold commands' honest answer."));
        assert_eq!(NO_MANAGER_RUNNING, "no manager running");
        assert!(doc.contains(HYPRLAND_NOT_PACKAGED));
    }

    /// 04 pins the lazy-start report line and the `(code)` suffix form.
    #[test]
    fn report_line_and_error_form() {
        let doc = doc_flat();
        assert_eq!(
            "started manager (mode=background, pid=4242) socket=/run/user/1000/daiorama/default/manager.sock",
            started_manager(
                "background",
                4242,
                "/run/user/1000/daiorama/default/manager.sock"
            )
        );
        assert!(doc.contains(
            "it prints one line: `started manager (mode=<attached|background>, pid=<pid>) socket=<path>`"
        ));
        assert_eq!(
            "session not found (session_not_found)",
            error_line("session not found", "session_not_found")
        );
        assert_eq!(
            "hint: create a session first",
            hint_line(HINT_CREATE_A_SESSION_FIRST)
        );
    }
}
