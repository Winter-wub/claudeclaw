//! The scheduler task: timers, catch-up, the in-flight guard, retries, the One-shot finish and
//! the heartbeat (architecture §4 `scheduler`, #9, #12 §3, #29, #38, #46, #50).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use claudeclaw_jobs::{JobFields, JobFile};
use claudeclaw_store::{JobState, Layout};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::change::JobChange;
use crate::clock::WallClock;
use crate::dispatch::{Dispatch, JobFire, JobRunEnd, OneShotFinish, RunNow, Trigger};
use crate::due::{self, CATCH_UP_WINDOW, SlotRule};
use crate::heartbeat::HeartbeatConfig;

/// The longest the scheduler sleeps before re-checking (#38, architecture §4 `scheduler`
/// "Timing"). Tokio's clock stops during system sleep on macOS and Linux, so a wake or a
/// wall-clock step is noticed within this long even without a Resume.
pub const MAX_SLEEP: Duration = Duration::from_secs(30);

/// A wall-clock move this far from tokio's monotonic clock between two checks is logged as
/// `scheduler.clock_jump` (#23). It matches the Resume threshold (#38 D3).
pub const CLOCK_JUMP_THRESHOLD: Duration = Duration::from_secs(10);

/// The settings the scheduler reads, from `settings.json` through `core::settings` (§7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerSettings {
    /// The Daemon's time zone offset: cron fields and exclude windows are read on it.
    pub offset: FixedOffset,
    pub heartbeat: HeartbeatConfig,
}

/// Everything [`Scheduler::new`] needs (architecture §4 `scheduler` "Inputs").
pub struct SchedulerInputs<D> {
    pub dispatch: Arc<D>,
    pub clock: Arc<dyn WallClock>,
    /// Where `jobs.json` is saved. While the scheduler runs it is the only writer of job state
    /// in the Worker; `core` changes it through [`SchedulerHandle`].
    pub layout: Layout,
    /// Job state as loaded at Worker start (`JobState::load_merged`, #14 rule 6).
    pub state: JobState,
    /// Jobs whose run a previous Worker left in flight and the Reap proved dead (`killed`,
    /// `already_gone` or `not_ours` in `runs.json.reaped`, #29 D6). They re-run within 6 h.
    /// Any other job left in flight is not re-run; its slot counts as run.
    pub proven_dead: BTreeSet<String>,
    pub settings: watch::Receiver<SchedulerSettings>,
    /// Bumped on every Resume (#38 D3): the scheduler re-checks due jobs at once. `core` feeds it
    /// from the runner's Awake clock, e.g. with `tx.send_if_modified` whenever
    /// `AwakeTime::resumes` grows. Any change triggers a re-check; the value is not read.
    pub resumes: watch::Receiver<u64>,
}

/// The scheduler is gone, so the command was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the scheduler has stopped")]
pub struct SchedulerGone;

/// How `core` talks to a running [`Scheduler`]. Cheap to clone.
#[derive(Debug, Clone)]
pub struct SchedulerHandle {
    tx: mpsc::Sender<Command>,
}

enum Command {
    Apply(Vec<JobChange>),
    Forget(String),
    RunNow(String, oneshot::Sender<RunNow>),
    /// The job state as the scheduler holds it, for tests that must not read `jobs.json` while
    /// it is being renamed over (Windows refuses the rename).
    #[cfg(test)]
    Snapshot(oneshot::Sender<JobState>),
}

impl SchedulerHandle {
    /// Applies one Change scan pass, or one of the Daemon's own writes (#46 D4, D5). Changes to
    /// a job in flight are held back until its run and the frontmatter guard are done (#46
    /// D6); the latest one is then applied. Within one pass, a removed job whose identical
    /// bytes appear under a new name in the same dir hands its `last_started_at` and
    /// `last_result` to the new name (`job.state_carried`, #46 D4).
    pub async fn apply(&self, changes: Vec<JobChange>) -> Result<(), SchedulerGone> {
        self.tx
            .send(Command::Apply(changes))
            .await
            .map_err(|_| SchedulerGone)
    }

