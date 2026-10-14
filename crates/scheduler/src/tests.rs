//! The scheduler on tokio's paused clock with a fake wall clock (architecture §4 `scheduler`
//! "Tests").

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use claudeclaw_jobs::JobFile;
use claudeclaw_store::{JobResult, JobState, Layout};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

use crate::*;

// --- Harness -------------------------------------------------------------------------------

/// A wall clock that moves with tokio's paused clock, plus jumps that tokio doesn't see (a
/// clock step, or sleep on a platform whose tokio clock stops in it).
struct FakeClock {
    base: DateTime<Utc>,
    start: Instant,
    jump: Mutex<chrono::Duration>,
}

impl FakeClock {
    fn new(base: DateTime<Utc>) -> Arc<Self> {
        Arc::new(FakeClock {
            base,
            start: Instant::now(),
            jump: Mutex::new(chrono::Duration::zero()),
        })
    }

    fn jump(&self, by: chrono::Duration) {
        *self.jump.lock().unwrap() += by;
    }
}

impl WallClock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        let elapsed = chrono::Duration::from_std(self.start.elapsed()).unwrap();
        self.base + elapsed + *self.jump.lock().unwrap()
    }
}

/// One run the scheduler started; the test ends it.
struct Run {
    fire: JobFire,
    started_at: DateTime<Utc>,
    end: oneshot::Sender<JobRunEnd>,
}

impl Run {
    fn finish(self, ok: bool) {
        let result = result(if ok { "ok" } else { "failed" });
        let _ = self.end.send(JobRunEnd::Finished {
            started_at: self.started_at,
            result,
            ok,
        });
    }
}

struct FakeDispatch {
    clock: Arc<FakeClock>,
    runs: mpsc::UnboundedSender<Run>,
    heartbeats: mpsc::UnboundedSender<DateTime<Utc>>,
    finishes: mpsc::UnboundedSender<String>,
    rate_limit: Mutex<Option<DateTime<Utc>>>,
}

impl Dispatch for FakeDispatch {
    async fn run_job(&self, fire: JobFire) -> JobRunEnd {
        let started_at = self.clock.now();
        let (end, rx) = oneshot::channel();
        let _ = self.runs.send(Run {
            fire,
            started_at,
            end,
        });
        // A run the test drops was killed by a shutdown.
        rx.await.unwrap_or(JobRunEnd::Interrupted { started_at })
    }

    async fn heartbeat(&self) {
        let _ = self.heartbeats.send(self.clock.now());
    }

    async fn finish_one_shot(&self, job: String) -> OneShotFinish {
        let _ = self.finishes.send(job);
        OneShotFinish::Finished
    }

    fn rate_limited_until(&self) -> Option<DateTime<Utc>> {
        *self.rate_limit.lock().unwrap()
    }
}

/// A temp project dir, removed on drop (the crate has no `tempfile` dev-dependency).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "claudeclaw-scheduler-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(Layout::new(&path).root()).unwrap();
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A `JobResult` with the given outcome. `JobResult::new` needs `claudeclaw_api::RunOutcome`,
/// which this crate can't name (§3.1), so it is read from a `jobs.json` the way the Daemon would.
fn result(outcome: &str) -> JobResult {
    let dir = TempDir::new();
    let layout = Layout::new(&dir.0);
    std::fs::create_dir_all(layout.state_dir()).unwrap();
    let text = format!(r#"{{"jobs": {{"x": {{"last_result": {{"outcome": "{outcome}"}}}}}}}}"#);
    std::fs::write(layout.job_state(), text).unwrap();
    JobState::load(&layout).unwrap().jobs["x"]
        .last_result
        .clone()
        .unwrap()
}

fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).unwrap()
}

/// 2026-03-02 (a Monday) at `h:m`.
fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 2, h, m, 0).unwrap()
}

fn days(n: i64) -> chrono::Duration {
    chrono::Duration::days(n)
}

fn settings(heartbeat: HeartbeatConfig) -> SchedulerSettings {
    SchedulerSettings {
        offset: utc(),
        heartbeat,
    }
}

fn job_text(schedule: &str, extra: &str) -> String {
    format!("---\nschedule: \"{schedule}\"\n{extra}---\nDo the thing.\n")
}

