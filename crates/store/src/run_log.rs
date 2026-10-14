//! Run logs: one Markdown file per spawned run, in TS's name and body layout (#14 rule 5, #23 D4).
//!
//! The name comes from [`crate::rollback::run_log_name`]. The body is TS's header and output
//! (`src/runner.ts:1350-1360`), then the Rust-only `## Run` and `## Noise` sections appended at
//! the end, so TS and its catch-up seed read a Rust log as their own. Run logs are the content
//! log: they may hold prompts and output, never a secret the Daemon holds ([`scrub`]).
//!
//! The read side lists the TS-pattern logs in `logs/` and `logs/<agent>/` once per start
//! ([`RunLogListing`]), shared by the Run-index Back-fill and the job-state merge (#44 D3), and
//! reads a log's header ([`RunLogHeader`]).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use claudeclaw_api::runs::{RunOrigin, RunOutcome, RunSession};
use claudeclaw_api::scope::Scope;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;

use crate::atomic::{self, DirMode, FileMode};
use crate::error::StoreError;
use crate::layout::Layout;
use crate::rollback::run_log_name;

/// The Back-fill reads at most this much of a log's head (#44 D2).
pub const HEADER_CAP: u64 = 64 * 1024;

/// `## Noise` keeps at most this many lines (#23 D4).
pub const NOISE_MAX_LINES: usize = 1000;

/// `## Noise` keeps at most this many bytes (#23 D4).
pub const NOISE_MAX_BYTES: usize = 256 * 1024;

/// A Run log could not be named, written or read.
#[derive(Debug, thiserror::Error)]
pub enum RunLogError {
    /// The run name can't be a Run-log name: empty, absolute, `.`/`..`, a backslash, or more
    /// than one `/` (only `<agent>/<label>` may have one). The name itself is not in the message.
    #[error("invalid Run-log name: {0}")]
    InvalidName(&'static str),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The Run log of one run (#14 rule 5): `logs/<name>-<YYYY-MM-DDTHH-MM-SS-mmmZ>.log`, or
/// `logs/<agent>/<label>-….log` for an agent job named `<agent>/<label>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLog {
    name: String,
    start: DateTime<Utc>,
    path: PathBuf,
    rel: String,
}

impl RunLog {
    /// Names the Run log of the run `name` that started at `start` (architecture §8 rule 5). No
    /// IO: TS writes the log once, when the run ends (#23 D4), and so does [`RunLog::write`].
    pub fn create(layout: &Layout, name: &str, start: DateTime<Utc>) -> Result<Self, RunLogError> {
        let (dir, label) = match check_name(name)? {
            (Some(agent), label) => (layout.agent_run_logs_dir(agent), label),
            (None, label) => (layout.run_logs_dir(), label),
        };
        let path = dir.join(run_log_name::file_name(label, start));
        let rel = root_relative(layout, &path);
        Ok(RunLog {
            name: name.to_owned(),
            start,
            path,
            rel,
        })
    }

    /// The run's name, as given to [`RunLog::create`].
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The run's start time, which the file name carries.
    pub fn start(&self) -> DateTime<Utc> {
        self.start
    }

    /// The absolute path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path relative to `.claude/claudeclaw/`, with `/` separators (`logs/<name>-<iso>.log`):
    /// what `RunRecord.run_log` holds (#23 D4).
    pub fn rel(&self) -> &str {
        &self.rel
    }

