//! The cron dialect (#12 §1): croner 4 with standard semantics.
//!
//! A schedule is exactly 5 fields (minute, hour, day-of-month, month, day-of-week) or one of
//! croner's nicknames (`@yearly`, `@annually`, `@monthly`, `@weekly`, `@daily`, `@hourly`).
//! Within that, everything croner accepts is valid: lists, ranges, steps, month and weekday
//! names, `0`/`7` for Sunday, `?`, `L`, `W`, `#`, and `+` before day-of-week to ask for AND.
//! Day-of-month and day-of-week are otherwise ORed, as POSIX cron does, and `*/n` counts from
//! the field minimum.

use std::fmt;

use chrono::{DateTime, FixedOffset};
use croner::Cron;
use croner::parser::{CronParser, Seconds, Year};

/// A parsed schedule: the text as written, and croner's reading of it (#12 §1).
#[derive(Clone)]
pub struct Schedule {
    text: String,
    cron: Cron,
}

/// Why a schedule is rejected (#12 §1). The message is croner's, so it can quote the
/// expression.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ScheduleError(String);

/// The one parser configuration #12 §1 decides: no seconds or year field, standard OR day
/// matching, and no sloppy ranges (`5/15` is an error, not 5, 20, 35, 50).
fn parser() -> CronParser {
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .dom_and_dow(false)
        .sloppy_ranges(false)
        .build()
}

impl Schedule {
    /// Parses a schedule in #12 §1's dialect. Surrounding whitespace is ignored; the text is
    /// otherwise kept as written.
    pub fn parse(text: &str) -> Result<Self, ScheduleError> {
        let text = text.trim();
        let cron = parser()
            .parse(text)
            .map_err(|e| ScheduleError(e.to_string()))?;
        Ok(Schedule {
            text: text.to_owned(),
            cron,
        })
    }

    /// The schedule as written in the job file, trimmed.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The first fire strictly after `from`, on `from`'s wall clock. `None` when the schedule
    /// never fires again (for example `0 0 30 2 *`).
    pub fn next_fire(&self, from: DateTime<FixedOffset>) -> Option<DateTime<FixedOffset>> {
        self.cron.find_next_occurrence(&from, false).ok()
    }

    /// The next `n` fires strictly after `from`, on `from`'s wall clock (the Daemon's time zone
    /// offset, architecture §4 `scheduler`). Shorter than `n` only when the schedule runs out.
    pub fn next_fires(&self, from: DateTime<FixedOffset>, n: usize) -> Vec<DateTime<FixedOffset>> {
        self.cron.iter_after(from).take(n).collect()
    }

    /// The latest fire at or before `at`, for catch-up (#4, #9): the scheduler compares it with
    /// the job's last run and the 6 h window. Uses croner's `find_previous_occurrence`.
    pub fn previous_fire(&self, at: DateTime<FixedOffset>) -> Option<DateTime<FixedOffset>> {
        self.cron.find_previous_occurrence(&at, true).ok()
    }
}

impl PartialEq for Schedule {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for Schedule {}

impl fmt::Debug for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Schedule").field(&self.text).finish()
    }
}

