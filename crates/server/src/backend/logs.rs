//! The logs seam (`logs.tail`, `logs.stream`, `logs.summary` and the Bug-report `logs.export`,
//! management-api §2.9; #23 D5, #28).
//!
//! The cursor grammar lives here because the backend hands the cursors out and the route checks
//! the ones a caller sends back: cursors are opaque to clients (§1.6) but owned by the server.

use chrono::{DateTime, NaiveDate, Utc};
use tokio::sync::broadcast;

use claudeclaw_api::events::{LogStreamHello, StreamGap};
use claudeclaw_api::export::LogExport;
use claudeclaw_api::logs::{LogEntry, LogLevel, LogPage, LogSummary};
use claudeclaw_api::scope::Scope;

use super::BackendFuture;

/// `logs.tail` and `logs.stream`'s `source` (management-api §2.9): one process's log, or the two
/// merged by timestamp, which #22 made the CLI's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogSource {
    /// `worker.<date>.jsonl`.
    Worker,
    /// `watchdog.<date>.jsonl`.
    Watchdog,
    /// Worker and Watchdog merged server-side (#23 D13).
    Daemon,
}

impl LogSource {
    /// The wire value in the `{source}` path segment.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "worker" => Some(LogSource::Worker),
            "watchdog" => Some(LogSource::Watchdog),
            "daemon" => Some(LogSource::Daemon),
            _ => None,
        }
    }

    /// The wire value, for a message or a log field.
    pub fn as_wire(self) -> &'static str {
        match self {
            LogSource::Worker => "worker",
            LogSource::Watchdog => "watchdog",
            LogSource::Daemon => "daemon",
        }
    }

    /// Whether this source is the two logs merged (#23 D13), and so needs a two-position cursor.
    pub fn merged(self) -> bool {
        matches!(self, LogSource::Daemon)
    }
}

/// One process's place in its log: the log file's date and the byte offset of the line the next
/// page starts before. The grammar is `<YYYY-MM-DD>:<offset>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogPosition {
    pub date: NaiveDate,
    pub offset: u64,
}

impl LogPosition {
    pub fn parse(raw: &str) -> Option<Self> {
        let (date, offset) = raw.split_once(':')?;
        Some(LogPosition {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?,
            offset: offset.parse().ok()?,
        })
    }

    pub fn encode(&self) -> String {
        format!("{}:{}", self.date.format("%Y-%m-%d"), self.offset)
    }
}

/// A `before` cursor (§1.6): one position for `worker` or `watchdog`, and both processes'
/// positions for the merged `daemon` source, written `worker|watchdog` (#23 D13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCursor {
    Single(LogPosition),
    Merged {
        worker: LogPosition,
        watchdog: LogPosition,
    },
}

impl LogCursor {
    /// The cursor `raw` stands for, when it is exactly the canonical form `source` hands out.
    /// Anything else is `400 invalid_cursor`: a cursor from another source, or one this build
    /// would write differently.
    pub fn parse(source: LogSource, raw: &str) -> Option<Self> {
        let cursor = match (source, raw.split_once('|')) {
            (LogSource::Daemon, Some((worker, watchdog))) => LogCursor::Merged {
                worker: LogPosition::parse(worker)?,
                watchdog: LogPosition::parse(watchdog)?,
            },
            (LogSource::Daemon, None) | (_, Some(_)) => return None,
            (_, None) => LogCursor::Single(LogPosition::parse(raw)?),
        };
        (cursor.encode() == raw).then_some(cursor)
    }

    pub fn encode(&self) -> String {
        match self {
            LogCursor::Single(position) => position.encode(),
            LogCursor::Merged { worker, watchdog } => {
                format!("{}|{}", worker.encode(), watchdog.encode())
            }
        }
    }
}

/// `logs.tail`'s filters (management-api §2.9), already validated by the route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogTailQuery {
    /// The parsed `?before=`; the page continues older than it.
    pub before: Option<LogCursor>,
    /// The resolved `?limit=`: at least 1 (§1.6).
    pub limit: u32,
    pub min_level: Option<LogLevel>,
    /// A prefix of `event`, e.g. `run.`.
    pub event: Option<String>,
    pub scope: Option<Scope>,
    pub run_id: Option<String>,
    /// A substring of the message.
    pub q: Option<String>,
}

/// `logs.stream`'s parameters (management-api §2.9), already validated by the route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogStreamQuery {
    /// Lines to replay from the ring before the live feed (`?backfill=`; 0 is live only).
    pub backfill: u32,
    pub min_level: Option<LogLevel>,
    pub event: Option<String>,
    pub scope: Option<Scope>,
    pub run_id: Option<String>,
    /// The `seq` `Last-Event-ID` resumes after; `None` when the header was absent or unusable.
    pub resume: Option<u64>,
}

