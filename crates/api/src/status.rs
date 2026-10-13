//! Daemon and status (management-api §2.1): `health.get`, `status.get`, the daemon control
//! routes, `update.*` and `housekeeping.run`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::adapter::{AdapterState, Transport};
use crate::runs::RunIndexState;
use crate::scope::Scope;
use crate::settings::{RunLocaleSetting, RunPathSource};
use crate::supervisor::{DaemonReachability, SupervisorStatus};
use crate::wire::wire_enum;

/// `health.get`: the reachability probe (management-api §2.1, §1.8; #20 D1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Health {
    pub state: HealthState,
    pub boot_id: String,
    /// The Daemon's version.
    pub version: String,
    /// The API version, `"1"` for v1.
    pub api_version: String,
    /// The First-start import's progress (#17), or `null` when none is running.
    #[schema(required = true)]
    pub import: Option<ImportProgress>,
}

wire_enum! {
    /// Where the Worker is in its life (management-api §1.8).
    pub enum HealthState {
        /// Serving reads; every mutation gets `503 daemon_starting`.
        Starting = "starting",
        Ready = "ready",
        /// Serving reads; every mutation gets `503 daemon_stopping`.
        Stopping = "stopping",
    }
}

/// `Health.import` (management-api §2.1, #17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportProgress {
    pub running: bool,
    /// The step running now, or `null` between steps.
    #[schema(required = true)]
    pub step: Option<String>,
}

/// `status.get`, and `claudeclaw status --json` (management-api §2.1; #22 §5, #20). Served
/// online with `source: api`; the CLI fills it from disk when the Daemon doesn't answer. Fields
/// only a live Worker knows are `null` from disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DaemonStatus {
    /// The project dir.
    pub dir: String,
    /// A local state (#20 D1); always `running` when served.
    pub state: DaemonReachability,
    pub health: DaemonHealth,
    pub problems: Vec<Problem>,
    /// Who holds `daemon.lock`; CLI-only, `null` online (#22 §3).
    #[schema(required = true)]
    pub lock_holder: Option<LockHolder>,
    /// CLI-only, `null` online.
    #[schema(required = true)]
    pub watchdog_pid: Option<u32>,
    /// CLI-only, `null` online.
    #[schema(required = true)]
    pub worker_pid: Option<u32>,
    pub version: StatusVersion,
    #[schema(required = true)]
    pub started_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub boot_id: Option<String>,
    /// The local socket or pipe.
    pub socket: String,
    pub remote: RemoteStatus,
    pub adapters: Vec<AdapterStatus>,
    pub heartbeat: HeartbeatStatus,
    pub jobs: JobsStatus,
    pub active_runs: u32,
    pub launchers: LaunchersStatus,
    pub update: UpdateStatus,
    /// The Run PATH (#26).
    #[schema(required = true)]
    pub run_path: Option<RunPath>,
    /// The Run locale (#43).
    #[schema(required = true)]
    pub run_locale: Option<RunLocale>,
    /// The Run index Back-fill's progress (#44).
    #[schema(required = true)]
    pub run_index: Option<RunIndexState>,
    /// The Worker's stall-monitor beacons (#27 D5).
    pub beacons: Vec<Beacon>,
    /// The advisory notice of #48: the login shell exports `CLAUDE_CONFIG_DIR`, but `direct`
    /// runs use a different Config dir. Only the presence and the mismatch are reported, never
    /// the value.
    pub config_dir_mismatch: bool,
    /// `supervisor.json` (#20 D2); `null` when it doesn't exist (never started).
    #[schema(required = true)]
    pub supervisor: Option<SupervisorStatus>,
    pub source: StatusSource,
}

wire_enum! {
    /// `DaemonStatus.health` (#22 §5).
    pub enum DaemonHealth {
        Ok = "ok",
        /// At least one problem in `problems[]` degrades it.
        Degraded = "degraded",
    }
}

