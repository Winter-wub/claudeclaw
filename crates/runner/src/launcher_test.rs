//! The pure Test launcher check evaluation (architecture §4 `claudeclaw-runner` `launcher`, the
//! **Test launcher** procedure; #8, #16, #22, #25, #37, #39, #42, #43).
//!
//! [`evaluate`] turns the facts of one small run — gathered by `core` from the existing
//! [`crate::launcher`], [`crate::env`], [`crate::stream`] and [`crate::spawn`] types — into a
//! [`Report`]. It is pure and synchronous: no tokio, no filesystem, no spawning, so `core` runs
//! the run itself and feeds the facts in. Every [`Check::detail`] is short and human-readable and
//! never carries the Launcher's path, arguments, prompt or stderr (#8).

use std::fmt;

use crate::env::{RunLocaleSource, RunPathSource};
use crate::launcher::LauncherError;
use crate::spawn::Settle;
use crate::stream::{RunOutcome, TranscriptHint, Verdict};
use crate::version::{MIN_CLAUDE_CODE, VersionCheck};

/// The prefix of the marker the Test launcher's System-prompt file carries. It never appears in
/// the stdin prompt, so a reply that echoes `SYSTEM_PROMPT_MARKER_PREFIX + <marker>` proves the
/// model read the appended System-prompt file (#16, #22).
pub const SYSTEM_PROMPT_MARKER_PREFIX: &str = "CLAUDECLAW-VERIFICATION-MARKER:";

/// The stdin prompt and the System-prompt file text for one Test launcher run (#16, #22, #25).
///
/// `marker` is a freshly generated value the caller keeps; it exists only in the returned system
/// prompt text. The caller passes the reply through [`TestFacts::marker_echoed`].
pub fn test_prompt(marker: &str) -> (String, String) {
    let stdin =
        "Reply with exactly the verification marker given in your system prompt.".to_owned();
    let system = format!(
        "The verification marker is {SYSTEM_PROMPT_MARKER_PREFIX}{marker}. \
         Reply with exactly that marker when asked."
    );
    (stdin, system)
}

/// One Test launcher check's identity, in the order [`evaluate`] runs them (management-api
/// §2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckId {
    Runnable,
    PrintStreamJson,
    CleanStdout,
    SystemPrompt,
    ResultEvent,
    PromptStdin,
    ClaudeCodeVersion,
    /// Advisory (#39).
    Transcript,
    /// A warning; treated as advisory (#37).
    Stragglers,
}

impl CheckId {
    /// Every check, in order.
    pub const ALL: [CheckId; 9] = [
        CheckId::Runnable,
        CheckId::PrintStreamJson,
        CheckId::CleanStdout,
        CheckId::SystemPrompt,
        CheckId::ResultEvent,
        CheckId::PromptStdin,
        CheckId::ClaudeCodeVersion,
        CheckId::Transcript,
        CheckId::Stragglers,
    ];

    /// The wire id (management-api §2.5).
    pub fn as_str(self) -> &'static str {
        match self {
            CheckId::Runnable => "runnable",
            CheckId::PrintStreamJson => "print_stream_json",
            CheckId::CleanStdout => "clean_stdout",
            CheckId::SystemPrompt => "system_prompt",
            CheckId::ResultEvent => "result_event",
            CheckId::PromptStdin => "prompt_stdin",
            CheckId::ClaudeCodeVersion => "claude_code_version",
            CheckId::Transcript => "transcript",
            CheckId::Stragglers => "stragglers",
        }
    }

    /// Whether this check never makes the [`Report::ok`] false (#39, #37).
    fn is_advisory(self) -> bool {
        matches!(self, CheckId::Transcript | CheckId::Stragglers)
    }
}

/// One Test launcher check (management-api §2.5). Its `detail` never shows the Launcher's
/// arguments (#8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub id: CheckId,
    pub ok: bool,
    /// An advisory check never makes the result's `ok` false (#39 D6).
    pub advisory: bool,
    pub detail: String,
}

/// The Test launcher's result: `ok` is false iff some non-advisory check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub ok: bool,
    pub checks: Vec<Check>,
}

/// What a successful [`crate::launcher::resolve`] leaves for the `runnable` detail (#43). `core`
/// builds it from the Run PATH report's source and the Run locale report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEnv {
    /// `run_path.source` (#26).
    pub run_path: RunPathSource,
    /// The Run locale's source (#43).
    pub run_locale: RunLocaleSource,
    /// The ctype name runs see; empty when there is none to name.
    pub locale: String,
}