    /// Writes the log atomically at 0600 (#23 D4), creating `logs/` or `logs/<agent>/` if
    /// needed. Every exact occurrence of a `secrets` value is replaced first ([`scrub`]).
    pub fn write(&self, body: &RunLogBody, secrets: &[KnownSecret<'_>]) -> Result<(), RunLogError> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        atomic::create_dir(dir, DirMode::Default)?;
        let text = body.render(&self.name);
        let text = scrub(&text, secrets);
        atomic::write_atomic(&self.path, text.as_bytes(), FileMode::Private)?;
        Ok(())
    }
}

/// Splits a run name into its agent (if any) and its label, refusing names that would leave
/// `logs/` or nest deeper than `logs/<agent>/`.
fn check_name(name: &str) -> Result<(Option<&str>, &str), RunLogError> {
    let check = |part: &str| match part {
        "" => Err(RunLogError::InvalidName("empty component")),
        "." | ".." => Err(RunLogError::InvalidName("dot component")),
        _ if part.contains(['\\', '\0']) => Err(RunLogError::InvalidName("reserved character")),
        _ => Ok(()),
    };
    match name.split_once('/') {
        None => check(name).map(|()| (None, name)),
        Some((_, label)) if label.contains('/') => {
            Err(RunLogError::InvalidName("more than one '/'"))
        }
        Some((agent, label)) => {
            check(agent)?;
            check(label)?;
            Ok((Some(agent), label))
        }
    }
}

/// `path` relative to `.claude/claudeclaw/`, with `/` separators on every OS.
fn root_relative(layout: &Layout, path: &Path) -> String {
    let rel = path.strip_prefix(layout.root()).unwrap_or(path);
    let parts: Vec<_> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    parts.join("/")
}

/// Resolves a `RunRecord.run_log` value against `.claude/claudeclaw/`, or `None` if it isn't a
/// plain relative path (an absolute path or a `..` is never followed).
pub(crate) fn resolve_rel(layout: &Layout, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    rel.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| layout.root().join(rel))
}

/// A secret the Daemon holds in plaintext, and the label it is replaced with in Run logs
/// (#23 D4's known-secret scrub): `<redacted:<label>>`.
pub struct KnownSecret<'a> {
    /// E.g. `discord-token`.
    pub label: &'static str,
    pub value: &'a SecretString,
}

impl fmt::Debug for KnownSecret<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KnownSecret")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// Replaces every exact occurrence of each secret's value with `<redacted:<label>>` (#23 D4).
/// Used before a Run log is written and when one is served over the API. Empty values are
/// skipped.
pub fn scrub<'t>(text: &'t str, secrets: &[KnownSecret<'_>]) -> Cow<'t, str> {
    let mut out = Cow::Borrowed(text);
    for secret in secrets {
        let value = secret.value.expose_secret();
        if !value.is_empty() && out.contains(value) {
            let marker = format!("<redacted:{}>", secret.label);
            out = Cow::Owned(out.replace(value, &marker));
        }
    }
    out
}

/// What a Run log says: TS's header and body (`src/runner.ts:1350-1360`), plus the Rust-only
/// sections (#23 D4). Content: its `Debug` prints sizes only (architecture §2).
#[derive(Clone, PartialEq, Eq)]
pub struct RunLogBody {
    /// `Date:`, when the log is written (TS: `new Date().toISOString()` at the end of the run).
    pub date: DateTime<Utc>,
    /// `Session:`; `None` is written `unknown`, as TS does before Claude reports an id.
    pub session_id: Option<String>,
    /// `(new)` or `(resumed)`.
    pub session: RunSession,
    /// `Model config: fallback` instead of `primary`.
    pub fallback: bool,
    /// TS's agentic routing lines (`Task type:`, `Routing:`), when routing is on.
    pub routing: Option<(String, String)>,
    /// The prompt as sent.
    pub prompt: String,
    /// `Exit code:`; `None` (no code, e.g. killed by a signal) is written `null`.
    pub exit_code: Option<i32>,
    /// `## Output`: the result text.
    pub output: String,
    /// `## Stderr`, written only when not empty.
    pub stderr: String,
    /// `## Run` (Rust addition); `None` writes no section.
    pub run: Option<RunSection>,
    /// `## Noise` (Rust addition): non-JSON stdout lines, written only when there are some,
    /// capped at [`NOISE_MAX_LINES`] / [`NOISE_MAX_BYTES`].
    pub noise: Vec<String>,
}

impl fmt::Debug for RunLogBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunLogBody")
            .field("date", &self.date)
            .field("session", &self.session)
            .field("exit_code", &self.exit_code)
            .field("prompt_bytes", &self.prompt.len())
            .field("output_bytes", &self.output.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("run", &self.run)
            .field("noise_lines", &self.noise.len())
            .finish_non_exhaustive()
    }
}

