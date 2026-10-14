//! The typed frontmatter keys and their strict value grammar (#12 §2).

use crate::cron::Schedule;

/// Every key the Daemon parses (#12 §2). Anything else is an unknown key: kept, and a warning
/// only when it is a near miss of one of these.
pub const KNOWN_KEYS: &[&str] = &[
    "schedule",
    "recurring",
    "enabled",
    "notify",
    "model",
    "timeout",
    "retry",
    "retry_delay",
    "launcher",
    "label",
    "agent",
];

/// The seconds between retry attempts when `retry_delay` is absent: TS's default
/// (`src/jobs.ts:25`, "Defaults to 300 (5 min)").
pub const DEFAULT_RETRY_DELAY_SECS: u64 = 300;

/// A job's `notify:` value (#12 §2). Mirrors `claudeclaw_api::jobs::JobNotify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Notify {
    /// `true`/`yes`, and the default.
    Always,
    /// `false`/`no`.
    Never,
    /// `error`: only when the run fails.
    OnError,
}

/// The known frontmatter keys, typed (#12 §2, management-api §2.4 `JobFields`). `None` means
/// the key is absent, `null` or empty, or its value broke the grammar (which is then a
/// [`JobDiagnostic`](crate::JobDiagnostic)). Unknown keys stay in
/// [`JobFile::keys`](crate::JobFile::keys).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobFields {
    /// Absent: an unscheduled job, listed and runnable by hand (#12 §2).
    pub schedule: Option<Schedule>,
    /// Required whenever `schedule` is set (#12 §3).
    pub recurring: Option<bool>,
    pub enabled: Option<bool>,
    pub notify: Option<Notify>,
    pub model: Option<String>,
    /// Seconds.
    pub timeout: Option<u64>,
    /// Retry attempts after a failure before waiting for the next slot.
    pub retry: Option<u32>,
    /// Seconds between retry attempts.
    pub retry_delay: Option<u64>,
    /// `direct` or a Launcher path: the job's Launcher override (#8). Resolved by `runner`.
    pub launcher: Option<String>,
    /// Read for flat jobs; agent jobs take it from their path, as TS does (#12 §2).
    pub label: Option<String>,
    /// Read for flat jobs; agent jobs take it from their path, as TS does (#12 §2).
    pub agent: Option<String>,
}

impl JobFields {
    /// `enabled`, defaulting to `true` (#12 §2).
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// `notify`, defaulting to [`Notify::Always`] (#12 §2).
    pub fn notify_or_default(&self) -> Notify {
        self.notify.unwrap_or(Notify::Always)
    }

    /// `retry_delay`, defaulting to [`DEFAULT_RETRY_DELAY_SECS`].
    pub fn retry_delay_or_default(&self) -> u64 {
        self.retry_delay.unwrap_or(DEFAULT_RETRY_DELAY_SECS)
    }

    /// A scheduled job that runs once (`recurring: false`, #12 §3).
    pub fn is_one_shot(&self) -> bool {
        self.schedule.is_some() && self.recurring == Some(false)
    }
}

/// `true false yes no 1 0`, in any case (#12 §2: `recurring`, `enabled`).
pub(crate) fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// `true false yes no error`, in any case (#12 §2). Unlike `enabled`, `1` and `0` are not
/// accepted: the table doesn't list them, and TS read `notify: 0` as `true` (`src/jobs.ts:68-72`).
pub(crate) fn parse_notify(v: &str) -> Option<Notify> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" => Some(Notify::Always),
        "false" | "no" => Some(Notify::Never),
        "error" => Some(Notify::OnError),
        _ => None,
    }
}

/// A non-negative integer of ASCII digits (#12 §2): no sign, unit, fraction or exponent.
pub(crate) fn parse_uint<T: std::str::FromStr>(v: &str) -> Option<T> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse().ok()
}

/// Optimal string alignment distance, for near-miss keys (`recuring:`, #12 §2). Counts a
/// transposition as one edit.
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// The known key an unknown key nearly spells, if any (#12 §2: edit distance ≤ 2).
pub(crate) fn near_miss(key: &str) -> Option<&'static str> {
    KNOWN_KEYS
        .iter()
        .map(|k| (edit_distance(key, k), *k))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_grammar() {
        for v in ["true", "TRUE", "Yes", "1"] {
            assert_eq!(parse_bool(v), Some(true), "{v}");
        }
        for v in ["false", "No", "0", "FALSE"] {
            assert_eq!(parse_bool(v), Some(false), "{v}");
        }
        for v in ["maybe", "on", "off", "y", "2", "", "true!"] {
            assert_eq!(parse_bool(v), None, "{v}");
        }
    }

    #[test]
    fn notify_grammar() {
        assert_eq!(parse_notify("Error"), Some(Notify::OnError));
        assert_eq!(parse_notify("yes"), Some(Notify::Always));
        assert_eq!(parse_notify("no"), Some(Notify::Never));
        assert_eq!(parse_notify("1"), None);
        assert_eq!(parse_notify("errors"), None);
    }

    #[test]
    fn uint_grammar() {
        assert_eq!(parse_uint::<u64>("90"), Some(90));
        assert_eq!(parse_uint::<u64>("0"), Some(0));
        for v in ["90s", "-1", "+5", "1.5", "1e3", "", " 9", "0x10"] {
            assert_eq!(parse_uint::<u64>(v), None, "{v}");
        }
        assert_eq!(parse_uint::<u32>("4294967296"), None, "out of range");
    }

    #[test]
    fn near_misses() {
        assert_eq!(near_miss("recuring"), Some("recurring"));
        assert_eq!(near_miss("Schedule"), Some("schedule"));
        assert_eq!(near_miss("eanbled"), Some("enabled"));
        assert_eq!(near_miss("retry-delay"), Some("retry_delay"));
        assert_eq!(near_miss("daily"), None);
        assert_eq!(near_miss("description"), None);
    }

    #[test]
    fn defaults() {
        let f = JobFields::default();
        assert!(f.is_enabled());
        assert_eq!(f.notify_or_default(), Notify::Always);
        assert_eq!(f.retry_delay_or_default(), 300);
        assert!(!f.is_one_shot());
    }
}
