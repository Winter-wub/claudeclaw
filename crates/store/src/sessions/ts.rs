//! The TS session files, read and written in their exact TS shapes (architecture §4 "Sessions",
//! §9.1 owner S; #14 rules 1–2 and 7, #44).
//!
//! | TS key | Session | Fallback session |
//! |---|---|---|
//! | [`TsKey::Global`] | `session.json` | `session_fallback.json` |
//! | [`TsKey::Thread`] | `sessions.json` `threads[<key>]` | `fallback-sessions/<encodeURIComponent(key)>.json` |
//! | [`TsKey::Agent`] | `agents/<agent>/session.json` | `agents/<agent>/session_fallback.json` |
//!
//! (`src/sessions.ts:24-27`, `src/sessions.ts:134-138`, `src/sessionManager.ts:5`.) Every write
//! is `JSON.stringify(value, null, 2) + "\n"`, as TS writes it (`src/sessions.ts:58`,
//! `src/sessionManager.ts:35`), and patches the entry as read, so keys TS or a later build added
//! are kept. Backups are `session_<N>.backup`, numbered from the highest existing N
//! (`src/sessions.ts:196-218`).
//!
//! These readings are the permanent drop-in contract (§8 "What stays"); mapping a Scope to a TS
//! key is the rollback mirror's ([`crate::rollback::session_mirror`]).

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};

use super::SessionsError;
use crate::atomic::{self, DirMode, FileMode};
use crate::error::StoreError;
use crate::layout::Layout;

/// Where TS keeps a session: TS's `agentName` / `threadId` pair, with a thread winning over an
/// agent as in `src/runner.ts:1059-1061` and `src/sessions.ts:134-138`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TsKey {
    /// No thread and no agent: `session.json`.
    Global,
    /// A `sessions.json` thread key, exactly as TS spells it: a bare snowflake, a job name or
    /// `agent:<agent>`.
    Thread(String),
    /// An agent with no thread: `agents/<agent>/session.json`.
    Agent(String),
}

/// One TS session record: `GlobalSession` (`src/sessions.ts:9-16`) or `ThreadSession`
/// (`src/sessionManager.ts:7-14`). The typed fields are the ones TS reads; every other key of
/// the record is kept and written back.
#[derive(Clone, PartialEq)]
pub struct TsSession {
    /// `sessionId`; a record without a non-empty one is absent to TS
    /// (`src/sessionValidate.ts:2-6`).
    pub session_id: String,
    /// `threadId`, which TS sets on thread records only (`src/sessionManager.ts:62-64`).
    pub thread_id: Option<String>,
    /// `createdAt`; `None` when it isn't an ISO date, in which case the value read is kept.
    pub created_at: Option<DateTime<Utc>>,
    /// `lastUsedAt`, like `created_at`.
    pub last_used_at: Option<DateTime<Utc>>,
    /// `turnCount`; 0 when it isn't a number (`src/sessions.ts:68`).
    pub turn_count: u64,
    /// `compactWarned`; false when it isn't a boolean (`src/sessions.ts:69`).
    pub compact_warned: bool,
    /// `messageCount`, the global session's rotation counter (`src/sessions.ts:104-109`);
    /// `None` writes no key.
    pub message_count: Option<u64>,
    /// The record as read.
    raw: Map<String, Value>,
}

impl TsSession {
    /// A record as TS's `createSession` writes it (`src/sessions.ts:78-86`).
    pub fn new(session_id: impl Into<String>, now: DateTime<Utc>) -> Self {
        TsSession {
            session_id: session_id.into(),
            thread_id: None,
            created_at: Some(now),
            last_used_at: Some(now),
            turn_count: 0,
            compact_warned: false,
            message_count: None,
            raw: Map::new(),
        }
    }

