//! The wall clock the scheduler computes deadlines on (architecture §4 `scheduler` "Timing").

use std::time::SystemTime;

use chrono::{DateTime, Utc};

/// Where the scheduler reads the wall-clock time. Fire times, the 6 h catch-up window,
/// `retry_delay` and the heartbeat interval are all on the wall clock, never Awake time (#38
/// D7). Tests inject a fake one that moves with tokio's paused clock.
pub trait WallClock: Send + Sync + 'static {
    /// Now, on the wall clock.
    fn now(&self) -> DateTime<Utc>;
}

/// The system's wall clock (`SystemTime::now()`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl WallClock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from(SystemTime::now())
    }
}
