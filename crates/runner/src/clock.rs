//! The Worker-wide Awake clock and the Wake grace (architecture §4 `claudeclaw-runner` →
//! `clock`; #38).
//!
//! Every run timeout counts **awake time** (#38 D1). One [`AwakeClock`] per Worker ticks every
//! [`TICK`] and adds the [`tokio::time::Instant`] step since the last tick, capped at
//! [`STEP_CAP`], to `awake_ms` (#38 D2, the #27 D4 rule). On each tick it also reads a
//! [`SuspendClock`]; when that clock's step exceeds the accrued step by [`RESUME_THRESHOLD`] or
//! more, the tick is a **Resume** (#38 D3). A [`RunDeadline`] turns a run's timeout into an
//! awake-time deadline and applies the Wake grace on each Resume (#38 D4).
//!
//! The constants are compiled in; there is no setting (#38 D8). [`ClockConfig`] exists so tests
//! can shorten them.

use std::time::Duration;

use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior};

/// How often the Awake clock ticks (#38 D2).
pub const TICK: Duration = Duration::from_secs(1);
/// The most one tick may add to awake time, whatever the gap since the last one (#38 D2, #27 D4).
pub const STEP_CAP: Duration = Duration::from_secs(2);
/// How far the suspend-counting clock must run ahead of the accrued step for a Resume (#38 D3).
pub const RESUME_THRESHOLD: Duration = Duration::from_secs(10);
/// The Wake grace: awake time a running run keeps after a Resume, and the most it can gain over
/// its lifetime (#38 D4).
pub const WAKE_GRACE: Duration = Duration::from_secs(120);

/// What the Awake clock publishes on every tick (#38 D2).
///
/// `suspended_ms` is the running sum of every Resume gap, so a reader that misses a publication
/// (a `watch` channel keeps only the latest value) still sees every gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AwakeTime {
    /// Awake time since the clock started.
    pub awake_ms: u64,
    /// Resumes since the clock started.
    pub resumes: u64,
    /// The gap of the latest Resume; 0 before the first.
    pub last_resume_suspended_ms: u64,
    /// The sum of every Resume gap since the clock started.
    pub suspended_ms: u64,
}

/// A monotonic clock that keeps counting while the system is suspended (#38 D3). Tests inject a
/// fake one.
pub trait SuspendClock: Send + 'static {
    /// Time since an arbitrary fixed origin.
    fn now(&self) -> Duration;
}

/// The OS suspend-counting clock (#38 D3): `CLOCK_MONOTONIC` on macOS, `CLOCK_BOOTTIME` on
/// Linux, `QueryInterruptTime` on Windows. Other Unixes fall back to `CLOCK_MONOTONIC`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsSuspendClock;

impl SuspendClock for OsSuspendClock {
    fn now(&self) -> Duration {
        os_suspend_now()
    }
}

#[cfg(unix)]
fn os_suspend_now() -> Duration {
    #[cfg(target_os = "linux")]
    const CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec; both clock ids exist on their targets.
    let rc = unsafe { libc::clock_gettime(CLOCK, &mut ts) };
    if rc != 0 {
        return Duration::ZERO;
    }
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

#[cfg(windows)]
fn os_suspend_now() -> Duration {
    let mut t: u64 = 0;
    // SAFETY: `t` is a valid, writable u64. The value is in 100 ns units.
    unsafe { windows_sys::Win32::System::WindowsProgramming::QueryInterruptTime(&mut t) };
    Duration::from_nanos(t.saturating_mul(100))
}

/// The Awake clock's constants (#38 D8). [`Default`] is what every Worker runs with; tests use
/// shorter values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockConfig {
    pub tick: Duration,
    pub step_cap: Duration,
    pub resume_threshold: Duration,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            tick: TICK,
            step_cap: STEP_CAP,
            resume_threshold: RESUME_THRESHOLD,
        }
    }
}

/// The accrual rule, apart from any timer so it can be tested on its own (#38 D2–D3).
#[derive(Debug, Clone)]
pub(crate) struct Accrual {
    config: ClockConfig,
    time: AwakeTime,
}

impl Accrual {
    pub(crate) fn new(config: ClockConfig) -> Self {
        Self {
            config,
            time: AwakeTime::default(),
        }
    }

