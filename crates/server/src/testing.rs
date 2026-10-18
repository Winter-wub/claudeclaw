//! In-process fakes and helpers for the server's tests.
//!
//! Every backend seam has a fake whose state is shared through `Arc`, so a test can build the
//! router once and then flip the lifecycle, count calls, or replay events from the handle.

#![cfg(test)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use chrono::{DateTime, Utc};
use tokio::sync::{broadcast, watch};

use claudeclaw_api::DaemonEvent;
use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::events::{LogStreamHello, StreamGap};
use claudeclaw_api::export::{
    ExportManifest, ExportTrimmed, ExportVersions, ExportWindow, LogExport,
};
use claudeclaw_api::jobs::{
    JobCheck, JobCreate, JobDeleted, JobDetail, JobFields, JobLastRun, JobList, JobNotify,
    JobPatch, JobPreview, JobPreviewRequest, JobRaw, JobStatus, JobSummary, JobsAudit, JobsCheck,
};
use claudeclaw_api::logs::{
    AuditEntry, AuditPage, DiskHousekeeping, LogCounts, LogDisk, LogEntry, LogPage, LogSummary,
};
use claudeclaw_api::permission::TokenPermission;
use claudeclaw_api::revision::Revision;
use claudeclaw_api::runs::{
    CostBasis, CostStatus, RunIndexPhase, RunIndexState, RunModelUsage, RunOrigin, RunOutcome,
    RunPage, RunPurpose, RunRecord, RunSession, RunSettle, SettleOutcome,
};
use claudeclaw_api::status::{
    DaemonHealth, DaemonStatus, Health, HealthState, HeartbeatStatus, HousekeepingReport,
    HousekeepingRequest, JobsStatus, LaunchersStatus, ReexecAccepted, ReexecRequest, RemoteStatus,
    RestartAccepted, StatusSource, StatusVersion, StopAccepted, UpdateInfo, UpdateStatus,
};
use claudeclaw_api::supervisor::DaemonReachability;
use claudeclaw_api::turns::{TurnEvent, TurnOutcome, TurnPurpose, TurnRecord, TurnState};
use claudeclaw_api::usage::{
    CostTrust, UsageCoverage, UsageDay, UsageGroup, UsageReport, UsageRow, UsageTotals,
};

use crate::auth::{Principal, TokenRecord, TokenStore, hash_token};
use crate::backend::pairing::{PairingService, TestClock, TestHooks};
use crate::backend::{
    AuditBackend, AuditFuture, AuditQuery, BackendFuture, DaemonBackend, EventsBackend,
    JobAuditQuery, JobCheckQuery, JobListQuery, JobsBackend, LogExportQuery, LogSource, LogStream,
    LogStreamQuery, LogTailQuery, LogsBackend, RunQuery, RunsBackend, SequencedLog, StatusBackend,
    TurnCreate, TurnFeed, TurnHandle, TurnsBackend, UsageBackend, UsageQuery,
};
use crate::pairing::PairingRegistry;
use crate::state::AppState;

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// The sample `Health` the read routes return.
pub fn sample_health(state: HealthState) -> Health {
    Health {
        state,
        boot_id: "boot-1".into(),
        version: "2.0.0".into(),
        api_version: "1".into(),
        import: None,
    }
}

/// A complete `DaemonStatus`, so `status.get` has something real to serve.
pub fn sample_status() -> DaemonStatus {
    DaemonStatus {
        dir: "/tmp/demo".into(),
        state: DaemonReachability::Running,
        health: DaemonHealth::Ok,
        problems: vec![],
        lock_holder: None,
        watchdog_pid: None,
        worker_pid: None,
        version: StatusVersion {
            daemon: "2.0.0".into(),
            cli: None,
        },
        started_at: None,
        boot_id: Some("boot-1".into()),
        socket: "/tmp/demo/.claude/claudeclaw/api.sock".into(),
        remote: RemoteStatus {
            enabled: false,
            listen: vec![],
            fingerprint: None,
            fingerprint_curl: None,
        },
        adapters: vec![],
        heartbeat: HeartbeatStatus {
            enabled: false,
            next_at: None,
        },
        jobs: JobsStatus {
            total: 0,
            invalid: 0,
            warnings: 0,
            next: vec![],
        },
        active_runs: 0,
        launchers: LaunchersStatus {
            default: "direct".into(),
            refused_scopes: vec![],
            direct_resolved: None,
        },
        update: UpdateStatus {
            available: false,
            latest: None,
        },
        run_path: None,
        run_locale: None,
        run_index: None,
        beacons: vec![],
        config_dir_mismatch: false,
        supervisor: None,
        source: StatusSource::Api,
    }
}

pub fn sample_update(available: bool) -> UpdateInfo {
    UpdateInfo {
        current: "2.0.0".into(),
        latest: Some(if available { "2.0.1" } else { "2.0.0" }.into()),
        available,
        checked_at: None,
        release_url: None,
    }
}

/// The `health.get` / `status.get` / `update.get` backend.
#[derive(Clone)]
pub struct FakeStatus {
    state: Arc<Mutex<HealthState>>,
}

impl FakeStatus {
    pub fn ready() -> Self {
        FakeStatus {
            state: Arc::new(Mutex::new(HealthState::Ready)),
        }
    }

    /// Flips the lifecycle the auth layer's gate reads.
    pub fn set_state(&self, state: HealthState) {
        *self.state.lock().unwrap() = state;
    }
}

impl StatusBackend for FakeStatus {
    fn health_state(&self) -> HealthState {
        *self.state.lock().unwrap()
    }

    fn health(&self) -> BackendFuture<'_, Health> {
        let health = sample_health(self.health_state());
        Box::pin(async move { Ok(health) })
    }

    fn status(&self) -> BackendFuture<'_, DaemonStatus> {
        Box::pin(async { Ok(sample_status()) })
    }

    fn update(&self) -> BackendFuture<'_, UpdateInfo> {
        Box::pin(async { Ok(sample_update(false)) })
    }
}

