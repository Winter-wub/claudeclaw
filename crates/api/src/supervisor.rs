//! The Watchdog's status file, the Run registry and the not-answering states (architecture §4
//! "claudeclaw-api"; #20, #27, #29, #36, #37, #45). The client reads these files for a Daemon
//! whose API is down, so their shapes live here.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{PartialSchema, ToSchema};

use crate::adapter::Transport;
use crate::scope::Scope;
use crate::wire::{string_enum, wire_enum};

/// `daemon/state/supervisor.json`, written by the Watchdog on every transition and embedded in
/// `DaemonStatus.supervisor` (#20 D2; architecture §4 "Status file"). Every change after
/// `schema: 1` is additive (#27, #29, #36, #45).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SupervisorStatus {
    /// [`SupervisorStatus::SCHEMA`]. A reader that sees a higher value reports `unknown` (#20 D1).
    pub schema: u32,
    pub state: SupervisorState,
    pub updated_at: DateTime<Utc>,
    pub watchdog: WatchdogInfo,
    /// `null` when no Worker is running.
    #[schema(required = true)]
    pub worker: Option<WorkerInfo>,
    /// Set only in `backoff`.
    #[schema(required = true)]
    pub next_restart_at: Option<DateTime<Utc>>,
    /// The last intentional stop, or `null` if there was none.
    #[schema(required = true)]
    pub stop: Option<StopRecord>,
    /// `false` when `CLAUDECLAW_HANG_KILL=0` turned the Hang kill off (#27 D3).
    #[serde(default = "yes")]
    #[schema(default = true)]
    pub hang_kill: bool,
    /// `null` unless `state` is `gave_up`.
    #[schema(required = true)]
    pub gave_up: Option<GaveUp>,
    /// Cleared Give-ups, newest first, the last 10 (#45 D3).
    #[serde(default)]
    #[schema(required = true)]
    pub give_ups: Vec<ClearedGiveUp>,
    /// The Crash record: newest first, the last 20. It carries across Watchdog starts (#20 D2).
    pub crashes: Vec<Crash>,
}

impl SupervisorStatus {
    /// The `schema` this build writes.
    pub const SCHEMA: u32 = 1;
}

fn yes() -> bool {
    true
}

wire_enum! {
    /// The Watchdog's state (#20 D2).
    pub enum SupervisorState {
        Starting = "starting",
        Running = "running",
        /// Waiting `next_restart_at` after a crash.
        Backoff = "backoff",
        Stopping = "stopping",
        Stopped = "stopped",
        GaveUp = "gave_up",
    }
}

/// `supervisor.json.watchdog` (#20 D2, #36, #45).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WatchdogInfo {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub version: String,
    /// The OS start stamp that `stop` checks before it signals `pid` (#36).
    pub stamp: ProcessStamp,
    /// `null` unless started with `--service-manager` (#45 D4).
    #[schema(required = true)]
    pub service_manager: Option<ServiceManager>,
    /// The start-up Reap's counts, recorded against this Watchdog start (#29 D5).
    #[serde(default)]
    #[schema(required = true)]
    pub reaped: ReapCounts,
}

/// `watchdog.service_manager` (#45 D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServiceManager {
    pub kind: ServiceManagerKind,
    /// The launchd label or the systemd unit, best effort; `null` when it couldn't be read.
    #[schema(required = true)]
    pub name: Option<String>,
}

wire_enum! {
    /// The Service manager named by `--service-manager` (#45 D4).
    pub enum ServiceManagerKind {
        Launchd = "launchd",
        Systemd = "systemd",
    }
}

/// `supervisor.json.worker` (#20 D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkerInfo {
    pub pid: u32,
    pub boot_id: String,
    pub started_at: DateTime<Utc>,
}

/// `supervisor.json.stop`, the last intentional stop (#20 D2, #45 D3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StopRecord {
    pub at: DateTime<Utc>,
    pub by: StopBy,
}

wire_enum! {
    /// Who stopped the Daemon (#20 D2, #45 D3).
    pub enum StopBy {
        Api = "api",
        Cli = "cli",
        /// `claudeclaw clear-give-up` (#45 D3).
        ClearGiveUp = "clear_give_up",
    }
}