fn daily(extra: &str) -> String {
    job_text("0 9 * * *", &format!("recurring: true\n{extra}"))
}

fn updated(name: &str, text: &str, took_effect: DateTime<Utc>) -> JobChange {
    let file = JobFile::parse(text.as_bytes());
    assert!(file.is_valid(), "{:?}", file.diagnostics());
    JobChange::Updated {
        name: name.into(),
        file: Arc::new(file),
        took_effect,
        revision: None,
    }
}

struct Harness {
    clock: Arc<FakeClock>,
    dispatch: Arc<FakeDispatch>,
    runs: mpsc::UnboundedReceiver<Run>,
    heartbeats: mpsc::UnboundedReceiver<DateTime<Utc>>,
    finishes: mpsc::UnboundedReceiver<String>,
    handle: SchedulerHandle,
    resumes: watch::Sender<u64>,
    _settings: watch::Sender<SchedulerSettings>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
    layout: Layout,
    _dir: Arc<TempDir>,
}

struct Start {
    base: DateTime<Utc>,
    state: JobState,
    proven_dead: BTreeSet<String>,
    heartbeat: HeartbeatConfig,
    dir: Arc<TempDir>,
}

impl Start {
    fn at(base: DateTime<Utc>) -> Self {
        Start {
            base,
            state: JobState::default(),
            proven_dead: BTreeSet::new(),
            heartbeat: HeartbeatConfig::disabled(),
            dir: Arc::new(TempDir::new()),
        }
    }

    fn spawn(self) -> Harness {
        let clock = FakeClock::new(self.base);
        let (runs_tx, runs) = mpsc::unbounded_channel();
        let (hb_tx, heartbeats) = mpsc::unbounded_channel();
        let (fin_tx, finishes) = mpsc::unbounded_channel();
        let dispatch = Arc::new(FakeDispatch {
            clock: clock.clone(),
            runs: runs_tx,
            heartbeats: hb_tx,
            finishes: fin_tx,
            rate_limit: Mutex::new(None),
        });
        let (settings_tx, settings_rx) = watch::channel(settings(self.heartbeat));
        let (resumes, resumes_rx) = watch::channel(0);
        let layout = Layout::new(&self.dir.0);
        let (scheduler, handle) = Scheduler::new(SchedulerInputs {
            dispatch: dispatch.clone(),
            clock: clock.clone(),
            layout: layout.clone(),
            state: self.state,
            proven_dead: self.proven_dead,
            settings: settings_rx,
            resumes: resumes_rx,
        });
        let cancel = CancellationToken::new();
        let task = tokio::spawn(scheduler.run(cancel.clone()));
        Harness {
            clock,
            dispatch,
            runs,
            heartbeats,
            finishes,
            handle,
            resumes,
            _settings: settings_tx,
            cancel,
            task,
            layout,
            _dir: self.dir,
        }
    }
}

impl Harness {
    async fn apply(&self, changes: Vec<JobChange>) {
        self.handle.apply(changes).await.unwrap();
    }

    /// The next run, letting paused time run until it comes.
    async fn next_run(&mut self) -> Run {
        timeout(Duration::from_secs(3 * 24 * 3600), self.runs.recv())
            .await
            .expect("no run")
            .unwrap()
    }