/// The daemon control backend. It counts the calls it saw and can stall to exercise the deadline.
#[derive(Clone, Default)]
pub struct FakeDaemon {
    calls: Arc<Mutex<Vec<&'static str>>>,
    delay: Arc<Mutex<Option<Duration>>>,
}

impl FakeDaemon {
    /// Makes every method sleep first, for the deadline test.
    pub fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = Some(delay);
    }

    /// The ops this backend has served, in order.
    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, op: &'static str) {
        self.calls.lock().unwrap().push(op);
    }

    async fn wait(&self) {
        let delay = *self.delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
    }
}

impl DaemonBackend for FakeDaemon {
    fn stop(&self) -> BackendFuture<'_, StopAccepted> {
        self.record("daemon.stop");
        Box::pin(async move {
            self.wait().await;
            Ok(StopAccepted {
                boot_id: "boot-1".into(),
                in_flight_runs: 0,
                deadline_at: crate::audit::now(),
            })
        })
    }

    fn restart(&self) -> BackendFuture<'_, RestartAccepted> {
        self.record("daemon.restart");
        Box::pin(async move {
            self.wait().await;
            Ok(RestartAccepted {
                old_boot_id: "boot-1".into(),
            })
        })
    }

    fn reexec(&self, _request: ReexecRequest) -> BackendFuture<'_, ReexecAccepted> {
        self.record("daemon.reexec");
        Box::pin(async move {
            self.wait().await;
            Ok(ReexecAccepted {
                old_boot_id: "boot-1".into(),
                target_version: "2.0.1".into(),
            })
        })
    }

    fn check_update(&self) -> BackendFuture<'_, UpdateInfo> {
        self.record("update.check");
        Box::pin(async move {
            self.wait().await;
            Ok(sample_update(true))
        })
    }

    fn housekeeping(&self, request: HousekeepingRequest) -> BackendFuture<'_, HousekeepingReport> {
        self.record("housekeeping.run");
        Box::pin(async move {
            self.wait().await;
            Ok(HousekeepingReport {
                would_delete: request.dry_run.then(std::collections::BTreeMap::new),
                deleted: (!request.dry_run).then(std::collections::BTreeMap::new),
                freed_bytes: 0,
            })
        })
    }
}

/// The `events.stream` bus.
#[derive(Clone)]
pub struct FakeEvents {
    boot_id: String,
    sender: broadcast::Sender<DaemonEvent>,
}

impl FakeEvents {
    pub fn new(boot_id: &str) -> Self {
        FakeEvents {
            boot_id: boot_id.to_owned(),
            sender: broadcast::channel(256).0,
        }
    }

    pub fn publish(&self, event: DaemonEvent) {
        let _ = self.sender.send(event);
    }
}

impl EventsBackend for FakeEvents {
    fn boot_id(&self) -> String {
        self.boot_id.clone()
    }

    fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.sender.subscribe()
    }
}

/// One parsed Daemon-log line, for the logs fakes.
pub fn sample_log_line(seq: u64) -> LogEntry {
    let text = format!(
        r#"{{"seq":{seq},"timestamp":"2026-10-13T08:30:27.512585Z","level":"INFO","target":"claudeclaw_core::conversations","event":"run.start","message":"run started","span":{{"run_id":"run-1","scope":"job:example"}},"fields":{{"attempt":1}}}}"#
    );
    serde_json::from_str(&text).expect("a log line")
}

/// A one-line page with a cursor, so `logs.tail` has something to serve.
pub fn sample_log_page() -> LogPage {
    LogPage {
        lines: vec![sample_log_line(1)],
        next: Some("2026-10-13:128".into()),
        scan_truncated: false,
    }
}

/// The badges `logs.summary` serves.
pub fn sample_log_summary() -> LogSummary {
    LogSummary {
        level: "info".into(),
        since_boot: LogCounts { error: 0, warn: 1 },
        dropped_lines: 0,
        capped_today: false,
        disk: LogDisk {
            daemon_bytes: 1024,
            run_log_files: 2,
            run_log_bytes: 2048,
            audit_bytes: 128,
            inbox_files: 0,
            inbox_bytes: 0,
            inbox_foreign: 0,
            run_index_bytes: 512,
            outbox_bytes: 0,
            archive_bytes: 0,
            import_backup_bytes: 0,
            free_bytes: Some(1_000_000),
            housekeeping: DiskHousekeeping {
                last_at: None,
                next_at: None,
                deleted: BTreeMap::new(),
                freed_bytes: 0,
            },
        },
    }
}

/// A scrubbed Bug-report export, for `logs.export`.
pub fn sample_log_export() -> LogExport {
    LogExport {
        manifest: ExportManifest {
            format: "claudeclaw-export/1".into(),
            created_at: at("2026-10-13T09:00:00Z"),
            versions: ExportVersions {
                claudeclaw: "2.0.0".into(),
                commit: "abcdef0".into(),
                target: "release".into(),
                daemon: Some("2.0.0".into()),
                app: None,
            },
            window: ExportWindow {
                since: at("2026-10-12T09:00:00Z"),
                until: at("2026-10-13T09:00:00Z"),
            },
            lines: 1,
            runs: 1,
            pseudonyms: BTreeMap::new(),
            trimmed: ExportTrimmed {
                trace: 0,
                debug: 0,
                info: 0,
                warn_plus: 0,
                oldest_included: None,
            },
            map_written: false,
        },
        text: "# Bug report\n\nA scrubbed export.\n".into(),
        map: None,
    }
}

