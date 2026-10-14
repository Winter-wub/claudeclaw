//! Rule 7, mirror the active session (#14 rule 7; architecture §8 table).
//!
//! For each Scope, the TS session files hold the session of the Scope's *current* Launcher, in
//! TS shape, so a rolled-back TS daemon resumes the same conversation. On roll-forward, a TS
//! entry that differs from the Rust one is adopted as the current Launcher's session.
//!
//! The Scope → TS key mapping uses the keys TS itself reads (architecture §6 "Scope keys"):
//!
//! | Scope | TS session |
//! |---|---|
//! | `global` | `session.json` |
//! | `discord:<id>` | `sessions.json` `threads[<id>]` (`src/commands/discord.ts:1304`) |
//! | `job:<stem>` | `sessions.json` `threads[<stem>]` (`src/commands/start.ts:927`) |
//! | `job:<agent>/<stem>` | `sessions.json` `threads["agent:<agent>"]` (`src/commands/start.ts:927`) |
//! | `agent:<name>` | `agents/<name>/session.json` (`src/sessions.ts:25`) |
//!
//! TS runs every job of one agent under the one key `agent:<agent>`, so those Scopes share a
//! mirror entry: the last write wins, and adoption can hand one TS session to several of them,
//! as TS itself shares it.

use chrono::{DateTime, Utc};
use claudeclaw_api::scope::Scope;

use crate::layout::Layout;
use crate::sessions::ts::{self, TsKey, TsSession};
use crate::sessions::{SessionEntry, SessionStore, SessionsError};

/// Writes and reads the TS mirror of each Scope's current-Launcher session (#14 rule 7).
#[derive(Debug, Clone, Copy)]
pub struct SessionMirror<'a> {
    layout: &'a Layout,
}

/// What [`SessionMirror::adopt`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum Adoption {
    /// TS holds no session for the Scope; the store is unchanged.
    NoTsEntry,
    /// TS holds the session the store already has.
    Same,
    /// TS held another session, now the current Launcher's. `replaced` is the store's previous
    /// session for that pair, if there was one.
    Adopted { replaced: Option<Box<SessionEntry>> },
}

impl<'a> SessionMirror<'a> {
    pub fn new(layout: &'a Layout) -> Self {
        SessionMirror { layout }
    }

    /// The TS key that mirrors `scope` (see the module table).
    pub fn ts_key(scope: &Scope) -> TsKey {
        match scope {
            Scope::Global => TsKey::Global,
            Scope::Discord(id) => TsKey::Thread(id.clone()),
            Scope::Job(name) => match name.split_once('/') {
                Some((agent, _stem)) => TsKey::Thread(format!("agent:{agent}")),
                None => TsKey::Thread(name.clone()),
            },
            Scope::Agent(name) => TsKey::Agent(name.clone()),
        }
    }

    /// Writes `session`, the current Launcher's session of `scope`, into its TS file in TS shape.
    /// The TS record is patched, so its unknown keys stay. When the TS record held another
    /// session, its per-session TS state (`compactWarned`, `messageCount`) starts over, as TS's
    /// `createSession` would leave it.
    pub fn write(&self, scope: &Scope, session: &SessionEntry) -> Result<(), SessionsError> {
        let key = Self::ts_key(scope);
        let mut record = match ts::read(self.layout, &key)? {
            Some(existing) => existing,
            None => TsSession::new(session.session_id.clone(), session.created_at),
        };
        if record.session_id != session.session_id {
            record.session_id = session.session_id.clone();
            record.compact_warned = false;
            record.message_count = None;
        }
        record.created_at = Some(session.created_at);
        record.last_used_at = Some(session.last_used_at);
        record.turn_count = session.turns.into();
        ts::write(self.layout, &key, &record)
    }

    /// The TS session that mirrors `scope`, if there is one.
    pub fn read(&self, scope: &Scope) -> Result<Option<TsSession>, SessionsError> {
        ts::read(self.layout, &Self::ts_key(scope))
    }

    /// Removes the mirror entry of `scope` (a reset, a deleted job, a pruned thread; #12 §6,
    /// #44). Returns whether there was one.
    pub fn remove(&self, scope: &Scope) -> Result<bool, SessionsError> {
        ts::remove(self.layout, &Self::ts_key(scope))
    }

