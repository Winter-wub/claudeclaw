//! Reading the Daemon log (architecture §4 `store::logs`, §9.1 Logs; #20 D3, #23 D5).
//!
//! The Daemon log is `daemon/logs/worker.<YYYY-MM-DD>.jsonl` and `watchdog.<YYYY-MM-DD>.jsonl`:
//! one `tracing` JSON line each, event fields flattened and span fields under `span` (#23 D2).
//! This module reads them back as [`LogEntry`], for `claudeclaw logs --offline --json` (NDJSON,
//! #20 D3) and for the Bug-report export ([`crate::export`], #28).
//!
//! Reading is read-only: no lock, nothing created (#20 D3). A line that doesn't parse (a torn
//! last line, foreign text) comes back as [`LogEntry::Raw`] and is never skipped (#23 D5).

use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use claudeclaw_api::logs::{LogEntry, LogLevel, LogLine, RawLogLine};
use serde_json::{Map, Value};

use crate::layout::Layout;

/// One request reads at most this many bytes, newest files first; past it the read stops and
/// says so with [`LogRead::scan_truncated`] (#23 D5's 50 MB bound).
pub const SCAN_LIMIT_BYTES: u64 = 50_000_000;

/// Which Daemon log to read (management-api §2.9 `logs.tail`'s `source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogSource {
    /// `worker.<date>.jsonl`.
    Worker,
    /// `watchdog.<date>.jsonl`.
    Watchdog,
    /// Worker and Watchdog merged by timestamp, as `logs --source daemon` (#22).
    Daemon,
}

impl LogSource {
    fn processes(self) -> &'static [&'static str] {
        match self {
            LogSource::Worker => &["worker"],
            LogSource::Watchdog => &["watchdog"],
            LogSource::Daemon => &["worker", "watchdog"],
        }
    }
}

/// The filters of management-api §2.9 `logs.tail`, plus the time window of
/// `claudeclaw logs --offline --since` (#20 D3). Every filter is optional.
///
/// A [`LogEntry::Raw`] line has no level, event, scope or run, so those filters let it through:
/// unparseable lines are never skipped (#23 D5). It is placed in time by the line before it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogQuery {
    /// Lines at or after this time.
    pub since: Option<DateTime<Utc>>,
    /// Lines before this time.
    pub until: Option<DateTime<Utc>>,
    /// Lines at this level or more severe. A level this build doesn't know passes.
    pub min_level: Option<LogLevel>,
    /// A prefix of `event`, e.g. `run.`.
    pub event: Option<String>,
    /// The Scope key in `span.scope` or `fields.scope`, e.g. `job:example`.
    pub scope: Option<String>,
    /// `span.run_id` or `fields.run_id`.
    pub run_id: Option<String>,
    /// A substring of `message` (of the text, for a raw line).
    pub q: Option<String>,
    /// Keep only the newest `limit` lines that pass.
    pub limit: Option<usize>,
}

/// What [`read`] found: oldest first.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRead {
    pub entries: Vec<LogEntry>,
    /// [`SCAN_LIMIT_BYTES`] stopped the read before the oldest file in the window.
    pub scan_truncated: bool,
}

/// A Daemon log that couldn't be read. Messages name the file, never its contents.
#[derive(Debug, thiserror::Error)]
pub enum LogsError {
    #[error("{op} {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_err(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> LogsError {
    let path = path.to_path_buf();
    move |source| LogsError::Io { op, path, source }
}

/// Reads the Daemon log lines that pass `query`, oldest first (#20 D3, #23 D5).
///
/// Each line carries a `cursor`: `<file date>:<byte offset>` for one process, and
/// `<process>:<file date>:<byte offset>` for [`LogSource::Daemon`]. A missing `daemon/logs/`
/// reads as empty.
pub fn read(layout: &Layout, source: LogSource, query: &LogQuery) -> Result<LogRead, LogsError> {
    let (mut lines, scan_truncated) = read_positioned(layout, source, query)?;
    if let Some(limit) = query.limit {
        let drop = lines.len().saturating_sub(limit);
        lines.drain(..drop);
    }
    Ok(LogRead {
        entries: lines.into_iter().map(|l| l.entry).collect(),
        scan_truncated,
    })
}

