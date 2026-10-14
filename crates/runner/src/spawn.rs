//! Spawning one run: its process group or Job object, the prompt on stdin, capped output, the
//! Awake-time timeout and the Settle (architecture §4 `claudeclaw-runner` → `spawn`; #25, #26,
//! #36, #37, #38, #47).
//!
//! [`Spawner::spawn`] starts a [`SpawnSpec`] and returns a [`Run`], whose events arrive in this
//! order: stdout/stderr lines as they are read, [`SpawnEvent::LeaderExited`] when the leader
//! exits (the reply can go out), and finally [`SpawnEvent::Settled`] once the run's process group
//! (Job object on Windows) is empty and both pipes are closed (the Scope's next Turn may start).
//! [`SpawnEvent::StdoutEof`] comes whenever stdout reaches EOF, before `Settled`.
//!
//! **Seam with `stream`.** This module never parses stream-json. The caller feeds each
//! [`SpawnEvent::Stdout`] line to the stream parser and judges the run (#37 D2 step 2) once it has
//! the [`LeaderExit`] and either a `result` event or [`SpawnEvent::StdoutEof`]; otherwise at
//! [`SpawnEvent::Settled`], when no more lines can arrive. Whether a `result` was read before the
//! timeout (#47 D3) is decided here, so it can't race the kill: the caller passes a cheap
//! [`ResultProbe`] that the stdout reader applies to each line as it is read, and
//! [`LeaderExit::lingered`] reports the outcome.
//!
//! **Seam with `env`.** [`SpawnSpec::env`] is the list of changes the `env` module yields; the
//! Worker's own environment is inherited and never mutated.
//!
//! **Seam with `core::runs`.** A [`SpawnHook`] sees the pid and start stamp right after the spawn,
//! the leader's exit and the settle, which is what the Run registry needs (#29 D2, #37 D3).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::bytes::BytesMut;
use tokio_util::codec::{Decoder, LinesCodec, LinesCodecError};
use tracing::Instrument;

use crate::clock::{AwakeTime, RunDeadline, millis};
use crate::reap::{self, ProcessStamp};

/// The stdout and the stderr cap, each (§4 `spawn`). Past it the pipe is still drained, so the
/// child never blocks, but nothing more is forwarded (TS parity: `MAX_OUTPUT_BYTES` and
/// `collectStream`, `src/runner.ts:372-395`). It is also the longest line `LinesCodec` accepts.
pub const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024;

/// The Settle's and the timeout kill's waits (#37 D2, #47 D4). Not configurable (#37 D1);
/// [`Default`] is the spec's and tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnTimings {
    /// SIGTERM → SIGKILL, on the timeout path and in the Settle.
    pub term_grace: Duration,
    /// How long Stragglers may exit on their own after a normal leader exit.
    pub straggler_grace: Duration,
    /// SIGKILL → `survived`.
    pub kill_wait: Duration,
    /// How long the pipes may stay open once the group is gone (`pipe_held` after that).
    pub pipe_cap: Duration,
    /// How often an emptying group is probed.
    pub poll: Duration,
}

impl Default for SpawnTimings {
    fn default() -> Self {
        Self {
            term_grace: Duration::from_secs(5),
            straggler_grace: Duration::from_secs(2),
            kill_wait: Duration::from_secs(5),
            pipe_cap: Duration::from_secs(2),
            poll: Duration::from_millis(50),
        }
    }
}

/// Tells whether a stdout line is a stream-json `result` event (#47 D3). The `stream` module
/// provides it; it must be cheap and pure.
pub type ResultProbe = fn(&str) -> bool;

/// What to spawn (§4 `spawn`, #25 D1).
pub struct SpawnSpec {
    /// The run's `run_id`, used only to correlate logs and [`SpawnHook`] calls.
    pub run_id: String,
    /// The resolved, absolute executable: the Launcher, or `claude` for `direct` (#26).
    pub program: PathBuf,
    /// Short, single-line tokens only (#25 D1).
    pub args: Vec<OsString>,
    /// Env changes on top of the inherited environment: set, or `None` to remove (`env`).
    pub env: Vec<(OsString, Option<OsString>)>,
    pub cwd: PathBuf,
    /// The prompt, written to stdin, which is then closed (#25 D1).
    pub stdin: Vec<u8>,
    /// The run timeout, in awake time (#38 D1).
    pub timeout: Duration,
    /// A per-run System-prompt file, deleted at the settle (#25 D2, #37 D2 step 7).
    pub system_prompt_file: Option<PathBuf>,
    /// See [`ResultProbe`]. Without one, [`LeaderExit::lingered`] is always false.
    pub result_probe: Option<ResultProbe>,
}

impl fmt::Debug for SpawnSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Content-free (§2): no prompt, no env values.
        f.debug_struct("SpawnSpec")
            .field("run_id", &self.run_id)
            .field("args", &self.args.len())
            .field("env", &self.env.len())
            .field("stdin_bytes", &self.stdin.len())
            .field("timeout", &self.timeout)
            .field("system_prompt_file", &self.system_prompt_file.is_some())
            .field("result_probe", &self.result_probe.is_some())
            .finish_non_exhaustive()
    }
}

/// Lets `core::runs` keep the Run registry (#29 D2, #37 D3). `core` writes the `spawning` entry
/// before calling [`Spawner::spawn`]; the hook then marks it `running`, `settling` and removed.
/// The calls are synchronous and must not block: hand the work to a task or a channel.
pub trait SpawnHook: Send + Sync {
    /// The child was spawned, and not yet waited, so `pid` can't have been reused (#29 D2 steps
    /// 2–4). `stamp` is `None` when it couldn't be read.
    fn spawned(&self, _run_id: &str, _pid: u32, _stamp: Option<&ProcessStamp>) {}
    /// The leader exited; the run is `settling` (#37 D3).
    fn leader_exited(&self, _run_id: &str) {}
    /// The run is settled; remove its entry (#37 D2 step 7).
    fn settled(&self, _run_id: &str) {}
}

/// Why a run could not be started. None of them carries the program's path or arguments (#8).
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("the program path is not absolute")]
    RelativeProgram,
    /// The final argv guard (#25 D1): an argument held CR, LF or NUL.
    #[error("argument {index} holds CR, LF or NUL")]
    ArgvUnsafe { index: usize },
    #[error("the process could not be started")]
    Spawn(#[source] io::Error),
}

