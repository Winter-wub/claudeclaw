//! One run end to end: spawn → stream → [`RunEvent`]s + [`RunOutcome`]
//! (architecture §4 `claudeclaw-runner` → "Output"; #37, #47).
//!
//! [`start`] spawns through [`Spawner::spawn`] and supervises the run in one tokio task:
//!
//! 1. every [`SpawnEvent::Stdout`] line goes through the pure [`stream`] parser
//!    ([`stream::parse_line`] → [`StreamState::observe`]), and each returned [`StreamEvent`] is
//!    forwarded as a [`RunEvent::Stream`] on an **unbounded** channel: a slow reader must not
//!    lose reply text, and the buffered events stay bounded because `spawn` already caps the run's
//!    output at [`MAX_OUTPUT_BYTES`](crate::spawn::MAX_OUTPUT_BYTES);
//! 2. every [`SpawnEvent::Stderr`] line is kept, capped, for [`ExitReport::stderr`];
//! 3. the [`SpawnEvent::LeaderExited`] exit is remembered, and the run is judged ([`stream::judge`])
//!    and the [`RunOutcome`] sent at the first point where the leader has exited **and** the parser
//!    has seen a `result` event or [`SpawnEvent::StdoutEof`] has arrived. The stdout reader can
//!    still hold the final lines when the leader exit is reported, so a `result` that is the last
//!    line must not be lost. If neither has happened by [`SpawnEvent::Settled`], the run is judged
//!    there instead, just before the `Settle` is sent — no more lines can arrive (spawn.rs "Seam
//!    with `stream`"). Either way the reply goes out before the Stragglers are waited for (#37 D2);
//! 4. at [`SpawnEvent::Settled`] the [`Settle`] is sent, which is what the Scope's next Turn waits
//!    for (#37 D3).
//!
//! The stream layer already counts how many lines were JSON, so `judge()` sees the "no JSON line"
//! fact its exit-126/127 and `unknown option` rules need (#26 D3, #42 D3); this module never
//! re-implements those rules.

use std::fmt;

use tokio::sync::{mpsc, oneshot};

use crate::argv::ClaudeArgs;
use crate::spawn::{
    LeaderExit, Run, Settle, SpawnError, SpawnEvent, SpawnSpec, Spawner, Terminator,
};
use crate::stream::{
    self, Event, ExitReport, RunClock, RunOutcome, StreamEvent, StreamItem, StreamState,
};

/// How much stderr is kept for [`ExitReport::stderr`] (§4 runner `stream`). Past it the pipe is
/// still drained but nothing more is kept, so the buffer can't grow without bound.
const STDERR_CAP: usize = 64 * 1024;

/// Everything [`start`] needs: where and how to spawn, plus the argv `judge()` reads (#42 D3).
pub struct RunRequest {
    /// The process to start; [`start`] fills [`SpawnSpec::result_probe`] when it is `None`.
    pub spawn: SpawnSpec,
    /// The run's argv, so `judge()` can tell a flag the Daemon passed from one a Launcher added.
    pub args: ClaudeArgs,
}

impl fmt::Debug for RunRequest {
    // Content-free (architecture §2): both fields already print sizes only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunRequest")
            .field("spawn", &self.spawn)
            .field("args", &self.args)
            .finish()
    }
}

/// Progress while a run works (§4 runner "Output"). One variant today; the enum leaves room for
/// more later.
#[derive(Clone, PartialEq)]
pub enum RunEvent {
    /// A parsed stream event, in the order it was read.
    Stream(StreamEvent),
}

impl fmt::Debug for RunEvent {
    // Content-free (architecture §2): delegate to [`StreamEvent`]'s Debug (sizes and kinds only).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stream(event) => f.debug_tuple("Stream").field(event).finish(),
        }
    }
}

