//! One Chat Turn end to end (architecture §4 `conversations`, §6; #8, #9, #25, #29, #33, #37,
//! #79).
//!
//! [`Pipeline::run_turn`] is the body behind one queued Turn. In order it:
//!
//! 1. resolves the Scope's Launcher ([`LauncherSettings::resolve`]) and resolves it on the Run
//!    PATH ([`claudeclaw_runner::launcher::resolve`]). A Refused run — a Launcher that cannot run —
//!    becomes a failed outcome with a `run.refused` owner notice and **no fallback** (#8, #26);
//! 2. looks up the (Scope, Launcher) session ([`SessionManager::get`]) and, when there is one, tells
//!    the [`Planner`] to `--resume` it;
//! 3. plans the run ([`Planner::plan`]), writes the per-run System-prompt file at 0600 under
//!    `daemon/tmp/` (#25 D2), enters the run in the Run registry with its origin ([`RunRegistry`],
//!    #29, #79) and spawns it ([`claudeclaw_runner::run::start`]);
//! 4. forwards the run's events to the adapter, arms the Turn's health beacon
//!    ([`HealthMonitor::turn`]), and waits for the outcome and then the settle (#37);
//! 5. records the run against its session ([`SessionManager::record_run`]) and applies the
//!    recovery policy ([`SessionManager::decide`]): a stale session is retried once fresh; a
//!    resumed-session timeout runs `/compact` and re-runs the Turn once (#33). Every run in the
//!    Turn shares `turn_id` and counts `attempt` up, with a `purpose` of `turn`, `compact` or
//!    `compact_retry` (#33);
//! 6. writes the Run log and appends the Run-index record ([`RunLog`], [`RunIndex`]), ends the Turn
//!    ([`SessionManager::end_turn`]) and returns the adapter's [`ChatOutcome`].
//!
//! The queue calls this and only returns to the next Turn once it has resolved, so the next Turn
//! never starts before the previous run is settled (#37).
//!
//! # Rate-limit fallback (follow-up)
//!
//! When the recovery policy asks for [`Recovery::RateLimitFallback`], this module returns the
//! failed outcome and does **not** re-run against the fallback model. The branch is named
//! explicitly below; wiring the fallback model and API is a deliberate follow-up (#8).
//!
//! # Scopes with no wire form
//!
//! The session store, the Run registry and the Run index all key on
//! [`claudeclaw_api::scope::Scope`], which today names only Discord. A Chat Turn from a transport
//! with no wire form still runs — planned, spawned and logged — but **sessionless**: no `--resume`,
//! no session record, no recovery, no Run registry entry and no Run-index record. Phase 1 adapts
//! Discord only, so this path is not reachable in production; it exists so the pipeline is total.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use claudeclaw_api::log_event::daemon;
use claudeclaw_api::runs::{
    CostStatus, RunOrigin, RunOutcome as IndexOutcome, RunPurpose, RunRecord, RunSession,
    RunSettle, SettleOutcome as ApiSettleOutcome,
};
use claudeclaw_api::scope::Scope as ApiScope;
use claudeclaw_api::supervisor::{
    ChatOrigin, KillMethod, ReapOutcome, ReapedRun, RegistryEntry, RegistryState, SpawnPurpose,
};
use claudeclaw_chat::RunEvent;
use claudeclaw_chat::{ChatOutcome, MessageRef, Notifier, OutcomeKind, Turn};
use claudeclaw_runner::argv::{ArgvError, ClaudeArgs};
use claudeclaw_runner::env::{RunEnv, RunVars};
use claudeclaw_runner::launcher::{Launcher, LauncherError, resolve};
use claudeclaw_runner::run::{RunEvent as RunnerEvent, RunRequest, start};
use claudeclaw_runner::spawn::{Settle, SpawnError, SpawnSpec, Spawner};
use claudeclaw_runner::stream::{RunOutcome, StreamEvent, Verdict};
use claudeclaw_scheduler::WallClock;
use claudeclaw_store::{
    FileMode, Layout, RunIndex, RunLog, RunLogBody, RunSection, StoreError, write_atomic,
};
use tokio::sync::mpsc;

use crate::health::HealthMonitor;
use crate::notify::{Notice, NoticeKind, Notices};
use crate::runs::{RunRegistry, chat_origin};
use crate::scope::{LauncherSettings, Scope};
use crate::sessions::{Recovery, RunFacts, RunRecorded, SessionManager};

/// The prompt an Auto-compact run sends on stdin (`/compact`).
pub const COMPACT_PROMPT: &str = "/compact";

/// What the [`Planner`] is asked to plan: the Turn and the session to resume, if any.
#[derive(Clone, PartialEq, Eq)]
pub struct RunRequestInput {
    /// The Turn being run.
    pub turn: Turn,
    /// The session to `--resume`, or `None` for a fresh session.
    pub session_id: Option<String>,
}

impl fmt::Debug for RunRequestInput {
    // Content-free (§2): `Turn`'s Debug prints sizes, `session_id` is an opaque id.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunRequestInput")
            .field("turn", &self.turn)
            .field("session_id", &self.session_id)
            .finish()
    }
}

/// A planned run: everything [`claudeclaw_runner::run::start`] needs, plus the timeout and the
/// model for the record. The planner owns prompt shaping; the pipeline only sets the per-run
/// System-prompt file path on `args` (#25 D2).
pub struct PlannedRun {
    /// The prompt, written to the child's stdin.
    pub stdin_prompt: String,
    /// The appended System prompt, written to the per-run file.
    pub system_prompt: String,
    /// The argv after the executable. The pipeline appends `--append-system-prompt-file`.
    pub args: ClaudeArgs,
    /// The run timeout, in awake time (#38).
    pub timeout: Duration,
    /// The model for the Run-index record and `run.start`.
    pub model: Option<String>,
}

impl fmt::Debug for PlannedRun {
    // Content-free (§2): prompts print as sizes only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlannedRun")
            .field("stdin_prompt_bytes", &self.stdin_prompt.len())
            .field("system_prompt_bytes", &self.system_prompt.len())
            .field("args", &self.args)
            .field("timeout", &self.timeout)
            .field("model", &self.model)
            .finish()
    }
}

/// Why a [`Planner`] could not plan a run. The message is user-facing and must carry no secret.
#[derive(Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanFailure {
    /// The prompt could not be shaped (e.g. a project file could not be read).
    #[error("the prompt could not be shaped")]
    Prompt {
        /// The user-facing reason.
        message: String,
    },
}

impl PlanFailure {
    /// The `run.refused` reason code.
    pub fn code(&self) -> &'static str {
        match self {
            PlanFailure::Prompt { .. } => "prompt_failed",
        }
    }

    /// The user-facing message.
    pub fn message(&self) -> &str {
        match self {
            PlanFailure::Prompt { message } => message,
        }
    }
}

impl fmt::Debug for PlanFailure {
    // Content-free (§2): the message prints as a size.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanFailure::Prompt { message } => f
                .debug_struct("Prompt")
                .field("code", &self.code())
                .field("message_bytes", &message.len())
                .finish(),
        }
    }
}