impl fmt::Display for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Datelike, NaiveDate, TimeZone, Timelike, Weekday};

    use super::*;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    /// #12's evidence tables start every reading at 2026-01-01 00:00.
    fn jan1() -> DateTime<FixedOffset> {
        utc().with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        utc().with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    fn fires(expr: &str, n: usize) -> Vec<DateTime<FixedOffset>> {
        Schedule::parse(expr).unwrap().next_fires(jan1(), n)
    }

    fn days(expr: &str, n: usize) -> Vec<u32> {
        fires(expr, n).iter().map(|t| t.day()).collect()
    }

    fn months(expr: &str, n: usize) -> Vec<u32> {
        fires(expr, n).iter().map(|t| t.month()).collect()
    }

    fn weekdays(expr: &str, n: usize) -> Vec<Weekday> {
        fires(expr, n).iter().map(|t| t.weekday()).collect()
    }

    fn rejected(expr: &str) -> bool {
        Schedule::parse(expr).is_err()
    }

    // #12 "Cron readings, first fires from 2026-01-01", croner 4.0.1 column.

    #[test]
    fn evidence_dom_step_counts_from_the_field_minimum() {
        assert_eq!(days("0 9 */3 * *", 4), [1, 4, 7, 10]);
        assert_eq!(days("0 9 3-31/3 * *", 4), [3, 6, 9, 12]);
    }

    #[test]
    fn evidence_month_step_counts_from_the_field_minimum() {
        assert_eq!(months("0 9 1 */3 *", 4), [1, 4, 7, 10]);
        assert_eq!(months("0 9 1 3-12/3 *", 4), [3, 6, 9, 12]);
    }

    #[test]
    fn evidence_day_fields_are_ored() {
        // Every Monday plus every 13th. 2026-01-13 is a Tuesday.
        assert_eq!(days("0 9 13 * 1", 5), [5, 12, 13, 19, 26]);
    }

    #[test]
    fn evidence_plus_asks_for_and() {
        let got = fires("0 9 13 * +1", 3);
        assert_eq!(got[0], at(2026, 4, 13, 9, 0));
        assert_eq!(got[1], at(2026, 7, 13, 9, 0));
        assert!(
            got.iter()
                .all(|t| t.day() == 13 && t.weekday() == Weekday::Mon)
        );
    }

    #[test]
    fn evidence_seven_is_sunday() {
        // 2026-01-02 is a Friday.
        use Weekday::*;
        assert_eq!(weekdays("0 9 * * 5-7", 6), [Fri, Sat, Sun, Fri, Sat, Sun]);
        assert_eq!(weekdays("0 9 * * 5-6", 4), [Fri, Sat, Fri, Sat]);
        assert_eq!(weekdays("0 9 * * 7", 2), [Sun, Sun]);
        assert_eq!(weekdays("0 9 * * 0", 2), [Sun, Sun]);
    }

    #[test]
    fn evidence_single_value_step_is_rejected() {
        let err = Schedule::parse("5/15 * * * *").unwrap_err();
        assert!(err.to_string().contains("not allowed"), "{err}");
    }

    #[test]
    fn evidence_names_and_question_mark() {
        use Weekday::*;
        assert_eq!(weekdays("0 9 * * MON", 3), [Mon, Mon, Mon]);
        assert_eq!(weekdays("0 9 * * mon-wed", 3), [Mon, Tue, Wed]);
        assert_eq!(days("0 9 ? * *", 3), [1, 2, 3]);
        assert_eq!(days("0 9 * * ?", 3), [1, 2, 3]);
        assert_eq!(months("0 9 1 JAN,jul *", 3), [1, 7, 1]);
    }

    #[test]
    fn evidence_wrong_field_counts_are_rejected() {
        assert!(rejected("0 9 * *"));
        assert!(
            rejected("0 0 9 * * *"),
            "6 fields: seconds first or year last"
        );
        assert!(rejected("0 9 * * * 2026"));
        assert!(rejected("0 0 9 * * * 2026"));
        assert!(rejected(""));
        assert!(rejected("   "));
    }

    #[test]
    fn last_weekday_and_nth() {
        assert_eq!(days("0 9 L * *", 3), [31, 28, 31]);
        // 2026-02-15 and 2026-03-15 are Sundays.
        assert_eq!(days("0 9 15W * *", 3), [15, 16, 16]);
        assert_eq!(days("0 9 * * 1#2", 3), [12, 9, 9]);
        assert_eq!(days("0 9 * * 5L", 3), [30, 27, 27]);
    }

    #[test]
    fn every_nickname() {
        let noon = at(2026, 1, 1, 12, 0);
        let next = |expr: &str| Schedule::parse(expr).unwrap().next_fire(noon).unwrap();
        assert_eq!(next("@yearly"), at(2027, 1, 1, 0, 0));
        assert_eq!(next("@annually"), at(2027, 1, 1, 0, 0));
        assert_eq!(next("@monthly"), at(2026, 2, 1, 0, 0));
        assert_eq!(next("@weekly"), at(2026, 1, 4, 0, 0));
        assert_eq!(next("@daily"), at(2026, 1, 2, 0, 0));
        assert_eq!(next("@hourly"), at(2026, 1, 1, 13, 0));
        assert_eq!(next("@DAILY"), at(2026, 1, 2, 0, 0));
        assert!(rejected("@reboot"));
        assert!(rejected("@daily 9"));
    }

    #[test]
    fn next_fires_use_the_offset_wall_clock() {
        let bangkok = FixedOffset::east_opt(7 * 3600).unwrap();
        let s = Schedule::parse("0 9 * * *").unwrap();
        let got = s.next_fires(jan1().with_timezone(&bangkok), 2);
        assert_eq!(
            got[0],
            bangkok.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap()
        );
        assert_eq!(got[0].naive_utc().hour(), 2);
        assert_eq!(
            got[1],
            bangkok.with_ymd_and_hms(2026, 1, 2, 9, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_fire_is_strictly_after() {
        let s = Schedule::parse("0 9 * * *").unwrap();
        assert_eq!(
            s.next_fire(at(2026, 1, 1, 9, 0)),
            Some(at(2026, 1, 2, 9, 0))
        );
        let half = at(2026, 1, 1, 8, 59) + chrono::Duration::seconds(30);
        assert_eq!(s.next_fire(half), Some(at(2026, 1, 1, 9, 0)));
    }

    #[test]
    fn previous_fire_includes_the_instant() {
        let s = Schedule::parse("0 9 * * *").unwrap();
        assert_eq!(
            s.previous_fire(at(2026, 1, 2, 10, 0)),
            Some(at(2026, 1, 2, 9, 0))
        );
        assert_eq!(
            s.previous_fire(at(2026, 1, 2, 9, 0)),
            Some(at(2026, 1, 2, 9, 0))
        );
        assert_eq!(
            s.previous_fire(at(2026, 1, 2, 8, 59)),
            Some(at(2026, 1, 1, 9, 0))
        );
        let monthly = Schedule::parse("0 9 */3 * *").unwrap();
        assert_eq!(
            monthly.previous_fire(at(2026, 1, 3, 0, 0)),
            Some(at(2026, 1, 1, 9, 0))
        );
    }

    #[test]
    fn a_schedule_that_never_fires_has_no_next() {
        let s = Schedule::parse("0 0 30 2 *").unwrap();
        assert_eq!(s.next_fire(jan1()), None);
        assert!(s.next_fires(jan1(), 3).is_empty());
    }

    #[test]
    fn leap_day_fires_every_four_years() {
        let got = fires("0 0 29 2 *", 2);
        assert_eq!(
            got[0].date_naive(),
            NaiveDate::from_ymd_opt(2028, 2, 29).unwrap()
        );
        assert_eq!(
            got[1].date_naive(),
            NaiveDate::from_ymd_opt(2032, 2, 29).unwrap()
        );
    }

    #[test]
    fn text_is_kept_trimmed() {
        let s = Schedule::parse("  */5 * * * *\n").unwrap();
        assert_eq!(s.as_str(), "*/5 * * * *");
        assert_eq!(s.to_string(), "*/5 * * * *");
    }
}