/// One event of a [`Run`]. See the module docs for the order.
pub enum SpawnEvent {
    /// A stdout line, without its line ending.
    Stdout(String),
    /// A stderr line, without its line ending.
    Stderr(String),
    /// Stdout reached EOF: no more [`SpawnEvent::Stdout`].
    StdoutEof,
    LeaderExited(LeaderExit),
    /// The last event.
    Settled(Settle),
}

impl fmt::Debug for SpawnEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Content-free (§2): lines print as sizes.
        match self {
            Self::Stdout(l) => write!(f, "Stdout({} bytes)", l.len()),
            Self::Stderr(l) => write!(f, "Stderr({} bytes)", l.len()),
            Self::StdoutEof => f.write_str("StdoutEof"),
            Self::LeaderExited(e) => f.debug_tuple("LeaderExited").field(e).finish(),
            Self::Settled(s) => f.debug_tuple("Settled").field(s).finish(),
        }
    }
}

/// The leader's exit (§4 `spawn`). The run's outcome and reply go out at this point (#37 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderExit {
    /// The exit code; `None` when a signal ended it, or when it couldn't be waited.
    pub code: Option<i32>,
    /// Unix: the signal that ended it.
    pub signal: Option<i32>,
    /// The awake-time timeout fired and the group was killed (#38 D1).
    pub timed_out: bool,
    /// The [`ResultProbe`] matched a stdout line before the leader exited.
    pub result_seen: bool,
    /// The timeout fired after a `result` had been read: judge by that `result`, not as a
    /// timeout (#47 D3). `run.lingered` was logged.
    pub lingered: bool,
    /// Awake time the run took (#38 D6).
    pub awake_ms: u64,
    /// Resume gaps during the run (#38 D6).
    pub suspended_ms: u64,
    /// A Wake grace moved the deadline (#38 D6).
    pub wake_grace: bool,
    /// Wall time from spawn to leader exit.
    pub wall_ms: u64,
}

impl LeaderExit {
    /// Exit 126 or 127: the Launcher couldn't start what it wraps. With no JSON line on stdout
    /// the stream worker makes it a Refused run, `launcher_exec_failed` (#26 D3).
    pub fn exec_failed(&self) -> bool {
        matches!(self.code, Some(126 | 127))
    }
}

/// How the Stragglers ended (#37 D4); mirrors `api::runs::SettleOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettleOutcome {
    /// Nothing was left at leader exit.
    Clean,
    /// They exited within the grace.
    Exited,
    Terminated,
    Killed,
    Survived,
}

impl SettleOutcome {
    /// The wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Exited => "exited",
            Self::Terminated => "terminated",
            Self::Killed => "killed",
            Self::Survived => "survived",
        }
    }
}

/// Byte and line counts for one pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputStats {
    /// Bytes read, including any past the cap.
    pub bytes: u64,
    /// Lines forwarded.
    pub lines: u64,
    /// [`MAX_OUTPUT_BYTES`] was reached; later output was drained and dropped.
    pub capped: bool,
    /// Lines dropped for being longer than the cap or not UTF-8.
    pub dropped_lines: u64,
}

/// The settle (#37 D4), reported after the leader's exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settle {
    /// Group members (Job descendants on Windows, best effort) alive at leader exit.
    pub stragglers: u32,
    pub outcome: SettleOutcome,
    /// Something outside the group held a pipe past the cap; its read end was closed.
    pub pipe_held: bool,
    /// From leader exit to settled.
    pub settle_ms: u64,
    pub stdout: OutputStats,
    pub stderr: OutputStats,
    /// The whole prompt reached the child's stdin (`run.stdin_write_failed` otherwise, #25 D1).
    pub stdin_written: bool,
}

/// Spawns runs against one Awake clock (#38 D2).
#[derive(Clone)]
pub struct Spawner {
    clock: watch::Receiver<AwakeTime>,
    hook: Option<Arc<dyn SpawnHook>>,
    timings: SpawnTimings,
}

impl fmt::Debug for Spawner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Spawner")
            .field("hook", &self.hook.is_some())
            .field("timings", &self.timings)
            .finish_non_exhaustive()
    }
}

impl Spawner {
    /// A spawner on the Awake clock's receiver ([`crate::clock::AwakeClock::subscribe`]). Tests
    /// pass a `watch` channel they drive by hand.
    pub fn new(clock: watch::Receiver<AwakeTime>) -> Self {
        Self {
            clock,
            hook: None,
            timings: SpawnTimings::default(),
        }
    }

    /// Adds the Run registry hook.
    pub fn with_hook(mut self, hook: Arc<dyn SpawnHook>) -> Self {
        self.hook = Some(hook);
        self
    }

    /// Other waits, for tests.
    pub fn with_timings(mut self, timings: SpawnTimings) -> Self {
        self.timings = timings;
        self
    }

    /// Spawns `spec` in its own process group (Unix) or kill-on-close Job object (Windows) and
    /// supervises it to the settle in a background task. Must be called inside a tokio runtime.
    pub fn spawn(&self, spec: SpawnSpec) -> Result<Run, SpawnError> {
        if !spec.program.is_absolute() {
            return Err(SpawnError::RelativeProgram);
        }
        if let Some(index) = spec.args.iter().position(|a| unsafe_arg(a)) {
            return Err(SpawnError::ArgvUnsafe { index });
        }
        let SpawnSpec {
            run_id,
            program,
            args,
            env,
            cwd,
            stdin,
            timeout,
            system_prompt_file,
            result_probe,
        } = spec;

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &env {
            match value {
                Some(v) => cmd.env(key, v),
                None => cmd.env_remove(key),
            };
        }
        let mut child = wrap_command(cmd).spawn().map_err(SpawnError::Spawn)?;
        let pid = child.id().expect("a child that was never waited has a pid");
        let stamp = match reap::process_stamp(pid) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::debug!(err = %e, "the run's start stamp couldn't be read");
                None
            }
        };
        if let Some(hook) = &self.hook {
            hook.spawned(&run_id, pid, stamp.as_ref());
        }

        let span = tracing::Span::current();
        let (tx, rx) = mpsc::unbounded_channel();
        let result_at: Arc<OnceLock<Instant>> = Arc::default();
        let stdin_task = child
            .stdin()
            .take()
            .map(|pipe| tokio::spawn(write_stdin(pipe, stdin).instrument(span.clone())));
        let stdout = child.stdout().take().map(|pipe| {
            Reader::spawn(
                pipe,
                Pipe::Stdout,
                tx.clone(),
                result_probe,
                result_at.clone(),
                &span,
            )
        });
        let stderr = child.stderr().take().map(|pipe| {
            Reader::spawn(
                pipe,
                Pipe::Stderr,
                tx.clone(),
                None,
                result_at.clone(),
                &span,
            )
        });

        let supervised = Supervised {
            run_id,
            child,
            pid,
            #[cfg(windows)]
            stamp: stamp.clone(),
            clock: self.clock.clone(),
            timeout,
            timings: self.timings,
            hook: self.hook.clone(),
            tx,
            result_at,
            stdin_task,
            stdout,
            stderr,
            system_prompt_file,
        };
        tokio::spawn(supervised.run().instrument(span));
        Ok(Run {
            pid,
            stamp,
            events: rx,
        })
    }
}