    pub(crate) fn time(&self) -> AwakeTime {
        self.time
    }

    /// Accrues one tick. `instant_step` is the [`Instant`] step since the last tick and
    /// `suspend_step` the [`SuspendClock`] step. Returns the gap in ms when the tick is a Resume.
    pub(crate) fn step(&mut self, instant_step: Duration, suspend_step: Duration) -> Option<u64> {
        let accrued = instant_step.min(self.config.step_cap);
        self.time.awake_ms += millis(accrued);
        let gap = suspend_step.saturating_sub(accrued);
        if gap < self.config.resume_threshold {
            return None;
        }
        let gap_ms = millis(gap);
        self.time.resumes += 1;
        self.time.last_resume_suspended_ms = gap_ms;
        self.time.suspended_ms += gap_ms;
        Some(gap_ms)
    }
}

/// The Worker-wide Awake clock (#38 D2). Started once by the composition root; every run, the
/// scheduler and `core` read it through [`AwakeClock::subscribe`].
///
/// The ticking task stops once this handle and every receiver are dropped.
#[derive(Debug, Clone)]
pub struct AwakeClock {
    rx: watch::Receiver<AwakeTime>,
}

impl AwakeClock {
    /// Starts the clock on the OS suspend-counting clock with the compiled-in constants. Must be
    /// called inside a tokio runtime.
    pub fn start() -> Self {
        Self::start_with(OsSuspendClock, ClockConfig::default())
    }

    /// Starts the clock on `suspend` with `config`. Tests inject a fake [`SuspendClock`] and run
    /// on tokio's paused clock (#38 D8).
    pub fn start_with<S: SuspendClock>(suspend: S, config: ClockConfig) -> Self {
        let (tx, rx) = watch::channel(AwakeTime::default());
        tokio::spawn(tick_loop(tx, suspend, config));
        Self { rx }
    }

    /// A receiver for the published [`AwakeTime`], for a run, the scheduler or `core`.
    pub fn subscribe(&self) -> watch::Receiver<AwakeTime> {
        self.rx.clone()
    }

    /// The latest published [`AwakeTime`].
    pub fn now(&self) -> AwakeTime {
        *self.rx.borrow()
    }
}

async fn tick_loop<S: SuspendClock>(tx: watch::Sender<AwakeTime>, suspend: S, config: ClockConfig) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The first tick completes at once.
    interval.tick().await;
    let mut accrual = Accrual::new(config);
    let mut last_instant = Instant::now();
    let mut last_suspend = suspend.now();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            () = tx.closed() => return,
        }
        let now_instant = Instant::now();
        let now_suspend = suspend.now();
        let resumed = accrual.step(
            now_instant.saturating_duration_since(last_instant),
            now_suspend.saturating_sub(last_suspend),
        );
        last_instant = now_instant;
        last_suspend = now_suspend;
        if let Some(suspended_ms) = resumed {
            tracing::info!(
                event = "system.resumed",
                suspended_ms,
                "the system resumed; running runs get the Wake grace"
            );
        }
        tx.send_replace(accrual.time());
    }
}

/// One run's awake-time deadline with the Wake grace (#38 D2, D4).
///
/// `deadline = awake_at_spawn + timeout`. On every Resume seen,
/// `deadline = min(max(deadline, awake_now + grace), awake_at_spawn + timeout + grace)`.
#[derive(Debug, Clone)]
pub struct RunDeadline {
    start: AwakeTime,
    grace_ms: u64,
    limit_ms: u64,
    deadline_ms: u64,
    seen_resumes: u64,
    wake_grace: bool,
}

impl RunDeadline {
    /// A deadline `timeout` of awake time after `now`, with the compiled-in [`WAKE_GRACE`].
    pub fn new(now: AwakeTime, timeout: Duration) -> Self {
        Self::with_grace(now, timeout, WAKE_GRACE)
    }

    /// As [`RunDeadline::new`] with another grace, for tests (#38 D8).
    pub fn with_grace(now: AwakeTime, timeout: Duration, grace: Duration) -> Self {
        let deadline_ms = now.awake_ms.saturating_add(millis(timeout));
        let grace_ms = millis(grace);
        Self {
            start: now,
            grace_ms,
            limit_ms: deadline_ms.saturating_add(grace_ms),
            deadline_ms,
            seen_resumes: now.resumes,
            wake_grace: false,
        }
    }