/// One Run-index record, for the runs fakes.
pub fn sample_run() -> RunRecord {
    RunRecord {
        run_id: "run-1".into(),
        turn_id: Some("turn-1".into()),
        name: "daily-report".into(),
        scope: Some("job:daily-report".parse().unwrap()),
        origin: RunOrigin::Scheduler,
        launcher: Some("direct".into()),
        job: Some("daily-report".into()),
        agent: None,
        session_id: Some("5e55-1d".into()),
        session: Some(RunSession::Resumed),
        model: Some("claude-sonnet-5-5".into()),
        started_at: at("2026-10-13T08:30:27.512Z"),
        ended_at: Some(at("2026-10-13T08:31:02Z")),
        duration_ms: Some(34_488),
        outcome: Some(RunOutcome::Ok),
        exit_code: Some(0),
        is_error: Some(false),
        api_error_status: None,
        attempt: 1,
        state: None,
        tokens_in: Some(1200),
        tokens_out: Some(300),
        cache_read_tokens: Some(9000),
        cache_write_tokens: Some(400),
        cost_usd: Some(0.0125),
        cost_status: CostStatus::Priced,
        cost_basis: Some(CostBasis::List),
        provider: Some("firstParty".into()),
        api_key_source: Some("ANTHROPIC_API_KEY".into()),
        models: vec![RunModelUsage {
            model: "claude-sonnet-5-5".into(),
            tokens_in: 1200,
            tokens_out: 300,
            cache_read_tokens: 9000,
            cache_write_tokens: 400,
            cost_usd: Some(0.0125),
            cost_basis: CostBasis::List,
        }],
        run_log: Some("logs/daily-report-2026-10-13T08-30-27-512Z.log".into()),
        launcher_resolved: Some("~/.local/bin/claude".into()),
        purpose: Some(RunPurpose::Turn),
        settle: Some(RunSettle {
            stragglers: 0,
            outcome: SettleOutcome::Clean,
            pipe_held: false,
            settle_ms: 3,
        }),
        awake_ms: Some(34_488),
        suspended_ms: Some(0),
        wake_grace: false,
        claude_code_version: Some("2.1.296".into()),
        lingered: false,
    }
}

/// A one-record page with the Run index's state, for `runs.list`.
pub fn sample_run_page() -> RunPage {
    RunPage {
        runs: vec![sample_run()],
        next: Some("2026-10:42".into()),
        index: RunIndexState {
            state: RunIndexPhase::Ready,
            done: 1,
            total: 1,
            since: at("2026-10-13T08:00:00Z"),
        },
    }
}

/// A small report, for `usage.get`.
pub fn sample_usage_report() -> UsageReport {
    UsageReport {
        from: "2026-10-01".parse().unwrap(),
        to: "2026-10-02".parse().unwrap(),
        tz: "UTC".into(),
        currency: "USD".into(),
        totals: UsageTotals {
            runs: 1,
            tokens_in: 1200,
            tokens_out: 300,
            cache_read_tokens: 9000,
            cache_write_tokens: 400,
            cache_hit_pct: Some(88.2),
            cost_usd: Some(0.0125),
            cost_trust: Some(CostTrust::ApiList),
        },
        days: vec![UsageDay {
            date: "2026-10-01".parse().unwrap(),
            runs: 1,
            tokens_in: 1200,
            tokens_out: 300,
            cost_usd: Some(0.0125),
        }],
        rows: vec![UsageRow {
            key: "job:daily-report".into(),
            label: "daily-report".into(),
            kind: UsageGroup::Job,
            launcher: Some("direct".into()),
            runs: 1,
            tokens_in: 1200,
            tokens_out: 300,
            cache_read_tokens: 9000,
            cache_write_tokens: 400,
            cache_hit_pct: Some(88.2),
            cost_usd: Some(0.0125),
            cost_trust: Some(CostTrust::ApiList),
        }],
        coverage: UsageCoverage {
            index_from: Some(at("2026-09-01T09:00:00Z")),
            runs_without_usage: 0,
            runs_no_baseline: 0,
            runs_unknown_price: 0,
            index: RunIndexState {
                state: RunIndexPhase::Ready,
                done: 1,
                total: 1,
                since: at("2026-10-13T08:00:00Z"),
            },
        },
    }
}

/// The `logs.*` backend: it serves the samples above, records what the routes asked for, and
/// publishes live lines for `logs.stream`.
#[derive(Clone)]
pub struct FakeLogs {
    tail_calls: Arc<Mutex<Vec<(LogSource, LogTailQuery)>>>,
    stream_calls: Arc<Mutex<Vec<(LogSource, LogStreamQuery)>>>,
    export_calls: Arc<Mutex<Vec<LogExportQuery>>>,
    page: Arc<Mutex<LogPage>>,
    hello: Arc<Mutex<LogStreamHello>>,
    backfill: Arc<Mutex<Vec<SequencedLog>>>,
    gap: Arc<Mutex<Option<StreamGap>>>,
    sender: broadcast::Sender<SequencedLog>,
    export_error: Arc<Mutex<Option<ApiError>>>,
}

impl Default for FakeLogs {
    fn default() -> Self {
        FakeLogs {
            tail_calls: Arc::new(Mutex::new(Vec::new())),
            stream_calls: Arc::new(Mutex::new(Vec::new())),
            export_calls: Arc::new(Mutex::new(Vec::new())),
            page: Arc::new(Mutex::new(sample_log_page())),
            hello: Arc::new(Mutex::new(LogStreamHello {
                boot_id: "boot-1".into(),
                last_seq: 0,
            })),
            backfill: Arc::new(Mutex::new(Vec::new())),
            gap: Arc::new(Mutex::new(None)),
            sender: broadcast::channel(8).0,
            export_error: Arc::new(Mutex::new(None)),
        }
    }
}

impl FakeLogs {
    /// The `(source, query)` pairs `logs.tail` served.
    pub fn tail_calls(&self) -> Vec<(LogSource, LogTailQuery)> {
        self.tail_calls.lock().unwrap().clone()
    }

    /// The `(source, query)` pairs `logs.stream` served.
    pub fn stream_calls(&self) -> Vec<(LogSource, LogStreamQuery)> {
        self.stream_calls.lock().unwrap().clone()
    }

    /// The queries `logs.export` served.
    pub fn export_calls(&self) -> Vec<LogExportQuery> {
        self.export_calls.lock().unwrap().clone()
    }

    /// What the next `logs.export` answers instead, e.g. `409 export_busy`.
    pub fn fail_export_with(&self, error: ApiError) {
        *self.export_error.lock().unwrap() = Some(error);
    }

    /// The lines the next `logs.stream` replays before the live feed.
    pub fn set_backfill(&self, backfill: Vec<SequencedLog>) {
        *self.backfill.lock().unwrap() = backfill;
    }