    async fn no_run_for(&mut self, d: Duration) {
        if let Ok(run) = timeout(d, self.runs.recv()).await {
            panic!("unexpected run {:?}", run.map(|r| r.fire));
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Stops the scheduler and reads `jobs.json` back.
    async fn stop(self) -> (JobState, Arc<TempDir>) {
        self.cancel.cancel();
        self.task.await.unwrap();
        (JobState::load(&self.layout).unwrap(), self._dir)
    }

    fn saved(&self) -> JobState {
        JobState::load(&self.layout).unwrap()
    }

    /// `jobs.json` once it satisfies `done`: saves run on the blocking pool, outside paused
    /// time.
    async fn saved_until(&self, done: impl Fn(&JobState) -> bool) -> JobState {
        for _ in 0..1000 {
            let state = self.saved();
            if done(&state) {
                return state;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!(
            "jobs.json never reached the expected state: {:?}",
            self.saved()
        );
    }
}

/// Lets the scheduler handle what was just sent, without moving the clock.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

// --- Next fire and catch-up ------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn fires_at_the_next_slot_and_again_a_day_later() {
    let mut h = Start::at(at(8, 0)).spawn();
    h.apply(vec![updated("daily", &daily(""), at(8, 0) - days(1))])
        .await;

    let run = h.next_run().await;
    assert_eq!(h.now(), at(9, 0));
    assert_eq!(run.fire.name, "daily");
    assert_eq!(run.fire.scheduled_for, at(9, 0));
    assert_eq!(run.fire.trigger, Trigger::Scheduled);
    let state = h.saved();
    assert_eq!(state.jobs["daily"].in_flight_since, Some(at(9, 0)));
    run.finish(true);

    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(9, 0) + days(1));
    assert_eq!(h.now(), at(9, 0) + days(1));
    run.finish(true);
    let (state, _) = h.stop().await;
    let rec = &state.jobs["daily"];
    assert_eq!(rec.in_flight_since, None);
    assert_eq!(rec.last_started_at, Some(at(9, 0) + days(1)));
    assert_eq!(rec.last_result, Some(result("ok")));
}

#[tokio::test(start_paused = true)]
async fn catches_up_a_missed_slot_inside_six_hours() {
    let mut start = Start::at(at(11, 0));
    start.state.entry("daily").last_started_at = Some(at(9, 0) - days(1));
    let mut h = start.spawn();
    h.apply(vec![updated("daily", &daily(""), at(0, 0) - days(7))])
        .await;

    let run = h.next_run().await;
    assert_eq!(h.now(), at(11, 0), "fired at once");
    assert_eq!(run.fire.scheduled_for, at(9, 0));
    assert_eq!(run.fire.trigger, Trigger::CatchUp);
    run.finish(true);
    // The caught-up slot is not fired again.
    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(9, 0) + days(1));
}

#[tokio::test(start_paused = true)]
async fn does_not_catch_up_a_slot_older_than_six_hours() {
    let mut start = Start::at(at(15, 30));
    start.state.entry("daily").last_started_at = Some(at(9, 0) - days(1));
    let mut h = start.spawn();
    h.apply(vec![updated("daily", &daily(""), at(0, 0) - days(7))])
        .await;

    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(9, 0) + days(1));
    assert_eq!(run.fire.trigger, Trigger::Scheduled);
}

/// #50: a daily 09:00 job created at 10:00 first runs the next day at 09:00.
#[tokio::test(start_paused = true)]
async fn no_catch_up_before_the_schedule_took_effect() {
    let mut h = Start::at(at(10, 0)).spawn();
    h.apply(vec![updated("daily", &daily(""), at(10, 0))]).await;
    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(9, 0) + days(1));
    assert_eq!(h.now(), at(9, 0) + days(1));
}

/// #50: a body-only edit doesn't move the took-effect time; a schedule edit does.
#[tokio::test(start_paused = true)]
async fn only_a_schedule_edit_moves_the_took_effect_time() {
    let mut h = Start::at(at(10, 0)).spawn();
    let old = at(0, 0) - days(7);
    // Known since last week; the 09:00 slot was missed while the Daemon was down. A body edit
    // in the same pass as the start-up report keeps last week's took-effect time.
    let body_edit = daily("").replace("Do the thing.", "Do another thing.");
    h.apply(vec![
        updated("daily", &daily(""), old),
        updated("daily", &body_edit, at(10, 0)),
    ])
    .await;
    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(9, 0));
    assert_eq!(run.fire.file.prompt().trim(), "Do another thing.");
    run.finish(true);

    // An edit to 10:30 at 10:40 doesn't fire 10:30 today.
    let edited = job_text("30 10 * * *", "recurring: true\n");
    h.apply(vec![updated("daily", &edited, at(10, 40))]).await;
    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(10, 30) + days(1));
}

