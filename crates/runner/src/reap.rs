//! The Reap: ending a gone Worker's runs, for the Watchdog and `claudeclaw stop` (architecture
//! §4 `claudeclaw-runner` → `reap`; #29).
//!
//! The caller reads the Run registry (`store::runs`) and passes each entry as a [`ReapEntry`].
//! [`reap`] proves each entry is still the process that was recorded, by its OS start stamp
//! (#29 D3), then ends it: on Unix SIGTERM to every verified process group at once, 5 s, SIGKILL,
//! 5 s (#29 D4; TERM before KILL is an invariant, #47 D4). On Windows the kernel has already
//! ended the run's Job object (`KILL_ON_JOB_CLOSE`), so a recorded pid that is still alive is
//! terminated and reported `survived` (#29 D1). There is no file IO here.
//!
//! [`process_stamp`] is also what a spawn records (#29 D2 step 3; see [`crate::spawn::SpawnHook`]).

mod os;

use std::fmt;
use std::time::Duration;

use tokio::time::Instant;

#[cfg(all(unix, test))]
pub(crate) use os::pid_alive;
#[cfg(windows)]
pub(crate) use os::win;
#[cfg(unix)]
pub(crate) use os::{group_alive, group_members, signal_group};

/// The env var every run carries with its `run_id` (#29 D2); the leader-gone fallback looks for
/// it on group members (#29 D3).
pub const RUN_ID_ENV: &str = "CLAUDECLAW_RUN_ID";

/// An OS process start stamp, which proves a pid is still the process that was recorded (#29
/// D2–D3). It mirrors `api::supervisor::ProcessStamp`, which this crate can't depend on (§3.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProcessStamp {
    /// `proc_pidinfo(PROC_PIDTBSDINFO)`'s start time.
    Macos { sec: u64, usec: u64 },
    /// The boot and `/proc/<pid>/stat` field 22 (`starttime`, in ticks since boot).
    Linux { boot_id: String, start_ticks: u64 },
    /// `GetProcessTimes`' creation time as a FILETIME.
    Windows { creation_time: u64 },
}

impl ProcessStamp {
    fn same_os(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// Why a stamp couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum ReapError {
    #[error("no such process")]
    NoSuchProcess,
    #[error("process start stamps are not supported on this OS")]
    Unsupported,
    #[error("reading the process start stamp failed")]
    Io(#[source] std::io::Error),
}

impl From<std::io::Error> for ReapError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::NoSuchProcess,
            std::io::ErrorKind::Unsupported => Self::Unsupported,
            _ => Self::Io(e),
        }
    }
}

/// Reads the OS start stamp of a live `pid` (#29 D2): `proc_pidinfo` on macOS, `boot_id` plus
/// `/proc/<pid>/stat` on Linux, `GetProcessTimes` on Windows. Read it before the child is waited,
/// so the pid can't have been reused.
///
/// **Blocking** (it reads `/proc` on Linux): async code calls it through `spawn_blocking`.
pub fn process_stamp(pid: u32) -> Result<ProcessStamp, ReapError> {
    Ok(os::process_stamp(pid)?)
}

/// How a run is ended (#29 D1); mirrors `api::supervisor::KillMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KillMethod {
    /// Unix: the run leader's process group, where pid = pgid.
    ProcessGroup,
    /// Windows: the run's kill-on-close Job object.
    JobObject,
}

impl KillMethod {
    /// What this OS's spawns use.
    pub const NATIVE: Self = if cfg!(windows) {
        Self::JobObject
    } else {
        Self::ProcessGroup
    };
}

/// One Run registry entry to reap (#29 D2): the minimum the Reap needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapEntry {
    /// The run's `run_id`; also what [`RUN_ID_ENV`] carries.
    pub run_id: String,
    /// The Scope's wire form, opaque here and only logged on `run.reaped`.
    pub scope: Option<String>,
    /// `None` while the entry is still `spawning`.
    pub pid: Option<u32>,
    /// `None` while the entry is still `spawning`.
    pub stamp: Option<ProcessStamp>,
    pub kill: KillMethod,
}