    /// The gap the backend reports for a resume point it couldn't line up (`evicted`).
    pub fn set_gap(&self, gap: Option<StreamGap>) {
        *self.gap.lock().unwrap() = gap;
    }

    /// Publishes one live line for every open `logs.stream`.
    pub fn publish(&self, seq: u64) {
        let _ = self.sender.send(SequencedLog {
            seq,
            entry: sample_log_line(seq),
        });
    }
}

impl LogsBackend for FakeLogs {
    fn tail(&self, source: LogSource, query: LogTailQuery) -> BackendFuture<'_, LogPage> {
        self.tail_calls.lock().unwrap().push((source, query));
        let page = self.page.lock().unwrap().clone();
        Box::pin(async move { Ok(page) })
    }

    fn stream(&self, source: LogSource, query: LogStreamQuery) -> BackendFuture<'_, LogStream> {
        self.stream_calls.lock().unwrap().push((source, query));
        let stream = LogStream {
            hello: self.hello.lock().unwrap().clone(),
            backfill: self.backfill.lock().unwrap().clone(),
            receiver: self.sender.subscribe(),
            gap: self.gap.lock().unwrap().clone(),
        };
        Box::pin(async move { Ok(stream) })
    }

    fn summary(&self) -> BackendFuture<'_, LogSummary> {
        Box::pin(async { Ok(sample_log_summary()) })
    }

    fn export(&self, query: LogExportQuery) -> BackendFuture<'_, LogExport> {
        self.export_calls.lock().unwrap().push(query);
        let error = self.export_error.lock().unwrap().clone();
        Box::pin(async move {
            match error {
                Some(error) => Err(error),
                None => Ok(sample_log_export()),
            }
        })
    }
}

/// The runs backend: it serves the samples above and records what the routes asked for. Like the
/// store's Run index, a `before` it didn't hand out is `400 invalid_cursor` — its pages always
/// end (`next: None`), so every `before` is one it didn't hand out.
#[derive(Clone)]
pub struct FakeRuns {
    list_calls: Arc<Mutex<Vec<RunQuery>>>,
    get_calls: Arc<Mutex<Vec<String>>>,
    log_calls: Arc<Mutex<Vec<Option<u32>>>>,
    log_text: Arc<Mutex<String>>,
}

impl Default for FakeRuns {
    fn default() -> Self {
        FakeRuns {
            list_calls: Arc::new(Mutex::new(Vec::new())),
            get_calls: Arc::new(Mutex::new(Vec::new())),
            log_calls: Arc::new(Mutex::new(Vec::new())),
            log_text: Arc::new(Mutex::new(
                "# run daily-report\n\nThe Run log, already scrubbed.\n".into(),
            )),
        }
    }
}

impl FakeRuns {
    /// The queries `runs.list` served.
    pub fn list_calls(&self) -> Vec<RunQuery> {
        self.list_calls.lock().unwrap().clone()
    }

    /// The run ids `runs.get` served.
    pub fn get_calls(&self) -> Vec<String> {
        self.get_calls.lock().unwrap().clone()
    }

    /// The `tail_lines` values `runs.log` served.
    pub fn log_calls(&self) -> Vec<Option<u32>> {
        self.log_calls.lock().unwrap().clone()
    }

    /// The Markdown the next `runs.log` answers with (already scrubbed, by construction).
    pub fn set_log_text(&self, text: &str) {
        *self.log_text.lock().unwrap() = text.to_owned();
    }
}

impl RunsBackend for FakeRuns {
    fn list(&self, query: RunQuery) -> BackendFuture<'_, RunPage> {
        self.list_calls.lock().unwrap().push(query.clone());
        if query.before.is_some() {
            return Box::pin(async {
                Err(ApiError::new(
                    ErrorCode::InvalidCursor,
                    "the before cursor is not one this index handed out",
                ))
            });
        }
        let page = sample_run_page();
        Box::pin(async move { Ok(page) })
    }

    fn get(&self, run_id: &str) -> BackendFuture<'_, RunRecord> {
        self.get_calls.lock().unwrap().push(run_id.to_owned());
        let record = sample_run();
        Box::pin(async move { Ok(record) })
    }

    fn log(&self, _run_id: &str, tail_lines: Option<u32>) -> BackendFuture<'_, String> {
        self.log_calls.lock().unwrap().push(tail_lines);
        let text = self.log_text.lock().unwrap().clone();
        Box::pin(async move { Ok(text) })
    }
}

/// The usage backend: it serves one report and records the queries.
#[derive(Clone, Default)]
pub struct FakeUsage {
    calls: Arc<Mutex<Vec<UsageQuery>>>,
}

impl FakeUsage {
    /// The queries `usage.get` served.
    pub fn calls(&self) -> Vec<UsageQuery> {
        self.calls.lock().unwrap().clone()
    }
}

impl UsageBackend for FakeUsage {
    fn usage(&self, query: UsageQuery) -> BackendFuture<'_, UsageReport> {
        self.calls.lock().unwrap().push(query);
        Box::pin(async { Ok(sample_usage_report()) })
    }
}

/// The job file the jobs fakes serve, so its revision is the one `If-Match` has to send back.
const JOB_FILE: &[u8] =
    b"---\nschedule: \"0 9 * * *\"\nrecurring: true\n---\nSummarise yesterday.\n";

/// The sample job every jobs route serves (§2.4, content-free like `jobs list`).
pub fn sample_job_summary() -> JobSummary {
    JobSummary {
        name: "daily-report".into(),
        agent: None,
        path: ".claude/claudeclaw/jobs/daily-report.md".into(),
        status: JobStatus::Valid,
        schedule: Some("0 9 * * *".into()),
        recurring: Some(true),
        enabled: true,
        notify: JobNotify::Bool(true),
        launcher: None,
        model: None,
        next_at: Some(at("2026-10-15T09:00:00Z")),
        last_run: Some(JobLastRun {
            at: at("2026-10-14T09:00:02Z"),
            outcome: RunOutcome::Ok,
        }),
        in_flight: Some(false),
        retry_at: None,
        revision: Revision::of(JOB_FILE),
    }
}