wire_enum! {
    /// Where a [`DaemonStatus`] was read from (#22 §3).
    pub enum StatusSource {
        Api = "api",
        /// The CLI read the files because the Daemon doesn't answer.
        Disk = "disk",
    }
}

/// One `DaemonStatus.problems[]` item (management-api §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Problem {
    pub kind: ProblemKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    /// The Launcher, for `claude_code_outdated` (#42).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
    pub since: DateTime<Utc>,
}

wire_enum! {
    /// What a [`Problem`] is (management-api §2.1).
    pub enum ProblemKind {
        DiscordDisconnected = "discord_disconnected",
        LauncherRefused = "launcher_refused",
        JobInvalid = "job_invalid",
        SettingsInvalid = "settings_invalid",
        UpdateAvailable = "update_available",
        VersionMismatch = "version_mismatch",
        ImportErrors = "import_errors",
        /// #26.
        RunPathCaptureFailed = "run_path_capture_failed",
        /// A Reap left runs it couldn't end (#29).
        LeftoverRuns = "leftover_runs",
        /// #37.
        RunPipeHeld = "run_pipe_held",
        /// #42.
        ClaudeCodeOutdated = "claude_code_outdated",
        /// #43.
        RunLocaleUnavailable = "run_locale_unavailable",
        /// #44.
        DiskLow = "disk_low",
        /// #46.
        JobsDirUnavailable = "jobs_dir_unavailable",
    }
}

/// `DaemonStatus.lock_holder` (#22 §3, §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LockHolder {
    pub role: LockRole,
    #[schema(required = true)]
    pub pid: Option<u32>,
    /// The CLI command doing an Offline write, e.g. `jobs import`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

wire_enum! {
    /// Who holds `daemon.lock` (#22 §3).
    pub enum LockRole {
        Watchdog = "watchdog",
        /// An Offline write.
        Cli = "cli",
    }
}

/// `DaemonStatus.version` (#22 §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StatusVersion {
    pub daemon: String,
    /// The CLI's own version; only the CLI sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
}

/// `DaemonStatus.remote`: the pinned-TLS listener (#13, #41).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemoteStatus {
    pub enabled: bool,
    pub listen: Vec<String>,
    /// The certificate's SPKI fingerprint; `null` while remote access is off.
    #[schema(required = true)]
    pub fingerprint: Option<String>,
    /// The same pin in `curl --pinnedpubkey` form.
    #[schema(required = true)]
    pub fingerprint_curl: Option<String>,
}

/// One `DaemonStatus.adapters[]` item (management-api §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdapterStatus {
    pub transport: Transport,
    pub state: AdapterState,
    pub since: DateTime<Utc>,
}

/// `DaemonStatus.heartbeat` (#22 §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HeartbeatStatus {
    pub enabled: bool,
    #[schema(required = true)]
    pub next_at: Option<DateTime<Utc>>,
}

/// `DaemonStatus.jobs` (#22 §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobsStatus {
    pub total: u32,
    pub invalid: u32,
    pub warnings: u32,
    /// The next fires, soonest first.
    pub next: Vec<NextFire>,
}

/// One `jobs.next[]` item (#22 §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NextFire {
    pub job: String,
    pub at: DateTime<Utc>,
}

/// `DaemonStatus.launchers` (#22 §9, #26 D7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LaunchersStatus {
    /// `direct` or a Launcher path.
    pub default: String,
    pub refused_scopes: Vec<Scope>,
    /// Where `direct` resolves on the Run PATH, or `null` if it doesn't (#26 D7).
    #[schema(required = true)]
    pub direct_resolved: Option<String>,
}

/// `DaemonStatus.update` (#22 §9); [`UpdateInfo`] has the detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateStatus {
    pub available: bool,
    #[schema(required = true)]
    pub latest: Option<String>,
}