/// Everything [`evaluate`]'s checks need, gathered by the caller from the existing runner types
/// (architecture §4 `launcher` Test launcher). Nothing here is read from the filesystem: the
/// transcript-exists check takes a bool the caller supplies (#39).
pub struct TestFacts {
    /// The [`crate::launcher::resolve`] result: `Ok` with the env for the `runnable` detail
    /// (#43), or the refusal (#8, #26).
    pub resolve: Result<ResolvedEnv, LauncherError>,
    /// The stdout lines the run read.
    pub stdout_lines: u64,
    /// How many of them were stream-json JSON objects.
    pub json_lines: u64,
    /// How many were non-JSON noise (#5).
    pub noise_lines: u64,
    /// An `init` event was seen.
    pub init_seen: bool,
    /// The `result` event's `is_error`; `None` when no `result` was read (#8).
    pub result_is_error: Option<bool>,
    /// The judged run, when it happened (#39, #42).
    pub outcome: Option<RunOutcome>,
    /// The run's verdict ([`crate::stream::judge`]).
    pub verdict: Option<Verdict>,
    /// The reply echoed the System-prompt marker ([`test_prompt`]).
    pub marker_echoed: bool,
    /// The reply echoed the stdin nonce.
    pub nonce_echoed: bool,
    /// The Settle, when the run happened (#37).
    pub settle: Option<Settle>,
    /// `init.memory_paths.auto`'s hint, when it had one (#39).
    pub transcript_hint: Option<TranscriptHint>,
    /// Whether the transcript file exists; read by the caller, never here (#39).
    pub transcript_exists: bool,
}

impl fmt::Debug for TestFacts {
    /// Content-free (§2, #8): the `LauncherError`'s path and the reply-bearing outcome's text
    /// never print, only counts, flags, codes and the content-free nested Debugs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestFacts")
            .field("resolved", &self.resolve.is_ok())
            .field(
                "resolve_code",
                &self.resolve.as_ref().err().map(LauncherError::code),
            )
            .field("stdout_lines", &self.stdout_lines)
            .field("json_lines", &self.json_lines)
            .field("noise_lines", &self.noise_lines)
            .field("init_seen", &self.init_seen)
            .field("result_is_error", &self.result_is_error)
            .field("outcome", &self.outcome)
            .field("verdict", &self.verdict)
            .field("marker_echoed", &self.marker_echoed)
            .field("nonce_echoed", &self.nonce_echoed)
            .field("settle", &self.settle)
            .field("transcript_hint", &self.transcript_hint.is_some())
            .field("transcript_exists", &self.transcript_exists)
            .finish()
    }
}

/// Evaluates every Test launcher check (architecture §4 `launcher` Test launcher; #8, #16, #22,
/// #25, #37, #39, #42, #43).
///
/// When the Launcher is not runnable, every later check is `ok=false` with the detail `not run`.
pub fn evaluate(facts: &TestFacts) -> Report {
    let mut checks = Vec::with_capacity(CheckId::ALL.len());
    let runnable = facts.resolve.is_ok();
    checks.push(Check {
        id: CheckId::Runnable,
        ok: runnable,
        advisory: false,
        detail: runnable_detail(&facts.resolve),
    });

    if !runnable {
        for id in CheckId::ALL.iter().copied().skip(1) {
            checks.push(Check {
                id,
                ok: false,
                advisory: id.is_advisory(),
                detail: "not run".to_owned(),
            });
        }
    } else {
        checks.push(print_stream_json(facts));
        checks.push(clean_stdout(facts));
        checks.push(system_prompt(facts));
        checks.push(result_event(facts));
        checks.push(prompt_stdin(facts));
        checks.push(claude_code_version(facts));
        checks.push(transcript(facts));
        checks.push(stragglers(facts));
    }

    let ok = checks.iter().all(|c| c.ok || c.advisory);
    Report { ok, checks }
}

fn runnable_detail(resolve: &Result<ResolvedEnv, LauncherError>) -> String {
    match resolve {
        // The Launcher's path and arguments never appear here (#8); the Run PATH source and the
        // Run locale do (#43).
        Ok(env) => {
            let locale = if env.locale.is_empty() {
                env.run_locale.as_str().to_owned()
            } else {
                env.locale.clone()
            };
            format!(
                "runnable (Run PATH {}; locale {locale})",
                env.run_path.as_str()
            )
        }
        Err(err) => format!("not runnable ({})", err.code()),
    }
}

