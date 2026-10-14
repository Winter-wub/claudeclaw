//! `judge()`: a finished run's outcome from its stream and its exit (architecture §4 runner
//! `stream`; #5, #8, #26, #39, #42, #47).

use std::collections::BTreeMap;
use std::fmt;

use super::hint::TranscriptHint;
use super::item::{ModelUsage, ResultEvent, Usage};
use super::state::StreamState;
use crate::argv::ClaudeArgs;
use crate::version::{ClaudeCodeVersion, VersionCheck};

/// The Awake-clock facts spawn measured for the run (#38 D6); copied into [`RunOutcome`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunClock {
    /// The runner's measure of the run, without suspended time.
    pub awake_ms: u64,
    /// The sum of Resume gaps during the run.
    pub suspended_ms: u64,
    /// A Wake grace moved the run's deadline.
    pub wake_grace: bool,
}

/// How the run's process ended, as spawn reports it (the seam between `spawn` and `stream`).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ExitReport {
    /// The leader's exit code; `None` when a signal ended it.
    pub exit_code: Option<i32>,
    /// The signal that ended the leader (Unix).
    pub signal: Option<i32>,
    /// The Awake-clock timeout fired and the run was killed (#38, #47).
    pub timed_out: bool,
    /// A Daemon shutdown stopped the run.
    pub interrupted: bool,
    /// Stderr, as captured (capped by spawn). Read only for the rules below.
    pub stderr: String,
    pub clock: RunClock,
}

impl fmt::Debug for ExitReport {
    // Content-free (architecture §2): stderr may quote the prompt.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExitReport")
            .field("exit_code", &self.exit_code)
            .field("signal", &self.signal)
            .field("timed_out", &self.timed_out)
            .field("interrupted", &self.interrupted)
            .field("stderr_bytes", &self.stderr.len())
            .field("clock", &self.clock)
            .finish()
    }
}

/// How a run ended (architecture §4 runner `stream`). Maps onto the Run index's `outcome`
/// (`ok`, `failed`, `timeout`, `interrupted`, `rate_limited`) plus Refused runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A `result` with `is_error: false`, whatever the exit code (#5, #8).
    Ok,
    Failed(Failure),
    /// Killed at the Awake-clock timeout before any `result` was read (#47 D3).
    Timeout,
    /// Stopped by a Daemon shutdown before any `result` was read.
    Interrupted,
    /// The subscription limit (TS `RATE_LIMIT_PATTERN`, `src/runner.ts:164`).
    RateLimited {
        reset: Option<RateLimitReset>,
    },
    /// Claude Code never accepted the run (glossary "Refused run"; #26, #42).
    Refused(Refusal),
}

/// Why a run failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// No `result` event, even on exit 0 (#8).
    NoResult,
    /// `result.is_error` (#5): `subtype` and `api_error_status` as reported.
    ResultError {
        subtype: Option<String>,
        api_error_status: Option<u16>,
    },
    /// `--resume` of a session Claude Code doesn't have (#39 D5, E3).
    SessionNotFound,
    /// A thinking-block signature mismatch on a resumed session (TS `SIGNATURE_ERROR`,
    /// `src/runner.ts:166`).
    ThinkingSignature,
}

impl Failure {
    /// A stable snake_case code for logs and records.
    pub fn code(&self) -> &'static str {
        match self {
            Failure::NoResult => "no_result",
            Failure::ResultError { .. } => "result_error",
            Failure::SessionNotFound => "session_not_found",
            Failure::ThinkingSignature => "thinking_signature",
        }
    }
}

/// Why a run was refused (#26 D3, #42 D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Exit 126 or 127 with no JSON line (#26 D3).
    LauncherExecFailed { exit_code: i32 },
    /// `error: unknown option '<flag>'` with no JSON line and a non-zero exit (#42 D3).
    ClaudeCodeUnsupportedFlag { flag: String, from: FlagSource },
}