    /// Drops a job and its state at once: a Management API delete (#12 §6) or the 30-day
    /// orphan prune (#46 D4). A job in flight is dropped when its run ends.
    pub async fn forget(&self, job: impl Into<String>) -> Result<(), SchedulerGone> {
        self.tx
            .send(Command::Forget(job.into()))
            .await
            .map_err(|_| SchedulerGone)
    }

    /// Run now (#50): starts the job at once, unless it is already running (the in-flight
    /// guard). Runs a disabled job too. It never counts as the One-shot finish.
    pub async fn run_now(&self, job: impl Into<String>) -> RunNow {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Command::RunNow(job.into(), reply))
            .await
            .is_err()
        {
            return RunNow::Stopping;
        }
        rx.await.unwrap_or(RunNow::Stopping)
    }

    #[cfg(test)]
    pub(crate) async fn snapshot(&self) -> JobState {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Command::Snapshot(reply)).await;
        rx.await.expect("the scheduler is running")
    }
}

/// A loaded job.
struct JobEntry {
    file: Arc<JobFile>,
    /// When its current schedule took effect (#50).
    took_effect: DateTime<Utc>,
}

/// The frontmatter that decides timing: an edit that changes it re-sets the took-effect time
/// (#50), a body-only edit or a `touch` doesn't. Persisted as `JobRecord::schedule_key`, so this
/// also holds across a Daemon restart.
pub(crate) fn schedule_key(file: &JobFile) -> String {
    let f = file.fields();
    format!(
        "{}|{:?}|{}|{}",
        f.schedule.as_ref().map_or("", |s| s.as_str()),
        f.recurring,
        f.is_enabled(),
        file.is_valid(),
    )
}

impl JobEntry {
    fn rule(&self, covered_through: Option<DateTime<Utc>>) -> Option<SlotRule<'_>> {
        let fields = self.file.fields();
        if !self.file.is_valid() || !fields.is_enabled() {
            return None;
        }
        Some(SlotRule {
            schedule: fields.schedule.as_ref()?,
            one_shot: fields.is_one_shot(),
            took_effect: self.took_effect,
            covered_through,
        })
    }
}

/// A job whose run (or One-shot finish) is under way.
struct InFlight {
    /// The settings the run started with: retries and the finish follow them (TS
    /// `src/commands/start.ts:916-970` captures `job` at fire time).
    fields: JobFields,
    finishing: bool,
    /// The latest change that arrived meanwhile (#46 D6).
    held: Option<Held>,
}

enum Held {
    Change(JobChange),
    Forget,
}

enum Done {
    Run {
        name: String,
        end: Box<JobRunEnd>,
    },
    Finish {
        name: String,
        outcome: OneShotFinish,
    },
}

