//! The pure session decisions behind [`crate::sessions`]: recovery, the Timeout breaker, the
//! Auto-compact budget, the Cost baseline, parking and orphan pruning (#32, #33, #39, #12 §6,
//! #44).
//!
//! Everything here is a deterministic function of its arguments — no IO, no clock of its own —
//! so every branch is a table test. [`crate::sessions::SessionManager`] owns the store, the clock
//! and the events, and calls into this module.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use claudeclaw_api::runs::RunOutcome as IndexOutcome;
use claudeclaw_api::sessions::{SessionContext, SessionStatus};
use claudeclaw_api::settings::{AutoCompactSettings, TimeoutBreakerSettings};
use claudeclaw_runner::stream::{Failure, ModelUsage, RunOutcome, Verdict};
use claudeclaw_store::SessionEntry;
use claudeclaw_store::sessions::{AutoCompact, CostState, ModelCost, TimeoutBreaker};

/// An orphaned session's state is kept this long before pruning (#12 §6, #44 D8): thirty days on
/// the wall clock.
pub const ORPHAN_RETENTION: TimeDelta = TimeDelta::days(30);

/// How much of a finished run [`recovery`] needs (#33).
#[derive(Debug)]
pub struct RunFacts<'a> {
    /// The finished run's judged outcome.
    pub outcome: &'a RunOutcome,
    /// The Turn began a new session, or recovered one (stale session, thinking signature) in this
    /// Turn, so there is no session to `/compact` (#33 step 1).
    pub is_new: bool,
    /// The Turn's run timeout.
    pub run_timeout: Duration,
    /// When the Turn started, on the wall clock; the Timeout breaker's `failing_since` (#32).
    pub started_at: DateTime<Utc>,
}

/// What should happen after a finished run (#8, #33, #32, #39 D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// The run's outcome stands.
    None,
    /// Re-run the Turn once with `--resume` dropped: the stored session is dead or corrupt
    /// (#39 D5, #8).
    NewSession(NewSessionReason),
    /// Re-run once against the `fallback` model and API (#8).
    RateLimitFallback,
    /// Run `/compact` with this timeout, then re-run the Turn once (#33).
    AutoCompact { timeout: Duration },
    /// Auto-compact was skipped; the reason is logged as `run.recovery_skipped` (#33).
    Skipped(SkipReason),
}

impl Recovery {
    /// The `kind` the `run.recovery` and `run.recovery_skipped` events carry.
    pub const fn kind(self) -> &'static str {
        match self {
            Recovery::None => "none",
            Recovery::NewSession(NewSessionReason::StaleSession) => "stale_session",
            Recovery::NewSession(NewSessionReason::ThinkingSignature) => "thinking_signature",
            Recovery::RateLimitFallback => "rate_limit",
            Recovery::AutoCompact { .. } => "auto_compact",
            Recovery::Skipped(_) => "auto_compact",
        }
    }
}

/// Why a session is being re-run fresh (#39 D5, #8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewSessionReason {
    /// Claude Code no longer has the session (#39 D5, E3).
    StaleSession,
    /// The stored session's thinking-block signature is corrupt (#8).
    ThinkingSignature,
}

impl NewSessionReason {
    /// The `run.recovery` `kind` for this reason.
    pub const fn as_str(self) -> &'static str {
        match self {
            NewSessionReason::StaleSession => "stale_session",
            NewSessionReason::ThinkingSignature => "thinking_signature",
        }
    }
}

/// Why Auto-compact was skipped for a timed-out Turn (#33 §2, in check order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The session was new (or freshly recovered) in this Turn.
    NewSession,
    /// The Timeout breaker is open (#32).
    TimeoutBreaker,
    /// `auto_compact.enabled` is `false`.
    Disabled,
    /// This Failing stretch's compact was already spent (#33).
    BudgetSpent,
}

impl SkipReason {
    /// The `reason` the `run.recovery_skipped` event carries.
    pub const fn as_str(self) -> &'static str {
        match self {
            SkipReason::NewSession => "new_session",
            SkipReason::TimeoutBreaker => "timeout_breaker",
            SkipReason::Disabled => "disabled",
            SkipReason::BudgetSpent => "budget_spent",
        }
    }
}

/// `true` when a finished run was the runner's own Awake-clock timeout kill (#32, #47).
///
/// A Launcher that exits 124 on its own is a failed run, and a `lingered` run read a `result`
/// before the kill; [`Verdict::Timeout`] is only ever the runner's own kill.
pub fn counts_as_timeout(outcome: &RunOutcome) -> bool {
    matches!(outcome.verdict, Verdict::Timeout)
}

