//! The Bug-report export (architecture §4 `store::export`; #28, #42, #69).
//!
//! [`export`] builds one reviewable Markdown file, in memory, from what is on disk plus what
//! the caller supplies ([`ExportInput`]: status, job schedules, Claude Code versions and host
//! facts, which need crates the store can't depend on). `claudeclaw logs export` and
//! `GET /v1/logs/export` both call it (#28 D7).
//!
//! - **Sections** (#28 D1): header, Summary, Status, Settings shape, Jobs, Crash record, Runs,
//!   Daemon log. Run logs, the audit log and settings values are never read into it (D2).
//! - **Pseudonyms** (D3, D4): every identifier becomes `<kind>:#n`. One [`ExportInput`] gets one
//!   map, numbered per kind on first appearance in output order, with no hash and no salt: the
//!   same input always gives the same file, and numbering restarts in every export, so a
//!   different window numbers differently and two exports can't be linked.
//! - **Scrubbing** (D5): each field by its `api::export::FIELD_CLASSES` class; free text through
//!   the known-secret scrub, the project dir, paths, known values, snowflakes and IP literals.
//! - **Fails closed** (D5.4): a verify pass scans the finished text for every high-entropy
//!   identifier the export saw. A hit returns [`ExportError::Leak`] (`export_leak`), naming the
//!   section, field and kind, never the value, and nothing is returned to write.
//! - **Size** (D6): 10 MiB by default, at most 24 MiB. Over the cap, Daemon log lines go first:
//!   TRACE, then DEBUG, then the oldest INFO, then the oldest WARN and above; the manifest and
//!   the Summary say what was trimmed. The other sections are never trimmed, so if they alone
//!   are over the cap the file comes back over it.

mod collect;
mod render;
mod scrub;
mod verify;

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use claudeclaw_api::error::ErrorCode;
use claudeclaw_api::export::{
    ExportManifest, ExportSection, ExportTrimmed, ExportVersions, ExportWindow, LogExport,
    PseudonymKind, settings_value_allowed,
};
use claudeclaw_api::jobs::{JobDiagnostic, JobSummary};
use claudeclaw_api::logs::{LogEntry, LogLevel};
use claudeclaw_api::runs::RunRecord;
use claudeclaw_api::scopes::ClaudeCodeSeen;
use claudeclaw_api::status::DaemonStatus;
use secrecy::SecretString;
use serde_json::{Map, Value, json};

use self::collect::{CrashState, SECRET_PATHS, SettingsState};
use self::render::Section;
use self::scrub::Scrubber;
use crate::error::StoreError;
use crate::layout::Layout;
use crate::logs::{self, LogQuery, LogSource, LogsError, Positioned, severity};

/// The format marker, `claudeclaw-export/1` (#28 D1.1).
pub const FORMAT: &str = "claudeclaw-export/1";
/// The default window: the last 24 h (#28 D1).
pub const DEFAULT_WINDOW: TimeDelta = TimeDelta::hours(24);
/// The default size cap, 10 MiB (#28 D6).
pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// The largest size cap, 24 MiB, under GitHub's 25 MB attachment limit (#28 D6).
pub const MAX_BYTES: u64 = 24 * 1024 * 1024;
/// The Summary's size budget, so it can be pasted into an issue body (#28 D1.2).
pub const SUMMARY_MAX_BYTES: usize = 2048;
/// The newest crash's `fatal` in the Summary, after scrubbing.
const FATAL_MAX_BYTES: usize = 300;
/// Caller-supplied strings in the Summary (versions, OS, install method).
const SUMMARY_FIELD_MAX_BYTES: usize = 48;
/// `Claude Code … via …` rows in the Summary, at most.
const CLAUDE_CODE_ROWS: usize = 5;

const NOTICE: &str =
    "Read this file before posting it. Identifiers are replaced, but you are the last check.";

/// What was asked for: `logs export`'s flags, or `logs.export`'s query (#28 D7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportRequest {
    /// Default: `until` minus 24 h.
    pub since: Option<DateTime<Utc>>,
    /// Default: [`ExportInput::now`].
    pub until: Option<DateTime<Utc>>,
    /// The lowest Daemon log level included; default every level on disk.
    pub min_level: Option<LogLevel>,
    /// Default [`DEFAULT_MAX_BYTES`]; more than [`MAX_BYTES`] is refused.
    pub max_bytes: Option<u64>,
    /// Return the reverse map (`--map`, `map=true`).
    pub map: bool,
}