/// `supervisor.json.gave_up`: the Give-up that holds every later start (#20 D2, #45 D1–D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GaveUp {
    pub at: DateTime<Utc>,
    pub crashes_in_window: u32,
    /// `null` until the first Held start (#45 D2).
    #[schema(required = true)]
    pub held_starts: Option<HeldStarts>,
    /// The last reminder DM, sent at most once per 24 h (#45 D2).
    #[schema(required = true)]
    pub reminded_at: Option<DateTime<Utc>>,
}

/// `gave_up.held_starts` (#45 D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HeldStarts {
    pub count: u32,
    pub last_at: DateTime<Utc>,
    pub last_by: HeldStartBy,
}

wire_enum! {
    /// What made the last Held start (#45 D2).
    pub enum HeldStartBy {
        Launchd = "launchd",
        Systemd = "systemd",
        Cli = "cli",
    }
}

/// One entry of `give_ups[]`: a [`GaveUp`] that a person cleared (#45 D3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClearedGiveUp {
    pub at: DateTime<Utc>,
    pub crashes_in_window: u32,
    #[schema(required = true)]
    pub held_starts: Option<HeldStarts>,
    #[schema(required = true)]
    pub reminded_at: Option<DateTime<Utc>>,
    pub cleared_at: DateTime<Utc>,
    pub cleared_by: ClearedBy,
}

wire_enum! {
    /// How a Give-up was cleared (#45 D3). The wire values are #45's, hyphen included.
    pub enum ClearedBy {
        /// `start --clear-give-up`.
        Start = "start",
        /// `claudeclaw clear-give-up`.
        ClearGiveUp = "clear-give-up",
    }
}

/// One Crash record entry (#20 D2, #27 D6, #29 D5, #36). Its `fatal` reason and `stderr_tail`
/// are redacted but still free text, so `Debug` prints only their sizes.
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Crash {
    pub at: DateTime<Utc>,
    /// The `boot_id` of the Worker that crashed.
    pub boot_id: String,
    pub uptime_ms: u64,
    pub exit: CrashExit,
    /// What the Worker sent as `fatal(reason)`, or the last stall report for `hung`; `null` if
    /// nothing was sent.
    #[schema(required = true)]
    pub fatal: Option<String>,
    pub cause: CrashCause,
    /// Whether it counts toward the Give-up (#27 D6).
    pub counted: bool,
    /// The Reap after this exit (#29 D5).
    pub reaped: ReapCounts,
    /// The redacted stderr tail, on the newest 5 crashes only: at most 40 lines and 8 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<Vec<String>>,
}

impl fmt::Debug for Crash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Crash")
            .field("at", &self.at)
            .field("boot_id", &self.boot_id)
            .field("uptime_ms", &self.uptime_ms)
            .field("exit", &self.exit)
            .field("fatal_bytes", &self.fatal.as_ref().map(String::len))
            .field("cause", &self.cause)
            .field("counted", &self.counted)
            .field("reaped", &self.reaped)
            .field(
                "stderr_tail_lines",
                &self.stderr_tail.as_ref().map(Vec::len),
            )
            .finish()
    }
}

/// How the Worker ended: `{"code": 101}` or `{"signal": "SIGSEGV"}` (#20 D2). One of the two is
/// set; a form added later in v1 parses with neither set.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct CrashExit {
    /// The exit status. Wide enough for Windows statuses such as `0xC0000409` (#36).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

wire_enum! {
    /// Why the Worker ended (#27 D6, #36).
    pub enum CrashCause {
        /// A non-zero exit or a signal with no `draining` first.
        Exit = "exit",
        Fatal = "fatal",
        /// A stall-monitor beacon went quiet (#27 D5).
        Stall = "stall",
        /// The Hang kill after 45 s of awake silence (#27 D3).
        Hung = "hung",
        /// No exit within the deadline after a planned drain; not counted (#27 D6).
        ShutdownOverrun = "shutdown_overrun",
        /// `abort()`, including Windows status `0xC0000409` (#36).
        Abort = "abort",
    }
}

/// A Reap's per-outcome counts (#29 D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct ReapCounts {
    pub killed: u32,
    pub already_gone: u32,
    pub not_ours: u32,
    pub unverified: u32,
    pub unknown_pid: u32,
    pub survived: u32,
}