/// The sample job read fresh, for `jobs.get` and every write's answer.
pub fn sample_job_detail() -> JobDetail {
    JobDetail {
        summary: sample_job_summary(),
        fields: JobFields {
            schedule: Some("0 9 * * *".into()),
            recurring: Some(true),
            ..JobFields::default()
        },
        prompt: "Summarise yesterday.\n".into(),
        raw: std::str::from_utf8(JOB_FILE).unwrap().to_owned(),
        diagnostics: vec![],
        import_findings: vec![],
        next_fires: vec![at("2026-10-15T09:00:00Z"), at("2026-10-16T09:00:00Z")],
    }
}

/// The preview `jobs.preview` answers with, carrying as many fires as were asked for (§2.4).
pub fn sample_job_preview(count: u32) -> JobPreview {
    JobPreview {
        fields: JobFields {
            schedule: Some("0 9 * * *".into()),
            ..JobFields::default()
        },
        prompt: String::new(),
        raw: "---\nschedule: \"0 9 * * *\"\n---\n".into(),
        diagnostics: vec![],
        next_fires: future_fires(count),
        timezone: "Asia/Bangkok".into(),
        notes: vec!["day-of-month and day-of-week match with OR".into()],
    }
}

/// `jobs.check`'s sample: one job with exactly `next` future fires (§2.4).
pub fn sample_jobs_check(next: u32) -> JobsCheck {
    JobsCheck {
        daemon_running: true,
        jobs: vec![JobCheck {
            name: "daily-report".into(),
            path: ".claude/claudeclaw/jobs/daily-report.md".into(),
            status: JobStatus::Valid,
            diagnostics: vec![],
            next_fires: future_fires(next),
            last_run: None,
            catch_up: None,
            import: vec![],
        }],
    }
}

/// `jobs.audit`'s sample window, with nothing wrong in it (§2.4).
pub fn sample_jobs_audit() -> JobsAudit {
    JobsAudit {
        window: claudeclaw_api::jobs::AuditWindow {
            from: at("2026-10-13T10:00:00Z"),
            to: at("2026-10-14T10:00:00Z"),
        },
        jobs: vec![],
        ok: true,
        index: RunIndexState {
            state: RunIndexPhase::Ready,
            done: 0,
            total: 0,
            since: at("2026-10-13T08:00:00Z"),
        },
    }
}

fn future_fires(count: u32) -> Vec<DateTime<Utc>> {
    (0..count)
        .map(|offset| at("2026-10-15T09:00:00Z") + chrono::Duration::days(i64::from(offset)))
        .collect()
}

/// One Turn the fake holds: its record, its history for the replay, and its live feed.
#[derive(Clone)]
struct LiveTurn {
    principal: Option<String>,
    record: watch::Sender<TurnRecord>,
    history: Arc<Mutex<Vec<TurnEvent>>>,
    live: broadcast::Sender<TurnEvent>,
}

/// The turns seam: it registers each Turn, records what the routes asked for, and lets a test
/// publish events and finish the Turn on its own schedule.
#[derive(Clone, Default)]
pub struct FakeTurns {
    create_calls: Arc<Mutex<Vec<TurnCreate>>>,
    get_calls: Arc<Mutex<Vec<String>>>,
    delay: Arc<Mutex<Option<Duration>>>,
    finish_after: Arc<Mutex<Option<Duration>>>,
    create_error: Arc<Mutex<Option<ApiError>>>,
    turns: Arc<Mutex<BTreeMap<String, LiveTurn>>>,
    created: Arc<Mutex<u32>>,
}

impl FakeTurns {
    /// The Turns `turns.create` and `jobs.run` started, in order.
    pub fn create_calls(&self) -> Vec<TurnCreate> {
        self.create_calls.lock().unwrap().clone()
    }

    /// The turn ids `turns.get` and `turns.events` asked for.
    pub fn get_calls(&self) -> Vec<String> {
        self.get_calls.lock().unwrap().clone()
    }

    /// How long `create` itself takes, for the detach test (§1.9).
    pub fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = Some(delay);
    }

    /// How long after `create` the Turn finishes on its own.
    pub fn set_finish_after(&self, delay: Duration) {
        *self.finish_after.lock().unwrap() = Some(delay);
    }

    /// What the next `create` answers instead, e.g. `507 insufficient_storage`.
    pub fn fail_create_with(&self, error: ApiError) {
        *self.create_error.lock().unwrap() = Some(error);
    }

    /// The Turn's record as it stands.
    pub fn record(&self, turn_id: &str) -> Option<TurnRecord> {
        self.turns
            .lock()
            .unwrap()
            .get(turn_id)
            .map(|live| live.record.borrow().clone())
    }

    /// Whether the Turn reached `done` (§2.3).
    pub fn finished(&self, turn_id: &str) -> bool {
        self.record(turn_id)
            .is_some_and(|record| record.state == TurnState::Done)
    }

    /// Publishes one event to the Turn's history and every open feed. An `outcome` finishes the
    /// Turn, so calling it twice changes nothing (§2.3).
    pub fn publish(&self, turn_id: &str, event: TurnEvent) {
        let Some(live) = self.turns.lock().unwrap().get(turn_id).cloned() else {
            return;
        };
        if let TurnEvent::Outcome { record } = &event {
            let _ = live.record.send_replace(record.as_ref().clone());
        }
        live.history.lock().unwrap().push(event.clone());
        let _ = live.live.send(event);
    }

    /// Ends the Turn with an `outcome`, which is what closes `turns.events` (§2.3).
    pub fn finish(&self, turn_id: &str, outcome: TurnOutcome, text: &str) {
        let Some(current) = self.record(turn_id) else {
            return;
        };
        if current.state == TurnState::Done {
            return;
        }
        let record = TurnRecord {
            state: TurnState::Done,
            outcome: Some(outcome),
            text: Some(text.to_owned()),
            finished_at: Some(crate::audit::now()),
            duration_ms: Some(0),
            ..current
        };
        self.publish(
            turn_id,
            TurnEvent::Outcome {
                record: Box::new(record),
            },
        );
    }

    /// Registers a queued Turn and answers with the handle its feed replays from (§2.3).
    fn register(&self, request: &TurnCreate) -> TurnHandle {
        let mut created = self.created.lock().unwrap();
        *created += 1;
        let queue_position = *created;
        let turn_id = format!("turn-{queue_position}");
        self.create_calls.lock().unwrap().push(request.clone());
        let record = TurnRecord {
            turn_id: turn_id.clone(),
            scope: request.scope.clone(),
            origin: "api".into(),
            principal: Some(request.principal.clone()),
            purpose: TurnPurpose::Prompt,
            state: TurnState::Queued,
            outcome: None,
            text: None,
            compact: None,
            session_id: None,
            launcher: None,
            run_ids: vec![],
            created_at: crate::audit::now(),
            finished_at: None,
            duration_ms: None,
        };
        let (live, receiver) = broadcast::channel(64);
        let (record, record_receiver) = watch::channel(record);
        let history = Arc::new(Mutex::new(vec![TurnEvent::Queued {
            position: queue_position,
        }]));
        // Subscribe before the snapshot, so an event is in exactly one of the two (§2.3).
        let replay = history.lock().unwrap().clone();
        self.turns.lock().unwrap().insert(
            turn_id.clone(),
            LiveTurn {
                principal: Some(request.principal.clone()),
                record,
                history,
                live,
            },
        );
        TurnHandle {
            turn_id: turn_id.clone(),
            scope: request.scope.clone(),
            queue_position,
            feed: TurnFeed {
                principal: Some(request.principal.clone()),
                record: record_receiver,
                replay,
                live: receiver,
            },
        }
    }
}

