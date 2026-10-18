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
use tokio::sync::broadcast;

use claudeclaw_api::DaemonEvent;
use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::events::{LogStreamHello, StreamGap};
use claudeclaw_api::export::{
    ExportManifest, ExportTrimmed, ExportVersions, ExportWindow, LogExport,
};
use claudeclaw_api::logs::{
    AuditEntry, AuditPage, DiskHousekeeping, LogCounts, LogDisk, LogEntry, LogPage, LogSummary,
};
use claudeclaw_api::permission::TokenPermission;
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
use claudeclaw_api::usage::{
    CostTrust, UsageCoverage, UsageDay, UsageGroup, UsageReport, UsageRow, UsageTotals,
};

use crate::auth::{Principal, TokenRecord, TokenStore, hash_token};
use crate::backend::pairing::{PairingService, TestClock, TestHooks};
use crate::backend::{
    AuditBackend, AuditFuture, AuditQuery, BackendFuture, DaemonBackend, EventsBackend,
    LogExportQuery, LogSource, LogStream, LogStreamQuery, LogTailQuery, LogsBackend, RunQuery,
    RunsBackend, SequencedLog, StatusBackend, UsageBackend, UsageQuery,
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
