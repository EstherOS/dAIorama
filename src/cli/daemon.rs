//! `daiorama daemon start|end` — the detached manager lifecycle (04/10).
//!
//! `start` is an explicit process runner (neither-class, 04): it spawns `serve` detached — a
//! new session, stdio to the data-dir log — and reports readiness on the pinned line. `end` is
//! cold: with no manager it is the cold answer; with live sessions it refuses and lists them;
//! `--force` stops the manager and states the drop ("it is a session drop, by design" — 10).
//!
//! Provisional wording where the specs pin none (additive, flagged as such): the end
//! completion lines, the refusal envelope's message/hint, and the daemon log path
//! (`<data-root>/daemon.log`).

use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::ipc::{self, Address, Client, ErrorCode, ErrorEnvelope, methods};
use crate::strings;

use super::{
    Answer, App, Class, START_DEADLINE, START_POLL, manager_program, render_session, start_failure,
};

/// How long `end` waits for the manager to go away after SIGTERM (provisional bound).
pub const STOP_DEADLINE: Duration = Duration::from_secs(5);

/// Start the manager detached (04: "survives shell exit"). If one already serves, the
/// command's outcome already holds and it reports that manager.
pub async fn start(app: &App) -> Result<Answer, ErrorEnvelope> {
    let socket = ipc::socket_path(&app.instance);
    if let Some(status) = app.call(Class::Cold, methods::STATUS, None).await? {
        let pid = status["pid"].as_u64().unwrap_or(0) as u32;
        return Ok(Answer::from_lines(vec![strings::started_manager(
            "background",
            pid,
            &app.instance_socket_display(&socket),
        )]));
    }

    let paths = app.paths();
    std::fs::create_dir_all(&paths.data_dir)
        .map_err(|error| ErrorEnvelope::internal(format!("data dir: {error}")))?;
    let log_path = paths.data_dir.join("daemon.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| ErrorEnvelope::internal(format!("daemon log: {error}")))?;

    let mut command = std::process::Command::new(manager_program());
    command
        .arg("serve")
        .arg("--instance")
        .arg(&app.instance)
        .env("DAIORAMA_MODE", "background")
        .stdin(Stdio::null())
        .stdout(
            log.try_clone()
                .map_err(|error| ErrorEnvelope::internal(format!("daemon log: {error}")))?,
        )
        .stderr(log);
    // Detach: a new session so a shell exit cannot take the manager down with it (04).
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| {
        ErrorEnvelope::internal(format!(
            "cannot start {}: {error}",
            manager_program().display()
        ))
    })?;

    let address = Address::Unix(socket.clone());
    let deadline = Instant::now() + START_DEADLINE;
    loop {
        if Client::probe(&address, Duration::from_millis(200)).await {
            return report_serving(app, &socket, child.id()).await;
        }
        if let Ok(Some(status)) = child.try_wait() {
            // A racing starter: this child may be the lock loser while the winner is still
            // coming up. One last probe before declaring failure.
            if Client::probe(&address, Duration::from_millis(200)).await {
                return report_serving(app, &socket, child.id()).await;
            }
            let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
            let tail: String = log_text
                .lines()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            return Err(start_failure(&tail).unwrap_or_else(|| {
                ErrorEnvelope::internal(format!(
                    "manager start failed ({status}); daemon log: {}",
                    log_path.display()
                ))
            }));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(ErrorEnvelope::internal(format!(
                "manager did not answer within {}s of the detached start; see {}",
                START_DEADLINE.as_secs(),
                log_path.display()
            )));
        }
        tokio::time::sleep(START_POLL).await;
    }
}

/// Report readiness with the pid of the manager that actually serves. A racing starter's
/// child may be the lock loser (it exits); the serving pid comes from `status`, never from
/// the process this command happened to spawn.
async fn report_serving(
    app: &App,
    socket: &std::path::Path,
    fallback: u32,
) -> Result<Answer, ErrorEnvelope> {
    let pid = match app.call(Class::Cold, methods::STATUS, None).await? {
        Some(status) => status["pid"]
            .as_u64()
            .map(|pid| pid as u32)
            .unwrap_or(fallback),
        None => fallback,
    };
    Ok(Answer::from_lines(vec![strings::started_manager(
        "background",
        pid,
        &app.instance_socket_display(socket),
    )]))
}

/// Stop the manager (04: plain `end` refuses while sessions are live and lists them; `--force`
/// stops it and states the drop — 10).
pub async fn end(app: &App, force: bool) -> Result<Answer, ErrorEnvelope> {
    let Some(status) = app.call(Class::Cold, methods::STATUS, None).await? else {
        return Ok(Answer::from_lines(vec![
            strings::NO_MANAGER_RUNNING.to_string(),
        ]));
    };
    let pid = status["pid"].as_u64().unwrap_or(0) as u32;

    let sessions = app
        .call(Class::Cold, methods::LIST_SESSIONS, None)
        .await?
        .unwrap_or_else(|| json!({ "sessions": [] }));
    let live: Vec<Value> = sessions["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|session| {
            matches!(
                session["state"].as_str(),
                Some("running") | Some("degraded")
            )
        })
        .collect();
    let listed: Vec<String> = live.iter().flat_map(render_session).collect();

    if !force && !live.is_empty() {
        // Refuses and lists them: the listing on stdout, the envelope on stderr.
        if !app.json {
            for line in &listed {
                println!("{line}");
            }
        }
        return Err(end_refusal(live.len()));
    }

    let socket = ipc::socket_path(&app.instance);
    let address = Address::Unix(socket);
    let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if rc != 0 {
        return Err(ErrorEnvelope::internal(format!(
            "stopping manager (pid {pid}): {}",
            std::io::Error::last_os_error()
        )));
    }
    let deadline = Instant::now() + STOP_DEADLINE;
    while Client::probe(&address, Duration::from_millis(100)).await {
        if Instant::now() >= deadline {
            return Err(ErrorEnvelope::internal(format!(
                "manager (pid {pid}) did not stop within {}s",
                STOP_DEADLINE.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let mut out = Vec::new();
    if force && !listed.is_empty() {
        out.push(format!(
            "stopped manager (pid {pid}) — dropped {} session(s):",
            live.len()
        ));
        out.extend(listed);
    } else {
        out.push(format!("stopped manager (pid {pid})"));
    }
    Ok(Answer::from_lines(out))
}

/// The refusal envelope for a plain `end` against live sessions (provisional wording; the
/// listing itself travels on stdout).
pub(crate) fn end_refusal(live: usize) -> ErrorEnvelope {
    ErrorEnvelope {
        code: ErrorCode::BadRequest,
        message: format!(
            "refusing to stop the manager: {live} live session(s); --force stops it and drops them"
        ),
        hint: Some("close the sessions first, or pass --force".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_refusal_names_the_force_escape() {
        let envelope = end_refusal(3);
        assert_eq!(envelope.code, ErrorCode::BadRequest);
        assert!(
            envelope.message.contains("3 live session(s)"),
            "{envelope:?}"
        );
        assert!(envelope.message.contains("--force"), "{envelope:?}");
        assert_eq!(
            envelope.hint.as_deref(),
            Some("close the sessions first, or pass --force")
        );
    }
}
