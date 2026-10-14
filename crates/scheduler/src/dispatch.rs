//! The seam `core` implements: the scheduler decides, `core` runs and writes (architecture §4
//! `scheduler` "Output").

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use claudeclaw_jobs::JobFile;
use claudeclaw_store::JobResult;

/// What the scheduler calls (architecture §4 `scheduler`, §2 "Async traits": native `async fn`
/// in traits, static dispatch). `core` implements it over `core::conversations`; an
/// implementation may write each method as `async fn`.
pub trait Dispatch: Send + Sync + 'static {
    /// Runs one job and returns once its run has ended **and** the frontmatter guard has run
    /// (#46 D6): frontmatter changes the scheduler held back for the job are applied right
    /// after. A Refused run is a [`JobRunEnd::Finished`] with a failed result (#26). During a
    /// Worker shutdown the run is killed and this returns [`JobRunEnd::Interrupted`] (#9,
    /// architecture §5 "Shutdown" step 4); the scheduler waits for every run it started before
    /// [`Scheduler::run`](crate::Scheduler::run) returns.
    fn run_job(&self, fire: JobFire) -> impl Future<Output = JobRunEnd> + Send;

    /// Runs the heartbeat: `core` resolves the prompt, runs it and decides what to forward (TS
    /// `src/commands/start.ts:698-750`). The scheduler has already applied the exclude windows
    /// and the rate-limit skip. As in TS there is no in-flight guard for the heartbeat. A
    /// shutdown waits for it, like a job run.
    fn heartbeat(&self) -> impl Future<Output = ()> + Send;

    /// The One-shot finish (#12 §3): set `enabled: false` with
    /// [`claudeclaw_jobs::finish_one_shot`] through the Daemon's job-file write path (#46 D5).
    /// The scheduler decides when; `core` skips the write when the file is gone or no longer
    /// says `recurring: false` (#46 D4) and reports [`OneShotFinish::Skipped`]. The write's
    /// own [`JobChange`](crate::JobChange) reaches the scheduler like any other.
    fn finish_one_shot(&self, job: String) -> impl Future<Output = OneShotFinish> + Send;

    /// Until when Claude Code is rate-limited (`ratelimit.set`, #23), or `None`. While it is in
    /// the future, the heartbeat and on-time job slots are skipped (`job.skip
    /// {reason: rate_limited}`), and retries, interrupted re-runs and catch-up wait (TS
    /// `src/commands/start.ts:699-708`, `1010-1022`). The scheduler re-checks at that time.
    fn rate_limited_until(&self) -> Option<DateTime<Utc>>;
}

/// Why a job run starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Its slot came due while the scheduler was watching.
    Scheduled,
    /// A slot missed while the Daemon was down, asleep or busy, within the 6 h window (#9).
    CatchUp,
    /// A retry after a failed run (`retry`, `retry_delay`).
    Retry,
    /// A re-run of a run that a shutdown interrupted or a Reap proved dead (#9, #29).
    Interrupted,
    /// Run now, from the Management API or the CLI (#50).
    RunNow,
}

/// One job run the scheduler starts. Its `Debug` shows no prompt (architecture §2).
#[derive(Clone)]
pub struct JobFire {
    /// The job name.
    pub name: String,
    /// The job as the scheduler last applied it; the run uses this prompt and these settings
    /// even if the file changes while it runs (#46 D4).
    pub file: Arc<JobFile>,
    /// The slot this run is for (the retry time for a retry, the start of the interrupted run
    /// for a re-run, now for Run now).
    pub scheduled_for: DateTime<Utc>,
    pub trigger: Trigger,
}

impl fmt::Debug for JobFire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobFire")
            .field("name", &self.name)
            .field("text_bytes", &self.file.text().len())
            .field("scheduled_for", &self.scheduled_for)
            .field("trigger", &self.trigger)
            .finish()
    }
}

/// How a job run ended, as [`Dispatch::run_job`] reports it.
#[derive(Debug, Clone, PartialEq)]
pub enum JobRunEnd {
    /// The run ended on its own (or was refused).
    Finished {
        /// The run's start, the instant its Run log is named by: stored as `last_started_at`
        /// so the per-start merge with Run logs sees the same run (#14 rule 6).
        started_at: DateTime<Utc>,
        /// Stored as `last_result`.
        result: JobResult,
        /// Whether the run succeeded. A failure counts toward the job's `retry`.
        ok: bool,
    },
    /// A Worker shutdown killed the run: it is recorded as interrupted, and catch-up re-runs
    /// it within 6 h (#9).
    Interrupted {
        /// The run's start, as for [`JobRunEnd::Finished`].
        started_at: DateTime<Utc>,
    },
}

/// What [`Dispatch::finish_one_shot`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneShotFinish {
    /// `enabled: false` was written; logged as `job.oneshot_finished`.
    Finished,
    /// Nothing to write: the file is gone, or no longer a One-shot job (#46 D4).
    Skipped,
    /// The write failed; `core` logged why. The job stays enabled, and its slot stays consumed.
    Failed,
}

/// The answer to [`SchedulerHandle::run_now`](crate::SchedulerHandle::run_now).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunNow {
    /// The run started.
    Started,
    /// The job is already running: the in-flight guard refused a second run.
    InFlight,
    /// No job of that name is loaded.
    Unknown,
    /// The job file is invalid (#12 §4).
    Invalid,
    /// The scheduler is shutting down, or gone.
    Stopping,
}