/// Why the Reap runs (#29 D4); logged on `reap.start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapReason {
    /// The Watchdog reaping the Worker whose exit it just observed (#29 D4 case 1). A leader-gone
    /// group is then provably ours, because the gap is milliseconds (#29 D3 (a)). `graceful`
    /// makes a `killed` a WARN: the Worker should have left nothing.
    WorkerExit { graceful: bool },
    /// The Watchdog at start-up (#29 D4 case 2).
    Startup,
    /// `claudeclaw stop` (#29 D4 case 3).
    CliStop,
}

impl ReapReason {
    /// The wire form: `worker_exit | startup | cli_stop`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkerExit { .. } => "worker_exit",
            Self::Startup => "startup",
            Self::CliStop => "cli_stop",
        }
    }
}

/// What the Reap found for one entry (#29 D3, D5); mirrors `api::supervisor::ReapOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReapOutcome {
    Killed,
    AlreadyGone,
    /// The pid was reused by another process.
    NotOurs,
    /// The leader is gone and no group member proved it ours.
    Unverified,
    /// The entry was still `spawning`.
    UnknownPid,
    Survived,
}

impl ReapOutcome {
    /// The wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Killed => "killed",
            Self::AlreadyGone => "already_gone",
            Self::NotOurs => "not_ours",
            Self::Unverified => "unverified",
            Self::UnknownPid => "unknown_pid",
            Self::Survived => "survived",
        }
    }
}

impl fmt::Display for ReapOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The last signal the Reap sent to an entry. Windows' `TerminateProcess` counts as `kill`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReapSignal {
    Term,
    Kill,
}

impl ReapSignal {
    /// The wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Term => "term",
            Self::Kill => "kill",
        }
    }
}

/// One entry's result (#29 D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapedRun {
    pub run_id: String,
    pub outcome: ReapOutcome,
    /// `None` when nothing was signalled.
    pub signal: Option<ReapSignal>,
    /// From the start of the kill until the group was gone (or given up on).
    pub ms: u64,
    /// For `unverified`: the group's member pids, for the leftover report (#29 D3, D5).
    pub members: Vec<u32>,
}

/// A Reap's per-outcome counts (#29 D5); mirrors `api::supervisor::ReapCounts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReapCounts {
    pub killed: u32,
    pub already_gone: u32,
    pub not_ours: u32,
    pub unverified: u32,
    pub unknown_pid: u32,
    pub survived: u32,
}

impl ReapCounts {
    fn add(&mut self, outcome: ReapOutcome) {
        let n = match outcome {
            ReapOutcome::Killed => &mut self.killed,
            ReapOutcome::AlreadyGone => &mut self.already_gone,
            ReapOutcome::NotOurs => &mut self.not_ours,
            ReapOutcome::Unverified => &mut self.unverified,
            ReapOutcome::UnknownPid => &mut self.unknown_pid,
            ReapOutcome::Survived => &mut self.survived,
        };
        *n += 1;
    }
}

/// Every entry's result, in input order, and the counts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReapReport {
    pub runs: Vec<ReapedRun>,
    pub counts: ReapCounts,
}

/// The Reap's waits (#29 D4). [`Default`] is the spec's; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReapTimings {
    /// SIGTERM → SIGKILL.
    pub term_grace: Duration,
    /// SIGKILL → `survived`.
    pub kill_wait: Duration,
    /// How often an emptying group is probed.
    pub poll: Duration,
}

impl Default for ReapTimings {
    fn default() -> Self {
        Self {
            term_grace: Duration::from_secs(5),
            kill_wait: Duration::from_secs(5),
            poll: Duration::from_millis(50),
        }
    }
}

/// Reaps `entries` with the spec's timings (#29 D3–D5) and logs `reap.start`, one `run.reaped`
/// per entry and `reap.done`.
pub async fn reap(entries: &[ReapEntry], reason: ReapReason) -> ReapReport {
    reap_with(entries, reason, ReapTimings::default()).await
}