impl TurnsBackend for FakeTurns {
    fn create(&self, request: TurnCreate) -> BackendFuture<'_, TurnHandle> {
        let this = self.clone();
        let delay = *self.delay.lock().unwrap();
        let finish_after = *self.finish_after.lock().unwrap();
        let error = self.create_error.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(error) = error {
                return Err(error);
            }
            let handle = this.register(&request);
            if let Some(after) = finish_after {
                let turn_id = handle.turn_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(after).await;
                    this.finish(&turn_id, TurnOutcome::Ok, "the reply");
                });
            }
            Ok(handle)
        })
    }

    fn get(&self, turn_id: &str) -> BackendFuture<'_, TurnFeed> {
        self.get_calls.lock().unwrap().push(turn_id.to_owned());
        let found = self.turns.lock().unwrap().get(turn_id).cloned();
        Box::pin(async move {
            let Some(live) = found else {
                return Err(ApiError::new(ErrorCode::NotFound, "no such turn"));
            };
            // Subscribe before the snapshot (§2.3): the receiver above only sees what follows.
            let live_receiver = live.live.subscribe();
            let replay = live.history.lock().unwrap().clone();
            Ok(TurnFeed {
                principal: live.principal.clone(),
                record: live.record.subscribe(),
                replay,
                live: live_receiver,
            })
        })
    }
}

/// What one write was given: the job's name and the revision its `If-Match` carried (§1.5).
type JobWrite = (String, Option<Revision>);

/// The jobs seam: it serves the samples above, records what the routes asked for, and refuses a
/// stale revision the way the store does (§1.5).
#[derive(Clone, Default)]
pub struct FakeJobs {
    list_calls: Arc<Mutex<Vec<JobListQuery>>>,
    get_calls: Arc<Mutex<Vec<String>>>,
    create_calls: Arc<Mutex<Vec<JobCreate>>>,
    update_calls: Arc<Mutex<Vec<JobWrite>>>,
    replace_calls: Arc<Mutex<Vec<(String, Revision)>>>,
    delete_calls: Arc<Mutex<Vec<JobWrite>>>,
    run_calls: Arc<Mutex<Vec<(String, String)>>>,
    preview_calls: Arc<Mutex<Vec<JobPreviewRequest>>>,
    check_calls: Arc<Mutex<Vec<JobCheckQuery>>>,
    audit_calls: Arc<Mutex<Vec<JobAuditQuery>>>,
    create_error: Arc<Mutex<Option<ApiError>>>,
    pub turns: FakeTurns,
}

impl FakeJobs {
    /// A jobs fake whose `run` starts Turns on the given turns fake (§2.4), so a test sees the
    /// same Turn bus `turns.create` writes to.
    pub fn new(turns: FakeTurns) -> Self {
        FakeJobs {
            turns,
            ..FakeJobs::default()
        }
    }

    /// The queries `jobs.list` served.
    pub fn list_calls(&self) -> Vec<JobListQuery> {
        self.list_calls.lock().unwrap().clone()
    }

    /// The job names `jobs.get` served.
    pub fn get_calls(&self) -> Vec<String> {
        self.get_calls.lock().unwrap().clone()
    }

    /// The `(job, revision)` pairs `jobs.update` and `jobs.delete` were given.
    pub fn update_calls(&self) -> Vec<(String, Option<Revision>)> {
        self.update_calls.lock().unwrap().clone()
    }

    /// The `(job, revision)` pairs `jobs.replace` was given.
    pub fn replace_calls(&self) -> Vec<(String, Revision)> {
        self.replace_calls.lock().unwrap().clone()
    }

    /// The `(job, revision)` pairs `jobs.delete` was given.
    pub fn delete_calls(&self) -> Vec<(String, Option<Revision>)> {
        self.delete_calls.lock().unwrap().clone()
    }

    /// The `(job, principal)` pairs `jobs.run` started.
    pub fn run_calls(&self) -> Vec<(String, String)> {
        self.run_calls.lock().unwrap().clone()
    }

    /// The requests `jobs.preview` served.
    pub fn preview_calls(&self) -> Vec<JobPreviewRequest> {
        self.preview_calls.lock().unwrap().clone()
    }

    /// The queries `jobs.check` and `jobs.audit` served.
    pub fn check_calls(&self) -> Vec<JobCheckQuery> {
        self.check_calls.lock().unwrap().clone()
    }

