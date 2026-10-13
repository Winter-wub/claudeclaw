//! Daemon-log lines, the log summary and the audit log (management-api §2.9; #23 D5, #44).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

use crate::error::ErrorCode;
use crate::wire::wire_enum;

/// One Daemon-log line from `logs.tail`, `logs.stream` or `claudeclaw logs --offline`
/// (#23 D5). A line that doesn't parse (a torn last line, foreign text) comes back as `Raw` and
/// is never skipped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum LogEntry {
    Line(Box<LogLine>),
    Raw(RawLogLine),
}

/// A parsed Daemon-log line (#23 D2, D5). Content-free by #23 D6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LogLine {
    /// Opaque; pass it as `before`. Absent on streamed lines and in the Bug-report export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Per-process, restarting at 1; a gap means lost lines.
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub target: String,
    /// The stable dotted event name; required at INFO and above from workspace crates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    pub message: String,
    /// The current span's correlation fields (`run_id`, `scope`, `launcher`, …).
    #[serde(default)]
    #[schema(required = true)]
    pub span: Map<String, Value>,
    /// The event's own fields.
    #[serde(default)]
    #[schema(required = true)]
    pub fields: Map<String, Value>,
}

/// A Daemon-log line that didn't parse (#23 D5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RawLogLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub raw: String,
}

wire_enum! {
    /// A tracing level as written in the Daemon log (#23 D1).
    pub enum LogLevel {
        Error = "ERROR",
        Warn = "WARN",
        Info = "INFO",
        Debug = "DEBUG",
        Trace = "TRACE",
    }
}

/// `logs.tail`: one page, newest first (management-api §2.9, #23 D5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LogPage {
    pub lines: Vec<LogEntry>,
    /// The `before` cursor for the next page, or `null` at the end.
    #[schema(required = true)]
    pub next: Option<String>,
    /// The 50 MB scan bound stopped this page before `limit` lines; `next` continues it.
    pub scan_truncated: bool,
}

/// `logs.summary`: badges for the Logs and Status screens (management-api §2.9; #23 D5, #44).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogSummary {
    /// The `EnvFilter` directive in effect, e.g. `info,claudeclaw_discord=debug`.
    pub level: String,
    pub since_boot: LogCounts,
    /// Lines the Worker's sink dropped since boot (`log.dropped`).
    pub dropped_lines: u32,
    /// The daily cap was hit today (`log.capped`).
    pub capped_today: bool,
    pub disk: LogDisk,
}

/// ERROR and WARN lines since the Worker started (#23 D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogCounts {
    pub error: u32,
    pub warn: u32,
}

/// What the Daemon keeps on disk (#23 D5, #44 D7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogDisk {
    pub daemon_bytes: u64,
    pub run_log_files: u32,
    pub run_log_bytes: u64,
    pub audit_bytes: u64,
    /// Daemon-pattern files under `inbox/*`.
    pub inbox_files: u32,
    pub inbox_bytes: u64,
    /// Files under `inbox/*` the Daemon didn't write, and so never deletes.
    pub inbox_foreign: u32,
    pub run_index_bytes: u64,
    pub outbox_bytes: u64,
    pub archive_bytes: u64,
    pub import_backup_bytes: u64,
    /// Free space on the project dir's volume; `null` when it couldn't be read.
    #[schema(required = true)]
    pub free_bytes: Option<u64>,
    pub housekeeping: DiskHousekeeping,
}

/// The last and next Housekeeping pass (#44 D7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiskHousekeeping {
    /// `null` until the first pass.
    #[schema(required = true)]
    pub last_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub next_at: Option<DateTime<Utc>>,
    /// Files the last pass deleted, by kind (`inbox`, `run_logs`, `run_index`, …).
    pub deleted: BTreeMap<String, u32>,
    pub freed_bytes: u64,
}

/// `audit.list`: one page, newest first (management-api §2.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    #[schema(required = true)]
    pub next: Option<String>,
}

/// One audit-log line: who changed what, when; never a secret, a prompt or a value (#13, #44).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditEntry {
    pub id: String,
    pub at: DateTime<Utc>,
    /// `local`, `local-offline`, `device:<id>`, `token:<id>`, `system:import` or
    /// `system:housekeeping`.
    pub principal: String,
    /// The route's Op, e.g. `jobs.update`.
    pub action: String,
    /// An identifier (job name, Scope key, Device ID, a Housekeeping kind); `null` when the
    /// action has none.
    #[schema(required = true)]
    pub target: Option<String>,
    pub outcome: AuditOutcome,
    /// Why it failed, when `outcome` is `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
    /// How many things one aggregate entry covers, e.g. files one Housekeeping kind deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