/// [`reap`] with other timings, for tests.
pub async fn reap_with(
    entries: &[ReapEntry],
    reason: ReapReason,
    timings: ReapTimings,
) -> ReapReport {
    tracing::info!(
        event = "reap.start",
        reason = reason.as_str(),
        "reaping the runs of a gone Worker"
    );
    let mut runs: Vec<ReapedRun> = entries
        .iter()
        .map(|e| ReapedRun {
            run_id: e.run_id.clone(),
            outcome: ReapOutcome::Unverified,
            signal: None,
            ms: 0,
            members: Vec::new(),
        })
        .collect();

    let start = Instant::now();
    // Verify and send the first signal in one blocking pass: the checks read `/proc` on Linux
    // (#27), and no await falls between an entry's last identity check and its signal (#29 D3).
    let owned = entries.to_vec();
    let verdicts = tokio::task::spawn_blocking(move || {
        owned
            .iter()
            .map(|e| verify_and_signal(e, reason))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!(err = %e, "the Reap's verification failed; every entry is unverified");
        Vec::new()
    });
    let mut to_kill = Vec::new();
    for (i, verdict) in verdicts.into_iter().enumerate() {
        match verdict {
            Verdict::Ours(target) => {
                runs[i].signal = Some(FIRST_SIGNAL);
                to_kill.push((i, target));
            }
            Verdict::Done(outcome, members) => {
                runs[i].outcome = outcome;
                runs[i].members = members;
            }
        }
    }
    finish(&mut runs, to_kill, start, timings).await;

    let mut counts = ReapCounts::default();
    for (entry, run) in entries.iter().zip(&runs) {
        counts.add(run.outcome);
        log_reaped(entry, run, reason);
    }
    tracing::info!(
        event = "reap.done",
        killed = counts.killed,
        already_gone = counts.already_gone,
        not_ours = counts.not_ours,
        unverified = counts.unverified,
        unknown_pid = counts.unknown_pid,
        survived = counts.survived,
        "reap finished"
    );
    ReapReport { runs, counts }
}

fn log_reaped(entry: &ReapEntry, run: &ReapedRun, reason: ReapReason) {
    macro_rules! reaped {
        ($level:ident) => {
            tracing::$level!(
                event = "run.reaped",
                run_id = %run.run_id,
                scope = entry.scope.as_deref(),
                outcome = run.outcome.as_str(),
                signal = run.signal.map(ReapSignal::as_str),
                ms = run.ms,
                "reaped a run"
            )
        };
    }
    // Levels: #29 D5; `already_gone` and `not_ours` at INFO (#74).
    match run.outcome {
        ReapOutcome::Killed if matches!(reason, ReapReason::WorkerExit { graceful: true }) => {
            reaped!(warn)
        }
        ReapOutcome::Killed | ReapOutcome::AlreadyGone | ReapOutcome::NotOurs => reaped!(info),
        ReapOutcome::Unverified | ReapOutcome::UnknownPid | ReapOutcome::Survived => {
            reaped!(error)
        }
    }
}

enum Verdict<T> {
    /// Proven ours, and already sent [`FIRST_SIGNAL`].
    Ours(T),
    Done(ReapOutcome, Vec<u32>),
}

/// Unix: TERM first (#47 D4). The target is the pgid.
#[cfg(unix)]
const FIRST_SIGNAL: ReapSignal = ReapSignal::Term;

/// Windows: `TerminateProcess`, through the handle that was verified.
#[cfg(windows)]
const FIRST_SIGNAL: ReapSignal = ReapSignal::Kill;

/// #29 D3's rules, in order (Unix); a verified group is sent SIGTERM at once. Blocking.
#[cfg(unix)]
fn verify_and_signal(entry: &ReapEntry, reason: ReapReason) -> Verdict<u32> {
    let verdict = verify(entry, reason);
    if let Verdict::Ours(pgid) = verdict
        && let Err(e) = os::signal_group(pgid, nix::sys::signal::Signal::SIGTERM)
    {
        tracing::debug!(err = %e, "SIGTERM to a reaped group failed");
    }
    verdict
}