/// What the caller knows and the store can't read itself (#28 D1, D8): the store depends only
/// on `api` (§3.1), so status, job schedules and Launcher versions arrive as `api` types.
#[derive(Debug, Clone)]
pub struct ExportInput {
    /// The export's creation time and the default `until`.
    pub now: DateTime<Utc>,
    pub versions: ExportVersions,
    /// `brew`, `archive`, `cargo` or `desktop` (#22 §7); `None` when unknown.
    pub install_method: Option<String>,
    pub os: OsInfo,
    /// The live `status.get`, or `client::offline_status` when the Daemon is down (#28 D1.3).
    pub status: Option<DaemonStatus>,
    /// One row per Job (#28 D1.5).
    pub jobs: Vec<ExportJob>,
    /// The Claude Code version seen per Launcher, from `LauncherInfo.claude_code` (#42 D5).
    /// When empty, the newest version per Launcher in the window's Run index is used.
    pub claude_code: Vec<ClaudeCodeRow>,
    pub host: HostFacts,
    /// Secrets the caller holds that aren't in `settings.json` (API token plaintexts, a pairing
    /// code), redacted wherever they appear (#28 D3).
    pub secrets: Vec<(SecretString, SecretLabel)>,
}

/// The OS line of the Summary (#28 D1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsInfo {
    /// `std::env::consts::OS`.
    pub family: String,
    /// `kern.osproductversion`, `PRETTY_NAME` or `RtlGetVersion`; `None` when not read.
    pub version: Option<String>,
    /// `std::env::consts::ARCH`.
    pub arch: String,
}

impl OsInfo {
    /// This build's OS family and arch, with no version.
    pub fn current() -> Self {
        OsInfo {
            family: std::env::consts::OS.to_owned(),
            version: None,
            arch: std::env::consts::ARCH.to_owned(),
        }
    }
}

/// The machine's identifiers, pseudonymised as `host`, `os-user` and `~` (#28 D3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFacts {
    pub hostname: Option<String>,
    pub os_user: Option<String>,
    pub home_dir: Option<PathBuf>,
}

impl HostFacts {
    /// Best effort from the environment: `USER`/`USERNAME`, `HOME`/`USERPROFILE`,
    /// `HOSTNAME`/`COMPUTERNAME`. The binary fills the hostname from the OS where it can.
    pub fn from_env() -> Self {
        let var = |names: &[&str]| {
            names
                .iter()
                .find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
        };
        HostFacts {
            hostname: var(&["HOSTNAME", "COMPUTERNAME"]),
            os_user: var(&["USER", "USERNAME"]),
            home_dir: var(&["HOME", "USERPROFILE"]).map(PathBuf::from),
        }
    }
}

/// One Jobs row's source (#28 D1.5): the summary and its findings, whose messages are dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportJob {
    pub summary: JobSummary,
    pub diagnostics: Vec<JobDiagnostic>,
}

/// `Claude Code <version> via <launcher>, seen <date>` (#42 D5).
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeCodeRow {
    /// `direct` or a Launcher path.
    pub launcher: String,
    pub seen: ClaudeCodeSeen,
}

/// What a redacted secret was: `<redacted:<label>>` (#28 D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretLabel {
    DiscordToken,
    ApiToken,
    LegacyApiToken,
    WebToken,
    PairingCode,
}

impl SecretLabel {
    pub const fn as_str(self) -> &'static str {
        match self {
            SecretLabel::DiscordToken => "discord-token",
            SecretLabel::ApiToken => "api-token",
            SecretLabel::LegacyApiToken => "legacy-api-token",
            SecretLabel::WebToken => "web-token",
            SecretLabel::PairingCode => "pairing-code",
        }
    }
}