/// `DaemonStatus.run_path`, the PATH every run gets (#26 D7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RunPath {
    /// `run_path.source` as set.
    pub source_requested: RunPathSource,
    /// The source actually used, after `auto` and any fallback.
    pub source_used: RunPathSource,
    /// The login shell's basename; `null` when no shell was run.
    #[schema(required = true)]
    pub shell: Option<String>,
    pub dirs: Vec<String>,
    pub captured_at: DateTime<Utc>,
    pub duration_ms: u64,
    /// Why the capture failed, or `null`.
    #[schema(required = true)]
    pub error: Option<RunPathError>,
    /// Empty or relative entries left out.
    pub dropped_entries: u32,
}

/// `RunPath.error` (#26 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RunPathError {
    pub kind: RunPathErrorKind,
}

wire_enum! {
    /// How a Run PATH capture failed (#26 D2).
    pub enum RunPathErrorKind {
        SpawnFailed = "spawn_failed",
        Timeout = "timeout",
        NoMarker = "no_marker",
        BadOutput = "bad_output",
        UnsafeExePath = "unsafe_exe_path",
    }
}

/// `DaemonStatus.run_locale`: the character type every run gets (#43 D5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RunLocale {
    pub setting: RunLocaleSetting,
    pub source: RunLocaleSource,
    /// The `LC_CTYPE` value added to every run, or `null` when none was added.
    #[schema(required = true)]
    pub lc_ctype_added: Option<String>,
    /// The ctype name runs see.
    pub effective: String,
    /// `locale charmap` for `effective`, e.g. `UTF-8`.
    pub charmap: String,
    /// `false` when `locale` wasn't found and `C.UTF-8` was used unchecked (#43 D3).
    pub verified: bool,
}

wire_enum! {
    /// Why the Run locale is what it is (#43 D1, D6).
    pub enum RunLocaleSource {
        /// The inherited ctype is already UTF-8.
        Inherited = "inherited",
        /// `LC_CTYPE` was added.
        Default = "default",
        /// An inherited `LC_ALL` decides.
        LcAll = "lc_all",
        /// `run_locale: inherit`.
        Setting = "setting",
        /// No UTF-8 candidate exists on this machine.
        Unavailable = "unavailable",
        /// Windows.
        NotApplicable = "not_applicable",
    }
}

/// One stall-monitor beacon (#27 D5; management-api §8 item 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Beacon {
    /// `scheduler`, `adapter:<transport>`, `outcome_sink` or `turn:<scope>`.
    pub beacon: String,
    pub quiet_ms: u64,
}

/// `202` from `daemon.stop` (management-api §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StopAccepted {
    pub boot_id: String,
    pub in_flight_runs: u32,
    /// When the drain ends at the latest (#9).
    pub deadline_at: DateTime<Utc>,
}

/// `202` from `daemon.restart` (management-api §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestartAccepted {
    pub old_boot_id: String,
}

/// `daemon.reexec`'s body (management-api §2.1; #22 §7, #36).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReexecRequest {
    /// The unresolved `PATH` location from `argv[0]`, never `current_exe()`.
    pub exe: String,
}

/// `202` from `daemon.reexec` (management-api §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReexecAccepted {
    pub old_boot_id: String,
    /// What `<exe> --version` reported.
    pub target_version: String,
}

/// `update.get` and `update.check`: the Worker's daily release check (management-api §2.1; #6,
/// #35 §7). The channel follows from the running version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateInfo {
    pub current: String,
    /// The newest release on this channel; `null` before the first successful check.
    #[schema(required = true)]
    pub latest: Option<String>,
    pub available: bool,
    #[schema(required = true)]
    pub checked_at: Option<DateTime<Utc>>,
    /// The release page of `latest`.
    #[schema(required = true)]
    pub release_url: Option<String>,
}

/// `housekeeping.run`'s body (management-api §2.1, #44 D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HousekeepingRequest {
    /// Report what would be deleted, and delete nothing.
    #[serde(default)]
    #[schema(default = false)]
    pub dry_run: bool,
}

/// `housekeeping.run`'s answer (management-api §2.1, #44 D7). A dry run sets `would_delete`, a
/// real one `deleted`. Both map a kind (`inbox`, `run_logs`, …) to its totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HousekeepingReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub would_delete: Option<BTreeMap<String, HousekeepingTotal>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<BTreeMap<String, HousekeepingTotal>>,
    /// Freed, or that would be freed on a dry run.
    pub freed_bytes: u64,
}

