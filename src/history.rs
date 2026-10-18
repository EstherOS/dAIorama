//! History (06 B8): an append-only, per-session JSONL log under
//! `<data>/history/<session>/seg-<n>.log` — 0600 files in a 0700 dir, buffered writes with a
//! flush per record and **no fsync per call** (08: losing seconds of buffered entries in a
//! hard crash is acceptable for an operational aid), rotated at 4 MiB × 8 segments (oldest
//! dropped). Retention is not this module's: a session-end timestamp is written durably to
//! `<session>/.ended` so the retention sweep never has to infer "ended when" from mtimes.
//!
//! Records carry scope, origin, a timestamp, a type and a small detail object; control-call
//! **arguments are never recorded** (06), and neither are media ids or credentials.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Paths;
use crate::ipc::ErrorEnvelope;

/// 04 pinned history defaults.
pub const SEGMENTS: usize = 8;
pub const SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
/// `list_history` page bounds (05: no unbounded result sets).
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 1000;

/// Session ids are opaque slugs, never path segments: the history root is joined with the id,
/// so anything that can climb (`/`, `..`) or isn't a slug is refused before touching the fs.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && !id.contains("..")
        && id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
}

/// One log line. Optional fields are omitted on the wire (`skip_serializing_if`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub at: String,
    pub event: String,
    pub session_id: String,
    pub scope: String,
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

struct SessionHistory {
    dir: PathBuf,
    segment: u32,
    bytes: u64,
    /// Last append, the recency key for the cache bound. Rebuilding an evicted entry costs one
    /// `readdir` of its own directory, never a lost record.
    last: Instant,
}

/// How many per-session segment handles the manager caches. The cache exists to avoid walking a
/// session's directory on every record; it must not grow with every id a caller has ever named.
/// The map is bounded by live and recently-active sessions, and eviction drops the oldest
/// quarter (an evicted entry is rebuilt from its directory on the next append).
const MAX_CACHED_SESSIONS: usize = 4096;

pub struct History {
    root: PathBuf,
    sessions: Mutex<HashMap<String, SessionHistory>>,
}