    /// Roll-forward (#14 rule 7): when TS holds a session for `scope` other than the store's
    /// session of (`scope`, `launcher`), it becomes that pair's session. `launcher` is the
    /// Scope's current Launcher, which the caller resolves. The adopted session has TS's
    /// dates and turn count (`now` where TS has none) and no Rust state yet: no Transcript,
    /// context, Cost baseline, breaker or budget. The store is changed in memory only; the
    /// caller saves it.
    pub fn adopt(
        &self,
        store: &mut SessionStore,
        scope: &Scope,
        launcher: &str,
        now: DateTime<Utc>,
    ) -> Result<Adoption, SessionsError> {
        let Some(record) = self.read(scope)? else {
            return Ok(Adoption::NoTsEntry);
        };
        if store
            .get(scope, launcher)
            .is_some_and(|e| e.session_id == record.session_id)
        {
            return Ok(Adoption::Same);
        }
        let mut entry = SessionEntry::new(scope.clone(), launcher, record.session_id, now);
        entry.created_at = record.created_at.unwrap_or(now);
        entry.last_used_at = record.last_used_at.unwrap_or(now);
        entry.turns = u32::try_from(record.turn_count).unwrap_or(u32::MAX);
        Ok(Adoption::Adopted {
            replaced: store.upsert(entry).map(Box::new),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::TimeZone;
    use serde_json::{Value, json};

    use super::*;
    use crate::sessions::TimeoutBreaker;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, hour, 0, 0).unwrap()
    }

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        fs::create_dir_all(layout.root()).unwrap();
        (dir, layout)
    }

    fn scope(s: &str) -> Scope {
        s.parse().unwrap()
    }