/// #50: a One-shot job whose slot passed shortly before it took effect still runs.
#[tokio::test(start_paused = true)]
async fn one_shot_grace_runs_a_just_missed_slot() {
    let mut h = Start::at(at(10, 0)).spawn();
    let text = job_text("55 9 2 3 *", "recurring: false\n");
    let late = job_text("45 9 2 3 *", "recurring: false\n");
    h.apply(vec![
        updated("soon", &text, at(10, 0)),
        updated("too-late", &late, at(10, 0)),
    ])
    .await;
    let run = h.next_run().await;
    assert_eq!(run.fire.name, "soon");
    assert_eq!(run.fire.scheduled_for, at(9, 55));
    assert_eq!(h.now(), at(10, 0));
    run.finish(true);
    assert_eq!(h.finishes.recv().await.unwrap(), "soon");
    // "too-late" passed 15 min before it took effect: next year's slot is the first.
    h.no_run_for(Duration::from_secs(48 * 3600)).await;
}

// --- In-flight guard -------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn in_flight_guard_never_double_fires() {
    let base = at(8, 0) + chrono::Duration::seconds(30);
    let mut h = Start::at(base).spawn();
    let every_minute = job_text("* * * * *", "recurring: true\n");
    h.apply(vec![updated("minutely", &every_minute, at(0, 0))])
        .await;

    // 08:00 fires on time, 30 s late.
    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(8, 0));
    assert_eq!(run.fire.trigger, Trigger::Scheduled);
    // Three more slots pass while it runs: none fires, and Run now is refused.
    h.no_run_for(Duration::from_secs(3 * 60 + 15)).await;
    assert_eq!(h.handle.run_now("minutely").await, RunNow::InFlight);
    run.finish(true);

    // The slots skipped in flight are not caught up; the next one fires on time.
    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(8, 4));
    assert_eq!(run.fire.trigger, Trigger::Scheduled);
    assert_eq!(h.now(), at(8, 4));
    run.finish(true);
}

#[tokio::test(start_paused = true)]
async fn run_now_starts_a_job_once() {
    let mut h = Start::at(at(8, 0)).spawn();
    h.apply(vec![updated("daily", &daily("enabled: false\n"), at(0, 0))])
        .await;
    assert_eq!(h.handle.run_now("nope").await, RunNow::Unknown);
    assert_eq!(h.handle.run_now("daily").await, RunNow::Started);
    let run = h.next_run().await;
    assert_eq!(run.fire.trigger, Trigger::RunNow);
    assert_eq!(run.fire.scheduled_for, at(8, 0));
    assert_eq!(h.handle.run_now("daily").await, RunNow::InFlight);
    run.finish(true);
    settle().await;
    // Disabled: nothing fires on schedule.
    h.no_run_for(Duration::from_secs(48 * 3600)).await;
}

/// #46 D6: a change to a job in flight waits until its run (and the guard) is done.
#[tokio::test(start_paused = true)]
async fn changes_to_a_job_in_flight_are_held_back() {
    let mut h = Start::at(at(8, 59)).spawn();
    h.apply(vec![updated("daily", &daily(""), at(0, 0))]).await;
    let run = h.next_run().await;

    // Made invalid mid-run: Run now still sees the old job, so the change wasn't applied.
    let invalid = JobFile::parse(b"---\nschedule: \"0 9 * * *\"\n---\nno recurring key\n");
    assert!(!invalid.is_valid());
    h.apply(vec![JobChange::Updated {
        name: "daily".into(),
        file: Arc::new(invalid),
        took_effect: at(9, 0),
        revision: None,
    }])
    .await;
    // Then re-scheduled to noon: the latest held change wins.
    let noon = job_text("0 12 * * *", "recurring: true\n");
    h.apply(vec![updated("daily", &noon, at(9, 0))]).await;
    assert_eq!(h.handle.run_now("daily").await, RunNow::InFlight);
    run.finish(true);

    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(12, 0));
}