/// A started run's channels and its stops, returned by [`start`] (§4 runner "Output", #37).
///
/// The order is: [`RunHandle::events`] while it works, [`RunHandle::outcome`] at the leader's exit
/// (#37 D2), then [`RunHandle::settled`] once the group is empty (#37 D3). Dropping `events` is
/// safe: the run still settles and the outcome is still delivered.
pub struct RunHandle {
    /// Stream events, in order. Unbounded: a slow reader must not lose reply text, and the
    /// buffer stays bounded because `spawn` caps the run's output at
    /// [`MAX_OUTPUT_BYTES`](crate::spawn::MAX_OUTPUT_BYTES).
    pub events: mpsc::UnboundedReceiver<RunEvent>,
    /// The judged run, sent at the leader's exit.
    pub outcome: oneshot::Receiver<RunOutcome>,
    /// The settle, sent once the run's process group (Job object on Windows) is empty.
    pub settled: oneshot::Receiver<Settle>,
    /// Stops the run from elsewhere while another task reads the events (§5 step 3).
    pub terminator: Terminator,
    /// The leader's pid; on Unix also the pgid.
    pub pid: u32,
}

impl fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunHandle")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// The pure [`crate::spawn::ResultProbe`]: a line is a `result` event when the stream parser reads
/// one (#47 D3). [`start`] sets it when the caller left [`SpawnSpec::result_probe`] `None`.
fn result_probe(line: &str) -> bool {
    matches!(
        stream::parse_line(line),
        StreamItem::Event(Event::Result(_))
    )
}

/// Spawns `req` and returns the handle. Must be called inside a tokio runtime, like
/// [`Spawner::spawn`].
pub fn start(spawner: &Spawner, req: RunRequest) -> Result<RunHandle, SpawnError> {
    let RunRequest { mut spawn, args } = req;
    if spawn.result_probe.is_none() {
        spawn.result_probe = Some(result_probe);
    }
    let run_id = spawn.run_id.clone();
    let run = spawner.spawn(spawn)?;
    let pid = run.pid();
    let terminator = run.terminator();
    let (events_tx, events) = mpsc::unbounded_channel();
    let (outcome_tx, outcome) = oneshot::channel();
    let (settled_tx, settled) = oneshot::channel();
    tokio::spawn(pump(run, run_id, args, events_tx, outcome_tx, settled_tx));
    Ok(RunHandle {
        events,
        outcome,
        settled,
        terminator,
        pid,
    })
}

/// The one task that drains a [`Run`]: stream events out, the outcome once it can be judged, the
/// settle last. It runs to the end whatever the receiver does, so the run always settles.
async fn pump(
    mut run: Run,
    run_id: String,
    args: ClaudeArgs,
    events: mpsc::UnboundedSender<RunEvent>,
    outcome: oneshot::Sender<RunOutcome>,
    settled: oneshot::Sender<Settle>,
) {
    // Each is sent at most once, so `Option::take` keeps the loop able to move them out.
    let mut outcome = Some(outcome);
    let mut settled = Some(settled);
    let mut state = StreamState::new();
    let mut stderr = StderrBuffer::new(STDERR_CAP);
    let mut stdout_lines: u64 = 0;
    let mut json_lines: u64 = 0;
    // The remembered leader exit and whether stdout has ended. The outcome waits for both facts:
    // the leader exit, plus a `result` line or EOF (spawn.rs "Seam with `stream`").
    let mut leader_exit: Option<LeaderExit> = None;
    let mut stdout_eof = false;

    while let Some(event) = run.next_event().await {
        match event {
            SpawnEvent::Stdout(line) => {
                stdout_lines += 1;
                let item = stream::parse_line(&line);
                if matches!(item, StreamItem::Event(_) | StreamItem::Malformed(_)) {
                    json_lines += 1;
                }
                for event in state.observe(&item) {
                    forward(&events, RunEvent::Stream(event));
                }
                // The `result` can be the very last line, read after the leader exit was already
                // reported: judge as soon as the parser has seen one.
                if state.result_seen()
                    && let Some(exit) = &leader_exit
                {
                    judge(&run_id, &state, exit, &args, &mut stderr, &mut outcome);
                }
            }
            SpawnEvent::Stderr(line) => stderr.push(&line),
            SpawnEvent::StdoutEof => {
                stdout_eof = true;
                // No more lines can arrive, so an already-seen leader exit can be judged now.
                if let Some(exit) = &leader_exit {
                    judge(&run_id, &state, exit, &args, &mut stderr, &mut outcome);
                }
            }
            SpawnEvent::LeaderExited(exit) => {
                tracing::debug!(
                    run_id = %run_id,
                    stdout_lines,
                    json_lines,
                    exit_code = ?exit.code,
                    timed_out = exit.timed_out,
                    lingered = exit.lingered,
                    stopped = exit.stopped,
                    "run.leader_exited"
                );
                // Judge now only if a `result` was read or stdout ended; otherwise wait for one of
                // them, or for the Settle (spawn.rs "Seam with `stream`").
                if state.result_seen() || stdout_eof {
                    judge(&run_id, &state, &exit, &args, &mut stderr, &mut outcome);
                }
                leader_exit = Some(exit);
            }
            SpawnEvent::Settled(settle) => {
                // Nothing more can arrive: judge now if the outcome is still owed, just before the
                // Settle goes out so the reply is still delivered first (#37 D2).
                if let Some(exit) = &leader_exit {
                    judge(&run_id, &state, exit, &args, &mut stderr, &mut outcome);
                }
                tracing::debug!(run_id = %run_id, "run.settled");
                if let Some(settled) = settled.take() {
                    let _ = settled.send(settle);
                }
            }
        }
    }
}