impl Refusal {
    /// The refusal reason's wire code.
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::LauncherExecFailed { .. } => "launcher_exec_failed",
            Refusal::ClaudeCodeUnsupportedFlag { .. } => "claude_code_unsupported_flag",
        }
    }
}

/// Who passed a flag Claude Code rejected (#42 D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagSource {
    /// [`ClaudeArgs`] emitted it: the Claude Code is too old.
    Daemon,
    /// The Launcher script added it.
    Launcher,
}

/// The UTC time of day a subscription limit resets, from `resets 3pm (UTC)` (TS
/// `parseRateLimitResetTime`, `src/runner.ts:203`). The caller picks the next such instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitReset {
    pub hour: u8,
    pub minute: u8,
}

/// The judged run (architecture §4 runner "Output"; #39, #38, #42, #47).
#[derive(Clone, PartialEq)]
pub struct RunOutcome {
    pub verdict: Verdict,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// A `result` was read before the timeout killed the run; judged by that `result` (#47 D3).
    pub lingered: bool,
    pub session_id: Option<String>,
    /// From `init`; `None` when there was no `init` (#42 D5).
    pub claude_code_version: Option<ClaudeCodeVersion>,
    /// `init.apiKeySource`; `None` when there was no `init` (#39 D9).
    pub api_key_source: Option<String>,
    pub transcript_hint: Option<TranscriptHint>,
    /// `result.result`: the reply text.
    pub reply: Option<String>,
    pub is_error: Option<bool>,
    pub api_error_status: Option<u16>,
    pub num_turns: Option<u32>,
    /// `result.usage`: this run only (#39 E4).
    pub usage: Option<Usage>,
    /// `result.modelUsage`: session-cumulative on `--resume` (#39 E4).
    pub model_usage: BTreeMap<String, ModelUsage>,
    /// `result.total_cost_usd`: session-cumulative on `--resume` (#39 E4).
    pub total_cost_usd: Option<f64>,
    /// The last main-thread assistant `message.usage`, for the context meter (#39 D4).
    pub last_main_usage: Option<Usage>,
    /// The main thread's full model ID, the key into `model_usage`: the last main-thread
    /// assistant `message.model`, else `init.model` (#39 D4).
    pub model: Option<String>,
    pub clock: RunClock,
}