    /// Takes a published [`AwakeTime`], applies the Wake grace if a Resume happened since the
    /// last call, and returns whether the deadline has passed.
    pub fn observe(&mut self, now: AwakeTime) -> bool {
        if now.resumes > self.seen_resumes {
            self.seen_resumes = now.resumes;
            let graced = self
                .deadline_ms
                .max(now.awake_ms.saturating_add(self.grace_ms))
                .min(self.limit_ms);
            if graced > self.deadline_ms {
                self.deadline_ms = graced;
                self.wake_grace = true;
            }
        }
        now.awake_ms >= self.deadline_ms
    }

    /// The deadline, in the clock's awake ms.
    pub fn deadline_ms(&self) -> u64 {
        self.deadline_ms
    }

    /// Whether a Wake grace moved the deadline (`RunRecord.wake_grace`, #38 D6).
    pub fn wake_grace(&self) -> bool {
        self.wake_grace
    }

    /// Awake time since the run started (`RunRecord.awake_ms`, #38 D6).
    pub fn awake_ms(&self, now: AwakeTime) -> u64 {
        now.awake_ms.saturating_sub(self.start.awake_ms)
    }

    /// The sum of Resume gaps since the run started (`RunRecord.suspended_ms`, #38 D6).
    pub fn suspended_ms(&self, now: AwakeTime) -> u64 {
        now.suspended_ms.saturating_sub(self.start.suspended_ms)
    }
}

