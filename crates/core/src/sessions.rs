//! The (Scope, Launcher) session lifecycle and its decisions (architecture §4 `claudeclaw-core`
//! `sessions`, §9.1; #8, #12 §6, #32, #33, #39, #44).
//!
//! One [`SessionManager`] per project dir owns the Rust session store
//! (`daemon/state/sessions.json`, `claudeclaw-store`'s [`SessionStore`]) and makes every decision
//! the store only persists:
//!
//! - **identity and cost** (#39): a finished run's reported `session_id`, its context meter and
//!   its Cost baseline, and the per-run cost by difference against that baseline. A session's
//!   first fresh run differences against zero; an imported session's first resumed run has no
//!   baseline, so its cost is `null`;
//! - **recovery** (#8, #33, #39 D5): a dead or corrupt resumed session is re-run once fresh, a
//!   subscription limit falls back, and a timed-out Turn is Auto-compacted — the skip order is
//!   `new_session` → `timeout_breaker` → `disabled` → `budget_spent` (#33);
//! - the **Timeout breaker** (#32), opt-in and off by default: one per (Scope, Launcher) session,
//!   persisted. Only the runner's own timeout kill counts (#47). Once open it skips Auto-compact
//!   and retry but never refuses or delays a Turn; every opening raises one owner notice, and it
//!   closes on the session's next successful Turn;
//! - **parking** (#8): sessions are keyed by (Scope, Launcher), so a Scope switching Launcher
//!   parks the old session and unparks it when it switches back;
//! - **orphan pruning** (#12 §6, #44): an orphaned session's state is kept for thirty days
//!   ([`ORPHAN_RETENTION`]) and then pruned, its TS mirror entry with it.
//!
//! The pure decisions live in [`policy`]; this module owns the store, the wall clock, the events
//! and the owner notices. The synchronous store reads and writes run on `spawn_blocking`
//! (architecture §2), and wall-clock work reads the injectable [`WallClock`].
//!
//! `crate::housekeeping`'s orphaned-Scope step (#44 D7) is a documented stub today. When it is
//! filled in it calls [`SessionManager::prune_orphans`] with the pass's `now` and `dry_run` and
//! adds the returned count under [`crate::housekeeping::KIND_ORPHANS`]; the Discord-liveness
//! decision (`Unknown Channel`) calls [`SessionManager::mark_orphaned`] first.

mod policy;

pub use policy::*;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use claudeclaw_api::log_event::daemon;
use claudeclaw_api::scope::Scope as ApiScope;
use claudeclaw_api::sessions::{SessionContext, SessionStatus};
use claudeclaw_api::settings::Settings;
use claudeclaw_runner::stream::{RunOutcome, Verdict};
use claudeclaw_scheduler::{SystemClock, WallClock};
use claudeclaw_store::sessions::{AutoCompact, TranscriptLocation, TranscriptVia};
use claudeclaw_store::{Layout, SessionEntry, SessionMirror, SessionStore, SessionsError};

use crate::notify::{Notice, NoticeKind, Notices};
use crate::scope::Scope;

/// The decisions [`SessionManager`] makes with: the Timeout breaker and Auto-compact settings
/// (#32, #33). Copied from [`Settings`] at open and again on every reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPolicy {
    pub timeout_breaker: claudeclaw_api::settings::TimeoutBreakerSettings,
    pub auto_compact: claudeclaw_api::settings::AutoCompactSettings,
}

impl Default for SessionPolicy {
    /// The breaker off, Auto-compact on with its stock timeout. The Daemon's real values come
    /// from `Settings` through [`SessionPolicy::from`].
    fn default() -> Self {
        SessionPolicy {
            timeout_breaker: claudeclaw_api::settings::TimeoutBreakerSettings {
                max_consecutive_timeouts: None,
                max_failing_seconds: None,
            },
            auto_compact: claudeclaw_api::settings::AutoCompactSettings {
                enabled: true,
                timeout_secs: 300,
            },
        }
    }
}

impl From<&Settings> for SessionPolicy {
    fn from(settings: &Settings) -> Self {
        SessionPolicy {
            timeout_breaker: settings.timeout_breaker,
            auto_compact: settings.auto_compact,
        }
    }
}

/// Why a [`SessionManager`] operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error(transparent)]
    Sessions(#[from] SessionsError),
    #[error("the session store operation did not finish")]
    Join(#[from] tokio::task::JoinError),
}

/// A session as the Management API shows it (management-api §2.5): the entry, its status under
/// the Scope's current Launcher, and when it would be pruned.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionView {
    pub entry: SessionEntry,
    /// `orphaned` overrides both, else `in_use` for the current Launcher, else `parked` (#8).
    pub status: SessionStatus,
    /// When the 30-day prune removes it; `None` while it isn't orphaned (#12 §6, #44).
    pub prune_at: Option<DateTime<Utc>>,
}

impl SessionView {
    fn of(entry: &SessionEntry, current_launcher: &str) -> Self {
        SessionView {
            status: policy::status(entry, current_launcher),
            prune_at: policy::prune_at(entry),
            entry: entry.clone(),
        }
    }
}

/// What [`SessionManager::record_run`] wrote for one finished run (#39).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunRecorded {
    /// The run's own cost, by difference against the Cost baseline (#39 D8).
    pub cost: CostDelta,
    /// The session's context meter after the run (#39 D4).
    pub context: Option<SessionContext>,
    /// The run changed the session (`--reset`, a rotation, a fresh start after a stale session).
    pub session_changed: bool,
}

