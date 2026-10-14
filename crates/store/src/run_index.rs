//! The Run index: `daemon/runs/<YYYY-MM>.jsonl`, one [`RunRecord`] line per spawned run (#23 D4,
//! architecture §5), with its Back-fill and month compaction (#44 D2–D4).
//!
//! - A record goes in the UTC month of its `started_at`. Month files are not time-ordered
//!   (Back-fill and rollback-era records arrive late), so readers sort each month by
//!   `(started_at, run_id)` (#44 D3).
//! - A torn last line is skipped and reported as `runindex.torn`; before appending to a file that
//!   doesn't end in `\n`, the writer adds one first. Lines that don't parse are skipped and
//!   counted in `runindex.unparsed`. Each is reported once per file per [`RunIndex`].
//! - Appends, the Back-fill and compaction share one append mutex.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use claudeclaw_api::log_event::daemon as event;
use claudeclaw_api::runs::{CostStatus, RunOrigin, RunRecord};
use uuid::Uuid;

use crate::atomic::{self, FileMode};
use crate::error::StoreError;
use crate::jsonl;
use crate::layout::Layout;
use crate::run_log::{self, HEADER_CAP, RunLogFile, RunLogHeader, RunLogListing};

/// The UUID v5 namespace of back-filled TS records' `run_id`s (#23 D4).
const TS_RUN_NAMESPACE: Uuid = Uuid::from_u128(0xe33643d2_e0ba_4c85_ba72_95573aa1c8c2);

/// A Run-index operation failed.
#[derive(Debug, thiserror::Error)]
pub enum RunIndexError {
    /// A `before` cursor this index didn't hand out (`400 invalid_cursor`).
    #[error("invalid Run-index cursor")]
    InvalidCursor,
    /// A record could not be serialised.
    #[error("Run-index record could not be serialised: {0}")]
    Encode(#[source] serde_json::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The Run index of one project dir (#23 D4). Cheap to share: hold one per Worker, so appends,
/// the Back-fill and compaction take the same mutex.
#[derive(Debug)]
pub struct RunIndex {
    layout: Layout,
    append: Mutex<()>,
    /// `(file, event)` pairs already reported, so a torn or unparsed line warns once.
    reported: Mutex<HashSet<(PathBuf, &'static str)>>,
}

/// One page of the index, newest first (management-api §2.9, `runs.list`).
#[derive(Debug, Clone, PartialEq)]
pub struct RunIndexPage {
    pub runs: Vec<RunRecord>,
    /// The `before` cursor for the next page, or `None` at the end.
    pub next: Option<String>,
}

/// What a Back-fill did (#44 D2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Records added.
    pub added: u32,
    /// Of those, logs with no `Exit code:` line (`outcome: unknown`).
    pub unknown: u32,
}

/// What a compaction did, or would do on a dry run (#44 D4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactReport {
    pub files_rewritten: u32,
    pub files_deleted: u32,
    pub records_dropped: u32,
    pub freed_bytes: u64,
}

impl RunIndex {
    /// The index under `layout`'s `daemon/runs/`. No IO.
    pub fn new(layout: Layout) -> Self {
        RunIndex {
            layout,
            append: Mutex::new(()),
            reported: Mutex::new(HashSet::new()),
        }
    }

    /// Appends one record to its month file (#23 D4), creating `daemon/` (0700) if needed.
    pub fn append(&self, record: &RunRecord) -> Result<(), RunIndexError> {
        let mut line = serde_json::to_vec(record).map_err(RunIndexError::Encode)?;
        line.push(b'\n');
        let _guard = self.lock();
        self.append_lines(&jsonl::month_of(record.started_at), &line)
    }