fn print_stream_json(facts: &TestFacts) -> Check {
    let ok = facts.json_lines > 0 && facts.init_seen;
    let detail = if facts.json_lines == 0 {
        "no stream-json line on stdout".to_owned()
    } else if !facts.init_seen {
        "stream-json without an init event".to_owned()
    } else {
        format!(
            "{} of {} stdout line(s) were stream-json",
            facts.json_lines, facts.stdout_lines
        )
    };
    Check {
        id: CheckId::PrintStreamJson,
        ok,
        advisory: false,
        detail,
    }
}

fn clean_stdout(facts: &TestFacts) -> Check {
    Check {
        id: CheckId::CleanStdout,
        ok: facts.noise_lines == 0,
        advisory: false,
        detail: format!("{} non-JSON line(s) on stdout", facts.noise_lines),
    }
}

fn system_prompt(facts: &TestFacts) -> Check {
    let ok = facts.marker_echoed;
    let detail = if ok {
        "the reply echoed the System-prompt marker"
    } else {
        "the reply did not echo the System-prompt marker"
    };
    Check {
        id: CheckId::SystemPrompt,
        ok,
        advisory: false,
        detail: detail.to_owned(),
    }
}

fn result_event(facts: &TestFacts) -> Check {
    let ok = facts.result_is_error == Some(false);
    let detail = if ok {
        "the run reported a result".to_owned()
    } else {
        verdict_detail(facts.verdict.as_ref())
    };
    Check {
        id: CheckId::ResultEvent,
        ok,
        advisory: false,
        detail,
    }
}

fn prompt_stdin(facts: &TestFacts) -> Check {
    let (ok, detail) = match &facts.settle {
        None => (false, "the run did not settle".to_owned()),
        Some(settle) if !settle.stdin_written => (
            false,
            "the prompt did not reach the child's stdin".to_owned(),
        ),
        Some(_) if !facts.nonce_echoed => {
            (false, "the reply did not echo the stdin marker".to_owned())
        }
        Some(_) => (
            true,
            "the prompt reached stdin and the reply echoed it".to_owned(),
        ),
    };
    Check {
        id: CheckId::PromptStdin,
        ok,
        advisory: false,
        detail,
    }
}

fn claude_code_version(facts: &TestFacts) -> Check {
    let version = facts
        .outcome
        .as_ref()
        .and_then(|outcome| outcome.claude_code_version.as_ref());
    let (ok, detail) = match version {
        None => (false, "no version reported".to_owned()),
        Some(v) => {
            let label = v
                .parsed
                .map_or_else(|| v.raw.clone(), |parsed| parsed.to_string());
            match v.check() {
                VersionCheck::Supported | VersionCheck::Unknown => {
                    (true, format!("Claude Code {label}"))
                }
                VersionCheck::Outdated => (
                    false,
                    format!("Claude Code {label} is below the minimum {MIN_CLAUDE_CODE}"),
                ),
            }
        }
    };
    Check {
        id: CheckId::ClaudeCodeVersion,
        ok,
        advisory: false,
        detail,
    }
}

fn transcript(facts: &TestFacts) -> Check {
    let (ok, detail) = match (&facts.transcript_hint, facts.transcript_exists) {
        (Some(_), true) => (true, "the transcript file exists".to_owned()),
        (Some(_), false) => (false, "the transcript file was not found".to_owned()),
        (None, _) => (false, "no transcript hint".to_owned()),
    };
    Check {
        id: CheckId::Transcript,
        ok,
        advisory: true,
        detail,
    }
}

fn stragglers(facts: &TestFacts) -> Check {
    let (ok, detail) = match &facts.settle {
        None => (false, "the run did not settle".to_owned()),
        Some(settle) if settle.stragglers == 0 => (true, "no stragglers".to_owned()),
        Some(settle) => (
            false,
            format!(
                "{} straggler(s), settle {}",
                settle.stragglers,
                settle.outcome.as_str()
            ),
        ),
    };
    Check {
        id: CheckId::Stragglers,
        ok,
        advisory: true,
        detail,
    }
}