/// Judges the run once and sends the outcome, if it is still owed. `stderr` is snapshotted at this
/// point, so it holds everything captured up to the judgement; `state` and `exit` are only read.
/// After the first call the outcome is gone and later calls are no-ops.
fn judge(
    run_id: &str,
    state: &StreamState,
    exit: &LeaderExit,
    args: &ClaudeArgs,
    stderr: &mut StderrBuffer,
    outcome: &mut Option<oneshot::Sender<RunOutcome>>,
) {
    let Some(outcome) = outcome.take() else {
        return;
    };
    let report = ExitReport {
        exit_code: exit.code,
        signal: exit.signal,
        // A timeout that a `result` beat is judged by that `result`, not as a timeout
        // (#47 D3). `lingered` carries that fact on the outcome below.
        timed_out: exit.timed_out && !exit.lingered,
        interrupted: exit.stopped,
        stderr: stderr.take(),
        clock: RunClock {
            awake_ms: exit.awake_ms,
            suspended_ms: exit.suspended_ms,
            wake_grace: exit.wake_grace,
        },
    };
    let mut judged = stream::judge(state, &report, args);
    judged.lingered = exit.lingered;
    tracing::debug!(run_id = %run_id, "run.judged");
    let _ = outcome.send(judged);
}

/// Forwards one event to the reader. The channel is unbounded, so `send` never blocks and never
/// drops an event while the reader is alive; only a dropped receiver's `SendError` is ignored,
/// and the pump then keeps draining so the run still settles and the outcome is still delivered.
fn forward(events: &mpsc::UnboundedSender<RunEvent>, event: RunEvent) {
    let _ = events.send(event);
}

/// A capped stderr buffer, built line by line for [`ExitReport::stderr`].
struct StderrBuffer {
    text: String,
    cap: usize,
}

impl StderrBuffer {
    fn new(cap: usize) -> Self {
        Self {
            text: String::new(),
            cap,
        }
    }

    /// Appends `line`, keeping at most `cap` bytes on a char boundary. Once full it drops the rest.
    fn push(&mut self, line: &str) {
        if self.text.len() >= self.cap {
            return;
        }
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        let room = self.cap - self.text.len();
        if line.len() <= room {
            self.text.push_str(line);
        } else {
            let end = floor_char_boundary(line, room);
            self.text.push_str(&line[..end]);
        }
    }

    /// The kept stderr.
    fn take(&mut self) -> String {
        std::mem::take(&mut self.text)
    }
}