    fn append_lines(&self, month: &str, lines: &[u8]) -> Result<(), RunIndexError> {
        if !self.layout.run_index_dir().is_dir() {
            self.layout.create_daemon_dirs()?;
        }
        jsonl::append(&self.layout.run_index(month), lines)?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.append.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One page of records, newest first by `(started_at, run_id)`, older than the `before`
    /// cursor, keeping those `filter` accepts (management-api §2.9). `limit` is at least 1.
    pub fn page(
        &self,
        before: Option<&str>,
        limit: usize,
        filter: impl Fn(&RunRecord) -> bool,
    ) -> Result<RunIndexPage, RunIndexError> {
        let limit = limit.max(1);
        let before = before.map(parse_cursor).transpose()?;
        let mut runs = Vec::new();
        for (month, path) in jsonl::month_files(&self.layout.run_index_dir())? {
            if let Some((at, _)) = &before
                && month > jsonl::month_of(*at)
            {
                continue;
            }
            let mut records = self.read_month(&path)?;
            records.sort_by(|a, b| key(b).cmp(&key(a)));
            let older = records.into_iter().filter(|r| {
                before
                    .as_ref()
                    .is_none_or(|(at, id)| key(r) < (*at, id.as_str()))
            });
            runs.extend(older.filter(|r| filter(r)).take(limit + 1 - runs.len()));
            if runs.len() > limit {
                break;
            }
        }
        let next = if runs.len() > limit {
            runs.truncate(limit);
            runs.last().map(cursor)
        } else {
            None
        };
        Ok(RunIndexPage { runs, next })
    }

    /// The records of one month file, skipping (and reporting once) a torn last line and lines
    /// that don't parse.
    fn read_month(&self, path: &Path) -> Result<Vec<RunRecord>, RunIndexError> {
        let read = jsonl::read_lines(path)?;
        if read.torn {
            self.report_once(path, event::RUNINDEX_TORN, || {
                tracing::warn!(event = event::RUNINDEX_TORN, file = %file_name(path), "Run index has a torn last line");
            });
        }
        let mut unparsed = 0u32;
        let mut records = Vec::with_capacity(read.lines.len());
        for line in read.lines.iter().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<RunRecord>(line) {
                Ok(record) => records.push(record),
                Err(_) => unparsed += 1,
            }
        }
        if unparsed > 0 {
            self.report_once(path, event::RUNINDEX_UNPARSED, || {
                tracing::warn!(
                    event = event::RUNINDEX_UNPARSED,
                    count = unparsed,
                    "Run index lines did not parse"
                );
            });
        }
        Ok(records)
    }

    fn report_once(&self, path: &Path, event: &'static str, report: impl FnOnce()) {
        let mut reported = self.reported.lock().unwrap_or_else(|e| e.into_inner());
        if reported.insert((path.to_path_buf(), event)) {
            report();
        }
    }

    /// The Back-fill and every-start check (#44 D2, D3): adds an `origin: ts` record for every
    /// TS-pattern Run log in `listing` not yet in its month file, newest month first, one sync
    /// per month. Logs whose name time is at or after `boot` are this Worker's own runs and are
    /// skipped. `progress(done, total)` is called before the first month and after each one.
    ///
    /// Logs `runindex.backfill_start`/`_done` (and `runindex.unparsed` for logs with no
    /// `Exit code:`) when anything was missing, or `runindex.backfill_failed` on error. Read-only
    /// on the Run logs (#14 rule 3).
    pub fn backfill(
        &self,
        listing: &RunLogListing,
        boot: DateTime<Utc>,
        mut progress: impl FnMut(u32, u32),
    ) -> Result<BackfillReport, RunIndexError> {
        let result = self.try_backfill(listing, boot, &mut progress);
        if let Err(e) = &result {
            tracing::warn!(event = event::RUNINDEX_BACKFILL_FAILED, err = %e, "Run index back-fill failed");
        }
        result
    }