/// The `## Run` section a Rust run appends (#23 D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSection {
    pub run_id: String,
    pub scope: Scope,
    pub origin: RunOrigin,
    /// `direct` or the Launcher's file name.
    pub launcher: String,
    pub attempt: u32,
    pub outcome: RunOutcome,
    pub duration_ms: u64,
}

impl RunLogBody {
    /// The log's text for the run `name`: TS's lines joined by `\n`
    /// (`src/runner.ts:1350-1360`), then `## Run` and `## Noise` after a blank line each.
    pub fn render(&self, name: &str) -> String {
        let mut lines: Vec<Cow<'_, str>> = vec![
            format!("# {name}").into(),
            format!(
                "Date: {}",
                self.date.to_rfc3339_opts(SecondsFormat::Millis, true)
            )
            .into(),
            format!(
                "Session: {} ({})",
                self.session_id.as_deref().unwrap_or("unknown"),
                wire(&self.session)
            )
            .into(),
            format!(
                "Model config: {}",
                if self.fallback { "fallback" } else { "primary" }
            )
            .into(),
        ];
        if let Some((task_type, routing)) = &self.routing {
            lines.push(format!("Task type: {task_type}").into());
            lines.push(format!("Routing: {routing}").into());
        }
        lines.push(format!("Prompt: {}", self.prompt).into());
        lines.push(match self.exit_code {
            Some(code) => format!("Exit code: {code}").into(),
            None => "Exit code: null".into(),
        });
        lines.extend(["".into(), "## Output".into(), self.output.as_str().into()]);
        if !self.stderr.is_empty() {
            lines.extend(["## Stderr".into(), self.stderr.as_str().into()]);
        }
        if let Some(run) = &self.run {
            lines.extend([
                "".into(),
                "## Run".into(),
                format!("Run ID: {}", run.run_id).into(),
                format!("Scope: {}", run.scope).into(),
                format!("Origin: {}", wire(&run.origin)).into(),
                format!("Launcher: {}", run.launcher).into(),
                format!("Attempt: {}", run.attempt).into(),
                format!("Outcome: {}", wire(&run.outcome)).into(),
                format!("Duration: {} ms", run.duration_ms).into(),
            ]);
        }
        if !self.noise.is_empty() {
            lines.extend(["".into(), "## Noise".into()]);
            let mut bytes = 0;
            let mut kept = 0;
            for line in &self.noise {
                bytes += line.len() + 1;
                if kept == NOISE_MAX_LINES || bytes > NOISE_MAX_BYTES {
                    break;
                }
                lines.push(line.as_str().into());
                kept += 1;
            }
            if kept < self.noise.len() {
                lines.push(format!("[… {} more]", self.noise.len() - kept).into());
            }
        }
        lines.join("\n")
    }
}

/// A wire enum's string, e.g. `resumed`.
fn wire(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "unknown".to_owned(),
    }
}

/// How a TS exit code reads as a Run outcome (#44 D2): `0` → `ok`, `124` (TS's own timeout
/// exit) → `timeout`, anything else → `failed`, and no `Exit code:` line → `unknown`.
pub fn exit_outcome(exit_code: Option<i32>) -> RunOutcome {
    match exit_code {
        Some(0) => RunOutcome::Ok,
        Some(124) => RunOutcome::Timeout,
        Some(_) => RunOutcome::Failed,
        None => RunOutcome::Unknown,
    }
}

/// One TS-pattern Run log found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLogFile {
    /// The run name: the label in the file name, or `<agent>/<label>` in `logs/<agent>/`.
    pub name: String,
    /// The subdir of `logs/` it is in, for agent jobs.
    pub agent: Option<String>,
    /// The start time in the file name.
    pub started_at: DateTime<Utc>,
    /// Relative to `.claude/claudeclaw/`, with `/` separators.
    pub rel: String,
    pub path: PathBuf,
}

/// Every TS-pattern Run log in `logs/` and `logs/<agent>/`, oldest first (#44 D3: listed once
/// per start and shared by the Back-fill and the job-state merge).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunLogListing {
    pub files: Vec<RunLogFile>,
}