/// The Rust session store's decisions for one project dir (#8, #32, #33, #39, #12 §6, #44).
///
/// Reads are synchronous (the store is in memory); every mutation clones the store and writes it
/// on `spawn_blocking`, so a slow disk never blocks the async runtime.
pub struct SessionManager {
    layout: Layout,
    store: Mutex<SessionStore>,
    notices: Arc<Notices>,
    clock: Arc<dyn WallClock>,
    policy: Mutex<SessionPolicy>,
}

impl SessionManager {
    /// Reads `daemon/state/sessions.json` for `layout` and makes `policy`'s decisions. `notices`
    /// raises the breaker-open and root-moved owner notices (#32, #39).
    pub async fn open(
        layout: &Layout,
        notices: Arc<Notices>,
        policy: SessionPolicy,
    ) -> Result<Self, ManagerError> {
        let read = layout.clone();
        let store = tokio::task::spawn_blocking(move || SessionStore::load(&read)).await??;
        Ok(SessionManager {
            layout: layout.clone(),
            store: Mutex::new(store),
            notices,
            clock: Arc::new(SystemClock),
            policy: Mutex::new(policy),
        })
    }

    /// Uses `clock` for every wall-clock decision (the breaker's `failing_since`, orphan
    /// pruning, cost `located_at`). The default is the system clock.
    pub fn with_clock(mut self, clock: Arc<dyn WallClock>) -> Self {
        self.clock = clock;
        self
    }

    /// The project layout this manager owns.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The policy in force.
    pub fn policy(&self) -> SessionPolicy {
        *lock(&self.policy)
    }

    /// Replaces the policy (a settings reload). Turning the Timeout breaker off closes every open
    /// one with reason `disabled` (#32).
    pub async fn set_policy(&self, policy: SessionPolicy) -> Result<(), ManagerError> {
        *lock(&self.policy) = policy;
        if policy::breaker_enabled(&policy.timeout_breaker) {
            return Ok(());
        }
        let closed = {
            let mut store = lock(&self.store);
            let keys: Vec<(ApiScope, String)> = store
                .iter()
                .filter(|entry| entry.timeout_breaker_state().open)
                .map(|entry| (entry.scope.clone(), entry.launcher.clone()))
                .collect();
            for (scope, launcher) in &keys {
                if let Some(entry) = store.get_mut(scope, launcher) {
                    entry.timeout_breaker = None;
                }
            }
            keys.len()
        };
        for _ in 0..closed {
            tracing::info!(
                event = daemon::SESSION_BREAKER_CLOSED,
                reason = "disabled",
                "the Timeout breaker was turned off"
            );
        }
        self.persist().await
    }

    /// The session of (`scope`, `launcher`), if any.
    pub fn get(&self, scope: &ApiScope, launcher: &str) -> Option<SessionEntry> {
        lock(&self.store).get(scope, launcher).cloned()
    }

    /// The session of (`scope`, `launcher`) as the Management API shows it, under the Scope's
    /// `current_launcher`.
    pub fn view(
        &self,
        scope: &ApiScope,
        launcher: &str,
        current_launcher: &str,
    ) -> Option<SessionView> {
        lock(&self.store)
            .get(scope, launcher)
            .map(|entry| SessionView::of(entry, current_launcher))
    }

    /// Every session of `scope`, one per Launcher it has used, as the Management API shows them.
    pub fn views(&self, scope: &ApiScope, current_launcher: &str) -> Vec<SessionView> {
        lock(&self.store)
            .for_scope(scope)
            .map(|entry| SessionView::of(entry, current_launcher))
            .collect()
    }

    /// Stores `entry` as its (Scope, Launcher) pair's session.
    pub async fn upsert(&self, entry: SessionEntry) -> Result<(), ManagerError> {
        lock(&self.store).upsert(entry);
        self.persist().await
    }

    /// Removes the session of (`scope`, `launcher`), returning it.
    pub async fn remove(
        &self,
        scope: &ApiScope,
        launcher: &str,
    ) -> Result<Option<SessionEntry>, ManagerError> {
        let removed = lock(&self.store).remove(scope, launcher);
        self.persist().await?;
        Ok(removed)
    }

    /// Removes every session of `scope` (a job deleted through the API, #12 §6, #44) and its TS
    /// mirror entries. Returns the removed sessions.
    pub async fn remove_scope(&self, scope: &ApiScope) -> Result<Vec<SessionEntry>, ManagerError> {
        let removed = lock(&self.store).remove_scope(scope);
        let mirrors: Vec<(ApiScope, Option<String>)> = removed
            .iter()
            .map(|entry| (entry.scope.clone(), Some(entry.session_id.clone())))
            .collect();
        self.persist().await?;
        self.remove_mirrors(mirrors).await?;
        Ok(removed)
    }