    pub fn audit_calls(&self) -> Vec<JobAuditQuery> {
        self.audit_calls.lock().unwrap().clone()
    }

    /// What the next `jobs.create` answers instead, e.g. `422 job_invalid`.
    pub fn fail_create_with(&self, error: ApiError) {
        *self.create_error.lock().unwrap() = Some(error);
    }
}

/// §1.5: a revision that is not the one the file now has is `409 revision_mismatch` with the
/// current one in `details.current`; no `If-Match` at all is a patch over a fresh read. Boxed so
/// the refusal fits the clippy `result_large_err` bound on a plain `fn`.
fn check_revision(expected: Option<Revision>, current: Revision) -> Result<(), Box<ApiError>> {
    match expected {
        None => Ok(()),
        Some(expected) if expected == current => Ok(()),
        Some(_) => Err(Box::new(
            ApiError::new(
                ErrorCode::RevisionMismatch,
                "the file changed on disk since it was read",
            )
            .with_detail("current", current.to_string()),
        )),
    }
}

impl JobsBackend for FakeJobs {
    fn list(&self, query: JobListQuery) -> BackendFuture<'_, JobList> {
        self.list_calls.lock().unwrap().push(query.clone());
        let jobs = match query.agent {
            None => vec![sample_job_summary()],
            Some(_) => vec![],
        };
        Box::pin(async move { Ok(JobList { jobs }) })
    }

    fn get(&self, job: &str) -> BackendFuture<'_, JobDetail> {
        let job = job.to_owned();
        self.get_calls.lock().unwrap().push(job.clone());
        Box::pin(async move {
            if job == "daily-report" {
                Ok(sample_job_detail())
            } else {
                Err(ApiError::new(ErrorCode::NotFound, "no such job"))
            }
        })
    }

    fn create(&self, create: JobCreate) -> BackendFuture<'_, JobDetail> {
        self.create_calls.lock().unwrap().push(create.clone());
        let error = self.create_error.lock().unwrap().clone();
        let taken = create.name == "daily-report";
        Box::pin(async move {
            if let Some(error) = error {
                return Err(error);
            }
            if taken {
                return Err(ApiError::new(
                    ErrorCode::JobExists,
                    "a job with this name already exists",
                ));
            }
            let mut detail = sample_job_detail();
            detail.summary.name = create.name;
            detail.summary.agent = create.agent;
            Ok(detail)
        })
    }

    fn update(
        &self,
        job: &str,
        patch: JobPatch,
        expected: Option<Revision>,
    ) -> BackendFuture<'_, JobDetail> {
        let job = job.to_owned();
        self.update_calls
            .lock()
            .unwrap()
            .push((job.clone(), expected));
        let current = sample_job_summary().revision;
        Box::pin(async move {
            check_revision(expected, current).map_err(|failure| *failure)?;
            let mut detail = sample_job_detail();
            detail.summary.name = job;
            if let Some(prompt) = patch.prompt {
                detail.prompt = prompt;
            }
            Ok(detail)
        })
    }

    fn replace(&self, job: &str, raw: JobRaw, expected: Revision) -> BackendFuture<'_, JobDetail> {
        let job = job.to_owned();
        self.replace_calls
            .lock()
            .unwrap()
            .push((job.clone(), expected));
        let current = sample_job_summary().revision;
        Box::pin(async move {
            check_revision(Some(expected), current).map_err(|failure| *failure)?;
            let mut detail = sample_job_detail();
            detail.summary.name = job;
            detail.raw = raw.raw;
            Ok(detail)
        })
    }

    fn delete(&self, job: &str, expected: Option<Revision>) -> BackendFuture<'_, JobDeleted> {
        self.delete_calls
            .lock()
            .unwrap()
            .push((job.to_owned(), expected));
        let current = sample_job_summary().revision;
        Box::pin(async move {
            check_revision(expected, current).map_err(|failure| *failure)?;
            Ok(JobDeleted {
                deleted: true,
                sessions_removed: 1,
                state_removed: true,
            })
        })
    }

    fn run(&self, job: &str, principal: String) -> BackendFuture<'_, TurnHandle> {
        self.run_calls
            .lock()
            .unwrap()
            .push((job.to_owned(), principal.clone()));
        // The same Turn bus `turns.create` writes to (§2.4), so the Turn it starts reads back.
        self.turns.create(TurnCreate {
            scope: claudeclaw_api::Scope::Job(job.to_owned()),
            text: format!("run {job}"),
            attachments: Vec::new(),
            notify: false,
            principal,
        })
    }

    fn preview(&self, request: JobPreviewRequest) -> BackendFuture<'_, JobPreview> {
        let count = request.count;
        self.preview_calls.lock().unwrap().push(request);
        Box::pin(async move { Ok(sample_job_preview(count)) })
    }

    fn check(&self, query: JobCheckQuery) -> BackendFuture<'_, JobsCheck> {
        self.check_calls.lock().unwrap().push(query.clone());
        let check = match query.job.as_deref() {
            None | Some("daily-report") => sample_jobs_check(query.next),
            Some(_) => JobsCheck {
                daemon_running: true,
                jobs: vec![],
            },
        };
        Box::pin(async move { Ok(check) })
    }

    fn audit(&self, query: JobAuditQuery) -> BackendFuture<'_, JobsAudit> {
        self.audit_calls.lock().unwrap().push(query);
        Box::pin(async { Ok(sample_jobs_audit()) })
    }
}

/// An audit backend that keeps every entry it is handed. Its `audit.list` pages those entries
/// newest first; like the store's audit log, a `before` is refused (`400 invalid_cursor`) because
/// these pages always end.
#[derive(Clone, Default)]
pub struct RecordingAudit {
    entries: Arc<Mutex<Vec<AuditEntry>>>,
    list_calls: Arc<Mutex<Vec<AuditQuery>>>,
}

impl RecordingAudit {
    pub fn entries(&self) -> Vec<AuditEntry> {
        self.entries.lock().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The queries `audit.list` served.
    pub fn list_calls(&self) -> Vec<AuditQuery> {
        self.list_calls.lock().unwrap().clone()
    }
}

impl AuditBackend for RecordingAudit {
    fn record(&self, entry: AuditEntry) -> AuditFuture<'_> {
        self.entries.lock().unwrap().push(entry);
        Box::pin(async { Ok(()) })
    }