impl RunLogListing {
    /// Lists `logs/` and each dir directly in it. Files that don't match TS's pattern
    /// (`daemon.log`, …) and names that aren't UTF-8 are left out; a missing `logs/` is empty.
    pub fn scan(layout: &Layout) -> Result<Self, StoreError> {
        let mut files = Vec::new();
        let top = layout.run_logs_dir();
        for entry in read_dir(&top)? {
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            let is_dir = entry
                .file_type()
                .map_err(StoreError::io("stat", &entry.path()))?
                .is_dir();
            if !is_dir {
                if let Some(file) = Self::file(layout, &file_name, None, entry.path()) {
                    files.push(file);
                }
                continue;
            }
            let dir = layout.agent_run_logs_dir(&file_name);
            for inner in read_dir(&dir)? {
                let Ok(inner_name) = inner.file_name().into_string() else {
                    continue;
                };
                let is_file = inner
                    .file_type()
                    .map_err(StoreError::io("stat", &inner.path()))?
                    .is_file();
                if is_file
                    && let Some(file) =
                        Self::file(layout, &inner_name, Some(&file_name), inner.path())
                {
                    files.push(file);
                }
            }
        }
        files.sort_by(|a, b| (a.started_at, &a.rel).cmp(&(b.started_at, &b.rel)));
        Ok(RunLogListing { files })
    }

    fn file(
        layout: &Layout,
        file_name: &str,
        agent: Option<&str>,
        path: PathBuf,
    ) -> Option<RunLogFile> {
        let (label, started_at) = run_log_name::parse(file_name)?;
        let name = match agent {
            Some(agent) => format!("{agent}/{label}"),
            None => label.to_owned(),
        };
        Some(RunLogFile {
            name,
            agent: agent.map(str::to_owned),
            started_at,
            rel: root_relative(layout, &path),
            path,
        })
    }

    /// The newest log per run name (by the time in its name).
    pub fn newest_per_name(&self) -> BTreeMap<&str, &RunLogFile> {
        let mut newest = BTreeMap::new();
        for file in &self.files {
            newest
                .entry(file.name.as_str())
                .and_modify(|cur: &mut &RunLogFile| {
                    if file.started_at > cur.started_at {
                        *cur = file;
                    }
                })
                .or_insert(file);
        }
        newest
    }
}

/// The entries of `dir`, or none if it doesn't exist.
fn read_dir(dir: &Path) -> Result<Vec<fs::DirEntry>, StoreError> {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .collect::<Result<_, _>>()
            .map_err(StoreError::io("list", dir)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(StoreError::io("list", dir)(e)),
    }
}

/// What the Back-fill and the job-state seed read from a Run log's header (#17 §4, #44 D2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunLogHeader {
    /// From `Session: <id> (new|resumed)`; `None` for TS's `unknown`.
    pub session_id: Option<String>,
    pub session: Option<RunSession>,
    /// From the `Exit code: <n>` line that is directly followed by a blank line and
    /// `## Output`, so prompt text can't fool it (#17 §4); `None` if there is none.
    pub exit_code: Option<i32>,
}