    /// Reads a record the way TS does; `None` when TS would treat it as absent.
    pub fn from_value(value: &Value) -> Option<Self> {
        let raw = value.as_object()?;
        let session_id = raw.get("sessionId")?.as_str().filter(|s| !s.is_empty())?;
        Some(TsSession {
            session_id: session_id.to_owned(),
            thread_id: raw
                .get("threadId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            created_at: raw.get("createdAt").and_then(parse_time),
            last_used_at: raw.get("lastUsedAt").and_then(parse_time),
            turn_count: raw.get("turnCount").and_then(as_count).unwrap_or(0),
            compact_warned: raw
                .get("compactWarned")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            message_count: raw.get("messageCount").and_then(as_count),
            raw: raw.clone(),
        })
    }

    /// The record in TS shape: the keys as read, patched in place. A new record's keys come in
    /// TS's order.
    pub fn to_value(&self) -> Value {
        let mut map = self.raw.clone();
        map.insert("sessionId".into(), self.session_id.clone().into());
        if let Some(thread_id) = &self.thread_id {
            map.insert("threadId".into(), thread_id.clone().into());
        }
        if let Some(at) = self.created_at {
            map.insert("createdAt".into(), iso(at).into());
        }
        if let Some(at) = self.last_used_at {
            map.insert("lastUsedAt".into(), iso(at).into());
        }
        map.insert("turnCount".into(), self.turn_count.into());
        map.insert("compactWarned".into(), self.compact_warned.into());
        match self.message_count {
            Some(n) => map.insert("messageCount".into(), n.into()),
            None => map.remove("messageCount"),
        };
        Value::Object(map)
    }
}

impl fmt::Debug for TsSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TsSession")
            .field("session_id", &self.session_id)
            .field("thread_id", &self.thread_id)
            .field("created_at", &self.created_at)
            .field("last_used_at", &self.last_used_at)
            .field("turn_count", &self.turn_count)
            .field("compact_warned", &self.compact_warned)
            .field("message_count", &self.message_count)
            .field("keys", &self.raw.len())
            .finish()
    }
}

/// `new Date().toISOString()`: UTC with milliseconds and `Z`.
fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_time(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// A JSON number as a count. TS keeps whatever number it finds; a fraction or a negative one is
/// truncated into range here.
fn as_count(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|f| f.max(0.0) as u64))
}

// ---- Sessions --------------------------------------------------------------------------------

/// Reads the session TS keeps under `key`; `None` when there is none, or when TS would treat
/// the file as absent (unparseable JSON, no `sessionId`; `src/sessions.ts:29-54`,
/// `src/sessionManager.ts:22-31`).
pub fn read(layout: &Layout, key: &TsKey) -> Result<Option<TsSession>, SessionsError> {
    match key {
        TsKey::Global => read_record(&layout.session()),
        TsKey::Agent(agent) => read_record(&layout.agent_session(agent_name(agent)?)),
        TsKey::Thread(thread) => Ok(read_threads(layout)?
            .get("threads")
            .and_then(|t| t.get(thread))
            .and_then(TsSession::from_value)),
    }
}

/// Writes `session` as TS's session under `key`. A thread record gets `threadId` set to its key,
/// as `createThreadSession` does (`src/sessionManager.ts:60-71`); the rest of `sessions.json` is
/// kept.
pub fn write(layout: &Layout, key: &TsKey, session: &TsSession) -> Result<(), SessionsError> {
    match key {
        TsKey::Global => write_json(&layout.session(), &session.to_value()),
        TsKey::Agent(agent) => write_json(
            &layout.agent_session(agent_name(agent)?),
            &session.to_value(),
        ),
        TsKey::Thread(thread) => {
            let mut record = session.clone();
            record.thread_id = Some(thread.clone());
            let mut file = read_threads(layout)?;
            threads_mut(&mut file).insert(thread.clone(), record.to_value());
            write_json(&layout.sessions(), &Value::Object(file))
        }
    }
}

/// Removes TS's session under `key`, as `resetSession` and `removeThreadSession` do
/// (`src/sessions.ts:119-126`, `src/sessionManager.ts:74-79`). Returns whether there was one.
pub fn remove(layout: &Layout, key: &TsKey) -> Result<bool, SessionsError> {
    match key {
        TsKey::Global => remove_file(&layout.session()),
        TsKey::Agent(agent) => remove_file(&layout.agent_session(agent_name(agent)?)),
        TsKey::Thread(thread) => {
            let mut file = read_threads(layout)?;
            if threads_mut(&mut file).remove(thread).is_none() {
                return Ok(false);
            }
            write_json(&layout.sessions(), &Value::Object(file))?;
            Ok(true)
        }
    }
}