// --- Retries ---------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn retries_are_persisted_across_a_restart() {
    let mut h = Start::at(at(8, 59)).spawn();
    let text = daily("retry: 2\nretry_delay: 600\n");
    h.apply(vec![updated("daily", &text, at(0, 0))]).await;
    let run = h.next_run().await;
    run.finish(false);
    let (state, dir) = h.stop().await;
    let rec = &state.jobs["daily"];
    assert_eq!(rec.fail_count, 1);
    assert_eq!(rec.retry_at, Some(at(9, 10)));
    assert_eq!(rec.in_flight_since, None);

    // A new Worker at 09:02 with the saved state.
    let mut start = Start::at(at(9, 2));
    start.state = state;
    start.dir = dir;
    let mut h = start.spawn();
    h.apply(vec![updated("daily", &text, at(0, 0))]).await;
    let run = h.next_run().await;
    assert_eq!(run.fire.trigger, Trigger::Retry);
    assert_eq!(run.fire.scheduled_for, at(9, 10));
    assert_eq!(h.now(), at(9, 10));
    run.finish(false);

    let run = h.next_run().await;
    assert_eq!(run.fire.trigger, Trigger::Retry);
    assert_eq!(h.now(), at(9, 20));
    run.finish(false);
    // Retries exhausted.
    let state = h
        .saved_until(|s| {
            let rec = &s.jobs["daily"];
            rec.last_started_at == Some(at(9, 20)) && rec.in_flight_since.is_none()
        })
        .await;
    assert_eq!(state.jobs["daily"].fail_count, 0);
    assert_eq!(state.jobs["daily"].retry_at, None);

    // Back to the schedule.
    let run = h.next_run().await;
    assert_eq!(run.fire.trigger, Trigger::Scheduled);
    assert_eq!(run.fire.scheduled_for, at(9, 0) + days(1));
}

#[tokio::test(start_paused = true)]
async fn a_success_clears_the_retry_count() {
    let mut h = Start::at(at(8, 59)).spawn();
    let text = daily("retry: 3\nretry_delay: 60\n");
    h.apply(vec![updated("daily", &text, at(0, 0))]).await;
    h.next_run().await.finish(false);
    let run = h.next_run().await;
    assert_eq!(h.now(), at(9, 1));
    run.finish(true);
    let state = h
        .saved_until(|s| s.jobs["daily"].in_flight_since.is_none())
        .await;
    assert_eq!(state.jobs["daily"].fail_count, 0);
    assert_eq!(state.jobs["daily"].retry_at, None);
}

// --- One-shot finish -------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn one_shot_finishes_once_no_retry_is_pending() {
    let mut h = Start::at(at(8, 0)).spawn();
    let text = job_text(
        "0 9 2 3 *",
        "recurring: false\nretry: 1\nretry_delay: 300\n",
    );
    h.apply(vec![updated("once", &text, at(0, 0))]).await;

    h.next_run().await.finish(false);
    // A retry is pending: not finished yet.
    let retry = h.next_run().await;
    assert_eq!(retry.fire.trigger, Trigger::Retry);
    assert!(h.finishes.try_recv().is_err());
    retry.finish(false);
    assert_eq!(h.finishes.recv().await.unwrap(), "once");

    // The finish's own write (`enabled: false`) arrives like any change.
    let done = job_text(
        "0 9 2 3 *",
        "recurring: false\nenabled: false\nretry: 1\nretry_delay: 300\n",
    );
    h.apply(vec![updated("once", &done, h.now())]).await;
    h.no_run_for(Duration::from_secs(48 * 3600)).await;
}

#[tokio::test(start_paused = true)]
async fn run_now_is_never_the_one_shot_finish() {
    let mut h = Start::at(at(8, 0)).spawn();
    let text = job_text("0 9 2 3 *", "recurring: false\n");
    h.apply(vec![updated("once", &text, at(0, 0))]).await;
    assert_eq!(h.handle.run_now("once").await, RunNow::Started);
    h.next_run().await.finish(true);
    settle().await;
    assert!(h.finishes.try_recv().is_err());
    // Its slot still comes.
    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(9, 0));
    run.finish(true);
    assert_eq!(h.finishes.recv().await.unwrap(), "once");
}

// --- Interrupted runs ------------------------------------------------------------------------

/// #29 D6: only a run the Reap proved dead is re-run.
#[tokio::test(start_paused = true)]
async fn only_runs_proved_dead_are_re_run() {
    let mut start = Start::at(at(10, 0));
    for name in ["dead", "unsure"] {
        start.state.entry(name).in_flight_since = Some(at(9, 0));
    }
    start.proven_dead.insert("dead".into());
    let mut h = start.spawn();
    h.apply(vec![
        updated("dead", &daily(""), at(0, 0)),
        updated("unsure", &daily(""), at(0, 0)),
    ])
    .await;

    let run = h.next_run().await;
    assert_eq!(run.fire.name, "dead");
    assert_eq!(run.fire.trigger, Trigger::Interrupted);
    assert_eq!(run.fire.scheduled_for, at(9, 0));
    run.finish(true);
    // "unsure" counts as run: its next fire is tomorrow's.
    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(9, 0) + days(1));
}