/// A short, content-free label for why a run did not return a clean result (#8, #42, #47).
fn verdict_detail(verdict: Option<&Verdict>) -> String {
    match verdict {
        Some(Verdict::Ok) => "the run reported a result".to_owned(),
        Some(Verdict::Failed(failure)) => format!("the run failed: {}", failure.code()),
        Some(Verdict::Timeout) => "the run timed out without a result".to_owned(),
        Some(Verdict::Interrupted) => "the run was interrupted without a result".to_owned(),
        Some(Verdict::RateLimited { .. }) => "the run hit a subscription limit".to_owned(),
        Some(Verdict::Refused(refusal)) => format!("the run was refused: {}", refusal.code()),
        None => "no result event".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::ClaudeArgs;
    use crate::spawn::{OutputStats, SettleOutcome};
    use crate::stream::{ExitReport, StreamState, judge};

    fn hint() -> TranscriptHint {
        TranscriptHint::from_memory_path(
            &std::env::temp_dir()
                .join("cfg")
                .join("projects")
                .join("p")
                .join("memory")
                .to_string_lossy(),
        )
        .unwrap()
    }

    /// A judged run built through the real `stream` API, so `outcome` and `verdict` stay
    /// consistent.
    fn run_outcome(version: Option<&str>, result: Option<bool>) -> RunOutcome {
        let mut state = StreamState::new();
        let init = match version {
            Some(v) => {
                format!(r#"{{"type":"system","subtype":"init","claude_code_version":"{v}"}}"#)
            }
            None => r#"{"type":"system","subtype":"init"}"#.to_owned(),
        };
        state.push_line(&init);
        match result {
            Some(false) => {
                state.push_line(
                    r#"{"type":"result","subtype":"success","is_error":false,"result":"reply"}"#,
                );
            }
            Some(true) => {
                state.push_line(
                    r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["boom"]}"#,
                );
            }
            None => {}
        }
        judge(&state, &ExitReport::default(), &ClaudeArgs::new())
    }

    fn settle(stdin_written: bool) -> Settle {
        Settle {
            stragglers: 0,
            outcome: SettleOutcome::Clean,
            pipe_held: false,
            settle_ms: 1,
            stdout: OutputStats::default(),
            stderr: OutputStats::default(),
            stdin_written,
            stopped: false,
        }
    }

    /// Every check passes.
    fn facts() -> TestFacts {
        let outcome = run_outcome(Some("2.1.296"), Some(false));
        TestFacts {
            resolve: Ok(ResolvedEnv {
                run_path: RunPathSource::LoginShell,
                run_locale: RunLocaleSource::Default,
                locale: "C.UTF-8".to_owned(),
            }),
            stdout_lines: 2,
            json_lines: 2,
            noise_lines: 0,
            init_seen: true,
            result_is_error: outcome.is_error,
            verdict: Some(outcome.verdict.clone()),
            outcome: Some(outcome),
            marker_echoed: true,
            nonce_echoed: true,
            settle: Some(settle(true)),
            transcript_hint: Some(hint()),
            transcript_exists: true,
        }
    }

    fn check(report: &Report, id: CheckId) -> &Check {
        report.checks.iter().find(|c| c.id == id).unwrap()
    }

    #[test]
    fn all_pass() {
        let report = evaluate(&facts());
        assert!(report.ok);
        let ids: Vec<CheckId> = report.checks.iter().map(|c| c.id).collect();
        assert_eq!(ids, CheckId::ALL);
        assert!(report.checks.iter().all(|c| c.ok), "{:?}", report.checks);
        let advisory: Vec<bool> = report.checks.iter().map(|c| c.advisory).collect();
        assert_eq!(
            advisory,
            [false, false, false, false, false, false, false, true, true]
        );
        assert!(
            check(&report, CheckId::Runnable)
                .detail
                .contains("login_shell")
        );
    }

    #[test]
    fn not_runnable_cascades_not_run() {
        let mut facts = facts();
        facts.resolve = Err(LauncherError::Missing {
            path: std::env::temp_dir().join("gone"),
        });
        let report = evaluate(&facts);
        assert!(!report.ok);
        let runnable = check(&report, CheckId::Runnable);
        assert!(!runnable.ok);
        assert!(runnable.detail.contains("missing"), "{}", runnable.detail);
        for c in &report.checks[1..] {
            assert!(!c.ok, "{c:?}");
            assert_eq!(c.detail, "not run", "{c:?}");
        }
        // The advisory checks are marked advisory even when not run.
        assert!(check(&report, CheckId::Transcript).advisory);
        assert!(check(&report, CheckId::Stragglers).advisory);
    }

    #[test]
    fn noisy_stdout_fails_only_clean_stdout() {
        let mut facts = facts();
        facts.stdout_lines = 4;
        facts.json_lines = 2;
        facts.noise_lines = 2;
        let report = evaluate(&facts);
        assert!(!report.ok);
        let failed: Vec<CheckId> = report
            .checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| c.id)
            .collect();
        assert_eq!(failed, [CheckId::CleanStdout]);
        let clean = check(&report, CheckId::CleanStdout);
        assert!(clean.detail.contains('2'), "{}", clean.detail);
    }

    #[test]
    fn outdated_version_fails_its_check() {
        let mut facts = facts();
        let outcome = run_outcome(Some("2.1.100"), Some(false));
        facts.result_is_error = outcome.is_error;
        facts.verdict = Some(outcome.verdict.clone());
        facts.outcome = Some(outcome);
        let report = evaluate(&facts);
        assert!(!report.ok);
        let version = check(&report, CheckId::ClaudeCodeVersion);
        assert!(!version.ok);
        assert!(
            version.detail.contains(&MIN_CLAUDE_CODE.to_string()),
            "{}",
            version.detail
        );
    }

    #[test]
    fn missing_result_fails_result_event() {
        let mut facts = facts();
        let outcome = run_outcome(Some("2.1.296"), None);
        facts.result_is_error = outcome.is_error;
        facts.verdict = Some(outcome.verdict.clone());
        facts.outcome = Some(outcome);
        assert_eq!(facts.result_is_error, None);
        let report = evaluate(&facts);
        assert!(!report.ok);
        let result = check(&report, CheckId::ResultEvent);
        assert!(!result.ok);
        assert_eq!(result.detail, "the run failed: no_result");
    }

    #[test]
    fn advisory_transcript_failure_keeps_ok() {
        let mut facts = facts();
        facts.transcript_hint = None;
        facts.transcript_exists = false;
        let report = evaluate(&facts);
        assert!(report.ok);
        let transcript = check(&report, CheckId::Transcript);
        assert!(!transcript.ok && transcript.advisory);
    }

    #[test]
    fn stragglers_warning_keeps_ok() {
        let mut facts = facts();
        if let Some(settle) = facts.settle.as_mut() {
            settle.stragglers = 2;
            settle.outcome = SettleOutcome::Terminated;
        }
        let report = evaluate(&facts);
        assert!(report.ok);
        let stragglers = check(&report, CheckId::Stragglers);
        assert!(!stragglers.ok && stragglers.advisory);
        assert!(stragglers.detail.contains('2'), "{}", stragglers.detail);
        assert!(
            stragglers.detail.contains("terminated"),
            "{}",
            stragglers.detail
        );
    }

    #[test]
    fn details_never_contain_the_launcher_path() {
        let path = std::env::temp_dir()
            .join("claudeclaw-launcher-secret")
            .join("ccs-work");
        let mut facts = facts();
        facts.resolve = Err(LauncherError::Missing { path: path.clone() });
        let leak = path.to_string_lossy().into_owned();
        let report = evaluate(&facts);
        for c in &report.checks {
            assert!(!c.detail.contains(&leak), "{:?}: {}", c.id, c.detail);
            assert!(!c.detail.contains("ccs-work"), "{:?}: {}", c.id, c.detail);
        }
        // Debug is content-free too: it reports the refusal's code, never the path.
        let debug = format!("{facts:?}");
        assert!(!debug.contains(&leak), "{debug}");
        assert!(!debug.contains("ccs-work"), "{debug}");
        assert!(debug.contains("missing"), "{debug}");
    }

    #[test]
    fn check_ids_are_the_wire_order() {
        let want = [
            "runnable",
            "print_stream_json",
            "clean_stdout",
            "system_prompt",
            "result_event",
            "prompt_stdin",
            "claude_code_version",
            "transcript",
            "stragglers",
        ];
        let got: Vec<&str> = CheckId::ALL.iter().map(|id| id.as_str()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn test_prompt_keeps_the_marker_in_the_system_prompt_only() {
        let (stdin, system) = test_prompt("nonce-1234");
        assert!(system.contains(&format!("{SYSTEM_PROMPT_MARKER_PREFIX}nonce-1234")));
        assert!(!stdin.contains("nonce-1234"), "{stdin}");
        assert!(stdin.contains("system prompt"));
        assert!(!system.contains("stdin"));
    }
}
