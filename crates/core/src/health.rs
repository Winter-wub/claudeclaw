//! The stall monitor (architecture §4 `core::health`; #27 D5, #74).
//!
//! Every long-lived task that must keep making progress holds a [`Beacon`]. The Worker arms a
//! named beacon once ([`HealthMonitor::named`]) and re-arms a Turn's beacon on every run in the
//! Turn ([`HealthMonitor::turn`]). [`HealthMonitor::check`] accrues quiet time to every armed
//! beacon in [`STEP_CAP`]-bounded steps and returns a [`Stall`] when one passes its limit.
//!
//! [`HealthMonitor::run`] is the Worker's monitor task: it ticks every [`TICK`], logs
//! `worker.stalled{beacon, quiet_ms}` at ERROR and returns the [`Stall`]. Shutting down, exiting
//! 75 and letting the Watchdog count it is the composition root's job; `core` never exits.
//!
//! A limit is compiled in, never a setting (#27 D5): the named beacons get [`NAMED_LIMIT`] and a
//! Turn gets its run timeout plus the Wake grace, the kill slack and [`TURN_SLACK`] (#33, #38).
//! Time comes from [`tokio::time::Instant`], so tests run on tokio's paused clock.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use claudeclaw_api::log_event::daemon as event;
use claudeclaw_api::scope::Scope;
use claudeclaw_api::status::Beacon as StatusBeacon;
use tokio::time::{Instant, MissedTickBehavior};

/// How often the monitor accuses quiet beacons (#27 D5). Every long-lived task's own loop ticks
/// at least this often, so a healthy Worker never shows a quiet beacon for long.
pub const TICK: Duration = Duration::from_secs(5);
/// The most one check may add to a beacon's quiet time, whatever the gap since the last one
/// (#27 D4/D5, the same rule the Awake clock uses).
pub const STEP_CAP: Duration = Duration::from_secs(10);
/// The limit of the named beacons `scheduler`, `adapter:<transport>` and `outcome_sink`
/// (#27 D5).
pub const NAMED_LIMIT: Duration = Duration::from_secs(300);
/// The Wake grace a run keeps after a Resume (#33, #38 D4), part of a Turn beacon's limit.
pub const WAKE_GRACE: Duration = Duration::from_secs(120);
/// The slack past a run's timeout before it is killed (#27 D5).
pub const KILL_SLACK: Duration = Duration::from_secs(5);
/// Room after the kill for the Turn to wind down before its beacon may stall (#27 D5).
pub const TURN_SLACK: Duration = Duration::from_secs(120);

/// The limit of a `turn:<scope>` beacon: the run's own timeout plus the Wake grace, the kill
/// slack and [`TURN_SLACK`] (#27 D5, #33, #38). Every run in the Turn re-arms it.
pub fn turn_limit(run_timeout: Duration) -> Duration {
    run_timeout
        .saturating_add(WAKE_GRACE)
        .saturating_add(KILL_SLACK)
        .saturating_add(TURN_SLACK)
}

/// A beacon that has gone quiet for at least its limit (#27 D5). The caller logs, shuts down and
/// exits 75.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    /// The beacon's name: `scheduler`, `adapter:<transport>`, `outcome_sink` or `turn:<scope>`.
    pub beacon: String,
    /// How long the beacon has been quiet, in milliseconds.
    pub quiet_ms: u64,
}

impl Stall {
    /// The `fatal("…")` message the Watchdog is shown: a beacon name and whole seconds, never a
    /// path or content.
    pub fn fatal_message(&self) -> String {
        format!("stall: {} quiet {}s", self.beacon, self.quiet_ms / 1_000)
    }
}

/// One beacon's shared, clonable handle. Every clone points at the same state, so any holder's
/// beat counts.
#[derive(Debug, Clone)]
pub struct Beacon {
    inner: Arc<BeaconInner>,
}

#[derive(Debug)]
struct BeaconInner {
    name: String,
    state: Mutex<BeaconState>,
}

#[derive(Debug)]
struct BeaconState {
    /// The quiet time that trips a Stall; `None` while the beacon is unarmed.
    limit: Option<Duration>,
    /// Quiet time accrued since the last beat or arm.
    quiet: Duration,
}