/// The `/compact` run's timeout: `min(auto_compact.timeout_secs, the Turn's run timeout)` (#33).
pub fn compact_timeout(settings: &AutoCompactSettings, run_timeout: Duration) -> Duration {
    Duration::from_secs(u64::from(settings.timeout_secs).min(run_timeout.as_secs()))
}

/// The recovery policy for a finished run (#8, #33, #32, #39 D5).
///
/// Order: a dead or corrupt resumed session is re-run fresh; a subscription limit falls back;
/// a timed-out Turn is auto-compacted under the skip order `new_session` → `timeout_breaker` →
/// `disabled` → `budget_spent` (#33 §2).
pub fn recovery(
    entry: Option<&SessionEntry>,
    facts: &RunFacts<'_>,
    breaker: &TimeoutBreakerSettings,
    auto: &AutoCompactSettings,
    now: DateTime<Utc>,
) -> Recovery {
    match &facts.outcome.verdict {
        Verdict::Failed(Failure::SessionNotFound) if !facts.is_new => {
            Recovery::NewSession(NewSessionReason::StaleSession)
        }
        Verdict::Failed(Failure::ThinkingSignature) if !facts.is_new => {
            Recovery::NewSession(NewSessionReason::ThinkingSignature)
        }
        Verdict::RateLimited { .. } => Recovery::RateLimitFallback,
        Verdict::Timeout => timeout_recovery(entry, facts, breaker, auto, now),
        Verdict::Ok | Verdict::Failed(_) | Verdict::Interrupted | Verdict::Refused(_) => {
            Recovery::None
        }
    }
}

fn timeout_recovery(
    entry: Option<&SessionEntry>,
    facts: &RunFacts<'_>,
    breaker: &TimeoutBreakerSettings,
    auto: &AutoCompactSettings,
    now: DateTime<Utc>,
) -> Recovery {
    // #33 step 1: nothing to compact when the session is new (or was recovered) this Turn.
    let Some(entry) = entry.filter(|_| !facts.is_new) else {
        return Recovery::Skipped(SkipReason::NewSession);
    };
    // #33 step 2.
    if breaker_open(entry, breaker, now) {
        return Recovery::Skipped(SkipReason::TimeoutBreaker);
    }
    // #33 step 3.
    if !auto.enabled {
        return Recovery::Skipped(SkipReason::Disabled);
    }
    // #33 step 4.
    if budget_spent(entry) {
        return Recovery::Skipped(SkipReason::BudgetSpent);
    }
    Recovery::AutoCompact {
        timeout: compact_timeout(auto, facts.run_timeout),
    }
}

/// Whether the Auto-compact budget for the current session has been spent (#33).
pub fn budget_spent(entry: &SessionEntry) -> bool {
    entry.auto_compact_state().spent_at.is_some()
}

