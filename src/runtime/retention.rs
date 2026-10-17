//! Retention (04/08): a session's history and persisted media are removed once it has been
//! ended for `history.retention` (default seven days, 04). The `.ended` sidecar is the only
//! clock — mtimes are never read — so a missing or unparsable marker is never an expiry: the
//! startup pass adopts such directories (`cause=manager_restart`) instead of deleting on a
//! guess, and a session the live registry still knows is kept regardless of its marker.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How often a running manager re-runs the pass. Not a config key: one directory scan per hour
/// is bounded by the session count, and the window is measured in days.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// One pass's outcome — names only, for the startup lines and tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub removed: Vec<String>,
    pub kept: Vec<String>,
    pub failed: Vec<String>,
}

impl SweepReport {
    /// Whether the pass wrote or removed anything worth printing.
    pub fn changed(&self) -> bool {
        !self.removed.is_empty() || !self.failed.is_empty()
    }
}

/// Session ids under `history_root`: directory names that pass the slug gate, sorted. A name
/// that is not a slug is not a session and is skipped before any path join.
pub fn session_dirs(history_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(history_root) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| crate::history::valid_session_id(name))
        .collect();
    ids.sort();
    ids
}

/// Unix seconds from `<history_root>/<id>/.ended`, when the marker exists and its first field
/// parses. `None` means "do not age this directory".
pub fn marker_secs(history_root: &Path, id: &str) -> Option<i64> {
    let text = std::fs::read_to_string(history_root.join(id).join(".ended")).ok()?;
    // The marker is one line of tab-separated fields: `<ts>\tscope=…\torigin=…\tcause=…`.
    parse_rfc3339_secs(text.split_whitespace().next()?)
}

/// One retention pass over `history_root` and `media_root`. `live` answers whether this
/// manager still knows the session (never swept); `retention_secs` is the configured window
/// (`history.retention`, 08) in seconds; `now_secs` is injectable so tests pin the clock.
pub fn sweep(
    history_root: &Path,
    media_root: &Path,
    live: &dyn Fn(&str) -> bool,
    retention_secs: u64,
    now_secs: i64,
) -> SweepReport {
    let mut report = SweepReport::default();
    for id in session_dirs(history_root) {
        if live(&id) {
            report.kept.push(id);
            continue;
        }
        let Some(ended) = marker_secs(history_root, &id) else {
            // No marker, no deletion: an unmarked directory is not evidence of an old session.
            report.kept.push(id);
            continue;
        };
        let age = now_secs.saturating_sub(ended);
        if age < 0 || (age as u64) < retention_secs {
            report.kept.push(id);
            continue;
        }
        // Media first: if this process dies between the two removals the marker is still
        // there and the next pass retries. The reverse order would strand a media directory
        // with no clock left to age it.
        match std::fs::remove_dir_all(media_root.join(&id)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                report.failed.push(id);
                continue;
            }
        }
        match std::fs::remove_dir_all(history_root.join(&id)) {
            Ok(()) => report.removed.push(id),
            // Media is gone; the marker survives, so the next pass retries the history half.
            Err(_) => report.failed.push(id),
        }
    }
    report
}