    fn json_at(path: &std::path::Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn scopes_map_to_the_keys_ts_reads() {
        // src/commands/discord.ts:1304 (channel/thread → bare id), src/commands/start.ts:927
        // (job → its name, agent job → `agent:<agent>`), src/sessions.ts:25 (agent →
        // agents/<agent>/session.json).
        let cases = [
            ("global", TsKey::Global),
            ("discord:1234", TsKey::Thread("1234".into())),
            ("job:daily-report", TsKey::Thread("daily-report".into())),
            ("job:ops/digest", TsKey::Thread("agent:ops".into())),
            ("agent:ops", TsKey::Agent("ops".into())),
        ];
        for (s, key) in cases {
            assert_eq!(SessionMirror::ts_key(&scope(s)), key, "{s}");
        }
    }

    #[test]
    fn each_scope_is_written_into_its_ts_file() {
        let (_dir, layout) = layout();
        let mirror = SessionMirror::new(&layout);
        for (s, id) in [
            ("global", "g-1"),
            ("discord:1234", "d-1"),
            ("job:ops/digest", "j-1"),
            ("agent:ops", "a-1"),
        ] {
            let mut entry = SessionEntry::new(scope(s), "direct", id, at(8));
            entry.turns = 4;
            entry.last_used_at = at(9);
            mirror.write(&scope(s), &entry).unwrap();
        }
        assert_eq!(
            json_at(&layout.session()),
            json!({
                "sessionId": "g-1",
                "createdAt": "2026-10-14T08:00:00.000Z",
                "lastUsedAt": "2026-10-14T09:00:00.000Z",
                "turnCount": 4,
                "compactWarned": false,
            })
        );
        let threads = json_at(&layout.sessions());
        assert_eq!(threads["threads"]["1234"]["sessionId"], json!("d-1"));
        assert_eq!(threads["threads"]["1234"]["threadId"], json!("1234"));
        assert_eq!(threads["threads"]["agent:ops"]["sessionId"], json!("j-1"));
        assert_eq!(
            json_at(&layout.agent_session("ops"))["sessionId"],
            json!("a-1")
        );
    }

    #[test]
    fn a_write_keeps_unknown_ts_keys_and_resets_per_session_state_on_a_new_session() {
        let (_dir, layout) = layout();
        fs::write(
            layout.session(),
            json!({
                "sessionId": "old",
                "createdAt": "2026-10-01T00:00:00.000Z",
                "lastUsedAt": "2026-10-01T00:00:00.000Z",
                "turnCount": 40,
                "compactWarned": true,
                "messageCount": 40,
                "addedLater": "kept",
            })
            .to_string(),
        )
        .unwrap();
        let mirror = SessionMirror::new(&layout);

        // The same session: TS's per-session state is kept.
        let mut same = SessionEntry::new(Scope::Global, "direct", "old", at(8));
        same.turns = 41;
        mirror.write(&Scope::Global, &same).unwrap();
        let saved = json_at(&layout.session());
        assert_eq!(saved["compactWarned"], json!(true));
        assert_eq!(saved["messageCount"], json!(40));
        assert_eq!(saved["turnCount"], json!(41));

        // Another session: per-session state starts over, unknown keys stay.
        let fresh = SessionEntry::new(Scope::Global, "direct", "new", at(10));
        mirror.write(&Scope::Global, &fresh).unwrap();
        assert_eq!(
            json_at(&layout.session()),
            json!({
                "sessionId": "new",
                "createdAt": "2026-10-14T10:00:00.000Z",
                "lastUsedAt": "2026-10-14T10:00:00.000Z",
                "turnCount": 0,
                "compactWarned": false,
                "addedLater": "kept",
            })
        );
    }

    #[test]
    fn a_differing_ts_session_is_adopted_for_the_current_launcher() {
        let (_dir, layout) = layout();
        let thread = scope("discord:1234");
        let mut store = SessionStore::new();
        let mut rust = SessionEntry::new(thread.clone(), "/opt/ccs.sh", "rust-1", at(8));
        rust.timeout_breaker = Some(TimeoutBreaker {
            session_id: "rust-1".into(),
            consecutive_timeouts: 2,
            failing_since: Some(at(8)),
            opened_at: None,
        });
        store.upsert(rust.clone());
        store.upsert(SessionEntry::new(
            thread.clone(),
            "direct",
            "parked-1",
            at(7),
        ));

        // TS ran during a rollback and started its own session for the thread.
        let mut ts_record = TsSession::new("ts-1", at(9));
        ts_record.turn_count = 3;
        ts::write(&layout, &TsKey::Thread("1234".into()), &ts_record).unwrap();

        let mirror = SessionMirror::new(&layout);
        let adoption = mirror
            .adopt(&mut store, &thread, "/opt/ccs.sh", at(12))
            .unwrap();
        assert_eq!(
            adoption,
            Adoption::Adopted {
                replaced: Some(Box::new(rust))
            }
        );
        let adopted = store.get(&thread, "/opt/ccs.sh").unwrap();
        assert_eq!(adopted.session_id, "ts-1");
        assert_eq!(adopted.created_at, at(9));
        assert_eq!(adopted.turns, 3);
        assert_eq!(adopted.timeout_breaker, None);
        assert_eq!(adopted.cost_state, None);
        // The parked session of the other Launcher is untouched.
        assert_eq!(store.get(&thread, "direct").unwrap().session_id, "parked-1");

        // Read again: nothing differs now.
        assert_eq!(
            mirror
                .adopt(&mut store, &thread, "/opt/ccs.sh", at(13))
                .unwrap(),
            Adoption::Same
        );
    }

    #[test]
    fn adoption_without_a_ts_entry_or_a_rust_session() {
        let (_dir, layout) = layout();
        let mirror = SessionMirror::new(&layout);
        let mut store = SessionStore::new();
        assert_eq!(
            mirror
                .adopt(&mut store, &Scope::Global, "direct", at(8))
                .unwrap(),
            Adoption::NoTsEntry
        );

        // A TS session with no Rust session yet: adopted, with `now` for TS's unparsed dates.
        fs::write(
            layout.session(),
            r#"{"sessionId":"ts-2","createdAt":"bad"}"#,
        )
        .unwrap();
        assert_eq!(
            mirror
                .adopt(&mut store, &Scope::Global, "direct", at(8))
                .unwrap(),
            Adoption::Adopted { replaced: None }
        );
        let adopted = store.get(&Scope::Global, "direct").unwrap();
        assert_eq!(adopted.created_at, at(8));
        assert_eq!(adopted.turns, 0);
    }

    #[test]
    fn remove_clears_the_mirror_entry() {
        let (_dir, layout) = layout();
        let mirror = SessionMirror::new(&layout);
        let job = scope("job:daily-report");
        mirror
            .write(
                &job,
                &SessionEntry::new(job.clone(), "direct", "j-1", at(8)),
            )
            .unwrap();
        assert!(mirror.read(&job).unwrap().is_some());
        assert!(mirror.remove(&job).unwrap());
        assert_eq!(mirror.read(&job).unwrap(), None);
        assert!(!mirror.remove(&job).unwrap());
    }
}