/// Every thread record TS would list: the ones with a valid `sessionId`, in file order, with
/// their keys (`src/sessionManager.ts:102-105`).
pub fn list_threads(layout: &Layout) -> Result<Vec<(String, TsSession)>, SessionsError> {
    let file = read_threads(layout)?;
    let Some(Value::Object(threads)) = file.get("threads") else {
        return Ok(Vec::new());
    };
    Ok(threads
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), TsSession::from_value(v)?)))
        .collect())
}

/// `sessions.json` as TS loads it: `{"threads": {}}` when it is missing or unparseable
/// (`src/sessionManager.ts:22-31`).
fn read_threads(layout: &Layout) -> Result<Map<String, Value>, SessionsError> {
    match read_json(&layout.sessions())? {
        Some(Value::Object(file)) => Ok(file),
        _ => Ok(empty_threads()),
    }
}

fn empty_threads() -> Map<String, Value> {
    let mut file = Map::new();
    file.insert("threads".into(), Value::Object(Map::new()));
    file
}

fn threads_mut(file: &mut Map<String, Value>) -> &mut Map<String, Value> {
    let threads = file
        .entry("threads")
        .or_insert_with(|| Value::Object(Map::new()));
    if !threads.is_object() {
        *threads = Value::Object(Map::new());
    }
    threads.as_object_mut().expect("just made an object")
}

// ---- Fallback sessions -----------------------------------------------------------------------

/// The fallback session's file for `key` (`src/sessions.ts:134-138`). An agent name that would
/// leave `agents/` is refused ([`SessionsError::InvalidAgentName`]).
pub fn fallback_path(layout: &Layout, key: &TsKey) -> Result<PathBuf, SessionsError> {
    Ok(match key {
        TsKey::Global => layout.session_fallback(),
        TsKey::Agent(agent) => layout.agent_session_fallback(agent_name(agent)?),
        TsKey::Thread(thread) => layout.fallback_session(&encode_uri_component(thread)),
    })
}

/// `agent`, if it names one dir directly under `agents/`. The API's Scope parser accepts
/// `agent:..`, which would otherwise resolve `agents/../session.json` in the project dir.
fn agent_name(agent: &str) -> Result<&str, SessionsError> {
    if agent.is_empty() || agent == "." || agent == ".." || agent.contains(['/', '\\']) {
        return Err(SessionsError::InvalidAgentName);
    }
    Ok(agent)
}

/// Reads the fallback session for `key` (`src/sessions.ts:140-147`).
pub fn read_fallback(layout: &Layout, key: &TsKey) -> Result<Option<TsSession>, SessionsError> {
    read_record(&fallback_path(layout, key)?)
}

/// Writes the fallback session for `key`, creating its dir as TS does (`src/sessions.ts:149-153`).
pub fn write_fallback(
    layout: &Layout,
    key: &TsKey,
    session: &TsSession,
) -> Result<(), SessionsError> {
    write_json(&fallback_path(layout, key)?, &session.to_value())
}

/// Removes the fallback session for `key` (`src/sessions.ts:188-194`). Returns whether there
/// was one.
pub fn remove_fallback(layout: &Layout, key: &TsKey) -> Result<bool, SessionsError> {
    remove_file(&fallback_path(layout, key)?)
}

/// JavaScript's `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` becomes
/// `%XX` per UTF-8 byte, upper-case hex.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// ---- Backups ---------------------------------------------------------------------------------

/// One `session_<N>.backup` (#44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBackup {
    /// `N`, as TS reads it with `Number(...)`; a number too large for `u64` reads as
    /// `u64::MAX`.
    pub n: u64,
    pub path: PathBuf,
}