    fn list(&self, query: AuditQuery) -> BackendFuture<'_, AuditPage> {
        self.list_calls.lock().unwrap().push(query.clone());
        if query.before.is_some() {
            return Box::pin(async {
                Err(ApiError::new(
                    ErrorCode::InvalidCursor,
                    "the before cursor is not one this log handed out",
                ))
            });
        }
        let entries: Vec<AuditEntry> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|entry| {
                query
                    .principal
                    .as_deref()
                    .is_none_or(|principal| entry.principal == principal)
            })
            .filter(|entry| {
                query
                    .action
                    .as_deref()
                    .is_none_or(|action| entry.action == action)
            })
            .take(query.limit as usize)
            .cloned()
            .collect();
        let page = AuditPage {
            entries,
            next: None,
        };
        Box::pin(async move { Ok(page) })
    }
}

/// An in-memory token store, keyed by the SHA-256 of each token's secret.
#[derive(Clone, Default)]
pub struct FakeTokens {
    records: Arc<Mutex<Vec<TokenRecord>>>,
}

impl FakeTokens {
    /// Adds a token. `revoked` exercises the revoked-token path.
    pub fn add(&self, secret: &str, id: &str, permissions: Vec<TokenPermission>, revoked: bool) {
        self.records.lock().unwrap().push(TokenRecord {
            id: id.to_owned(),
            permissions,
            revoked,
            hash: hash_token(secret),
        });
    }
}

impl TokenStore for FakeTokens {
    fn lookup(&self, hash: &[u8; 32]) -> Option<TokenRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|record| &record.hash == hash)
            .cloned()
    }
}

/// Everything a test needs: the state (for the real listener), the router, and each fake handle.
pub struct Harness {
    pub state: AppState,
    pub audit: RecordingAudit,
    pub daemon: FakeDaemon,
    pub status: FakeStatus,
    pub events: FakeEvents,
    pub tokens: FakeTokens,
    pub logs: FakeLogs,
    pub runs: FakeRuns,
    pub usage: FakeUsage,
    pub turns: FakeTurns,
    pub jobs: FakeJobs,
}

impl Harness {
    pub fn new() -> Self {
        let audit = RecordingAudit::default();
        let daemon = FakeDaemon::default();
        let status = FakeStatus::ready();
        let events = FakeEvents::new("boot-1");
        let tokens = FakeTokens::default();
        let logs = FakeLogs::default();
        let runs = FakeRuns::default();
        let usage = FakeUsage::default();
        let turns = FakeTurns::default();
        // `jobs.run` must reach the same Turn as `turns.create`, so one fake serves both.
        let jobs = FakeJobs::new(turns.clone());
        // A real §2.12 service over an in-memory store, so the mounted routes are exercised
        // end to end.
        let clock = Arc::new(TestClock::at(9));
        let registry = PairingRegistry::with_clock(clock.clone());
        let state = AppState::builder()
            .status(status.clone())
            .daemon(daemon.clone())
            .events(events.clone())
            .audit(audit.clone())
            .tokens(tokens.clone())
            .with_logs(logs.clone())
            .with_runs(runs.clone())
            .with_usage(usage.clone())
            .with_turns(turns.clone())
            .with_jobs(jobs.clone())
            .with_pairing(PairingService::for_test(
                TestHooks::enabled(),
                registry,
                clock,
            ))
            .build();
        Harness {
            state,
            audit,
            daemon,
            status,
            events,
            tokens,
            logs,
            runs,
            usage,
            turns,
            jobs,
        }
    }

    pub fn router(&self) -> Router {
        crate::router::router(self.state.clone())
    }

    pub fn local_router(&self) -> Router {
        crate::router::local_app(self.state.clone())
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// A request stamped with a [`PeerIdentity`], as a listener would.
pub fn request(method: Method, uri: &str, identity: crate::auth::PeerIdentity) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(identity);
    request
}

/// A request from an unpaired remote cert carrying a bearer token.
pub fn bearer_request(method: Method, uri: &str, secret: &str) -> Request<Body> {
    let mut request = request(method, uri, crate::auth::PeerIdentity::UnknownCert);
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {secret}").parse().unwrap(),
    );
    request
}

/// A request with a JSON body.
pub fn json_request<T: serde::Serialize>(
    method: Method,
    uri: &str,
    identity: crate::auth::PeerIdentity,
    body: &T,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    request.extensions_mut().insert(identity);
    request
}

/// An unpaired-remote request with a bearer token and a JSON body, for the §2.3/§2.4 writes.
pub fn bearer_json_request<T: serde::Serialize>(
    method: Method,
    uri: &str,
    secret: &str,
    body: &T,
) -> Request<Body> {
    let mut request = bearer_request(method, uri, secret);
    request
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    *request.body_mut() = Body::from(serde_json::to_vec(body).unwrap());
    request
}

/// Drives one request through the router.
pub async fn call(router: &Router, request: Request<Body>) -> Response {
    use tower::ServiceExt;
    router
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible")
}

/// Reads a response's JSON body, or `null` for an empty one.
pub async fn body_json(response: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }
}

/// Asserts a `2xx` and returns the JSON body.
pub async fn ok_json(response: Response) -> serde_json::Value {
    assert!(
        response.status().is_success(),
        "expected success, got {}",
        response.status()
    );
    body_json(response).await
}

pub const LOCAL: crate::auth::PeerIdentity = crate::auth::PeerIdentity::Local;

pub fn device(id: &str) -> crate::auth::PeerIdentity {
    crate::auth::PeerIdentity::Device { id: id.to_owned() }
}

pub fn unpaired() -> crate::auth::PeerIdentity {
    crate::auth::PeerIdentity::UnknownCert
}

/// A principal's audit name, for assertions.
pub fn audit_name(principal: &Principal) -> String {
    principal.audit_name()
}

#[allow(dead_code)]
pub fn status_code() -> StatusCode {
    StatusCode::OK
}