/// #9: a run a shutdown kills is recorded as interrupted and re-run by the next Worker.
#[tokio::test(start_paused = true)]
async fn a_shutdown_waits_for_runs_and_records_them_interrupted() {
    let mut h = Start::at(at(8, 59)).spawn();
    h.apply(vec![updated("daily", &daily(""), at(0, 0))]).await;
    let run = h.next_run().await;
    h.cancel.cancel();
    settle().await;
    assert!(!h.task.is_finished(), "waits for the run in flight");
    drop(run); // core's shutdown killed it
    let (state, dir) = h.stop().await;
    assert_eq!(state.jobs["daily"].interrupted_at, Some(at(9, 0)));
    assert_eq!(state.jobs["daily"].in_flight_since, None);

    let mut start = Start::at(at(9, 5));
    start.state = state;
    start.dir = dir;
    let mut h = start.spawn();
    h.apply(vec![updated("daily", &daily(""), at(0, 0))]).await;
    let run = h.next_run().await;
    assert_eq!(run.fire.trigger, Trigger::Interrupted);
    assert_eq!(h.now(), at(9, 5));
}

// --- Rate limit ------------------------------------------------------------------------------

/// TS records an on-time slot during a rate limit as skipped, so it isn't caught up later
/// (`src/commands/start.ts:1010-1022`).
#[tokio::test(start_paused = true)]
async fn a_slot_during_a_rate_limit_is_skipped_not_caught_up() {
    let mut h = Start::at(at(8, 59)).spawn();
    *h.dispatch.rate_limit.lock().unwrap() = Some(at(10, 0));
    h.apply(vec![updated("daily", &daily(""), at(0, 0))]).await;
    h.no_run_for(Duration::from_secs(3 * 3600)).await;
    let run = h.next_run().await;
    assert_eq!(run.fire.scheduled_for, at(9, 0) + days(1));
}

// --- Heartbeat -------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn heartbeat_skips_exclude_windows_and_rate_limits() {
    let mut start = Start::at(at(7, 30));
    start.heartbeat = HeartbeatConfig {
        enabled: true,
        interval: Duration::from_secs(3600),
        exclude_windows: vec![ExcludeWindow::new(None, "09:00", "12:00").unwrap()],
    };
    let mut h = start.spawn();
    let mut beats = Vec::new();
    while h.now() < at(13, 0) {
        if let Ok(Some(t)) = timeout(Duration::from_secs(600), h.heartbeats.recv()).await {
            beats.push(t);
        }
    }
    assert_eq!(beats, [at(8, 30), at(12, 30)]);

    *h.dispatch.rate_limit.lock().unwrap() = Some(at(14, 0));
    while h.now() < at(14, 45) {
        if let Ok(Some(t)) = timeout(Duration::from_secs(600), h.heartbeats.recv()).await {
            beats.push(t);
        }
    }
    assert_eq!(beats, [at(8, 30), at(12, 30), at(14, 30)], "13:30 skipped");
}

// --- Timing ----------------------------------------------------------------------------------

/// #38: on a Resume, due jobs are re-checked at once, not after the 30 s cap.
#[tokio::test(start_paused = true)]
async fn a_resume_re_checks_at_once() {
    let mut h = Start::at(at(8, 0)).spawn();
    h.apply(vec![updated("daily", &daily(""), at(0, 0))]).await;
    settle().await;
    // Asleep from 08:00 to 09:30: the wall clock moved, tokio's clock didn't.
    h.clock.jump(chrono::Duration::minutes(90));
    h.resumes.send_modify(|n| *n += 1);
    let before = Instant::now();
    let run = timeout(Duration::from_millis(10), h.runs.recv())
        .await
        .expect("re-checked at once")
        .unwrap();
    assert!(before.elapsed() < Duration::from_millis(10));
    assert_eq!(run.fire.scheduled_for, at(9, 0));
    assert_eq!(run.fire.trigger, Trigger::CatchUp);
}