impl SessionBackup {
    /// The file name, e.g. `session_3.backup`: what TS's `backupSession` returns.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Every `session_<N>.backup`, newest (highest N) first, so Housekeeping keeps the first 20
/// (#44). Names are matched as TS matches them, `/^session_\d+\.backup$/`
/// (`src/sessions.ts:207-209`).
pub fn list_backups(layout: &Layout) -> Result<Vec<SessionBackup>, SessionsError> {
    let root = layout.root();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(StoreError::io("list", root)(e).into()),
    };
    let mut backups = Vec::new();
    for entry in entries {
        let entry = entry.map_err(StoreError::io("list", root))?;
        let name = entry.file_name();
        let Some(digits) = name
            .to_str()
            .and_then(|n| n.strip_prefix("session_"))
            .and_then(|n| n.strip_suffix(".backup"))
            .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        backups.push(SessionBackup {
            n: digits.parse().unwrap_or(u64::MAX),
            path: entry.path(),
        });
    }
    backups.sort_by(|a, b| b.n.cmp(&a.n).then_with(|| b.path.cmp(&a.path)));
    Ok(backups)
}

/// The path of the next backup: N is one above the highest existing N, or 1
/// (`src/sessions.ts:207-213`).
fn next_backup(layout: &Layout) -> Result<SessionBackup, SessionsError> {
    let last = list_backups(layout)?.first().map_or(0, |b| b.n);
    let n = last
        .checked_add(1)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(SessionsError::BackupNumberExhausted { last })?;
    Ok(SessionBackup {
        n: n.into(),
        path: layout.session_backup(n),
    })
}

/// TS's `backupSession` (`src/sessions.ts:196-218`): when `session.json` holds a session, it is
/// renamed to the next `session_<N>.backup`, byte for byte, and `session.json` is gone after.
/// `None` when there was no session to back up.
pub fn backup_global(layout: &Layout) -> Result<Option<SessionBackup>, SessionsError> {
    if read(layout, &TsKey::Global)?.is_none() {
        return Ok(None);
    }
    let backup = next_backup(layout)?;
    let from = layout.session();
    fs::rename(&from, &backup.path).map_err(StoreError::io("rename", &from))?;
    Ok(Some(backup))
}

/// Writes `session` in TS shape as the next `session_<N>.backup`, for a session that has no
/// `session.json` to rename (another Scope's, or a parked one).
pub fn write_backup(layout: &Layout, session: &TsSession) -> Result<SessionBackup, SessionsError> {
    let backup = next_backup(layout)?;
    write_json(&backup.path, &session.to_value())?;
    Ok(backup)
}

// ---- IO --------------------------------------------------------------------------------------

/// A JSON file TS reads with `Bun.file(path).json()` inside a `catch`: missing or unparseable is
/// `None`. Other IO errors are reported rather than swallowed.
fn read_json(path: &Path) -> Result<Option<Value>, SessionsError> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::io("read", path)(e).into()),
    }
}

fn read_record(path: &Path) -> Result<Option<TsSession>, SessionsError> {
    Ok(read_json(path)?.as_ref().and_then(TsSession::from_value))
}

/// `JSON.stringify(value, null, 2) + "\n"`, atomically, creating the parent dir.
fn write_json(path: &Path, value: &Value) -> Result<(), SessionsError> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("a Value serialises");
    bytes.push(b'\n');
    if let Some(dir) = path.parent() {
        atomic::create_dir(dir, DirMode::Default)?;
    }
    atomic::write_atomic(path, &bytes, FileMode::Keep)?;
    Ok(())
}