    /// Records a finished run against its session (#39): the reported `session_id` (a change
    /// clears the breaker and budget and starts the Cost baseline at zero), the context meter,
    /// the per-run cost and the Transcript location, and clears the orphan clock.
    pub async fn record_run(
        &self,
        scope: &ApiScope,
        launcher: &str,
        outcome: &RunOutcome,
        now: DateTime<Utc>,
    ) -> Result<RunRecorded, ManagerError> {
        let session_id = self.resolve_session_id(scope, launcher, outcome);
        let Some(session_id) = session_id else {
            return Ok(RunRecorded::default());
        };
        let transcript = self.locate_transcript(outcome, &session_id, now).await;
        let (recorded, closed) = {
            let mut store = lock(&self.store);
            if store.get(scope, launcher).is_none() {
                let mut entry = SessionEntry::new(scope.clone(), launcher, session_id.clone(), now);
                entry.cost_state = Some(policy::zero_baseline());
                store.upsert(entry);
            }
            let entry = store
                .get_mut(scope, launcher)
                .expect("the entry was just ensured");
            // A changed session is a fresh conversation: the breaker and budget start over, and
            // the Cost baseline starts at zero so this run's cost is the reported figure (#39 D8).
            let changed = entry.session_id != session_id;
            let closed = if changed {
                let event = policy::session_changed(entry);
                entry.session_id = session_id;
                entry.cost_state = Some(policy::zero_baseline());
                entry.context = None;
                event.closed
            } else {
                None
            };
            let cost = policy::per_run_cost(outcome, entry.cost_state.as_ref());
            if cost.baseline_reset {
                tracing::warn!(
                    event = daemon::USAGE_BASELINE_RESET,
                    "the reported cost was below the baseline; taking it as the run's own"
                );
            }
            if let Some(context) = policy::context(outcome) {
                entry.context = Some(context);
            }
            if let Some(location) = transcript {
                entry.transcript = Some(location);
            }
            if let Some(baseline) = policy::baseline_from_outcome(outcome) {
                entry.cost_state = Some(baseline);
            }
            entry.turns = entry.turns.saturating_add(1);
            entry.last_used_at = now;
            entry.orphaned_since = None;
            (
                RunRecorded {
                    cost,
                    context: entry.context,
                    session_changed: changed,
                },
                closed,
            )
        };
        if let Some(reason) = closed {
            tracing::info!(
                event = daemon::SESSION_BREAKER_CLOSED,
                reason = reason.as_str(),
                "the Timeout breaker closed"
            );
        }
        self.persist().await?;
        Ok(recorded)
    }

    /// The recovery policy for a finished run (#8, #32, #33, #39 D5), and its side effects: a
    /// chosen Auto-compact spends the session's budget before it is spawned, and the decision is
    /// logged (`run.recovery`, `run.recovery_skipped`).
    pub async fn decide(
        &self,
        scope: &ApiScope,
        launcher: &str,
        facts: &RunFacts<'_>,
    ) -> Result<Recovery, ManagerError> {
        let now = self.clock.now();
        let policy = self.policy();
        let recovery = {
            let store = lock(&self.store);
            policy::recovery(
                store.get(scope, launcher),
                facts,
                &policy.timeout_breaker,
                &policy.auto_compact,
                now,
            )
        };
        match recovery {
            Recovery::AutoCompact { timeout } => {
                {
                    let mut store = lock(&self.store);
                    if let Some(entry) = store.get_mut(scope, launcher) {
                        let session_id = entry.session_id.clone();
                        let budget = entry.auto_compact.get_or_insert_with(|| AutoCompact {
                            session_id: session_id.clone(),
                            spent_at: None,
                            last: None,
                        });
                        if budget.session_id != session_id {
                            *budget = AutoCompact {
                                session_id,
                                spent_at: None,
                                last: None,
                            };
                        }
                        budget.spent_at = Some(now);
                    }
                }
                tracing::warn!(
                    event = daemon::RUN_RECOVERY,
                    kind = "auto_compact",
                    compact_timeout_ms = millis(timeout),
                    turn_timeout_ms = millis(facts.run_timeout),
                    "Auto-compact runs before the Turn is re-run once"
                );
                self.persist().await?;
            }
            Recovery::Skipped(reason) => {
                let spent_ms_ago = {
                    let store = lock(&self.store);
                    store
                        .get(scope, launcher)
                        .map(|entry| policy::spent_ms_ago(entry, now))
                        .unwrap_or(0)
                };
                tracing::info!(
                    event = daemon::RUN_RECOVERY_SKIPPED,
                    kind = "auto_compact",
                    reason = reason.as_str(),
                    spent_ms_ago = spent_ms_ago,
                    "Auto-compact was skipped"
                );
            }
            Recovery::NewSession(reason) => {
                tracing::warn!(
                    event = daemon::RUN_RECOVERY,
                    kind = reason.as_str(),
                    turn_timeout_ms = millis(facts.run_timeout),
                    "the resumed session is dead or corrupt; re-running the Turn fresh"
                );
            }
            Recovery::RateLimitFallback => {
                tracing::warn!(
                    event = daemon::RUN_RECOVERY,
                    kind = "rate_limit",
                    turn_timeout_ms = millis(facts.run_timeout),
                    "the subscription limit was hit; retrying with the fallback model"
                );
            }
            Recovery::None => {}
        }
        Ok(recovery)
    }

    /// Records a finished `/compact` run for the session's Management API record (#33). It does
    /// not re-arm the budget: only a successful Turn does.
    pub async fn record_compact(
        &self,
        scope: &ApiScope,
        launcher: &str,
        at: DateTime<Utc>,
        outcome: claudeclaw_api::runs::RunOutcome,
        duration_ms: u64,
    ) -> Result<(), ManagerError> {
        {
            let mut store = lock(&self.store);
            if let Some(entry) = store.get_mut(scope, launcher) {
                policy::record_compact(entry, at, outcome, duration_ms);
            }
        }
        self.persist().await
    }