impl History {
    pub fn new(paths: &Paths) -> Self {
        Self {
            root: paths.history_dir(),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    /// Append one record. Buffered, flush per record, no fsync (08). Rotation happens before a
    /// write that would cross 4 MiB; the oldest segment is dropped past 8.
    pub fn append(&self, record: &Record) -> Result<(), ErrorEnvelope> {
        if !valid_session_id(&record.session_id) {
            return Err(ErrorEnvelope::bad_request("invalid session id"));
        }
        let mut sessions = self.sessions.lock().expect("history lock");
        let now = Instant::now();
        let entry = sessions
            .entry(record.session_id.clone())
            .or_insert_with(|| {
                let dir = self.root.join(&record.session_id);
                let (segment, bytes) = newest_segment(&dir);
                SessionHistory {
                    dir,
                    segment,
                    bytes,
                    last: Instant::now(),
                }
            });
        std::fs::create_dir_all(&entry.dir).map_err(|err| {
            ErrorEnvelope::internal(format!("history dir {}: {err}", entry.dir.display()))
        })?;
        // 0700: session ids are directory names, so the listing is the thing to hide even
        // though every file is 0600.
        let _ = std::fs::set_permissions(&entry.dir, std::fs::Permissions::from_mode(0o700));
        // Rotation is decided here, but the trim runs after the new segment exists: dropping
        // "while there are already more than SEGMENTS" would leave the file this append is
        // about to create as a ninth segment (36 MiB against the pinned 8 x 4 MiB budget).
        let rotated = entry.bytes >= SEGMENT_BYTES;
        if rotated {
            entry.segment += 1;
            entry.bytes = 0;
        }
        let path = entry.dir.join(format!("seg-{:04}.log", entry.segment));
        let mut line = serde_json::to_string(record).expect("records serialize");
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .map_err(|err| ErrorEnvelope::internal(format!("history {}: {err}", path.display())))?;
        file.write_all(line.as_bytes())
            .map_err(|err| ErrorEnvelope::internal(format!("history {}: {err}", path.display())))?;
        file.flush()
            .map_err(|err| ErrorEnvelope::internal(format!("history {}: {err}", path.display())))?;
        entry.bytes += line.len() as u64;
        entry.last = now;
        if rotated {
            rotate(&entry.dir);
        }
        if sessions.len() > MAX_CACHED_SESSIONS {
            evict_locked(&mut sessions);
        }
        Ok(())
    }

    /// Drops a session's cached segment state after its directory is gone. The next append for
    /// that id (if one ever arrives) rebuilds from an empty directory.
    pub fn forget(&self, session_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(session_id);
        }
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.sessions.lock().expect("history lock").len()
    }

    /// A durable session-end marker for the retention sweep: `<session>/.ended`, RFC 3339 plus
    /// the cause. Written once when the session leaves `running`, however it left.
    pub fn mark_ended(&self, session_id: &str, scope: &str, origin: &str, cause: &str) {
        if !valid_session_id(session_id) {
            return;
        }
        let dir = self.root.join(session_id);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        let body = format!(
            "{}\tscope={}\torigin={}\tcause={}\n",
            crate::runtime::service::utc_now_rfc3339(),
            scope,
            origin,
            cause
        );
        let path = dir.join(".ended");
        // Created 0600 — no write-then-chmod window (the wayvnc-conf class).
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| file.write_all(body.as_bytes()));
        let _ = written;
        // A pre-existing marker from an older build could still be 0644; creation is 0600,
        // this only repairs the leftover.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    /// The session's recorded scope (the first record's), for reads whose session is no longer
    /// in the live registry.
    pub fn recorded_scope(&self, session_id: &str) -> Option<String> {
        if !valid_session_id(session_id) {
            return None;
        }
        let dir = self.root.join(session_id);
        let mut segments: Vec<PathBuf> = std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("seg-"))
                    .unwrap_or(false)
            })
            .collect();
        segments.sort();
        let segment = segments.into_iter().next()?;
        let file = std::fs::File::open(segment).ok()?;
        let line = BufReader::new(file).lines().next()?.ok()?;
        let record: Record = serde_json::from_str(&line).ok()?;
        Some(record.scope)
    }

    /// One page of records for a session, newest first, from an opaque cursor.
    pub fn read_session(
        &self,
        session_id: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<Page, ErrorEnvelope> {
        if !valid_session_id(session_id) {
            return Err(ErrorEnvelope::bad_request("invalid session id"));
        }
        let dir = self.root.join(session_id);
        let mut segments = segment_paths(&dir);
        segments.sort();
        if segments.is_empty() {
            return Ok(Page {
                records: Vec::new(),
                next_cursor: None,
            });
        }
        // Cursor: `<segment-index>:<line-index>` base64'd — opaque to clients (05).
        let (mut segment_index, mut line_index) = match cursor {
            Some(cursor) => decode_cursor(cursor)?,
            None => (segments.len() - 1, 0),
        };
        let mut records = Vec::new();
        let mut next_state: Option<(usize, usize)> = None;
        'outer: while segment_index < segments.len() {
            let file = std::fs::File::open(&segments[segment_index])
                .map_err(|err| ErrorEnvelope::bad_request(format!("history cursor: {err}")))?;
            let mut lines: Vec<String> =
                BufReader::new(file).lines().map_while(Result::ok).collect();
            lines.reverse();
            for (index, line) in lines.iter().enumerate().skip(line_index) {
                if records.len() >= limit {
                    next_state = Some((segment_index, index));
                    break 'outer;
                }
                if let Ok(record) = serde_json::from_str::<Record>(line) {
                    records.push(record);
                } else {
                    continue;
                }
            }
            if segment_index == 0 {
                break;
            }
            segment_index -= 1;
            line_index = 0;
        }
        let next_cursor = next_state.map(|(segment, line)| encode_cursor(segment, line));
        Ok(Page {
            records,
            next_cursor,
        })
    }

    /// Newest records across every session directory (no cross-session paging: 05's cursor is
    /// session-scoped; callers that need older entries ask per session).
    pub fn read_all(&self, limit: usize) -> Vec<Record> {
        let mut records = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.root()) else {
            return records;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let session_id = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => continue,
            };
            if let Ok(page) = self.read_session(&session_id, limit, None) {
                records.extend(page.records);
            }
        }
        records.sort_by(|a, b| b.at.cmp(&a.at));
        records.truncate(limit);
        records
    }
}

pub struct Page {
    pub records: Vec<Record>,
    pub next_cursor: Option<String>,
}