pub(crate) fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_step_adds_at_most_the_cap() {
        let mut a = Accrual::new(ClockConfig::default());
        assert_eq!(a.step(S, S), None);
        assert_eq!(a.time().awake_ms, 1_000);
        // A process stopped for 30 s: Instant ran on, the cap holds the step to 2 s.
        assert_eq!(a.step(secs(30), secs(30)), Some(28_000));
        assert_eq!(a.time().awake_ms, 3_000);
    }

    #[test]
    fn a_resume_needs_a_gap_of_at_least_the_threshold() {
        let mut a = Accrual::new(ClockConfig::default());
        // A late tick under load: 2 s accrued, 11.9 s on the suspend clock, 9.9 s gap.
        assert_eq!(a.step(secs(12), Duration::from_millis(11_900)), None);
        assert_eq!(a.time().resumes, 0);
        // Exactly 10 s of gap is a Resume.
        assert_eq!(a.step(S, secs(11)), Some(10_000));
        let t = a.time();
        assert_eq!(
            (t.resumes, t.last_resume_suspended_ms, t.suspended_ms),
            (1, 10_000, 10_000)
        );
    }

    #[test]
    fn macos_sleep_shows_as_a_suspend_step_with_a_normal_instant_step() {
        // On macOS `Instant` stops in sleep; `CLOCK_MONOTONIC` doesn't (#38 Evidence 3).
        let mut a = Accrual::new(ClockConfig::default());
        assert_eq!(a.step(S, secs(1 + 600)), Some(600_000));
        assert_eq!(a.step(S, secs(1 + 60)), Some(60_000));
        let t = a.time();
        assert_eq!((t.awake_ms, t.resumes, t.suspended_ms), (2_000, 2, 660_000));
    }

    fn at(awake_s: u64, resumes: u64, suspended_s: u64) -> AwakeTime {
        AwakeTime {
            awake_ms: awake_s * 1_000,
            resumes,
            last_resume_suspended_ms: 0,
            suspended_ms: suspended_s * 1_000,
        }
    }

    #[test]
    fn a_deadline_counts_awake_time_only() {
        let mut d = RunDeadline::new(at(100, 0, 0), secs(300));
        assert_eq!(d.deadline_ms(), 400_000);
        assert!(!d.observe(at(399, 0, 0)));
        assert!(d.observe(at(400, 0, 0)));
        assert!(!d.wake_grace());
    }

    #[test]
    fn a_resume_near_the_deadline_gives_at_least_the_grace() {
        let mut d = RunDeadline::new(at(0, 0, 0), secs(300));
        // Asleep with 10 s left; after the wake the run gets 120 s more.
        assert!(!d.observe(at(290, 1, 3_600)));
        assert_eq!(d.deadline_ms(), 410_000);
        assert!(d.wake_grace());
        assert!(!d.observe(at(409, 1, 3_600)));
        assert!(d.observe(at(410, 1, 3_600)));
        assert_eq!(d.suspended_ms(at(410, 1, 3_600)), 3_600_000);
        assert_eq!(d.awake_ms(at(410, 1, 3_600)), 410_000);
    }

    #[test]
    fn the_grace_is_at_most_its_length_over_a_lifetime() {
        let mut d = RunDeadline::new(at(0, 0, 0), secs(300));
        assert!(!d.observe(at(290, 1, 10)));
        assert_eq!(d.deadline_ms(), 410_000);
        // Repeated dark wakes can't push it past timeout + grace.
        assert!(!d.observe(at(400, 2, 20)));
        assert_eq!(d.deadline_ms(), 420_000);
        assert!(!d.observe(at(419, 3, 30)));
        assert_eq!(d.deadline_ms(), 420_000);
        assert!(d.observe(at(420, 4, 40)));
    }

    #[test]
    fn a_run_far_from_its_deadline_is_unaffected() {
        let mut d = RunDeadline::new(at(0, 0, 0), secs(1_800));
        assert!(!d.observe(at(60, 1, 600)));
        assert_eq!(d.deadline_ms(), 1_800_000);
        assert!(!d.wake_grace());
    }

    #[test]
    fn resumes_before_the_spawn_are_not_counted() {
        let mut d = RunDeadline::new(at(50, 3, 900), secs(10));
        assert!(!d.observe(at(55, 3, 900)));
        assert!(!d.wake_grace());
        assert_eq!(d.suspended_ms(at(55, 3, 900)), 0);
    }

    #[derive(Clone, Default)]
    struct FakeSuspend(Arc<Mutex<Duration>>);

    impl FakeSuspend {
        fn advance(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }
    }

    impl SuspendClock for FakeSuspend {
        fn now(&self) -> Duration {
            *self.0.lock().unwrap()
        }
    }

    /// Advances tokio's paused clock and the fake suspend clock together, tick by tick.
    async fn run_ticks(fake: &FakeSuspend, n: u32) {
        for _ in 0..n {
            fake.advance(S);
            tokio::time::advance(S).await;
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_clock_ticks_and_publishes_resumes() {
        let fake = FakeSuspend::default();
        let clock = AwakeClock::start_with(fake.clone(), ClockConfig::default());
        let mut rx = clock.subscribe();
        tokio::task::yield_now().await;
        run_ticks(&fake, 3).await;
        assert_eq!(clock.now().awake_ms, 3_000);
        assert_eq!(clock.now().resumes, 0);

        // The machine sleeps 60 s: the suspend clock jumps, tokio's Instant doesn't.
        fake.advance(secs(60));
        run_ticks(&fake, 1).await;
        rx.changed().await.unwrap();
        let t = *rx.borrow_and_update();
        assert_eq!(
            (
                t.awake_ms,
                t.resumes,
                t.last_resume_suspended_ms,
                t.suspended_ms
            ),
            (4_000, 1, 60_000, 60_000)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_process_accrues_at_most_the_cap() {
        let fake = FakeSuspend::default();
        let clock = AwakeClock::start_with(fake.clone(), ClockConfig::default());
        tokio::task::yield_now().await;
        // Stopped for 30 s: both clocks run on, the tick comes late once.
        fake.advance(secs(30));
        tokio::time::advance(secs(30)).await;
        tokio::task::yield_now().await;
        let t = clock.now();
        assert_eq!((t.awake_ms, t.resumes, t.suspended_ms), (2_000, 1, 28_000));
    }

    #[tokio::test(start_paused = true)]
    async fn the_task_stops_when_every_receiver_is_gone() {
        let clock = AwakeClock::start_with(FakeSuspend::default(), ClockConfig::default());
        let rx = clock.subscribe();
        drop(clock);
        drop(rx);
        // Nothing to assert beyond not hanging: the paused runtime auto-advances and the task
        // returns on `closed()`.
        tokio::time::sleep(secs(5)).await;
    }

    #[test]
    fn the_os_clock_moves_forward() {
        let c = OsSuspendClock;
        let a = c.now();
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.now() > a);
    }
}
