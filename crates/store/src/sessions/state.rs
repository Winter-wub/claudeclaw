//! `daemon/state/sessions.json`: the Rust (Scope, Launcher) session store (architecture §4
//! "Sessions", §9.1; #8, #12 §6, #32, #33, #39, #44).
//!
//! One entry per (Scope, Launcher) pair. Besides the Claude `session_id`, an entry carries the
//! session's Transcript location, context meter and Cost baseline (#39), its Timeout breaker
//! (#32), its Auto-compact budget (#33) and its orphan clock (#12 §6, #44). The file is R: TS
//! never reads it; the TS mirror is [`crate::rollback::session_mirror`].
//!
//! Load, mutate through [`SessionStore`], save. Keys this build doesn't know survive the round
//! trip, at any depth, and an entry this build can't read at all (a Scope kind added later) is
//! written back as it was.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use claudeclaw_api::scope::Scope;
use claudeclaw_api::sessions::{
    AutoCompactLast, AutoCompactState, SessionContext, TimeoutBreakerState,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::SessionsError;
use crate::atomic::{self, DirMode, FileMode};
use crate::error::StoreError;
use crate::layout::Layout;
use crate::preserve::{Shape, merge_unknown};

/// The `schema` this build reads and writes. A file with a higher one is refused rather than
/// misread.
pub const SCHEMA: u32 = 1;

/// One (Scope, Launcher) session (#8). Its fields are the persisted state; whether it is
/// `in_use` or `parked` depends on the Scope's current Launcher, which `core` resolves.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEntry {
    pub scope: Scope,
    /// The Launcher path, or `direct` (#8).
    pub launcher: String,
    /// Claude Code's session id, passed to `--resume`.
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    /// Turns run in this session.
    #[serde(default)]
    pub turns: u32,
    /// The session's first message, cut to 160 chars (management-api §2.5); `None` for an
    /// imported or adopted session.
    #[serde(default)]
    pub first_message: Option<String>,
    /// When the session's Scope went away (a deleted job file, a thread Discord no longer
    /// knows); the orphan clock that the 30-day prune reads (#12 §6, #44). `None` while the
    /// Scope exists.
    #[serde(default)]
    pub orphaned_since: Option<DateTime<Utc>>,
    /// Where its Transcript was found (#39 D1–D2); `None` until located.
    #[serde(default)]
    pub transcript: Option<TranscriptLocation>,
    /// The context meter from the latest Daemon run (#39 D4); `None` before the first one.
    #[serde(default)]
    pub context: Option<SessionContext>,
    /// The Cost baseline: Claude Code's session-cumulative figures after the last run (#39 D8).
    /// `None` until a Daemon run of this session reported them.
    #[serde(default)]
    pub cost_state: Option<CostState>,
    /// The Timeout breaker (#32); `None` when it has never counted anything.
    #[serde(default)]
    pub timeout_breaker: Option<TimeoutBreaker>,
    /// The Auto-compact budget (#33); `None` when it has never been spent.
    #[serde(default)]
    pub auto_compact: Option<AutoCompact>,
}