fn job_span(name: &str) -> tracing::Span {
    tracing::info_span!("scheduler.job", job = %name, origin = "scheduler")
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn same_dir(a: &str, b: &str) -> bool {
    a.rsplit_once('/').map(|(agent, _)| agent) == b.rsplit_once('/').map(|(agent, _)| agent)
}

/// The scheduler (architecture §4 `scheduler`). Build it with [`Scheduler::new`] and drive it
/// with [`Scheduler::run`] on its own task.
pub struct Scheduler<D: Dispatch> {
    dispatch: Arc<D>,
    clock: Arc<dyn WallClock>,
    layout: Layout,
    state: JobState,
    settings_rx: watch::Receiver<SchedulerSettings>,
    settings: SchedulerSettings,
    resumes: watch::Receiver<u64>,
    commands: mpsc::Receiver<Command>,
    done_tx: mpsc::UnboundedSender<Done>,
    done_rx: mpsc::UnboundedReceiver<Done>,
    jobs: BTreeMap<String, JobEntry>,
    in_flight: BTreeMap<String, InFlight>,
    next_heartbeat: Option<DateTime<Utc>>,
    tracker: TaskTracker,
    /// Runs marked in flight but not yet handed to `core`: they start once `jobs.json` says so.
    pending: Vec<JobFire>,
    last_check: Option<(DateTime<Utc>, Instant)>,
    stopping: bool,
    dirty: bool,
}

impl<D: Dispatch> Scheduler<D> {
    /// Builds the scheduler and its handle. Runs a previous Worker left in flight are settled
    /// here (#29 D6): each counts as started, and becomes interrupted (re-run within 6 h) only
    /// if it is in `proven_dead`.
    pub fn new(inputs: SchedulerInputs<D>) -> (Self, SchedulerHandle) {
        let (tx, commands) = mpsc::channel(64);
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let mut state = inputs.state;
        let mut dirty = false;
        for (name, rec) in &mut state.jobs {
            let Some(since) = rec.in_flight_since.take() else {
                continue;
            };
            dirty = true;
            rec.last_started_at = rec.last_started_at.max(Some(since));
            if inputs.proven_dead.contains(name) {
                rec.interrupted_at = Some(since);
            } else {
                tracing::info!(
                    job = %name,
                    "a previous Worker's run was not proved dead; it is not re-run"
                );
            }
        }
        let settings = inputs.settings.borrow().clone();
        let scheduler = Scheduler {
            dispatch: inputs.dispatch,
            clock: inputs.clock,
            layout: inputs.layout,
            state,
            settings_rx: inputs.settings,
            settings,
            resumes: inputs.resumes,
            commands,
            done_tx,
            done_rx,
            jobs: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            next_heartbeat: None,
            tracker: TaskTracker::new(),
            pending: Vec::new(),
            last_check: None,
            stopping: false,
            dirty,
        };
        (scheduler, SchedulerHandle { tx })
    }

    /// Runs until `cancel` fires, then stops starting runs, waits for every run it started to
    /// end (core's shutdown kills them, architecture §5), saves job state and returns.
    pub async fn run(mut self, cancel: CancellationToken) {
        let now = self.clock.now();
        self.reanchor_heartbeat(now);
        loop {
            let now = self.clock.now();
            self.note_clock_jump(now);
            if !self.stopping {
                self.check(now);
            }
            self.persist().await;
            self.launch();
            if self.stopping && self.in_flight.is_empty() {
                break;
            }
            let sleep = self.sleep_for(now);
            tokio::select! {
                biased;
                () = cancel.cancelled(), if !self.stopping => self.stopping = true,
                Some(done) = self.done_rx.recv() => self.on_done(done),
                Some(command) = self.commands.recv() => self.on_command(command),
                Ok(()) = self.resumes.changed() => {}
                Ok(()) = self.settings_rx.changed() => self.on_settings(),
                () = tokio::time::sleep(sleep) => {}
            }
        }
        self.tracker.close();
        self.tracker.wait().await;
        self.persist().await;
    }

    /// Saves `jobs.json` if anything changed (#9: whole, atomically, whenever it changes). A
    /// failed save is logged and retried on the next pass.
    async fn persist(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let state = self.state.clone();
        let layout = self.layout.clone();
        let saved = tokio::task::spawn_blocking(move || state.save(&layout)).await;
        let failed = match saved {
            Ok(Ok(())) => return,
            Ok(Err(e)) => e.to_string(),
            Err(e) => e.to_string(),
        };
        tracing::error!(err = %failed, "can't save job state; retrying");
        self.dirty = true;
    }

    /// Logs `scheduler.clock_jump` when the wall clock moved unlike tokio's clock (#23): a
    /// manual change, an NTP step, or sleep on a platform whose tokio clock stops in it.
    fn note_clock_jump(&mut self, now: DateTime<Utc>) {
        let instant = Instant::now();
        if let Some((wall, then)) = self.last_check {
            let wall_ms = (now - wall).num_milliseconds();
            let mono_ms =
                i64::try_from(instant.duration_since(then).as_millis()).unwrap_or(i64::MAX);
            let jump = wall_ms.saturating_sub(mono_ms);
            if jump.unsigned_abs() >= CLOCK_JUMP_THRESHOLD.as_millis() as u64 {
                tracing::warn!(
                    event = "scheduler.clock_jump",
                    ms = jump,
                    "the wall clock moved; re-checking the schedule"
                );
            }
        }
        self.last_check = Some((now, instant));
    }

    /// How long to sleep: until the next deadline after `now`, at most [`MAX_SLEEP`]. Work that
    /// is due but blocked (in flight, rate-limited) waits for its own event, not a deadline.
    fn sleep_for(&self, now: DateTime<Utc>) -> Duration {
        let mut next: Option<DateTime<Utc>> = None;
        let mut consider = |t: Option<DateTime<Utc>>| {
            if let Some(t) = t.filter(|t| *t > now) {
                next = Some(next.map_or(t, |n| n.min(t)));
            }
        };
        consider(self.next_heartbeat);
        consider(self.dispatch.rate_limited_until());
        for (name, entry) in &self.jobs {
            if let Some(rule) = entry.rule(None) {
                consider(due::next_slot(rule.schedule, now, self.settings.offset));
            }
            consider(self.state.get(name).and_then(|r| r.retry_at));
        }
        next.and_then(|t| (t - now).to_std().ok())
            .unwrap_or(MAX_SLEEP)
            .min(MAX_SLEEP)
    }

    fn check(&mut self, now: DateTime<Utc>) {
        let limited = self
            .dispatch
            .rate_limited_until()
            .is_some_and(|until| until > now);
        self.check_heartbeat(now, limited);
        let names: Vec<String> = self.jobs.keys().cloned().collect();
        for name in names {
            self.check_job(&name, now, limited);
        }
    }

    /// TS `scheduleHeartbeat`'s tick (`src/commands/start.ts:681-750`): rate-limited or inside an
    /// exclude window it is skipped; either way the next one is an interval later.
    fn check_heartbeat(&mut self, now: DateTime<Utc>, limited: bool) {
        if self.next_heartbeat.is_none_or(|next| next > now) {
            return;
        }
        let _span = tracing::info_span!("scheduler.heartbeat", origin = "heartbeat").entered();
        let local = now.with_timezone(&self.settings.offset);
        if limited {
            tracing::info!(
                event = "job.skip",
                reason = "rate_limited",
                "heartbeat skipped"
            );
        } else if self.settings.heartbeat.is_excluded(local) {
            tracing::info!(event = "job.skip", reason = "excluded", "heartbeat skipped");
        } else {
            tracing::info!(event = "heartbeat.fire", "heartbeat");
            let dispatch = self.dispatch.clone();
            self.tracker
                .spawn(async move { dispatch.heartbeat().await });
        }
        self.next_heartbeat = Some(now + due::chrono(self.settings.heartbeat.effective_interval()));
    }

    fn reanchor_heartbeat(&mut self, now: DateTime<Utc>) {
        let hb = &self.settings.heartbeat;
        self.next_heartbeat = hb
            .enabled
            .then(|| now + due::chrono(hb.effective_interval()));
    }

    fn check_job(&mut self, name: &str, now: DateTime<Utc>, limited: bool) {
        let Some(entry) = self.jobs.get(name) else {
            return;
        };
        if !entry.file.is_valid() {
            return;
        }
        let enabled = entry.file.fields().is_enabled();
        let rec = self.state.get(name);
        let covered = rec
            .and_then(|r| r.last_started_at)
            .max(rec.and_then(|r| r.slot_handled_through));
        let slot = entry
            .rule(covered)
            .and_then(|rule| rule.due(now, self.settings.offset));
        let retry_at = rec.and_then(|r| r.retry_at).filter(|at| *at <= now);
        let interrupted_at = rec.and_then(|r| r.interrupted_at);
        let window = due::chrono(CATCH_UP_WINDOW);

        if self.in_flight.contains_key(name) {
            if let Some(slot) = slot {
                let _span = job_span(name).entered();
                tracing::info!(event = "job.skip", reason = "in_flight", "slot skipped");
                drop(_span);
                self.consume(name, Some(slot));
            }
            return;
        }

        // TS fires a due retry before the cron slot (`src/commands/start.ts:978-988`).
        if let Some(at) = retry_at {
            if now - at > window {
                tracing::info!(job = %name, "a pending retry is older than 6 h; dropped");
                self.clear_retry(name);
                if self.ends_one_shot(name) {
                    return;
                }
            } else if !enabled {
                let _span = job_span(name).entered();
                tracing::info!(event = "job.skip", reason = "disabled", "retry skipped");
                self.clear_retry(name);
            } else if !limited {
                self.cover(name, slot);
                self.fire(name, Trigger::Retry, at, now);
                return;
            }
        }

        // #9, #29 D6: a run a shutdown interrupted or a Reap proved dead.
        if let Some(at) = interrupted_at {
            if now - at > window {
                tracing::info!(job = %name, "an interrupted run is older than 6 h; not re-run");
                self.state.entry(name).interrupted_at = None;
                self.dirty = true;
                if self.ends_one_shot(name) {
                    return;
                }
            } else if !enabled {
                let _span = job_span(name).entered();
                tracing::info!(event = "job.skip", reason = "disabled", "re-run skipped");
                self.state.entry(name).interrupted_at = None;
                self.dirty = true;
            } else if !limited {
                self.cover(name, slot);
                self.fire(name, Trigger::Interrupted, at, now);
                return;
            }
        }

        let Some(slot) = slot else {
            return;
        };
        let catch_up = due::is_catch_up(slot, now);
        if !limited {
            let trigger = if catch_up {
                Trigger::CatchUp
            } else {
                Trigger::Scheduled
            };
            self.consume(name, Some(slot));
            self.fire(name, trigger, slot, now);
        } else if !catch_up {
            // TS records an on-time slot during a rate limit as run, so it isn't caught up later;
            // a missed slot found during the limit waits (`src/commands/start.ts:1010-1022`).
            let _span = job_span(name).entered();
            tracing::info!(event = "job.skip", reason = "rate_limited", "slot skipped");
            self.consume(name, Some(slot));
        }
    }

    /// Records `slot` as decided (fired or skipped) in job state, so catch-up never replays it,
    /// not even after a restart (#9).
    fn consume(&mut self, name: &str, slot: Option<DateTime<Utc>>) {
        if let Some(slot) = slot {
            let rec = self.state.entry(name);
            rec.slot_handled_through = rec.slot_handled_through.max(Some(slot));
            self.dirty = true;
        }
    }

    /// A retry or re-run that also covers a due slot becomes that slot's run, so it no longer
    /// continues a Run now (#12 §3).
    fn cover(&mut self, name: &str, slot: Option<DateTime<Utc>>) {
        if slot.is_some() {
            self.state.entry(name).run_now = false;
        }
        self.consume(name, slot);
    }

    /// When a One-shot job's pending retry or re-run is dropped, its chain has ended with nothing
    /// pending, so it is finished like a run that ended (#12 §3). Not for a Run now chain, nor
    /// while another retry or re-run is still pending. Returns whether the finish started.
    fn ends_one_shot(&mut self, name: &str) -> bool {
        let Some(entry) = self.jobs.get(name) else {
            return false;
        };
        let fields = entry.file.fields().clone();
        let rec = self.state.entry(name);
        let pending = rec.retry_at.is_some() || rec.interrupted_at.is_some();
        if !fields.is_one_shot() || !fields.is_enabled() || rec.run_now || pending {
            return false;
        }
        self.in_flight.insert(
            name.to_owned(),
            InFlight {
                fields,
                finishing: true,
                held: None,
            },
        );
        self.start_finish(name);
        true
    }

    /// Hands the One-shot finish to `core`; the job stays in flight until it returns.
    fn start_finish(&mut self, name: &str) {
        let dispatch = self.dispatch.clone();
        let done = self.done_tx.clone();
        let name = name.to_owned();
        self.tracker.spawn(async move {
            let outcome = dispatch.finish_one_shot(name.clone()).await;
            let _ = done.send(Done::Finish { name, outcome });
        });
    }

    fn clear_retry(&mut self, name: &str) {
        let rec = self.state.entry(name);
        rec.retry_at = None;
        rec.fail_count = 0;
        self.dirty = true;
    }

    /// Starts a run: marks it in flight; [`Self::launch`] hands it to `core` once `jobs.json`
    /// says so (#9).
    fn fire(
        &mut self,
        name: &str,
        trigger: Trigger,
        scheduled_for: DateTime<Utc>,
        now: DateTime<Utc>,
    ) {
        let Some(entry) = self.jobs.get(name) else {
            return;
        };
        let file = entry.file.clone();
        let rec = self.state.entry(name);
        rec.in_flight_since = Some(now);
        rec.last_started_at = Some(now);
        // #12 §3: only a scheduled slot's chain (its run, retries and re-run) finishes a
        // One-shot job. A Run now while that chain has a retry or re-run pending continues it.
        match trigger {
            Trigger::Retry => rec.retry_at = None,
            Trigger::Interrupted => rec.interrupted_at = None,
            Trigger::Scheduled | Trigger::CatchUp => rec.run_now = false,
            Trigger::RunNow => {
                if rec.retry_at.is_none() && rec.interrupted_at.is_none() {
                    rec.run_now = true;
                }
            }
        }
        self.dirty = true;
        {
            let _span = job_span(name).entered();
            let late_ms = u64::try_from((now - scheduled_for).num_milliseconds()).unwrap_or(0);
            tracing::info!(
                event = "job.fire",
                scheduled_for = %rfc3339(scheduled_for),
                late_ms,
                catch_up = matches!(trigger, Trigger::CatchUp | Trigger::Interrupted),
                "job fired"
            );
        }
        self.in_flight.insert(
            name.to_owned(),
            InFlight {
                fields: file.fields().clone(),
                finishing: false,
                held: None,
            },
        );
        self.pending.push(JobFire {
            name: name.to_owned(),
            file,
            scheduled_for,
            trigger,
        });
    }

    /// Hands the runs marked in flight to `core`, after [`Self::persist`].
    fn launch(&mut self) {
        for fire in self.pending.drain(..) {
            let dispatch = self.dispatch.clone();
            let done = self.done_tx.clone();
            self.tracker.spawn(async move {
                let name = fire.name.clone();
                let end = dispatch.run_job(fire).await;
                let _ = done.send(Done::Run {
                    name,
                    end: Box::new(end),
                });
            });
        }
    }

    fn on_done(&mut self, done: Done) {
        match done {
            Done::Run { name, end } => self.on_run_end(&name, *end),
            Done::Finish { name, outcome } => {
                let _span = job_span(&name).entered();
                match outcome {
                    OneShotFinish::Finished => {
                        tracing::info!(event = "job.oneshot_finished", "one-shot job disabled")
                    }
                    OneShotFinish::Skipped => {
                        tracing::info!("one-shot finish skipped: the file changed or is gone")
                    }
                    OneShotFinish::Failed => tracing::warn!("one-shot finish failed"),
                }
                drop(_span);
                self.release(&name);
            }
        }
    }

    fn on_run_end(&mut self, name: &str, end: JobRunEnd) {
        let Some(flight) = self.in_flight.get_mut(name) else {
            return;
        };
        let now = self.clock.now();
        let rec = self.state.entry(name);
        rec.in_flight_since = None;
        self.dirty = true;
        let _span = job_span(name).entered();
        let (finished, restored) = match end {
            JobRunEnd::Interrupted {
                started_at,
                restored,
            } => {
                rec.last_started_at = Some(started_at);
                rec.interrupted_at = Some(started_at);
                (false, restored)
            }
            JobRunEnd::Finished {
                started_at,
                result,
                ok,
                restored,
            } => {
                rec.last_started_at = Some(started_at);
                rec.last_result = Some(result);
                // TS `runJob` (`src/commands/start.ts:940-957`), with the state persisted (#9).
                if ok {
                    rec.fail_count = 0;
                    rec.retry_at = None;
                } else if let Some(retry) = flight.fields.retry.filter(|r| *r > 0) {
                    rec.fail_count += 1;
                    if rec.fail_count <= retry {
                        let delay = flight.fields.retry_delay_or_default();
                        rec.retry_at = Some(now + chrono::Duration::seconds(delay as i64));
                        tracing::warn!(
                            event = "job.retry_scheduled",
                            "run failed; retry {} of {retry} in {delay} s",
                            rec.fail_count
                        );
                    } else {
                        rec.fail_count = 0;
                        rec.retry_at = None;
                        tracing::error!(
                            event = "job.retries_exhausted",
                            "run failed; all {retry} retries used"
                        );
                    }
                }
                (true, restored)
            }
        };
        // #46 D6: the guard's restore is the job's one change, applied with the release.
        // An API delete or orphan prune held back meanwhile wins over the restore (#12 §6).
        if let Some(restored) = restored
            && !matches!(flight.held, Some(Held::Forget))
        {
            flight.held = Some(Held::Change(restored));
        }
        // #12 §3: a One-shot job whose scheduled chain ends with no retry pending is finished.
        // TS `src/commands/start.ts:962-971`.
        let finish =
            finished && flight.fields.is_one_shot() && rec.retry_at.is_none() && !rec.run_now;
        drop(_span);
        if finish {
            flight.finishing = true;
            self.start_finish(name);
        } else {
            self.release(name);
        }
    }

    /// The run is over: apply what was held back (#46 D6).
    fn release(&mut self, name: &str) {
        let Some(flight) = self.in_flight.remove(name) else {
            return;
        };
        let now = self.clock.now();
        match flight.held {
            Some(Held::Change(change)) => self.apply_one(change, now),
            Some(Held::Forget) => self.forget(name),
            None => {}
        }
    }

    fn on_command(&mut self, command: Command) {
        let now = self.clock.now();
        match command {
            Command::Apply(changes) => self.apply(changes, now),
            Command::Forget(name) => match self.in_flight.get_mut(&name) {
                Some(flight) => flight.held = Some(Held::Forget),
                None => self.forget(&name),
            },
            Command::RunNow(name, reply) => {
                let _ = reply.send(self.run_now(&name, now));
            }
            #[cfg(test)]
            Command::Snapshot(reply) => {
                let _ = reply.send(self.state.clone());
            }
        }
    }

    fn run_now(&mut self, name: &str, now: DateTime<Utc>) -> RunNow {
        if self.stopping {
            return RunNow::Stopping;
        }
        let Some(entry) = self.jobs.get(name) else {
            return RunNow::Unknown;
        };
        if !entry.file.is_valid() {
            return RunNow::Invalid;
        }
        if self.in_flight.contains_key(name) {
            let _span = job_span(name).entered();
            tracing::info!(event = "job.skip", reason = "in_flight", "run now refused");
            return RunNow::InFlight;
        }
        self.fire(name, Trigger::RunNow, now, now);
        RunNow::Started
    }

    fn apply(&mut self, changes: Vec<JobChange>, now: DateTime<Utc>) {
        // #46 D4 rename carry-over: removed, and identical bytes added in the same dir.
        let mut removed: Vec<(String, String)> = changes
            .iter()
            .filter_map(|c| match c {
                JobChange::Removed { name } if !self.in_flight.contains_key(name) => self
                    .jobs
                    .get(name)
                    .map(|e| (name.clone(), e.file.text().to_owned())),
                _ => None,
            })
            .collect();
        let mut carries = Vec::new();
        for change in &changes {
            let JobChange::Updated { name, file, .. } = change else {
                continue;
            };
            if self.jobs.contains_key(name) || self.in_flight.contains_key(name) {
                continue;
            }
            if let Some(pos) = removed
                .iter()
                .position(|(from, text)| same_dir(from, name) && text == file.text())
            {
                carries.push((removed.swap_remove(pos).0, name.clone()));
            }
        }
        // The sources' records as they were: removal clears their took-effect time.
        let sources: Vec<_> = carries
            .into_iter()
            .filter_map(|(from, to)| Some((self.state.get(&from).cloned()?, from, to)))
            .collect();
        for change in changes {
            self.apply_one(change, now);
        }
        for (src, from, to) in sources {
            let dst = self.state.entry(&to);
            if dst.last_started_at.is_some() || src.last_started_at.is_none() {
                continue;
            }
            dst.last_started_at = src.last_started_at;
            dst.last_result = src.last_result;
            dst.slot_handled_through = src.slot_handled_through;
            dst.schedule_key = src.schedule_key;
            dst.schedule_took_effect = src.schedule_took_effect;
            if let Some(entry) = self.jobs.get_mut(&to)
                && let Some(took) = dst.schedule_took_effect
            {
                entry.took_effect = took;
            }
            self.dirty = true;
            tracing::info!(event = "job.state_carried", from = %from, to = %to, "job renamed");
        }
    }

    fn apply_one(&mut self, change: JobChange, now: DateTime<Utc>) {
        if let Some(flight) = self.in_flight.get_mut(change.name()) {
            tracing::debug!(job = %change.name(), "change held back until the run ends");
            flight.held = Some(Held::Change(change));
            return;
        }
        match change {
            JobChange::Updated {
                name,
                file,
                took_effect,
                revision,
            } => {
                let rec = self.state.entry(&name);
                rec.orphaned_since = None;
                if let Some(text) = revision
                    && let Ok(rev) = text.parse()
                {
                    rec.revision = Some(rev);
                }
                // #50: the schedule took effect when its timing frontmatter last changed, as
                // recorded in job state, so neither a body edit nor a restart moves it.
                let key = schedule_key(&file);
                let took_effect = match rec.schedule_took_effect {
                    Some(took) if rec.schedule_key.as_deref() == Some(key.as_str()) => took,
                    _ => took_effect,
                };
                rec.schedule_key = Some(key);
                rec.schedule_took_effect = Some(took_effect);
                self.dirty = true;
                self.jobs.insert(name, JobEntry { file, took_effect });
            }
            JobChange::Removed { name } => {
                self.jobs.remove(&name);
                // #46 D4: an Orphaned job keeps its state, minus retries and re-runs. Its
                // took-effect time goes too: a job that reappears takes effect then (#50), unless
                // a rename carries it over in the same pass.
                if let Some(rec) = self.state.jobs.get_mut(&name) {
                    rec.orphaned_since.get_or_insert(now);
                    rec.retry_at = None;
                    rec.fail_count = 0;
                    rec.interrupted_at = None;
                    rec.schedule_key = None;
                    rec.schedule_took_effect = None;
                    self.dirty = true;
                }
            }
        }
    }

    fn forget(&mut self, name: &str) {
        self.jobs.remove(name);
        if self.state.remove(name).is_some() {
            self.dirty = true;
        }
    }

    fn on_settings(&mut self) {
        let new = self.settings_rx.borrow_and_update().clone();
        if new == self.settings {
            return;
        }
        // TS re-arms the heartbeat when its settings or the offset change
        // (`src/commands/start.ts:787-806`).
        let reanchor =
            new.heartbeat != self.settings.heartbeat || new.offset != self.settings.offset;
        self.settings = new;
        if reanchor {
            self.reanchor_heartbeat(self.clock.now());
        }
    }
}
