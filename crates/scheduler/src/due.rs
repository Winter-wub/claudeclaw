//! Which slot of a job is due: next fire, catch-up and the took-effect cut-off (#9, #50).
//! Pure functions, so the rules are tested without a runtime.

use std::time::Duration;

use chrono::{DateTime, FixedOffset, Utc};
use claudeclaw_jobs::Schedule;

/// How far back a missed slot is still fired: 6 h (#9; TS `CATCHUP_WINDOW_MINUTES = 360`,
/// `src/commands/start.ts:887`, inclusive as in `prevCronMatch`, `src/cron.ts:71-84`). Also
/// bounds interrupted re-runs and persisted retries.
pub const CATCH_UP_WINDOW: Duration = Duration::from_secs(6 * 3600);

/// A One-shot job's build-time grace (#50): its single slot still runs if it passed at most
/// this long before the job took effect.
pub const ONE_SHOT_GRACE: Duration = Duration::from_secs(10 * 60);

/// A fire this late or later is logged as `catch_up: true`. TS fired on time only in the
/// slot's own minute and treated anything later as a missed run (`src/commands/start.ts:997-1006`).
pub const CATCH_UP_AFTER: Duration = Duration::from_secs(60);

pub(crate) fn chrono(d: Duration) -> chrono::Duration {
    chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}

/// What the due check needs to know about one scheduled job.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SlotRule<'a> {
    pub schedule: &'a Schedule,
    pub one_shot: bool,
    /// When the job's current schedule took effect (#50).
    pub took_effect: DateTime<Utc>,
    /// The latest instant already covered: the last run's start, or the last slot this Worker
    /// handled (fired or skipped). Only slots strictly after it are due.
    pub covered_through: Option<DateTime<Utc>>,
}

impl SlotRule<'_> {
    /// The slot due at `now`, if any: the latest slot at or before `now` (croner's
    /// `find_previous_occurrence`) that is within [`CATCH_UP_WINDOW`], after
    /// `covered_through`, and not before the schedule took effect (less [`ONE_SHOT_GRACE`] for a
    /// One-shot job). Only the latest slot can be due, so a long gap fires once (#9).
    pub fn due(&self, now: DateTime<Utc>, offset: FixedOffset) -> Option<DateTime<Utc>> {
        let slot = self
            .schedule
            .previous_fire(now.with_timezone(&offset))?
            .with_timezone(&Utc);
        if now - slot > chrono(CATCH_UP_WINDOW) {
            return None;
        }
        if self.covered_through.is_some_and(|c| slot <= c) {
            return None;
        }
        let cutoff = if self.one_shot {
            self.took_effect - chrono(ONE_SHOT_GRACE)
        } else {
            self.took_effect
        };
        (slot >= cutoff).then_some(slot)
    }
}

/// The first slot strictly after `now`, for the timer.
pub(crate) fn next_slot(
    schedule: &Schedule,
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> Option<DateTime<Utc>> {
    schedule
        .next_fire(now.with_timezone(&offset))
        .map(|t| t.with_timezone(&Utc))
}

/// Whether a fire at `now` for `slot` counts as catch-up.
pub(crate) fn is_catch_up(slot: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - slot >= chrono(CATCH_UP_AFTER)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn at(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, d, h, m, 0).unwrap()
    }

    fn daily() -> Schedule {
        Schedule::parse("0 9 * * *").unwrap()
    }

    fn rule(schedule: &Schedule, took_effect: DateTime<Utc>) -> SlotRule<'_> {
        SlotRule {
            schedule,
            one_shot: false,
            took_effect,
            covered_through: None,
        }
    }

    #[test]
    fn next_slot_is_strictly_after() {
        let s = daily();
        assert_eq!(next_slot(&s, at(2, 8, 0), utc()), Some(at(2, 9, 0)));
        assert_eq!(next_slot(&s, at(2, 9, 0), utc()), Some(at(3, 9, 0)));
        let bangkok = FixedOffset::east_opt(7 * 3600).unwrap();
        assert_eq!(next_slot(&s, at(2, 0, 0), bangkok), Some(at(2, 2, 0)));
    }

    #[test]
    fn the_slot_is_due_at_its_own_instant() {
        let s = daily();
        let r = rule(&s, at(1, 0, 0));
        assert_eq!(r.due(at(2, 9, 0), utc()), Some(at(2, 9, 0)));
        // Yesterday's slot is outside the window.
        assert_eq!(r.due(at(2, 8, 59), utc()), None);
    }

    /// TS `prevCronMatch(…, 360)` looks back 360 minutes inclusive (`src/cron.ts:71-84`).
    #[test]
    fn catch_up_window_is_six_hours() {
        let s = daily();
        let r = rule(&s, at(1, 0, 0));
        assert_eq!(r.due(at(2, 15, 0), utc()), Some(at(2, 9, 0)));
        assert_eq!(r.due(at(2, 15, 1), utc()), None);
    }

    /// TS `if (lastRan >= due.getTime()) continue` (`src/commands/start.ts:1000`).
    #[test]
    fn a_covered_slot_is_not_due() {
        let s = daily();
        let mut r = rule(&s, at(1, 0, 0));
        r.covered_through = Some(at(2, 9, 0));
        assert_eq!(r.due(at(2, 10, 0), utc()), None);
        r.covered_through = Some(at(2, 8, 59));
        assert_eq!(r.due(at(2, 10, 0), utc()), Some(at(2, 9, 0)));
    }

    /// #50: a daily 09:00 job created or edited at 10:00 first runs the next day.
    #[test]
    fn slots_before_the_schedule_took_effect_are_not_due() {
        let s = daily();
        let r = rule(&s, at(2, 10, 0));
        assert_eq!(r.due(at(2, 10, 0), utc()), None);
        assert_eq!(r.due(at(3, 9, 0), utc()), Some(at(3, 9, 0)));
        // Took effect before the slot (the Daemon was down at 09:00): caught up.
        let r = rule(&s, at(2, 8, 0));
        assert_eq!(r.due(at(2, 11, 0), utc()), Some(at(2, 9, 0)));
    }

    /// #50: a One-shot job's slot that passed shortly before it took effect still runs.
    #[test]
    fn one_shot_grace() {
        let s = Schedule::parse("55 9 2 3 *").unwrap();
        let mut r = rule(&s, at(2, 10, 0));
        r.one_shot = true;
        assert_eq!(r.due(at(2, 10, 0), utc()), Some(at(2, 9, 55)));
        let s = Schedule::parse("45 9 2 3 *").unwrap();
        let mut r = rule(&s, at(2, 10, 0));
        r.one_shot = true;
        assert_eq!(r.due(at(2, 10, 0), utc()), None, "15 min is past the grace");
        let s = Schedule::parse("55 9 2 3 *").unwrap();
        let r = rule(&s, at(2, 10, 0));
        assert_eq!(
            r.due(at(2, 10, 0), utc()),
            None,
            "recurring jobs get no grace"
        );
    }

    #[test]
    fn catch_up_flag() {
        assert!(!is_catch_up(at(2, 9, 0), at(2, 9, 0)));
        assert!(!is_catch_up(
            at(2, 9, 0),
            at(2, 9, 0) + chrono::Duration::seconds(59)
        ));
        assert!(is_catch_up(at(2, 9, 0), at(2, 9, 1)));
    }
}