/// A running run. Dropping it doesn't stop the run: it is still supervised to its settle.
#[derive(Debug)]
pub struct Run {
    pid: u32,
    stamp: Option<ProcessStamp>,
    events: mpsc::UnboundedReceiver<SpawnEvent>,
}

impl Run {
    /// The leader's pid; on Unix also the pgid.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The leader's OS start stamp, if it could be read.
    pub fn stamp(&self) -> Option<&ProcessStamp> {
        self.stamp.as_ref()
    }

    /// The next event; `None` after [`SpawnEvent::Settled`].
    pub async fn next_event(&mut self) -> Option<SpawnEvent> {
        self.events.recv().await
    }
}

/// The wrapper stack every run uses (#36 E1): `ProcessGroup::leader()` on Unix; on Windows a
/// `JobObject` plus `CREATE_NO_WINDOW` (#25 D3). `KillOnDrop` is required on Windows, where it is
/// what makes the Job object kill-on-close (#29 D1), and kills the leader during unwinding on
/// Unix (#37 D5).
pub(crate) fn wrap_command(cmd: Command) -> CommandWrap {
    let mut wrap = CommandWrap::from(cmd);
    #[cfg(unix)]
    wrap.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    {
        wrap.wrap(JobObject);
        // After `JobObject`, whose `pre_spawn` replaces the creation flags.
        wrap.wrap(NoWindow);
    }
    wrap.wrap(KillOnDrop);
    wrap
}

/// Adds `CREATE_NO_WINDOW` (#25 D3). Registered after `JobObject`: `JobObject::pre_spawn` sets
/// the flags to `CREATE_SUSPENDED` and tokio's `creation_flags` replaces rather than adds, so
/// this keeps `CREATE_SUSPENDED` (the job resumes the child after assigning it).
#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct NoWindow;

#[cfg(windows)]
impl process_wrap::tokio::CommandWrapper for NoWindow {
    fn pre_spawn(&mut self, command: &mut Command, _core: &CommandWrap) -> io::Result<()> {
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        Ok(())
    }
}

fn unsafe_arg(arg: &OsStr) -> bool {
    arg.as_encoded_bytes()
        .iter()
        .any(|b| matches!(b, b'\r' | b'\n' | b'\0'))
}

/// The leader itself, below every wrapper. Its `wait()` is how leader exit is detected, never
/// the wrapper's (#37 D2 step 1).
fn leader(child: &mut Box<dyn ChildWrapper>) -> &mut Child {
    // SAFETY: the wrappers' only state that depends on who reaps the leader is their cached exit
    // status, used by their own `wait`/`try_wait`, which this module never calls.
    unsafe { child.try_inner_child_mut() }.expect("the wrapper stack ends in a tokio Child")
}