    fn try_backfill(
        &self,
        listing: &RunLogListing,
        boot: DateTime<Utc>,
        progress: &mut impl FnMut(u32, u32),
    ) -> Result<BackfillReport, RunIndexError> {
        let started = Instant::now();
        let mut by_month: BTreeMap<String, Vec<&RunLogFile>> = BTreeMap::new();
        for file in listing.files.iter().filter(|f| f.started_at < boot) {
            by_month
                .entry(jsonl::month_of(file.started_at))
                .or_default()
                .push(file);
        }
        // Pass 1: one month's `run_log` set at a time (#44 D3).
        let mut missing: Vec<(String, Vec<&RunLogFile>)> = Vec::new();
        for (month, files) in by_month.into_iter().rev() {
            let indexed = self.indexed_run_logs(&month)?;
            let mut files: Vec<_> = files
                .into_iter()
                .filter(|f| !indexed.contains(&f.rel))
                .collect();
            if !files.is_empty() {
                files.sort_by(|a, b| a.rel.cmp(&b.rel));
                missing.push((month, files));
            }
        }
        let total: u32 = missing.iter().map(|(_, f)| f.len() as u32).sum();
        let mut report = BackfillReport::default();
        if total == 0 {
            progress(0, 0);
            return Ok(report);
        }
        tracing::info!(
            event = event::RUNINDEX_BACKFILL_START,
            total,
            "Run index back-fill started"
        );
        progress(0, total);
        let mut done = 0;
        // Pass 2: newest month first, one append and one sync per month (#44 D2).
        for (month, files) in missing {
            let mut lines = Vec::new();
            for file in &files {
                let header = match RunLogHeader::read(&file.path, Some(HEADER_CAP)) {
                    Ok(header) => header,
                    // Deleted since the listing: nothing to index.
                    Err(e) if e.is_not_found() => continue,
                    Err(e) => return Err(e.into()),
                };
                if header.exit_code.is_none() {
                    report.unknown += 1;
                }
                let record = ts_record(file, header);
                serde_json::to_writer(&mut lines, &record).map_err(RunIndexError::Encode)?;
                lines.push(b'\n');
                report.added += 1;
            }
            if !lines.is_empty() {
                let _guard = self.lock();
                self.append_lines(&month, &lines)?;
            }
            done += files.len() as u32;
            progress(done, total);
        }
        tracing::info!(
            event = event::RUNINDEX_BACKFILL_DONE,
            added = report.added,
            unknown = report.unknown,
            duration_ms = started.elapsed().as_millis() as u64,
            "Run index back-fill done"
        );
        if report.unknown > 0 {
            tracing::warn!(
                event = event::RUNINDEX_UNPARSED,
                count = report.unknown,
                "Run logs with no Exit code line"
            );
        }
        Ok(report)
    }

    /// The `run_log` values already in month `month`, the Back-fill's dedupe key (#44 D3).
    fn indexed_run_logs(&self, month: &str) -> Result<BTreeSet<String>, RunIndexError> {
        let read = jsonl::read_lines(&self.layout.run_index(month))?;
        Ok(read
            .lines
            .iter()
            .filter_map(|line| run_log_of(line).map(str::to_owned))
            .collect())
    }

