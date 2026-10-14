//! TS Run-log file names (#14 rule 5, architecture §8).
//!
//! A Run log is `logs/<name>-<YYYY-MM-DDTHH-MM-SS-mmmZ>.log`, named by the run's start time, as
//! TS's catch-up seed expects. An agent job is named `<agent>/<label>`, so its log lands in
//! `logs/<agent>/` (TS joins the name onto `logs/`, `src/runner.ts:1066`).

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

/// The length of the stamp in a Run-log name: `YYYY-MM-DDTHH-MM-SS-mmmZ`.
const STAMP_LEN: usize = 24;

/// The file name TS gives a run named `label` that started at `start`:
/// `<label>-<ISO with ':' and '.' as '-'>.log` (`src/runner.ts:1065-1066`).
pub fn file_name(label: &str, start: DateTime<Utc>) -> String {
    format!("{label}-{}.log", stamp(start))
}

/// `start` as TS stamps it: `new Date().toISOString().replace(/[:.]/g, "-")`.
pub fn stamp(start: DateTime<Utc>) -> String {
    start.format("%Y-%m-%dT%H-%M-%S-%3fZ").to_string()
}

/// Splits a Run-log file name into the run's label and its start time, or `None` if it doesn't
/// match TS's catch-up pattern
/// `^(.+)-(\d{4}-\d{2}-\d{2})T(\d{2})-(\d{2})-(\d{2})-(\d{3})Z\.log$` (`src/commands/start.ts:899`).
///
/// TS drops a match whose time `Date.parse` rejects; here a stamp chrono rejects (a 13th month,
/// a 30 February) doesn't match either.
pub fn parse(file_name: &str) -> Option<(&str, DateTime<Utc>)> {
    let rest = file_name.strip_suffix(".log")?;
    let split = rest.len().checked_sub(STAMP_LEN + 1)?;
    if split == 0 || !rest.is_char_boundary(split) {
        return None;
    }
    let (label, stamp) = rest.split_at(split);
    let stamp = stamp.strip_prefix('-')?.as_bytes();
    let shape = b"dddd-dd-ddTdd-dd-dd-dddZ";
    let fits = stamp.iter().zip(shape).all(|(c, s)| {
        if *s == b'd' {
            c.is_ascii_digit()
        } else {
            c == s
        }
    });
    if !fits {
        return None;
    }
    let num = |from: usize, to: usize| -> u32 {
        stamp[from..to]
            .iter()
            .fold(0, |n, d| n * 10 + u32::from(d - b'0'))
    };
    let date = NaiveDate::from_ymd_opt(num(0, 4) as i32, num(5, 7), num(8, 10))?;
    let time = NaiveTime::from_hms_milli_opt(num(11, 13), num(14, 16), num(17, 19), num(20, 23))?;
    Some((label, NaiveDateTime::new(date, time).and_utc()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn names_follow_ts() {
        // Expected values from `src/runner.ts:1065-1066`: `toISOString()` always has three
        // fraction digits, and every ':' and '.' becomes '-'.
        let table = [
            (
                "daily-report",
                "2026-10-13T08:30:27.512Z",
                "daily-report-2026-10-13T08-30-27-512Z.log",
            ),
            (
                "heartbeat",
                "2026-01-02T03:04:05Z",
                "heartbeat-2026-01-02T03-04-05-000Z.log",
            ),
            (
                "ops/digest",
                "2026-12-31T23:59:59.999Z",
                "ops/digest-2026-12-31T23-59-59-999Z.log",
            ),
        ];
        for (label, start, want) in table {
            assert_eq!(file_name(label, at(start)), want);
        }
        // Sub-millisecond time is cut, not rounded, like a JS Date.
        assert_eq!(
            stamp(at("2026-10-13T08:30:27.512999Z")),
            "2026-10-13T08-30-27-512Z"
        );
    }

    #[test]
    fn parsing_follows_the_ts_catch_up_pattern() {
        // Expected values from the regex in `src/commands/start.ts:899`; `(.+)` is greedy, so a
        // label may itself contain a stamp-like suffix.
        let table: &[(&str, Option<(&str, &str)>)] = &[
            (
                "daily-report-2026-10-13T08-30-27-512Z.log",
                Some(("daily-report", "2026-10-13T08:30:27.512Z")),
            ),
            (
                "a-2026-01-01T00-00-00-000Z-2026-02-01T00-00-00-001Z.log",
                Some(("a-2026-01-01T00-00-00-000Z", "2026-02-01T00:00:00.001Z")),
            ),
            (
                "x-2026-10-13T08-30-27-512Z.log",
                Some(("x", "2026-10-13T08:30:27.512Z")),
            ),
            ("daemon.log", None),
            ("daemon.prev.log", None),
            ("-2026-10-13T08-30-27-512Z.log", None),
            ("x-2026-10-13T08-30-27-512Z.txt", None),
            ("x-2026-10-13T08-30-27-51Z.log", None),
            ("x-2026-10-13T08:30:27.512Z.log", None),
            ("x_2026-10-13T08-30-27-512Z.log", None),
            ("x-2026-13-01T00-00-00-000Z.log", None),
            ("x-2026-02-30T00-00-00-000Z.log", None),
            (
                "é-2026-10-13T08-30-27-512Z.log",
                Some(("é", "2026-10-13T08:30:27.512Z")),
            ),
        ];
        for (name, want) in table {
            let got = parse(name);
            assert_eq!(got, want.map(|(l, t)| (l, at(t))), "{name}");
        }
    }

    #[test]
    fn a_name_round_trips() {
        let start = at("2026-10-13T08:30:27.512Z");
        let name = file_name("daily-report", start);
        assert_eq!(parse(&name), Some(("daily-report", start)));
    }
}