#[cfg(unix)]
fn verify(entry: &ReapEntry, reason: ReapReason) -> Verdict<u32> {
    let Some(pid) = entry.pid else {
        return Verdict::Done(ReapOutcome::UnknownPid, Vec::new());
    };
    if !os::group_alive(pid) {
        return Verdict::Done(ReapOutcome::AlreadyGone, Vec::new());
    }
    let leader = if os::pid_alive(pid) {
        os::process_stamp(pid).ok()
    } else {
        None
    };
    match (leader, &entry.stamp) {
        (Some(now), Some(recorded)) if now == *recorded => Verdict::Ours(pid),
        // A live leader with another stamp: the pid was reused, so our group had emptied (Linux
        // and XNU never reuse a pid still in use as a pgid).
        (Some(now), Some(recorded)) if now.same_os(recorded) => {
            Verdict::Done(ReapOutcome::NotOurs, Vec::new())
        }
        (Some(_), _) => Verdict::Done(ReapOutcome::Unverified, os::group_members(pid)),
        (None, _) => {
            // The leader is gone and the group is alive.
            if matches!(reason, ReapReason::WorkerExit { .. }) {
                return Verdict::Ours(pid);
            }
            let marker = format!("{RUN_ID_ENV}={}", entry.run_id);
            let members = os::group_members(pid);
            if members
                .iter()
                .any(|m| os::has_env_entry(*m, marker.as_bytes()))
            {
                Verdict::Ours(pid)
            } else {
                Verdict::Done(ReapOutcome::Unverified, members)
            }
        }
    }
}

/// #29 D3 on Windows: the kernel already ended the job; a live pid with the recorded creation
/// time is terminated (and `survived`). The check and the kill use one handle, which pins the
/// process, so a reused pid can't be hit in between.
#[cfg(windows)]
fn verify_and_signal(entry: &ReapEntry, _reason: ReapReason) -> Verdict<win::Process> {
    use std::io::ErrorKind;

    let Some(pid) = entry.pid else {
        return Verdict::Done(ReapOutcome::UnknownPid, Vec::new());
    };
    let process = match win::Process::open_for_kill(pid) {
        Ok(p) => p,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Verdict::Done(ReapOutcome::AlreadyGone, Vec::new());
        }
        // Not ours to kill, e.g. another user's process: tell which with a query handle.
        Err(_) => {
            let outcome = match (os::process_stamp(pid), &entry.stamp) {
                (Err(e), _) if e.kind() == ErrorKind::NotFound => ReapOutcome::AlreadyGone,
                (Ok(now), Some(recorded)) if now == *recorded => ReapOutcome::Survived,
                (Ok(now), Some(recorded)) if now.same_os(recorded) => ReapOutcome::NotOurs,
                _ => ReapOutcome::Unverified,
            };
            return Verdict::Done(outcome, Vec::new());
        }
    };
    if !process.is_running() {
        return Verdict::Done(ReapOutcome::AlreadyGone, Vec::new());
    }
    let Ok(creation_time) = process.creation_time() else {
        return Verdict::Done(ReapOutcome::Unverified, Vec::new());
    };
    let now = ProcessStamp::Windows { creation_time };
    match &entry.stamp {
        Some(recorded) if *recorded == now => {
            if let Err(e) = process.terminate(1) {
                tracing::debug!(err = %e, "TerminateProcess on a reaped run failed");
            }
            Verdict::Ours(process)
        }
        Some(recorded) if recorded.same_os(&now) => Verdict::Done(ReapOutcome::NotOurs, Vec::new()),
        _ => Verdict::Done(ReapOutcome::Unverified, Vec::new()),
    }
}

/// The TERM went out at verification: wait, SIGKILL what is left, wait (#29 D4).
#[cfg(unix)]
async fn finish(
    runs: &mut [ReapedRun],
    mut left: Vec<(usize, u32)>,
    start: Instant,
    t: ReapTimings,
) {
    use nix::sys::signal::Signal;

    if left.is_empty() {
        return;
    }
    wait_groups(runs, &mut left, start, start + t.term_grace, t.poll).await;
    if left.is_empty() {
        return;
    }
    // A group that is still alive keeps its pgid, so it is still the verified one.
    for &(i, pgid) in &left {
        runs[i].signal = Some(ReapSignal::Kill);
        if let Err(e) = os::signal_group(pgid, Signal::SIGKILL) {
            tracing::debug!(err = %e, "SIGKILL to a reaped group failed");
        }
    }
    let kill_deadline = Instant::now() + t.kill_wait;
    wait_groups(runs, &mut left, start, kill_deadline, t.poll).await;
    for (i, _) in left {
        runs[i].outcome = ReapOutcome::Survived;
        runs[i].ms = crate::clock::millis(start.elapsed());
    }
}