/// An OS process start stamp, which proves a pid is still the process that was recorded (#29
/// D2–D3, #36). Exactly one field is set; an OS added later in v1 parses with none set,
/// which a reaper treats as unprovable.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct ProcessStamp {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos: Option<MacosStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux: Option<LinuxStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<WindowsStamp>,
}

/// `proc_pidinfo(PROC_PIDTBSDINFO)`'s start time (#29 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MacosStamp {
    pub sec: u64,
    pub usec: u64,
}

/// The boot and `/proc/<pid>/stat` field 22 (#29 D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LinuxStamp {
    pub boot_id: String,
    pub start_ticks: u64,
}

/// `GetProcessTimes`' creation time as a FILETIME (#29 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WindowsStamp {
    pub creation_time: u64,
}

/// What the app and `claudeclaw status` show for a Daemon (#20 D1; #22 §5): the 9 local states
/// ([`DaemonReachability::LOCAL`]) and the 7 remote ones ([`DaemonReachability::REMOTE`]),
/// which share `running` and `not_responding`. Unlike [`wire_enum!`] sets, `unknown` is a real
/// value here (a local state); a value added later in v1 parses as `Unknown` too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonReachability {
    /// The API answers.
    Running,
    Starting,
    /// The Worker crashed and the Watchdog is in backoff.
    Restarting,
    NotResponding,
    GaveUp,
    /// Stopped, or never started.
    Stopped,
    /// Killed hard: the lock is free but `supervisor.json` still says it runs.
    Vanished,
    /// `daemon.pid` names another live process, e.g. the TS daemon.
    Foreign,
    /// Remote: the connection was lost less than 90 s ago.
    Reconnecting,
    /// Remote: at least 90 s of failures.
    Unreachable,
    /// Remote: the TLS pin doesn't match.
    IdentityChanged,
    /// Remote: the server rejected this Device (`device_revoked`).
    Revoked,
    /// Remote: back with a new `boot_id`.
    Recovered,
    /// Local: the lock is held but `supervisor.json` is missing or has a higher `schema`.
    #[serde(other)]
    Unknown,
}

impl DaemonReachability {
    /// Every value the server and the client send, `unknown` included.
    pub const KNOWN: &[DaemonReachability] = &[
        DaemonReachability::Running,
        DaemonReachability::Starting,
        DaemonReachability::Restarting,
        DaemonReachability::NotResponding,
        DaemonReachability::GaveUp,
        DaemonReachability::Stopped,
        DaemonReachability::Vanished,
        DaemonReachability::Foreign,
        DaemonReachability::Unknown,
        DaemonReachability::Reconnecting,
        DaemonReachability::Unreachable,
        DaemonReachability::IdentityChanged,
        DaemonReachability::Revoked,
        DaemonReachability::Recovered,
    ];

    /// The 9 states of a local Daemon (#20 D1).
    pub const LOCAL: &[DaemonReachability] = &[
        DaemonReachability::Running,
        DaemonReachability::Starting,
        DaemonReachability::Restarting,
        DaemonReachability::NotResponding,
        DaemonReachability::GaveUp,
        DaemonReachability::Stopped,
        DaemonReachability::Vanished,
        DaemonReachability::Foreign,
        DaemonReachability::Unknown,
    ];

    /// The 7 states of a remote Daemon (#20 D1).
    pub const REMOTE: &[DaemonReachability] = &[
        DaemonReachability::Running,
        DaemonReachability::Reconnecting,
        DaemonReachability::NotResponding,
        DaemonReachability::Unreachable,
        DaemonReachability::IdentityChanged,
        DaemonReachability::Revoked,
        DaemonReachability::Recovered,
    ];
}

impl PartialSchema for DaemonReachability {
    fn schema() -> RefOr<Schema> {
        string_enum(Self::KNOWN)
    }
}

impl ToSchema for DaemonReachability {}

/// `daemon/state/runs.json`, the Run registry: every process the Worker spawned that isn't
/// settled yet (#29 D2, #37 D3; architecture §4 "claudeclaw-api").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RunRegistry {
    /// [`RunRegistry::SCHEMA`].
    pub schema: u32,
    /// Written first thing at Worker start; `null` when no Worker runs.
    #[schema(required = true)]
    pub worker: Option<RegistryWorker>,
    pub runs: Vec<RegistryEntry>,
    /// Entries a Reap could not end (#29 D5).
    pub leftovers: Vec<Leftover>,
    /// The last Reap's results, consumed and cleared by the next Worker (#29 D6).
    pub reaped: Vec<ReapedRun>,
}