/// Unix seconds for a `YYYY-MM-DDTHH:MM:SSZ` marker (the shape `mark_ended` writes). The bare
/// `Z` form only: anything else is `None` → the directory is kept, never deleted on a guess.
pub fn parse_rfc3339_secs(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'Z' {
        return None;
    }
    let number = |start: usize, len: usize| -> Option<i64> {
        let slice = text.get(start..start + len)?;
        if !slice.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        slice.parse().ok()
    };
    let (year, month, day) = (number(0, 4)?, number(5, 2)?, number(8, 2)?);
    let (hour, minute, second) = (number(11, 2)?, number(14, 2)?, number(17, 2)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm; the inverse of `civil_from_days`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = month as i64;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Wall clock seconds now — one place, so the caller and tests agree on the clock's shape.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn stamp(secs: i64) -> String {
        // Build the marker text through the same civil algorithm the parser inverts.
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil(days);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    fn civil(z: i64) -> (i64, u32, u32) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let year = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if month <= 2 { year + 1 } else { year }, month, day)
    }

    fn session(history: &Path, id: &str, ended_secs: Option<i64>) {
        let dir = history.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(secs) = ended_secs {
            std::fs::write(
                dir.join(".ended"),
                format!("{}\tcause=closed\n", stamp(secs)),
            )
            .unwrap();
        }
    }

    fn roots() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let history = tmp.path().join("history");
        let media = tmp.path().join("media");
        (tmp, history, media)
    }

    const WEEK: u64 = 7 * 86_400;

    #[test]
    fn the_marker_round_trips_through_the_parser() {
        let now = 1_792_000_000;
        assert_eq!(parse_rfc3339_secs(&stamp(now)), Some(now));
        assert_eq!(parse_rfc3339_secs("not a timestamp"), None);
        assert_eq!(parse_rfc3339_secs("2026-10-15T00:00:00+00:00"), None);
    }

    #[test]
    fn expired_sessions_lose_history_and_media_and_fresh_ones_stay() {
        let (_tmp, history, media) = roots();
        let now = 1_792_000_000;
        session(&history, "old", Some(now - 8 * 86_400));
        session(&history, "fresh", Some(now - 6 * 86_400));
        session(&history, "unmarked", None);
        std::fs::create_dir_all(media.join("old")).unwrap();
        std::fs::write(media.join("old").join("m-1.png"), b"png").unwrap();
        std::fs::create_dir_all(media.join("fresh")).unwrap();
        std::fs::create_dir_all(media.join("orphan")).unwrap();

        let report = sweep(&history, &media, &|_| false, WEEK, now);

        assert_eq!(report.removed, vec!["old".to_string()]);
        assert!(report.failed.is_empty());
        assert!(!history.join("old").exists());
        assert!(!media.join("old").exists());
        assert!(history.join("fresh").exists());
        assert!(media.join("fresh").exists());
        assert!(history.join("unmarked").exists());
        assert!(media.join("orphan").exists(), "no marker, no deletion");
        assert_eq!(report.kept.len(), 2);
    }

    #[test]
    fn the_window_is_exact_at_the_boundary() {
        let (_tmp, history, media) = roots();
        let now = 1_792_000_000;
        session(&history, "exactly", Some(now - 7 * 86_400));
        session(&history, "one-second-short", Some(now - 7 * 86_400 + 1));

        let report = sweep(&history, &media, &|_| false, WEEK, now);

        assert_eq!(report.removed, vec!["exactly".to_string()]);
        assert!(history.join("one-second-short").exists());
    }

    #[test]
    fn a_live_session_is_never_swept() {
        let (_tmp, history, media) = roots();
        let now = 1_792_000_000;
        session(&history, "running", Some(now - 30 * 86_400));
        let report = sweep(&history, &media, &|id| id == "running", WEEK, now);
        assert!(report.removed.is_empty());
        assert!(history.join("running").exists());
    }

    #[test]
    fn a_future_marker_is_kept_and_a_corrupt_one_is_not_a_deletion() {
        let (_tmp, history, media) = roots();
        let now = 1_792_000_000;
        session(&history, "skewed", Some(now + 86_400));
        session(&history, "corrupt", None);
        std::fs::write(history.join("corrupt").join(".ended"), b"garbage\n").unwrap();

        let report = sweep(&history, &media, &|_| false, WEEK, now);

        assert!(report.removed.is_empty());
        assert_eq!(
            report.kept,
            vec!["corrupt".to_string(), "skewed".to_string()]
        );
        assert!(history.join("corrupt").exists());
    }

    #[test]
    fn a_hostile_directory_name_is_not_a_session() {
        let (_tmp, history, media) = roots();
        std::fs::create_dir_all(history.join("..")).ok();
        let not_a_slug = history.join("not a session");
        std::fs::create_dir_all(&not_a_slug).unwrap();
        std::fs::write(not_a_slug.join(".ended"), stamp(0)).unwrap();
        let report = sweep(&history, &media, &|_| false, WEEK, 1_792_000_000);
        assert!(report.removed.is_empty());
        assert!(
            not_a_slug.exists(),
            "a name outside the slug gate is not a session"
        );
    }
}