/// Writes `entries` as NDJSON, one [`LogEntry`] per line (#20 D3).
pub fn write_ndjson<W: Write>(entries: &[LogEntry], mut out: W) -> io::Result<()> {
    for entry in entries {
        serde_json::to_writer(&mut out, entry)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// Parses one Daemon log line (#23 D2). `seq`, `timestamp`, `level` and `target` are required;
/// `message` defaults to empty; every other top-level key but `span` goes into `fields`.
/// Anything else, including invalid JSON, is a [`LogEntry::Raw`].
pub fn parse_line(line: &str, cursor: Option<String>) -> LogEntry {
    match parse_object(line) {
        Some(mut line) => {
            line.cursor = cursor;
            LogEntry::Line(Box::new(line))
        }
        None => LogEntry::Raw(RawLogLine {
            cursor,
            raw: line.to_owned(),
        }),
    }
}

fn parse_object(line: &str) -> Option<LogLine> {
    let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(line) else {
        return None;
    };
    let seq = map.shift_remove("seq")?.as_u64()?;
    let timestamp = serde_json::from_value(map.shift_remove("timestamp")?).ok()?;
    let level = serde_json::from_value(map.shift_remove("level")?).ok()?;
    let Value::String(target) = map.shift_remove("target")? else {
        return None;
    };
    let message = match map.shift_remove("message") {
        None => String::new(),
        Some(Value::String(m)) => m,
        Some(_) => return None,
    };
    let event = match map.shift_remove("event") {
        None | Some(Value::Null) => None,
        Some(Value::String(e)) => Some(e),
        Some(_) => return None,
    };
    let span = match map.shift_remove("span") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(span)) => span,
        Some(_) => return None,
    };
    Some(LogLine {
        cursor: None,
        seq,
        timestamp,
        level,
        target,
        event,
        message,
        span,
        fields: map,
    })
}

/// A line with its place in time, so Worker and Watchdog lines merge and raw lines window.
pub(crate) struct Positioned {
    /// The line's timestamp; a raw line's is the line before it in its file, or the file's
    /// date at midnight UTC.
    pub at: DateTime<Utc>,
    process: usize,
    date: NaiveDate,
    offset: u64,
    pub entry: LogEntry,
}

/// The lines in `query`'s window that pass its filters, oldest first, ignoring `limit`.
pub(crate) fn read_positioned(
    layout: &Layout,
    source: LogSource,
    query: &LogQuery,
) -> Result<(Vec<Positioned>, bool), LogsError> {
    let mut files = list_files(layout, source)?;
    files.retain(|f| {
        query.since.is_none_or(|s| f.date >= s.date_naive())
            && query.until.is_none_or(|u| f.date <= u.date_naive())
    });
    // Newest first, so the scan bound keeps the most recent lines.
    files.sort_by(|a, b| b.date.cmp(&a.date).then(a.process.cmp(&b.process)));

    let mut budget = SCAN_LIMIT_BYTES;
    let mut truncated = false;
    let mut lines = Vec::new();
    for file in &files {
        if budget == 0 {
            truncated = true;
            break;
        }
        let (bytes, start, cut) = read_tail(&file.path, budget)?;
        budget = budget.saturating_sub(bytes.len() as u64);
        truncated |= cut;
        let label = (source == LogSource::Daemon).then(|| source.processes()[file.process]);
        collect_lines(file, label, &bytes, start, query, &mut lines);
    }
    lines.sort_by(|a, b| {
        a.at.cmp(&b.at)
            .then(a.date.cmp(&b.date))
            .then(a.process.cmp(&b.process))
            .then(a.offset.cmp(&b.offset))
    });
    Ok((lines, truncated))
}

struct LogFile {
    process: usize,
    date: NaiveDate,
    path: PathBuf,
}

fn list_files(layout: &Layout, source: LogSource) -> Result<Vec<LogFile>, LogsError> {
    let dir = layout.daemon_logs_dir();
    let dir = dir.as_path();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err("list", dir)(e)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_err("list", dir))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        for (process, prefix) in source.processes().iter().enumerate() {
            // `<process>.<YYYY-MM-DD>.jsonl`; `Layout::daemon_log` is the canonical name.
            let Some(date) = name.get(prefix.len() + 1..prefix.len() + 11) else {
                continue;
            };
            let Ok(parsed) = NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
                continue;
            };
            if entry.path() == layout.daemon_log(prefix, date) {
                files.push(LogFile {
                    process,
                    date: parsed,
                    path: entry.path(),
                });
            }
        }
    }
    Ok(files)
}