/// Polls `left` until each group is empty (`killed`) or `deadline` passes.
#[cfg(unix)]
async fn wait_groups(
    runs: &mut [ReapedRun],
    left: &mut Vec<(usize, u32)>,
    start: Instant,
    deadline: Instant,
    poll: Duration,
) {
    loop {
        left.retain(|&(i, pgid)| {
            if os::group_alive(pgid) {
                return true;
            }
            runs[i].outcome = ReapOutcome::Killed;
            runs[i].ms = crate::clock::millis(start.elapsed());
            false
        });
        if left.is_empty() || Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(poll).await;
    }
}

/// Waits, through the verified handles, for the terminated processes to exit. The outcome is
/// `survived` either way: the kernel should already have ended them (#29 D1).
#[cfg(windows)]
async fn finish(
    runs: &mut [ReapedRun],
    left: Vec<(usize, win::Process)>,
    start: Instant,
    t: ReapTimings,
) {
    let deadline = start + t.kill_wait;
    for (i, _) in &left {
        runs[*i].outcome = ReapOutcome::Survived;
    }
    while left.iter().any(|(_, p)| !p.has_exited()) && Instant::now() < deadline {
        tokio::time::sleep(t.poll).await;
    }
    for (i, _) in &left {
        runs[*i].ms = crate::clock::millis(start.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_fields_count_from_the_last_paren() {
        // `comm` with a space and a parenthesis.
        let line = "4242 (my (odd) cmd) S 1 4240 4240 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765 \
                    1000 10 4096";
        let s = os::parse_stat(line).unwrap();
        assert_eq!(s.pgrp, 4240);
        assert_eq!(s.start_ticks, 98765);
        assert_eq!(os::parse_stat("garbage"), None);
    }

    #[test]
    fn procargs2_env_skips_path_padding_and_argv() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/x\0\0\0\0x\0-v\0A=1\0CLAUDECLAW_RUN_ID=r1\0\0garbage");
        let env: Vec<&[u8]> = os::procargs2_env(&buf).collect();
        assert_eq!(env, vec![&b"A=1"[..], b"CLAUDECLAW_RUN_ID=r1"]);
        assert_eq!(os::procargs2_env(&[]).count(), 0);
    }

    #[test]
    fn counts_add_up() {
        let mut c = ReapCounts::default();
        for o in [
            ReapOutcome::Killed,
            ReapOutcome::Killed,
            ReapOutcome::AlreadyGone,
            ReapOutcome::NotOurs,
            ReapOutcome::Unverified,
            ReapOutcome::UnknownPid,
            ReapOutcome::Survived,
        ] {
            c.add(o);
        }
        assert_eq!(
            c,
            ReapCounts {
                killed: 2,
                already_gone: 1,
                not_ours: 1,
                unverified: 1,
                unknown_pid: 1,
                survived: 1,
            }
        );
    }

    #[test]
    fn wire_forms() {
        assert_eq!(
            ReapReason::WorkerExit { graceful: false }.as_str(),
            "worker_exit"
        );
        assert_eq!(ReapReason::Startup.as_str(), "startup");
        assert_eq!(ReapReason::CliStop.as_str(), "cli_stop");
        assert_eq!(ReapOutcome::AlreadyGone.to_string(), "already_gone");
        assert_eq!(ReapOutcome::UnknownPid.as_str(), "unknown_pid");
        assert_eq!(ReapSignal::Term.as_str(), "term");
    }

    fn fast() -> ReapTimings {
        ReapTimings {
            term_grace: Duration::from_millis(500),
            kill_wait: Duration::from_secs(2),
            poll: Duration::from_millis(20),
        }
    }

    fn entry(pid: Option<u32>, stamp: Option<ProcessStamp>) -> ReapEntry {
        ReapEntry {
            run_id: "r1".into(),
            scope: Some("job:demo".into()),
            pid,
            stamp,
            kill: KillMethod::NATIVE,
        }
    }

    #[tokio::test]
    async fn a_spawning_entry_is_unknown_pid() {
        let r = reap_with(&[entry(None, None)], ReapReason::Startup, fast()).await;
        assert_eq!(r.runs[0].outcome, ReapOutcome::UnknownPid);
        assert_eq!(r.counts.unknown_pid, 1);
    }

    #[cfg(unix)]
    mod unix {
        use std::process::Stdio;

        use tokio::process::{Child, Command};

        use super::*;

        /// A process-group leader running `script`, awaited in the background so it never
        /// lingers as a zombie.
        fn leader(script: &str, env: &[(&str, &str)]) -> (u32, tokio::task::JoinHandle<()>) {
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-c")
                .arg(script)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            for (k, v) in env {
                cmd.env(k, v);
            }
            let mut child: Child = cmd.spawn().unwrap();
            let pid = child.id().unwrap();
            let waiter = tokio::spawn(async move {
                let _ = child.wait().await;
            });
            (pid, waiter)
        }

        #[tokio::test]
        async fn a_live_verified_group_is_killed_with_term_first() {
            let (pid, waiter) = leader("exec sleep 30", &[]);
            let stamp = process_stamp(pid).unwrap();
            let r = reap_with(
                &[entry(Some(pid), Some(stamp))],
                ReapReason::CliStop,
                fast(),
            )
            .await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::Killed);
            assert_eq!(r.runs[0].signal, Some(ReapSignal::Term));
            assert!(!group_alive(pid));
            waiter.await.unwrap();
        }

        #[tokio::test]
        async fn a_term_ignoring_group_needs_kill() {
            let dir = tempfile::tempdir().unwrap();
            let mark = dir.path().join("ready");
            let script = format!("trap '' TERM; : > '{}'; exec sleep 30", mark.display());
            let (pid, waiter) = leader(&script, &[]);
            let stamp = process_stamp(pid).unwrap();
            // The trap must be set before the Reap's TERM.
            while !mark.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let r = reap_with(
                &[entry(Some(pid), Some(stamp))],
                ReapReason::CliStop,
                fast(),
            )
            .await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::Killed);
            assert_eq!(r.runs[0].signal, Some(ReapSignal::Kill));
            assert!(r.runs[0].ms >= 500, "KILL only after the TERM grace");
            waiter.await.unwrap();
        }

        #[tokio::test]
        async fn a_dead_pid_is_already_gone() {
            let (pid, waiter) = leader("exit 0", &[]);
            waiter.await.unwrap();
            let stamp = match process_stamp(pid) {
                Err(ReapError::NoSuchProcess) => None,
                other => panic!("expected no such process, got {other:?}"),
            };
            let r = reap_with(&[entry(Some(pid), stamp)], ReapReason::Startup, fast()).await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::AlreadyGone);
            assert_eq!(r.runs[0].signal, None);
        }

        #[tokio::test]
        async fn a_live_pid_with_another_stamp_is_not_ours_and_left_alone() {
            let (pid, waiter) = leader("exec sleep 30", &[]);
            let wrong = match process_stamp(pid).unwrap() {
                ProcessStamp::Macos { sec, usec } => ProcessStamp::Macos { sec: sec - 1, usec },
                ProcessStamp::Linux {
                    boot_id,
                    start_ticks,
                } => ProcessStamp::Linux {
                    boot_id,
                    start_ticks: start_ticks + 1,
                },
                other => other,
            };
            let r = reap_with(
                &[entry(Some(pid), Some(wrong))],
                ReapReason::Startup,
                fast(),
            )
            .await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::NotOurs);
            assert!(group_alive(pid), "not_ours is never signalled");
            signal_group(pid, nix::sys::signal::Signal::SIGKILL).unwrap();
            waiter.await.unwrap();
        }

        /// A leader that exits at once, leaving a member that ignores nothing.
        async fn leader_gone_group(env: &[(&str, &str)]) -> (u32, Vec<u32>) {
            let (pid, waiter) = leader("sleep 30 & exit 0", env);
            waiter.await.unwrap();
            assert!(group_alive(pid));
            let members = group_members(pid);
            (pid, members)
        }

        #[tokio::test]
        async fn a_leader_gone_group_is_ours_right_after_a_worker_exit() {
            let (pid, _) = leader_gone_group(&[]).await;
            let r = reap_with(
                &[entry(Some(pid), None)],
                ReapReason::WorkerExit { graceful: false },
                fast(),
            )
            .await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::Killed);
            assert!(!group_alive(pid));
        }

        #[tokio::test]
        async fn a_leader_gone_group_without_a_marker_is_unverified_at_startup() {
            let (pid, members) = leader_gone_group(&[]).await;
            let r = reap_with(&[entry(Some(pid), None)], ReapReason::Startup, fast()).await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::Unverified);
            assert!(group_alive(pid), "unverified is never signalled");
            if !members.is_empty() {
                assert_eq!(r.runs[0].members, members);
            }
            signal_group(pid, nix::sys::signal::Signal::SIGKILL).unwrap();
        }

        /// macOS hides platform binaries' env (#29 Evidence), so the marker test is Linux-only.
        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn a_leader_gone_group_with_the_marker_is_killed_at_startup() {
            let (pid, _) = leader_gone_group(&[(RUN_ID_ENV, "r1")]).await;
            let r = reap_with(&[entry(Some(pid), None)], ReapReason::Startup, fast()).await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::Killed);
            assert!(!group_alive(pid));
        }
    }

    #[cfg(windows)]
    mod windows {
        use std::process::Stdio;

        use tokio::process::Command;

        use super::*;

        fn system32(exe: &str) -> std::path::PathBuf {
            let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
            std::path::Path::new(&root).join("System32").join(exe)
        }

        #[tokio::test]
        async fn a_live_matching_pid_is_terminated_and_survived() {
            let mut child = Command::new(system32("ping.exe"))
                .args(["-n", "30", "127.0.0.1"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let pid = child.id().unwrap();
            let stamp = process_stamp(pid).unwrap();
            let r = reap_with(
                &[entry(Some(pid), Some(stamp))],
                ReapReason::Startup,
                fast(),
            )
            .await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::Survived);
            assert_eq!(r.runs[0].signal, Some(ReapSignal::Kill));
            let status = child.wait().await.unwrap();
            assert_eq!(status.code(), Some(1));
        }

        #[tokio::test]
        async fn a_live_pid_with_another_stamp_is_not_ours() {
            let mut child = Command::new(system32("ping.exe"))
                .args(["-n", "30", "127.0.0.1"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let pid = child.id().unwrap();
            let ProcessStamp::Windows { creation_time } = process_stamp(pid).unwrap() else {
                panic!("a Windows stamp");
            };
            let wrong = ProcessStamp::Windows {
                creation_time: creation_time - 1,
            };
            let r = reap_with(
                &[entry(Some(pid), Some(wrong))],
                ReapReason::Startup,
                fast(),
            )
            .await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::NotOurs);
            assert!(process_stamp(pid).is_ok(), "left alone");
            child.kill().await.unwrap();
        }

        #[tokio::test]
        async fn a_dead_pid_is_already_gone() {
            let mut child = Command::new(system32("cmd.exe"))
                .args(["/C", "exit 0"])
                .spawn()
                .unwrap();
            let pid = child.id().unwrap();
            let stamp = process_stamp(pid).unwrap();
            child.wait().await.unwrap();
            // `child` still holds its process handle, so the pid can't be reused by a parallel
            // test until the Reap has looked at it.
            let r = reap_with(
                &[entry(Some(pid), Some(stamp))],
                ReapReason::Startup,
                fast(),
            )
            .await;
            assert_eq!(r.runs[0].outcome, ReapOutcome::AlreadyGone);
            assert_eq!(r.runs[0].signal, None);
            drop(child);
        }
    }
}