/// Shapes the stdin prompt, the appended System prompt and the argv for one run. Implemented by
/// the composition root (`conversations::prompt`); tests use a fake.
pub trait Planner: Send + Sync {
    /// Plans the run for `request`, or fails with a user-facing reason.
    fn plan(&self, request: &RunRequestInput) -> Result<PlannedRun, PlanFailure>;
}

/// Everything the pipeline needs to run Turns. Built by the composition root; tests build it over
/// a temp-dir [`Layout`] and a fake `claude`.
pub struct PipelineConfig {
    pub layout: Layout,
    pub notices: Arc<Notices>,
    pub sessions: Arc<SessionManager>,
    pub runs: RunRegistry,
    pub health: Arc<HealthMonitor>,
    pub spawner: Spawner,
    pub planner: Arc<dyn Planner>,
    pub launchers: LauncherSettings,
    pub run_env: RunEnv,
    pub run_index: Arc<RunIndex>,
    pub clock: Arc<dyn WallClock>,
}

/// The Turn → run → session → record pipeline (§4 `conversations`).
pub struct Pipeline {
    layout: Layout,
    notices: Arc<Notices>,
    sessions: Arc<SessionManager>,
    runs: RunRegistry,
    health: Arc<HealthMonitor>,
    spawner: Spawner,
    planner: Arc<dyn Planner>,
    launchers: LauncherSettings,
    run_env: RunEnv,
    run_index: Arc<RunIndex>,
    clock: Arc<dyn WallClock>,
    ids: AtomicU64,
}

impl fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pipeline")
            .field("project_dir", &self.layout.project_dir())
            .finish_non_exhaustive()
    }
}