impl SessionEntry {
    /// A fresh session, first used at `now`, with no state yet.
    pub fn new(
        scope: Scope,
        launcher: impl Into<String>,
        session_id: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Self {
        SessionEntry {
            scope,
            launcher: launcher.into(),
            session_id: session_id.into(),
            created_at: now,
            last_used_at: now,
            turns: 0,
            first_message: None,
            orphaned_since: None,
            transcript: None,
            context: None,
            cost_state: None,
            timeout_breaker: None,
            auto_compact: None,
        }
    }

    /// The breaker as the Management API shows it (#32). State recorded for another
    /// `session_id` is stale (the session changed since) and reads as empty.
    pub fn timeout_breaker_state(&self) -> TimeoutBreakerState {
        match &self.timeout_breaker {
            Some(b) if b.session_id == self.session_id => TimeoutBreakerState {
                consecutive_timeouts: b.consecutive_timeouts,
                failing_since: b.failing_since,
                open: b.opened_at.is_some(),
                opened_at: b.opened_at,
            },
            _ => TimeoutBreakerState {
                consecutive_timeouts: 0,
                failing_since: None,
                open: false,
                opened_at: None,
            },
        }
    }

    /// The Auto-compact budget as the Management API shows it (#33). State recorded for another
    /// `session_id` reads as unspent.
    pub fn auto_compact_state(&self) -> AutoCompactState {
        match &self.auto_compact {
            Some(a) if a.session_id == self.session_id => AutoCompactState {
                spent_at: a.spent_at,
                last: a.last,
            },
            _ => AutoCompactState {
                spent_at: None,
                last: None,
            },
        }
    }
}

impl fmt::Debug for SessionEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionEntry")
            .field("scope", &self.scope)
            .field("launcher", &self.launcher)
            .field("session_id", &self.session_id)
            .field("created_at", &self.created_at)
            .field("last_used_at", &self.last_used_at)
            .field("turns", &self.turns)
            .field(
                "first_message_len",
                &self.first_message.as_ref().map(String::len),
            )
            .field("orphaned_since", &self.orphaned_since)
            .field("transcript", &self.transcript)
            .field("context", &self.context)
            .field("cost_state", &self.cost_state)
            .field("timeout_breaker", &self.timeout_breaker)
            .field("auto_compact", &self.auto_compact)
            .finish()
    }
}

/// `transcript`: where a session's Transcript was located (#39 D1–D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptLocation {
    /// `<config dir>/projects/<project dir>/<session_id>.jsonl`.
    pub path: PathBuf,
    pub via: TranscriptVia,
    pub located_at: DateTime<Utc>,
}

/// How a Transcript was located (#39 D1–D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptVia {
    /// From the run's `system/init.memory_paths.auto` hint (D1).
    Init,
    /// From the fallback search over the known roots (D2).
    Search,
    /// A value a later build wrote.
    #[serde(other)]
    Unknown,
}

/// `cost_state`: Claude Code's session-cumulative cost and tokens after the session's last
/// Daemon run, the baseline the next run's figures are a difference against (#39 D8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostState {
    pub cost_usd: f64,
    #[serde(default)]
    pub by_model: BTreeMap<String, ModelCost>,
}

/// One model's share of a [`CostState`] (#39 D8).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelCost {
    pub cost_usd: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

/// `timeout_breaker`: the Timeout breaker's persisted state (#32). It applies to `session_id`
/// only; once the session changes it reads as empty ([`SessionEntry::timeout_breaker_state`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeoutBreaker {
    pub session_id: String,
    pub consecutive_timeouts: u32,
    /// The start of the first failed Turn since the last success.
    pub failing_since: Option<DateTime<Utc>>,
    /// Set while the breaker is open.
    pub opened_at: Option<DateTime<Utc>>,
}

/// `auto_compact`: the Auto-compact budget's persisted state (#33). It applies to `session_id`
/// only ([`SessionEntry::auto_compact_state`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoCompact {
    pub session_id: String,
    /// When this stretch's compact was spent; written before the compact is spawned.
    pub spent_at: Option<DateTime<Utc>>,
    /// The last auto-compact run, for the Management API record.
    #[serde(default)]
    pub last: Option<AutoCompactLast>,
}

/// `daemon/state/sessions.json`, loaded. Mutate it, then [`SessionStore::save`].
#[derive(Debug, Clone, Default)]
pub struct SessionStore {
    sessions: Vec<SessionEntry>,
    /// Entries this build can't read, written back verbatim.
    unreadable: Vec<Value>,
    /// The file as loaded, whose unknown keys [`SessionStore::save`] keeps.
    loaded: Option<Value>,
}