    /// Folds a finished Turn's final outcome into its session's Timeout breaker, once, after any
    /// recovery (#32). A success re-arms the Auto-compact budget; an opening raises one owner
    /// notice.
    pub async fn end_turn(
        &self,
        scope: &ApiScope,
        launcher: &str,
        outcome: &RunOutcome,
        started_at: DateTime<Utc>,
    ) -> Result<BreakerEvent, ManagerError> {
        let now = self.clock.now();
        let policy = self.policy();
        let event = {
            let mut store = lock(&self.store);
            let Some(entry) = store.get_mut(scope, launcher) else {
                return Ok(BreakerEvent::default());
            };
            let event =
                policy::apply_turn(entry, outcome, started_at, now, &policy.timeout_breaker);
            if matches!(outcome.verdict, Verdict::Ok) {
                policy::rearm_budget(entry);
            }
            event
        };
        if let Some(opening) = event.opened {
            tracing::warn!(
                event = daemon::SESSION_BREAKER_OPEN,
                scope = %scope,
                launcher = launcher,
                limit = opening.limit,
                consecutive_timeouts = opening.consecutive_timeouts,
                failing_ms = opening.failing_ms,
                "the Timeout breaker opened"
            );
            self.notices
                .send(
                    Notice::new(
                        NoticeKind::BreakerOpen,
                        "Timeout breaker open",
                        format!(
                            "Runs through the `{}` Launcher keep timing out; Auto-compact and retry \
                             are paused until one succeeds.",
                            launcher_file_name(launcher)
                        ),
                    )
                    .with_scope(Scope::from_api(scope)),
                )
                .await;
        }
        if let Some(reason) = event.closed {
            tracing::info!(
                event = daemon::SESSION_BREAKER_CLOSED,
                reason = reason.as_str(),
                "the Timeout breaker closed"
            );
        }
        self.persist().await?;
        Ok(event)
    }

    /// The root-moved check (#39 D5): when a resumed session reported `session_not_found` but its
    /// stored Transcript is still on disk, the Launcher's config dir moved. Logs
    /// `session.root_moved` and raises one deduped `launcher_root_moved` notice. Returns whether
    /// the root moved.
    pub async fn note_root_moved(
        &self,
        scope: &ApiScope,
        launcher: &str,
    ) -> Result<bool, ManagerError> {
        let Some(entry) = self.get(scope, launcher) else {
            return Ok(false);
        };
        let Some(path) = entry.transcript.as_ref().map(|t| t.path.clone()) else {
            return Ok(false);
        };
        let exists = tokio::fs::metadata(&path)
            .await
            .map(|meta| meta.is_file())
            .unwrap_or(false);
        if !policy::root_moved(&entry, exists) {
            return Ok(false);
        }
        tracing::warn!(
            event = daemon::SESSION_ROOT_MOVED,
            scope = %scope,
            launcher = launcher,
            "the Launcher's config dir moved; the session was started fresh"
        );
        self.notices
            .send_once(
                Notice::new(
                    NoticeKind::LauncherRootMoved,
                    "Launcher config dir moved",
                    format!(
                        "The `{}` Launcher now points Claude Code at a different config dir; the \
                         session was started fresh.",
                        launcher_file_name(launcher)
                    ),
                )
                .with_scope(Scope::from_api(scope)),
            )
            .await;
        Ok(true)
    }

    /// Marks every session of `scope` orphaned at the current time (#12 §6, #44): its Scope went
    /// away. Returns how many were newly marked.
    pub async fn mark_orphaned(&self, scope: &ApiScope) -> Result<u64, ManagerError> {
        let now = self.clock.now();
        let count = {
            let mut store = lock(&self.store);
            let launchers: Vec<String> = store
                .for_scope(scope)
                .map(|entry| entry.launcher.clone())
                .collect();
            let mut count = 0;
            for launcher in launchers {
                if let Some(entry) = store.get_mut(scope, &launcher)
                    && entry.orphaned_since.is_none()
                {
                    entry.orphaned_since = Some(now);
                    count += 1;
                }
            }
            count
        };
        self.persist().await?;
        Ok(count)
    }

    /// Prunes the sessions orphaned for [`ORPHAN_RETENTION`] or more (#12 §6, #44), removing
    /// their TS mirror entries too. With `dry_run` the count is computed and nothing is changed,
    /// in memory or on disk. Returns how many were (or would be) pruned.
    pub async fn prune_orphans(
        &self,
        now: DateTime<Utc>,
        dry_run: bool,
    ) -> Result<u64, ManagerError> {
        let doomed: Vec<(ApiScope, String)> = {
            let mut store = lock(&self.store);
            let expired: Vec<(ApiScope, String)> = store
                .iter()
                .filter(|entry| policy::orphan_expired(entry, now))
                .map(|entry| (entry.scope.clone(), entry.launcher.clone()))
                .collect();
            if dry_run {
                return Ok(expired.len() as u64);
            }
            expired
                .into_iter()
                .filter_map(|(scope, launcher)| {
                    store
                        .remove(&scope, &launcher)
                        .map(|entry| (entry.scope, entry.session_id))
                })
                .collect()
        };
        let mirrors: Vec<(ApiScope, Option<String>)> = doomed
            .iter()
            .map(|(scope, session_id)| (scope.clone(), Some(session_id.clone())))
            .collect();
        self.persist().await?;
        self.remove_mirrors(mirrors).await?;
        Ok(doomed.len() as u64)
    }

    /// Writes the store atomically on `spawn_blocking`.
    pub async fn persist(&self) -> Result<(), ManagerError> {
        let store = lock(&self.store).clone();
        let layout = self.layout.clone();
        tokio::task::spawn_blocking(move || store.save(&layout)).await??;
        Ok(())
    }