/// A planner, spawn or argv failure that stopped a Turn before it produced a run.
#[derive(Debug, thiserror::Error)]
enum PipelineError {
    #[error("the System-prompt file could not be written")]
    SystemPrompt(#[source] StoreError),
    #[error("the run could not be spawned")]
    Spawn(#[source] SpawnError),
    #[error("the argv guard refused the run")]
    Argv(#[source] ArgvError),
    #[error("the run's outcome was lost")]
    OutcomeLost,
    #[error("a background task did not finish")]
    Join(#[source] tokio::task::JoinError),
}

impl Pipeline {
    /// Builds the pipeline over `config`.
    pub fn new(config: PipelineConfig) -> Self {
        Pipeline {
            layout: config.layout,
            notices: config.notices,
            sessions: config.sessions,
            runs: config.runs,
            health: config.health,
            spawner: config.spawner,
            planner: config.planner,
            launchers: config.launchers,
            run_env: config.run_env,
            run_index: config.run_index,
            clock: config.clock,
            ids: AtomicU64::new(0),
        }
    }

    /// Runs one Turn to its settle and returns the adapter's outcome. The events channel is
    /// dropped before this returns, so the adapter sees the channel close before the outcome.
    pub async fn run_turn(
        &self,
        turn: Turn,
        events: Option<mpsc::Sender<RunEvent>>,
    ) -> ChatOutcome {
        let scope = Scope::from_chat(&turn.scope);
        let label = scope_label(&scope);
        let api_scope = scope.to_api().ok();
        let launcher = self.launchers.resolve(&scope, None);
        let launcher_key = launcher_key(&launcher);
        let launcher_name = launcher_file_name(&launcher);

        let resolved = match resolve(&launcher, &self.run_env.path) {
            Ok(resolved) => resolved,
            Err(err) => {
                return self.refuse(&label, &scope, &launcher_name, err).await;
            }
        };
        let launcher_resolved = resolved.program().to_string_lossy().into_owned();
        let program = resolved.program().to_path_buf();

        let turn_id = self.next_id("turn");
        let started_at = self.clock.now();
        let cwd = run_cwd(&self.layout, &turn);
        let origin = chat_origin(&turn);

        let mut resume: Option<String> = api_scope
            .as_ref()
            .and_then(|api| self.sessions.get(api, &launcher_key).map(|e| e.session_id));
        let mut is_new = resume.is_none();
        let mut planned = match self.plan(&turn, resume.as_deref()) {
            Ok(planned) => planned,
            Err(failure) => return self.plan_failed(&label, &failure),
        };

        let mut events = events;
        let mut compact_note: Option<String> = None;
        let mut attempt: u32 = 0;
        let final_outcome: RunOutcome;

        loop {
            attempt += 1;
            let session_id = resume.clone();
            let session = session_of(&session_id);
            let input = AttemptInput {
                scope: &scope,
                scope_label: &label,
                turn_id: &turn_id,
                attempt,
                purpose: RunPurpose::Turn,
                spawn_purpose: SpawnPurpose::Turn,
                origin: origin.clone(),
                api_scope: api_scope.as_ref(),
                program: &program,
                cwd: &cwd,
                launcher: &launcher,
                launcher_key: &launcher_key,
                launcher_name: &launcher_name,
                launcher_resolved: &launcher_resolved,
                session_id: session_id.as_deref(),
                session,
                stdin_prompt: &planned.stdin_prompt,
                system_prompt: &planned.system_prompt,
                base_args: &planned.args,
                timeout: planned.timeout,
                model: planned.model.as_deref(),
            };

            let attempt_result = match self.run_attempt(&input, &mut events).await {
                Ok(result) => result,
                Err(err) => return self.start_failed(&label, &err),
            };
            self.record_attempt(&attempt_result, &input).await;

            let recovery = match &api_scope {
                Some(api) => self
                    .sessions
                    .decide(
                        api,
                        &launcher_key,
                        &RunFacts {
                            outcome: &attempt_result.outcome,
                            is_new,
                            run_timeout: planned.timeout,
                            started_at,
                        },
                    )
                    .await
                    .unwrap_or(Recovery::None),
                None => Recovery::None,
            };

            match recovery {
                // Re-plan without `--resume` and run once more. The next attempt's outcome decides:
                // `is_new` is now true, so a second stale-session or timeout verdict falls through
                // to `None`/`Skipped` and no third run happens.
                Recovery::NewSession(_) => match self.plan(&turn, None) {
                    Ok(fresh) => {
                        resume = None;
                        is_new = true;
                        planned = fresh;
                        continue;
                    }
                    Err(failure) => {
                        tracing::warn!(
                            event = daemon::RUN_REFUSED,
                            reason = failure.code(),
                            "the Turn could not be re-planned for a new session"
                        );
                        final_outcome = attempt_result.outcome;
                        break;
                    }
                },
                // `/compact` with its own timeout, then re-run the Turn once (#33).
                Recovery::AutoCompact { timeout } => {
                    attempt += 1;
                    let compact_input = AttemptInput {
                        scope: &scope,
                        scope_label: &label,
                        turn_id: &turn_id,
                        attempt,
                        purpose: RunPurpose::Compact,
                        spawn_purpose: SpawnPurpose::Compact,
                        origin: origin.clone(),
                        api_scope: api_scope.as_ref(),
                        program: &program,
                        cwd: &cwd,
                        launcher: &launcher,
                        launcher_key: &launcher_key,
                        launcher_name: &launcher_name,
                        launcher_resolved: &launcher_resolved,
                        session_id: resume.as_deref(),
                        session: RunSession::Resumed,
                        stdin_prompt: COMPACT_PROMPT,
                        system_prompt: &planned.system_prompt,
                        base_args: &planned.args,
                        timeout,
                        model: planned.model.as_deref(),
                    };
                    let compact = match self.run_attempt(&compact_input, &mut events).await {
                        Ok(result) => result,
                        Err(err) => return self.start_failed(&label, &err),
                    };
                    self.record_attempt(&compact, &compact_input).await;
                    compact_note = Some(compact_note_text(&compact.outcome));

                    attempt += 1;
                    let retry_input = AttemptInput {
                        scope: &scope,
                        scope_label: &label,
                        turn_id: &turn_id,
                        attempt,
                        purpose: RunPurpose::CompactRetry,
                        spawn_purpose: SpawnPurpose::Turn,
                        origin: origin.clone(),
                        api_scope: api_scope.as_ref(),
                        program: &program,
                        cwd: &cwd,
                        launcher: &launcher,
                        launcher_key: &launcher_key,
                        launcher_name: &launcher_name,
                        launcher_resolved: &launcher_resolved,
                        session_id: resume.as_deref(),
                        session: RunSession::Resumed,
                        stdin_prompt: &planned.stdin_prompt,
                        system_prompt: &planned.system_prompt,
                        base_args: &planned.args,
                        timeout: planned.timeout,
                        model: planned.model.as_deref(),
                    };
                    let retry = match self.run_attempt(&retry_input, &mut events).await {
                        Ok(result) => result,
                        Err(err) => return self.start_failed(&label, &err),
                    };
                    self.record_attempt(&retry, &retry_input).await;
                    final_outcome = retry.outcome;
                    break;
                }
                // The run's own outcome stands. `RateLimitFallback` is the named follow-up: it
                // returns the failed outcome instead of re-running against the fallback model (#8).
                Recovery::None | Recovery::Skipped(_) | Recovery::RateLimitFallback => {
                    final_outcome = attempt_result.outcome;
                    break;
                }
            }
        }

        if let Some(api) = &api_scope {
            let _ = self
                .sessions
                .end_turn(api, &launcher_key, &final_outcome, started_at)
                .await;
        }
        self.health.turn(&scope, planned.timeout).disarm();
        chat_outcome(&final_outcome, compact_note.as_deref())
    }

    /// The Refused-run path (#8, #26): a failed outcome and a `run.refused` owner notice, no
    /// spawn and no fallback.
    async fn refuse(
        &self,
        label: &str,
        scope: &Scope,
        launcher_name: &str,
        err: LauncherError,
    ) -> ChatOutcome {
        let text = format!("The Launcher `{launcher_name}` can't run: {err}");
        tracing::info_span!("turn", scope = %label).in_scope(|| {
            tracing::warn!(
                event = daemon::RUN_REFUSED,
                reason = err.code(),
                "the Launcher could not run; the Turn is refused"
            )
        });
        self.notices
            .send(
                Notice::new(NoticeKind::RunRefused, "Run refused", text.clone())
                    .with_scope(scope.clone()),
            )
            .await;
        ChatOutcome {
            kind: OutcomeKind::Refused,
            text,
        }
    }

    fn plan_failed(&self, label: &str, failure: &PlanFailure) -> ChatOutcome {
        tracing::info_span!("turn", scope = %label).in_scope(|| {
            tracing::warn!(
                event = daemon::RUN_REFUSED,
                reason = failure.code(),
                "the Turn could not be planned"
            )
        });
        ChatOutcome {
            kind: OutcomeKind::Failed,
            text: failure.message().to_owned(),
        }
    }

    fn start_failed(&self, label: &str, err: &PipelineError) -> ChatOutcome {
        tracing::info_span!("turn", scope = %label)
            .in_scope(|| tracing::error!(err = %err, "the Turn could not start"));
        ChatOutcome {
            kind: OutcomeKind::Failed,
            text: "The run could not be started.".to_owned(),
        }
    }

    fn plan(&self, turn: &Turn, session_id: Option<&str>) -> Result<PlannedRun, PlanFailure> {
        self.planner.plan(&RunRequestInput {
            turn: turn.clone(),
            session_id: session_id.map(str::to_owned),
        })
    }

    /// Runs one attempt: writes the System-prompt file, spawns the run, forwards its events to
    /// `events`, and waits for the outcome and then the settle.
    async fn run_attempt(
        &self,
        input: &AttemptInput<'_>,
        events: &mut Option<mpsc::Sender<RunEvent>>,
    ) -> Result<Attempt, PipelineError> {
        let run_id = self.next_id("run");
        let started_at = self.clock.now();
        let span = tracing::info_span!(
            "run",
            run_id = %run_id,
            scope = %input.scope_label,
            turn_id = %input.turn_id,
            attempt = input.attempt,
            launcher = %input.launcher_key,
        );
        let clock_start = tokio::time::Instant::now();
        // Re-arms the Turn beacon for this run (a Turn arms it on every run).
        let _beacon = self.health.turn(input.scope, input.timeout);

        let system_prompt_file = self.layout.tmp_dir().join(format!("asp-{run_id}.md"));
        self.write_system_prompt(&system_prompt_file, input.system_prompt)
            .await?;

        let args = input
            .base_args
            .clone()
            .append_system_prompt_file(system_prompt_file.clone());
        let argv = args.to_argv().map_err(PipelineError::Argv)?;
        let env = self.run_env.child_env(&RunVars {
            claudeclaw_dir: self.layout.root(),
            run_id: &run_id,
            launcher: input.launcher,
        });

        if let Some(api) = input.api_scope {
            let entry = RegistryEntry {
                run_id: run_id.clone(),
                state: RegistryState::Spawning,
                pid: None,
                stamp: None,
                kill: KillMethod::ProcessGroup,
                purpose: input.spawn_purpose,
                scope: api.clone(),
                origin: Some(input.origin.clone()),
                started_at,
            };
            if let Err(err) = self.runs.register_spawning(entry).await {
                tracing::warn!(err = %err, "the run could not be entered in the Run registry");
            }
        }

        span.in_scope(|| {
            tracing::info!(
                event = daemon::RUN_START,
                session = session_str(input.session),
                session_id = input.session_id.unwrap_or(""),
                model = input.model.unwrap_or(""),
                purpose = purpose_str(input.purpose),
                "the run started"
            )
        });

        let spec = SpawnSpec {
            run_id: run_id.clone(),
            program: input.program.to_path_buf(),
            args: argv,
            env,
            cwd: input.cwd.to_path_buf(),
            stdin: input.stdin_prompt.as_bytes().to_vec(),
            timeout: input.timeout,
            system_prompt_file: Some(system_prompt_file),
            result_probe: None,
        };
        let mut handle = start(
            &self.spawner,
            RunRequest {
                spawn: spec,
                args: args.clone(),
            },
        )
        .map_err(PipelineError::Spawn)?;

        let mut outcome: Option<RunOutcome> = None;
        let mut settle: Option<Settle> = None;
        let mut outcome_done = false;
        let mut settle_done = false;
        let mut events_done = false;
        let mut tool_uses: u64 = 0;
        loop {
            if outcome_done && settle_done {
                break;
            }
            tokio::select! {
                biased;
                result = &mut handle.outcome, if !outcome_done => {
                    outcome_done = true;
                    if let Ok(judged) = result {
                        outcome = Some(judged);
                    }
                }
                result = &mut handle.settled, if !settle_done => {
                    settle_done = true;
                    if let Ok(settled) = result {
                        settle = Some(settled);
                    }
                }
                event = handle.events.recv(), if !events_done => {
                    match event {
                        Some(RunnerEvent::Stream(stream)) => {
                            if let Some(chat_event) = chat_event(stream) {
                                if matches!(chat_event, RunEvent::ToolUse { .. }) {
                                    tool_uses += 1;
                                }
                                if let Some(tx) = events.as_ref() {
                                    let _ = tx.try_send(chat_event);
                                }
                            }
                        }
                        None => events_done = true,
                    }
                }
            }
        }

        let outcome = outcome.ok_or(PipelineError::OutcomeLost)?;
        let ended_at = self.clock.now();
        let duration_ms = clock_start.elapsed().as_millis() as u64;
        let session_id = outcome
            .session_id
            .clone()
            .or_else(|| input.session_id.map(str::to_owned));

        span.in_scope(|| {
            tracing::info!(
                event = daemon::RUN_FINISH,
                outcome = outcome_str(&outcome),
                exit_code = ?outcome.exit_code,
                is_error = ?outcome.is_error,
                api_error_status = ?outcome.api_error_status,
                duration_ms = duration_ms,
                result_bytes = settle.as_ref().map(|s| s.stdout.bytes).unwrap_or(0),
                stderr_bytes = settle.as_ref().map(|s| s.stderr.bytes).unwrap_or(0),
                tool_uses = tool_uses,
                tokens_in = outcome.usage.map(|u| u.input_tokens).unwrap_or(0),
                tokens_out = outcome.usage.map(|u| u.output_tokens).unwrap_or(0),
                cost_usd = ?outcome.total_cost_usd,
                purpose = purpose_str(input.purpose),
                awake_ms = outcome.clock.awake_ms,
                suspended_ms = outcome.clock.suspended_ms,
                wake_grace = outcome.clock.wake_grace,
                claude_code_version = outcome
                    .claude_code_version
                    .as_ref()
                    .map(|v| v.raw.as_str())
                    .unwrap_or(""),
                "the run finished"
            )
        });
        if let Some(settled) = &settle {
            span.in_scope(|| {
                tracing::info!(
                    event = daemon::RUN_SETTLE,
                    run_id = %run_id,
                    stragglers = settled.stragglers,
                    outcome = settled.outcome.as_str(),
                    pipe_held = settled.pipe_held,
                    settle_ms = settled.settle_ms,
                    "the run settled"
                )
            });
        }

        Ok(Attempt {
            outcome,
            settle,
            run_id,
            started_at,
            ended_at,
            duration_ms,
            session_id,
        })
    }

    /// Records a finished attempt against its session and appends its Run-index record; a
    /// non-compact attempt also writes a Run log. Best effort: a failure is logged, never fatal.
    async fn record_attempt(&self, attempt: &Attempt, input: &AttemptInput<'_>) {
        let Some(api) = input.api_scope else {
            return;
        };
        let now = self.clock.now();
        if input.purpose == RunPurpose::Compact {
            let _ = self
                .sessions
                .record_compact(
                    api,
                    input.launcher_key,
                    now,
                    index_outcome(&attempt.outcome),
                    attempt.duration_ms,
                )
                .await;
            self.write_index(attempt, input, None, None).await;
            return;
        }
        let recorded = match self
            .sessions
            .record_run(api, input.launcher_key, &attempt.outcome, now)
            .await
        {
            Ok(recorded) => Some(recorded),
            Err(err) => {
                tracing::warn!(err = %err, "the run could not be recorded against its session");
                None
            }
        };
        let run_log = self.write_run_log(attempt, input).await;
        self.write_index(attempt, input, recorded.as_ref(), run_log)
            .await;
    }

    async fn write_run_log(&self, attempt: &Attempt, input: &AttemptInput<'_>) -> Option<String> {
        let api = input.api_scope?;
        let log = match RunLog::create(&self.layout, &attempt.run_id, attempt.started_at) {
            Ok(log) => log,
            Err(err) => {
                tracing::warn!(err = %err, "the Run log name was refused");
                return None;
            }
        };
        let rel = log.rel().to_owned();
        let body = RunLogBody {
            date: attempt.ended_at,
            session_id: attempt.session_id.clone(),
            session: input.session,
            fallback: false,
            routing: None,
            prompt: input.stdin_prompt.to_owned(),
            exit_code: attempt.outcome.exit_code,
            output: attempt.outcome.reply.clone().unwrap_or_default(),
            stderr: String::new(),
            run: Some(RunSection {
                run_id: attempt.run_id.clone(),
                scope: api.clone(),
                origin: RunOrigin::Discord,
                launcher: input.launcher_name.to_owned(),
                attempt: input.attempt,
                outcome: index_outcome(&attempt.outcome),
                duration_ms: attempt.duration_ms,
            }),
            noise: Vec::new(),
        };
        match tokio::task::spawn_blocking(move || log.write(&body, &[])).await {
            Ok(Ok(())) => Some(rel),
            Ok(Err(err)) => {
                tracing::warn!(err = %err, "the Run log could not be written");
                None
            }
            Err(err) => {
                tracing::warn!(err = %err, "the Run log task did not finish");
                None
            }
        }
    }

    async fn write_index(
        &self,
        attempt: &Attempt,
        input: &AttemptInput<'_>,
        recorded: Option<&RunRecorded>,
        run_log: Option<String>,
    ) {
        let record = build_record(attempt, input, recorded, run_log);
        let index = Arc::clone(&self.run_index);
        match tokio::task::spawn_blocking(move || index.append(&record)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(err = %err, "the Run-index record could not be appended");
            }
            Err(err) => {
                tracing::warn!(err = %err, "the Run-index append task did not finish");
            }
        }
    }

    async fn write_system_prompt(&self, path: &Path, text: &str) -> Result<(), PipelineError> {
        let layout = self.layout.clone();
        let path = path.to_path_buf();
        let text = text.to_owned();
        tokio::task::spawn_blocking(move || {
            layout.create_daemon_dirs()?;
            write_atomic(&path, text.as_bytes(), FileMode::Private)
        })
        .await
        .map_err(PipelineError::Join)?
        .map_err(PipelineError::SystemPrompt)
    }

    fn next_id(&self, prefix: &str) -> String {
        let n = self.ids.fetch_add(1, Ordering::Relaxed);
        let ms = self.clock.now().timestamp_millis();
        format!("{prefix}-{ms:x}-{n:x}")
    }
}

/// One attempt's spawn inputs, borrowed from the Turn so it can be reused between attempts.
struct AttemptInput<'a> {
    scope: &'a Scope,
    scope_label: &'a str,
    turn_id: &'a str,
    attempt: u32,
    purpose: RunPurpose,
    spawn_purpose: SpawnPurpose,
    origin: ChatOrigin,
    api_scope: Option<&'a ApiScope>,
    program: &'a Path,
    cwd: &'a Path,
    launcher: &'a Launcher,
    launcher_key: &'a str,
    launcher_name: &'a str,
    launcher_resolved: &'a str,
    session_id: Option<&'a str>,
    session: RunSession,
    stdin_prompt: &'a str,
    system_prompt: &'a str,
    base_args: &'a ClaudeArgs,
    timeout: Duration,
    model: Option<&'a str>,
}

/// What one finished attempt produced, for the record.
struct Attempt {
    outcome: RunOutcome,
    settle: Option<Settle>,
    run_id: String,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    duration_ms: u64,
    session_id: Option<String>,
}

fn build_record(
    attempt: &Attempt,
    input: &AttemptInput<'_>,
    recorded: Option<&RunRecorded>,
    run_log: Option<String>,
) -> RunRecord {
    let outcome = &attempt.outcome;
    let (cost_usd, cost_status) = match recorded {
        Some(recorded) if recorded.cost.priced => (recorded.cost.cost_usd, CostStatus::Priced),
        _ if matches!(outcome.verdict, Verdict::Refused(_)) => (None, CostStatus::Refused),
        _ if outcome.total_cost_usd.is_some() => (None, CostStatus::NoBaseline),
        _ => (None, CostStatus::NoUsage),
    };
    let usage = outcome.usage.as_ref();
    RunRecord {
        run_id: attempt.run_id.clone(),
        turn_id: Some(input.turn_id.to_owned()),
        name: attempt.run_id.clone(),
        scope: input.api_scope.cloned(),
        origin: RunOrigin::Discord,
        launcher: Some(input.launcher_name.to_owned()),
        job: None,
        agent: None,
        session_id: attempt.session_id.clone(),
        session: Some(input.session),
        model: input.model.map(str::to_owned),
        started_at: attempt.started_at,
        ended_at: Some(attempt.ended_at),
        duration_ms: Some(attempt.duration_ms),
        outcome: Some(index_outcome(outcome)),
        exit_code: outcome.exit_code,
        is_error: outcome.is_error,
        api_error_status: outcome.api_error_status,
        attempt: input.attempt,
        state: None,
        tokens_in: usage.map(|u| u.input_tokens),
        tokens_out: usage.map(|u| u.output_tokens),
        cache_read_tokens: usage.map(|u| u.cache_read_input_tokens),
        cache_write_tokens: usage.map(|u| u.cache_creation_input_tokens),
        cost_usd,
        cost_status,
        cost_basis: None,
        provider: None,
        api_key_source: outcome.api_key_source.clone(),
        models: Vec::new(),
        run_log,
        launcher_resolved: Some(input.launcher_resolved.to_owned()),
        purpose: Some(input.purpose),
        settle: attempt.settle.as_ref().map(settle_record),
        awake_ms: Some(outcome.clock.awake_ms),
        suspended_ms: Some(outcome.clock.suspended_ms),
        wake_grace: outcome.clock.wake_grace,
        claude_code_version: outcome.claude_code_version.as_ref().map(|v| v.raw.clone()),
        lingered: outcome.lingered,
    }
}

/// The Scope's label for spans and the health beacon: the API wire form when it has one, else a
/// `transport:id` / `job:name` / `agent:name` form.
pub(crate) fn scope_label(scope: &Scope) -> String {
    if let Ok(api) = scope.to_api() {
        return api.to_string();
    }
    match scope {
        Scope::Global => "global".to_owned(),
        Scope::Channel { transport, id } | Scope::Thread { transport, id, .. } => {
            format!("{transport}:{id}")
        }
        Scope::Job(name) => format!("job:{name}"),
        Scope::Agent(name) => format!("agent:{name}"),
    }
}

/// The `direct` text or the Launcher's absolute path, the (Scope, Launcher) session key.
fn launcher_key(launcher: &Launcher) -> String {
    match launcher {
        Launcher::Direct => "direct".to_owned(),
        Launcher::Path(path) => path.as_path().to_string_lossy().into_owned(),
    }
}

/// The `direct` text or the Launcher's file name, for the Run record and Run log.
fn launcher_file_name(launcher: &Launcher) -> String {
    match launcher {
        Launcher::Direct => "direct".to_owned(),
        Launcher::Path(path) => path
            .as_path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.as_path().to_string_lossy().into_owned()),
    }
}

/// The run's cwd: `agents/<agent>` for a hired thread, else the project dir.
fn run_cwd(layout: &Layout, turn: &Turn) -> PathBuf {
    match &turn.agent_dir {
        Some(dir) => layout.project_dir().join(dir),
        None => layout.project_dir().to_path_buf(),
    }
}

fn session_of(session_id: &Option<String>) -> RunSession {
    if session_id.is_some() {
        RunSession::Resumed
    } else {
        RunSession::New
    }
}

fn session_str(session: RunSession) -> &'static str {
    match session {
        RunSession::New => "new",
        RunSession::Resumed => "resumed",
        RunSession::Unknown => "unknown",
    }
}