impl SessionStore {
    /// An empty store, as for a dir with no `sessions.json` yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads `daemon/state/sessions.json`; a missing file is an empty store.
    pub fn load(layout: &Layout) -> Result<Self, SessionsError> {
        let path = layout.session_state();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => return Err(StoreError::io("read", &path)(e).into()),
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(|source| StoreError::Json {
            path: path.clone(),
            source,
        })?;
        let Value::Object(top) = &value else {
            return Err(StoreError::NotAnObject { path }.into());
        };
        let found = top.get("schema").and_then(Value::as_u64).unwrap_or(1);
        if found > u64::from(SCHEMA) {
            return Err(SessionsError::NewerSchema {
                path,
                found,
                supported: SCHEMA,
            });
        }
        let mut store = SessionStore::new();
        for raw in top
            .get("sessions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match serde_json::from_value::<SessionEntry>(raw.clone()) {
                Ok(entry) => store.sessions.push(entry),
                Err(_) => store.unreadable.push(raw.clone()),
            }
        }
        store.loaded = Some(value);
        Ok(store)
    }

    /// Writes the store atomically to `daemon/state/sessions.json`, creating `daemon/` (0700)
    /// and `daemon/state/` when missing (§9.1).
    pub fn save(&self, layout: &Layout) -> Result<(), SessionsError> {
        let mut sessions = self
            .sessions
            .iter()
            .map(|e| serde_json::to_value(e).expect("a session entry serialises"))
            .collect::<Vec<_>>();
        sessions.extend(self.unreadable.iter().cloned());
        let mut value = json!({"schema": SCHEMA, "sessions": sessions});
        if let Some(loaded) = &self.loaded {
            merge_unknown(
                &mut value,
                loaded,
                &Shape {
                    id: &entry_id,
                    // `cost_state.by_model` is keyed by model name (#39 D8).
                    maps: &["by_model"],
                },
            );
        }
        let mut bytes = serde_json::to_vec_pretty(&value).expect("a Value serialises");
        bytes.push(b'\n');
        atomic::create_dir(&layout.daemon_dir(), DirMode::Private)?;
        atomic::create_dir(&layout.state_dir(), DirMode::Default)?;
        atomic::write_atomic(&layout.session_state(), &bytes, FileMode::Keep)?;
        Ok(())
    }

    /// The session of (`scope`, `launcher`).
    pub fn get(&self, scope: &Scope, launcher: &str) -> Option<&SessionEntry> {
        self.sessions
            .iter()
            .find(|e| e.scope == *scope && e.launcher == launcher)
    }

    /// The session of (`scope`, `launcher`), to mutate.
    pub fn get_mut(&mut self, scope: &Scope, launcher: &str) -> Option<&mut SessionEntry> {
        self.sessions
            .iter_mut()
            .find(|e| e.scope == *scope && e.launcher == launcher)
    }

    /// Stores `entry` as its (Scope, Launcher) pair's session, in place of the one there, which
    /// is returned.
    pub fn upsert(&mut self, entry: SessionEntry) -> Option<SessionEntry> {
        match self.get_mut(&entry.scope, &entry.launcher) {
            Some(slot) => Some(std::mem::replace(slot, entry)),
            None => {
                self.sessions.push(entry);
                None
            }
        }
    }

    /// Removes the session of (`scope`, `launcher`).
    pub fn remove(&mut self, scope: &Scope, launcher: &str) -> Option<SessionEntry> {
        let i = self
            .sessions
            .iter()
            .position(|e| e.scope == *scope && e.launcher == launcher)?;
        Some(self.sessions.remove(i))
    }

    /// Removes every session of `scope`, whatever its Launcher (a deleted job, a pruned thread;
    /// #12 §6, #44).
    pub fn remove_scope(&mut self, scope: &Scope) -> Vec<SessionEntry> {
        let (removed, kept) = std::mem::take(&mut self.sessions)
            .into_iter()
            .partition(|e| e.scope == *scope);
        self.sessions = kept;
        removed
    }

    /// Every session, in file order.
    pub fn iter(&self) -> impl Iterator<Item = &SessionEntry> {
        self.sessions.iter()
    }

    /// The sessions of `scope`, one per Launcher it has used.
    pub fn for_scope<'a>(&'a self, scope: &'a Scope) -> impl Iterator<Item = &'a SessionEntry> {
        self.sessions.iter().filter(move |e| e.scope == *scope)
    }

    /// How many entries this build couldn't read; they are kept and written back unchanged.
    pub fn unreadable_entries(&self) -> usize {
        self.unreadable.len()
    }
}