/// One kind's totals in a [`HousekeepingReport`] (#44 D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HousekeepingTotal {
    pub count: u32,
    pub bytes: u64,
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    Health,
    HealthState,
    ImportProgress,
    DaemonStatus,
    DaemonHealth,
    StatusSource,
    Problem,
    ProblemKind,
    LockHolder,
    LockRole,
    StatusVersion,
    RemoteStatus,
    AdapterStatus,
    HeartbeatStatus,
    JobsStatus,
    NextFire,
    LaunchersStatus,
    UpdateStatus,
    RunPath,
    RunPathError,
    RunPathErrorKind,
    RunLocale,
    RunLocaleSource,
    Beacon,
    StopAccepted,
    RestartAccepted,
    ReexecRequest,
    ReexecAccepted,
    UpdateInfo,
    HousekeepingRequest,
    HousekeepingReport,
    HousekeepingTotal,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use utoipa::PartialSchema;

    use super::*;
    use crate::runs::RunIndexPhase;
    use crate::supervisor::{ProcessStamp, ReapCounts, SupervisorState, WatchdogInfo, WorkerInfo};

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn health_matches_the_spec_shape() {
        let health = Health {
            state: HealthState::Starting,
            boot_id: "0199e0c2-0000-7000-8000-00000000000c".into(),
            version: "2.0.0".into(),
            api_version: "1".into(),
            import: Some(ImportProgress {
                running: true,
                step: Some("jobs".into()),
            }),
        };
        let json = json!({
            "state": "starting",
            "boot_id": "0199e0c2-0000-7000-8000-00000000000c",
            "version": "2.0.0",
            "api_version": "1",
            "import": {"running": true, "step": "jobs"},
        });
        assert_eq!(serde_json::to_value(&health).unwrap(), json);
        assert_eq!(serde_json::from_value::<Health>(json).unwrap(), health);

        let ready = json!({
            "state": "ready",
            "boot_id": "b",
            "version": "2.0.0",
            "api_version": "1",
            "import": null,
        });
        let health: Health = serde_json::from_value(ready.clone()).unwrap();
        assert_eq!(health.import, None);
        assert_eq!(serde_json::to_value(&health).unwrap(), ready);
    }

    fn supervisor() -> (SupervisorStatus, Value) {
        let status = SupervisorStatus {
            schema: 1,
            state: SupervisorState::Running,
            updated_at: at("2026-10-13T08:00:01Z"),
            watchdog: WatchdogInfo {
                pid: 4100,
                started_at: at("2026-10-13T08:00:00Z"),
                version: "2.0.0".into(),
                stamp: ProcessStamp::default(),
                service_manager: None,
                reaped: ReapCounts::default(),
            },
            worker: Some(WorkerInfo {
                pid: 4101,
                boot_id: "0199e0c2-0000-7000-8000-00000000000c".into(),
                started_at: at("2026-10-13T08:00:00Z"),
            }),
            next_restart_at: None,
            stop: None,
            hang_kill: true,
            gave_up: None,
            give_ups: vec![],
            crashes: vec![],
        };
        let json = json!({
            "schema": 1,
            "state": "running",
            "updated_at": "2026-10-13T08:00:01Z",
            "watchdog": {
                "pid": 4100,
                "started_at": "2026-10-13T08:00:00Z",
                "version": "2.0.0",
                "stamp": {},
                "service_manager": null,
                "reaped": {
                    "killed": 0, "already_gone": 0, "not_ours": 0,
                    "unverified": 0, "unknown_pid": 0, "survived": 0,
                },
            },
            "worker": {
                "pid": 4101,
                "boot_id": "0199e0c2-0000-7000-8000-00000000000c",
                "started_at": "2026-10-13T08:00:00Z",
            },
            "next_restart_at": null,
            "stop": null,
            "hang_kill": true,
            "gave_up": null,
            "give_ups": [],
            "crashes": [],
        });
        (status, json)
    }

    fn online() -> (DaemonStatus, Value) {
        let (supervisor, supervisor_json) = supervisor();
        let status = DaemonStatus {
            dir: "~/bots/demo".into(),
            state: DaemonReachability::Running,
            health: DaemonHealth::Degraded,
            problems: vec![
                Problem {
                    kind: ProblemKind::ClaudeCodeOutdated,
                    scope: None,
                    job: None,
                    launcher: Some("direct".into()),
                    since: at("2026-10-13T08:00:05Z"),
                },
                Problem {
                    kind: ProblemKind::JobInvalid,
                    scope: Some("job:daily-report".parse().unwrap()),
                    job: Some("daily-report".into()),
                    launcher: None,
                    since: at("2026-10-13T08:00:06Z"),
                },
            ],
            lock_holder: None,
            watchdog_pid: None,
            worker_pid: None,
            version: StatusVersion {
                daemon: "2.0.0".into(),
                cli: None,
            },
            started_at: Some(at("2026-10-13T08:00:00Z")),
            boot_id: Some("0199e0c2-0000-7000-8000-00000000000c".into()),
            socket: "~/bots/demo/daemon/api.sock".into(),
            remote: RemoteStatus {
                enabled: false,
                listen: vec![],
                fingerprint: None,
                fingerprint_curl: None,
            },
            adapters: vec![AdapterStatus {
                transport: Transport::Discord,
                state: AdapterState::Connected,
                since: at("2026-10-13T08:00:03Z"),
            }],
            heartbeat: HeartbeatStatus {
                enabled: true,
                next_at: Some(at("2026-10-13T09:00:00Z")),
            },
            jobs: JobsStatus {
                total: 4,
                invalid: 1,
                warnings: 0,
                next: vec![NextFire {
                    job: "researcher/paper-digest".into(),
                    at: at("2026-10-13T09:30:00Z"),
                }],
            },
            active_runs: 1,
            launchers: LaunchersStatus {
                default: "direct".into(),
                refused_scopes: vec![Scope::Agent("researcher".into())],
                direct_resolved: Some("~/.local/bin/claude".into()),
            },
            update: UpdateStatus {
                available: false,
                latest: Some("2.0.0".into()),
            },
            run_path: Some(RunPath {
                source_requested: RunPathSource::Auto,
                source_used: RunPathSource::LoginShell,
                shell: Some("zsh".into()),
                dirs: vec!["~/.local/bin".into(), "/usr/bin".into()],
                captured_at: at("2026-10-13T08:00:01Z"),
                duration_ms: 1300,
                error: None,
                dropped_entries: 0,
            }),
            run_locale: Some(RunLocale {
                setting: RunLocaleSetting::Auto,
                source: RunLocaleSource::Default,
                lc_ctype_added: Some("C.UTF-8".into()),
                effective: "C.UTF-8".into(),
                charmap: "UTF-8".into(),
                verified: true,
            }),
            run_index: Some(RunIndexState {
                state: RunIndexPhase::Ready,
                done: 10,
                total: 10,
                since: at("2026-10-13T08:00:02Z"),
            }),
            beacons: vec![Beacon {
                beacon: "scheduler".into(),
                quiet_ms: 4000,
            }],
            config_dir_mismatch: true,
            supervisor: Some(supervisor),
            source: StatusSource::Api,
        };
        let json = json!({
            "dir": "~/bots/demo",
            "state": "running",
            "health": "degraded",
            "problems": [
                {"kind": "claude_code_outdated", "launcher": "direct", "since": "2026-10-13T08:00:05Z"},
                {
                    "kind": "job_invalid",
                    "scope": "job:daily-report",
                    "job": "daily-report",
                    "since": "2026-10-13T08:00:06Z",
                },
            ],
            "lock_holder": null,
            "watchdog_pid": null,
            "worker_pid": null,
            "version": {"daemon": "2.0.0"},
            "started_at": "2026-10-13T08:00:00Z",
            "boot_id": "0199e0c2-0000-7000-8000-00000000000c",
            "socket": "~/bots/demo/daemon/api.sock",
            "remote": {"enabled": false, "listen": [], "fingerprint": null, "fingerprint_curl": null},
            "adapters": [{"transport": "discord", "state": "connected", "since": "2026-10-13T08:00:03Z"}],
            "heartbeat": {"enabled": true, "next_at": "2026-10-13T09:00:00Z"},
            "jobs": {
                "total": 4,
                "invalid": 1,
                "warnings": 0,
                "next": [{"job": "researcher/paper-digest", "at": "2026-10-13T09:30:00Z"}],
            },
            "active_runs": 1,
            "launchers": {
                "default": "direct",
                "refused_scopes": ["agent:researcher"],
                "direct_resolved": "~/.local/bin/claude",
            },
            "update": {"available": false, "latest": "2.0.0"},
            "run_path": {
                "source_requested": "auto",
                "source_used": "login_shell",
                "shell": "zsh",
                "dirs": ["~/.local/bin", "/usr/bin"],
                "captured_at": "2026-10-13T08:00:01Z",
                "duration_ms": 1300,
                "error": null,
                "dropped_entries": 0,
            },
            "run_locale": {
                "setting": "auto",
                "source": "default",
                "lc_ctype_added": "C.UTF-8",
                "effective": "C.UTF-8",
                "charmap": "UTF-8",
                "verified": true,
            },
            "run_index": {"state": "ready", "done": 10, "total": 10, "since": "2026-10-13T08:00:02Z"},
            "beacons": [{"beacon": "scheduler", "quiet_ms": 4000}],
            "config_dir_mismatch": true,
            "supervisor": supervisor_json,
            "source": "api",
        });
        (status, json)
    }

    #[test]
    fn an_online_status_matches_the_spec_shape() {
        let (status, json) = online();
        assert_eq!(serde_json::to_value(&status).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<DaemonStatus>(json).unwrap(),
            status
        );
    }

    #[test]
    fn a_status_read_from_disk_round_trips() {
        let json = json!({
            "dir": "~/bots/demo",
            "state": "stopped",
            "health": "ok",
            "problems": [],
            "lock_holder": {"role": "cli", "pid": 812, "command": "jobs import"},
            "watchdog_pid": null,
            "worker_pid": null,
            "version": {"daemon": "2.0.0", "cli": "2.0.1"},
            "started_at": null,
            "boot_id": null,
            "socket": "~/bots/demo/daemon/api.sock",
            "remote": {"enabled": false, "listen": [], "fingerprint": null, "fingerprint_curl": null},
            "adapters": [],
            "heartbeat": {"enabled": false, "next_at": null},
            "jobs": {"total": 0, "invalid": 0, "warnings": 0, "next": []},
            "active_runs": 0,
            "launchers": {"default": "direct", "refused_scopes": [], "direct_resolved": null},
            "update": {"available": false, "latest": null},
            "run_path": {
                "source_requested": "auto",
                "source_used": "inherit",
                "shell": null,
                "dirs": [],
                "captured_at": "2026-10-13T08:00:01Z",
                "duration_ms": 10002,
                "error": {"kind": "timeout"},
                "dropped_entries": 1,
            },
            "run_locale": null,
            "run_index": null,
            "beacons": [],
            "config_dir_mismatch": false,
            "supervisor": null,
            "source": "disk",
        });
        let status: DaemonStatus = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(status.state, DaemonReachability::Stopped);
        assert_eq!(status.lock_holder.as_ref().unwrap().role, LockRole::Cli);
        assert_eq!(
            status.run_path.as_ref().unwrap().error,
            Some(RunPathError {
                kind: RunPathErrorKind::Timeout
            })
        );
        assert_eq!(serde_json::to_value(&status).unwrap(), json);
    }

    #[test]
    fn an_unknown_problem_kind_parses() {
        let problem: Problem = serde_json::from_value(json!({
            "kind": "added_in_v1_9",
            "since": "2026-10-13T08:00:00Z",
            "detail": "ignored",
        }))
        .unwrap();
        assert_eq!(problem.kind, ProblemKind::Unknown);
        assert_eq!(problem.scope, None);
        assert_eq!(ProblemKind::KNOWN.len(), 14);
        let schema = serde_json::to_value(ProblemKind::schema()).unwrap();
        assert_eq!(schema["enum"].as_array().unwrap().len(), 14);
    }

    #[test]
    fn daemon_control_bodies_match_the_spec_shape() {
        fn check<T>(value: T, json: Value)
        where
            T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
        {
            assert_eq!(serde_json::to_value(&value).unwrap(), json);
            assert_eq!(serde_json::from_value::<T>(json).unwrap(), value);
        }
        check(
            StopAccepted {
                boot_id: "b1".into(),
                in_flight_runs: 2,
                deadline_at: at("2026-10-13T08:00:30Z"),
            },
            json!({"boot_id": "b1", "in_flight_runs": 2, "deadline_at": "2026-10-13T08:00:30Z"}),
        );
        check(
            RestartAccepted {
                old_boot_id: "b1".into(),
            },
            json!({"old_boot_id": "b1"}),
        );
        check(
            ReexecRequest {
                exe: "/opt/homebrew/bin/claudeclaw".into(),
            },
            json!({"exe": "/opt/homebrew/bin/claudeclaw"}),
        );
        check(
            ReexecAccepted {
                old_boot_id: "b1".into(),
                target_version: "2.0.1".into(),
            },
            json!({"old_boot_id": "b1", "target_version": "2.0.1"}),
        );
        check(
            UpdateInfo {
                current: "2.0.0".into(),
                latest: Some("2.0.1".into()),
                available: true,
                checked_at: Some(at("2026-10-13T03:00:00Z")),
                release_url: Some(
                    "https://github.com/Winter-wub/claudeclaw/releases/tag/v2.0.1".into(),
                ),
            },
            json!({
                "current": "2.0.0",
                "latest": "2.0.1",
                "available": true,
                "checked_at": "2026-10-13T03:00:00Z",
                "release_url": "https://github.com/Winter-wub/claudeclaw/releases/tag/v2.0.1",
            }),
        );
        check(
            UpdateInfo {
                current: "2.0.0".into(),
                latest: None,
                available: false,
                checked_at: None,
                release_url: None,
            },
            json!({
                "current": "2.0.0",
                "latest": null,
                "available": false,
                "checked_at": null,
                "release_url": null,
            }),
        );
    }

    #[test]
    fn housekeeping_reports_match_the_spec_shape() {
        assert_eq!(
            serde_json::from_value::<HousekeepingRequest>(json!({})).unwrap(),
            HousekeepingRequest { dry_run: false }
        );
        let totals = BTreeMap::from([
            (
                "inbox".to_owned(),
                HousekeepingTotal {
                    count: 2,
                    bytes: 4096,
                },
            ),
            (
                "session_backups".to_owned(),
                HousekeepingTotal {
                    count: 1,
                    bytes: 512,
                },
            ),
        ]);
        let preview = HousekeepingReport {
            would_delete: Some(totals.clone()),
            deleted: None,
            freed_bytes: 4608,
        };
        let json = json!({
            "would_delete": {
                "inbox": {"count": 2, "bytes": 4096},
                "session_backups": {"count": 1, "bytes": 512},
            },
            "freed_bytes": 4608,
        });
        assert_eq!(serde_json::to_value(&preview).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<HousekeepingReport>(json).unwrap(),
            preview
        );

        let done = HousekeepingReport {
            would_delete: None,
            deleted: Some(BTreeMap::new()),
            freed_bytes: 0,
        };
        let json = json!({"deleted": {}, "freed_bytes": 0});
        assert_eq!(serde_json::to_value(&done).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<HousekeepingReport>(json).unwrap(),
            done
        );
    }
}