/// The largest byte index `<= max` that lies on a `char` boundary.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tokio::sync::watch;

    use super::*;
    use crate::clock::AwakeTime;
    use crate::spawn::SpawnTimings;
    use crate::stream::{Failure, Refusal, Verdict};

    const LIMIT: Duration = Duration::from_secs(20);

    fn timings() -> SpawnTimings {
        SpawnTimings {
            term_grace: Duration::from_millis(400),
            straggler_grace: Duration::from_millis(200),
            kill_wait: Duration::from_secs(2),
            pipe_cap: Duration::from_millis(300),
            poll: Duration::from_millis(20),
        }
    }

    fn spawner_with(timings: SpawnTimings) -> (watch::Sender<AwakeTime>, Spawner) {
        let (tx, rx) = watch::channel(AwakeTime::default());
        (tx, Spawner::new(rx).with_timings(timings))
    }

    fn spawner() -> (watch::Sender<AwakeTime>, Spawner) {
        spawner_with(timings())
    }

    fn request(program: &Path) -> RunRequest {
        RunRequest {
            spawn: SpawnSpec {
                run_id: "r1".into(),
                program: program.to_path_buf(),
                args: Vec::new(),
                env: Vec::new(),
                cwd: std::env::temp_dir(),
                stdin: Vec::new(),
                timeout: Duration::from_secs(60),
                system_prompt_file: None,
                result_probe: None,
            },
            args: ClaudeArgs::new(),
        }
    }

    fn awake(ms: u64) -> AwakeTime {
        AwakeTime {
            awake_ms: ms,
            ..AwakeTime::default()
        }
    }

    /// Writes an executable fake `claude` into `dir` and returns its path.
    #[cfg(unix)]
    async fn fake_claude(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.path().join("fake-claude");
        tokio::fs::write(&path, format!("#!/bin/sh\n{body}\n"))
            .await
            .unwrap();
        let mut perms = tokio::fs::metadata(&path).await.unwrap().permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&path, perms).await.unwrap();
        path
    }

    /// Drains every [`RunEvent`], then the outcome and the settle.
    #[cfg(unix)]
    async fn finish(mut handle: RunHandle) -> (Vec<StreamEvent>, RunOutcome, Settle) {
        let mut events = Vec::new();
        while let Some(event) = handle.events.recv().await {
            match event {
                RunEvent::Stream(event) => events.push(event),
            }
        }
        let outcome = handle.outcome.await.unwrap();
        let settle = handle.settled.await.unwrap();
        (events, outcome, settle)
    }

    // A fake claude that prints init + assistant text + result, so the happy path has the session
    // ID, a text delta, and an OK verdict.
    const HAPPY: &str = r#"printf '%s\n' \
        '{"type":"system","subtype":"init","session_id":"s-1","claude_code_version":"2.1.296"}' \
        '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"hello there"}]},"parent_tool_use_id":null}' \
        '{"type":"result","subtype":"success","is_error":false,"result":"hello there","session_id":"s-1"}'
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn happy_path_streams_events_then_outcome_then_settle() {
        let dir = tempfile::tempdir().unwrap();
        // A Straggler holds the group open past leader exit, so the settle demonstrably comes
        // after the outcome, which is sent as soon as the leader exits (#37 D2/D3).
        let body = format!("{HAPPY}sleep 30 >/dev/null 2>&1 &\n");
        let program = fake_claude(&dir, &body).await;
        let (_clock, spawner) = spawner_with(SpawnTimings {
            straggler_grace: Duration::from_secs(3),
            ..timings()
        });
        let mut handle = start(&spawner, request(&program)).unwrap();

        // The session ID and the text delta are forwarded as they are read (before leader exit).
        let mut seen = Vec::new();
        let mut got_session = false;
        let mut got_text = false;
        while let Some(RunEvent::Stream(event)) = tokio::time::timeout(LIMIT, handle.events.recv())
            .await
            .expect("an event in time")
        {
            got_session |= matches!(&event, StreamEvent::SessionId(_));
            got_text |= matches!(
                &event,
                StreamEvent::TextDelta { text, .. } if text == "hello there"
            );
            seen.push(event);
            if got_session && got_text {
                break;
            }
        }
        assert!(
            seen.iter()
                .any(|e| matches!(e, StreamEvent::SessionId(id) if id == "s-1"))
        );
        assert!(
            seen.iter()
                .any(|e| matches!(e, StreamEvent::TextDelta { text, .. } if text == "hello there"))
        );

        let outcome = tokio::time::timeout(LIMIT, handle.outcome)
            .await
            .expect("the outcome in time")
            .unwrap();
        assert_eq!(outcome.verdict, Verdict::Ok);
        assert_eq!(outcome.reply.as_deref(), Some("hello there"));
        assert_eq!(outcome.session_id.as_deref(), Some("s-1"));
        // The Straggler keeps the group alive, so the settle can't be in yet.
        assert!(
            handle.settled.try_recv().is_err(),
            "the settle comes after the outcome"
        );

        while handle.events.recv().await.is_some() {}
        let settle = tokio::time::timeout(LIMIT, handle.settled)
            .await
            .expect("the settle in time")
            .unwrap();
        assert!(settle.stragglers >= 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_result_that_is_the_last_line_is_not_judged_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        // The leader prints `init`, then the `result` as its final line — with no trailing
        // newline, so the codec can only deliver it at EOF — and exits at once. A backgrounded
        // Straggler inherits stdout and holds the pipe open past the leader exit (until the
        // Straggler grace reaps it), so the `result` is guaranteed to be read *after*
        // `LeaderExited`. Judging at the leader exit (the old behaviour) reports
        // `Failed(no_result)`; the rule waits for the `result` or EOF instead.
        let body = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-1"}'
printf '%s' '{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s-1"}'
sleep 30 &
exit 0
"#;
        let program = fake_claude(&dir, body).await;
        let (_clock, spawner) = spawner();
        let handle = start(&spawner, request(&program)).unwrap();
        let (_events, outcome, _settle) = finish(handle).await;
        assert_eq!(outcome.verdict, Verdict::Ok);
        assert_eq!(outcome.reply.as_deref(), Some("done"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn no_result_on_exit_zero_is_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"printf '%s\n' \
            '{"type":"system","subtype":"init","session_id":"s-1"}' \
            '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Starting"}]},"parent_tool_use_id":null}'
