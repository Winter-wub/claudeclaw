//! Timers, catch-up, in-flight guard, retries and heartbeat (architecture §4
//! `claudeclaw-scheduler`; #9, #12 §3, #29, #38, #46, #50).
//!
//! One [`Scheduler`] task per Worker decides **when** jobs and the heartbeat run; `core` runs
//! them through the [`Dispatch`] trait it implements. While the scheduler runs it is the only
//! writer of job state (`daemon/state/jobs.json`, #9): `core` changes it through the
//! [`SchedulerHandle`].
//!
//! What it owns:
//! - **Next fire** from each job's cron [`Schedule`](claudeclaw_jobs::Schedule) on the Daemon's
//!   offset, on the wall clock ([`WallClock`]).
//! - **Catch-up** of the latest missed slot within [`CATCH_UP_WINDOW`] (6 h), only for slots
//!   after the job's current schedule took effect, less [`ONE_SHOT_GRACE`] for a One-shot job
//!   (#50). The took-effect time is kept in job state with the timing frontmatter it belongs to,
//!   so a body edit or a `touch`, even while the Daemon is down, doesn't move it; slots missed
//!   while the Daemon was down are caught up. Every slot fired or skipped is recorded
//!   (`slot_handled_through`), so catch-up never replays a slot already decided.
//! - **The in-flight guard**: one run per job; a slot that comes due meanwhile is skipped
//!   (`job.skip {reason: in_flight}`), and changes to the job wait until its run and the
//!   frontmatter guard are done (#46 D6). The guard's restore comes back in [`JobRunEnd`] and is
//!   applied in the same step that ends the run.
//! - **Retries**: `retry` and `retry_delay` as TS ran them, persisted in `jobs.json`.
//! - **The One-shot finish** (#12 §3), written by `core` through
//!   [`Dispatch::finish_one_shot`], once a scheduled slot's chain (its run, retries and re-run)
//!   ends with nothing pending, including when a pending retry or re-run is dropped as older than
//!   6 h. A Run now and its retries never finish a One-shot job.
//! - **Interrupted re-runs**: a run a shutdown interrupted, or that a Reap proved dead
//!   ([`SchedulerInputs::proven_dead`], #29 D6), re-runs within 6 h.
//! - **The heartbeat timer**, with exclude windows ([`HeartbeatConfig`]) and the rate-limit
//!   skip ([`Dispatch::rate_limited_until`]).
//!
//! Timing (#38): every sleep is capped at [`MAX_SLEEP`] (30 s), and due work is re-checked at
//! once on a Resume. `core` feeds [`SchedulerInputs::resumes`] from the runner's Awake clock:
//!
//! ```ignore
//! let (resume_tx, resume_rx) = tokio::sync::watch::channel(0u64);
//! tokio::spawn(async move {
//!     while awake.changed().await.is_ok() {
//!         let resumes = awake.borrow_and_update().resumes;
//!         resume_tx.send_if_modified(|seen| std::mem::replace(seen, resumes) != resumes);
//!     }
//! });
//! ```
//!
//! Log events are #23's, as declared in `claudeclaw_api::log_event::daemon`: `job.fire
//! {scheduled_for, late_ms, catch_up}`, `job.skip {reason}`, `job.retry_scheduled`,
//! `job.retries_exhausted`, `job.oneshot_finished`, `job.state_carried {from, to}`,
//! `heartbeat.fire` and `scheduler.clock_jump {ms}`. The job name is the `job` span field.

mod change;
mod clock;
mod dispatch;
mod due;
mod heartbeat;
mod scheduler;
#[cfg(test)]
mod tests;

pub use change::JobChange;
pub use clock::{SystemClock, WallClock};
pub use dispatch::{Dispatch, JobFire, JobRunEnd, OneShotFinish, RunNow, Trigger};
pub use due::{CATCH_UP_AFTER, CATCH_UP_WINDOW, ONE_SHOT_GRACE};
pub use heartbeat::{ExcludeWindow, HeartbeatConfig, MIN_HEARTBEAT_INTERVAL};
pub use scheduler::{
    CLOCK_JUMP_THRESHOLD, MAX_SLEEP, Scheduler, SchedulerGone, SchedulerHandle, SchedulerInputs,
    SchedulerSettings,
};