/// Architecture §4 `scheduler`: each sleep is capped at 30 s, so a wall-clock jump is noticed
/// within 30 s even without a Resume.
#[tokio::test(start_paused = true)]
async fn a_clock_jump_is_noticed_within_thirty_seconds() {
    let mut h = Start::at(at(8, 0)).spawn();
    h.apply(vec![updated("daily", &daily(""), at(0, 0))]).await;
    settle().await;
    h.clock.jump(chrono::Duration::minutes(90));
    let before = Instant::now();
    let run = timeout(MAX_SLEEP + Duration::from_secs(1), h.runs.recv())
        .await
        .expect("noticed within the cap")
        .unwrap();
    assert!(before.elapsed() <= MAX_SLEEP);
    assert_eq!(run.fire.scheduled_for, at(9, 0));
}

// --- Job set ---------------------------------------------------------------------------------

/// #46 D4: an identical-bytes rename keeps `last_started_at`, so the slot isn't re-run.
#[tokio::test(start_paused = true)]
async fn a_rename_carries_the_last_run() {
    let mut start = Start::at(at(10, 0));
    start.state.entry("old").last_started_at = Some(at(9, 0));
    let mut h = start.spawn();
    let old = at(0, 0) - days(7);
    h.apply(vec![updated("old", &daily(""), old)]).await;
    h.apply(vec![
        JobChange::Removed { name: "old".into() },
        updated("new", &daily(""), old),
    ])
    .await;
    h.no_run_for(Duration::from_secs(3600)).await;
    let state = h
        .saved_until(|s| {
            s.jobs
                .get("new")
                .is_some_and(|r| r.last_started_at.is_some())
        })
        .await;
    assert_eq!(state.jobs["new"].last_started_at, Some(at(9, 0)));
    assert_eq!(state.jobs["old"].orphaned_since, Some(at(10, 0)));
}

/// #46 D4: a removed job is unscheduled and its retries dropped; `forget` drops its state.
#[tokio::test(start_paused = true)]
async fn removed_jobs_are_orphaned_and_forgotten_jobs_dropped() {
    let mut start = Start::at(at(8, 0));
    let rec = start.state.entry("gone");
    rec.retry_at = Some(at(8, 30));
    rec.fail_count = 1;
    let mut h = start.spawn();
    h.apply(vec![updated("gone", &daily("retry: 2\n"), at(0, 0))])
        .await;
    h.apply(vec![JobChange::Removed {
        name: "gone".into(),
    }])
    .await;
    h.no_run_for(Duration::from_secs(48 * 3600)).await;
    let state = h
        .saved_until(|s| s.jobs["gone"].orphaned_since.is_some())
        .await;
    let rec = &state.jobs["gone"];
    assert_eq!(rec.orphaned_since, Some(at(8, 0)));
    assert_eq!((rec.retry_at, rec.fail_count), (None, 0));

    // It comes back: the orphan clock clears.
    h.apply(vec![updated("gone", &daily(""), at(0, 0))]).await;
    h.saved_until(|s| s.jobs["gone"].orphaned_since.is_none())
        .await;
    h.handle.forget("gone").await.unwrap();
    h.saved_until(|s| !s.jobs.contains_key("gone")).await;
    assert_eq!(h.handle.run_now("gone").await, RunNow::Unknown);
}

/// Change scan events convert as the scheduler needs them (#46, #50).
#[test]
fn scan_changes_convert() {
    use claudeclaw_store::{ScanTargets, Scanner, StdFs};

    let dir = TempDir::new();
    let layout = Layout::new(&dir.0);
    let targets = ScanTargets::new(&layout, None);
    std::fs::create_dir_all(targets.jobs_dir()).unwrap();
    std::fs::write(targets.jobs_dir().join("daily.md"), daily("")).unwrap();
    let mut scanner = Scanner::new(targets);
    let changes = scanner.startup_pass(&StdFs, std::time::SystemTime::now());
    let converted: Vec<JobChange> = changes.iter().filter_map(JobChange::from_scan).collect();
    assert_eq!(converted.len(), 1, "{changes:?}");
    let JobChange::Updated {
        name,
        file,
        revision,
        ..
    } = &converted[0]
    else {
        panic!("{converted:?}");
    };
    assert_eq!(name, "daily");
    assert!(file.is_valid());
    assert_eq!(revision.as_ref().map(String::len), Some(16));
}