/// The last `budget` bytes of `path`, starting at a line boundary, with that start offset and
/// whether anything was cut.
fn read_tail(path: &Path, budget: u64) -> Result<(Vec<u8>, u64, bool), LogsError> {
    let mut file = File::open(path).map_err(io_err("open", path))?;
    let len = file.metadata().map_err(io_err("stat", path))?.len();
    let start = len.saturating_sub(budget);
    file.seek(SeekFrom::Start(start))
        .map_err(io_err("seek", path))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes).map_err(io_err("read", path))?;
    if start == 0 {
        return Ok((bytes, 0, false));
    }
    // Drop the partial first line.
    let skip = bytes
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |i| i + 1);
    bytes.drain(..skip);
    Ok((bytes, start + skip as u64, true))
}

fn collect_lines(
    file: &LogFile,
    label: Option<&str>,
    bytes: &[u8],
    start: u64,
    query: &LogQuery,
    out: &mut Vec<Positioned>,
) {
    let mut at = file
        .date
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_utc();
    let mut offset = start;
    for chunk in bytes.split_inclusive(|&b| b == b'\n') {
        let here = offset;
        offset += chunk.len() as u64;
        let text = String::from_utf8_lossy(chunk);
        let text = text.trim_end_matches(['\n', '\r']);
        if text.trim().is_empty() {
            continue;
        }
        let cursor = match label {
            Some(process) => format!("{process}:{}:{here}", file.date),
            None => format!("{}:{here}", file.date),
        };
        let entry = parse_line(text, Some(cursor));
        if let LogEntry::Line(line) = &entry {
            at = line.timestamp;
        }
        if in_window(at, query) && passes(&entry, query) {
            out.push(Positioned {
                at,
                process: file.process,
                date: file.date,
                offset: here,
                entry,
            });
        }
    }
}

fn in_window(at: DateTime<Utc>, query: &LogQuery) -> bool {
    query.since.is_none_or(|s| at >= s) && query.until.is_none_or(|u| at < u)
}

fn passes(entry: &LogEntry, query: &LogQuery) -> bool {
    let line = match entry {
        LogEntry::Line(line) => line,
        LogEntry::Raw(raw) => {
            return query
                .q
                .as_ref()
                .is_none_or(|q| raw.raw.contains(q.as_str()));
        }
    };
    if let Some(min) = query.min_level
        && severity(line.level).cmp(&severity(min)) == Ordering::Less
    {
        return false;
    }
    if let Some(prefix) = &query.event
        && !line
            .event
            .as_deref()
            .is_some_and(|e| e.starts_with(prefix.as_str()))
    {
        return false;
    }
    let correlated = |key: &str, want: &str| {
        [&line.span, &line.fields]
            .iter()
            .any(|map| map.get(key).and_then(Value::as_str) == Some(want))
    };
    if let Some(scope) = &query.scope
        && !correlated("scope", scope)
    {
        return false;
    }
    if let Some(run_id) = &query.run_id
        && !correlated("run_id", run_id)
    {
        return false;
    }
    query
        .q
        .as_ref()
        .is_none_or(|q| line.message.contains(q.as_str()))
}

