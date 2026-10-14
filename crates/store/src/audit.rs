//! The audit log: `daemon/audit/<YYYY-MM>.jsonl`, one [`AuditEntry`] line per mutating call
//! (#13, management-api §2.9). Append-only; an entry goes in the UTC month of its `at`.
//!
//! An entry holds who, what and when, never a secret, a prompt or a value: [`AuditEntry`] has no
//! field that could carry one. Kept forever by default; [`AuditLog::prune`] is the opt-in
//! `logging.auditKeepMonths` rule (§9.1 Retention).

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Months, Utc};
use claudeclaw_api::logs::AuditEntry;

use crate::error::StoreError;
use crate::jsonl;
use crate::layout::Layout;

/// An audit-log operation failed.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    /// A `before` cursor this log didn't hand out (`400 invalid_cursor`).
    #[error("invalid audit cursor")]
    InvalidCursor,
    /// An entry could not be serialised.
    #[error("audit entry could not be serialised: {0}")]
    Encode(#[source] serde_json::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The audit log of one project dir (#13). Hold one per process, so appends share its mutex.
#[derive(Debug)]
pub struct AuditLog {
    layout: Layout,
    append: Mutex<()>,
}

/// One page of the audit log, newest first (management-api §2.9, `audit.list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditLogPage {
    pub entries: Vec<AuditEntry>,
    /// The `before` cursor for the next page, or `None` at the end.
    pub next: Option<String>,
}

/// What a prune deleted, or would delete on a dry run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub files_deleted: u32,
    pub freed_bytes: u64,
}

impl AuditLog {
    /// The audit log under `layout`'s `daemon/audit/`. No IO.
    pub fn new(layout: Layout) -> Self {
        AuditLog {
            layout,
            append: Mutex::new(()),
        }
    }

    /// Appends one entry to its month file, creating `daemon/` (0700) if needed. A torn last
    /// line is ended first, so the entry starts on its own line.
    pub fn append(&self, entry: &AuditEntry) -> Result<(), AuditError> {
        let mut line = serde_json::to_vec(entry).map_err(AuditError::Encode)?;
        line.push(b'\n');
        let _guard = self.append.lock().unwrap_or_else(|e| e.into_inner());
        if !self.layout.audit_dir().is_dir() {
            self.layout.create_daemon_dirs()?;
        }
        jsonl::append(&self.layout.audit_log(&jsonl::month_of(entry.at)), &line)?;
        Ok(())
    }

    /// One page of entries, newest first (latest line first), before the `before` cursor,
    /// keeping those `filter` accepts. A torn last line and lines that don't parse are skipped.
    /// `limit` is at least 1.
    pub fn page(
        &self,
        before: Option<&str>,
        limit: usize,
        filter: impl Fn(&AuditEntry) -> bool,
    ) -> Result<AuditLogPage, AuditError> {
        let limit = limit.max(1);
        let before = before.map(parse_cursor).transpose()?;
        // Each entry with its position, for the cursor.
        let mut entries: Vec<(AuditEntry, String, usize)> = Vec::new();
        'months: for (month, path) in jsonl::month_files(&self.layout.audit_dir())? {
            let end = match &before {
                Some((m, _)) if month > *m => continue,
                Some((m, line)) if month == *m => *line,
                _ => usize::MAX,
            };
            let read = jsonl::read_lines(&path)?;
            for (n, line) in read.lines.iter().enumerate().take(end).rev() {
                let Ok(entry) = serde_json::from_str::<AuditEntry>(line) else {
                    continue;
                };
                if filter(&entry) {
                    entries.push((entry, month.clone(), n));
                    if entries.len() > limit {
                        break 'months;
                    }
                }
            }
        }
        let next = if entries.len() > limit {
            entries.truncate(limit);
            entries.last().map(|(_, month, n)| format!("{month}:{n}"))
        } else {
            None
        };
        Ok(AuditLogPage {
            entries: entries.into_iter().map(|(e, _, _)| e).collect(),
            next,
        })
    }

    /// The opt-in `logging.auditKeepMonths` rule (#23 D7): keeps the `keep_months` newest
    /// calendar months up to and including `now`'s, and deletes whole older month files. With
    /// `dry_run`, nothing is deleted and the report says what would be.
    pub fn prune(
        &self,
        keep_months: u32,
        now: DateTime<Utc>,
        dry_run: bool,
    ) -> Result<PruneReport, AuditError> {
        let mut report = PruneReport::default();
        let Some(oldest_kept) = jsonl::month_start(&jsonl::month_of(now))
            .and_then(|start| start.checked_sub_months(Months::new(keep_months.saturating_sub(1))))
        else {
            return Ok(report);
        };
        let oldest_kept = jsonl::month_of(oldest_kept);
        let _guard = self.append.lock().unwrap_or_else(|e| e.into_inner());
        for (month, path) in jsonl::month_files(&self.layout.audit_dir())? {
            if month >= oldest_kept {
                continue;
            }
            report.files_deleted += 1;
            report.freed_bytes += size(&path)?;
            if !dry_run {
                std::fs::remove_file(&path).map_err(StoreError::io("delete", &path))?;
            }
        }
        Ok(report)
    }
}