    /// Month compaction (#44 D4): every month that ended more than `keep_days` days before
    /// `now` is rewritten atomically to keep only the records whose Run log still exists, and
    /// deleted if none are left. Records with no Run log (Refused runs) and lines that don't
    /// parse go with their month. With `dry_run`, nothing is written and the report says what
    /// would be.
    ///
    /// Call it only when `logging.runLogKeepDays` is set; with no value the index is kept
    /// forever (§9.1 Retention).
    pub fn compact(
        &self,
        keep_days: u32,
        now: DateTime<Utc>,
        dry_run: bool,
    ) -> Result<CompactReport, RunIndexError> {
        let cutoff = now - TimeDelta::days(i64::from(keep_days));
        let mut report = CompactReport::default();
        let _guard = self.lock();
        for (month, path) in jsonl::month_files(&self.layout.run_index_dir())? {
            if jsonl::month_end(&month).is_none_or(|end| end >= cutoff) {
                continue;
            }
            let read = jsonl::read_lines(&path)?;
            let total = read.lines.iter().filter(|l| !l.trim().is_empty()).count();
            let kept: Vec<&String> = read
                .lines
                .iter()
                .filter(|line| {
                    run_log_of(line)
                        .and_then(|rel| run_log::resolve_rel(&self.layout, rel))
                        .is_some_and(|p| p.symlink_metadata().is_ok())
                })
                .collect();
            if !read.torn && kept.len() == read.lines.len() {
                continue;
            }
            report.records_dropped += (total - kept.len()) as u32;
            if kept.is_empty() {
                report.files_deleted += 1;
                report.freed_bytes += read.len;
                if !dry_run {
                    std::fs::remove_file(&path).map_err(StoreError::io("delete", &path))?;
                }
                continue;
            }
            let mut bytes = Vec::new();
            for line in kept {
                bytes.extend_from_slice(line.as_bytes());
                bytes.push(b'\n');
            }
            report.files_rewritten += 1;
            report.freed_bytes += read.len.saturating_sub(bytes.len() as u64);
            if !dry_run {
                atomic::write_atomic(&path, &bytes, FileMode::Keep)?;
            }
        }
        Ok(report)
    }
}

/// The `run_log` string of an index line, without parsing the whole record.
fn run_log_of(line: &str) -> Option<&str> {
    #[derive(serde::Deserialize)]
    struct RunLogOnly<'a> {
        #[serde(borrow)]
        run_log: Option<&'a str>,
    }
    serde_json::from_str::<RunLogOnly<'_>>(line).ok()?.run_log
}

/// A back-filled TS record (#23 D4, #44 D2): only the file name and header are known.
fn ts_record(file: &RunLogFile, header: RunLogHeader) -> RunRecord {
    RunRecord {
        run_id: ts_run_id(&file.rel),
        turn_id: None,
        name: file.name.clone(),
        scope: None,
        origin: RunOrigin::Ts,
        launcher: None,
        job: None,
        agent: file.agent.clone(),
        session_id: header.session_id,
        session: header.session,
        model: None,
        started_at: file.started_at,
        ended_at: None,
        duration_ms: None,
        outcome: Some(run_log::exit_outcome(header.exit_code)),
        exit_code: header.exit_code,
        is_error: None,
        api_error_status: None,
        attempt: 1,
        state: None,
        tokens_in: None,
        tokens_out: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
        cost_status: CostStatus::NoUsage,
        cost_basis: None,
        provider: None,
        api_key_source: None,
        models: Vec::new(),
        run_log: Some(file.rel.clone()),
        launcher_resolved: None,
        purpose: None,
        settle: None,
        awake_ms: None,
        suspended_ms: None,
        wake_grace: false,
        claude_code_version: None,
        lingered: false,
    }
}

/// A back-filled record's `run_id`: a UUID v5 of the Run log's relative path, so it is stable
/// (#23 D4).
pub fn ts_run_id(rel: &str) -> String {
    Uuid::new_v5(&TS_RUN_NAMESPACE, rel.as_bytes()).to_string()
}

/// The sort key: `(started_at, run_id)` (#44 D3).
fn key(record: &RunRecord) -> (DateTime<Utc>, &str) {
    (record.started_at, record.run_id.as_str())
}

/// `<started_at RFC 3339>_<run_id>`: the key of the last record a page returned.
fn cursor(record: &RunRecord) -> String {
    format!(
        "{}_{}",
        record
            .started_at
            .to_rfc3339_opts(SecondsFormat::AutoSi, true),
        record.run_id
    )
}