impl fmt::Debug for RunOutcome {
    // Content-free (architecture §2): no reply text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunOutcome")
            .field("verdict", &self.verdict)
            .field("exit_code", &self.exit_code)
            .field("signal", &self.signal)
            .field("lingered", &self.lingered)
            .field("session_id", &self.session_id)
            .field("claude_code_version", &self.claude_code_version)
            .field("reply_bytes", &self.reply.as_ref().map(String::len))
            .field("api_error_status", &self.api_error_status)
            .field("model", &self.model)
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl RunOutcome {
    /// The `claude_code_outdated` judgement (#42 D4); `None` when there was no version.
    pub fn version_check(&self) -> Option<VersionCheck> {
        self.claude_code_version
            .as_ref()
            .map(ClaudeCodeVersion::check)
    }

    /// The context window `modelUsage` reports for the main thread's [`RunOutcome::model`]
    /// (#39 D4, E10). The caller's alias (`opus`) is not a `modelUsage` key; this full ID is.
    pub fn context_window(&self) -> Option<u64> {
        self.model_usage.get(self.model.as_deref()?)?.context_window
    }
}

/// Judges a finished run (architecture §4 runner `stream`). `args` is the run's argv, used to
/// tell a flag the Daemon passed from one the Launcher added (#42 D3).
///
/// Rules, in order:
/// 1. A `result` was read: it decides, even after a timeout (`lingered`, #47 D3). A
///    `session_not_found` shape (#39 D5), then the subscription limit, then `is_error`
///    (#5, #8); otherwise `Ok`, whatever the exit code.
/// 2. No `result`: `Interrupted`, then `Timeout`.
/// 3. No JSON line and a non-zero exit: `unknown option` on stderr is Refused
///    `claude_code_unsupported_flag` (#42 D3); exit 126/127 is Refused `launcher_exec_failed`
///    (#26 D3).
/// 4. Otherwise failed: the subscription limit or a thinking signature on stderr, else
///    `NoResult` (#8).
pub fn judge(state: &StreamState, exit: &ExitReport, args: &ClaudeArgs) -> RunOutcome {
    let result = state.result.as_ref();
    let verdict = match result {
        Some(r) => judge_result(r, &exit.stderr),
        None if exit.interrupted => Verdict::Interrupted,
        None if exit.timed_out => Verdict::Timeout,
        None => judge_no_result(state, exit, args),
    };
    let init = state.init.as_ref();
    RunOutcome {
        verdict,
        exit_code: exit.exit_code,
        signal: exit.signal,
        lingered: result.is_some() && exit.timed_out,
        session_id: state.session_id.clone(),
        claude_code_version: init.and_then(|i| i.version.clone()),
        api_key_source: init.and_then(|i| i.api_key_source.clone()),
        transcript_hint: init.and_then(|i| i.transcript_hint.clone()),
        reply: result.and_then(|r| r.result.clone()),
        is_error: result.map(|r| r.is_error),
        api_error_status: result.and_then(|r| r.api_error_status),
        num_turns: result.and_then(|r| r.num_turns),
        usage: result.and_then(|r| r.usage),
        model_usage: result.map(|r| r.model_usage.clone()).unwrap_or_default(),
        total_cost_usd: result.and_then(|r| r.total_cost_usd),
        last_main_usage: state.last_main_usage,
        model: state
            .last_main_model
            .clone()
            .or_else(|| init.and_then(|i| i.model.clone())),
        clock: exit.clock,
    }
}

fn judge_result(r: &ResultEvent, stderr: &str) -> Verdict {
    if is_session_not_found(r) {
        return Verdict::Failed(Failure::SessionNotFound);
    }
    if !r.is_error {
        return Verdict::Ok;
    }
    let reply = r.result.as_deref().unwrap_or("");
    for text in [reply, stderr] {
        if is_rate_limit(text) {
            return Verdict::RateLimited {
                reset: rate_limit_reset(text),
            };
        }
    }
    if has_signature_error(reply)
        || r.errors.iter().any(|e| has_signature_error(e))
        || has_signature_error(stderr)
    {
        return Verdict::Failed(Failure::ThinkingSignature);
    }
    Verdict::Failed(Failure::ResultError {
        subtype: r.subtype.clone(),
        api_error_status: r.api_error_status,
    })
}

fn judge_no_result(state: &StreamState, exit: &ExitReport, args: &ClaudeArgs) -> Verdict {
    if !state.json_seen() && exit.exit_code != Some(0) {
        if let Some(flag) = unknown_option(&exit.stderr) {
            let from = if args.emits_flag(&flag) {
                FlagSource::Daemon
            } else {
                FlagSource::Launcher
            };
            return Verdict::Refused(Refusal::ClaudeCodeUnsupportedFlag { flag, from });
        }
        if let Some(code @ (126 | 127)) = exit.exit_code {
            return Verdict::Refused(Refusal::LauncherExecFailed { exit_code: code });
        }
    }
    if is_rate_limit(&exit.stderr) {
        return Verdict::RateLimited {
            reset: rate_limit_reset(&exit.stderr),
        };
    }
    if has_signature_error(&exit.stderr) {
        return Verdict::Failed(Failure::ThinkingSignature);
    }
    Verdict::Failed(Failure::NoResult)
}

/// #39 E3: `result{subtype: "error_during_execution", is_error: true, num_turns: 0,
/// errors: ["No conversation found with session ID: <id>"]}`. Never read from stderr (#39 D5).
fn is_session_not_found(r: &ResultEvent) -> bool {
    r.is_error
        && r.subtype.as_deref() == Some("error_during_execution")
        && matches!(r.num_turns, None | Some(0))
        && r.errors
            .iter()
            .any(|e| contains_ci(e, "no conversation found with session id"))
}

/// The first stderr line matching `^error: unknown option '(--[A-Za-z0-9-]+)'` (#42 D3, E4).
fn unknown_option(stderr: &str) -> Option<String> {
    stderr.lines().find_map(|line| {
        let rest = line.strip_prefix("error: unknown option '")?;
        let (flag, _) = rest.split_once('\'')?;
        let name = flag.strip_prefix("--")?;
        let ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        ok.then(|| flag.to_owned())
    })
}

fn contains_ci(haystack: &str, needle_lower: &str) -> bool {
    haystack.to_lowercase().contains(needle_lower)
}

/// TS `RATE_LIMIT_PATTERN` (`src/runner.ts:164`), case-insensitive. The curly apostrophe is
/// accepted too.
fn is_rate_limit(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "you've hit your limit",
        "you\u{2019}ve hit your limit",
        "out of extra usage",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// TS `SIGNATURE_ERROR` = `/Invalid.*signature.*thinking block/i` (`src/runner.ts:166`): within
/// one line.
fn has_signature_error(text: &str) -> bool {
    text.lines().any(|line| {
        let lower = line.to_lowercase();
        let Some(a) = lower.find("invalid") else {
            return false;
        };
        let rest = &lower[a + "invalid".len()..];
        let Some(b) = rest.find("signature") else {
            return false;
        };
        rest[b + "signature".len()..].contains("thinking block")
    })
}

/// TS `RATE_LIMIT_RESET_PATTERN` =
/// `/resets?\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s*\(?\s*UTC\s*\)?/i` and the 12-hour
/// conversion of `parseRateLimitResetTime` (`src/runner.ts:165`, `:203-212`). Out-of-range
/// times give `None`.
fn rate_limit_reset(text: &str) -> Option<RateLimitReset> {
    let lower = text.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut from = 0;
    while let Some(off) = lower[from..].find("reset") {
        let start = from + off + "reset".len();
        from = start;
        let mut i = start;
        if b.get(i) == Some(&b's') {
            i += 1;
        }
        // Try "resets" first, then "reset" followed directly by whitespace.
        for j in [i, start] {
            if let Some(r) = reset_after(b, j) {
                return Some(r);
            }
        }
    }
    None
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// `\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\s*\(?\s*utc`, at `i`, with the regex's backtracking
/// over the hour's width.
fn reset_after(b: &[u8], i: usize) -> Option<RateLimitReset> {
    let after_ws = skip_ws(b, i);
    if after_ws == i {
        return None;
    }
    let digits = b[after_ws..]
        .iter()
        .take(2)
        .take_while(|c| c.is_ascii_digit())
        .count();
    (1..=digits).rev().find_map(|width| {
        let hour_end = after_ws + width;
        let hour: u32 = std::str::from_utf8(&b[after_ws..hour_end])
            .ok()?
            .parse()
            .ok()?;
        let mut k = hour_end;
        let mut minute = 0u32;
        if b.get(k) == Some(&b':')
            && b.get(k + 1).is_some_and(u8::is_ascii_digit)
            && b.get(k + 2).is_some_and(u8::is_ascii_digit)
        {
            minute = u32::from(b[k + 1] - b'0') * 10 + u32::from(b[k + 2] - b'0');
            k += 3;
        }
        k = skip_ws(b, k);
        let mut ampm = None;
        if b[k..].starts_with(b"am") || b[k..].starts_with(b"pm") {
            ampm = Some(b[k]);
            k += 2;
        }
        k = skip_ws(b, k);
        if b.get(k) == Some(&b'(') {
            k += 1;
        }
        k = skip_ws(b, k);
        if !b[k..].starts_with(b"utc") {
            return None;
        }
        let mut hour = hour;
        match ampm {
            Some(b'p') if hour < 12 => hour += 12,
            Some(b'a') if hour == 12 => hour = 0,
            _ => {}
        }
        (hour < 24 && minute < 60).then_some(RateLimitReset {
            hour: hour as u8,
            minute: minute as u8,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::ClaudeArgs;
    use crate::stream::StreamState;

    fn state(lines: &[&str]) -> StreamState {
        let mut s = StreamState::new();
        for line in lines {
            s.push_line(line);
        }
        s
    }

    fn exit(code: i32) -> ExitReport {
        ExitReport {
            exit_code: Some(code),
            ..ExitReport::default()
        }
    }

    fn with_stderr(mut e: ExitReport, stderr: &str) -> ExitReport {
        e.stderr = stderr.into();
        e
    }

    const INIT: &str =
        r#"{"type":"system","subtype":"init","session_id":"s-1","claude_code_version":"2.1.296"}"#;
    const OK: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"hi","session_id":"s-1"}"#;
    const ERR_429: &str = r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: 429","api_error_status":429}"#;
    const STALE: &str = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":0,"errors":["No conversation found with session ID: 1234"]}"#;
    const LIMIT: &str = r#"{"type":"result","subtype":"success","is_error":true,"result":"You've hit your limit · resets 3pm (UTC)"}"#;
    const SIG: &str = r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: 400 Invalid `signature` in `thinking` block... thinking block"}"#;

    #[test]
    fn judge_rules() {
        let asp_args =
            ClaudeArgs::new().append_system_prompt_file(std::env::temp_dir().join("asp.md"));
        let timed_out = ExitReport {
            timed_out: true,
            signal: Some(15),
            ..ExitReport::default()
        };
        let interrupted = ExitReport {
            interrupted: true,
            exit_code: Some(143),
            ..ExitReport::default()
        };
        let unknown_daemon = "error: unknown option '--append-system-prompt-file'\n";
        let unknown_launcher =
            "error: unknown option '--settings'\n(Did you mean --setting-sources?)\n";

        let cases: Vec<(&str, StreamState, ExitReport, Verdict)> = vec![
            ("result ok", state(&[INIT, OK]), exit(0), Verdict::Ok),
            // #5/#8: judged from the result, not the exit code.
            (
                "result ok, exit 1",
                state(&[INIT, OK]),
                exit(1),
                Verdict::Ok,
            ),
            (
                "no result on exit 0",
                state(&[INIT]),
                exit(0),
                Verdict::Failed(Failure::NoResult),
            ),
            (
                "nothing at all, exit 0",
                state(&[]),
                exit(0),
                Verdict::Failed(Failure::NoResult),
            ),
            (
                "is_error with api_error_status",
                state(&[INIT, ERR_429]),
                exit(1),
                Verdict::Failed(Failure::ResultError {
                    subtype: Some("success".into()),
                    api_error_status: Some(429),
                }),
            ),
            (
                "session_not_found (#39 E3)",
                state(&[STALE]),
                with_stderr(exit(1), "No conversation found with session ID: 1234"),
                Verdict::Failed(Failure::SessionNotFound),
            ),
            (
                "stale text on stderr alone is not session_not_found (#39 D5)",
                state(&[INIT]),
                with_stderr(exit(1), "No conversation found with session ID: 1234"),
                Verdict::Failed(Failure::NoResult),
            ),
            (
                "rate limit in the result",
                state(&[INIT, LIMIT]),
                exit(1),
                Verdict::RateLimited {
                    reset: Some(RateLimitReset {
                        hour: 15,
                        minute: 0,
                    }),
                },
            ),
            (
                "rate limit on stderr with no result",
                state(&[INIT]),
                with_stderr(
                    exit(1),
                    "Claude AI: you've hit your limit, resets 9:30am UTC",
                ),
                Verdict::RateLimited {
                    reset: Some(RateLimitReset {
                        hour: 9,
                        minute: 30,
                    }),
                },
            ),
            (
                "a reply quoting the limit text is still ok",
                state(&[
                    INIT,
                    r#"{"type":"result","subtype":"success","is_error":false,"result":"they said you've hit your limit"}"#,
                ]),
                exit(0),
                Verdict::Ok,
            ),
            (
                "thinking signature in the result",
                state(&[INIT, SIG]),
                exit(1),
                Verdict::Failed(Failure::ThinkingSignature),
            ),
            (
                "thinking signature on stderr with no result",
                state(&[INIT]),
                with_stderr(exit(1), "Error: invalid signature in thinking block"),
                Verdict::Failed(Failure::ThinkingSignature),
            ),
            (
                "timeout without result",
                state(&[INIT]),
                timed_out.clone(),
                Verdict::Timeout,
            ),
            // #47 D3: a result read before the timeout wins.
            (
                "result before timeout wins",
                state(&[INIT, OK]),
                timed_out.clone(),
                Verdict::Ok,
            ),
            (
                "interrupted without result",
                state(&[INIT]),
                interrupted.clone(),
                Verdict::Interrupted,
            ),
            (
                "result before interruption wins",
                state(&[INIT, OK]),
                interrupted,
                Verdict::Ok,
            ),
            // #26 D3.
            (
                "exit 127 with no JSON",
                state(&["env: node: No such file or directory"]),
                exit(127),
                Verdict::Refused(Refusal::LauncherExecFailed { exit_code: 127 }),
            ),
            (
                "exit 126 with no JSON",
                state(&[]),
                exit(126),
                Verdict::Refused(Refusal::LauncherExecFailed { exit_code: 126 }),
            ),
            (
                "exit 127 after a JSON line is an ordinary failure",
                state(&[INIT]),
                exit(127),
                Verdict::Failed(Failure::NoResult),
            ),
            (
                "exit 127 after a malformed JSON line is an ordinary failure",
                state(&[r#"{"no_type":1}"#]),
                exit(127),
                Verdict::Failed(Failure::NoResult),
            ),
            // #42 D3.
            (
                "unknown option the Daemon passed",
                state(&[]),
                with_stderr(exit(1), unknown_daemon),
                Verdict::Refused(Refusal::ClaudeCodeUnsupportedFlag {
                    flag: "--append-system-prompt-file".into(),
                    from: FlagSource::Daemon,
                }),
            ),
            (
                "unknown option a Launcher added",
                state(&["[ccs] starting"]),
                with_stderr(exit(1), unknown_launcher),
                Verdict::Refused(Refusal::ClaudeCodeUnsupportedFlag {
                    flag: "--settings".into(),
                    from: FlagSource::Launcher,
                }),
            ),
            (
                "unknown option after a JSON line is not a refusal",
                state(&[INIT]),
                with_stderr(exit(1), unknown_daemon),
                Verdict::Failed(Failure::NoResult),
            ),
            (
                "unknown option on exit 0 is not a refusal",
                state(&[]),
                with_stderr(exit(0), unknown_daemon),
                Verdict::Failed(Failure::NoResult),
            ),
            (
                "the template must start the line",
                state(&[]),
                with_stderr(exit(1), "warn: error: unknown option '--x'"),
                Verdict::Failed(Failure::NoResult),
            ),
            (
                "killed by a signal with no JSON",
                state(&[]),
                ExitReport {
                    signal: Some(9),
                    ..ExitReport::default()
                },
                Verdict::Failed(Failure::NoResult),
            ),
        ];
        for (name, st, ex, want) in cases {
            assert_eq!(judge(&st, &ex, &asp_args).verdict, want, "{name}");
        }
    }

    #[test]
    fn lingered_only_when_a_result_beat_the_timeout() {
        let args = ClaudeArgs::new();
        let to = ExitReport {
            timed_out: true,
            ..ExitReport::default()
        };
        assert!(judge(&state(&[INIT, OK]), &to, &args).lingered);
        assert!(!judge(&state(&[INIT]), &to, &args).lingered);
        assert!(!judge(&state(&[INIT, OK]), &exit(0), &args).lingered);
    }

    #[test]
    fn outcome_carries_clock_and_version() {
        let ex = ExitReport {
            exit_code: Some(0),
            clock: RunClock {
                awake_ms: 1_500,
                suspended_ms: 60_000,
                wake_grace: true,
            },
            ..ExitReport::default()
        };
        let out = judge(&state(&[INIT, OK]), &ex, &ClaudeArgs::new());
        assert_eq!(out.clock, ex.clock);
        assert_eq!(out.session_id.as_deref(), Some("s-1"));
        assert_eq!(out.claude_code_version.unwrap().raw, "2.1.296");
        assert_eq!(out.reply.as_deref(), Some("hi"));

        let old = state(&[
            r#"{"type":"system","subtype":"init","claude_code_version":"2.1.100"}"#,
            OK,
        ]);
        let out = judge(&old, &exit(0), &ClaudeArgs::new());
        assert_eq!(out.version_check(), Some(VersionCheck::Outdated));
        // Below the minimum still runs and is judged as usual (#42 D4).
        assert_eq!(out.verdict, Verdict::Ok);
        assert_eq!(
            judge(&state(&[OK]), &exit(0), &ClaudeArgs::new()).version_check(),
            None
        );
    }

    #[test]
    fn rate_limit_reset_times() {
        let cases: &[(&str, Option<(u8, u8)>)] = &[
            ("resets 3pm (UTC)", Some((15, 0))),
            ("Resets 12am UTC", Some((0, 0))),
            ("resets 12pm UTC", Some((12, 0))),
            ("resets 11:45 pm (utc)", Some((23, 45))),
            ("reset 7 UTC", Some((7, 0))),
            ("resets 14:05 UTC", Some((14, 5))),
            ("resets 3pm (PST)", None),
            ("resets soon", None),
            ("resets 123 UTC", None),
            ("resets 25 UTC", None),
            ("presets 3pm, then resets 4pm UTC", Some((16, 0))),
            ("no time here", None),
        ];
        for (text, want) in cases {
            let got = rate_limit_reset(text).map(|r| (r.hour, r.minute));
            assert_eq!(got, *want, "{text:?}");
        }
    }

    #[test]
    fn rate_limit_patterns() {
        for (text, want) in [
            ("You've hit your limit", true),
            ("YOU\u{2019}VE HIT YOUR LIMIT", true),
            ("You're out of extra usage", true),
            ("you have hit your limit", false),
        ] {
            assert_eq!(is_rate_limit(text), want, "{text:?}");
        }
    }

    #[test]
    fn signature_pattern_stays_within_a_line() {
        assert!(has_signature_error(
            "x Invalid foo signature bar thinking block y"
        ));
        assert!(!has_signature_error("Invalid signature\nin thinking block"));
        assert!(!has_signature_error("thinking block has invalid signature"));
    }

    #[test]
    fn unknown_option_template() {
        for (stderr, want) in [
            ("error: unknown option '--foo'", Some("--foo")),
            (
                "x\nerror: unknown option '--foo-bar2'\n",
                Some("--foo-bar2"),
            ),
            ("error: unknown option '-p'", None),
            ("error: unknown option '--'", None),
            ("error: unknown option '--a b'", None),
            ("error: unknown option '--foo", None),
            ("Error: unknown option '--foo'", None),
        ] {
            assert_eq!(unknown_option(stderr).as_deref(), want, "{stderr:?}");
        }
    }

    #[test]
    fn codes() {
        assert_eq!(
            Refusal::LauncherExecFailed { exit_code: 127 }.code(),
            "launcher_exec_failed"
        );
        assert_eq!(
            Refusal::ClaudeCodeUnsupportedFlag {
                flag: "--x".into(),
                from: FlagSource::Daemon
            }
            .code(),
            "claude_code_unsupported_flag"
        );
        assert_eq!(Failure::SessionNotFound.code(), "session_not_found");
    }

    #[test]
    fn debug_is_content_free() {
        let out = judge(
            &state(&[INIT, OK]),
            &with_stderr(exit(0), "secret stderr"),
            &ClaudeArgs::new(),
        );
        assert!(!format!("{out:?}").contains("hi\""));
        assert!(!format!("{:?}", with_stderr(exit(0), "secret stderr")).contains("secret"));
    }
}