impl RunLogHeader {
    /// Reads the header of the log at `path`, looking at most `cap` bytes in (`None`: the whole
    /// file). The Back-fill passes [`HEADER_CAP`] (#44 D2); the job-state seed reads the whole
    /// file, because a long prompt can push `Exit code:` past any fixed size (#17 §4).
    pub fn read(path: &Path, cap: Option<u64>) -> Result<Self, StoreError> {
        let file = fs::File::open(path).map_err(StoreError::io("open", path))?;
        let mut bytes = Vec::new();
        file.take(cap.unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(StoreError::io("read", path))?;
        Ok(Self::parse(&String::from_utf8_lossy(&bytes)))
    }

    /// Parses a log's text (or its head).
    pub fn parse(text: &str) -> Self {
        let mut header = RunLogHeader::default();
        for line in text.lines() {
            if line.starts_with("Prompt: ") || line == "## Output" {
                break;
            }
            if let Some(rest) = line.strip_prefix("Session: ") {
                if let Some((id, kind)) = rest.rsplit_once(" (") {
                    header.session = match kind {
                        "new)" => Some(RunSession::New),
                        "resumed)" => Some(RunSession::Resumed),
                        _ => None,
                    };
                    header.session_id = (id != "unknown" && !id.is_empty()).then(|| id.to_owned());
                }
                break;
            }
        }
        header.exit_code = exit_code(text);
        header
    }
}

/// The first `Exit code: <n>` line directly followed by a blank line and `## Output` (#17 §4).
fn exit_code(text: &str) -> Option<i32> {
    const KEY: &str = "\nExit code: ";
    for (at, _) in text.match_indices(KEY) {
        let rest = &text[at + KEY.len()..];
        let Some((value, after)) = rest.split_once('\n') else {
            continue;
        };
        let value = value.strip_suffix('\r').unwrap_or(value);
        let after = after.strip_prefix('\r').unwrap_or(after);
        let Some(after) = after.strip_prefix("\n## Output") else {
            continue;
        };
        if !(after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n")) {
            continue;
        }
        if let Ok(code) = value.trim().parse() {
            return Some(code);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        fs::create_dir_all(layout.root()).unwrap();
        (dir, layout)
    }

    fn ts_body() -> RunLogBody {
        RunLogBody {
            date: at("2026-10-13T08:31:02Z"),
            session_id: Some("5e55-1d".into()),
            session: RunSession::Resumed,
            fallback: false,
            routing: None,
            prompt: "[clock]\nsay hi".into(),
            exit_code: Some(0),
            output: "hi".into(),
            stderr: String::new(),
            run: None,
            noise: Vec::new(),
        }
    }

    #[test]
    fn paths_follow_rule_5() {
        let layout = Layout::new("/p");
        let start = at("2026-10-13T08:30:27.512Z");
        let flat = RunLog::create(&layout, "daily-report", start).unwrap();
        assert_eq!(
            flat.path(),
            Path::new("/p/.claude/claudeclaw/logs/daily-report-2026-10-13T08-30-27-512Z.log")
        );
        assert_eq!(flat.rel(), "logs/daily-report-2026-10-13T08-30-27-512Z.log");
        // An agent job is `<agent>/<label>` (`src/jobs.ts:143`), so TS's
        // `join(LOGS_DIR, `${name}-…`)` puts it in `logs/<agent>/` (`src/runner.ts:1066`).
        let agent = RunLog::create(&layout, "ops/digest", start).unwrap();
        assert_eq!(
            agent.path(),
            Path::new("/p/.claude/claudeclaw/logs/ops/digest-2026-10-13T08-30-27-512Z.log")
        );
        assert_eq!(agent.rel(), "logs/ops/digest-2026-10-13T08-30-27-512Z.log");
        assert_eq!(agent.name(), "ops/digest");
        assert_eq!(agent.start(), start);

        for bad in ["", "..", "a/b/c", "/abs", "a/", "../x", "a\\b", "a/.."] {
            assert!(
                matches!(
                    RunLog::create(&layout, bad, start),
                    Err(RunLogError::InvalidName(_))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_body_follows_ts_byte_for_byte() {
        // Expected text from `src/runner.ts:1350-1360`: the lines joined by "\n", a blank line
        // before `## Output`, `## Stderr` only when stderr isn't empty, no trailing newline.
        let mut body = ts_body();
        assert_eq!(
            body.render("daily-report"),
            "# daily-report\n\
             Date: 2026-10-13T08:31:02.000Z\n\
             Session: 5e55-1d (resumed)\n\
             Model config: primary\n\
             Prompt: [clock]\nsay hi\n\
             Exit code: 0\n\
             \n\
             ## Output\n\
             hi"
        );

        body.session_id = None;
        body.session = RunSession::New;
        body.fallback = true;
        body.routing = Some(("code".into(), "heavy task".into()));
        body.exit_code = Some(124);
        body.stderr = "boom".into();
        assert_eq!(
            body.render("x"),
            "# x\n\
             Date: 2026-10-13T08:31:02.000Z\n\
             Session: unknown (new)\n\
             Model config: fallback\n\
             Task type: code\n\
             Routing: heavy task\n\
             Prompt: [clock]\nsay hi\n\
             Exit code: 124\n\
             \n\
             ## Output\n\
             hi\n\
             ## Stderr\n\
             boom"
        );
    }

    #[test]
    fn rust_sections_are_appended_after_the_ts_body() {
        let mut body = ts_body();
        let ts_text = body.render("daily-report");
        body.run = Some(RunSection {
            run_id: "0199e0c2".into(),
            scope: "job:daily-report".parse().unwrap(),
            origin: RunOrigin::Scheduler,
            launcher: "direct".into(),
            attempt: 1,
            outcome: RunOutcome::Ok,
            duration_ms: 34488,
        });
        body.noise = vec!["warn: a".into(), "warn: b".into()];
        let text = body.render("daily-report");
        assert_eq!(
            text.strip_prefix(&ts_text).unwrap(),
            "\n\n## Run\n\
             Run ID: 0199e0c2\n\
             Scope: job:daily-report\n\
             Origin: scheduler\n\
             Launcher: direct\n\
             Attempt: 1\n\
             Outcome: ok\n\
             Duration: 34488 ms\n\
             \n\
             ## Noise\n\
             warn: a\n\
             warn: b"
        );
        // The header still reads the same.
        assert_eq!(RunLogHeader::parse(&text).exit_code, Some(0));
    }

    #[test]
    fn noise_is_capped() {
        let mut body = ts_body();
        body.noise = (0..NOISE_MAX_LINES + 5).map(|i| format!("n{i}")).collect();
        let text = body.render("x");
        assert!(text.ends_with(&format!("n{}\n[… 5 more]", NOISE_MAX_LINES - 1)));

        body.noise = vec!["x".repeat(NOISE_MAX_BYTES / 2); 3];
        let text = body.render("x");
        assert!(
            text.ends_with("\n[… 2 more]"),
            "{}",
            &text[text.len() - 20..]
        );
    }

    #[test]
    fn write_scrubs_known_secrets_and_is_private() {
        let (_dir, layout) = layout();
        let token = SecretString::from("tok-123");
        let empty = SecretString::from("");
        let secrets = [
            KnownSecret {
                label: "discord-token",
                value: &token,
            },
            KnownSecret {
                label: "nothing",
                value: &empty,
            },
        ];
        let mut body = ts_body();
        body.output = "the token is tok-123, again tok-123".into();
        let log = RunLog::create(&layout, "ops/digest", at("2026-10-13T08:30:27.512Z")).unwrap();
        log.write(&body, &secrets).unwrap();
        let text = fs::read_to_string(log.path()).unwrap();
        assert!(
            text.ends_with("the token is <redacted:discord-token>, again <redacted:discord-token>")
        );
        assert!(!text.contains("tok-123"));
        assert!(format!("{:?}", secrets[0]).contains("discord-token"));
        assert!(!format!("{:?}", secrets[0]).contains("tok-123"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(log.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn the_body_debug_prints_sizes_only() {
        let debug = format!("{:?}", ts_body());
        assert!(debug.contains("prompt_bytes: 14"), "{debug}");
        assert!(!debug.contains("say hi"));
    }

    #[test]
    fn exit_codes_need_the_output_heading_after_them() {
        // #17 §4: only the `Exit code:` line followed by a blank line and `## Output` counts.
        let table: &[(&str, Option<i32>)] = &[
            ("# x\nPrompt: p\nExit code: 0\n\n## Output\nok", Some(0)),
            ("# x\nPrompt: p\nExit code: 124\n\n## Output\n", Some(124)),
            ("# x\nPrompt: p\nExit code: 1\n\n## Output", Some(1)),
            ("# x\nPrompt: p\nExit code: -1\n\n## Output\n", Some(-1)),
            (
                "# x\nPrompt: p\r\nExit code: 2\r\n\r\n## Output\r\n",
                Some(2),
            ),
            // A prompt line that looks like the key, without the heading after it.
            (
                "# x\nPrompt: a\nExit code: 9\nmore\nExit code: 0\n\n## Output\n",
                Some(0),
            ),
            ("# x\nPrompt: p\nExit code: null\n\n## Output\n", None),
            ("# x\nPrompt: p\nExit code: 0\n## Output\n", None),
            ("# x\nPrompt: p\nExit code: 0\n\n## Outputs\n", None),
            ("# x\nPrompt: p\n\n## Output\n", None),
            ("", None),
        ];
        for (text, want) in table {
            assert_eq!(RunLogHeader::parse(text).exit_code, *want, "{text:?}");
        }
    }

    #[test]
    fn the_session_line_is_read() {
        let header = RunLogHeader::parse("# x\nDate: d\nSession: 5e55-1d (new)\nPrompt: p\n");
        assert_eq!(header.session_id.as_deref(), Some("5e55-1d"));
        assert_eq!(header.session, Some(RunSession::New));
        let header = RunLogHeader::parse("# x\nSession: unknown (resumed)\nPrompt: p\n");
        assert_eq!(header.session_id, None);
        assert_eq!(header.session, Some(RunSession::Resumed));
        // Only before `Prompt:`.
        let header = RunLogHeader::parse("# x\nPrompt: p\nSession: a (new)\n");
        assert_eq!(header, RunLogHeader::default());
    }

    #[test]
    fn the_header_read_can_be_capped() {
        let (_dir, layout) = layout();
        let log = RunLog::create(&layout, "x", at("2026-10-13T08:30:27.512Z")).unwrap();
        let mut body = ts_body();
        body.prompt = "p".repeat(HEADER_CAP as usize);
        log.write(&body, &[]).unwrap();
        let capped = RunLogHeader::read(log.path(), Some(HEADER_CAP)).unwrap();
        assert_eq!(capped.exit_code, None);
        assert_eq!(capped.session_id.as_deref(), Some("5e55-1d"));
        let whole = RunLogHeader::read(log.path(), None).unwrap();
        assert_eq!(whole.exit_code, Some(0));
    }

    #[test]
    fn exit_codes_map_to_outcomes() {
        // #44 D2: TS exits 124 on its own timeout (`src/watchdog.ts:19`).
        assert_eq!(exit_outcome(Some(0)), RunOutcome::Ok);
        assert_eq!(exit_outcome(Some(124)), RunOutcome::Timeout);
        assert_eq!(exit_outcome(Some(1)), RunOutcome::Failed);
        assert_eq!(exit_outcome(Some(143)), RunOutcome::Failed);
        assert_eq!(exit_outcome(None), RunOutcome::Unknown);
    }

    #[test]
    fn the_listing_covers_logs_and_agent_dirs() {
        let (_dir, layout) = layout();
        let logs = layout.run_logs_dir();
        fs::create_dir_all(logs.join("ops")).unwrap();
        fs::create_dir_all(logs.join("empty")).unwrap();
        for name in [
            "daily-2026-10-01T00-00-00-000Z.log",
            "daily-2026-10-02T00-00-00-000Z.log",
            "daemon.log",
            "daemon.prev.log",
            "ops/digest-2026-09-30T00-00-00-000Z.log",
            "ops/notes.txt",
        ] {
            fs::write(logs.join(name), "x").unwrap();
        }
        let listing = RunLogListing::scan(&layout).unwrap();
        let got: Vec<_> = listing
            .files
            .iter()
            .map(|f| (f.name.as_str(), f.agent.as_deref(), f.rel.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "ops/digest",
                    Some("ops"),
                    "logs/ops/digest-2026-09-30T00-00-00-000Z.log"
                ),
                ("daily", None, "logs/daily-2026-10-01T00-00-00-000Z.log"),
                ("daily", None, "logs/daily-2026-10-02T00-00-00-000Z.log"),
            ]
        );
        let newest = listing.newest_per_name();
        assert_eq!(newest["daily"].started_at, at("2026-10-02T00:00:00Z"));
        assert_eq!(newest["ops/digest"].agent.as_deref(), Some("ops"));
        assert_eq!(newest.len(), 2);
    }

    #[test]
    fn a_missing_logs_dir_lists_nothing() {
        let (_dir, layout) = layout();
        assert!(RunLogListing::scan(&layout).unwrap().files.is_empty());
    }

    #[test]
    fn records_resolve_only_plain_relative_paths() {
        let layout = Layout::new("/p");
        assert_eq!(
            resolve_rel(&layout, "logs/a.log"),
            Some(PathBuf::from("/p/.claude/claudeclaw/logs/a.log"))
        );
        assert_eq!(resolve_rel(&layout, "../a.log"), None);
        assert_eq!(resolve_rel(&layout, "/etc/a.log"), None);
    }
}
