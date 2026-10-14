//! The heartbeat timer's settings and exclude windows ("Heartbeat works as it does in TS", #9).

use std::time::Duration;

use chrono::{DateTime, Datelike, FixedOffset, Timelike};

/// The shortest heartbeat interval: TS clamps to one minute (`src/commands/start.ts:172`).
pub const MIN_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// The heartbeat's settings, as `core` maps them from `settings.heartbeat` (#9). The prompt is
/// not here: `core` resolves it when [`Dispatch::heartbeat`](crate::Dispatch::heartbeat) runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatConfig {
    /// `heartbeat.enabled`.
    pub enabled: bool,
    /// `heartbeat.interval` (minutes in the file). Clamped to [`MIN_HEARTBEAT_INTERVAL`].
    pub interval: Duration,
    /// `heartbeat.excludeWindows`, already parsed with [`ExcludeWindow::new`].
    pub exclude_windows: Vec<ExcludeWindow>,
}

impl HeartbeatConfig {
    /// A disabled heartbeat.
    pub fn disabled() -> Self {
        HeartbeatConfig {
            enabled: false,
            interval: Duration::from_secs(15 * 60),
            exclude_windows: Vec::new(),
        }
    }

    /// The interval the timer uses: at least [`MIN_HEARTBEAT_INTERVAL`].
    pub fn effective_interval(&self) -> Duration {
        self.interval.max(MIN_HEARTBEAT_INTERVAL)
    }

    /// Whether any exclude window covers `at` (on the Daemon's offset). TS
    /// `isHeartbeatExcludedAt` (`src/commands/start.ts:137-163`).
    pub fn is_excluded(&self, at: DateTime<FixedOffset>) -> bool {
        self.exclude_windows.iter().any(|w| w.contains(at))
    }
}

/// One `heartbeat.excludeWindows` entry: weekdays and an `HH:MM` range on the Daemon's offset,
/// read as TS reads it (`src/commands/start.ts:137-163`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExcludeWindow {
    /// Bit `d` set for weekday `d` (0 = Sunday).
    days: u8,
    /// Minutes after local midnight.
    start: u16,
    end: u16,
}

const ALL_DAYS: u8 = 0b111_1111;

impl ExcludeWindow {
    /// Reads one window. `days` are weekdays 0–6 (0 = Sunday); none, or none valid, means every
    /// day, as in TS. `None` when a bound isn't `HH:MM` (TS `parseClockMinutes`,
    /// `src/commands/start.ts:127-131`, skips such a window).
    pub fn new(days: Option<&[u8]>, start: &str, end: &str) -> Option<Self> {
        let start = clock_minutes(start)?;
        let end = clock_minutes(end)?;
        let mask = days
            .into_iter()
            .flatten()
            .filter(|d| **d <= 6)
            .fold(0u8, |mask, d| mask | (1 << d));
        Some(ExcludeWindow {
            days: if mask == 0 { ALL_DAYS } else { mask },
            start,
            end,
        })
    }

    fn has_day(&self, day: u32) -> bool {
        self.days & (1 << day) != 0
    }

    /// Whether the window covers `at`. A same-day range is `[start, end)` on its days; equal
    /// bounds cover the whole of its days; an overnight range covers `[start, 24:00)` on its days
    /// and `[00:00, end)` on the day after each.
    pub fn contains(&self, at: DateTime<FixedOffset>) -> bool {
        let day = at.weekday().num_days_from_sunday();
        let minute = (at.hour() * 60 + at.minute()) as u16;
        if self.start < self.end {
            return self.has_day(day) && minute >= self.start && minute < self.end;
        }
        if self.start == self.end {
            return self.has_day(day);
        }
        let previous_day = (day + 6) % 7;
        (minute >= self.start && self.has_day(day))
            || (minute < self.end && self.has_day(previous_day))
    }
}