impl Beacon {
    fn new(name: &str, limit: Duration) -> Self {
        Beacon {
            inner: Arc::new(BeaconInner {
                name: name.to_owned(),
                state: Mutex::new(BeaconState {
                    limit: Some(limit),
                    quiet: Duration::ZERO,
                }),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, BeaconState> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The beacon's name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Marks progress: quiet time restarts at zero. The limit is unchanged.
    pub fn beat(&self) {
        self.state().quiet = Duration::ZERO;
    }

    /// Arms (or re-arms) the beacon with `limit`, resetting quiet time. A Turn calls this on
    /// every run.
    pub fn arm(&self, limit: Duration) {
        let mut state = self.state();
        state.limit = Some(limit);
        state.quiet = Duration::ZERO;
    }

    /// Disarms the beacon: it stops being watched and leaves [`HealthMonitor::beacons`].
    pub fn disarm(&self) {
        let mut state = self.state();
        state.limit = None;
        state.quiet = Duration::ZERO;
    }

    /// Quiet time accrued since the last beat or arm, in milliseconds.
    pub fn quiet_ms(&self) -> u64 {
        millis(self.state().quiet)
    }

    /// Adds `step` to quiet time and reports the total once it has reached the limit. An unarmed
    /// beacon never stalls.
    fn accrue(&self, step: Duration) -> Option<u64> {
        let mut state = self.state();
        let limit = state.limit?;
        state.quiet = state.quiet.saturating_add(step);
        (state.quiet >= limit).then(|| millis(state.quiet))
    }
}

/// The Worker's beacons and their quiet-time accrual (#27 D5). Cheap to share: the monitor task
/// and every task that beats hold the same [`Arc`]-backed beacons.
#[derive(Debug, Default)]
pub struct HealthMonitor {
    beacons: Mutex<BTreeMap<String, Beacon>>,
    /// When [`HealthMonitor::check`] last ran, for the step it charges each beacon.
    last_check: Mutex<Option<Instant>>,
}

impl HealthMonitor {
    /// A monitor with no beacons.
    pub fn new() -> Self {
        Self::default()
    }

    /// The named beacon [`NAMED_LIMIT`] watches, creating and arming it once. A second call is
    /// idempotent: it never resets a running beacon's quiet time. `name` is `scheduler`,
    /// `adapter:<transport>` or `outcome_sink`.
    pub fn named(&self, name: &str) -> Beacon {
        self.beacons
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(name.to_owned())
            .or_insert_with(|| Beacon::new(name, NAMED_LIMIT))
            .clone()
    }

    /// The beacon for `scope`'s Turn, armed with [`turn_limit`] and re-armed on every call so
    /// each run in the Turn gets the full timeout again. The name is `turn:<scope>`.
    pub fn turn(&self, scope: &Scope, run_timeout: Duration) -> Beacon {
        let limit = turn_limit(run_timeout);
        let name = format!("turn:{scope}");
        let mut beacons = self.beacons.lock().unwrap_or_else(|e| e.into_inner());
        match beacons.entry(name) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().arm(limit);
                entry.get().clone()
            }
            Entry::Vacant(entry) => {
                let beacon = Beacon::new(entry.key(), limit);
                entry.insert(beacon.clone());
                beacon
            }
        }
    }

    /// Every armed beacon, in name order (#27 D5): what `DaemonStatus.beacons[]` serves.
    pub fn beacons(&self) -> Vec<StatusBeacon> {
        self.beacons
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter_map(|beacon| {
                let state = beacon.state();
                state.limit.map(|_| StatusBeacon {
                    beacon: beacon.name().to_owned(),
                    quiet_ms: millis(state.quiet),
                })
            })
            .collect()
    }

    /// Charges every armed beacon the step since the last check, capped at [`STEP_CAP`], and
    /// returns the first beacon that has passed its limit. The first call only starts the clock:
    /// it charges nothing.
    pub fn check(&self) -> Option<Stall> {
        let now = Instant::now();
        let step = {
            let mut last = self.last_check.lock().unwrap_or_else(|e| e.into_inner());
            let step = last
                .map(|last| now.saturating_duration_since(last).min(STEP_CAP))
                .unwrap_or(Duration::ZERO);
            *last = Some(now);
            step
        };
        let beacons = self.beacons.lock().unwrap_or_else(|e| e.into_inner());
        beacons.iter().find_map(|(name, beacon)| {
            beacon.accrue(step).map(|quiet_ms| Stall {
                beacon: name.clone(),
                quiet_ms,
            })
        })
    }

    /// The monitor task: ticks every [`TICK`], logs `worker.stalled` at ERROR when a beacon goes
    /// quiet and returns the [`Stall`]. The caller shuts down and exits 75.
    pub async fn run(&self) -> Stall {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Some(stall) = self.check() {
                tracing::error!(
                    event = event::WORKER_STALLED,
                    beacon = %stall.beacon,
                    quiet_ms = stall.quiet_ms,
                    "a beacon went quiet; the Worker shuts down"
                );
                return stall;
            }
        }
    }
}

/// A [`Duration`] in milliseconds, saturating at `u64::MAX`.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn advance(by: Duration) {
        tokio::time::advance(by).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_beating_beacon_never_stalls() {
        let monitor = HealthMonitor::new();
        let beacon = monitor.named("scheduler");
        assert!(
            monitor.check().is_none(),
            "the first check only starts the clock"
        );
        for _ in 0..100 {
            advance(TICK).await;
            beacon.beat();
            assert!(monitor.check().is_none());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_named_beacon_stalls_at_exactly_its_limit() {
        let monitor = HealthMonitor::new();
        monitor.named("scheduler");
        monitor.check();
        for _ in 0..59 {
            advance(TICK).await;
            assert!(monitor.check().is_none());
        }
        advance(TICK).await;
        let stall = monitor.check().expect("300 s of quiet is a Stall");
        assert_eq!(stall.beacon, "scheduler");
        assert_eq!(stall.quiet_ms, 300_000);
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_gap_adds_at_most_the_step_cap() {
        let monitor = HealthMonitor::new();
        let beacon = monitor.named("outcome_sink");
        monitor.check();
        // Ten minutes pass in one step: the cap holds it to 10 s.
        advance(Duration::from_secs(600)).await;
        assert!(monitor.check().is_none());
        assert_eq!(beacon.quiet_ms(), 10_000);
    }

    #[tokio::test(start_paused = true)]
    async fn run_returns_the_first_stall() {
        let monitor = HealthMonitor::new();
        monitor.named("scheduler").arm(Duration::ZERO);
        let stall = tokio::time::timeout(Duration::from_secs(1), monitor.run())
            .await
            .expect("an already-quiet beacon stalls on the first tick");
        assert_eq!(stall.beacon, "scheduler");
        assert_eq!(stall.quiet_ms, 0);
    }

    #[test]
    fn turn_limit_adds_the_graces() {
        assert_eq!(
            turn_limit(Duration::from_secs(300)),
            Duration::from_secs(545)
        );
        assert_eq!(turn_limit(Duration::ZERO), Duration::from_secs(245));
    }

    #[tokio::test(start_paused = true)]
    async fn beacons_are_sorted_and_disarm_removes_one() {
        let monitor = HealthMonitor::new();
        let scheduler = monitor.named("scheduler");
        monitor.named("adapter:discord");
        monitor.named("outcome_sink");
        monitor.turn(&Scope::Discord("123".into()), Duration::from_secs(300));
        let names = |monitor: &HealthMonitor| {
            monitor
                .beacons()
                .into_iter()
                .map(|b| b.beacon)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&monitor),
            [
                "adapter:discord",
                "outcome_sink",
                "scheduler",
                "turn:discord:123"
            ]
        );
        scheduler.disarm();
        assert_eq!(
            names(&monitor),
            ["adapter:discord", "outcome_sink", "turn:discord:123"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn arming_resets_quiet_time() {
        let monitor = HealthMonitor::new();
        let beacon = monitor.named("scheduler");
        monitor.check();
        advance(TICK).await;
        assert!(monitor.check().is_none());
        assert_eq!(beacon.quiet_ms(), 5_000);

        beacon.arm(NAMED_LIMIT);
        assert_eq!(beacon.quiet_ms(), 0);
        advance(Duration::from_secs(600)).await;
        assert!(monitor.check().is_none());
        assert_eq!(beacon.quiet_ms(), 10_000);
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_re_arms_on_every_run() {
        let monitor = HealthMonitor::new();
        let scope = Scope::Job("daily".into());
        let beacon = monitor.turn(&scope, Duration::from_secs(300));
        monitor.check();
        advance(TICK).await;
        assert!(monitor.check().is_none());
        assert_eq!(beacon.quiet_ms(), 5_000);

        let again = monitor.turn(&scope, Duration::from_secs(300));
        assert_eq!(again.name(), "turn:job:daily");
        assert_eq!(beacon.quiet_ms(), 0);
    }

    #[test]
    fn a_stall_renders_its_fatal_message() {
        let stall = Stall {
            beacon: "outcome_sink".into(),
            quiet_ms: 301_500,
        };
        assert_eq!(stall.fatal_message(), "stall: outcome_sink quiet 301s");
    }
}