    /// The Claude session the run belongs to: what the run reported, else the stored one.
    fn resolve_session_id(
        &self,
        scope: &ApiScope,
        launcher: &str,
        outcome: &RunOutcome,
    ) -> Option<String> {
        outcome.session_id.clone().or_else(|| {
            lock(&self.store)
                .get(scope, launcher)
                .map(|entry| entry.session_id.clone())
        })
    }

    /// The Transcript location a run's hint points at, when the hint names a path that is a
    /// regular file (#39 D1).
    async fn locate_transcript(
        &self,
        outcome: &RunOutcome,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> Option<TranscriptLocation> {
        let path = outcome
            .transcript_hint
            .as_ref()?
            .transcript_path(session_id)?;
        tokio::fs::metadata(&path)
            .await
            .ok()?
            .is_file()
            .then_some(TranscriptLocation {
                path,
                via: TranscriptVia::Init,
                located_at: now,
            })
    }

    /// Removes the TS mirror entries of the given (Scope, session) pairs, on `spawn_blocking`.
    async fn remove_mirrors(
        &self,
        mirrors: Vec<(ApiScope, Option<String>)>,
    ) -> Result<(), ManagerError> {
        let layout = self.layout.clone();
        tokio::task::spawn_blocking(move || {
            let mirror = SessionMirror::new(&layout);
            for (scope, session_id) in &mirrors {
                mirror.remove(scope, session_id.as_deref())?;
            }
            Ok::<(), SessionsError>(())
        })
        .await??;
        Ok(())
    }
}

/// A mutex guard, recovering from poisoning rather than panicking: nothing here panics while
/// holding one, so a poison can only come from a bug elsewhere, and dropping the lock is worse.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A duration in whole milliseconds for a log field.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The Launcher's file name, for an owner notice: never the full path (#39, §2 "no paths").
fn launcher_file_name(launcher: &str) -> &str {
    Path::new(launcher)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(launcher)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicI64, Ordering};

    use chrono::{TimeDelta, TimeZone};
    use claudeclaw_chat::{MessageRef, Notifier, NotifyError, OwnerNotice};
    use claudeclaw_runner::stream::{ModelUsage, RunClock, Usage};
    use claudeclaw_store::sessions::TimeoutBreaker;
    use futures_util::future::BoxFuture;

    use super::*;
    use crate::events::EventBus;

    /// A wall clock that reads a shared millisecond counter, so a test moves time without a
    /// paused runtime.
    struct TestClock(Arc<AtomicI64>);

    impl TestClock {
        fn new(base: DateTime<Utc>) -> (Arc<Self>, Arc<AtomicI64>) {
            let millis = Arc::new(AtomicI64::new(base.timestamp_millis()));
            (Arc::new(TestClock(millis.clone())), millis)
        }
    }