fn purpose_str(purpose: RunPurpose) -> &'static str {
    match purpose {
        RunPurpose::Turn => "turn",
        RunPurpose::Compact => "compact",
        RunPurpose::CompactRetry => "compact_retry",
        RunPurpose::Unknown => "unknown",
    }
}

fn outcome_str(outcome: &RunOutcome) -> &'static str {
    match outcome.verdict {
        Verdict::Ok => "ok",
        Verdict::Timeout => "timeout",
        Verdict::Interrupted => "interrupted",
        Verdict::RateLimited { .. } => "rate_limited",
        Verdict::Failed(_) | Verdict::Refused(_) => "failed",
    }
}

fn index_outcome(outcome: &RunOutcome) -> IndexOutcome {
    match outcome.verdict {
        Verdict::Ok => IndexOutcome::Ok,
        Verdict::Timeout => IndexOutcome::Timeout,
        Verdict::Interrupted => IndexOutcome::Interrupted,
        Verdict::RateLimited { .. } => IndexOutcome::RateLimited,
        Verdict::Failed(_) | Verdict::Refused(_) => IndexOutcome::Failed,
    }
}

fn settle_record(settle: &Settle) -> RunSettle {
    RunSettle {
        stragglers: settle.stragglers,
        outcome: match settle.outcome {
            claudeclaw_runner::spawn::SettleOutcome::Clean => ApiSettleOutcome::Clean,
            claudeclaw_runner::spawn::SettleOutcome::Exited => ApiSettleOutcome::Exited,
            claudeclaw_runner::spawn::SettleOutcome::Terminated => ApiSettleOutcome::Terminated,
            claudeclaw_runner::spawn::SettleOutcome::Killed => ApiSettleOutcome::Killed,
            claudeclaw_runner::spawn::SettleOutcome::Survived => ApiSettleOutcome::Survived,
        },
        pipe_held: settle.pipe_held,
        settle_ms: settle.settle_ms,
    }
}