fn parse_cursor(cursor: &str) -> Result<(DateTime<Utc>, String), RunIndexError> {
    let (at, id) = cursor.split_once('_').ok_or(RunIndexError::InvalidCursor)?;
    let at = DateTime::parse_from_rfc3339(at).map_err(|_| RunIndexError::InvalidCursor)?;
    Ok((at.to_utc(), id.to_owned()))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use claudeclaw_api::runs::{RunOutcome, RunSession};
    use serde_json::{Value, json};

    use super::*;
    use crate::run_log::{RunLog, RunLogBody};

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn setup() -> (tempfile::TempDir, Layout, RunIndex) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        fs::create_dir_all(layout.root()).unwrap();
        let index = RunIndex::new(layout.clone());
        (dir, layout, index)
    }

    /// A Rust record, from a TS one with the Rust fields filled in.
    fn record(run_id: &str, started_at: &str, run_log: Option<&str>) -> RunRecord {
        let file = RunLogFile {
            name: "daily".into(),
            agent: None,
            started_at: at(started_at),
            rel: run_log.unwrap_or("").into(),
            path: PathBuf::new(),
        };
        let mut record = ts_record(&file, RunLogHeader::default());
        record.run_id = run_id.into();
        record.origin = RunOrigin::Scheduler;
        record.run_log = run_log.map(str::to_owned);
        record
    }

    fn ids(page: &RunIndexPage) -> Vec<&str> {
        page.runs.iter().map(|r| r.run_id.as_str()).collect()
    }

    fn write_ts_log(layout: &Layout, name: &str, start: &str, exit_code: Option<i32>) -> RunLog {
        let log = RunLog::create(layout, name, at(start)).unwrap();
        let body = RunLogBody {
            date: at(start),
            session_id: Some("5e55-1d".into()),
            session: RunSession::New,
            fallback: false,
            routing: None,
            prompt: "p".into(),
            exit_code: Some(0),
            output: "o".into(),
            stderr: String::new(),
            run: None,
            noise: Vec::new(),
        };
        let mut text = body.render(name);
        match exit_code {
            Some(code) => text = text.replace("Exit code: 0", &format!("Exit code: {code}")),
            None => text = text.replace("Exit code: 0\n", ""),
        }
        fs::create_dir_all(log.path().parent().unwrap()).unwrap();
        fs::write(log.path(), text).unwrap();
        log
    }

    #[test]
    fn appends_go_to_the_start_month_and_pages_are_newest_first() {
        let (_dir, layout, index) = setup();
        // Out of order on purpose: month files are not time-ordered (#44 D3).
        for (id, start) in [
            ("b", "2026-10-02T00:00:00Z"),
            ("a", "2026-10-01T00:00:00Z"),
            ("z", "2026-09-30T23:59:59.999Z"),
            ("c", "2026-10-02T00:00:00Z"),
            ("d", "2026-10-03T00:00:00Z"),
        ] {
            index.append(&record(id, start, None)).unwrap();
        }
        assert!(layout.run_index("2026-09").is_file());
        assert_eq!(
            fs::read_to_string(layout.run_index("2026-10"))
                .unwrap()
                .lines()
                .count(),
            4
        );

        let page = index.page(None, 2, |_| true).unwrap();
        assert_eq!(ids(&page), ["d", "c"]);
        let page = index.page(page.next.as_deref(), 2, |_| true).unwrap();
        assert_eq!(ids(&page), ["b", "a"]);
        let page = index.page(page.next.as_deref(), 2, |_| true).unwrap();
        assert_eq!(ids(&page), ["z"]);
        assert_eq!(page.next, None);

        // Filters apply before the limit.
        let page = index.page(None, 10, |r| r.run_id != "c").unwrap();
        assert_eq!(ids(&page), ["d", "b", "a", "z"]);
        assert_eq!(page.next, None);

        assert!(matches!(
            index.page(Some("nonsense"), 1, |_| true),
            Err(RunIndexError::InvalidCursor)
        ));
        assert!(index.page(None, 0, |_| true).unwrap().runs.len() == 1);
    }

    #[test]
    fn torn_and_unparsed_lines_are_skipped() {
        let (_dir, layout, index) = setup();
        index
            .append(&record("a", "2026-10-01T00:00:00Z", None))
            .unwrap();
        let path = layout.run_index("2026-10");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"not\":\"a record\"}\n{\"run_id\":\"b\",\"sta")
            .unwrap();
        drop(file);
        let page = index.page(None, 10, |_| true).unwrap();
        assert_eq!(ids(&page), ["a"]);

        // The next append starts on a fresh line; the torn fragment becomes an unparsed line.
        index
            .append(&record("c", "2026-10-02T00:00:00Z", None))
            .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"sta\n{"), "{text}");
        let page = index.page(None, 10, |_| true).unwrap();
        assert_eq!(ids(&page), ["c", "a"]);
    }

    #[test]
    fn backfill_adds_each_ts_log_once() {
        let (_dir, layout, index) = setup();
        write_ts_log(&layout, "daily", "2026-09-01T09:00:00.123Z", Some(0));
        write_ts_log(&layout, "daily", "2026-10-01T09:00:00.000Z", Some(124));
        write_ts_log(&layout, "ops/digest", "2026-10-02T09:00:00.000Z", Some(1));
        write_ts_log(&layout, "chat", "2026-10-03T09:00:00.000Z", None);
        // This Worker's own run: at or after boot, never back-filled (#44 D2).
        write_ts_log(&layout, "daily", "2026-10-10T00:00:00.000Z", Some(0));
        fs::write(layout.run_logs_dir().join("daemon.log"), "x").unwrap();
        let boot = at("2026-10-10T00:00:00Z");

        let listing = RunLogListing::scan(&layout).unwrap();
        let mut calls = Vec::new();
        let report = index
            .backfill(&listing, boot, |done, total| calls.push((done, total)))
            .unwrap();
        assert_eq!(
            report,
            BackfillReport {
                added: 4,
                unknown: 1
            }
        );
        // Newest month first.
        assert_eq!(calls, [(0, 4), (3, 4), (4, 4)]);

        let page = index.page(None, 10, |_| true).unwrap();
        let got: Vec<_> = page
            .runs
            .iter()
            .map(|r| (r.name.as_str(), r.outcome, r.exit_code, r.agent.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("chat", Some(RunOutcome::Unknown), None, None),
                ("ops/digest", Some(RunOutcome::Failed), Some(1), Some("ops")),
                ("daily", Some(RunOutcome::Timeout), Some(124), None),
                ("daily", Some(RunOutcome::Ok), Some(0), None),
            ]
        );

        // The every-start check finds nothing missing.
        let mut calls = Vec::new();
        let again = index
            .backfill(&listing, boot, |d, t| calls.push((d, t)))
            .unwrap();
        assert_eq!(again, BackfillReport::default());
        assert_eq!(calls, [(0, 0)]);
        assert_eq!(index.page(None, 10, |_| true).unwrap().runs.len(), 4);
    }

    #[test]
    fn a_backfilled_record_matches_the_api_shape() {
        let (_dir, layout, index) = setup();
        write_ts_log(
            &layout,
            "daily-report",
            "2026-09-01T09:00:00.123Z",
            Some(124),
        );
        let listing = RunLogListing::scan(&layout).unwrap();
        index
            .backfill(&listing, at("2026-10-01T00:00:00Z"), |_, _| {})
            .unwrap();
        let line = fs::read_to_string(layout.run_index("2026-09")).unwrap();
        let mut value: Value = serde_json::from_str(line.trim_end()).unwrap();
        let rel = "logs/daily-report-2026-09-01T09-00-00-123Z.log";
        assert_eq!(value["run_id"], json!(ts_run_id(rel)));
        // The same fields as `claudeclaw_api::runs`' TS fixture (#23 D4, #44 D2).
        value["run_id"] = json!("x");
        assert_eq!(
            value,
            json!({
                "run_id": "x", "turn_id": null, "name": "daily-report", "scope": null,
                "origin": "ts", "launcher": null, "job": null, "agent": null,
                "session_id": "5e55-1d", "session": "new", "model": null,
                "started_at": "2026-09-01T09:00:00.123Z", "ended_at": null, "duration_ms": null,
                "outcome": "timeout", "exit_code": 124, "is_error": null,
                "api_error_status": null, "attempt": 1, "tokens_in": null, "tokens_out": null,
                "cache_read_tokens": null, "cache_write_tokens": null, "cost_usd": null,
                "cost_status": "no_usage", "cost_basis": null, "provider": null,
                "api_key_source": null, "models": [], "run_log": rel,
                "launcher_resolved": null, "purpose": null, "settle": null, "awake_ms": null,
                "suspended_ms": null, "wake_grace": false, "claude_code_version": null,
                "lingered": false,
            })
        );
    }

    #[test]
    fn ts_run_ids_are_stable_v5_uuids() {
        let id = ts_run_id("logs/a-2026-09-01T09-00-00-123Z.log");
        assert_eq!(id, ts_run_id("logs/a-2026-09-01T09-00-00-123Z.log"));
        assert_ne!(id, ts_run_id("logs/b-2026-09-01T09-00-00-123Z.log"));
        let uuid: Uuid = id.parse().unwrap();
        assert_eq!(uuid.get_version_num(), 5);
    }

    #[test]
    fn compaction_keeps_only_records_with_a_live_run_log() {
        let (_dir, layout, index) = setup();
        let kept = write_ts_log(&layout, "daily", "2026-08-01T00:00:00.000Z", Some(0));
        let gone = write_ts_log(&layout, "daily", "2026-08-02T00:00:00.000Z", Some(0));
        let old = write_ts_log(&layout, "weekly", "2026-07-01T00:00:00.000Z", Some(0));
        let recent = write_ts_log(&layout, "daily", "2026-10-01T00:00:00.000Z", Some(0));
        index
            .append(&record("k", "2026-08-01T00:00:00Z", Some(kept.rel())))
            .unwrap();
        index
            .append(&record("g", "2026-08-02T00:00:00Z", Some(gone.rel())))
            .unwrap();
        index
            .append(&record("refused", "2026-08-03T00:00:00Z", None))
            .unwrap();
        index
            .append(&record("o", "2026-07-01T00:00:00Z", Some(old.rel())))
            .unwrap();
        index
            .append(&record("r", "2026-10-01T00:00:00Z", Some(recent.rel())))
            .unwrap();
        index
            .append(&record("r2", "2026-10-02T00:00:00Z", None))
            .unwrap();
        fs::remove_file(gone.path()).unwrap();
        fs::remove_file(old.path()).unwrap();
        fs::remove_file(recent.path()).unwrap();

        // 2026-08 ended 2026-09-01; with 30 days kept, on 2026-10-15 only July and August
        // qualify. October is untouched although its Run log is gone.
        let now = at("2026-10-15T00:00:00Z");
        let before = fs::read(layout.run_index("2026-08")).unwrap().len() as u64
            + fs::read(layout.run_index("2026-07")).unwrap().len() as u64;
        let dry = index.compact(30, now, true).unwrap();
        assert_eq!(
            fs::read(layout.run_index("2026-08")).unwrap().len() as u64
                + fs::read(layout.run_index("2026-07")).unwrap().len() as u64,
            before
        );
        let report = index.compact(30, now, false).unwrap();
        assert_eq!(dry, report);
        assert_eq!(report.files_rewritten, 1);
        assert_eq!(report.files_deleted, 1);
        assert_eq!(report.records_dropped, 3);
        assert!(report.freed_bytes > 0);
        assert!(!layout.run_index("2026-07").exists());
        let page = index.page(None, 10, |_| true).unwrap();
        assert_eq!(ids(&page), ["r2", "r", "k"]);

        // A month that is already compact is left alone.
        assert_eq!(
            index.compact(30, now, false).unwrap(),
            CompactReport::default()
        );
        // With a longer keep, August doesn't qualify yet.
        index
            .append(&record("g2", "2026-08-05T00:00:00Z", None))
            .unwrap();
        assert_eq!(
            index.compact(60, now, false).unwrap(),
            CompactReport::default()
        );
    }
}