/// How many milliseconds ago the budget was spent, for `run.recovery_skipped` (#33).
pub fn spent_ms_ago(entry: &SessionEntry, now: DateTime<Utc>) -> u64 {
    entry
        .auto_compact_state()
        .spent_at
        .map(|at| {
            u64::try_from(now.signed_duration_since(at).num_milliseconds().max(0)).unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Whether the Timeout breaker is on: at least one limit set (#32). Both absent turns it off.
pub fn breaker_enabled(settings: &TimeoutBreakerSettings) -> bool {
    settings.max_consecutive_timeouts.is_some() || settings.max_failing_seconds.is_some()
}

/// The seconds limit, when the failing clock has passed it.
fn failing_seconds(
    settings: &TimeoutBreakerSettings,
    failing_since: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<u64> {
    let limit = settings.max_failing_seconds?;
    let since = failing_since?;
    (now.signed_duration_since(since).num_seconds() >= i64::try_from(limit).unwrap_or(i64::MAX))
        .then_some(limit)
}

/// Whether the limits trip for the given count and clock (#32).
fn limits_trip(
    count: u32,
    failing_since: Option<DateTime<Utc>>,
    settings: &TimeoutBreakerSettings,
    now: DateTime<Utc>,
) -> bool {
    settings
        .max_consecutive_timeouts
        .is_some_and(|max| count >= max)
        || failing_seconds(settings, failing_since, now).is_some()
}

/// The limit value that tripped, for `session.breaker_open { limit }` (#32): the count limit when
/// the count tripped, else the seconds limit.
fn tripped_limit(
    count: u32,
    failing_since: Option<DateTime<Utc>>,
    settings: &TimeoutBreakerSettings,
    now: DateTime<Utc>,
) -> u64 {
    if let Some(max) = settings.max_consecutive_timeouts
        && count >= max
    {
        return u64::from(max);
    }
    failing_seconds(settings, failing_since, now).unwrap_or(0)
}

/// Whether the Timeout breaker is open for `entry`, evaluated against the current settings (#32).
///
/// An open latch stays open; otherwise the count and clock are evaluated now. Turning the breaker
/// off (`!breaker_enabled`) reports it closed.
pub fn breaker_open(
    entry: &SessionEntry,
    settings: &TimeoutBreakerSettings,
    now: DateTime<Utc>,
) -> bool {
    if !breaker_enabled(settings) {
        return false;
    }
    let state = entry.timeout_breaker_state();
    state.open
        || limits_trip(
            state.consecutive_timeouts,
            state.failing_since,
            settings,
            now,
        )
}

/// Why the Timeout breaker closed (#32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Success,
    SessionChanged,
    Disabled,
}

impl CloseReason {
    /// The `reason` the `session.breaker_closed` event carries.
    pub const fn as_str(self) -> &'static str {
        match self {
            CloseReason::Success => "success",
            CloseReason::SessionChanged => "session_changed",
            CloseReason::Disabled => "disabled",
        }
    }
}

/// The fields of a `session.breaker_open` (#32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakerOpening {
    /// The limit that tripped: the count limit, or the seconds limit.
    pub limit: u64,
    pub consecutive_timeouts: u32,
    /// Milliseconds since `failing_since`.
    pub failing_ms: u64,
}

/// What folding a finished Turn into a session's breaker did (#32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BreakerEvent {
    /// Some when the breaker opened on this Turn.
    pub opened: Option<BreakerOpening>,
    /// Some when it closed on this Turn.
    pub closed: Option<CloseReason>,
}

/// How a Turn's final outcome is classified (#32 §2).
enum TurnClass {
    Success,
    Timeout,
    OtherFailure,
    Interrupted,
}

fn classify(outcome: &RunOutcome) -> TurnClass {
    match outcome.verdict {
        Verdict::Ok => TurnClass::Success,
        Verdict::Timeout => TurnClass::Timeout,
        Verdict::Interrupted => TurnClass::Interrupted,
        Verdict::Failed(_) | Verdict::RateLimited { .. } | Verdict::Refused(_) => {
            TurnClass::OtherFailure
        }
    }
}

/// Folds a finished Turn into `entry`'s Timeout breaker, once at the Turn's end after any
/// recovery (#32 §2). The Turn's final outcome decides: a success clears everything; a timeout
/// counts and may open; any other failure resets the count but keeps the clock; an interrupted
/// Turn changes nothing.
pub fn apply_turn(
    entry: &mut SessionEntry,
    outcome: &RunOutcome,
    started_at: DateTime<Utc>,
    now: DateTime<Utc>,
    settings: &TimeoutBreakerSettings,
) -> BreakerEvent {
    let class = classify(outcome);
    if matches!(class, TurnClass::Interrupted) {
        return BreakerEvent::default();
    }
    if !breaker_enabled(settings) {
        // Turning the breaker off closes every open breaker (#32 §2).
        let was_open = entry.timeout_breaker_state().open;
        entry.timeout_breaker = None;
        return BreakerEvent {
            opened: None,
            closed: was_open.then_some(CloseReason::Disabled),
        };
    }
    match class {
        TurnClass::Success => {
            let was_open = entry.timeout_breaker_state().open;
            entry.timeout_breaker = None;
            BreakerEvent {
                opened: None,
                closed: was_open.then_some(CloseReason::Success),
            }
        }
        TurnClass::OtherFailure => {
            let session_id = entry.session_id.clone();
            let breaker = entry.timeout_breaker.get_or_insert_with(|| TimeoutBreaker {
                session_id: session_id.clone(),
                consecutive_timeouts: 0,
                failing_since: None,
                opened_at: None,
            });
            if breaker.session_id != session_id {
                *breaker = TimeoutBreaker {
                    session_id,
                    consecutive_timeouts: 0,
                    failing_since: None,
                    opened_at: None,
                };
            }
            breaker.consecutive_timeouts = 0;
            if breaker.failing_since.is_none() {
                breaker.failing_since = Some(started_at);
            }
            BreakerEvent::default()
        }
        TurnClass::Timeout => {
            let session_id = entry.session_id.clone();
            let breaker = entry.timeout_breaker.get_or_insert_with(|| TimeoutBreaker {
                session_id: session_id.clone(),
                consecutive_timeouts: 0,
                failing_since: None,
                opened_at: None,
            });
            if breaker.session_id != session_id {
                *breaker = TimeoutBreaker {
                    session_id,
                    consecutive_timeouts: 0,
                    failing_since: None,
                    opened_at: None,
                };
            }
            breaker.consecutive_timeouts = breaker.consecutive_timeouts.saturating_add(1);
            if breaker.failing_since.is_none() {
                breaker.failing_since = Some(started_at);
            }
            let was_open = breaker.opened_at.is_some();
            let mut event = BreakerEvent::default();
            if !was_open
                && limits_trip(
                    breaker.consecutive_timeouts,
                    breaker.failing_since,
                    settings,
                    now,
                )
            {
                breaker.opened_at = Some(now);
                event.opened = Some(BreakerOpening {
                    limit: tripped_limit(
                        breaker.consecutive_timeouts,
                        breaker.failing_since,
                        settings,
                        now,
                    ),
                    consecutive_timeouts: breaker.consecutive_timeouts,
                    failing_ms: failing_ms(breaker.failing_since, now),
                });
            }
            event
        }
        TurnClass::Interrupted => BreakerEvent::default(),
    }
}

fn failing_ms(failing_since: Option<DateTime<Utc>>, now: DateTime<Utc>) -> u64 {
    failing_since
        .map(|since| {
            u64::try_from(now.signed_duration_since(since).num_milliseconds().max(0)).unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Clears a session's breaker and Auto-compact budget because its `session_id` changed (#32, #33):
/// a `/reset`, a rotation, a stale-session or thinking-signature recovery, or firing a thread.
/// Returns the breaker close, when it was open.
pub fn session_changed(entry: &mut SessionEntry) -> BreakerEvent {
    let was_open = entry.timeout_breaker_state().open;
    entry.timeout_breaker = None;
    entry.auto_compact = None;
    BreakerEvent {
        opened: None,
        closed: was_open.then_some(CloseReason::SessionChanged),
    }
}

/// Re-arms the Auto-compact budget after a Turn whose final outcome is `ok` (#33 §2). A manual
/// `/compact` and other failures do not.
pub fn rearm_budget(entry: &mut SessionEntry) {
    if let Some(budget) = entry
        .auto_compact
        .as_mut()
        .filter(|b| b.session_id == entry.session_id)
    {
        budget.spent_at = None;
    }
}

/// Records a finished `/compact` run for the session's Management API record (#33).
pub fn record_compact(
    entry: &mut SessionEntry,
    at: DateTime<Utc>,
    outcome: IndexOutcome,
    duration_ms: u64,
) {
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
    budget.last = Some(claudeclaw_api::sessions::AutoCompactLast {
        at,
        outcome,
        duration_ms,
    });
}

/// The context meter from a finished run's own stream (#39 D4): the last main-thread assistant
/// `usage`, and the main model's `contextWindow`. `None` when either is missing.
pub fn context(outcome: &RunOutcome) -> Option<SessionContext> {
    let used_tokens = outcome.last_main_usage?.context_tokens();
    let window = outcome.context_window()?;
    Some(SessionContext {
        used_tokens,
        window,
    })
}

/// A finished run's cost by difference from the session's baseline (#39 D8).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CostDelta {
    /// The run's own cost, or `None` when there is no baseline to difference against
    /// (`cost_status: no_baseline`) or the run reported no cost.
    pub cost_usd: Option<f64>,
    /// Per-model figures for the run.
    pub by_model: BTreeMap<String, ModelCost>,
    /// Whether a baseline existed and the figure was priced against it.
    pub priced: bool,
    /// The reported cumulative figure was below the baseline, so it was taken as the run's own
    /// figure and `usage.baseline_reset` is logged (#39 D8).
    pub baseline_reset: bool,
}

/// The per-run cost of `outcome` against `previous`, the session's Cost baseline (#39 D8).
pub fn per_run_cost(outcome: &RunOutcome, previous: Option<&CostState>) -> CostDelta {
    let Some(reported) = outcome.total_cost_usd else {
        return CostDelta::default();
    };
    let Some(baseline) = previous else {
        return CostDelta::default();
    };
    let reset = reported < baseline.cost_usd;
    let by_model = if reset {
        per_model(outcome, None)
    } else {
        per_model(outcome, Some(baseline))
    };
    CostDelta {
        cost_usd: Some(if reset {
            reported
        } else {
            reported - baseline.cost_usd
        }),
        by_model,
        priced: true,
        baseline_reset: reset,
    }
}

/// The Cost baseline of a brand-new session: zero (#39 D8). A fresh session's `--resume`-less
/// run reports only its own cost, so the next run differences against zero and the first fresh
/// run's cost is the reported figure. An imported session has no baseline at all, so its first
/// resumed run's cost is `null`.
pub fn zero_baseline() -> CostState {
    CostState {
        cost_usd: 0.0,
        by_model: BTreeMap::new(),
    }
}

/// The session-cumulative figure a finished run leaves for the next run's baseline (#39 D8).
pub fn baseline_from_outcome(outcome: &RunOutcome) -> Option<CostState> {
    let cost_usd = outcome.total_cost_usd?;
    Some(CostState {
        cost_usd,
        by_model: per_model(outcome, None),
    })
}

/// `outcome.model_usage` as [`ModelCost`]s, differenced against `baseline.by_model` when given.
fn per_model(outcome: &RunOutcome, baseline: Option<&CostState>) -> BTreeMap<String, ModelCost> {
    outcome
        .model_usage
        .iter()
        .map(|(model, usage)| {
            let base = baseline.and_then(|state| state.by_model.get(model));
            (model.clone(), model_cost(usage, base))
        })
        .collect()
}

fn model_cost(usage: &ModelUsage, base: Option<&ModelCost>) -> ModelCost {
    ModelCost {
        cost_usd: usage.cost_usd.unwrap_or(0.0) - base.map_or(0.0, |b| b.cost_usd),
        tokens_in: usage.input_tokens - base.map_or(0, |b| b.tokens_in),
        tokens_out: usage.output_tokens - base.map_or(0, |b| b.tokens_out),
        cache_read_tokens: usage.cache_read_input_tokens - base.map_or(0, |b| b.cache_read_tokens),
        cache_write_tokens: usage.cache_creation_input_tokens
            - base.map_or(0, |b| b.cache_write_tokens),
    }
}

/// A session's status under the Scope's effective Launcher (management-api §2.5, #8): orphaned
/// overrides both, else in use for the current Launcher, else parked.
pub fn status(entry: &SessionEntry, current_launcher: &str) -> SessionStatus {
    if entry.orphaned_since.is_some() {
        SessionStatus::Orphaned
    } else if entry.launcher == current_launcher {
        SessionStatus::InUse
    } else {
        SessionStatus::Parked
    }
}

/// When the 30-day prune removes `entry`; `None` while it isn't orphaned (#12 §6, #44).
pub fn prune_at(entry: &SessionEntry) -> Option<DateTime<Utc>> {
    entry.orphaned_since.map(|since| since + ORPHAN_RETENTION)
}

/// Whether an orphaned session's state is old enough to prune at `now` (#12 §6, #44).
pub fn orphan_expired(entry: &SessionEntry, now: DateTime<Utc>) -> bool {
    entry
        .orphaned_since
        .is_some_and(|since| now - since >= ORPHAN_RETENTION)
}

/// Whether a `session_not_found` means the Launcher's config dir moved (#39 D5): the session
/// still has a stored Transcript path, and that file is still on disk.
pub fn root_moved(entry: &SessionEntry, transcript_exists: bool) -> bool {
    entry.transcript.is_some() && transcript_exists
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use claudeclaw_runner::stream::RunClock;
    use claudeclaw_store::sessions::{TranscriptLocation, TranscriptVia};

    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, hour, 0, 0).unwrap()
    }

    fn base(session_id: &str) -> SessionEntry {
        SessionEntry::new(
            claudeclaw_api::scope::Scope::Discord("1234".into()),
            "direct",
            session_id,
            at(8),
        )
    }

    fn outcome(verdict: Verdict, exit_code: Option<i32>, lingered: bool) -> RunOutcome {
        RunOutcome {
            verdict,
            exit_code,
            signal: None,
            lingered,
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

    fn breaker(
        consecutive: u32,
        failing_since: Option<DateTime<Utc>>,
        opened_at: Option<DateTime<Utc>>,
        session_id: &str,
    ) -> TimeoutBreaker {
        TimeoutBreaker {
            session_id: session_id.into(),
            consecutive_timeouts: consecutive,
            failing_since,
            opened_at,
        }
    }

    fn auto(enabled: bool, timeout_secs: u32) -> AutoCompactSettings {
        AutoCompactSettings {
            enabled,
            timeout_secs,
        }
    }

    fn limits(count: Option<u32>, secs: Option<u64>) -> TimeoutBreakerSettings {
        TimeoutBreakerSettings {
            max_consecutive_timeouts: count,
            max_failing_seconds: secs,
        }
    }

    fn facts<'a>(outcome: &'a RunOutcome, is_new: bool, run_timeout_secs: u64) -> RunFacts<'a> {
        RunFacts {
            outcome,
            is_new,
            run_timeout: Duration::from_secs(run_timeout_secs),
            started_at: at(9),
        }
    }

    #[test]
    fn timeout_is_only_the_runners_own_kill() {
        assert!(counts_as_timeout(&outcome(
            Verdict::Timeout,
            Some(1),
            false
        )));
        // A Launcher that exits 124 on its own is an ordinary failure (#32).
        assert!(!counts_as_timeout(&outcome(
            Verdict::Failed(Failure::NoResult),
            Some(124),
            false
        )));
        // A `lingered` run read its `result` before the kill (#47).
        assert!(!counts_as_timeout(&outcome(Verdict::Ok, Some(0), true)));
    }

    #[test]
    fn recovery_covers_every_branch() {
        let ok = outcome(Verdict::Ok, Some(0), false);
        let stale = outcome(Verdict::Failed(Failure::SessionNotFound), Some(1), false);
        let signature = outcome(Verdict::Failed(Failure::ThinkingSignature), Some(1), false);
        let limit = outcome(Verdict::RateLimited { reset: None }, Some(1), false);
        let failed = outcome(Verdict::Failed(Failure::NoResult), Some(1), false);
        let refused = outcome(
            Verdict::Refused(claudeclaw_runner::stream::Refusal::LauncherExecFailed {
                exit_code: 127,
            }),
            Some(127),
            false,
        );
        let interrupted = outcome(Verdict::Interrupted, Some(143), false);
        let timed_out = outcome(Verdict::Timeout, Some(1), false);
        let breaker_off = limits(None, None);
        let compact_on = auto(true, 300);

        let cases: Vec<(&str, Option<SessionEntry>, RunFacts<'_>, Recovery)> = vec![
            ("ok", None, facts(&ok, false, 900), Recovery::None),
            (
                "a stale resumed session",
                Some(base("s-1")),
                facts(&stale, false, 900),
                Recovery::NewSession(NewSessionReason::StaleSession),
            ),
            (
                "a stale session that was new this Turn is not recovered",
                None,
                facts(&stale, true, 900),
                Recovery::None,
            ),
            (
                "a corrupt resumed session",
                Some(base("s-1")),
                facts(&signature, false, 900),
                Recovery::NewSession(NewSessionReason::ThinkingSignature),
            ),
            (
                "a rate limit",
                Some(base("s-1")),
                facts(&limit, false, 900),
                Recovery::RateLimitFallback,
            ),
            (
                "an ordinary failure",
                Some(base("s-1")),
                facts(&failed, false, 900),
                Recovery::None,
            ),
            (
                "a refusal",
                Some(base("s-1")),
                facts(&refused, false, 900),
                Recovery::None,
            ),
            (
                "an interruption",
                Some(base("s-1")),
                facts(&interrupted, false, 900),
                Recovery::None,
            ),
            (
                "a timed-out resumed Turn compacts",
                Some(base("s-1")),
                facts(&timed_out, false, 900),
                Recovery::AutoCompact {
                    timeout: Duration::from_secs(300),
                },
            ),
            (
                "a timed-out new Turn skips with new_session",
                None,
                facts(&timed_out, true, 900),
                Recovery::Skipped(SkipReason::NewSession),
            ),
        ];
        for (name, entry, f, want) in cases {
            assert_eq!(
                recovery(entry.as_ref(), &f, &breaker_off, &compact_on, at(9)),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn timeout_recovery_skip_order() {
        let timed_out = outcome(Verdict::Timeout, Some(1), false);
        let on = limits(Some(3), None);
        let compact_on = auto(true, 300);
        let facts = facts(&timed_out, false, 900);

        // breaker before disabled and budget.
        let mut open = base("s-1");
        open.timeout_breaker = Some(breaker(3, Some(at(8)), Some(at(8)), "s-1"));
        assert_eq!(
            recovery(Some(&open), &facts, &on, &auto(false, 300), at(9)),
            Recovery::Skipped(SkipReason::TimeoutBreaker)
        );
        // disabled before budget.
        let mut spent = base("s-1");
        spent.auto_compact = Some(AutoCompact {
            session_id: "s-1".into(),
            spent_at: Some(at(8)),
            last: None,
        });
        assert_eq!(
            recovery(Some(&spent), &facts, &on, &auto(false, 300), at(9)),
            Recovery::Skipped(SkipReason::Disabled)
        );
        assert_eq!(
            recovery(Some(&spent), &facts, &on, &compact_on, at(9)),
            Recovery::Skipped(SkipReason::BudgetSpent)
        );
    }

    #[test]
    fn the_compact_timeout_is_capped_by_the_run_timeout() {
        assert_eq!(
            compact_timeout(&auto(true, 300), Duration::from_secs(900)),
            Duration::from_secs(300)
        );
        assert_eq!(
            compact_timeout(&auto(true, 300), Duration::from_secs(300)),
            Duration::from_secs(300)
        );
        assert_eq!(
            compact_timeout(&auto(true, 300), Duration::from_secs(290)),
            Duration::from_secs(290)
        );
    }

    #[test]
    fn breaker_counting_and_open() {
        let on = limits(Some(2), None);
        let mut entry = base("s-1");
        entry.session_id = "s-1".into();
        let timeout = outcome(Verdict::Timeout, Some(1), false);
        let ok = outcome(Verdict::Ok, Some(0), false);
        let failed = outcome(Verdict::Failed(Failure::NoResult), Some(1), false);
        let interrupted = outcome(Verdict::Interrupted, Some(143), false);

        // An interruption changes nothing.
        assert_eq!(
            apply_turn(&mut entry, &interrupted, at(9), at(9), &on),
            BreakerEvent::default()
        );
        assert!(entry.timeout_breaker.is_none());

        // First timeout counts but does not open.
        let first = apply_turn(&mut entry, &timeout, at(9), at(9), &on);
        assert_eq!(first.opened, None);
        assert_eq!(
            entry.timeout_breaker.as_ref().unwrap().consecutive_timeouts,
            1
        );
        assert_eq!(
            entry.timeout_breaker.as_ref().unwrap().failing_since,
            Some(at(9))
        );

        // Second timeout opens and latches.
        let second = apply_turn(&mut entry, &timeout, at(10), at(10), &on);
        assert_eq!(
            second.opened,
            Some(BreakerOpening {
                limit: 2,
                consecutive_timeouts: 2,
                failing_ms: 60 * 60 * 1000
            })
        );
        assert!(breaker_open(&entry, &on, at(10)));

        // An ordinary failure resets the count but keeps the latch and the clock.
        let other = apply_turn(&mut entry, &failed, at(11), at(11), &on);
        assert_eq!(other, BreakerEvent::default());
        let state = entry.timeout_breaker_state();
        assert_eq!(state.consecutive_timeouts, 0);
        assert_eq!(state.failing_since, Some(at(9)));
        assert!(state.open, "the latch holds through another failure");

        // A success closes and clears.
        let closed = apply_turn(&mut entry, &ok, at(12), at(12), &on);
        assert_eq!(closed.closed, Some(CloseReason::Success));
        assert!(entry.timeout_breaker.is_none());
        assert!(!breaker_open(&entry, &on, at(12)));
    }

    #[test]
    fn a_lingered_or_exit_124_run_never_opens_the_breaker() {
        let on = limits(Some(1), None);
        let mut entry = base("s-1");
        let exit_124 = outcome(Verdict::Failed(Failure::NoResult), Some(124), false);
        let lingered = outcome(Verdict::Ok, Some(0), true);
        assert_eq!(
            apply_turn(&mut entry, &exit_124, at(9), at(9), &on).opened,
            None
        );
        assert_eq!(
            apply_turn(&mut entry, &lingered, at(9), at(9), &on).opened,
            None
        );
        assert!(entry.timeout_breaker.is_none());
    }

    #[test]
    fn the_seconds_limit_can_open_the_breaker() {
        let settings = limits(None, Some(3600));
        let timeout = outcome(Verdict::Timeout, Some(1), false);
        let mut entry = base("s-1");
        apply_turn(&mut entry, &timeout, at(8), at(8), &settings);
        // One second short of the limit: still closed.
        assert!(!breaker_open(
            &entry,
            &settings,
            at(8) + TimeDelta::seconds(3599)
        ));
        // The limit reached: open.
        assert!(breaker_open(&entry, &settings, at(9)));

        // A timeout that reaches the limit opens and records the seconds limit.
        let mut entry = base("s-1");
        apply_turn(&mut entry, &timeout, at(8), at(8), &settings);
        let event = apply_turn(&mut entry, &timeout, at(8), at(9), &settings);
        assert_eq!(event.opened.unwrap().limit, 3600);
    }

    #[test]
    fn turning_the_breaker_off_closes_an_open_one() {
        let on = limits(Some(1), None);
        let off = limits(None, None);
        let timeout = outcome(Verdict::Timeout, Some(1), false);
        let mut entry = base("s-1");
        assert!(
            apply_turn(&mut entry, &timeout, at(9), at(9), &on)
                .opened
                .is_some()
        );
        let closed = apply_turn(
            &mut entry,
            &outcome(Verdict::Ok, Some(0), false),
            at(10),
            at(10),
            &off,
        );
        assert_eq!(closed.closed, Some(CloseReason::Disabled));
        assert!(entry.timeout_breaker.is_none());
    }

    #[test]
    fn a_session_change_clears_the_breaker_and_budget() {
        let mut entry = base("s-1");
        entry.timeout_breaker = Some(breaker(2, Some(at(8)), Some(at(8)), "s-1"));
        entry.auto_compact = Some(AutoCompact {
            session_id: "s-1".into(),
            spent_at: Some(at(8)),
            last: None,
        });
        let event = session_changed(&mut entry);
        assert_eq!(event.closed, Some(CloseReason::SessionChanged));
        assert!(entry.timeout_breaker.is_none() && entry.auto_compact.is_none());
    }

    #[test]
    fn a_success_rearms_the_budget_but_keeps_the_last_compact() {
        let mut entry = base("s-1");
        entry.auto_compact = Some(AutoCompact {
            session_id: "s-1".into(),
            spent_at: Some(at(8)),
            last: Some(claudeclaw_api::sessions::AutoCompactLast {
                at: at(8),
                outcome: IndexOutcome::Timeout,
                duration_ms: 1,
            }),
        });
        rearm_budget(&mut entry);
        let state = entry.auto_compact_state();
        assert_eq!(state.spent_at, None);
        assert!(state.last.is_some());
    }

    #[test]
    fn cost_is_differenced_against_the_baseline() {
        let mut first = outcome(Verdict::Ok, Some(0), false);
        first.total_cost_usd = Some(0.003);
        first.model_usage = BTreeMap::from([(
            "m-1".to_owned(),
            ModelUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 2,
                cache_creation_input_tokens: 1,
                cost_usd: Some(0.003),
                ..ModelUsage::default()
            },
        )]);

        let baseline = baseline_from_outcome(&first).unwrap();
        assert_eq!(baseline.cost_usd, 0.003);

        let mut resumed = outcome(Verdict::Ok, Some(0), false);
        resumed.total_cost_usd = Some(0.006);
        resumed.model_usage = BTreeMap::from([(
            "m-1".to_owned(),
            ModelUsage {
                input_tokens: 20,
                output_tokens: 10,
                cache_read_input_tokens: 4,
                cache_creation_input_tokens: 2,
                cost_usd: Some(0.006),
                ..ModelUsage::default()
            },
        )]);
        let delta = per_run_cost(&resumed, Some(&baseline));
        assert!(delta.priced && !delta.baseline_reset);
        assert_eq!(delta.cost_usd, Some(0.003));
        let model = &delta.by_model["m-1"];
        assert_eq!(model.tokens_in, 10);
        assert_eq!(model.cache_write_tokens, 1);
    }

    #[test]
    fn a_baseline_that_did_not_restore_resets_it() {
        let baseline = CostState {
            cost_usd: 0.01,
            by_model: BTreeMap::new(),
        };
        let mut run = outcome(Verdict::Ok, Some(0), false);
        run.total_cost_usd = Some(0.002);
        let delta = per_run_cost(&run, Some(&baseline));
        assert!(delta.baseline_reset);
        assert_eq!(delta.cost_usd, Some(0.002));
    }

    #[test]
    fn a_run_without_a_baseline_is_unpriced() {
        let mut run = outcome(Verdict::Ok, Some(0), false);
        run.total_cost_usd = Some(0.003);
        let delta = per_run_cost(&run, None);
        assert_eq!(delta.cost_usd, None);
        assert!(!delta.priced && !delta.baseline_reset);
    }

    #[test]
    fn parking_and_orphaning_decide_the_status() {
        let entry = base("s-1");
        assert_eq!(status(&entry, "direct"), SessionStatus::InUse);
        assert_eq!(status(&entry, "/opt/other"), SessionStatus::Parked);

        let mut orphan = base("s-1");
        orphan.orphaned_since = Some(at(8));
        assert_eq!(status(&orphan, "direct"), SessionStatus::Orphaned);
        assert_eq!(prune_at(&orphan), Some(at(8) + ORPHAN_RETENTION));
        assert_eq!(prune_at(&base("s-1")), None);
    }

    #[test]
    fn orphans_prune_at_thirty_days() {
        let mut case = base("s-1");
        case.orphaned_since = Some(at(0));
        // Twenty-nine days: kept.
        assert!(!orphan_expired(&case, at(0) + TimeDelta::days(29)));
        // Thirty days: pruned.
        assert!(orphan_expired(&case, at(0) + TimeDelta::days(30)));
        // Thirty-one days: pruned.
        assert!(orphan_expired(&case, at(0) + TimeDelta::days(31)));
        // A live session never expires.
        assert!(!orphan_expired(&base("s-1"), at(0) + TimeDelta::days(400)));
    }

    #[test]
    fn root_moved_needs_a_stored_path_that_still_exists() {
        let mut entry = base("s-1");
        assert!(!root_moved(&entry, true));
        entry.transcript = Some(TranscriptLocation {
            path: "/cfg/projects/p/s-1.jsonl".into(),
            via: TranscriptVia::Init,
            located_at: at(8),
        });
        assert!(root_moved(&entry, true));
        assert!(!root_moved(&entry, false));
    }

    #[test]
    fn context_comes_from_the_runs_own_stream() {
        let mut run = outcome(Verdict::Ok, Some(0), false);
        assert_eq!(context(&run), None);
        run.last_main_usage = Some(claudeclaw_runner::stream::Usage {
            input_tokens: 100,
            output_tokens: 9,
            cache_read_input_tokens: 20,
            cache_creation_input_tokens: 5,
        });
        run.model_usage = BTreeMap::from([(
            "m-1".to_owned(),
            ModelUsage {
                context_window: Some(200_000),
                ..ModelUsage::default()
            },
        )]);
        run.model = Some("m-1".into());
        assert_eq!(
            context(&run),
            Some(SessionContext {
                used_tokens: 125,
                window: 200_000
            })
        );
    }
}