/// Maps a runner stream event onto the adapter's [`RunEvent`]. Subagent chatter is dropped — the
/// adapter shows the main thread only — and the events with no adapter meaning (session id,
/// version, transcript hint) too.
fn chat_event(event: StreamEvent) -> Option<RunEvent> {
    match event {
        StreamEvent::TextDelta { text, subagent } => {
            (!subagent).then_some(RunEvent::TextDelta(text))
        }
        StreamEvent::ToolUse {
            name,
            summary,
            subagent,
            ..
        } => (!subagent).then_some(RunEvent::ToolUse { name, summary }),
        StreamEvent::ToolResult { name, is_error, .. } => Some(RunEvent::ToolResult {
            name: name.unwrap_or_else(|| "tool".to_owned()),
            ok: !is_error,
        }),
        StreamEvent::AgentSpawn { id, description } => {
            Some(RunEvent::AgentSpawn { id, description })
        }
        StreamEvent::AgentDone { id, .. } => Some(RunEvent::AgentDone { id }),
        _ => None,
    }
}

/// The user-facing outcome: the reply on success, or a short, secret-free message on a failure.
/// `note` (what Auto-compact did, #33) is appended to a failure only.
fn chat_outcome(outcome: &RunOutcome, note: Option<&str>) -> ChatOutcome {
    let suffix = || note.map(|note| format!(" {note}")).unwrap_or_default();
    match &outcome.verdict {
        Verdict::Ok => ChatOutcome {
            kind: OutcomeKind::Ok,
            text: outcome.reply.clone().unwrap_or_default(),
        },
        Verdict::Refused(refusal) => ChatOutcome {
            kind: OutcomeKind::Refused,
            text: format!("The run was refused ({}).", refusal.code()),
        },
        Verdict::Interrupted => ChatOutcome {
            kind: OutcomeKind::Interrupted,
            text: "The run was interrupted; please resend your message.".to_owned(),
        },
        Verdict::RateLimited { .. } => ChatOutcome {
            kind: OutcomeKind::Failed,
            text: format!("The subscription limit was hit.{}", suffix()),
        },
        Verdict::Timeout => ChatOutcome {
            kind: OutcomeKind::Failed,
            text: format!("The run timed out.{}", suffix()),
        },
        Verdict::Failed(failure) => ChatOutcome {
            kind: OutcomeKind::Failed,
            text: format!("The run failed ({}).{}", failure.code(), suffix()),
        },
    }
}