/// Higher is more severe; a level this build doesn't know ranks with ERROR, so no filter hides
/// it.
pub(crate) fn severity(level: LogLevel) -> u8 {
    match level {
        LogLevel::Trace => 0,
        LogLevel::Debug => 1,
        LogLevel::Info => 2,
        LogLevel::Warn => 3,
        LogLevel::Error | LogLevel::Unknown => 4,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn line(seq: u64, ts: &str, level: &str, event: &str, extra: Value) -> String {
        let mut v = json!({
            "seq": seq, "timestamp": ts, "level": level, "message": format!("m{seq}"),
            "event": event, "target": "claudeclaw_core::x",
            "span": {"run_id": format!("r{seq}"), "scope": "job:example"},
        });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v.to_string()
    }

    fn setup(files: &[(&str, &[String])]) -> (TempDir, Layout) {
        let dir = TempDir::new().unwrap();
        let layout = Layout::new(dir.path());
        fs::create_dir_all(layout.daemon_logs_dir()).unwrap();
        for (name, lines) in files {
            let mut text = lines.join("\n");
            text.push('\n');
            fs::write(layout.daemon_logs_dir().join(name), text).unwrap();
        }
        (dir, layout)
    }

    fn seqs(read: &LogRead) -> Vec<String> {
        read.entries
            .iter()
            .map(|e| match e {
                LogEntry::Line(l) => l.seq.to_string(),
                LogEntry::Raw(r) => format!("raw:{}", r.raw),
            })
            .collect()
    }

    #[test]
    fn a_line_parses_with_flattened_fields_and_a_cursor() {
        let text = r#"{"seq":1,"timestamp":"2026-10-13T08:30:27.512585Z","level":"INFO","message":"run started","event":"run.start","attempt":1,"target":"claudeclaw_core::conversations","span":{"run_id":"0199e0c2","scope":"job:example","launcher":"direct"}}"#;
        let LogEntry::Line(line) = parse_line(text, Some("2026-10-13:0".into())) else {
            panic!("expected a parsed line")
        };
        assert_eq!(line.cursor.as_deref(), Some("2026-10-13:0"));
        assert_eq!(line.level, LogLevel::Info);
        assert_eq!(line.event.as_deref(), Some("run.start"));
        assert_eq!(
            line.fields,
            json!({"attempt": 1}).as_object().unwrap().clone()
        );
        assert_eq!(line.span["launcher"], "direct");
        let text = r#"{"seq":1,"timestamp":"2026-10-13T08:30:27Z","level":"INFO","b":1,"target":"t","a":2,"c":3}"#;
        let LogEntry::Line(line) = parse_line(text, None) else {
            panic!("expected a parsed line")
        };
        let keys: Vec<&str> = line.fields.keys().map(String::as_str).collect();
        assert_eq!(keys, ["b", "a", "c"], "fields keep their order");
    }

    #[test]
    fn unparseable_lines_come_back_raw() {
        for text in [
            r#"{"seq":2,"timest"#,
            "thread 'main' panicked at src/main.rs:1:1",
            r#"{"seq":"x","timestamp":"2026-10-13T00:00:00Z","level":"INFO","target":"t"}"#,
            r#"{"timestamp":"2026-10-13T00:00:00Z","level":"INFO","target":"t"}"#,
            "[1, 2]",
        ] {
            assert_eq!(
                parse_line(text, None),
                LogEntry::Raw(RawLogLine {
                    cursor: None,
                    raw: text.into()
                }),
                "{text}"
            );
        }
    }

    #[test]
    fn a_raw_line_passes_through_every_filter_but_q() {
        let (_dir, layout) = setup(&[(
            "worker.2026-10-13.jsonl",
            &[
                line(1, "2026-10-13T01:00:00Z", "ERROR", "run.finish", json!({})),
                "panicked at oops".into(),
                line(2, "2026-10-13T02:00:00Z", "DEBUG", "api.request", json!({})),
            ],
        )]);
        let query = LogQuery {
            min_level: Some(LogLevel::Error),
            event: Some("run.".into()),
            scope: Some("job:example".into()),
            ..LogQuery::default()
        };
        let read = super::read(&layout, LogSource::Worker, &query).unwrap();
        assert_eq!(seqs(&read), ["1", "raw:panicked at oops"]);
        let LogEntry::Raw(raw) = &read.entries[1] else {
            unreachable!()
        };
        let offset = line(1, "2026-10-13T01:00:00Z", "ERROR", "run.finish", json!({})).len() + 1;
        assert_eq!(
            raw.cursor.as_deref(),
            Some(&*format!("2026-10-13:{offset}"))
        );

        let q = LogQuery {
            q: Some("m2".into()),
            ..LogQuery::default()
        };
        let read = super::read(&layout, LogSource::Worker, &q).unwrap();
        assert_eq!(seqs(&read), ["2"]);
    }

    #[test]
    fn filters_by_window_level_event_scope_run_and_message() {
        let lines = [
            line(1, "2026-10-12T23:00:00Z", "INFO", "run.start", json!({})),
            line(2, "2026-10-13T01:00:00Z", "WARN", "run.finish", json!({})),
            line(3, "2026-10-13T02:00:00Z", "INFO", "job.fire", json!({})),
            line(4, "2026-10-13T03:00:00Z", "TRACE", "run.start", json!({})),
            line(5, "2026-10-13T04:00:00Z", "NOTICE", "run.later", json!({})),
            line(
                6,
                "2026-10-13T05:00:00Z",
                "INFO",
                "run.start",
                json!({"span": {"scope": "global", "run_id": "other"}}),
            ),
        ];
        let (_dir, layout) = setup(&[
            ("worker.2026-10-12.jsonl", &lines[..1]),
            ("worker.2026-10-13.jsonl", &lines[1..]),
        ]);
        let all = |q: LogQuery| seqs(&super::read(&layout, LogSource::Worker, &q).unwrap());
        assert_eq!(all(LogQuery::default()), ["1", "2", "3", "4", "5", "6"]);
        assert_eq!(
            all(LogQuery {
                since: Some(at("2026-10-13T01:00:00Z")),
                until: Some(at("2026-10-13T04:00:00Z")),
                ..LogQuery::default()
            }),
            ["2", "3", "4"]
        );
        assert_eq!(
            all(LogQuery {
                min_level: Some(LogLevel::Info),
                ..LogQuery::default()
            }),
            ["1", "2", "3", "5", "6"],
            "an unknown level is never hidden"
        );
        assert_eq!(
            all(LogQuery {
                event: Some("run.".into()),
                ..LogQuery::default()
            }),
            ["1", "2", "4", "5", "6"]
        );
        assert_eq!(
            all(LogQuery {
                scope: Some("global".into()),
                ..LogQuery::default()
            }),
            ["6"]
        );
        assert_eq!(
            all(LogQuery {
                run_id: Some("r3".into()),
                ..LogQuery::default()
            }),
            ["3"]
        );
        assert_eq!(
            all(LogQuery {
                limit: Some(2),
                ..LogQuery::default()
            }),
            ["5", "6"],
            "limit keeps the newest"
        );
    }

    #[test]
    fn the_daemon_source_merges_worker_and_watchdog_by_time() {
        let (_dir, layout) = setup(&[
            (
                "worker.2026-10-13.jsonl",
                &[
                    line(1, "2026-10-13T01:00:00Z", "INFO", "a", json!({})),
                    line(3, "2026-10-13T03:00:00Z", "INFO", "a", json!({})),
                ],
            ),
            (
                "watchdog.2026-10-13.jsonl",
                &[line(2, "2026-10-13T02:00:00Z", "INFO", "a", json!({}))],
            ),
            ("unrelated.2026-10-13.jsonl", &["x".into()]),
        ]);
        let read = super::read(&layout, LogSource::Daemon, &LogQuery::default()).unwrap();
        assert_eq!(seqs(&read), ["1", "2", "3"]);
        let LogEntry::Line(l) = &read.entries[1] else {
            unreachable!()
        };
        assert_eq!(l.cursor.as_deref(), Some("watchdog:2026-10-13:0"));
        let worker = super::read(&layout, LogSource::Watchdog, &LogQuery::default()).unwrap();
        assert_eq!(seqs(&worker), ["2"]);
    }

    #[test]
    fn a_missing_log_dir_reads_as_empty() {
        let dir = TempDir::new().unwrap();
        let read = super::read(
            &Layout::new(dir.path()),
            LogSource::Daemon,
            &LogQuery::default(),
        )
        .unwrap();
        assert_eq!(read.entries, []);
        assert!(!read.scan_truncated);
    }

    #[test]
    fn the_scan_bound_keeps_the_newest_lines_and_says_so() {
        let first = line(1, "2026-10-13T01:00:00Z", "INFO", "a", json!({}));
        let second = line(2, "2026-10-13T02:00:00Z", "INFO", "a", json!({}));
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("worker.2026-10-13.jsonl");
        fs::write(&path, format!("{first}\n{second}\n")).unwrap();
        let (bytes, start, cut) = read_tail(&path, second.len() as u64 + 5).unwrap();
        assert!(cut);
        assert_eq!(start, first.len() as u64 + 1);
        assert_eq!(bytes, format!("{second}\n").into_bytes());
    }

    #[test]
    fn ndjson_is_one_entry_per_line() {
        let entries = vec![
            parse_line(
                &line(1, "2026-10-13T01:00:00Z", "INFO", "a", json!({})),
                None,
            ),
            parse_line("torn", Some("2026-10-13:9".into())),
        ];
        let mut out = Vec::new();
        write_ndjson(&entries, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let back: Vec<LogEntry> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(back, entries);
    }
}