wire_enum! {
    /// Whether an audited call succeeded.
    pub enum AuditOutcome {
        Ok = "ok",
        Error = "error",
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    LogEntry,
    LogLine,
    RawLogLine,
    LogLevel,
    LogPage,
    LogSummary,
    LogCounts,
    LogDisk,
    DiskHousekeeping,
    AuditPage,
    AuditEntry,
    AuditOutcome,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn a_log_page_matches_the_spec_shape() {
        let json = json!({
            "lines": [
                {
                    "cursor": "2026-10-13:4096",
                    "seq": 1,
                    "timestamp": "2026-10-13T08:30:27.512585Z",
                    "level": "INFO",
                    "target": "claudeclaw_core::conversations",
                    "event": "run.start",
                    "message": "run started",
                    "span": {"run_id": "0199e0c2", "scope": "job:example", "launcher": "direct"},
                    "fields": {"attempt": 1},
                },
                {"cursor": "2026-10-13:4300", "raw": "{\"seq\":2,\"timest"},
            ],
            "next": "2026-10-13:0",
            "scan_truncated": true,
        });
        let page: LogPage = serde_json::from_value(json.clone()).unwrap();
        let LogEntry::Line(line) = &page.lines[0] else {
            panic!("expected a parsed line")
        };
        assert_eq!(line.level, LogLevel::Info);
        assert_eq!(line.timestamp, at("2026-10-13T08:30:27.512585Z"));
        assert_eq!(line.fields["attempt"], 1);
        assert_eq!(
            page.lines[1],
            LogEntry::Raw(RawLogLine {
                cursor: Some("2026-10-13:4300".into()),
                raw: "{\"seq\":2,\"timest".into(),
            })
        );
        assert_eq!(serde_json::to_value(&page).unwrap(), json);
    }

    #[test]
    fn a_streamed_line_has_no_cursor_and_may_have_no_event() {
        let json = json!({
            "seq": 7,
            "timestamp": "2026-10-13T08:30:27Z",
            "level": "DEBUG",
            "target": "hyper",
            "message": "connected",
        });
        let entry: LogEntry = serde_json::from_value(json.clone()).unwrap();
        let LogEntry::Line(line) = &entry else {
            panic!("expected a parsed line")
        };
        assert_eq!((line.cursor.as_ref(), line.event.as_ref()), (None, None));
        let mut expected = json;
        expected["span"] = json!({});
        expected["fields"] = json!({});
        assert_eq!(serde_json::to_value(&entry).unwrap(), expected);
    }

    #[test]
    fn a_later_level_still_parses_as_a_line() {
        let entry: LogEntry = serde_json::from_value(json!({
            "seq": 1, "timestamp": "2026-10-13T08:30:27Z", "level": "NOTICE",
            "target": "t", "message": "m",
        }))
        .unwrap();
        let LogEntry::Line(line) = entry else {
            panic!("expected a parsed line")
        };
        assert_eq!(line.level, LogLevel::Unknown);
    }

    #[test]
    fn a_log_summary_matches_the_spec_shape() {
        let summary = LogSummary {
            level: "info".into(),
            since_boot: LogCounts { error: 1, warn: 4 },
            dropped_lines: 0,
            capped_today: false,
            disk: LogDisk {
                daemon_bytes: 1000,
                run_log_files: 5755,
                run_log_bytes: 9_000_000,
                audit_bytes: 300,
                inbox_files: 9,
                inbox_bytes: 7_000_000,
                inbox_foreign: 1,
                run_index_bytes: 3_000_000,
                outbox_bytes: 0,
                archive_bytes: 0,
                import_backup_bytes: 2048,
                free_bytes: Some(13_000_000_000),
                housekeeping: DiskHousekeeping {
                    last_at: Some(at("2026-10-13T08:40:00Z")),
                    next_at: Some(at("2026-10-14T08:40:00Z")),
                    deleted: BTreeMap::from([("inbox".into(), 2)]),
                    freed_bytes: 1_500_000,
                },
            },
        };
        let json = json!({
            "level": "info",
            "since_boot": {"error": 1, "warn": 4},
            "dropped_lines": 0,
            "capped_today": false,
            "disk": {
                "daemon_bytes": 1000,
                "run_log_files": 5755,
                "run_log_bytes": 9000000,
                "audit_bytes": 300,
                "inbox_files": 9,
                "inbox_bytes": 7000000,
                "inbox_foreign": 1,
                "run_index_bytes": 3000000,
                "outbox_bytes": 0,
                "archive_bytes": 0,
                "import_backup_bytes": 2048,
                "free_bytes": 13000000000u64,
                "housekeeping": {
                    "last_at": "2026-10-13T08:40:00Z",
                    "next_at": "2026-10-14T08:40:00Z",
                    "deleted": {"inbox": 2},
                    "freed_bytes": 1500000,
                },
            },
        });
        assert_eq!(serde_json::to_value(&summary).unwrap(), json);
        assert_eq!(serde_json::from_value::<LogSummary>(json).unwrap(), summary);
    }

    #[test]
    fn an_audit_page_matches_the_spec_shape() {
        let page = AuditPage {
            entries: vec![
                AuditEntry {
                    id: "0199e0c2-0000-7000-8000-00000000000a".into(),
                    at: at("2026-10-13T09:00:00Z"),
                    principal: "device:1234".into(),
                    action: "jobs.update".into(),
                    target: Some("daily-report".into()),
                    outcome: AuditOutcome::Error,
                    code: Some(ErrorCode::RevisionMismatch),
                    count: None,
                },
                AuditEntry {
                    id: "0199e0c2-0000-7000-8000-00000000000b".into(),
                    at: at("2026-10-13T08:40:00Z"),
                    principal: "system:housekeeping".into(),
                    action: "housekeeping.run".into(),
                    target: Some("inbox".into()),
                    outcome: AuditOutcome::Ok,
                    code: None,
                    count: Some(2),
                },
            ],
            next: None,
        };
        let json = json!({
            "entries": [
                {
                    "id": "0199e0c2-0000-7000-8000-00000000000a",
                    "at": "2026-10-13T09:00:00Z",
                    "principal": "device:1234",
                    "action": "jobs.update",
                    "target": "daily-report",
                    "outcome": "error",
                    "code": "revision_mismatch",
                },
                {
                    "id": "0199e0c2-0000-7000-8000-00000000000b",
                    "at": "2026-10-13T08:40:00Z",
                    "principal": "system:housekeeping",
                    "action": "housekeeping.run",
                    "target": "inbox",
                    "outcome": "ok",
                    "count": 2,
                },
            ],
            "next": null,
        });
        assert_eq!(serde_json::to_value(&page).unwrap(), json);
        assert_eq!(serde_json::from_value::<AuditPage>(json).unwrap(), page);
    }
}