fn compact_note_text(outcome: &RunOutcome) -> String {
    match outcome.verdict {
        Verdict::Ok => "Auto-compact ran before the Turn was re-run.".to_owned(),
        _ => "Auto-compact was attempted before the Turn was re-run.".to_owned(),
    }
}

/// On graceful shutdown, sends #9's "interrupted, please resend" reply for every in-flight chat
/// Turn by calling the adapter's [`Notifier::interrupted`] once per origin (#9, #79). Returns how
/// many calls succeeded. The composition root collects the in-flight origins (the Run registry's
/// `origin` values) and calls this per adapter.
pub async fn interrupt_in_flight(
    notifier: &dyn Notifier,
    origins: impl IntoIterator<Item = MessageRef>,
) -> usize {
    let mut sent = 0;
    for origin in origins {
        if notifier.interrupted(origin).await.is_ok() {
            sent += 1;
        }
    }
    sent
}

/// After a crash, sends the same reply for every run a Reap proved dead that still carries a chat
/// origin (#29, #79). A run that was `settling` when the Worker died already had its reply, and a
/// run whose fate is unknown is left alone (#37 D3). Returns how many calls succeeded.
pub async fn interrupt_reaped(notifier: &dyn Notifier, reaped: &[ReapedRun]) -> usize {
    let mut sent = 0;
    for run in reaped {
        if run.state == RegistryState::Settling {
            continue;
        }
        if !matches!(
            run.outcome,
            ReapOutcome::Killed | ReapOutcome::AlreadyGone | ReapOutcome::NotOurs
        ) {
            continue;
        }
        let Some(origin) = &run.origin else {
            continue;
        };
        let message = MessageRef {
            channel: origin.channel.clone(),
            message: origin.message.clone(),
        };
        if notifier.interrupted(message).await.is_ok() {
            sent += 1;
        }
    }
    sent
}

/// Test scaffolding shared by the `pipeline` and `queue` tests: a temp-dir [`Layout`], a fake
/// `claude` on the Run PATH, recording notices, a movable wall clock and a fake [`Planner`].
#[cfg(all(test, unix))]
pub(crate) mod test_support {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use claudeclaw_api::runs::RunRecord;
    use claudeclaw_api::scope::Scope as ApiScope;
    use claudeclaw_api::supervisor::{ChatOrigin, ReapOutcome, RegistryState, SpawnPurpose};
    use claudeclaw_chat::{
        Author, ChatScope, MessageRef, Notifier, NotifyError, OwnerNotice, Transport,
    };
    use claudeclaw_runner::RunPath;
    use claudeclaw_runner::argv::ClaudeArgs;
    use claudeclaw_runner::clock::AwakeTime;
    use claudeclaw_runner::spawn::SpawnTimings;
    use claudeclaw_scheduler::WallClock;
    use claudeclaw_store::Layout;
    use futures_util::future::BoxFuture;
    use tempfile::TempDir;
    use tokio::sync::watch;

    use super::{
        HealthMonitor, LauncherSettings, Notices, Pipeline, PipelineConfig, PlanFailure,
        PlannedRun, Planner, RunIndex, RunRegistry, RunRequestInput, SessionManager, Spawner, Turn,
    };
    use crate::events::EventBus;
    use crate::sessions::SessionPolicy;

    /// A `result` line with a reply, the shape the stream parser reads (runner `HAPPY`).
    pub(crate) const HAPPY: &str = r#"printf '%s\n' \
    '{"type":"system","subtype":"init","session_id":"s-1"}' \
    '{"type":"result","subtype":"success","is_error":false,"result":"hello there","session_id":"s-1"}'"#;