/// TS `parseClockMinutes`: `^([01]\d|2[0-3]):([0-5]\d)$`.
fn clock_minutes(text: &str) -> Option<u16> {
    let b = text.as_bytes();
    let valid = b.len() == 5
        && b[2] == b':'
        && matches!(
            (b[0], b[1]),
            (b'0'..=b'1', b'0'..=b'9') | (b'2', b'0'..=b'3')
        )
        && matches!((b[3], b[4]), (b'0'..=b'5', b'0'..=b'9'));
    if !valid {
        return None;
    }
    let digit = |i: usize| u16::from(b[i] - b'0');
    Some((digit(0) * 10 + digit(1)) * 60 + digit(3) * 10 + digit(4))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    /// 2026-01-04 is a Sunday (day 0).
    fn at(offset_h: i32, day: u32, h: u32, m: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(offset_h * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 1, 4 + day, h, m, 0)
            .unwrap()
    }

    fn window(days: &[u8], start: &str, end: &str) -> ExcludeWindow {
        ExcludeWindow::new(Some(days), start, end).unwrap()
    }

    #[test]
    fn clock_minutes_follow_the_ts_pattern() {
        // TS `parseClockMinutes` (`src/commands/start.ts:127-131`).
        assert_eq!(clock_minutes("00:00"), Some(0));
        assert_eq!(clock_minutes("09:30"), Some(570));
        assert_eq!(clock_minutes("23:59"), Some(1439));
        for bad in ["24:00", "9:30", "09:60", "09-30", " 09:30", "0930", ""] {
            assert_eq!(clock_minutes(bad), None, "{bad}");
        }
        assert!(ExcludeWindow::new(None, "9:00", "10:00").is_none());
    }

    /// Expected values read off TS `isHeartbeatExcludedAt` (`src/commands/start.ts:137-163`).
    #[test]
    fn windows_match_ts() {
        let all: &[u8] = &[];
        // (days, start, end, weekday, hour, minute, excluded)
        type Case<'a> = (&'a [u8], &'a str, &'a str, u32, u32, u32, bool);
        let cases: &[Case<'_>] = &[
            // Same-day range: [start, end).
            (all, "09:00", "17:00", 1, 9, 0, true),
            (all, "09:00", "17:00", 1, 16, 59, true),
            (all, "09:00", "17:00", 1, 17, 0, false),
            (all, "09:00", "17:00", 1, 8, 59, false),
            // Only on its days.
            (&[1], "09:00", "17:00", 1, 12, 0, true),
            (&[1], "09:00", "17:00", 2, 12, 0, false),
            // Equal bounds: the whole day.
            (&[3], "10:00", "10:00", 3, 0, 0, true),
            (&[3], "10:00", "10:00", 3, 23, 59, true),
            (&[3], "10:00", "10:00", 4, 12, 0, false),
            // Overnight: late on its days, early on the next day.
            (&[5], "23:00", "07:00", 5, 23, 30, true),
            (&[5], "23:00", "07:00", 6, 6, 59, true),
            (&[5], "23:00", "07:00", 6, 7, 0, false),
            (&[5], "23:00", "07:00", 5, 6, 0, false),
            (&[5], "23:00", "07:00", 6, 23, 30, false),
            // Saturday night runs into Sunday morning.
            (&[6], "22:00", "02:00", 0, 1, 0, true),
        ];
        for &(days, start, end, day, h, m, want) in cases {
            let w = window(days, start, end);
            assert_eq!(
                w.contains(at(0, day, h, m)),
                want,
                "{days:?} {start}-{end} on day {day} {h:02}:{m:02}"
            );
        }
    }

    #[test]
    fn empty_or_invalid_days_mean_every_day() {
        for days in [None, Some(&[][..]), Some(&[7, 9][..])] {
            let w = ExcludeWindow::new(days, "01:00", "02:00").unwrap();
            assert!((0..7).all(|d| w.contains(at(0, d, 1, 30))), "{days:?}");
        }
    }

    #[test]
    fn windows_read_the_local_time_at_the_offset() {
        let w = window(&[], "09:00", "10:00");
        let cfg = HeartbeatConfig {
            enabled: true,
            interval: Duration::from_secs(60),
            exclude_windows: vec![w],
        };
        // 02:30 UTC is 09:30 at +07:00.
        let utc = at(0, 1, 2, 30);
        assert!(!cfg.is_excluded(utc));
        assert!(cfg.is_excluded(utc.with_timezone(&FixedOffset::east_opt(7 * 3600).unwrap())));
    }

    #[test]
    fn interval_is_at_least_a_minute() {
        let mut cfg = HeartbeatConfig::disabled();
        cfg.interval = Duration::from_secs(5);
        assert_eq!(cfg.effective_interval(), MIN_HEARTBEAT_INTERVAL);
        cfg.interval = Duration::from_secs(900);
        assert_eq!(cfg.effective_interval(), Duration::from_secs(900));
    }
}