"#;
        let program = fake_claude(&dir, body).await;
        let (_clock, spawner) = spawner();
        let handle = start(&spawner, request(&program)).unwrap();
        let (events, outcome, settle) = finish(handle).await;
        assert_eq!(outcome.verdict, Verdict::Failed(Failure::NoResult));
        assert_eq!(outcome.exit_code, Some(0));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::TextDelta { text, .. } if text == "Starting"))
        );
        assert_eq!(settle.stragglers, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn no_result_with_a_straggler_holding_stdout_is_judged_at_settle() {
        let dir = tempfile::tempdir().unwrap();
        // The leader prints `init` and exits at once; a Straggler inherits stdout, so neither a
        // `result` nor EOF arrive until the Straggler grace reaps it. The run must be judged at
        // Settle — `Failed(no_result)` — and must not hang waiting for a line that never comes.
        let body = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-1"}'
sleep 30 &
exit 0
"#;
        let program = fake_claude(&dir, body).await;
        let (_clock, spawner) = spawner();
        let mut handle = start(&spawner, request(&program)).unwrap();
        // The init event means the run is up.
        assert!(handle.events.recv().await.is_some());
        let outcome = tokio::time::timeout(LIMIT, handle.outcome)
            .await
            .expect("the outcome in time")
            .unwrap();
        assert_eq!(outcome.verdict, Verdict::Failed(Failure::NoResult));
        // The settle still follows, with the reaped Straggler accounted for.
        while handle.events.recv().await.is_some() {}
        let settle = tokio::time::timeout(LIMIT, handle.settled)
            .await
            .expect("the settle in time")
            .unwrap();
        assert!(settle.stragglers >= 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_127_with_no_stdout_is_a_launcher_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_claude(&dir, "exit 127").await;
        let (_clock, spawner) = spawner();
        let handle = start(&spawner, request(&program)).unwrap();
        let (events, outcome, _settle) = finish(handle).await;
        assert!(events.is_empty());
        assert_eq!(outcome.exit_code, Some(127));
        assert_eq!(
            outcome.verdict,
            Verdict::Refused(Refusal::LauncherExecFailed { exit_code: 127 })
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_run_past_its_timeout_is_a_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-1"}'
exec sleep 30
"#;
        let program = fake_claude(&dir, body).await;
        let (clock, spawner) = spawner();
        let mut req = request(&program);
        req.spawn.timeout = Duration::from_secs(1);
        let mut handle = start(&spawner, req).unwrap();
        // Wait for the first event so the run is up, then fire the Awake-clock deadline.
        assert!(handle.events.recv().await.is_some());
        clock.send_replace(awake(1_000));
        let (_events, outcome, _settle) = finish(handle).await;
        assert_eq!(outcome.verdict, Verdict::Timeout);
        assert!(outcome.signal.is_some());
        assert!(!outcome.lingered);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_result_read_before_the_timeout_lingers_and_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        // No init line: the result's `session_id` is the first event, and the spawn probe runs on
        // that same line before the event is forwarded, so its arrival means the result was read.
        let body = r#"printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s-1"}'
exec sleep 30
"#;
        let program = fake_claude(&dir, body).await;
        let (clock, spawner) = spawner();
        let mut req = request(&program);
        req.spawn.timeout = Duration::from_secs(1);
        let mut handle = start(&spawner, req).unwrap();
        assert!(handle.events.recv().await.is_some());
        clock.send_replace(awake(1_000));
        let (_events, outcome, _settle) = finish(handle).await;
        assert_eq!(outcome.verdict, Verdict::Ok);
        assert!(outcome.lingered, "#47 D3: the result beat the timeout");
        assert_eq!(outcome.reply.as_deref(), Some("done"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dropped_events_receiver_still_yields_outcome_and_settle() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_claude(&dir, HAPPY).await;
        let (_clock, spawner) = spawner();
        let handle = start(&spawner, request(&program)).unwrap();
        let RunHandle {
            events,
            outcome,
            settled,
            terminator: _,
            pid: _,
        } = handle;
        drop(events);
        let outcome = tokio::time::timeout(LIMIT, outcome)
            .await
            .expect("the outcome in time")
            .unwrap();
        assert_eq!(outcome.verdict, Verdict::Ok);
        let _settle = tokio::time::timeout(LIMIT, settled)
            .await
            .expect("the settle in time")
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_slow_reader_loses_no_text_deltas() {
        const MESSAGES: usize = 1_200;
        let dir = tempfile::tempdir().unwrap();
        // More assistant messages than any bounded queue would hold, each with its own text block.
        let body = r#"i=0
while [ "$i" -lt @N@ ]; do
  printf '{"type":"assistant","message":{"id":"m%d","content":[{"type":"text","text":"x"}]},"parent_tool_use_id":null}\n' "$i"
  i=$((i+1))
done
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s-1"}\n'
"#
        .replace("@N@", &MESSAGES.to_string());
        let program = fake_claude(&dir, &body).await;
        let (_clock, spawner) = spawner();
        let mut handle = start(&spawner, request(&program)).unwrap();

        // The reader is as slow as possible: it reads no event until the outcome has arrived.
        let outcome = tokio::time::timeout(LIMIT, handle.outcome)
            .await
            .expect("the outcome in time")
            .unwrap();
        assert_eq!(outcome.verdict, Verdict::Ok);
        assert_eq!(outcome.reply.as_deref(), Some("done"));

        // Every delta was kept: one per message, plus the "\n" between messages.
        let mut deltas = Vec::new();
        while let Some(RunEvent::Stream(event)) = handle.events.recv().await {
            if let StreamEvent::TextDelta {
                text,
                subagent: false,
            } = event
            {
                deltas.push(text);
            }
        }
        assert_eq!(deltas.len(), 2 * MESSAGES - 1);
        assert_eq!(deltas.concat(), vec!["x"; MESSAGES].join("\n"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_before_any_result_is_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-1"}'
trap 'exit 0' TERM
while :; do sleep 0.1; done
"#;
        let program = fake_claude(&dir, body).await;
        let (_clock, spawner) = spawner();
        let mut handle = start(&spawner, request(&program)).unwrap();
        // The init event means the run is up.
        assert!(handle.events.recv().await.is_some());
        handle.terminator.terminate();
        let (_events, outcome, _settle) = finish(handle).await;
        assert_eq!(outcome.verdict, Verdict::Interrupted);
        assert!(!outcome.lingered);
    }

    #[test]
    fn the_default_result_probe_reads_only_result_events() {
        assert!(result_probe(r#"{"type":"result"}"#));
        assert!(!result_probe("noise"));
        assert!(!result_probe(r#"{"type":"assistant"}"#));
    }

    #[test]
    fn stderr_is_capped_on_a_char_boundary() {
        let mut buf = StderrBuffer::new(8);
        buf.push("abcd");
        buf.push("é"); // 2 bytes: "abcd\né" is 7 bytes, inside the cap.
        assert_eq!(buf.take(), "abcd\né");
        // A line past the cap keeps only whole characters.
        let mut buf = StderrBuffer::new(5);
        buf.push("ééé");
        assert_eq!(buf.take(), "éé");
    }

    #[test]
    fn debug_is_content_free() {
        let event = RunEvent::Stream(StreamEvent::TextDelta {
            text: "secret reply".into(),
            subagent: false,
        });
        let shown = format!("{event:?}");
        assert!(!shown.contains("secret"), "{shown}");
        let req = request(Path::new("/bin/sh"));
        let shown = format!("{req:?}");
        assert!(!shown.contains("/bin/sh"), "{shown}");
    }
}