async fn write_stdin(mut pipe: ChildStdin, prompt: Vec<u8>) -> bool {
    let written = async {
        pipe.write_all(&prompt).await?;
        pipe.shutdown().await
    }
    .await;
    drop(pipe);
    match written {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                event = "run.stdin_write_failed",
                err = %e,
                "the child closed stdin before the prompt was written"
            );
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pipe {
    Stdout,
    Stderr,
}

struct Reader {
    stats: Arc<Mutex<OutputStats>>,
    task: JoinHandle<()>,
}

impl Reader {
    fn spawn<R: AsyncRead + Unpin + Send + 'static>(
        pipe: R,
        which: Pipe,
        tx: mpsc::UnboundedSender<SpawnEvent>,
        probe: Option<ResultProbe>,
        result_at: Arc<OnceLock<Instant>>,
        span: &tracing::Span,
    ) -> Self {
        let stats = Arc::new(Mutex::new(OutputStats::default()));
        let task = tokio::spawn(
            read_lines(pipe, which, tx, probe, result_at, stats.clone()).instrument(span.clone()),
        );
        Self { stats, task }
    }

    fn stats(&self) -> OutputStats {
        *self.stats.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for EOF until `deadline`, then closes the read end. Returns whether it had to.
    async fn finish_by(&mut self, deadline: Instant) -> bool {
        if tokio::time::timeout_at(deadline, &mut self.task)
            .await
            .is_ok()
        {
            return false;
        }
        self.task.abort();
        let _ = (&mut self.task).await;
        true
    }
}

async fn read_lines<R: AsyncRead + Unpin>(
    mut pipe: R,
    which: Pipe,
    tx: mpsc::UnboundedSender<SpawnEvent>,
    probe: Option<ResultProbe>,
    result_at: Arc<OnceLock<Instant>>,
    stats: Arc<Mutex<OutputStats>>,
) {
    let mut codec = LinesCodec::new_with_max_length(MAX_OUTPUT_BYTES);
    let mut buf = BytesMut::with_capacity(16 * 1024);
    let mut forwarded = 0usize;
    let mut capped = false;
    loop {
        // A read error ends the pipe like EOF.
        let n = pipe.read_buf(&mut buf).await.unwrap_or(0);
        let eof = n == 0;
        let mut s = OutputStats {
            bytes: n as u64,
            ..OutputStats::default()
        };
        if capped {
            buf.clear();
        } else {
            loop {
                let item = if eof {
                    codec.decode_eof(&mut buf)
                } else {
                    codec.decode(&mut buf)
                };
                match item {
                    Ok(Some(line)) => {
                        forwarded += line.len() + 1;
                        if forwarded > MAX_OUTPUT_BYTES {
                            capped = true;
                            buf.clear();
                            break;
                        }
                        if which == Pipe::Stdout && probe.is_some_and(|p| p(&line)) {
                            let _ = result_at.set(Instant::now());
                        }
                        s.lines += 1;
                        let _ = tx.send(match which {
                            Pipe::Stdout => SpawnEvent::Stdout(line),
                            Pipe::Stderr => SpawnEvent::Stderr(line),
                        });
                    }
                    Ok(None) => break,
                    Err(LinesCodecError::MaxLineLengthExceeded | LinesCodecError::Io(_)) => {
                        s.dropped_lines += 1;
                    }
                }
            }
        }
        {
            let mut total = stats.lock().unwrap_or_else(PoisonError::into_inner);
            total.bytes += s.bytes;
            total.lines += s.lines;
            total.dropped_lines += s.dropped_lines;
            total.capped = capped;
        }
        if eof {
            break;
        }
    }
    if which == Pipe::Stdout {
        let _ = tx.send(SpawnEvent::StdoutEof);
    }
}

struct Supervised {
    run_id: String,
    child: Box<dyn ChildWrapper>,
    pid: u32,
    /// The leader's creation time anchors the descendant walk.
    #[cfg(windows)]
    stamp: Option<ProcessStamp>,
    clock: watch::Receiver<AwakeTime>,
    timeout: Duration,
    timings: SpawnTimings,
    hook: Option<Arc<dyn SpawnHook>>,
    tx: mpsc::UnboundedSender<SpawnEvent>,
    result_at: Arc<OnceLock<Instant>>,
    stdin_task: Option<JoinHandle<bool>>,
    stdout: Option<Reader>,
    stderr: Option<Reader>,
    system_prompt_file: Option<PathBuf>,
}

impl Supervised {
    async fn run(mut self) {
        let started = Instant::now();
        let mut deadline = RunDeadline::new(*self.clock.borrow_and_update(), self.timeout);
        let mut clock_alive = true;

        // The leader runs until it exits or its awake-time deadline passes (#38 D2).
        let waited = loop {
            let child = leader(&mut self.child);
            tokio::select! {
                status = child.wait() => break Some(status),
                changed = self.clock.changed(), if clock_alive => {
                    if changed.is_err() {
                        clock_alive = false;
                    } else if deadline.observe(*self.clock.borrow_and_update()) {
                        break None;
                    }
                }
            }
        };

        let (status, kill_started, lingered) = match waited {
            Some(status) => (status, None, false),
            None => {
                let killed_at = Instant::now();
                let result_at = self.result_at.get().copied();
                if let Some(at) = result_at {
                    tracing::warn!(
                        event = "run.lingered",
                        run_id = %self.run_id,
                        linger_ms = millis(killed_at.saturating_duration_since(at)),
                        "the run's process stayed after its result; judged by that result"
                    );
                }
                (
                    self.kill_on_timeout().await,
                    Some(killed_at),
                    result_at.is_some(),
                )
            }
        };

        let exited_at = Instant::now();
        let now = *self.clock.borrow();
        let (code, signal) = match &status {
            Ok(s) => exit_parts(s),
            Err(e) => {
                tracing::warn!(err = %e, "waiting for the run's leader failed");
                (None, None)
            }
        };
        let exit = LeaderExit {
            code,
            signal,
            timed_out: kill_started.is_some(),
            result_seen: self.result_at.get().is_some(),
            lingered,
            awake_ms: deadline.awake_ms(now),
            suspended_ms: deadline.suspended_ms(now),
            wake_grace: deadline.wake_grace(),
            wall_ms: millis(started.elapsed()),
        };
        if let Some(hook) = &self.hook {
            hook.leader_exited(&self.run_id);
        }
        let _ = self.tx.send(SpawnEvent::LeaderExited(exit));

        let stragglers = self.stragglers();
        let outcome = self.settle_group(stragglers, kill_started).await;

        // The pipe cap (#37 D2 step 6).
        let pipe_deadline = Instant::now() + self.timings.pipe_cap;
        let mut pipe_held = false;
        for reader in [self.stdout.as_mut(), self.stderr.as_mut()]
            .into_iter()
            .flatten()
        {
            pipe_held |= reader.finish_by(pipe_deadline).await;
        }
        let stdin_written = match self.stdin_task.take() {
            Some(task) if task.is_finished() => task.await.unwrap_or(false),
            Some(task) => {
                task.abort();
                false
            }
            None => false,
        };
        if let Some(path) = self.system_prompt_file.take() {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(err = %e, "the System-prompt file couldn't be deleted"),
            }
        }

        let settle = Settle {
            stragglers,
            outcome,
            pipe_held,
            settle_ms: millis(exited_at.elapsed()),
            stdout: self.stdout.as_ref().map(Reader::stats).unwrap_or_default(),
            stderr: self.stderr.as_ref().map(Reader::stats).unwrap_or_default(),
            stdin_written,
        };
        log_settle(&self.run_id, &settle);
        if let Some(hook) = &self.hook {
            hook.settled(&self.run_id);
        }
        // Dropping the child closes the Job handle on Windows, which ends anything still in it.
        drop(self.child);
        let _ = self.tx.send(SpawnEvent::Settled(settle));
    }

    /// The timeout kill: TERM the group, wait up to `term_grace` for the leader, then KILL the
    /// group (#47 D4: TERM before KILL). Windows ends the Job object at once (#25 D3).
    async fn kill_on_timeout(&mut self) -> io::Result<ExitStatus> {
        self.terminate_group();
        if let Ok(status) =
            tokio::time::timeout(self.timings.term_grace, leader(&mut self.child).wait()).await
        {
            return status;
        }
        self.kill_group();
        leader(&mut self.child).wait().await
    }

    #[cfg(unix)]
    fn terminate_group(&mut self) {
        if let Err(e) = reap::signal_group(self.pid, nix::sys::signal::Signal::SIGTERM) {
            tracing::debug!(err = %e, "SIGTERM to the run's group failed");
        }
    }

    #[cfg(unix)]
    fn kill_group(&mut self) {
        if let Err(e) = reap::signal_group(self.pid, nix::sys::signal::Signal::SIGKILL) {
            tracing::debug!(err = %e, "SIGKILL to the run's group failed");
        }
    }

    #[cfg(windows)]
    fn terminate_group(&mut self) {
        // `TerminateJobObject`; there is no TERM for console-less processes.
        if let Err(e) = self.child.start_kill() {
            tracing::debug!(err = %e, "ending the run's Job object failed");
        }
    }

    #[cfg(windows)]
    fn kill_group(&mut self) {
        self.terminate_group();
    }

    /// The group's members at leader exit (#37 D2 step 3).
    #[cfg(unix)]
    fn stragglers(&self) -> u32 {
        u32::try_from(reap::group_members(self.pid).len()).unwrap_or(u32::MAX)
    }

    #[cfg(unix)]
    fn group_alive(&self) -> bool {
        reap::group_alive(self.pid)
    }

    /// The leader's live descendants at leader exit, best effort (#37 D2 step 3).
    #[cfg(windows)]
    fn stragglers(&self) -> u32 {
        u32::try_from(self.descendants().len()).unwrap_or(u32::MAX)
    }

    #[cfg(windows)]
    fn group_alive(&self) -> bool {
        !self.descendants().is_empty()
    }

    #[cfg(windows)]
    fn descendants(&self) -> Vec<u32> {
        let created = match &self.stamp {
            Some(ProcessStamp::Windows { creation_time }) => *creation_time,
            _ => 0,
        };
        reap::win::descendants(self.pid, created)
    }

    /// Polls until the group is empty or `deadline` passes; returns whether it emptied.
    async fn empty_by(&self, deadline: Instant) -> bool {
        loop {
            if !self.group_alive() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(self.timings.poll).await;
        }
    }

    /// The Settle's group sequence (#37 D2 steps 4–5). After a timeout kill the TERM has already
    /// been sent at `kill_started`, so only its remaining grace and the KILL are left.
    async fn settle_group(
        &mut self,
        stragglers: u32,
        kill_started: Option<Instant>,
    ) -> SettleOutcome {
        let t = self.timings;
        if !self.group_alive() {
            return match (stragglers, kill_started) {
                (0, _) => SettleOutcome::Clean,
                (_, None) => SettleOutcome::Exited,
                (_, Some(_)) => SettleOutcome::Terminated,
            };
        }
        match kill_started {
            None => {
                if self.empty_by(Instant::now() + t.straggler_grace).await {
                    return SettleOutcome::Exited;
                }
                if cfg!(unix) {
                    self.terminate_group();
                    if self.empty_by(Instant::now() + t.term_grace).await {
                        return SettleOutcome::Terminated;
                    }
                }
            }
            Some(at) => {
                if cfg!(unix) && self.empty_by(at + t.term_grace).await {
                    return if stragglers == 0 {
                        SettleOutcome::Clean
                    } else {
                        SettleOutcome::Terminated
                    };
                }
            }
        }
        self.kill_group();
        if self.empty_by(Instant::now() + t.kill_wait).await {
            SettleOutcome::Killed
        } else {
            SettleOutcome::Survived
        }
    }
}

