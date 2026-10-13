//! `api::Deadline` (#40 D6): one table, used by the app's bridge, the CLI and the server's
//! backstop, so nothing waits forever and the server answers `504 deadline_exceeded` before the
//! client gives up.

use std::time::Duration;

use serde::Serialize;

/// A route's deadline class. Published as `x-claudeclaw-deadline` in `openapi.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Deadline {
    /// `health.get`: 3 s, the app's reachability probe (#20). No server backstop.
    Probe,
    /// Everything not listed elsewhere: 30 s.
    Short,
    /// `turns.create`: 120 s plus `wait`.
    Upload,
    /// `jobs.run`, `sessions.compact`, `pairing.get`: 30 s plus `wait`.
    Wait,
    /// `launchers.test`: its `timeout_s` plus 45 s.
    Test,
    /// Export, import, update check, usage, jobs audit: 120 s.
    Slow,
    /// The three SSE families: no total deadline, [`Deadline::STREAM_IDLE`] of silence instead.
    Stream,
}

/// The parts of a concrete request a deadline depends on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Call {
    /// The `?wait=` query value in seconds; 0 when absent.
    pub wait_s: u32,
    /// `launchers.test`'s `timeout_s`; [`Deadline::DEFAULT_TEST_TIMEOUT_S`] when absent.
    pub timeout_s: Option<u32>,
}

impl Deadline {
    /// SSE silence before a client reconnects: three missed 15 s keep-alives.
    pub const STREAM_IDLE: Duration = Duration::from_secs(45);
    /// The server's backstop fires this much before the client's deadline.
    pub const BACKSTOP_MARGIN: Duration = Duration::from_secs(5);
    /// `LauncherTestRequest.timeout_s` when the request leaves it out.
    pub const DEFAULT_TEST_TIMEOUT_S: u32 = 120;

    /// How long the app or the CLI waits for the whole response. `None` for streams.
    pub fn client(self, call: Call) -> Option<Duration> {
        let secs = match self {
            Deadline::Probe => 3,
            Deadline::Short => 30,
            Deadline::Upload => 120 + u64::from(call.wait_s),
            Deadline::Wait => 30 + u64::from(call.wait_s),
            Deadline::Test => {
                u64::from(call.timeout_s.unwrap_or(Self::DEFAULT_TEST_TIMEOUT_S)) + 45
            }
            Deadline::Slow => 120,
            Deadline::Stream => return None,
        };
        Some(Duration::from_secs(secs))
    }

    /// When the server answers `504 deadline_exceeded`. `None` for the probe and streams.
    pub fn server(self, call: Call) -> Option<Duration> {
        match self {
            Deadline::Probe | Deadline::Stream => None,
            _ => self.client(call).map(|d| d - Self::BACKSTOP_MARGIN),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(d: Option<Duration>) -> Option<u64> {
        d.map(|d| d.as_secs())
    }

    #[test]
    fn the_d6_table() {
        let none = Call::default();
        let wait_60 = Call {
            wait_s: 60,
            ..Call::default()
        };
        let test_600 = Call {
            timeout_s: Some(600),
            ..Call::default()
        };
        // (class, call, client, server) from #40 D6.
        let rows = [
            (Deadline::Probe, none, Some(3), None),
            (Deadline::Short, none, Some(30), Some(25)),
            (Deadline::Upload, none, Some(120), Some(115)),
            (Deadline::Upload, wait_60, Some(180), Some(175)),
            (Deadline::Wait, wait_60, Some(90), Some(85)),
            (Deadline::Test, none, Some(165), Some(160)),
            (Deadline::Test, test_600, Some(645), Some(640)),
            (Deadline::Slow, none, Some(120), Some(115)),
            (Deadline::Stream, wait_60, None, None),
        ];
        for (deadline, call, client, server) in rows {
            assert_eq!(secs(deadline.client(call)), client, "{deadline:?} client");
            assert_eq!(secs(deadline.server(call)), server, "{deadline:?} server");
        }
    }
}