fn remove_file(path: &Path) -> Result<bool, SessionsError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(StoreError::io("remove", path)(e).into()),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, hour, 0, 0).unwrap()
    }

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        fs::create_dir_all(layout.root()).unwrap();
        (dir, layout)
    }

    fn text(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn a_new_global_session_is_written_as_create_session_writes_it() {
        // `createSession` (src/sessions.ts:78-86) then `saveSession` (src/sessions.ts:58):
        // keys in this order, two-space indent, ISO dates with milliseconds, trailing newline.
        let (_dir, layout) = layout();
        write(&layout, &TsKey::Global, &TsSession::new("s-1", at(8))).unwrap();
        assert_eq!(
            text(&layout.session()),
            "{\n  \"sessionId\": \"s-1\",\n  \"createdAt\": \"2026-10-14T08:00:00.000Z\",\n  \
             \"lastUsedAt\": \"2026-10-14T08:00:00.000Z\",\n  \"turnCount\": 0,\n  \
             \"compactWarned\": false\n}\n"
        );
    }

    #[test]
    fn a_thread_record_is_written_as_create_thread_session_writes_it() {
        // `createThreadSession` (src/sessionManager.ts:60-71): `threadId` follows `sessionId`,
        // inside `{"threads": {...}}`.
        let (_dir, layout) = layout();
        let key = TsKey::Thread("1234".into());
        write(&layout, &key, &TsSession::new("s-2", at(9))).unwrap();
        assert_eq!(
            text(&layout.sessions()),
            "{\n  \"threads\": {\n    \"1234\": {\n      \"sessionId\": \"s-2\",\n      \
             \"threadId\": \"1234\",\n      \"createdAt\": \"2026-10-14T09:00:00.000Z\",\n      \
             \"lastUsedAt\": \"2026-10-14T09:00:00.000Z\",\n      \"turnCount\": 0,\n      \
             \"compactWarned\": false\n    }\n  }\n}\n"
        );
        let read_back = read(&layout, &key).unwrap().unwrap();
        assert_eq!(read_back.thread_id.as_deref(), Some("1234"));
        assert_eq!(read_back.created_at, Some(at(9)));
    }

    #[test]
    fn records_are_read_with_ts_backfills() {
        // Table from src/sessionValidate.ts:2-6 (sessionId), src/sessions.ts:68-69 and
        // src/sessionManager.ts:46-47 (turnCount/compactWarned defaults).
        let cases = [
            (json!(null), None),
            (json!({}), None),
            (json!({"sessionId": ""}), None),
            (json!({"sessionId": 7}), None),
            (json!({"sessionId": "s"}), Some((0, false, None))),
            (
                json!({"sessionId": "s", "turnCount": "3", "compactWarned": "yes"}),
                Some((0, false, None)),
            ),
            (
                json!({"sessionId": "s", "turnCount": 3, "compactWarned": true, "messageCount": 9}),
                Some((3, true, Some(9))),
            ),
        ];
        for (value, expected) in cases {
            let got = TsSession::from_value(&value)
                .map(|s| (s.turn_count, s.compact_warned, s.message_count));
            assert_eq!(got, expected, "{value}");
        }
    }

    #[test]
    fn unknown_keys_and_unparsed_dates_survive_a_patch() {
        let (_dir, layout) = layout();
        fs::write(
            layout.session(),
            r#"{"sessionId":"s-1","createdAt":"yesterday","lastUsedAt":"2026-10-14T08:00:00.000Z","turnCount":2,"compactWarned":false,"messageCount":4,"addedLater":{"a":1}}"#,
        )
        .unwrap();
        let mut session = read(&layout, &TsKey::Global).unwrap().unwrap();
        assert_eq!(session.created_at, None);
        session.turn_count = 3;
        session.last_used_at = Some(at(10));
        write(&layout, &TsKey::Global, &session).unwrap();
        let saved: Value = serde_json::from_str(&text(&layout.session())).unwrap();
        assert_eq!(
            saved,
            json!({
                "sessionId": "s-1",
                "createdAt": "yesterday",
                "lastUsedAt": "2026-10-14T10:00:00.000Z",
                "turnCount": 3,
                "compactWarned": false,
                "messageCount": 4,
                "addedLater": {"a": 1},
            })
        );
        // Key order is the file's.
        let keys: Vec<_> = saved.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys.last().unwrap(), "addedLater");
    }

    #[test]
    fn writing_a_thread_keeps_the_other_threads_and_top_level_keys() {
        let (_dir, layout) = layout();
        fs::write(
            layout.sessions(),
            json!({
                "threads": {"daily-report": {"sessionId": "j-1", "threadId": "daily-report", "x": 1}},
                "addedLater": true,
            })
            .to_string(),
        )
        .unwrap();
        write(
            &layout,
            &TsKey::Thread("agent:ops".into()),
            &TsSession::new("a-1", at(8)),
        )
        .unwrap();
        let saved: Value = serde_json::from_str(&text(&layout.sessions())).unwrap();
        assert_eq!(saved["addedLater"], json!(true));
        assert_eq!(saved["threads"]["daily-report"]["x"], json!(1));
        assert_eq!(
            saved["threads"]["agent:ops"]["threadId"],
            json!("agent:ops")
        );
        let listed: Vec<_> = list_threads(&layout)
            .unwrap()
            .into_iter()
            .map(|(k, s)| (k, s.session_id))
            .collect();
        assert_eq!(
            listed,
            [
                ("daily-report".to_owned(), "j-1".to_owned()),
                ("agent:ops".to_owned(), "a-1".to_owned())
            ]
        );

        assert!(remove(&layout, &TsKey::Thread("daily-report".into())).unwrap());
        assert!(!remove(&layout, &TsKey::Thread("daily-report".into())).unwrap());
        assert_eq!(list_threads(&layout).unwrap().len(), 1);
    }

    #[test]
    fn an_unparseable_sessions_json_reads_as_no_threads() {
        // `loadSessions`' catch (src/sessionManager.ts:27-29).
        let (_dir, layout) = layout();
        fs::write(layout.sessions(), "{not json").unwrap();
        assert_eq!(read(&layout, &TsKey::Thread("1".into())).unwrap(), None);
        assert!(list_threads(&layout).unwrap().is_empty());
    }

    #[test]
    fn agent_sessions_live_in_the_agent_dir() {
        let (_dir, layout) = layout();
        let key = TsKey::Agent("ops".into());
        assert_eq!(read(&layout, &key).unwrap(), None);
        write(&layout, &key, &TsSession::new("a-2", at(8))).unwrap();
        assert!(layout.agent_session("ops").is_file());
        assert_eq!(read(&layout, &key).unwrap().unwrap().session_id, "a-2");
        assert!(remove(&layout, &key).unwrap());
        assert!(!remove(&layout, &key).unwrap());
    }

    #[test]
    fn agent_names_that_leave_agents_dir_are_refused() {
        let (dir, layout) = layout();
        fs::write(dir.path().join("session.json"), r#"{"sessionId":"root"}"#).unwrap();
        let session = TsSession::new("a-1", at(8));
        for name in ["", ".", "..", "a/b", "../x", "a\\b"] {
            let key = TsKey::Agent(name.into());
            let refused = |r: Result<(), SessionsError>| {
                assert!(
                    matches!(r, Err(SessionsError::InvalidAgentName)),
                    "{name:?}"
                )
            };
            refused(read(&layout, &key).map(drop));
            refused(write(&layout, &key, &session));
            refused(remove(&layout, &key).map(drop));
            refused(read_fallback(&layout, &key).map(drop));
            refused(write_fallback(&layout, &key, &session));
            refused(remove_fallback(&layout, &key).map(drop));
        }
        // The project dir's own file was never touched.
        assert!(dir.path().join("session.json").is_file());
        // A dotted name that stays inside `agents/` is fine.
        write(&layout, &TsKey::Agent("ops.v2".into()), &session).unwrap();
    }

    #[test]
    fn fallback_paths_follow_fallback_session_path_for() {
        // src/sessions.ts:134-138; a thread wins over an agent, and its key is URI-encoded.
        let layout = Layout::new("/p");
        let root = "/p/.claude/claudeclaw";
        let cases = [
            (TsKey::Global, format!("{root}/session_fallback.json")),
            (
                TsKey::Agent("ops".into()),
                "/p/agents/ops/session_fallback.json".to_owned(),
            ),
            (
                TsKey::Thread("1234".into()),
                format!("{root}/fallback-sessions/1234.json"),
            ),
            (
                TsKey::Thread("agent:ops".into()),
                format!("{root}/fallback-sessions/agent%3Aops.json"),
            ),
            (
                TsKey::Thread("tg:-100:7".into()),
                format!("{root}/fallback-sessions/tg%3A-100%3A7.json"),
            ),
        ];
        for (key, path) in cases {
            assert_eq!(
                fallback_path(&layout, &key).unwrap(),
                PathBuf::from(path),
                "{key:?}"
            );
        }
    }

    #[test]
    fn encode_uri_component_matches_javascript() {
        // Expected values are what `encodeURIComponent` returns in Bun/Node.
        for (input, expected) in [
            ("abc-_.!~*'()XYZ09", "abc-_.!~*'()XYZ09"),
            ("a/b c", "a%2Fb%20c"),
            ("agent:ops", "agent%3Aops"),
            ("é", "%C3%A9"),
        ] {
            assert_eq!(encode_uri_component(input), expected);
        }
    }

    #[test]
    fn fallback_sessions_round_trip_and_create_their_dir() {
        let (_dir, layout) = layout();
        let key = TsKey::Thread("1234".into());
        write_fallback(&layout, &key, &TsSession::new("f-1", at(8))).unwrap();
        assert!(layout.fallback_sessions_dir().is_dir());
        assert_eq!(
            read_fallback(&layout, &key).unwrap().unwrap().session_id,
            "f-1"
        );
        assert!(remove_fallback(&layout, &key).unwrap());
        assert_eq!(read_fallback(&layout, &key).unwrap(), None);
    }

    #[test]
    fn backups_are_listed_newest_first_by_n() {
        let (_dir, layout) = layout();
        for name in [
            "session_2.backup",
            "session_10.backup",
            "session_1.backup",
            "session_x.backup",
            "session_.backup",
            "session_3.backup.tmp",
            "other.backup",
        ] {
            fs::write(layout.root().join(name), "{}").unwrap();
        }
        let names: Vec<_> = list_backups(&layout)
            .unwrap()
            .iter()
            .map(SessionBackup::name)
            .collect();
        assert_eq!(
            names,
            ["session_10.backup", "session_2.backup", "session_1.backup"]
        );
    }

    #[test]
    fn backup_global_renames_session_json_to_the_next_n() {
        let (_dir, layout) = layout();
        // No session: nothing to back up (src/sessions.ts:197-198).
        assert_eq!(backup_global(&layout).unwrap(), None);

        let original = r#"{"sessionId":"s-1","turnCount":5,"extra":1}"#;
        fs::write(layout.session(), original).unwrap();
        fs::write(layout.root().join("session_4.backup"), "{}").unwrap();
        let backup = backup_global(&layout).unwrap().unwrap();
        assert_eq!(backup.n, 5);
        assert_eq!(backup.name(), "session_5.backup");
        assert_eq!(text(&backup.path), original);
        assert!(!layout.session().exists());
    }

    #[test]
    fn the_first_backup_is_1_and_write_backup_uses_ts_shape() {
        let (_dir, layout) = layout();
        let backup = write_backup(&layout, &TsSession::new("s-3", at(8))).unwrap();
        assert_eq!(backup.name(), "session_1.backup");
        assert_eq!(
            text(&backup.path),
            "{\n  \"sessionId\": \"s-3\",\n  \"createdAt\": \"2026-10-14T08:00:00.000Z\",\n  \
             \"lastUsedAt\": \"2026-10-14T08:00:00.000Z\",\n  \"turnCount\": 0,\n  \
             \"compactWarned\": false\n}\n"
        );
        assert_eq!(
            write_backup(&layout, &TsSession::new("s-4", at(8)))
                .unwrap()
                .n,
            2
        );
    }

    #[test]
    fn debug_shows_no_unknown_values() {
        let session = TsSession::from_value(&json!({"sessionId": "s", "note": "private"})).unwrap();
        let debug = format!("{session:?}");
        assert!(!debug.contains("private"), "{debug}");
    }
}
