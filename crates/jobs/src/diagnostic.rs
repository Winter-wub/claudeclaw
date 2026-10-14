//! Findings on a job file (#12 §4, #28).

use std::fmt;

/// How much a [`JobDiagnostic`] matters (#12 §4). Mirrors `claudeclaw_api::jobs::DiagnosticSeverity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Rejected loudly: the job is invalid and not scheduled; the other jobs load normally.
    Error,
    /// The job runs; the warning is shown in the same places as an error.
    Warning,
}

/// One finding on a job file (#12 §4), shaped like `claudeclaw_api::jobs::JobDiagnostic` so
/// `core` converts it field by field. `jobs` can't depend on `api` (architecture §3.1).
///
/// `kind` is stable (see [`kind`]), so the Bug-report export can drop `message`, which can
/// quote a line of the job file, and keep the meaning (#28).
#[derive(Clone, PartialEq, Eq)]
pub struct JobDiagnostic {
    pub severity: Severity,
    /// Dotted `snake_case` segments, at most 64 bytes: the grammar of
    /// `claudeclaw_api::jobs::JobDiagnosticKind`.
    pub kind: &'static str,
    /// 1-based line in the whole file; `None` for a finding on the whole file.
    pub line: Option<u32>,
    /// 1-based column in characters; `None` for a finding on the whole file or line.
    pub column: Option<u32>,
    /// For people. Can quote the file.
    pub message: String,
}

impl JobDiagnostic {
    /// An [`Severity::Error`] finding.
    pub fn error(kind: &'static str, message: impl Into<String>) -> Self {
        JobDiagnostic {
            severity: Severity::Error,
            kind,
            line: None,
            column: None,
            message: message.into(),
        }
    }

    /// A [`Severity::Warning`] finding.
    pub fn warning(kind: &'static str, message: impl Into<String>) -> Self {
        JobDiagnostic {
            severity: Severity::Warning,
            ..JobDiagnostic::error(kind, message)
        }
    }

    /// Places the finding at a line and, optionally, a column.
    pub fn at(mut self, line: u32, column: Option<u32>) -> Self {
        self.line = Some(line);
        self.column = column;
        self
    }
}

/// Content-free: the message can quote the job file (#28).
impl fmt::Debug for JobDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobDiagnostic")
            .field("severity", &self.severity)
            .field("kind", &self.kind)
            .field("line", &self.line)
            .field("column", &self.column)
            .field("message_bytes", &self.message.len())
            .finish()
    }
}

/// The stable [`JobDiagnostic::kind`]s this crate emits (#28). A [`Lint`](crate::Lint) adds its
/// own under its own prefix.
pub mod kind {
    /// The bytes aren't UTF-8.
    pub const ENCODING_INVALID: &str = "encoding.invalid";
    /// The file doesn't start with a `---` line.
    pub const ENVELOPE_MISSING: &str = "envelope.missing";
    /// No `---` line closes the frontmatter.
    pub const ENVELOPE_UNCLOSED: &str = "envelope.unclosed";
    /// serde-saphyr rejected the frontmatter: syntax, a repeated key, an unknown alias, a
    /// nested value, or a document that isn't a mapping (#12 §2).
    pub const YAML_INVALID: &str = "yaml.invalid";
    /// An unknown key within edit distance 2 of a known one, such as `recuring` (warning).
    pub const KEY_UNKNOWN: &str = "key.unknown";
    /// croner rejected `schedule` (#12 §1).
    pub const SCHEDULE_INVALID: &str = "schedule.invalid";
    /// `schedule` parses but has no fire time, such as `0 0 30 2 *`.
    pub const SCHEDULE_NEVER_FIRES: &str = "schedule.never_fires";
    /// `schedule` without `recurring` (#12 §3).
    pub const RECURRING_MISSING: &str = "recurring.missing";
    /// `recurring` isn't `true false yes no 1 0`.
    pub const RECURRING_INVALID: &str = "recurring.invalid";
    /// `enabled` isn't `true false yes no 1 0`.
    pub const ENABLED_INVALID: &str = "enabled.invalid";
    /// `notify` isn't `true false yes no error`.
    pub const NOTIFY_INVALID: &str = "notify.invalid";
    /// `timeout` isn't a non-negative integer of seconds.
    pub const TIMEOUT_INVALID: &str = "timeout.invalid";
    /// `retry` isn't a non-negative integer.
    pub const RETRY_INVALID: &str = "retry.invalid";
    /// `retry_delay` isn't a non-negative integer of seconds.
    pub const RETRY_DELAY_INVALID: &str = "retry_delay.invalid";

    /// Every kind above, for tests and for `core`'s conversion check.
    pub const ALL: &[&str] = &[
        ENCODING_INVALID,
        ENVELOPE_MISSING,
        ENVELOPE_UNCLOSED,
        YAML_INVALID,
        KEY_UNKNOWN,
        SCHEDULE_INVALID,
        SCHEDULE_NEVER_FIRES,
        RECURRING_MISSING,
        RECURRING_INVALID,
        ENABLED_INVALID,
        NOTIFY_INVALID,
        TIMEOUT_INVALID,
        RETRY_INVALID,
        RETRY_DELAY_INVALID,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `claudeclaw_api::jobs::JobDiagnosticKind`'s grammar (crates/api/src/jobs.rs).
    fn api_grammar(s: &str) -> bool {
        let segment_ok = |seg: &str| {
            seg.starts_with(|c: char| c.is_ascii_lowercase())
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        s.len() <= 64 && s.split('.').all(segment_ok)
    }

    #[test]
    fn every_kind_fits_the_api_grammar() {
        for k in kind::ALL {
            assert!(api_grammar(k), "{k}");
        }
    }

    #[test]
    fn debug_hides_the_message() {
        let d = JobDiagnostic::error(kind::YAML_INVALID, "secret line").at(3, Some(1));
        let shown = format!("{d:?}");
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("yaml.invalid"));
        assert_eq!(
            JobDiagnostic::warning(kind::KEY_UNKNOWN, "x").severity,
            Severity::Warning
        );
    }
}