/// Matches an entry across a load and a save: the same pair and the same Claude session, so a
/// new session never inherits a replaced one's unknown keys.
fn entry_id(v: &Value) -> Option<String> {
    let field = |k| v.get(k).and_then(Value::as_str);
    Some(format!(
        "{}\n{}\n{}",
        field("scope")?,
        field("launcher")?,
        field("session_id")?
    ))
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::runs::RunOutcome;

    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 10, 14, hour, 0, 0).unwrap()
    }

    fn thread() -> Scope {
        Scope::Discord("1234".into())
    }

    fn full_entry() -> SessionEntry {
        SessionEntry {
            turns: 7,
            first_message: Some("hello there".into()),
            orphaned_since: Some(at(11)),
            transcript: Some(TranscriptLocation {
                path: "/h/.claude/projects/p/s-1.jsonl".into(),
                via: TranscriptVia::Init,
                located_at: at(9),
            }),
            context: Some(SessionContext {
                used_tokens: 41_000,
                window: 200_000,
            }),
            cost_state: Some(CostState {
                cost_usd: 1.5,
                by_model: BTreeMap::from([(
                    "m-1".to_owned(),
                    ModelCost {
                        cost_usd: 1.5,
                        tokens_in: 10,
                        tokens_out: 20,
                        cache_read_tokens: 30,
                        cache_write_tokens: 40,
                    },
                )]),
            }),
            timeout_breaker: Some(TimeoutBreaker {
                session_id: "s-1".into(),
                consecutive_timeouts: 2,
                failing_since: Some(at(8)),
                opened_at: Some(at(9)),
            }),
            auto_compact: Some(AutoCompact {
                session_id: "s-1".into(),
                spent_at: Some(at(8)),
                last: Some(AutoCompactLast {
                    at: at(8),
                    outcome: RunOutcome::Timeout,
                    duration_ms: 1_000,
                }),
            }),
            ..SessionEntry::new(thread(), "direct", "s-1", at(8))
        }
    }

    fn full_entry_json() -> Value {
        json!({
            "scope": "discord:1234",
            "launcher": "direct",
            "session_id": "s-1",
            "created_at": "2026-10-14T08:00:00Z",
            "last_used_at": "2026-10-14T08:00:00Z",
            "turns": 7,
            "first_message": "hello there",
            "orphaned_since": "2026-10-14T11:00:00Z",
            "transcript": {
                "path": "/h/.claude/projects/p/s-1.jsonl",
                "via": "init",
                "located_at": "2026-10-14T09:00:00Z",
            },
            "context": {"used_tokens": 41000, "window": 200000},
            "cost_state": {
                "cost_usd": 1.5,
                "by_model": {"m-1": {
                    "cost_usd": 1.5, "tokens_in": 10, "tokens_out": 20,
                    "cache_read_tokens": 30, "cache_write_tokens": 40,
                }},
            },
            "timeout_breaker": {
                "session_id": "s-1",
                "consecutive_timeouts": 2,
                "failing_since": "2026-10-14T08:00:00Z",
                "opened_at": "2026-10-14T09:00:00Z",
            },
            "auto_compact": {
                "session_id": "s-1",
                "spent_at": "2026-10-14T08:00:00Z",
                "last": {"at": "2026-10-14T08:00:00Z", "outcome": "timeout", "duration_ms": 1000},
            },
        })
    }

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        (dir, layout)
    }

    fn file(layout: &Layout) -> Value {
        serde_json::from_slice(&fs::read(layout.session_state()).unwrap()).unwrap()
    }

    #[test]
    fn an_entry_has_the_section_9_1_shape() {
        assert_eq!(
            serde_json::to_value(full_entry()).unwrap(),
            full_entry_json()
        );
        assert_eq!(
            serde_json::from_value::<SessionEntry>(full_entry_json()).unwrap(),
            full_entry()
        );
    }

    #[test]
    fn a_fresh_entry_writes_its_nulls_and_a_bare_one_parses() {
        let fresh = SessionEntry::new(Scope::Global, "direct", "s-2", at(8));
        let wire = serde_json::to_value(&fresh).unwrap();
        for key in [
            "first_message",
            "orphaned_since",
            "transcript",
            "context",
            "cost_state",
            "timeout_breaker",
            "auto_compact",
        ] {
            assert_eq!(wire[key], Value::Null, "{key}");
        }
        let bare = json!({
            "scope": "global", "launcher": "direct", "session_id": "s-2",
            "created_at": "2026-10-14T08:00:00Z", "last_used_at": "2026-10-14T08:00:00Z",
        });
        assert_eq!(serde_json::from_value::<SessionEntry>(bare).unwrap(), fresh);
    }

    #[test]
    fn a_missing_file_is_an_empty_store_and_a_save_round_trips() {
        let (_dir, layout) = layout();
        let mut store = SessionStore::load(&layout).unwrap();
        assert_eq!(store.iter().count(), 0);
        store.upsert(full_entry());
        store.save(&layout).unwrap();
        assert_eq!(
            file(&layout),
            json!({"schema": 1, "sessions": [full_entry_json()]})
        );
        let again = SessionStore::load(&layout).unwrap();
        assert_eq!(again.get(&thread(), "direct"), Some(&full_entry()));
    }

    #[cfg(unix)]
    #[test]
    fn save_creates_a_private_daemon_dir() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, layout) = layout();
        SessionStore::new().save(&layout).unwrap();
        let mode = fs::metadata(layout.daemon_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn unknown_keys_survive_a_mutation_and_save() {
        let (_dir, layout) = layout();
        let mut entry = full_entry_json();
        entry["added_later"] = json!({"x": 1});
        entry["timeout_breaker"]["added_later"] = json!(true);
        let on_disk = json!({"schema": 1, "top_level_later": [1, 2], "sessions": [entry]});
        atomic::create_dir(&layout.state_dir(), DirMode::Default).unwrap();
        fs::write(layout.session_state(), on_disk.to_string()).unwrap();

        let mut store = SessionStore::load(&layout).unwrap();
        let session = store.get_mut(&thread(), "direct").unwrap();
        session.turns = 8;
        session
            .timeout_breaker
            .as_mut()
            .unwrap()
            .consecutive_timeouts = 3;
        store.save(&layout).unwrap();

        let saved = file(&layout);
        assert_eq!(saved["top_level_later"], json!([1, 2]));
        let saved_entry = &saved["sessions"][0];
        assert_eq!(saved_entry["turns"], json!(8));
        assert_eq!(saved_entry["added_later"], json!({"x": 1}));
        assert_eq!(
            saved_entry["timeout_breaker"]["consecutive_timeouts"],
            json!(3)
        );
        assert_eq!(saved_entry["timeout_breaker"]["added_later"], json!(true));
    }

    #[test]
    fn a_replaced_cost_baseline_keeps_no_old_model() {
        // `by_model` is keyed by model name: a model gone from the baseline must stay gone, or
        // the next run's cost difference is taken against a stale figure (#39 D8).
        let (_dir, layout) = layout();
        let mut store = SessionStore::new();
        store.upsert(full_entry());
        store.save(&layout).unwrap();

        let mut store = SessionStore::load(&layout).unwrap();
        let cost = store
            .get_mut(&thread(), "direct")
            .unwrap()
            .cost_state
            .as_mut()
            .unwrap();
        let m1 = cost.by_model.remove("m-1").unwrap();
        cost.by_model.insert("m-2".into(), m1);
        store.save(&layout).unwrap();

        let reloaded = SessionStore::load(&layout).unwrap();
        let models: Vec<_> = reloaded
            .get(&thread(), "direct")
            .unwrap()
            .cost_state
            .as_ref()
            .unwrap()
            .by_model
            .keys()
            .cloned()
            .collect();
        assert_eq!(models, ["m-2"]);
    }

    #[test]
    fn a_new_session_for_the_pair_drops_the_old_ones_unknown_keys() {
        let (_dir, layout) = layout();
        let mut entry = full_entry_json();
        entry["added_later"] = json!(1);
        atomic::create_dir(&layout.state_dir(), DirMode::Default).unwrap();
        fs::write(
            layout.session_state(),
            json!({"schema": 1, "sessions": [entry]}).to_string(),
        )
        .unwrap();
        let mut store = SessionStore::load(&layout).unwrap();
        let old = store.upsert(SessionEntry::new(thread(), "direct", "s-9", at(12)));
        assert_eq!(old.unwrap().session_id, "s-1");
        store.save(&layout).unwrap();
        assert_eq!(file(&layout)["sessions"][0].get("added_later"), None);
    }

    #[test]
    fn an_entry_this_build_cannot_read_is_written_back_verbatim() {
        let (_dir, layout) = layout();
        let foreign = json!({"scope": "tg:-100:7", "launcher": "direct", "session_id": "t-1"});
        atomic::create_dir(&layout.state_dir(), DirMode::Default).unwrap();
        fs::write(
            layout.session_state(),
            json!({"schema": 1, "sessions": [foreign, full_entry_json()]}).to_string(),
        )
        .unwrap();
        let store = SessionStore::load(&layout).unwrap();
        assert_eq!(store.unreadable_entries(), 1);
        assert_eq!(store.iter().count(), 1);
        store.save(&layout).unwrap();
        let saved = file(&layout);
        assert_eq!(saved["sessions"].as_array().unwrap().len(), 2);
        assert!(saved["sessions"].as_array().unwrap().contains(&foreign));
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let (_dir, layout) = layout();
        atomic::create_dir(&layout.state_dir(), DirMode::Default).unwrap();
        fs::write(layout.session_state(), r#"{"schema": 2, "sessions": []}"#).unwrap();
        assert!(matches!(
            SessionStore::load(&layout),
            Err(SessionsError::NewerSchema { found: 2, .. })
        ));
        fs::write(layout.session_state(), "[]").unwrap();
        assert!(matches!(
            SessionStore::load(&layout),
            Err(SessionsError::Store(StoreError::NotAnObject { .. }))
        ));
    }

    #[test]
    fn breaker_and_budget_of_another_session_read_as_empty() {
        let entry = full_entry();
        assert_eq!(
            entry.timeout_breaker_state(),
            TimeoutBreakerState {
                consecutive_timeouts: 2,
                failing_since: Some(at(8)),
                open: true,
                opened_at: Some(at(9)),
            }
        );
        assert_eq!(entry.auto_compact_state().spent_at, Some(at(8)));

        let reset = SessionEntry {
            session_id: "s-2".into(),
            ..entry
        };
        assert_eq!(reset.timeout_breaker_state().consecutive_timeouts, 0);
        assert!(!reset.timeout_breaker_state().open);
        assert_eq!(
            reset.auto_compact_state(),
            AutoCompactState {
                spent_at: None,
                last: None,
            }
        );
    }

    #[test]
    fn sessions_are_keyed_by_scope_and_launcher() {
        let mut store = SessionStore::new();
        store.upsert(SessionEntry::new(thread(), "direct", "a", at(8)));
        store.upsert(SessionEntry::new(thread(), "/opt/ccs.sh", "b", at(8)));
        store.upsert(SessionEntry::new(Scope::Global, "direct", "c", at(8)));
        assert_eq!(store.get(&thread(), "/opt/ccs.sh").unwrap().session_id, "b");
        assert_eq!(store.for_scope(&thread()).count(), 2);

        assert_eq!(store.remove(&thread(), "direct").unwrap().session_id, "a");
        assert_eq!(store.remove(&thread(), "direct"), None);
        let removed = store.remove_scope(&thread());
        assert_eq!(removed.len(), 1);
        assert_eq!(
            store
                .iter()
                .map(|e| e.session_id.as_str())
                .collect::<Vec<_>>(),
            ["c"]
        );
    }

    #[test]
    fn debug_leaves_the_first_message_out() {
        let debug = format!("{:?}", full_entry());
        assert!(!debug.contains("hello"), "{debug}");
        assert!(debug.contains("first_message_len: Some(11)"), "{debug}");
    }
}