/// One line with the `seq` its SSE frame id uses, `<boot_id>:<seq>` (§2.9).
#[derive(Debug, Clone, PartialEq)]
pub struct SequencedLog {
    pub seq: u64,
    pub entry: LogEntry,
}

/// What `logs.stream` serves (§2.9): the `hello` frame, the replay and the live feed.
#[derive(Debug)]
pub struct LogStream {
    pub hello: LogStreamHello,
    /// Lines after the resume point, or the newest [`LogStreamQuery::backfill`] lines; oldest
    /// first.
    pub backfill: Vec<SequencedLog>,
    /// Live lines, each already carrying its `seq`.
    pub receiver: broadcast::Receiver<SequencedLog>,
    /// The resume point is gone from the ring, so the route sends this before the replay rather
    /// than skipping lines silently (§1.6).
    pub gap: Option<StreamGap>,
}

/// `logs.export`'s parameters (§2.9, #28). The window, level and size limits are the export's
/// own defaults; the route parses only what the caller sent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogExportQuery {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub min_level: Option<LogLevel>,
    pub max_bytes: Option<u64>,
    /// The caller asked for the pseudonym reverse map (`?map=true`).
    pub map: bool,
    /// The Desktop app's version, when the app made the export (#40).
    pub app_version: Option<String>,
}

/// The Daemon-log routes (management-api §2.9). The composition root wires this to `store`'s
/// Daemon-log reader, the Worker's live log ring and `store::export`.
pub trait LogsBackend: Send + Sync + 'static {
    /// One page, newest first. The merged `daemon` source's cursor carries both positions
    /// ([`LogCursor::Merged`]); a cursor this source didn't hand out is `400 invalid_cursor`.
    fn tail(&self, source: LogSource, query: LogTailQuery) -> BackendFuture<'_, LogPage>;

    /// The live feed with its replay and its `hello{boot_id, last_seq}` (§2.9).
    fn stream(&self, source: LogSource, query: LogStreamQuery) -> BackendFuture<'_, LogStream>;

    fn summary(&self) -> BackendFuture<'_, LogSummary>;

    /// The Bug-report export (#28): one reviewable Markdown file, already scrubbed. Only one
    /// export runs at a time (`409 export_busy`), and a leak the scrubber can't remove fails
    /// closed (`500 export_leak`).
    fn export(&self, query: LogExportQuery) -> BackendFuture<'_, LogExport>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(date: &str, offset: u64) -> LogPosition {
        LogPosition {
            date: date.parse().unwrap(),
            offset,
        }
    }

    #[test]
    fn a_single_source_cursor_round_trips() {
        for raw in ["2026-10-13:4096", "2026-01-01:0"] {
            let cursor = LogCursor::parse(LogSource::Worker, raw).unwrap();
            assert_eq!(cursor.encode(), raw);
        }
    }

    #[test]
    fn the_merged_source_cursor_carries_both_positions() {
        let raw = "2026-10-13:4096|2026-10-12:512";
        let cursor = LogCursor::parse(LogSource::Daemon, raw).unwrap();
        assert_eq!(
            cursor,
            LogCursor::Merged {
                worker: position("2026-10-13", 4096),
                watchdog: position("2026-10-12", 512),
            }
        );
        assert_eq!(cursor.encode(), raw, "it round-trips");
    }

    #[test]
    fn a_cursor_only_parses_for_the_source_that_handed_it_out() {
        assert_eq!(
            LogCursor::parse(LogSource::Worker, "watchdog:2026-10-13:1"),
            None
        );
        assert_eq!(
            LogCursor::parse(LogSource::Worker, "2026-10-13:4096|2026-10-12:512"),
            None,
            "a merged cursor is not a single source's"
        );
        assert_eq!(LogCursor::parse(LogSource::Daemon, "2026-10-13:4096"), None);
    }

    #[test]
    fn anything_not_the_canonical_form_is_rejected() {
        for raw in [
            "",
            "junk",
            "2026-10-13",
            "2026-10-13:",
            "2026-10-13:x",
            "13-10-2026:1",
            "2026-10-13:1:2",
            "2026-10-13:01",
        ] {
            assert_eq!(
                LogCursor::parse(LogSource::Worker, raw),
                None,
                "{raw:?} is not a cursor"
            );
        }
    }

    #[test]
    fn a_source_names_itself() {
        assert_eq!(LogSource::from_wire("daemon"), Some(LogSource::Daemon));
        assert_eq!(LogSource::Daemon.as_wire(), "daemon");
        assert!(LogSource::Daemon.merged() && !LogSource::Worker.merged());
        assert_eq!(LogSource::from_wire("runs"), None);
    }
}