    /// Writes an executable fake `claude` script into `dir`.
    fn write_script(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("claude");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    /// Short waits so a settle and a straggler kill happen in well under a second.
    fn timings() -> SpawnTimings {
        SpawnTimings {
            term_grace: Duration::from_millis(100),
            straggler_grace: Duration::from_millis(100),
            kill_wait: Duration::from_millis(200),
            pipe_cap: Duration::from_millis(100),
            poll: Duration::from_millis(10),
        }
    }

    fn awake(ms: u64) -> AwakeTime {
        AwakeTime {
            awake_ms: ms,
            ..AwakeTime::default()
        }
    }

    /// A wall clock a test can read; it is fixed, so records do not depend on the current time.
    pub(crate) struct TestClock(Mutex<DateTime<Utc>>);

    impl TestClock {
        fn arc() -> Arc<Self> {
            Arc::new(TestClock(Mutex::new(
                DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            )))
        }
    }

    impl WallClock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    /// A [`Planner`] that shapes a minimal run: the Turn text on stdin, a fixed System prompt and
    /// `--resume` only when a session id is given.
    pub(crate) struct FakePlanner {
        pub(crate) timeout: Duration,
    }

    impl Planner for FakePlanner {
        fn plan(&self, request: &RunRequestInput) -> Result<PlannedRun, PlanFailure> {
            let mut args = ClaudeArgs::new();
            if let Some(session_id) = &request.session_id {
                args = args.resume(session_id.clone());
            }
            Ok(PlannedRun {
                stdin_prompt: request.turn.text.clone(),
                system_prompt: "you are a test".to_owned(),
                args,
                timeout: self.timeout,
                model: Some("test-model".to_owned()),
            })
        }
    }

    /// A [`Notifier`] that records what it was asked to send.
    #[derive(Default)]
    pub(crate) struct RecordingNotifier {
        notices: Mutex<Vec<OwnerNotice>>,
        interrupted: Mutex<Vec<MessageRef>>,
    }

    impl RecordingNotifier {
        pub(crate) fn notices(&self) -> Vec<OwnerNotice> {
            self.notices.lock().unwrap().clone()
        }

        pub(crate) fn interrupted(&self) -> Vec<MessageRef> {
            self.interrupted.lock().unwrap().clone()
        }
    }

    impl Notifier for RecordingNotifier {
        fn notify_owners(&self, notice: OwnerNotice) -> BoxFuture<'_, Result<(), NotifyError>> {
            Box::pin(async move {
                self.notices.lock().unwrap().push(notice);
                Ok(())
            })
        }

        fn interrupted(&self, origin: MessageRef) -> BoxFuture<'_, Result<(), NotifyError>> {
            Box::pin(async move {
                self.interrupted.lock().unwrap().push(origin);
                Ok(())
            })
        }
    }

    /// A built pipeline over a temp dir, with the pieces a test needs to poke at.
    pub(crate) struct Harness {
        pub(crate) dir: TempDir,
        pub(crate) layout: Layout,
        pub(crate) sessions: Arc<SessionManager>,
        pub(crate) clock_tx: watch::Sender<AwakeTime>,
        pub(crate) pipeline: Arc<Pipeline>,
        pub(crate) recorder: Arc<RecordingNotifier>,
        pub(crate) bin_dir: PathBuf,
        pub(crate) marker: PathBuf,
        pub(crate) calls: PathBuf,
    }

    impl Harness {
        /// Writes the fake `claude` this harness resolves.
        pub(crate) fn write_claude(&self, body: &str) {
            write_script(&self.bin_dir, body);
        }

        /// Writes a fake `claude` into a dir that is **not** on the Run PATH.
        pub(crate) fn write_unreachable_claude(&self, body: &str) {
            write_script(&self.dir.path().join("off-path"), body);
        }
    }

    async fn build(timeout: Duration, on_path: bool) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.create_daemon_dirs().unwrap();
        let bin_dir = dir.path().join("bin");
        let marker = dir.path().join("marker");
        let calls = dir.path().join("calls.log");

        let notices = Arc::new(Notices::in_memory(EventBus::new()));
        let recorder = Arc::new(RecordingNotifier::default());
        notices.register(Arc::clone(&recorder) as Arc<dyn Notifier>);

        let sessions = Arc::new(
            SessionManager::open(&layout, Arc::clone(&notices), SessionPolicy::default())
                .await
                .unwrap(),
        );
        let runs = RunRegistry::open(&layout).await.unwrap();
        let health = Arc::new(HealthMonitor::new());
        let (clock_tx, clock_rx) = watch::channel(AwakeTime::default());
        let spawner = Spawner::new(clock_rx)
            .with_hook(Arc::new(runs.clone()))
            .with_timings(timings());
        let run_env = claudeclaw_runner::env::RunEnv {
            path: if on_path {
                // Prepend the fake-Launcher dir, keep the inherited PATH so the script's own
                // `cat`/`sleep` still resolve (#26).
                let inherited = std::env::var_os("PATH");
                RunPath::compose(std::slice::from_ref(&bin_dir), None, inherited.as_deref()).path
            } else {
                RunPath::default()
            },
            lc_ctype: None,
        };
        let clock = TestClock::arc();
        let planner = Arc::new(FakePlanner { timeout });
        let pipeline = Arc::new(Pipeline::new(PipelineConfig {
            layout: layout.clone(),
            notices: Arc::clone(&notices),
            sessions: Arc::clone(&sessions),
            runs,
            health: Arc::clone(&health),
            spawner,
            planner,
            launchers: LauncherSettings::default(),
            run_env,
            run_index: Arc::new(RunIndex::new(layout.clone())),
            clock,
        }));
        Harness {
            dir,
            layout,
            sessions,
            clock_tx,
            pipeline,
            recorder,
            bin_dir,
            marker,
            calls,
        }
    }

    /// A harness whose Run PATH holds the fake `claude`.
    pub(crate) async fn harness(timeout: Duration) -> Harness {
        build(timeout, true).await
    }

    /// A harness whose Run PATH is empty, so `direct` cannot resolve.
    pub(crate) async fn harness_without_launcher(timeout: Duration) -> Harness {
        build(timeout, false).await
    }

    /// Pushes the Awake clock to `ms`, which times out any run whose deadline has passed.
    pub(crate) fn timeout_awake(tx: &watch::Sender<AwakeTime>, ms: u64) {
        tx.send_replace(awake(ms));
    }

    /// A Discord channel Turn; `123` is a fake snowflake, never a real id (§2).
    pub(crate) fn turn(text: &str, channel: &str) -> Turn {
        Turn {
            scope: ChatScope::Channel {
                transport: Transport::Discord,
                id: channel.to_owned(),
            },
            origin: MessageRef {
                channel: channel.to_owned(),
                message: "m-1".to_owned(),
            },
            author: Author {
                display_name: "owner".to_owned(),
            },
            text: text.to_owned(),
            context_lines: Vec::new(),
            attachments: Vec::new(),
            agent_dir: None,
        }
    }

    pub(crate) fn message(channel: &str, message: &str) -> MessageRef {
        MessageRef {
            channel: channel.to_owned(),
            message: message.to_owned(),
        }
    }

    pub(crate) fn origin(channel: &str, message: &str) -> ChatOrigin {
        ChatOrigin {
            adapter: claudeclaw_api::adapter::Transport::Discord,
            channel: channel.to_owned(),
            message: message.to_owned(),
        }
    }

    pub(crate) fn reaped(
        run_id: &str,
        state: RegistryState,
        outcome: ReapOutcome,
        origin: Option<ChatOrigin>,
    ) -> claudeclaw_api::supervisor::ReapedRun {
        claudeclaw_api::supervisor::ReapedRun {
            run_id: run_id.to_owned(),
            scope: ApiScope::Discord("123".to_owned()),
            origin,
            purpose: SpawnPurpose::Turn,
            state,
            outcome,
        }
    }

    /// Every Run-index record under `layout`, oldest first.
    pub(crate) fn read_index(layout: &Layout) -> Vec<RunRecord> {
        let mut records = Vec::new();
        let Ok(entries) = std::fs::read_dir(layout.run_index_dir()) else {
            return records;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            for line in text.lines() {
                if let Ok(record) = serde_json::from_str::<RunRecord>(line) {
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|record| record.started_at);
        records
    }

    /// The recorded lines of a run's `calls` log, oldest first.
    pub(crate) fn read_calls(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Waits (real time) for `path` to exist, up to `limit`.
    pub(crate) async fn wait_for(path: &Path, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        while tokio::time::Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        path.exists()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::DateTime;

    use super::test_support::{
        RecordingNotifier, harness, harness_without_launcher, message, origin, read_index, reaped,
        turn, wait_for,
    };
    use super::{IndexOutcome, OutcomeKind, interrupt_in_flight, interrupt_reaped};
    use claudeclaw_api::runs::{RunPurpose, RunSession};
    use claudeclaw_api::scope::Scope as ApiScope;
    use claudeclaw_api::supervisor::{ReapOutcome, RegistryState};
    use claudeclaw_store::SessionEntry;

    fn seeded() -> SessionEntry {
        SessionEntry::new(
            ApiScope::Discord("123".to_owned()),
            "direct",
            "s-old",
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        )
    }

    #[tokio::test]
    async fn a_launcher_that_cannot_run_is_refused_with_no_spawn_and_a_notice() {
        let h = harness_without_launcher(Duration::from_secs(5)).await;
        // A script that would leave a marker if it ever ran.
        h.write_unreachable_claude(&format!("touch '{}'", h.marker.display()));

        let outcome = h.pipeline.run_turn(turn("hi", "123"), None).await;

        assert_eq!(outcome.kind, OutcomeKind::Refused);
        assert!(!h.marker.exists(), "the refused Launcher must never run");
        assert!(
            read_index(&h.layout).is_empty(),
            "a refused run has no record"
        );
        let kinds: Vec<String> = h
            .recorder
            .notices()
            .iter()
            .map(|notice| notice.kind.clone())
            .collect();
        assert!(
            kinds.iter().any(|kind| kind == "run_refused"),
            "expected a run.refused notice, got {kinds:?}"
        );
    }

    #[tokio::test]
    async fn a_stale_session_gets_exactly_one_new_session_retry() {
        let h = harness(Duration::from_secs(5)).await;
        h.sessions.upsert(seeded()).await.unwrap();
        let counter = h.dir.path().join("count");
        h.write_claude(&format!(
            "n=$(cat '{count}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{count}'\n\
             if [ \"$n\" -eq 1 ]; then\n\
               printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"num_turns\":0,\"errors\":[\"No conversation found with session ID: s-old\"]}}'\n\
             else\n\
               printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"fresh\",\"session_id\":\"s-new\"}}'\n\
             fi",
            count = counter.display()
        ));

        let outcome = h.pipeline.run_turn(turn("hi", "123"), None).await;

        assert_eq!(outcome.kind, OutcomeKind::Ok);
        assert_eq!(outcome.text, "fresh");
        let records = read_index(&h.layout);
        assert_eq!(records.len(), 2, "exactly one retry");
        assert_eq!(records[0].attempt, 1);
        assert_eq!(records[1].attempt, 2);
        assert_eq!(records[0].purpose, Some(RunPurpose::Turn));
        assert_eq!(records[1].purpose, Some(RunPurpose::Turn));
        assert_eq!(records[0].session, Some(RunSession::Resumed));
        assert_eq!(records[1].session, Some(RunSession::New));
    }

    #[tokio::test]
    async fn a_resumed_session_timeout_runs_compact_then_one_rerun() {
        let h = harness(Duration::from_secs(1)).await;
        h.sessions.upsert(seeded()).await.unwrap();
        h.write_claude(&format!(
            "prompt=$(cat)\n\
             printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s-1\"}}'\n\
             case \"$prompt\" in\n\
               /compact)\n\
                 printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"compacted\",\"session_id\":\"s-1\"}}'\n\
                 ;;\n\
               *)\n\
                 if [ ! -f '{marker}' ]; then : > '{marker}'; exec sleep 60; fi\n\
                 printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"recovered\",\"session_id\":\"s-1\"}}'\n\
                 ;;\n\
             esac",
            marker = h.marker.display()
        ));

        let pipeline = Arc::clone(&h.pipeline);
        let call = tokio::spawn(async move { pipeline.run_turn(turn("hi", "123"), None).await });

        assert!(
            wait_for(&h.marker, Duration::from_secs(5)).await,
            "the first run should reach the marker before the timeout"
        );
        // The first run is parked on `sleep 60`; time it out on the Awake clock.
        super::test_support::timeout_awake(&h.clock_tx, 1_000);

        let outcome = call.await.unwrap();
        assert_eq!(
            outcome.kind,
            OutcomeKind::Ok,
            "the retried Turn should succeed"
        );
        assert_eq!(outcome.text, "recovered");

        let records = read_index(&h.layout);
        assert_eq!(records.len(), 3, "turn, compact, retry");
        assert_eq!(records[0].attempt, 1);
        assert_eq!(records[0].purpose, Some(RunPurpose::Turn));
        assert_eq!(records[1].attempt, 2);
        assert_eq!(records[1].purpose, Some(RunPurpose::Compact));
        assert_eq!(records[2].attempt, 3);
        assert_eq!(records[2].purpose, Some(RunPurpose::CompactRetry));
        assert_eq!(
            records[0].outcome,
            Some(IndexOutcome::Timeout),
            "the first run timed out"
        );
    }

    #[tokio::test]
    async fn interrupted_replies_go_to_the_origins_of_in_flight_turns() {
        let recorder = Arc::new(RecordingNotifier::default());
        let notifier: &dyn claudeclaw_chat::Notifier = recorder.as_ref();

        let sent = interrupt_in_flight(notifier, [message("c1", "m1"), message("c2", "m2")]).await;

        assert_eq!(sent, 2);
        assert_eq!(
            recorder.interrupted(),
            vec![message("c1", "m1"), message("c2", "m2")]
        );
    }

    #[tokio::test]
    async fn reaped_replies_skip_settling_and_unproved_runs() {
        let recorder = Arc::new(RecordingNotifier::default());
        let notifier: &dyn claudeclaw_chat::Notifier = recorder.as_ref();

        let reaped_runs = vec![
            // Proved dead with an origin: reply.
            reaped(
                "run-a",
                RegistryState::Running,
                ReapOutcome::Killed,
                Some(origin("c3", "m3")),
            ),
            // Was settling: it already had its reply (#37 D3).
            reaped(
                "run-b",
                RegistryState::Settling,
                ReapOutcome::Killed,
                Some(origin("c4", "m4")),
            ),
            // Fate unknown: leave it alone.
            reaped(
                "run-c",
                RegistryState::Running,
                ReapOutcome::Unverified,
                Some(origin("c5", "m5")),
            ),
            // Proved dead but sessionless: no origin.
            reaped(
                "run-d",
                RegistryState::Running,
                ReapOutcome::AlreadyGone,
                None,
            ),
            // Proved dead with an origin: reply.
            reaped(
                "run-e",
                RegistryState::Running,
                ReapOutcome::NotOurs,
                Some(origin("c6", "m6")),
            ),
        ];

        let sent = interrupt_reaped(notifier, &reaped_runs).await;

        assert_eq!(sent, 2);
        assert_eq!(
            recorder.interrupted(),
            vec![message("c3", "m3"), message("c6", "m6")]
        );
    }
}