impl RunRegistry {
    /// The `schema` this build writes.
    pub const SCHEMA: u32 = 1;
}

/// `runs.json.worker` (#29 D2, D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RegistryWorker {
    pub pid: u32,
    pub boot_id: String,
    pub stamp: ProcessStamp,
}

/// One spawned process in the Run registry (#29 D2, #37 D3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RegistryEntry {
    /// The Run index's `run_id` (#23).
    pub run_id: String,
    pub state: RegistryState,
    /// `null` while `spawning`. On Unix it is also the pgid.
    #[schema(required = true)]
    pub pid: Option<u32>,
    /// `null` while `spawning`.
    #[schema(required = true)]
    pub stamp: Option<ProcessStamp>,
    pub kill: KillMethod,
    pub purpose: SpawnPurpose,
    pub scope: Scope,
    /// The chat message that started it; `null` for jobs, the heartbeat and API Turns.
    #[schema(required = true)]
    pub origin: Option<ChatOrigin>,
    pub started_at: DateTime<Utc>,
}

wire_enum! {
    /// Where a registered run is (#29 D2, #37 D3).
    pub enum RegistryState {
        /// Written before the spawn, so a death mid-spawn is reported (`unknown_pid`).
        Spawning = "spawning",
        Running = "running",
        /// The leader exited and Stragglers are being settled (#37).
        Settling = "settling",
    }
}

wire_enum! {
    /// How the run is ended (#29 D1).
    pub enum KillMethod {
        ProcessGroup = "process_group",
        JobObject = "job_object",
    }
}

wire_enum! {
    /// Why the process was spawned (#29 D2).
    pub enum SpawnPurpose {
        Turn = "turn",
        Compact = "compact",
        /// A session-rotation summary.
        Summary = "summary",
        TestLauncher = "test_launcher",
    }
}

/// The chat message a run answers, so the next Worker can send the interrupted reply (#29 D2,
/// D6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChatOrigin {
    pub adapter: Transport,
    pub channel: String,
    pub message: String,
}

/// A Run registry entry a Reap could not end (#29 D5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Leftover {
    #[serde(flatten)]
    pub entry: RegistryEntry,
    /// `unverified`, `unknown_pid` or `survived`.
    pub outcome: ReapOutcome,
    pub checked_at: DateTime<Utc>,
}

/// One run of the last Reap (#29 D2, D6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReapedRun {
    pub run_id: String,
    pub scope: Scope,
    #[schema(required = true)]
    pub origin: Option<ChatOrigin>,
    pub purpose: SpawnPurpose,
    /// A `settling` run never makes its job or chat `interrupted` (#37 D3).
    pub state: RegistryState,
    pub outcome: ReapOutcome,
}