fn size(path: &Path) -> Result<u64, StoreError> {
    Ok(std::fs::metadata(path)
        .map_err(StoreError::io("stat", path))?
        .len())
}

/// `<YYYY-MM>:<line>`: the month file and line index of the last entry a page returned. Lines
/// are only ever appended, so the position stays valid.
fn parse_cursor(cursor: &str) -> Result<(String, usize), AuditError> {
    let (month, line) = cursor.split_once(':').ok_or(AuditError::InvalidCursor)?;
    if jsonl::month_start(month).is_none() {
        return Err(AuditError::InvalidCursor);
    }
    let line = line.parse().map_err(|_| AuditError::InvalidCursor)?;
    Ok((month.to_owned(), line))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use claudeclaw_api::logs::AuditOutcome;

    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn setup() -> (tempfile::TempDir, Layout, AuditLog) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        fs::create_dir_all(layout.root()).unwrap();
        let log = AuditLog::new(layout.clone());
        (dir, layout, log)
    }

    fn entry(id: &str, when: &str, action: &str) -> AuditEntry {
        AuditEntry {
            id: id.into(),
            at: at(when),
            principal: "device:1234".into(),
            action: action.into(),
            target: Some("daily-report".into()),
            outcome: AuditOutcome::Ok,
            code: None,
            count: None,
        }
    }

    fn ids(page: &AuditLogPage) -> Vec<&str> {
        page.entries.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn entries_split_by_month_and_page_newest_first() {
        let (_dir, layout, log) = setup();
        for (id, when) in [
            ("a", "2026-09-30T23:59:59Z"),
            ("b", "2026-10-01T00:00:00Z"),
            ("c", "2026-10-02T00:00:00Z"),
            ("d", "2026-10-03T00:00:00Z"),
        ] {
            log.append(&entry(id, when, "jobs.update")).unwrap();
        }
        assert_eq!(
            fs::read_to_string(layout.audit_log("2026-09"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(
            fs::read_to_string(layout.audit_log("2026-10"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(layout.daemon_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700);
        }

        let page = log.page(None, 2, |_| true).unwrap();
        assert_eq!(ids(&page), ["d", "c"]);
        assert_eq!(page.next.as_deref(), Some("2026-10:1"));
        // An entry appended meanwhile doesn't shift the cursor.
        log.append(&entry("e", "2026-10-04T00:00:00Z", "jobs.update"))
            .unwrap();
        let page = log.page(page.next.as_deref(), 2, |_| true).unwrap();
        assert_eq!(ids(&page), ["b", "a"]);
        assert_eq!(page.next, None);

        let page = log.page(None, 10, |e| e.id != "c").unwrap();
        assert_eq!(ids(&page), ["e", "d", "b", "a"]);

        for bad in ["x", "2026-13:1", "2026-10:x"] {
            assert!(matches!(
                log.page(Some(bad), 1, |_| true),
                Err(AuditError::InvalidCursor)
            ));
        }
    }

    #[test]
    fn a_torn_line_is_skipped_and_ended_by_the_next_append() {
        let (_dir, layout, log) = setup();
        log.append(&entry("a", "2026-10-01T00:00:00Z", "jobs.update"))
            .unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(layout.audit_log("2026-10"))
            .unwrap()
            .write_all(b"{\"id\":\"to")
            .unwrap();
        assert_eq!(ids(&log.page(None, 10, |_| true).unwrap()), ["a"]);
        log.append(&entry("b", "2026-10-02T00:00:00Z", "jobs.update"))
            .unwrap();
        assert_eq!(ids(&log.page(None, 10, |_| true).unwrap()), ["b", "a"]);
    }

    #[test]
    fn prune_keeps_the_newest_months() {
        let (_dir, layout, log) = setup();
        for (id, when) in [
            ("a", "2026-06-15T00:00:00Z"),
            ("b", "2026-07-15T00:00:00Z"),
            ("c", "2026-08-15T00:00:00Z"),
            ("d", "2026-10-15T00:00:00Z"),
        ] {
            log.append(&entry(id, when, "jobs.update")).unwrap();
        }
        let now = at("2026-10-20T00:00:00Z");
        // 3 months up to October: August, September, October.
        let dry = log.prune(3, now, true).unwrap();
        assert_eq!(dry.files_deleted, 2);
        assert!(layout.audit_log("2026-06").exists());
        let report = log.prune(3, now, false).unwrap();
        assert_eq!(report, dry);
        assert!(report.freed_bytes > 0);
        assert!(!layout.audit_log("2026-06").exists());
        assert!(!layout.audit_log("2026-07").exists());
        assert_eq!(ids(&log.page(None, 10, |_| true).unwrap()), ["d", "c"]);
        assert_eq!(log.prune(3, now, false).unwrap(), PruneReport::default());
    }
}