/// Why an export produced nothing.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The verify pass found an identifier in the finished text (#28 D5.4): a Daemon bug in
    /// classification. Carries where and what kind, never the value.
    #[error("export_leak: a {kind} identifier survived in {section:?}, field {field}")]
    Leak {
        section: ExportSection,
        field: String,
        kind: &'static str,
    },
    /// `max_bytes` over [`MAX_BYTES`].
    #[error("max_bytes {requested} is over the maximum of {MAX_BYTES}")]
    TooLarge { requested: u64 },
    /// `since` is not before `until`.
    #[error("the export window is empty")]
    EmptyWindow,
    #[error(transparent)]
    Logs(#[from] LogsError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{op} {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl ExportError {
    /// The API error code (management-api §2.9): `export_leak`, `bad_request` for a request
    /// the export refuses, `internal` for a read failure (the CLI reports `data_unreadable`).
    pub fn code(&self) -> ErrorCode {
        match self {
            ExportError::Leak { .. } => ErrorCode::ExportLeak,
            ExportError::TooLarge { .. } | ExportError::EmptyWindow => ErrorCode::BadRequest,
            ExportError::Logs(_) | ExportError::Store(_) | ExportError::Io { .. } => {
                ErrorCode::Internal
            }
        }
    }
}

/// Builds the Bug-report export (#28). Read-only: it takes no lock and writes nothing; the
/// caller writes [`LogExport::text`] (and the map, if asked) only on `Ok`.
pub fn export(
    layout: &Layout,
    input: &ExportInput,
    request: &ExportRequest,
) -> Result<LogExport, ExportError> {
    let max_bytes = request.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    if max_bytes > MAX_BYTES {
        return Err(ExportError::TooLarge {
            requested: max_bytes,
        });
    }
    let until = request.until.unwrap_or(input.now);
    let since = request.since.unwrap_or(until - DEFAULT_WINDOW);
    if since >= until {
        return Err(ExportError::EmptyWindow);
    }

    let (settings, settings_file) = collect::settings(layout)?;
    let crash = collect::crash(layout, input)?;
    let (runs, runs_unparsed) = collect::runs(layout, since, until)?;
    let query = LogQuery {
        since: Some(since),
        until: Some(until),
        min_level: request.min_level,
        ..LogQuery::default()
    };
    let (lines, scan_truncated) = logs::read_positioned(layout, LogSource::Daemon, &query)?;
    let known = collect::known(layout, input, settings_file.as_ref(), &runs, &lines, &crash)?;
    let targets = known.verify_targets();
    let key_targets = known.key_targets();
    let parts = Parts {
        input,
        window: ExportWindow { since, until },
        settings: &settings,
        crash: &crash,
        runs: &runs,
        runs_unparsed,
        lines: &lines,
        scan_truncated,
    };

    // Render, then trim Daemon log lines until the file fits. Each attempt starts a fresh
    // Pseudonymiser, so numbers follow the final file's order.
    let order = trim_order(&lines);
    let mut keep = vec![true; lines.len()];
    let mut next_trim = 0;
    let mut trimmed = Trimmed::default();
    loop {
        let mut scrubber = Scrubber::new(known.clone());
        let built = parts.build(&mut scrubber, &keep, &trimmed);
        let rendered: Vec<String> = built.sections.iter().map(Section::render).collect();
        let size: usize = rendered.iter().map(String::len).sum();
        let mut excess = size.saturating_sub(max_bytes as usize);
        if excess > 0 && next_trim < order.len() {
            // Room for the Summary's trimming sentence, which grows as lines go.
            excess += 128;
            while excess > 0 && next_trim < order.len() {
                let i = order[next_trim];
                next_trim += 1;
                keep[i] = false;
                trimmed.count(&lines[i]);
                excess = excess.saturating_sub(built.line_bytes[i]);
            }
            continue;
        }
        verify::verify(&built.sections, &rendered, &targets, &key_targets)?;
        let manifest = ExportManifest {
            format: FORMAT.to_owned(),
            created_at: input.now,
            versions: input.versions.clone(),
            window: parts.window.clone(),
            lines: count(keep.iter().filter(|k| **k).count()),
            runs: count(runs.len()),
            pseudonyms: scrubber.counts(),
            trimmed: trimmed.manifest(&lines, &keep),
            map_written: request.map,
        };
        return Ok(LogExport {
            manifest,
            text: rendered.concat(),
            map: request.map.then(|| scrubber.reverse().clone()),
        });
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Daemon log lines in trimming order (#28 D6): TRACE, DEBUG, INFO, then WARN and above, each
/// oldest first. Raw lines rank with WARN and above: they are mostly panic text.
fn trim_order(lines: &[Positioned]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by_key(|&i| (trim_rank(&lines[i].entry), i));
    order
}

fn trim_rank(entry: &LogEntry) -> u8 {
    match entry {
        LogEntry::Line(line) => severity(line.level).min(3),
        LogEntry::Raw(_) => 3,
    }
}

#[derive(Default)]
struct Trimmed {
    by_rank: [u32; 4],
}

impl Trimmed {
    fn count(&mut self, line: &Positioned) {
        self.by_rank[trim_rank(&line.entry) as usize] += 1;
    }

    fn total(&self) -> u32 {
        self.by_rank.iter().sum()
    }

    fn manifest(&self, lines: &[Positioned], keep: &[bool]) -> ExportTrimmed {
        ExportTrimmed {
            trace: self.by_rank[0],
            debug: self.by_rank[1],
            info: self.by_rank[2],
            warn_plus: self.by_rank[3],
            oldest_included: lines.iter().zip(keep).find(|(_, k)| **k).map(|(l, _)| l.at),
        }
    }
}

/// Everything collected, ready to scrub and render.
struct Parts<'a> {
    input: &'a ExportInput,
    window: ExportWindow,
    settings: &'a SettingsState,
    crash: &'a CrashState,
    runs: &'a [RunRecord],
    runs_unparsed: u32,
    lines: &'a [Positioned],
    scan_truncated: bool,
}

struct Built {
    sections: Vec<Section>,
    /// The rendered size of each Daemon log line, for trimming.
    line_bytes: Vec<usize>,
}

impl Parts<'_> {
    /// Scrubs and lays out every section, in D1's order, so Pseudonyms number in file order.
    /// Only what is kept is scrubbed: a trimmed line or a cut-off tail takes no number, so the
    /// map and `manifest.pseudonyms` list exactly what the file shows (#28 D4).
    fn build(&self, s: &mut Scrubber, keep: &[bool], trimmed: &Trimmed) -> Built {
        let kept = keep.iter().filter(|k| **k).count();
        let header =
            Section::new(ExportSection::Header).prose(format!("# {FORMAT}\n\n> {NOTICE}\n"));
        let summary = self.summary(s, kept, trimmed);
        let status = self.status(s);
        let settings = self.settings(s);
        let jobs = self.jobs(s);
        let crash = self.crash_section(s);
        let runs = self.runs_section(s);
        let mut line_bytes = vec![0; self.lines.len()];
        let mut log = Vec::with_capacity(kept);
        for (i, line) in self.lines.iter().enumerate() {
            if keep[i] {
                let value = s.log_entry(&line.entry);
                line_bytes[i] = value.to_string().len() + 1;
                log.push(value);
            }
        }
        let log_section = Section::new(ExportSection::DaemonLog)
            .prose(format!(
                "{kept} `worker` and `watchdog` lines, oldest first, merged by timestamp."
            ))
            .jsonl(log);
        Built {
            sections: vec![
                header,
                summary,
                status,
                settings,
                jobs,
                crash,
                runs,
                log_section,
            ],
            line_bytes,
        }
    }

    /// `<at>, <exit>, fatal: <scrubbed fatal>`; the fatal is scrubbed, then cut (#28 D1.2).
    fn newest_crash(&self, s: &mut Scrubber) -> Option<String> {
        let CrashState::Read(status) = self.crash else {
            return None;
        };
        let crash = status.crashes.first()?;
        let exit = match (&crash.exit.code, &crash.exit.signal) {
            (Some(code), _) => format!("exit code {code}"),
            (None, Some(signal)) => format!("signal {}", cap(signal)),
            (None, None) => "no exit status".to_owned(),
        };
        let mut line = format!("{}, {exit}", stamp(crash.at));
        if let Some(fatal) = &crash.fatal {
            let fatal = s.free_text_capped(fatal, FATAL_MAX_BYTES);
            line.push_str(&format!(", fatal: {fatal}"));
        }
        Some(line)
    }

    /// `(launcher, version, seen)` for the `Claude Code … via …` rows (#42 D5).
    fn claude_code_seen(&self) -> Vec<(String, String, DateTime<Utc>)> {
        let mut rows: Vec<(String, String, DateTime<Utc>)> = self
            .input
            .claude_code
            .iter()
            .map(|r| (r.launcher.clone(), r.seen.version.clone(), r.seen.seen_at))
            .collect();
        if rows.is_empty() {
            // The newest version per Launcher in the window's Run index.
            for run in self.runs.iter().rev() {
                let (Some(version), Some(launcher)) = (&run.claude_code_version, &run.launcher)
                else {
                    continue;
                };
                if !rows.iter().any(|(l, _, _)| l == launcher) {
                    rows.push((launcher.clone(), version.clone(), run.started_at));
                }
            }
        }
        rows.truncate(CLAUDE_CODE_ROWS);
        rows
    }

    /// At most [`SUMMARY_MAX_BYTES`]: caller-supplied strings are capped, the fatal is capped,
    /// and `Claude Code` rows are added only while they fit, so nothing scrubbed is cut off.
    fn summary(&self, s: &mut Scrubber, kept: usize, trimmed: &Trimmed) -> Section {
        let v = &self.input.versions;
        let mut out = vec![
            format!("- Format: {FORMAT}, created {}", stamp(self.input.now)),
            format!(
                "- claudeclaw {} ({}, {})",
                cap(&v.claudeclaw),
                cap(&v.commit),
                cap(&v.target)
            ),
            format!(
                "- Daemon: {}",
                v.daemon.as_deref().map_or("not answering".into(), cap)
            ),
        ];
        if let Some(app) = &v.app {
            out.push(format!("- Desktop app: {}", cap(app)));
        }
        out.push(format!(
            "- Install method: {}",
            self.input
                .install_method
                .as_deref()
                .map_or("unknown".into(), cap)
        ));
        let os = &self.input.os;
        out.push(format!(
            "- OS: {} {} ({})",
            cap(&os.family),
            os.version
                .as_deref()
                .map_or("(version unknown)".into(), cap),
            cap(&os.arch)
        ));
        match &self.input.status {
            Some(status) => {
                let kinds: BTreeSet<String> =
                    status.problems.iter().map(|p| wire(&p.kind)).collect();
                let kinds: Vec<String> = kinds.into_iter().collect();
                out.push(format!(
                    "- State: {}, health {}, problems: {}",
                    wire(&status.state),
                    wire(&status.health),
                    if kinds.is_empty() {
                        "none".to_owned()
                    } else {
                        kinds.join(", ")
                    }
                ));
            }
            None => out.push("- State: unknown".to_owned()),
        }
        let newest_crash = self.newest_crash(s);
        out.push(format!(
            "- Newest crash: {}",
            newest_crash.unwrap_or_else(|| "none recorded".to_owned())
        ));
        out.push(format!(
            "- Window: {} to {}; {kept} Daemon log lines, {} runs",
            stamp(self.window.since),
            stamp(self.window.until),
            self.runs.len()
        ));
        if trimmed.total() > 0 {
            let [trace, debug, info, warn] = trimmed.by_rank;
            out.push(format!(
                "- Trimmed to fit the size cap: {trace} TRACE, {debug} DEBUG, {info} INFO and \
                 {warn} WARN-or-above lines, oldest first"
            ));
        }
        if self.scan_truncated {
            out.push(format!(
                "- The {} MB per-process scan bound stopped before the start of the window",
                logs::SCAN_LIMIT_BYTES / 1_000_000
            ));
        }
        if self.runs_unparsed > 0 {
            out.push(format!(
                "- {} Run index lines didn't parse and are left out",
                self.runs_unparsed
            ));
        }
        let mut size = out.iter().map(|l| l.len() + 1).sum::<usize>();
        let mut rows = 0;
        for (launcher, version, at) in self.claude_code_seen() {
            let row = |s: &mut Scrubber| {
                let via = s.identifier_str(PseudonymKind::Launcher, "launcher", &launcher);
                format!(
                    "- Claude Code {} via {via}, seen {}",
                    s.free_text(&cap(&version)),
                    at.date_naive()
                )
            };
            // Try on a copy first, so a row that doesn't fit takes no number.
            if size + row(&mut s.clone()).len() + 1 > SUMMARY_MAX_BYTES {
                break;
            }
            let row = row(s);
            size += row.len() + 1;
            out.push(row);
            rows += 1;
        }
        if rows == 0 {
            out.push("- Claude Code version: (run claude --version)".to_owned());
        }
        Section::new(ExportSection::Summary).prose(out.join("\n"))
    }

    /// The `status.get` subset of #28 D1.3; `dir`, `socket`, `remote.listen`, the TLS
    /// fingerprint and pids are dropped.
    fn status(&self, s: &mut Scrubber) -> Section {
        let section = Section::new(ExportSection::Status);
        let Some(status) = &self.input.status else {
            return section.prose("No status was available.");
        };
        let problems: Vec<Value> = status
            .problems
            .iter()
            .map(|p| {
                let mut m = Map::new();
                m.insert("kind".into(), json!(p.kind));
                if let Some(scope) = &p.scope {
                    m.insert("scope".into(), s.scope(&scope.to_string()).into());
                }
                if let Some(job) = &p.job {
                    m.insert(
                        "job".into(),
                        s.identifier_str(PseudonymKind::Job, "job", job).into(),
                    );
                }
                if let Some(launcher) = &p.launcher {
                    m.insert(
                        "launcher".into(),
                        s.identifier_str(PseudonymKind::Launcher, "launcher", launcher)
                            .into(),
                    );
                }
                m.insert("since".into(), json!(p.since));
                Value::Object(m)
            })
            .collect();
        let default = s.identifier_str(
            PseudonymKind::Launcher,
            "launcher",
            &status.launchers.default,
        );
        let refused: Vec<Value> = status
            .launchers
            .refused_scopes
            .iter()
            .map(|scope| s.scope(&scope.to_string()).into())
            .collect();
        section.json(json!({
            "source": status.source,
            "state": status.state,
            "health": status.health,
            "problems": problems,
            "adapters": status.adapters.iter()
                .map(|a| json!({"transport": a.transport, "state": a.state}))
                .collect::<Vec<_>>(),
            "heartbeat": {"enabled": status.heartbeat.enabled, "next_at": status.heartbeat.next_at},
            "jobs": {
                "total": status.jobs.total,
                "invalid": status.jobs.invalid,
                "warnings": status.jobs.warnings,
            },
            "launchers": {"default": default, "refused_scopes": refused},
            "update": {"available": status.update.available, "latest": status.update.latest},
            "remote": {"enabled": status.remote.enabled},
        }))
    }

    /// Every key path; values only for booleans and `SETTINGS_VALUE_ALLOW` (#28 D1.4).
    fn settings(&self, s: &mut Scrubber) -> Section {
        let section = Section::new(ExportSection::Settings);
        match self.settings {
            SettingsState::Missing => section.prose("There is no `settings.json`."),
            SettingsState::Unreadable => {
                section.prose("`settings.json` exists but is not a JSON object.")
            }
            SettingsState::Read(raw) => section
                .prose(
                    "Key paths only. Values are kept for booleans and allowlisted keys; other \
                     strings show as `\"<set>\"`, arrays as `{\"len\": n}` and secrets as \
                     `{\"set\": bool}`.",
                )
                .json(Value::Object(settings_shape(s, raw, ""))),
        }
    }

    fn jobs(&self, s: &mut Scrubber) -> Section {
        let rows: Vec<Value> = self
            .input
            .jobs
            .iter()
            .map(|job| {
                let j = &job.summary;
                let diagnostics: Vec<Value> = job
                    .diagnostics
                    .iter()
                    .map(|d| {
                        json!({
                            "severity": d.severity,
                            "line": d.line,
                            "column": d.column,
                            "kind": d.kind.as_str(),
                        })
                    })
                    .collect();
                let name = s.identifier_str(PseudonymKind::Job, "job", &j.name);
                let agent = j
                    .agent
                    .as_ref()
                    .map(|a| s.identifier_str(PseudonymKind::Agent, "agent", a));
                let launcher = j
                    .launcher
                    .as_ref()
                    .map(|l| s.identifier_str(PseudonymKind::Launcher, "launcher", l));
                json!({
                    "job": name,
                    "agent": agent,
                    "schedule": j.schedule,
                    "recurring": j.recurring,
                    "enabled": j.enabled,
                    "launcher": launcher,
                    "status": j.status,
                    "next_at": j.next_at,
                    "last_run": j.last_run.as_ref().map(|r| json!({"at": r.at, "outcome": r.outcome})),
                    "diagnostics": diagnostics,
                })
            })
            .collect();
        let section = Section::new(ExportSection::Jobs);
        if rows.is_empty() {
            section.prose("No jobs were supplied.")
        } else {
            section.jsonl(rows)
        }
    }

    /// `supervisor.json`, scrubbed: pids and process stamps dropped, `stderr_tail` lines that
    /// parse scrubbed as log lines and the rest as free text (#28 D1.6).
    fn crash_section(&self, s: &mut Scrubber) -> Section {
        let section = Section::new(ExportSection::Crash);
        match self.crash {
            CrashState::Missing => section.prose("There is no `supervisor.json`."),
            CrashState::Unreadable => section.prose("`supervisor.json` could not be parsed."),
            CrashState::Read(status) => section.json(crash_value(s, &to_value(status.as_ref()))),
        }
    }

    /// Run index records in the window, oldest first; `run_log` becomes `has_run_log` (#28
    /// D1.7).
    fn runs_section(&self, s: &mut Scrubber) -> Section {
        let records: Vec<Value> = self
            .runs
            .iter()
            .map(|run| {
                let Value::Object(map) = to_value(run) else {
                    unreachable!("RunRecord is an object")
                };
                let map: Map<String, Value> = map
                    .into_iter()
                    .map(|(k, v)| match k.as_str() {
                        "run_log" => ("has_run_log".to_owned(), Value::Bool(!v.is_null())),
                        _ => (k, v),
                    })
                    .collect();
                Value::Object(s.object(&map))
            })
            .collect();
        Section::new(ExportSection::Runs)
            .prose(format!(
                "{} Run index records, oldest first.",
                records.len()
            ))
            .jsonl(records)
    }
}

fn settings_shape(s: &mut Scrubber, raw: &Map<String, Value>, prefix: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, value) in raw {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let shown = if SECRET_PATHS.iter().any(|(p, _)| *p == path) {
            json!({"set": !(value.is_null() || value.as_str() == Some(""))})
        } else {
            match value {
                Value::Object(map) => Value::Object(settings_shape(s, map, &path)),
                Value::Bool(_) | Value::Null => value.clone(),
                Value::Array(items) => json!({"len": items.len()}),
                Value::Number(_) if settings_value_allowed(&path) => value.clone(),
                Value::String(text) if settings_value_allowed(&path) => s.free_text(text).into(),
                _ => json!("<set>"),
            }
        };
        out.insert(s.key(key), shown);
    }
    out
}