wire_enum! {
    /// What a Reap found for one entry (#29 D3, D5).
    pub enum ReapOutcome {
        Killed = "killed",
        AlreadyGone = "already_gone",
        /// The pid was reused by another process.
        NotOurs = "not_ours",
        /// The leader is gone and no group member proved it ours.
        Unverified = "unverified",
        /// The entry was still `spawning`.
        UnknownPid = "unknown_pid",
        Survived = "survived",
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    SupervisorStatus,
    SupervisorState,
    WatchdogInfo,
    ServiceManager,
    ServiceManagerKind,
    WorkerInfo,
    StopRecord,
    StopBy,
    GaveUp,
    HeldStarts,
    HeldStartBy,
    ClearedGiveUp,
    ClearedBy,
    Crash,
    CrashExit,
    CrashCause,
    ReapCounts,
    ProcessStamp,
    MacosStamp,
    LinuxStamp,
    WindowsStamp,
    DaemonReachability,
    RunRegistry,
    RegistryWorker,
    RegistryEntry,
    RegistryState,
    KillMethod,
    SpawnPurpose,
    ChatOrigin,
    Leftover,
    ReapedRun,
    ReapOutcome,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use utoipa::PartialSchema;

    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn macos(sec: u64) -> ProcessStamp {
        ProcessStamp {
            macos: Some(MacosStamp { sec, usec: 250 }),
            ..ProcessStamp::default()
        }
    }

    /// A Daemon that gave up after a hang and a fatal exit, then refused one start under launchd.
    fn gave_up() -> (SupervisorStatus, Value) {
        let status = SupervisorStatus {
            schema: SupervisorStatus::SCHEMA,
            state: SupervisorState::GaveUp,
            updated_at: at("2026-10-13T14:20:00Z"),
            watchdog: WatchdogInfo {
                pid: 4100,
                started_at: at("2026-10-13T14:19:59Z"),
                version: "2.0.0".into(),
                stamp: macos(1_791_900_000),
                service_manager: Some(ServiceManager {
                    kind: ServiceManagerKind::Launchd,
                    name: Some("com.claudeclaw.v2.demo".into()),
                }),
                reaped: ReapCounts::default(),
            },
            worker: None,
            next_restart_at: None,
            stop: Some(StopRecord {
                at: at("2026-10-12T09:00:00Z"),
                by: StopBy::Cli,
            }),
            hang_kill: true,
            gave_up: Some(GaveUp {
                at: at("2026-10-13T14:05:00Z"),
                crashes_in_window: 6,
                held_starts: Some(HeldStarts {
                    count: 1,
                    last_at: at("2026-10-13T14:20:00Z"),
                    last_by: HeldStartBy::Launchd,
                }),
                reminded_at: Some(at("2026-10-13T14:20:00Z")),
            }),
            give_ups: vec![ClearedGiveUp {
                at: at("2026-10-01T08:00:00Z"),
                crashes_in_window: 6,
                held_starts: None,
                reminded_at: None,
                cleared_at: at("2026-10-01T09:30:00Z"),
                cleared_by: ClearedBy::ClearGiveUp,
            }],
            crashes: vec![
                Crash {
                    at: at("2026-10-13T14:05:00Z"),
                    boot_id: "0199e0c2-0000-7000-8000-00000000000b".into(),
                    uptime_ms: 51_000,
                    exit: CrashExit {
                        code: None,
                        signal: Some("SIGKILL".into()),
                    },
                    fatal: Some("stalled seq=9 waited_ms=10048".into()),
                    cause: CrashCause::Hung,
                    counted: true,
                    reaped: ReapCounts {
                        killed: 2,
                        unverified: 1,
                        ..ReapCounts::default()
                    },
                    stderr_tail: Some(vec!["WARN worker.stalled".into()]),
                },
                Crash {
                    at: at("2026-10-13T14:03:00Z"),
                    boot_id: "0199e0c2-0000-7000-8000-00000000000a".into(),
                    uptime_ms: 2_400,
                    exit: CrashExit {
                        code: Some(101),
                        signal: None,
                    },
                    fatal: None,
                    cause: CrashCause::Exit,
                    counted: true,
                    reaped: ReapCounts::default(),
                    stderr_tail: None,
                },
            ],
        };
        let zero = json!({
            "killed": 0, "already_gone": 0, "not_ours": 0,
            "unverified": 0, "unknown_pid": 0, "survived": 0,
        });
        let json = json!({
            "schema": 1,
            "state": "gave_up",
            "updated_at": "2026-10-13T14:20:00Z",
            "watchdog": {
                "pid": 4100,
                "started_at": "2026-10-13T14:19:59Z",
                "version": "2.0.0",
                "stamp": {"macos": {"sec": 1_791_900_000_u64, "usec": 250}},
                "service_manager": {"kind": "launchd", "name": "com.claudeclaw.v2.demo"},
                "reaped": zero,
            },
            "worker": null,
            "next_restart_at": null,
            "stop": {"at": "2026-10-12T09:00:00Z", "by": "cli"},
            "hang_kill": true,
            "gave_up": {
                "at": "2026-10-13T14:05:00Z",
                "crashes_in_window": 6,
                "held_starts": {"count": 1, "last_at": "2026-10-13T14:20:00Z", "last_by": "launchd"},
                "reminded_at": "2026-10-13T14:20:00Z",
            },
            "give_ups": [{
                "at": "2026-10-01T08:00:00Z",
                "crashes_in_window": 6,
                "held_starts": null,
                "reminded_at": null,
                "cleared_at": "2026-10-01T09:30:00Z",
                "cleared_by": "clear-give-up",
            }],
            "crashes": [
                {
                    "at": "2026-10-13T14:05:00Z",
                    "boot_id": "0199e0c2-0000-7000-8000-00000000000b",
                    "uptime_ms": 51000,
                    "exit": {"signal": "SIGKILL"},
                    "fatal": "stalled seq=9 waited_ms=10048",
                    "cause": "hung",
                    "counted": true,
                    "reaped": {
                        "killed": 2, "already_gone": 0, "not_ours": 0,
                        "unverified": 1, "unknown_pid": 0, "survived": 0,
                    },
                    "stderr_tail": ["WARN worker.stalled"],
                },
                {
                    "at": "2026-10-13T14:03:00Z",
                    "boot_id": "0199e0c2-0000-7000-8000-00000000000a",
                    "uptime_ms": 2400,
                    "exit": {"code": 101},
                    "fatal": null,
                    "cause": "exit",
                    "counted": true,
                    "reaped": zero,
                },
            ],
        });
        (status, json)
    }

    #[test]
    fn a_held_give_up_with_two_crashes_round_trips() {
        let (status, json) = gave_up();
        assert_eq!(serde_json::to_value(&status).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<SupervisorStatus>(json).unwrap(),
            status
        );
    }

    #[test]
    fn fields_added_after_schema_1_may_be_missing() {
        let (_, mut json) = gave_up();
        for key in ["hang_kill", "give_ups"] {
            json.as_object_mut().unwrap().remove(key);
        }
        for key in ["service_manager", "reaped"] {
            json["watchdog"].as_object_mut().unwrap().remove(key);
        }
        for key in ["held_starts", "reminded_at"] {
            json["gave_up"].as_object_mut().unwrap().remove(key);
        }
        let status: SupervisorStatus = serde_json::from_value(json).unwrap();
        assert!(status.hang_kill);
        assert!(status.give_ups.is_empty());
        assert_eq!(status.watchdog.service_manager, None);
        assert_eq!(status.watchdog.reaped, ReapCounts::default());
        assert_eq!(status.gave_up.unwrap().held_starts, None);
    }

    #[test]
    fn later_values_parse_as_unknown() {
        let (_, mut json) = gave_up();
        json["state"] = json!("draining");
        json["crashes"][0]["cause"] = json!("added_in_v1_9");
        json["crashes"][0]["exit"] = json!({"exception": "0xC0000005"});
        json["watchdog"]["stamp"] = json!({"freebsd": {"start": 1}});
        let status: SupervisorStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.state, SupervisorState::Unknown);
        assert_eq!(status.crashes[0].cause, CrashCause::Unknown);
        assert_eq!(status.crashes[0].exit, CrashExit::default());
        assert_eq!(status.watchdog.stamp, ProcessStamp::default());
    }

    #[test]
    fn a_crash_debug_shows_only_sizes_of_its_text() {
        let (status, _) = gave_up();
        let debug = format!("{:?}", status.crashes[0]);
        assert!(!debug.contains("stalled seq"), "{debug}");
        assert!(!debug.contains("worker.stalled"), "{debug}");
        assert!(debug.contains("fatal_bytes: Some(29)"), "{debug}");
        assert!(debug.contains("stderr_tail_lines: Some(1)"), "{debug}");
    }

    #[test]
    fn reachability_has_9_local_and_7_remote_states() {
        assert_eq!(DaemonReachability::LOCAL.len(), 9);
        assert_eq!(DaemonReachability::REMOTE.len(), 7);
        for state in DaemonReachability::LOCAL
            .iter()
            .chain(DaemonReachability::REMOTE)
        {
            assert!(DaemonReachability::KNOWN.contains(state));
        }
        assert_eq!(
            serde_json::to_value(DaemonReachability::IdentityChanged).unwrap(),
            "identity_changed"
        );
        assert_eq!(
            serde_json::from_value::<DaemonReachability>(json!("unknown")).unwrap(),
            DaemonReachability::Unknown
        );
        assert_eq!(
            serde_json::from_value::<DaemonReachability>(json!("added_in_v1_9")).unwrap(),
            DaemonReachability::Unknown
        );
        let schema = serde_json::to_value(DaemonReachability::schema()).unwrap();
        assert_eq!(schema["enum"].as_array().unwrap().len(), 14);
        assert!(
            schema["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("unknown"))
        );
    }

    fn entry(run_id: &str, state: RegistryState, pid: Option<u32>) -> RegistryEntry {
        RegistryEntry {
            run_id: run_id.into(),
            state,
            pid,
            stamp: pid.map(|_| ProcessStamp {
                linux: Some(LinuxStamp {
                    boot_id: "b007".into(),
                    start_ticks: 88_000,
                }),
                ..ProcessStamp::default()
            }),
            kill: KillMethod::ProcessGroup,
            purpose: SpawnPurpose::Turn,
            scope: Scope::Discord("1234".into()),
            origin: Some(ChatOrigin {
                adapter: Transport::Discord,
                channel: "1234".into(),
                message: "5678".into(),
            }),
            started_at: at("2026-10-13T08:30:00Z"),
        }
    }

    fn entry_json(run_id: &str, state: &str, pid: Option<u32>) -> Value {
        let stamp = match pid {
            Some(_) => json!({"linux": {"boot_id": "b007", "start_ticks": 88000}}),
            None => Value::Null,
        };
        json!({
            "run_id": run_id,
            "state": state,
            "pid": pid,
            "stamp": stamp,
            "kill": "process_group",
            "purpose": "turn",
            "scope": "discord:1234",
            "origin": {"adapter": "discord", "channel": "1234", "message": "5678"},
            "started_at": "2026-10-13T08:30:00Z",
        })
    }

    #[test]
    fn a_run_registry_round_trips() {
        let mut job = entry("run-2", RegistryState::Settling, Some(702));
        job.purpose = SpawnPurpose::Compact;
        job.scope = "job:daily-report".parse().unwrap();
        job.origin = None;
        job.kill = KillMethod::JobObject;
        job.stamp = Some(ProcessStamp {
            windows: Some(WindowsStamp {
                creation_time: 134_000_000,
            }),
            ..ProcessStamp::default()
        });
        let registry = RunRegistry {
            schema: RunRegistry::SCHEMA,
            worker: Some(RegistryWorker {
                pid: 700,
                boot_id: "0199e0c2-0000-7000-8000-00000000000c".into(),
                stamp: macos(1_791_900_100),
            }),
            runs: vec![
                entry("run-1", RegistryState::Running, Some(701)),
                job,
                entry("run-3", RegistryState::Spawning, None),
            ],
            leftovers: vec![Leftover {
                entry: entry("run-0", RegistryState::Running, Some(600)),
                outcome: ReapOutcome::Unverified,
                checked_at: at("2026-10-13T08:29:00Z"),
            }],
            reaped: vec![ReapedRun {
                run_id: "run-9".into(),
                scope: Scope::Global,
                origin: None,
                purpose: SpawnPurpose::Summary,
                state: RegistryState::Settling,
                outcome: ReapOutcome::Killed,
            }],
        };
        let mut leftover = entry_json("run-0", "running", Some(600));
        leftover["outcome"] = json!("unverified");
        leftover["checked_at"] = json!("2026-10-13T08:29:00Z");
        let json = json!({
            "schema": 1,
            "worker": {
                "pid": 700,
                "boot_id": "0199e0c2-0000-7000-8000-00000000000c",
                "stamp": {"macos": {"sec": 1_791_900_100_u64, "usec": 250}},
            },
            "runs": [
                entry_json("run-1", "running", Some(701)),
                {
                    "run_id": "run-2",
                    "state": "settling",
                    "pid": 702,
                    "stamp": {"windows": {"creation_time": 134_000_000}},
                    "kill": "job_object",
                    "purpose": "compact",
                    "scope": "job:daily-report",
                    "origin": null,
                    "started_at": "2026-10-13T08:30:00Z",
                },
                entry_json("run-3", "spawning", None),
            ],
            "leftovers": [leftover],
            "reaped": [{
                "run_id": "run-9",
                "scope": "global",
                "origin": null,
                "purpose": "summary",
                "state": "settling",
                "outcome": "killed",
            }],
        });
        assert_eq!(serde_json::to_value(&registry).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<RunRegistry>(json).unwrap(),
            registry
        );
    }
}