    impl WallClock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            DateTime::from_timestamp_millis(self.0.load(Ordering::SeqCst))
                .expect("a valid millisecond timestamp")
        }
    }

    /// A Notifier that records the owner notices it was sent.
    struct RecordingNotifier {
        seen: Arc<Mutex<Vec<OwnerNotice>>>,
    }

    impl RecordingNotifier {
        fn new() -> (Arc<Self>, Arc<Mutex<Vec<OwnerNotice>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (Arc::new(RecordingNotifier { seen: seen.clone() }), seen)
        }
    }

    impl Notifier for RecordingNotifier {
        fn notify_owners(&self, msg: OwnerNotice) -> BoxFuture<'_, Result<(), NotifyError>> {
            Box::pin(async move {
                self.seen.lock().unwrap().push(msg);
                Ok(())
            })
        }

        fn interrupted(&self, _origin: MessageRef) -> BoxFuture<'_, Result<(), NotifyError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, hour, 0, 0).unwrap()
    }

    fn thread(id: &str) -> ApiScope {
        ApiScope::Discord(id.to_owned())
    }

    fn entry(scope: &ApiScope, launcher: &str, session_id: &str) -> SessionEntry {
        SessionEntry::new(scope.clone(), launcher, session_id, at(8))
    }

    fn outcome(verdict: Verdict) -> RunOutcome {
        RunOutcome {
            verdict,
            exit_code: None,
            signal: None,
            lingered: false,
            session_id: None,
            claude_code_version: None,
            api_key_source: None,
            transcript_hint: None,
            reply: None,
            is_error: None,
            api_error_status: None,
            num_turns: None,
            usage: None,
            model_usage: BTreeMap::new(),
            total_cost_usd: None,
            last_main_usage: None,
            model: None,
            clock: RunClock::default(),
        }
    }

    fn costing(verdict: Verdict, session_id: &str, cost_usd: f64) -> RunOutcome {
        RunOutcome {
            session_id: Some(session_id.to_owned()),
            total_cost_usd: Some(cost_usd),
            model_usage: BTreeMap::from([(
                "m-1".to_owned(),
                ModelUsage {
                    cost_usd: Some(cost_usd),
                    ..ModelUsage::default()
                },
            )]),
            ..outcome(verdict)
        }
    }

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        (dir, layout)
    }

    fn in_memory_notices() -> (Arc<Notices>, Arc<Mutex<Vec<OwnerNotice>>>) {
        let notices = Arc::new(Notices::in_memory(EventBus::new()));
        let (notifier, seen) = RecordingNotifier::new();
        notices.register(notifier);
        (notices, seen)
    }

    fn auto(enabled: bool, timeout_secs: u32) -> claudeclaw_api::settings::AutoCompactSettings {
        claudeclaw_api::settings::AutoCompactSettings {
            enabled,
            timeout_secs,
        }
    }

    fn breaker_limit(count: u32) -> SessionPolicy {
        SessionPolicy {
            timeout_breaker: claudeclaw_api::settings::TimeoutBreakerSettings {
                max_consecutive_timeouts: Some(count),
                max_failing_seconds: None,
            },
            auto_compact: auto(true, 300),
        }
    }

    fn facts<'a>(outcome: &'a RunOutcome, is_new: bool) -> RunFacts<'a> {
        RunFacts {
            outcome,
            is_new,
            run_timeout: Duration::from_secs(900),
            started_at: at(9),
        }
    }

    #[tokio::test]
    async fn a_session_round_trips_through_the_store() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();
        let mut saved = entry(&thread("1234"), "direct", "s-1");
        saved.turns = 3;
        saved.first_message = Some("hello".into());
        manager.upsert(saved.clone()).await.unwrap();

        let reopened = SessionManager::open(
            &layout,
            Arc::new(Notices::in_memory(EventBus::new())),
            SessionPolicy::default(),
        )
        .await
        .unwrap();
        assert_eq!(reopened.get(&thread("1234"), "direct"), Some(saved));
        assert_eq!(reopened.views(&thread("1234"), "direct").len(), 1);
    }

    #[tokio::test]
    async fn a_scope_switching_launcher_parks_and_unparks() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();
        manager
            .upsert(entry(&thread("1234"), "direct", "s-1"))
            .await
            .unwrap();
        manager
            .upsert(entry(&thread("1234"), "/opt/tools/ccs", "s-2"))
            .await
            .unwrap();

        let parked = manager
            .view(&thread("1234"), "/opt/tools/ccs", "direct")
            .unwrap();
        assert_eq!(parked.status, SessionStatus::Parked);
        assert_eq!(parked.prune_at, None);
        let in_use = manager.view(&thread("1234"), "direct", "direct").unwrap();
        assert_eq!(in_use.status, SessionStatus::InUse);
        assert_eq!(in_use.entry.session_id, "s-1");

        // Switching back unparks the other session.
        let unparked = manager
            .view(&thread("1234"), "/opt/tools/ccs", "/opt/tools/ccs")
            .unwrap();
        assert_eq!(unparked.status, SessionStatus::InUse);

        // An orphaned Scope overrides both.
        manager.mark_orphaned(&thread("1234")).await.unwrap();
        assert_eq!(
            manager
                .view(&thread("1234"), "direct", "direct")
                .unwrap()
                .status,
            SessionStatus::Orphaned
        );
    }

    #[tokio::test]
    async fn a_breaker_opening_raises_one_notice_and_a_success_closes_it() {
        let (_dir, layout) = layout();
        let (notices, seen) = in_memory_notices();
        let (clock, _millis) = TestClock::new(at(9));
        let manager = SessionManager::open(&layout, notices, breaker_limit(1))
            .await
            .unwrap()
            .with_clock(clock);
        manager
            .upsert(entry(&thread("1234"), "direct", "s-1"))
            .await
            .unwrap();

        let timeout = outcome(Verdict::Timeout);
        let first = manager
            .end_turn(&thread("1234"), "direct", &timeout, at(9))
            .await
            .unwrap();
        assert_eq!(first.opened.unwrap().limit, 1);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(seen.lock().unwrap()[0].kind, "breaker_open");

        // A second timeout finds the breaker already open: no second notice.
        let second = manager
            .end_turn(&thread("1234"), "direct", &timeout, at(10))
            .await
            .unwrap();
        assert_eq!(second, BreakerEvent::default());
        assert_eq!(seen.lock().unwrap().len(), 1);

        // A success closes it.
        let ok = outcome(Verdict::Ok);
        let closed = manager
            .end_turn(&thread("1234"), "direct", &ok, at(11))
            .await
            .unwrap();
        assert_eq!(closed.closed, Some(CloseReason::Success));
        assert!(
            !manager
                .get(&thread("1234"), "direct")
                .unwrap()
                .timeout_breaker_state()
                .open
        );
    }

    #[tokio::test]
    async fn turning_the_breaker_off_closes_every_open_one() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, breaker_limit(1))
            .await
            .unwrap();
        manager
            .upsert(entry(&thread("1234"), "direct", "s-1"))
            .await
            .unwrap();
        manager
            .end_turn(&thread("1234"), "direct", &outcome(Verdict::Timeout), at(9))
            .await
            .unwrap();
        assert!(
            manager
                .get(&thread("1234"), "direct")
                .unwrap()
                .timeout_breaker_state()
                .open
        );

        manager
            .set_policy(SessionPolicy {
                timeout_breaker: claudeclaw_api::settings::TimeoutBreakerSettings {
                    max_consecutive_timeouts: None,
                    max_failing_seconds: None,
                },
                auto_compact: auto(true, 300),
            })
            .await
            .unwrap();
        assert!(
            !manager
                .get(&thread("1234"), "direct")
                .unwrap()
                .timeout_breaker_state()
                .open
        );
    }

    #[tokio::test]
    async fn cost_is_differenced_across_resumed_runs_and_unpriced_for_an_imported_one() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();

        // A fresh session: the first run's reported figure is its own.
        let mut fresh = entry(&thread("1234"), "direct", "s-1");
        fresh.cost_state = Some(policy::zero_baseline());
        manager.upsert(fresh).await.unwrap();
        let first = manager
            .record_run(
                &thread("1234"),
                "direct",
                &costing(Verdict::Ok, "s-1", 0.003),
                at(10),
            )
            .await
            .unwrap();
        assert_eq!(first.cost.cost_usd, Some(0.003));
        assert!(first.cost.priced && !first.session_changed);

        // A resumed run: the difference against the last run's cumulative figure.
        let second = manager
            .record_run(
                &thread("1234"),
                "direct",
                &costing(Verdict::Ok, "s-1", 0.006),
                at(11),
            )
            .await
            .unwrap();
        assert_eq!(second.cost.cost_usd, Some(0.003));
        assert_eq!(
            manager
                .get(&thread("1234"), "direct")
                .unwrap()
                .cost_state
                .unwrap()
                .cost_usd,
            0.006
        );

        // An imported session (no baseline): its first resumed run is unpriced.
        manager
            .upsert(entry(&thread("5678"), "direct", "s-2"))
            .await
            .unwrap();
        let imported = manager
            .record_run(
                &thread("5678"),
                "direct",
                &costing(Verdict::Ok, "s-2", 0.004),
                at(12),
            )
            .await
            .unwrap();
        assert_eq!(imported.cost.cost_usd, None);
        assert!(!imported.cost.priced);
    }

    #[tokio::test]
    async fn a_changed_session_starts_its_baseline_at_zero() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();
        let mut old = entry(&thread("1234"), "direct", "s-1");
        old.cost_state = Some(policy::zero_baseline());
        old.timeout_breaker = Some(TimeoutBreaker {
            session_id: "s-1".into(),
            consecutive_timeouts: 2,
            failing_since: Some(at(8)),
            opened_at: Some(at(8)),
        });
        manager.upsert(old).await.unwrap();

        // The run reports a new session id: breaker and budget clear, cost is the reported figure.
        let recorded = manager
            .record_run(
                &thread("1234"),
                "direct",
                &costing(Verdict::Ok, "s-2", 0.005),
                at(10),
            )
            .await
            .unwrap();
        assert!(recorded.session_changed);
        assert_eq!(recorded.cost.cost_usd, Some(0.005));
        let after = manager.get(&thread("1234"), "direct").unwrap();
        assert_eq!(after.session_id, "s-2");
        assert!(after.timeout_breaker.is_none());
        assert!(!after.timeout_breaker_state().open);
    }

    #[tokio::test]
    async fn decide_spends_the_budget_once_and_then_skips() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let (clock, _millis) = TestClock::new(at(9));
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap()
            .with_clock(clock);
        manager
            .upsert(entry(&thread("1234"), "direct", "s-1"))
            .await
            .unwrap();

        let timed_out = outcome(Verdict::Timeout);
        let first = manager
            .decide(&thread("1234"), "direct", &facts(&timed_out, false))
            .await
            .unwrap();
        assert_eq!(
            first,
            Recovery::AutoCompact {
                timeout: Duration::from_secs(300)
            }
        );
        let spent = manager
            .get(&thread("1234"), "direct")
            .unwrap()
            .auto_compact_state()
            .spent_at;
        assert_eq!(spent, Some(at(9)));

        // The budget is spent: the next timed-out Turn skips with `budget_spent`.
        let second = manager
            .decide(&thread("1234"), "direct", &facts(&timed_out, false))
            .await
            .unwrap();
        assert_eq!(second, Recovery::Skipped(SkipReason::BudgetSpent));
    }

    #[tokio::test]
    async fn a_compact_run_is_recorded_without_rearming_the_budget() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();
        manager
            .upsert(entry(&thread("1234"), "direct", "s-1"))
            .await
            .unwrap();

        manager
            .record_compact(
                &thread("1234"),
                "direct",
                at(10),
                claudeclaw_api::runs::RunOutcome::Timeout,
                1_000,
            )
            .await
            .unwrap();
        let state = manager
            .get(&thread("1234"), "direct")
            .unwrap()
            .auto_compact_state();
        assert_eq!(state.spent_at, None);
        let last = state.last.unwrap();
        assert_eq!(last.outcome, claudeclaw_api::runs::RunOutcome::Timeout);
        assert_eq!(last.duration_ms, 1_000);
    }

    #[tokio::test]
    async fn decide_skips_for_a_new_or_breaker_open_session() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let (clock, _millis) = TestClock::new(at(9));
        let manager = SessionManager::open(&layout, notices, breaker_limit(1))
            .await
            .unwrap()
            .with_clock(clock);
        manager
            .upsert(entry(&thread("1234"), "direct", "s-1"))
            .await
            .unwrap();

        let timed_out = outcome(Verdict::Timeout);
        assert_eq!(
            manager
                .decide(&thread("1234"), "direct", &facts(&timed_out, true))
                .await
                .unwrap(),
            Recovery::Skipped(SkipReason::NewSession)
        );

        manager
            .end_turn(&thread("1234"), "direct", &timed_out, at(9))
            .await
            .unwrap();
        assert_eq!(
            manager
                .decide(&thread("1234"), "direct", &facts(&timed_out, false))
                .await
                .unwrap(),
            Recovery::Skipped(SkipReason::TimeoutBreaker)
        );
    }

    #[tokio::test]
    async fn an_orphan_prunes_at_thirty_days_with_a_dry_run() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();
        let now = at(12);

        let mut young = entry(&thread("5678"), "direct", "s-young");
        young.orphaned_since = Some(now - TimeDelta::days(29));
        manager.upsert(young.clone()).await.unwrap();
        let mut old = entry(&thread("1234"), "direct", "s-old");
        old.orphaned_since = Some(now - TimeDelta::days(31));
        manager.upsert(old.clone()).await.unwrap();

        // The mirror is written for both, so the prune's removal is visible.
        let mirror = SessionMirror::new(&layout);
        mirror.write(&thread("1234"), &old).unwrap();
        mirror.write(&thread("5678"), &young).unwrap();

        // A dry run reports one and touches nothing.
        assert_eq!(manager.prune_orphans(now, true).await.unwrap(), 1);
        assert!(manager.get(&thread("1234"), "direct").is_some());
        assert!(mirror.read(&thread("1234")).unwrap().is_some());

        // The real one removes the 31-day session, its mirror, and keeps the 29-day one.
        assert_eq!(manager.prune_orphans(now, false).await.unwrap(), 1);
        assert!(manager.get(&thread("1234"), "direct").is_none());
        assert!(manager.get(&thread("5678"), "direct").is_some());
        assert!(mirror.read(&thread("1234")).unwrap().is_none());
        assert!(mirror.read(&thread("5678")).unwrap().is_some());

        // It survives a reload.
        let reopened = SessionManager::open(
            &layout,
            Arc::new(Notices::in_memory(EventBus::new())),
            SessionPolicy::default(),
        )
        .await
        .unwrap();
        assert!(reopened.get(&thread("1234"), "direct").is_none());
        assert!(reopened.get(&thread("5678"), "direct").is_some());
    }

    #[tokio::test]
    async fn a_deleted_scope_removes_its_sessions_and_mirror() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();
        let job = ApiScope::Job("daily-report".into());
        let session = entry(&job, "direct", "j-1");
        manager.upsert(session.clone()).await.unwrap();
        SessionMirror::new(&layout).write(&job, &session).unwrap();

        let removed = manager.remove_scope(&job).await.unwrap();
        assert_eq!(removed.len(), 1);
        assert!(manager.get(&job, "direct").is_none());
        assert!(SessionMirror::new(&layout).read(&job).unwrap().is_none());
    }

    #[tokio::test]
    async fn a_moved_root_raises_one_deduped_notice() {
        let (_dir, layout) = layout();
        let (notices, seen) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();
        let transcript = layout.root().join("s-1.jsonl");
        tokio::fs::create_dir_all(layout.root()).await.unwrap();
        tokio::fs::write(&transcript, b"{}\n").await.unwrap();
        let mut session = entry(&thread("1234"), "direct", "s-1");
        session.transcript = Some(TranscriptLocation {
            path: transcript,
            via: TranscriptVia::Init,
            located_at: at(8),
        });
        manager.upsert(session).await.unwrap();

        assert!(
            manager
                .note_root_moved(&thread("1234"), "direct")
                .await
                .unwrap()
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(seen.lock().unwrap()[0].kind, "launcher_root_moved");
        // The same root moving again is one notice, not two (#39).
        assert!(
            manager
                .note_root_moved(&thread("1234"), "direct")
                .await
                .unwrap()
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_run_records_context_and_transcript_from_its_own_stream() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();
        manager
            .upsert(entry(&thread("1234"), "direct", "s-1"))
            .await
            .unwrap();

        // A UUID session id, so the hint's path rule applies.
        let session_id = "0000aaaa-0000-4000-8000-000000000001";
        let dir = layout.root().join("projects").join("p");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join(format!("{session_id}.jsonl")), b"{}\n")
            .await
            .unwrap();
        let hint = claudeclaw_runner::stream::TranscriptHint::from_memory_path(
            dir.join("memory").to_str().unwrap(),
        )
        .unwrap();

        let run = RunOutcome {
            session_id: Some(session_id.to_owned()),
            transcript_hint: Some(hint),
            last_main_usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 9,
                cache_read_input_tokens: 20,
                cache_creation_input_tokens: 5,
            }),
            model: Some("m-1".to_owned()),
            model_usage: BTreeMap::from([(
                "m-1".to_owned(),
                ModelUsage {
                    context_window: Some(200_000),
                    ..ModelUsage::default()
                },
            )]),
            ..outcome(Verdict::Ok)
        };
        let recorded = manager
            .record_run(&thread("1234"), "direct", &run, at(10))
            .await
            .unwrap();
        assert_eq!(
            recorded.context,
            Some(SessionContext {
                used_tokens: 125,
                window: 200_000
            })
        );
        let stored = manager.get(&thread("1234"), "direct").unwrap();
        assert!(stored.transcript.is_some());
        assert_eq!(stored.context.unwrap().used_tokens, 125);
    }

    #[tokio::test]
    async fn a_failed_run_without_a_session_id_records_nothing() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, SessionPolicy::default())
            .await
            .unwrap();
        let refused = outcome(Verdict::Refused(
            claudeclaw_runner::stream::Refusal::LauncherExecFailed { exit_code: 127 },
        ));
        let recorded = manager
            .record_run(&thread("1234"), "direct", &refused, at(10))
            .await
            .unwrap();
        assert_eq!(recorded, RunRecorded::default());
        assert!(manager.get(&thread("1234"), "direct").is_none());
    }

    #[tokio::test]
    async fn end_turn_without_a_session_is_a_no_op() {
        let (_dir, layout) = layout();
        let (notices, _) = in_memory_notices();
        let manager = SessionManager::open(&layout, notices, breaker_limit(1))
            .await
            .unwrap();
        let event = manager
            .end_turn(&thread("9999"), "direct", &outcome(Verdict::Timeout), at(9))
            .await
            .unwrap();
        assert_eq!(event, BreakerEvent::default());
    }
}