fn crash_value(s: &mut Scrubber, value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let shown = match (k.as_str(), v) {
                    ("pid" | "holder_pid" | "stamp", _) => continue,
                    ("stderr_tail", Value::Array(lines)) => Value::Array(
                        lines
                            .iter()
                            .map(|l| match l.as_str() {
                                Some(text) => tail_line(s, text).into(),
                                None => crash_value(s, l),
                            })
                            .collect(),
                    ),
                    // A launchd label or systemd unit, not a job name.
                    ("service_manager", Value::Object(sm)) => {
                        let mut out = Map::new();
                        for (k, v) in sm {
                            let shown = match (k.as_str(), v) {
                                ("name", Value::String(name)) => s.free_text(name).into(),
                                _ => s.field(k, v),
                            };
                            out.insert(k.clone(), shown);
                        }
                        Value::Object(out)
                    }
                    (_, Value::Object(_) | Value::Array(_)) => crash_value(s, v),
                    _ => s.field(k, v),
                };
                out.insert(k.clone(), shown);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| crash_value(s, v)).collect()),
        other => s.free_value(other),
    }
}

fn tail_line(s: &mut Scrubber, text: &str) -> String {
    match logs::parse_line(text, None) {
        line @ LogEntry::Line(_) => s.log_entry(&line).to_string(),
        LogEntry::Raw(raw) => s.free_text(&raw.raw),
    }
}

/// A caller-supplied Summary string, cut to [`SUMMARY_FIELD_MAX_BYTES`].
fn cap(text: &str) -> String {
    if text.len() <= SUMMARY_FIELD_MAX_BYTES {
        return text.to_owned();
    }
    let mut cut = SUMMARY_FIELD_MAX_BYTES - "…".len();
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("wire types serialise")
}

/// A wire enum's string.
fn wire<T: serde::Serialize>(value: &T) -> String {
    match to_value(value) {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests;