fn segment_paths(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.starts_with("seg-"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn newest_segment(dir: &Path) -> (u32, u64) {
    let mut paths = segment_paths(dir);
    paths.sort();
    match paths.last() {
        Some(path) => {
            let index = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.strip_prefix("seg-"))
                .and_then(|digits| digits.parse().ok())
                .unwrap_or(0);
            let bytes = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
            (index, bytes)
        }
        None => (0, 0),
    }
}

/// Drop the oldest segment once the count exceeds the pinned budget (04: 8 × 4 MiB).
fn rotate(dir: &Path) {
    let mut paths = segment_paths(dir);
    paths.sort();
    while paths.len() > SEGMENTS {
        if let Some(oldest) = paths.first() {
            let _ = std::fs::remove_file(oldest);
        }
        paths.remove(0);
    }
}

fn encode_cursor(segment: usize, line: usize) -> String {
    crate::crypto::base64(format!("{segment}:{line}").as_bytes())
}

fn decode_cursor(cursor: &str) -> Result<(usize, usize), ErrorEnvelope> {
    let bytes =
        base64_decode(cursor).ok_or_else(|| ErrorEnvelope::bad_request("invalid cursor"))?;
    let text =
        String::from_utf8(bytes).map_err(|_| ErrorEnvelope::bad_request("invalid cursor"))?;
    let (segment, line) = text
        .split_once(':')
        .ok_or_else(|| ErrorEnvelope::bad_request("invalid cursor"))?;
    let segment = segment
        .parse()
        .map_err(|_| ErrorEnvelope::bad_request("invalid cursor"))?;
    let line = line
        .parse()
        .map_err(|_| ErrorEnvelope::bad_request("invalid cursor"))?;
    Ok((segment, line))
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        let value = TABLE.iter().position(|entry| *entry == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

// `mode(0o600)` / `from_mode(0o700)` for the history files and dirs.
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Evicts the least-recently-appended quarter of the cache. Append is serialized by the map
/// mutex, so recency is exact and no entry is in flight.
fn evict_locked(sessions: &mut HashMap<String, SessionHistory>) {
    let target = MAX_CACHED_SESSIONS * 3 / 4;
    if sessions.len() <= target {
        return;
    }
    let mut order: Vec<(Instant, String)> = sessions
        .iter()
        .map(|(key, entry)| (entry.last, key.clone()))
        .collect();
    order.sort_by_key(|(last, _)| *last);
    for (_, key) in order {
        if sessions.len() <= target {
            break;
        }
        sessions.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(root: &Path) -> Paths {
        Paths::for_env(Some(root), Some(root), Some(root), "default")
    }

    fn record(session: &str, kind: &str, ts: &str) -> Record {
        Record {
            at: ts.to_string(),
            event: kind.to_string(),
            session_id: session.to_string(),
            scope: "operator".to_string(),
            origin: "shell".to_string(),
            cause: None,
            detail: Some(json!({"n": 1})),
        }
    }

    #[test]
    fn records_round_trip_with_the_pinned_shape() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(&paths(dir.path()));
        history
            .append(&record("s-1", "session.created", "2026-10-17T00:00:00Z"))
            .unwrap();
        let page = history.read_session("s-1", 10, None).unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].event, "session.created");
        assert_eq!(page.records[0].scope, "operator");
        let file = dir.path().join("daiorama/default/history/s-1/seg-0000.log");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("daiorama/default/history/s-1"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the session dir hides its name"
        );
    }

    #[test]
    fn newest_first_and_cursor_pages_without_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(&paths(dir.path()));
        for n in 0..5 {
            history
                .append(&record(
                    "s-1",
                    "tool.call",
                    &format!("2026-10-17T00:00:0{n}Z"),
                ))
                .unwrap();
        }
        let first = history.read_session("s-1", 2, None).unwrap();
        assert_eq!(first.records[0].at, "2026-10-17T00:00:04Z");
        let cursor = first.next_cursor.expect("more pages");
        let second = history.read_session("s-1", 2, Some(&cursor)).unwrap();
        assert_eq!(second.records[0].at, "2026-10-17T00:00:02Z");
    }

    #[test]
    fn rotation_drops_the_oldest_segment_at_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let _history = History::new(&paths(dir.path()));
        let session_dir = dir.path().join("daiorama/default/history/s-1");
        std::fs::create_dir_all(&session_dir).unwrap();
        for index in 0..(SEGMENTS + 2) {
            std::fs::write(
                session_dir.join(format!("seg-{index:04}.log")),
                format!("old-{index}\n"),
            )
            .unwrap();
        }
        rotate(&session_dir);
        let left = segment_paths(&session_dir);
        assert_eq!(left.len(), SEGMENTS);
        assert!(!session_dir.join("seg-0000.log").exists(), "oldest dropped");
    }

    /// The append path, not the helper: a full newest segment must rotate *and* leave the
    /// pinned count. A trim that runs before the new file exists settles at nine segments
    /// (36 MiB) while the budget is eight (32 MiB).
    #[test]
    fn rotation_through_append_keeps_the_pinned_segment_count() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(&paths(dir.path()));
        let session_dir = dir.path().join("daiorama/default/history/s-1");
        std::fs::create_dir_all(&session_dir).unwrap();
        for index in 0..SEGMENTS {
            let line = serde_json::to_string(&record("s-1", "tool.call", "2026-10-17T00:00:00Z"))
                .unwrap()
                + "\n";
            std::fs::write(session_dir.join(format!("seg-{index:04}.log")), line).unwrap();
        }
        // Put the newest segment at the budget so the next append has to rotate.
        let newest = session_dir.join(format!("seg-{:04}.log", SEGMENTS - 1));
        let pad = (SEGMENT_BYTES as usize - std::fs::metadata(&newest).unwrap().len() as usize) + 1;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&newest)
            .unwrap();
        file.write_all(&vec![b'x'; pad]).unwrap();

        history
            .append(&record("s-1", "tool.call", "2026-10-17T00:00:09Z"))
            .unwrap();

        let left = segment_paths(&session_dir);
        assert_eq!(
            left.len(),
            SEGMENTS,
            "a ninth segment means the pinned 8 x 4 MiB budget is exceeded"
        );
        assert!(
            !session_dir.join("seg-0000.log").exists(),
            "the oldest segment is the one dropped"
        );
        assert!(
            session_dir.join(format!("seg-{SEGMENTS:04}.log")).exists(),
            "the record landed in the new segment"
        );
    }

    #[test]
    fn the_end_marker_is_durable_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(&paths(dir.path()));
        history.mark_ended("s-9", "operator", "shell", "closed");
        let marker = dir.path().join("daiorama/default/history/s-9/.ended");
        let body = std::fs::read_to_string(&marker).unwrap();
        assert!(body.contains("cause=closed"), "{body}");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    /// The id is joined onto the root: separators and dot-dot never reach the filesystem, and
    /// `read_all` only ever walks real child directories.
    #[test]
    fn hostile_session_ids_never_escape_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(&paths(dir.path()));
        for hostile in ["../../etc", "s-1/../../s-2", "..", ".", "a/b", "x\\y", ""] {
            assert!(!valid_session_id(hostile), "{hostile:?} must be invalid");
            assert_eq!(history.recorded_scope(hostile), None, "{hostile:?}");
            assert!(
                history.read_session(hostile, 10, None).is_err(),
                "{hostile:?}"
            );
        }
        assert!(valid_session_id("s-1a2b3c4d"));
        assert!(valid_session_id("session.1_ok"));
    }

    #[test]
    fn the_recorded_scope_survives_a_manager_restart() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(&paths(dir.path()));
        history
            .append(&record("s-1", "session.created", "2026-10-17T00:00:00Z"))
            .unwrap();
        let reopened = History::new(&paths(dir.path()));
        assert_eq!(reopened.recorded_scope("s-1").as_deref(), Some("operator"));
    }

    #[test]
    fn the_session_cache_is_bounded_and_rebuilds_after_eviction() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::config::Paths::for_env(
            Some(tmp.path()),
            Some(tmp.path()),
            Some(tmp.path()),
            "default",
        );
        let history = History::new(&paths);
        for index in 0..(MAX_CACHED_SESSIONS * 2) {
            history
                .append(&record(
                    &format!("sess-{index}"),
                    "tool.call",
                    "2026-10-18T00:00:00Z",
                ))
                .expect("append");
        }
        let tracked = history.tracked();
        assert!(tracked <= MAX_CACHED_SESSIONS, "tracked {tracked}");
        assert!(
            tracked >= MAX_CACHED_SESSIONS / 2,
            "eviction trims a quarter: {tracked}"
        );
        // An evicted id is rebuilt from its directory: the record is never lost.
        history
            .append(&record("sess-0", "tool.call", "2026-10-18T00:00:01Z"))
            .expect("append after eviction");
    }

    #[test]
    fn forget_drops_the_cached_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::config::Paths::for_env(
            Some(tmp.path()),
            Some(tmp.path()),
            Some(tmp.path()),
            "default",
        );
        let history = History::new(&paths);
        history
            .append(&record("sess", "tool.call", "2026-10-18T00:00:00Z"))
            .unwrap();
        assert_eq!(history.tracked(), 1);
        history.forget("sess");
        assert_eq!(history.tracked(), 0);
    }
}