fn exit_parts(status: &ExitStatus) -> (Option<i32>, Option<i32>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        (status.code(), status.signal())
    }
    #[cfg(not(unix))]
    {
        (status.code(), None)
    }
}

/// `run.settle` at #37 D4's levels.
fn log_settle(run_id: &str, s: &Settle) {
    macro_rules! settle {
        ($level:ident) => {
            tracing::$level!(
                event = "run.settle",
                run_id,
                stragglers = s.stragglers,
                outcome = s.outcome.as_str(),
                pipe_held = s.pipe_held,
                settle_ms = s.settle_ms,
                "the run settled"
            )
        };
    }
    match s.outcome {
        SettleOutcome::Survived => settle!(error),
        SettleOutcome::Killed => settle!(warn),
        _ if s.pipe_held => settle!(warn),
        SettleOutcome::Exited | SettleOutcome::Terminated => settle!(info),
        SettleOutcome::Clean => settle!(debug),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn the_wrapper_stack_has_kill_on_drop() {
        // #36 E1: without `KillOnDrop` the Job object isn't kill-on-close on Windows.
        let wrap = wrap_command(Command::new("x"));
        assert!(wrap.has_wrap::<KillOnDrop>());
        #[cfg(unix)]
        assert!(wrap.has_wrap::<ProcessGroup>());
        #[cfg(windows)]
        {
            assert!(wrap.has_wrap::<JobObject>());
            assert!(wrap.has_wrap::<NoWindow>());
        }
    }

    #[test]
    fn content_types_debug_without_content() {
        let spec = SpawnSpec {
            run_id: "r1".into(),
            program: "/x".into(),
            args: vec!["--print".into()],
            env: vec![("K".into(), Some("secret-value".into()))],
            cwd: "/".into(),
            stdin: b"the prompt".to_vec(),
            timeout: Duration::from_secs(1),
            system_prompt_file: None,
            result_probe: None,
        };
        let shown = format!("{spec:?} {:?}", SpawnEvent::Stdout("the reply".into()));
        assert!(!shown.contains("secret-value"), "{shown}");
        assert!(!shown.contains("the prompt"), "{shown}");
        assert!(!shown.contains("the reply"), "{shown}");
        assert!(shown.contains("Stdout(9 bytes)"), "{shown}");
    }

    #[test]
    fn exit_126_and_127_are_exec_failures() {
        let exit = |code| LeaderExit {
            code,
            signal: None,
            timed_out: false,
            result_seen: false,
            lingered: false,
            awake_ms: 0,
            suspended_ms: 0,
            wake_grace: false,
            wall_ms: 0,
        };
        assert!(exit(Some(126)).exec_failed());
        assert!(exit(Some(127)).exec_failed());
        assert!(!exit(Some(1)).exec_failed());
        assert!(!exit(None).exec_failed());
    }

    pub(super) fn fast() -> SpawnTimings {
        SpawnTimings {
            term_grace: Duration::from_millis(400),
            straggler_grace: Duration::from_millis(200),
            kill_wait: Duration::from_secs(2),
            pipe_cap: Duration::from_millis(300),
            poll: Duration::from_millis(20),
        }
    }

    pub(super) fn spec(program: &Path, args: &[&str]) -> SpawnSpec {
        SpawnSpec {
            run_id: "r1".into(),
            program: program.to_path_buf(),
            args: args.iter().map(OsString::from).collect(),
            env: Vec::new(),
            cwd: std::env::temp_dir(),
            stdin: Vec::new(),
            timeout: Duration::from_secs(60),
            system_prompt_file: None,
            result_probe: None,
        }
    }

    /// Every event of a run, up to the settle.
    #[derive(Default)]
    pub(super) struct Seen {
        pub(super) stdout: Vec<String>,
        pub(super) stderr: Vec<String>,
        pub(super) order: Vec<&'static str>,
        pub(super) exit: Option<LeaderExit>,
        pub(super) settle: Option<Settle>,
    }

    impl Seen {
        pub(super) fn exit(&self) -> &LeaderExit {
            self.exit.as_ref().expect("the leader exited")
        }

        pub(super) fn settle(&self) -> &Settle {
            self.settle.as_ref().expect("the run settled")
        }

        fn push(&mut self, event: SpawnEvent) -> bool {
            match event {
                SpawnEvent::Stdout(l) => self.stdout.push(l),
                SpawnEvent::Stderr(l) => self.stderr.push(l),
                SpawnEvent::StdoutEof => self.order.push("eof"),
                SpawnEvent::LeaderExited(e) => {
                    self.order.push("exit");
                    self.exit = Some(e);
                }
                SpawnEvent::Settled(s) => {
                    self.order.push("settled");
                    self.settle = Some(s);
                    return true;
                }
            }
            false
        }
    }

    /// Reads events until the settle, or `limit` of wall time.
    pub(super) async fn drain(run: &mut Run, seen: &mut Seen, limit: Duration) {
        tokio::time::timeout(limit, async {
            while let Some(event) = run.next_event().await {
                if seen.push(event) {
                    return;
                }
            }
        })
        .await
        .expect("the run settled in time");
        assert!(run.next_event().await.is_none(), "nothing after Settled");
    }

    /// Reads events until a stdout line equals `line`.
    #[cfg(unix)]
    pub(super) async fn until_line(run: &mut Run, seen: &mut Seen, line: &str) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = run.next_event().await {
                let hit = matches!(&event, SpawnEvent::Stdout(l) if l == line);
                assert!(!seen.push(event), "settled before the line");
                if hit {
                    return;
                }
            }
        })
        .await
        .expect("the line arrived in time");
    }

    /// Reads events for `wait`; returns whether the leader exited meanwhile.
    pub(super) async fn exited_within(run: &mut Run, seen: &mut Seen, wait: Duration) -> bool {
        tokio::time::timeout(wait, async {
            while let Some(event) = run.next_event().await {
                seen.push(event);
                if seen.exit.is_some() {
                    return;
                }
            }
        })
        .await
        .is_ok()
    }

    pub(super) fn awake(ms: u64) -> AwakeTime {
        AwakeTime {
            awake_ms: ms,
            ..AwakeTime::default()
        }
    }

    #[cfg(unix)]
    #[derive(Default)]
    pub(super) struct Recorder(pub(super) Mutex<Vec<String>>);

    #[cfg(unix)]
    impl SpawnHook for Recorder {
        fn spawned(&self, run_id: &str, pid: u32, stamp: Option<&ProcessStamp>) {
            let has_stamp = stamp.is_some();
            self.0
                .lock()
                .unwrap()
                .push(format!("spawned {run_id} {pid} {has_stamp}"));
        }
        fn leader_exited(&self, run_id: &str) {
            self.0
                .lock()
                .unwrap()
                .push(format!("leader_exited {run_id}"));
        }
        fn settled(&self, run_id: &str) {
            self.0.lock().unwrap().push(format!("settled {run_id}"));
        }
    }

    #[tokio::test]
    async fn bad_specs_are_refused_before_spawning() {
        let (_tx, rx) = watch::channel(AwakeTime::default());
        let spawner = Spawner::new(rx);
        let err = spawner
            .spawn(spec(Path::new("relative/claude"), &[]))
            .unwrap_err();
        assert!(matches!(err, SpawnError::RelativeProgram));
        for bad in ["a\nb", "a\rb", "a\0b"] {
            let abs = std::env::temp_dir().join("claude");
            let err = spawner.spawn(spec(&abs, &["--print", bad])).unwrap_err();
            assert!(matches!(err, SpawnError::ArgvUnsafe { index: 1 }));
        }
        let missing = std::env::temp_dir().join("claudeclaw-no-such-launcher");
        let err = spawner.spawn(spec(&missing, &[])).unwrap_err();
        assert!(matches!(err, SpawnError::Spawn(_)));
        assert!(!err.to_string().contains("claudeclaw-no-such-launcher"));
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::path::Path;

    use super::tests::*;
    use super::*;

    const SH: &str = "/bin/sh";
    const LIMIT: Duration = Duration::from_secs(20);

    fn sh(script: &str) -> SpawnSpec {
        spec(Path::new(SH), &["-c", script])
    }

    fn spawner() -> (watch::Sender<AwakeTime>, Spawner) {
        let (tx, rx) = watch::channel(AwakeTime::default());
        (tx, Spawner::new(rx).with_timings(fast()))
    }

    async fn run_to_settle(spawner: &Spawner, spec: SpawnSpec) -> Seen {
        let mut run = spawner.spawn(spec).unwrap();
        let mut seen = Seen::default();
        drain(&mut run, &mut seen, LIMIT).await;
        seen
    }

    #[tokio::test]
    async fn the_prompt_reaches_stdin_and_the_run_settles_clean() {
        let (_clock, spawner) = spawner();
        let mut s = sh("cat; echo err >&2");
        s.stdin = "line one\nline two\n".into();
        let seen = run_to_settle(&spawner, s).await;
        assert_eq!(seen.stdout, ["line one", "line two"]);
        assert_eq!(seen.stderr, ["err"]);
        assert_eq!(seen.exit().code, Some(0));
        assert!(!seen.exit().timed_out);
        let settle = seen.settle();
        assert_eq!(settle.outcome, SettleOutcome::Clean);
        assert_eq!(settle.stragglers, 0);
        assert!(!settle.pipe_held);
        assert!(settle.stdin_written);
        assert_eq!(settle.stdout.lines, 2);
        assert_eq!(*seen.order.last().unwrap(), "settled");
        assert!(seen.order.contains(&"eof"));
    }

    #[tokio::test]
    async fn a_prompt_larger_than_the_pipe_buffer_is_delivered() {
        let (_clock, spawner) = spawner();
        let mut s = sh("wc -c");
        s.stdin = vec![b'x'; 1 << 20];
        let seen = run_to_settle(&spawner, s).await;
        assert_eq!(seen.stdout.len(), 1);
        assert_eq!(seen.stdout[0].trim(), "1048576");
        assert!(seen.settle().stdin_written);
    }

    #[tokio::test]
    async fn a_child_that_never_reads_stdin_fails_the_write_not_the_run() {
        let (_clock, spawner) = spawner();
        let mut s = sh("exit 0");
        s.stdin = vec![b'x'; 4 << 20];
        let seen = run_to_settle(&spawner, s).await;
        assert_eq!(seen.exit().code, Some(0));
        assert!(!seen.settle().stdin_written);
    }

    #[tokio::test]
    async fn env_changes_are_applied() {
        let (_clock, spawner) = spawner();
        let mut s = sh("echo \"$CLAUDECLAW_TEST_SET/${HOME-unset}\"");
        s.env = vec![
            ("CLAUDECLAW_TEST_SET".into(), Some("yes".into())),
            ("HOME".into(), None),
        ];
        let seen = run_to_settle(&spawner, s).await;
        assert_eq!(seen.stdout, ["yes/unset"]);
    }

    #[tokio::test]
    async fn output_past_the_cap_is_drained_and_dropped() {
        let (_clock, spawner) = spawner();
        // 12 MB of 20-byte lines.
        let seen = run_to_settle(&spawner, sh("yes 0123456789012345678 | head -c 12000000")).await;
        let stats = seen.settle().stdout;
        assert!(stats.capped);
        assert_eq!(stats.bytes, 12_000_000);
        let forwarded: usize = seen.stdout.iter().map(|l| l.len() + 1).sum();
        assert!(forwarded <= MAX_OUTPUT_BYTES);
        assert!(forwarded > MAX_OUTPUT_BYTES - 64);
        assert_eq!(seen.exit().code, Some(0));
    }

    #[tokio::test]
    async fn exit_126_and_127_are_reported() {
        let (_clock, spawner) = spawner();
        for code in [126, 127] {
            let seen = run_to_settle(&spawner, sh(&format!("exit {code}"))).await;
            assert_eq!(seen.exit().code, Some(code));
            assert!(seen.exit().exec_failed());
        }
    }

    #[tokio::test]
    async fn the_timeout_counts_awake_time_not_wall_time() {
        let (clock, spawner) = spawner();
        let mut s = sh("echo started; exec sleep 30");
        s.timeout = Duration::from_secs(1);
        let mut run = spawner.spawn(s).unwrap();
        let mut seen = Seen::default();
        until_line(&mut run, &mut seen, "started").await;
        // More than the timeout of wall time passes with no awake time: no kill.
        assert!(!exited_within(&mut run, &mut seen, Duration::from_millis(1_500)).await);
        clock.send_replace(awake(1_000));
        drain(&mut run, &mut seen, LIMIT).await;
        let exit = seen.exit();
        assert!(exit.timed_out);
        assert!(!exit.lingered);
        assert_eq!(exit.signal, Some(libc::SIGTERM));
        assert_eq!(exit.awake_ms, 1_000);
    }

    #[tokio::test]
    async fn a_resume_gives_the_run_its_wake_grace() {
        let (clock, spawner) = spawner();
        let mut s = sh("echo started; exec sleep 30");
        s.timeout = Duration::from_secs(1);
        let mut run = spawner.spawn(s).unwrap();
        let mut seen = Seen::default();
        until_line(&mut run, &mut seen, "started").await;
        clock.send_replace(AwakeTime {
            awake_ms: 900,
            resumes: 1,
            last_resume_suspended_ms: 60_000,
            suspended_ms: 60_000,
        });
        assert!(!exited_within(&mut run, &mut seen, Duration::from_millis(200)).await);
        // Past the timeout but inside the grace (900 + 120 s, capped at 1 s + 120 s).
        let mut t = *clock.borrow();
        t.awake_ms = 1_000;
        clock.send_replace(t);
        assert!(!exited_within(&mut run, &mut seen, Duration::from_millis(200)).await);
        t.awake_ms = 120_900;
        clock.send_replace(t);
        drain(&mut run, &mut seen, LIMIT).await;
        let exit = seen.exit();
        assert!(exit.timed_out);
        assert!(exit.wake_grace);
        assert_eq!(exit.suspended_ms, 60_000);
        assert_eq!(exit.awake_ms, 120_900);
    }

    #[tokio::test]
    async fn the_timeout_sends_term_before_kill() {
        let (clock, spawner) = spawner();
        let mut s = sh("trap 'echo got-term' TERM; echo ready; while :; do sleep 0.05; done");
        s.timeout = Duration::from_secs(1);
        let mut run = spawner.spawn(s).unwrap();
        let mut seen = Seen::default();
        until_line(&mut run, &mut seen, "ready").await;
        clock.send_replace(awake(1_000));
        drain(&mut run, &mut seen, LIMIT).await;
        assert!(
            seen.stdout.iter().any(|l| l == "got-term"),
            "TERM came first"
        );
        let exit = seen.exit();
        assert!(exit.timed_out);
        assert_eq!(exit.signal, Some(libc::SIGKILL), "then KILL");
        assert!(exit.wall_ms >= fast().term_grace.as_millis() as u64);
    }

    #[tokio::test]
    async fn a_result_read_before_the_timeout_wins() {
        let (clock, spawner) = spawner();
        let mut s = sh("echo '{\"type\":\"result\"}'; exec sleep 30");
        s.timeout = Duration::from_secs(1);
        s.result_probe = Some(|l| l.contains("\"type\":\"result\""));
        let mut run = spawner.spawn(s).unwrap();
        let mut seen = Seen::default();
        until_line(&mut run, &mut seen, "{\"type\":\"result\"}").await;
        clock.send_replace(awake(1_000));
        drain(&mut run, &mut seen, LIMIT).await;
        let exit = seen.exit();
        assert!(exit.timed_out);
        assert!(exit.result_seen);
        assert!(exit.lingered);
    }

    #[tokio::test]
    async fn a_straggler_that_obeys_term_is_terminated_after_the_grace() {
        let (_clock, spawner) = spawner();
        let seen = run_to_settle(&spawner, sh("sleep 30 >/dev/null 2>&1 & echo done")).await;
        assert_eq!(seen.exit().code, Some(0));
        let settle = seen.settle();
        assert_eq!(settle.outcome, SettleOutcome::Terminated);
        assert_eq!(settle.stragglers, 1);
        assert!(settle.settle_ms >= fast().straggler_grace.as_millis() as u64);
        assert!(!settle.pipe_held);
        let exit_at = seen.order.iter().position(|e| *e == "exit").unwrap();
        let settled_at = seen.order.iter().position(|e| *e == "settled").unwrap();
        assert!(exit_at < settled_at, "the leader's exit is reported first");
    }

    #[tokio::test]
    async fn a_straggler_that_ignores_term_is_killed() {
        let (_clock, spawner) = spawner();
        let seen = run_to_settle(
            &spawner,
            sh("(trap '' TERM; exec sleep 30) >/dev/null 2>&1 & echo done"),
        )
        .await;
        let settle = seen.settle();
        assert_eq!(settle.outcome, SettleOutcome::Killed);
        assert!(settle.stragglers >= 1);
    }

    #[tokio::test]
    async fn a_straggler_holding_stdout_is_settled_and_eof_follows() {
        let (_clock, spawner) = spawner();
        let seen = run_to_settle(&spawner, sh("sleep 30 & echo done")).await;
        assert_eq!(seen.stdout, ["done"]);
        assert_eq!(seen.settle().outcome, SettleOutcome::Terminated);
        assert!(!seen.settle().pipe_held);
        // The leader's exit is reported while the Straggler still holds stdout.
        assert_eq!(seen.order, ["exit", "eof", "settled"]);
    }

    #[tokio::test]
    async fn a_pipe_held_outside_the_group_is_closed_after_the_cap() {
        let Ok(perl) = which::which("perl") else {
            return;
        };
        let (_clock, spawner) = spawner();
        // The child leaves the group (setsid) but keeps stdout for 2 s.
        let s = spec(
            &perl,
            &[
                "-MPOSIX",
                "-e",
                "if (fork) { print qq(x\\n); exit 0 } setsid(); sleep 2",
            ],
        );
        let seen = run_to_settle(&spawner, s).await;
        assert_eq!(seen.stdout, ["x"]);
        assert!(seen.settle().pipe_held);
        assert!(!seen.order.contains(&"eof"));
    }

    #[tokio::test]
    async fn the_system_prompt_file_is_deleted_at_the_settle() {
        let (_clock, spawner) = spawner();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asp-r1.md");
        tokio::fs::write(&path, b"system prompt").await.unwrap();
        let mut s = sh("cat \"$0\"");
        s.args.push(path.clone().into());
        s.system_prompt_file = Some(path.clone());
        let seen = run_to_settle(&spawner, s).await;
        assert_eq!(seen.stdout, ["system prompt"]);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn the_hook_sees_spawn_exit_and_settle_in_order() {
        let (_clock, spawner) = spawner();
        let hook = Arc::new(Recorder::default());
        let spawner = spawner.with_hook(hook.clone());
        let mut run = spawner.spawn(sh("exit 0")).unwrap();
        let pid = run.pid();
        assert!(run.stamp().is_some());
        let mut seen = Seen::default();
        drain(&mut run, &mut seen, LIMIT).await;
        assert_eq!(
            *hook.0.lock().unwrap(),
            [
                format!("spawned r1 {pid} true"),
                "leader_exited r1".to_owned(),
                "settled r1".to_owned(),
            ]
        );
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::path::{Path, PathBuf};

    use super::tests::*;
    use super::*;

    const LIMIT: Duration = Duration::from_secs(30);

    fn system32(exe: &str) -> PathBuf {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        Path::new(&root).join("System32").join(exe)
    }

    fn spawner() -> (watch::Sender<AwakeTime>, Spawner) {
        let (tx, rx) = watch::channel(AwakeTime::default());
        (tx, Spawner::new(rx).with_timings(fast()))
    }

    async fn run_to_settle(spawner: &Spawner, spec: SpawnSpec) -> Seen {
        let mut run = spawner.spawn(spec).unwrap();
        let mut seen = Seen::default();
        drain(&mut run, &mut seen, LIMIT).await;
        seen
    }

    #[tokio::test]
    async fn the_prompt_reaches_stdin() {
        let (_clock, spawner) = spawner();
        let mut s = spec(&system32("sort.exe"), &[]);
        s.stdin = "b\r\na\r\n".into();
        let seen = run_to_settle(&spawner, s).await;
        assert_eq!(seen.stdout, ["a", "b"]);
        assert_eq!(seen.exit().code, Some(0));
        assert!(seen.settle().stdin_written);
    }

    #[tokio::test]
    async fn exit_127_is_reported() {
        let (_clock, spawner) = spawner();
        let seen = run_to_settle(&spawner, spec(&system32("cmd.exe"), &["/C", "exit 127"])).await;
        assert_eq!(seen.exit().code, Some(127));
        assert!(seen.exit().exec_failed());
    }

    #[tokio::test]
    async fn the_timeout_counts_awake_time_and_ends_the_job() {
        let (clock, spawner) = spawner();
        let mut s = spec(&system32("ping.exe"), &["-n", "30", "127.0.0.1"]);
        s.timeout = Duration::from_secs(1);
        let mut run = spawner.spawn(s).unwrap();
        let mut seen = Seen::default();
        assert!(!exited_within(&mut run, &mut seen, Duration::from_millis(1_500)).await);
        clock.send_replace(awake(1_000));
        drain(&mut run, &mut seen, LIMIT).await;
        assert!(seen.exit().timed_out);
        assert_eq!(seen.exit().code, Some(1), "TerminateJobObject's exit code");
    }

    #[tokio::test]
    async fn a_straggler_in_the_job_is_ended_at_the_settle() {
        let (_clock, spawner) = spawner();
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("straggle.cmd");
        tokio::fs::write(
            &script,
            "@echo off\r\nstart /B ping -n 30 127.0.0.1 >NUL\r\necho done\r\n",
        )
        .await
        .unwrap();
        let seen = run_to_settle(&spawner, spec(&script, &[])).await;
        assert_eq!(seen.stdout, ["done"]);
        let settle = seen.settle();
        assert_eq!(settle.outcome, SettleOutcome::Killed);
        assert!(settle.stragglers >= 1);
    }
}
