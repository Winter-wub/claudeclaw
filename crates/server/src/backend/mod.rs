//! The seams between the server and the Daemon it fronts (architecture §4, `claudeclaw-server`).
//!
//! Each trait is a narrow async API the composition root wires to `core`/`store`; the server
//! itself depends only on `core` and `api`, never `store` (architecture §3.1). [`MissingBackend`]
//! is the builder's default: it answers reads and mutations with `500 internal`, serves an empty
//! event bus and a deny-all [`TokenStore`], so a half-wired server fails loudly rather than
//! panicking.
//!
//! The traits live one per submodule — [`status`], [`daemon`], [`audit`], [`turns`], [`jobs`],
//! [`agents`], [`logs`], [`runs`], [`usage`], [`settings`], [`import`], [`scopes`] and [`sessions`] — and are re-exported here, so callers use
//! `backend::StatusBackend` regardless of where it is defined.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use tokio::sync::broadcast;

use claudeclaw_api::DaemonEvent;
use claudeclaw_api::agents::{
    AgentArchived, AgentCreate, AgentInfo, AgentList, AgentPersona, AgentPurged, ArchivedAgentList,
};
use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::export::LogExport;
use claudeclaw_api::import::{ImportApplyRequest, ImportPlan, ImportRecord, ImportResult};
use claudeclaw_api::jobs::{
    JobCreate, JobDeleted, JobDetail, JobList, JobPatch, JobPreview, JobPreviewRequest, JobRaw,
    JobsAudit, JobsCheck,
};
use claudeclaw_api::logs::{AuditEntry, AuditPage, LogPage, LogSummary};
use claudeclaw_api::revision::Revision;
use claudeclaw_api::runs::{RunPage, RunRecord};
use claudeclaw_api::scope::Scope;
use claudeclaw_api::scopes::{
    LauncherOverview, LauncherSetResult, LauncherTestRequest, LauncherTestResult, LauncherUnset,
    ScopeList,
};
use claudeclaw_api::sessions::{
    SessionDeleted, SessionDetail, SessionList, SessionReset, SessionResetRequest, TranscriptPage,
};
use claudeclaw_api::settings::{SettingsApplied, SettingsPatch, SettingsView};
use claudeclaw_api::status::{
    DaemonStatus, Health, HealthState, HousekeepingReport, HousekeepingRequest, ReexecAccepted,
    ReexecRequest, RestartAccepted, StopAccepted, UpdateInfo,
};
use claudeclaw_api::usage::UsageReport;

use crate::auth::{TokenRecord, TokenStore};

pub mod agents;
pub mod audit;
pub mod daemon;
pub mod import;
pub mod jobs;
pub mod logs;
pub mod pairing;
pub mod runs;
pub mod scopes;
pub mod sessions;
pub mod settings;
pub mod status;
pub mod turns;
pub mod usage;

pub use agents::AgentsBackend;
pub use audit::{AuditBackend, AuditError, AuditFuture, AuditQuery};
pub use daemon::{DaemonBackend, EventsBackend};
pub use import::{ImportBackend, ImportPlanQuery};
pub use jobs::{JobAuditQuery, JobCheckQuery, JobListQuery, JobsBackend};
pub use logs::{
    LogCursor, LogExportQuery, LogPosition, LogSource, LogStream, LogStreamQuery, LogTailQuery,
    LogsBackend, SequencedLog,
};
pub use runs::{RunQuery, RunsBackend};
pub use scopes::{ScopeListQuery, ScopesBackend};
pub use sessions::{SessionListQuery, SessionsBackend, TranscriptQuery};
pub use settings::SettingsBackend;
pub use status::StatusBackend;
pub use turns::{TurnAttachment, TurnCreate, TurnFeed, TurnHandle, TurnsBackend};
pub use usage::{UsageBackend, UsageQuery};

/// A boxed result a backend hands back. Boxing keeps the traits object-safe so [`AppState`] can
/// hold `Arc<dyn …>`.
///
/// [`AppState`]: crate::state::AppState
pub type BackendFuture<'a, T> = BoxFuture<'a, Result<T, ApiError>>;

/// The builder's default for every seam.
#[derive(Debug)]
pub struct MissingBackend {
    events: broadcast::Sender<DaemonEvent>,
}

impl MissingBackend {
    pub fn new() -> Self {
        MissingBackend {
            events: broadcast::channel(1).0,
        }
    }
}

impl Default for MissingBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn missing(what: &str) -> ApiError {
    ApiError::new(
        ErrorCode::Internal,
        format!("no {what} backend is configured"),
    )
}

impl StatusBackend for MissingBackend {
    fn health_state(&self) -> HealthState {
        HealthState::Ready
    }

    fn health(&self) -> BackendFuture<'_, Health> {
        Box::pin(async { Err(missing("status")) })
    }

    fn status(&self) -> BackendFuture<'_, DaemonStatus> {
        Box::pin(async { Err(missing("status")) })
    }

    fn update(&self) -> BackendFuture<'_, UpdateInfo> {
        Box::pin(async { Err(missing("status")) })
    }
}

impl DaemonBackend for MissingBackend {
    fn stop(&self) -> BackendFuture<'_, StopAccepted> {
        Box::pin(async { Err(missing("daemon")) })
    }

    fn restart(&self) -> BackendFuture<'_, RestartAccepted> {
        Box::pin(async { Err(missing("daemon")) })
    }

    fn reexec(&self, _request: ReexecRequest) -> BackendFuture<'_, ReexecAccepted> {
        Box::pin(async { Err(missing("daemon")) })
    }

    fn check_update(&self) -> BackendFuture<'_, UpdateInfo> {
        Box::pin(async { Err(missing("daemon")) })
    }

    fn housekeeping(&self, _request: HousekeepingRequest) -> BackendFuture<'_, HousekeepingReport> {
        Box::pin(async { Err(missing("daemon")) })
    }
}

impl EventsBackend for MissingBackend {
    fn boot_id(&self) -> String {
        String::new()
    }

    fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }
}

impl AuditBackend for MissingBackend {
    fn record(&self, _entry: AuditEntry) -> AuditFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    fn list(&self, _query: AuditQuery) -> BackendFuture<'_, AuditPage> {
        Box::pin(async { Err(missing("audit")) })
    }
}

impl LogsBackend for MissingBackend {
    fn tail(&self, _source: LogSource, _query: LogTailQuery) -> BackendFuture<'_, LogPage> {
        Box::pin(async { Err(missing("logs")) })
    }

    fn stream(&self, _source: LogSource, _query: LogStreamQuery) -> BackendFuture<'_, LogStream> {
        Box::pin(async { Err(missing("logs")) })
    }

    fn summary(&self) -> BackendFuture<'_, LogSummary> {
        Box::pin(async { Err(missing("logs")) })
    }

    fn export(&self, _query: LogExportQuery) -> BackendFuture<'_, LogExport> {
        Box::pin(async { Err(missing("logs")) })
    }
}

impl RunsBackend for MissingBackend {
    fn list(&self, _query: RunQuery) -> BackendFuture<'_, RunPage> {
        Box::pin(async { Err(missing("runs")) })
    }

    fn get(&self, _run_id: &str) -> BackendFuture<'_, RunRecord> {
        Box::pin(async { Err(missing("runs")) })
    }

    fn log(&self, _run_id: &str, _tail_lines: Option<u32>) -> BackendFuture<'_, String> {
        Box::pin(async { Err(missing("runs")) })
    }
}

impl UsageBackend for MissingBackend {
    fn usage(&self, _query: UsageQuery) -> BackendFuture<'_, UsageReport> {
        Box::pin(async { Err(missing("usage")) })
    }
}

impl TurnsBackend for MissingBackend {
    fn create(&self, _request: TurnCreate) -> BackendFuture<'_, TurnHandle> {
        Box::pin(async { Err(missing("turns")) })
    }

    fn get(&self, _turn_id: &str) -> BackendFuture<'_, TurnFeed> {
        Box::pin(async { Err(missing("turns")) })
    }
}

impl JobsBackend for MissingBackend {
    fn list(&self, _query: JobListQuery) -> BackendFuture<'_, JobList> {
        Box::pin(async { Err(missing("jobs")) })
    }

    fn get(&self, _job: &str) -> BackendFuture<'_, JobDetail> {
        Box::pin(async { Err(missing("jobs")) })
    }

    fn create(&self, _create: JobCreate) -> BackendFuture<'_, JobDetail> {
        Box::pin(async { Err(missing("jobs")) })
    }

    fn update(
        &self,
        _job: &str,
        _patch: JobPatch,
        _expected: Option<Revision>,
    ) -> BackendFuture<'_, JobDetail> {
        Box::pin(async { Err(missing("jobs")) })
    }

    fn replace(
        &self,
        _job: &str,
        _raw: JobRaw,
        _expected: Revision,
    ) -> BackendFuture<'_, JobDetail> {
        Box::pin(async { Err(missing("jobs")) })
    }

    fn delete(&self, _job: &str, _expected: Option<Revision>) -> BackendFuture<'_, JobDeleted> {
        Box::pin(async { Err(missing("jobs")) })
    }

    fn run(&self, _job: &str, _principal: String) -> BackendFuture<'_, TurnHandle> {
        Box::pin(async { Err(missing("jobs")) })
    }

    fn preview(&self, _request: JobPreviewRequest) -> BackendFuture<'_, JobPreview> {
        Box::pin(async { Err(missing("jobs")) })
    }

    fn check(&self, _query: JobCheckQuery) -> BackendFuture<'_, JobsCheck> {
        Box::pin(async { Err(missing("jobs")) })
    }

    fn audit(&self, _query: JobAuditQuery) -> BackendFuture<'_, JobsAudit> {
        Box::pin(async { Err(missing("jobs")) })
    }
}

impl AgentsBackend for MissingBackend {
    fn list(&self) -> BackendFuture<'_, AgentList> {
        Box::pin(async { Err(missing("agents")) })
    }

    fn archived(&self) -> BackendFuture<'_, ArchivedAgentList> {
        Box::pin(async { Err(missing("agents")) })
    }

    fn create(&self, _create: AgentCreate) -> BackendFuture<'_, AgentInfo> {
        Box::pin(async { Err(missing("agents")) })
    }

    fn persona_get(&self, _name: &str) -> BackendFuture<'_, AgentPersona> {
        Box::pin(async { Err(missing("agents")) })
    }

    fn persona_put(
        &self,
        _name: &str,
        _text: String,
        _expected: Revision,
    ) -> BackendFuture<'_, AgentPersona> {
        Box::pin(async { Err(missing("agents")) })
    }

    fn archive(&self, _name: &str) -> BackendFuture<'_, AgentArchived> {
        Box::pin(async { Err(missing("agents")) })
    }

    fn restore(&self, _entry: &str) -> BackendFuture<'_, AgentInfo> {
        Box::pin(async { Err(missing("agents")) })
    }

    fn purge(&self, _entry: &str) -> BackendFuture<'_, AgentPurged> {
        Box::pin(async { Err(missing("agents")) })
    }
}

impl ScopesBackend for MissingBackend {
    fn list(&self, _query: ScopeListQuery) -> BackendFuture<'_, ScopeList> {
        Box::pin(async { Err(missing("scopes")) })
    }

    fn launchers(&self) -> BackendFuture<'_, LauncherOverview> {
        Box::pin(async { Err(missing("scopes")) })
    }

    fn set_launcher(
        &self,
        _scope: Scope,
        _launcher: String,
    ) -> BackendFuture<'_, LauncherSetResult> {
        Box::pin(async { Err(missing("scopes")) })
    }

    fn unset_launcher(&self, _scope: Scope) -> BackendFuture<'_, LauncherUnset> {
        Box::pin(async { Err(missing("scopes")) })
    }

    fn test_launcher(
        &self,
        _request: LauncherTestRequest,
    ) -> BackendFuture<'_, LauncherTestResult> {
        Box::pin(async { Err(missing("scopes")) })
    }
}

impl SessionsBackend for MissingBackend {
    fn list(&self, _query: SessionListQuery) -> BackendFuture<'_, SessionList> {
        Box::pin(async { Err(missing("sessions")) })
    }

    fn get(&self, _session_id: &str) -> BackendFuture<'_, SessionDetail> {
        Box::pin(async { Err(missing("sessions")) })
    }

    fn messages(
        &self,
        _session_id: &str,
        _query: TranscriptQuery,
    ) -> BackendFuture<'_, TranscriptPage> {
        Box::pin(async { Err(missing("sessions")) })
    }

    fn reset(
        &self,
        _scope: Scope,
        _request: SessionResetRequest,
    ) -> BackendFuture<'_, SessionReset> {
        Box::pin(async { Err(missing("sessions")) })
    }

    fn compact(&self, _scope: Scope, _principal: String) -> BackendFuture<'_, TurnHandle> {
        Box::pin(async { Err(missing("sessions")) })
    }

    fn delete(
        &self,
        _scope: Scope,
        _launcher: Option<String>,
    ) -> BackendFuture<'_, SessionDeleted> {
        Box::pin(async { Err(missing("sessions")) })
    }
}

impl SettingsBackend for MissingBackend {
    fn get(&self) -> BackendFuture<'_, SettingsView> {
        Box::pin(async { Err(missing("settings")) })
    }

    fn update(
        &self,
        _patch: SettingsPatch,
        _expected: Option<Revision>,
    ) -> BackendFuture<'_, SettingsApplied> {
        Box::pin(async { Err(missing("settings")) })
    }
}

impl ImportBackend for MissingBackend {
    fn get(&self) -> BackendFuture<'_, ImportRecord> {
        Box::pin(async { Err(missing("import")) })
    }

    fn plan(&self, _query: ImportPlanQuery) -> BackendFuture<'_, ImportPlan> {
        Box::pin(async { Err(missing("import")) })
    }

    fn apply(&self, _request: ImportApplyRequest) -> BackendFuture<'_, ImportResult> {
        Box::pin(async { Err(missing("import")) })
    }
}

impl TokenStore for MissingBackend {
    fn lookup(&self, _hash: &[u8; 32]) -> Option<TokenRecord> {
        None
    }
}

/// Every seam, erased, as a composition root would pass them to [`crate::state::AppState`].
pub type Boxed = (
    Arc<dyn StatusBackend>,
    Arc<dyn DaemonBackend>,
    Arc<dyn EventsBackend>,
    Arc<dyn AuditBackend>,
    Arc<dyn TokenStore>,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_backend_fails_loudly_and_serves_an_empty_bus() {
        let backend = MissingBackend::new();
        assert_eq!(backend.health_state(), HealthState::Ready);
        assert!(backend.health().await.is_err());
        assert!(backend.stop().await.is_err());
        assert_eq!(backend.boot_id(), "");
        // A receiver that never sees an event, and whose `recv` stays pending, not closed.
        let receiver = backend.subscribe();
        assert_eq!(receiver.len(), 0);
    }

    #[tokio::test]
    async fn the_unwired_turns_and_jobs_reads_and_writes_fail_loudly() {
        let backend = MissingBackend::new();
        assert!(
            TurnsBackend::create(
                &backend,
                TurnCreate {
                    scope: claudeclaw_api::Scope::Global,
                    text: "hi".into(),
                    attachments: Vec::new(),
                    notify: false,
                    principal: "local".into(),
                }
            )
            .await
            .is_err()
        );
        assert!(TurnsBackend::get(&backend, "turn-1").await.is_err());
        assert!(
            JobsBackend::list(&backend, JobListQuery::default())
                .await
                .is_err()
        );
        assert!(JobsBackend::get(&backend, "daily-report").await.is_err());
        assert!(backend.run("daily-report", "local".into()).await.is_err());
        assert!(
            backend
                .preview(JobPreviewRequest {
                    source: claudeclaw_api::jobs::PreviewSource::Schedule {
                        schedule: "@daily".into()
                    },
                    count: 5,
                })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_unwired_agents_calls_fail_loudly() {
        let backend = MissingBackend::new();
        assert!(AgentsBackend::list(&backend).await.is_err());
        assert!(backend.archived().await.is_err());
        assert!(
            AgentsBackend::create(
                &backend,
                AgentCreate {
                    name: "scribe".into(),
                    persona: None,
                }
            )
            .await
            .is_err()
        );
        assert!(backend.persona_get("scribe").await.is_err());
        assert!(
            backend
                .persona_put("scribe", String::new(), Revision::of(b""))
                .await
                .is_err()
        );
        assert!(backend.archive("scribe").await.is_err());
        assert!(backend.restore("scribe-20261014T090000Z").await.is_err());
        assert!(backend.purge("scribe-20261014T090000Z").await.is_err());
    }

    #[tokio::test]
    async fn the_unwired_scopes_and_sessions_reads_and_writes_fail_loudly() {
        let backend = MissingBackend::new();
        assert!(
            ScopesBackend::list(&backend, ScopeListQuery::default())
                .await
                .is_err()
        );
        assert!(backend.launchers().await.is_err());
        assert!(
            backend
                .set_launcher(Scope::Global, "direct".into())
                .await
                .is_err()
        );
        assert!(
            backend
                .unset_launcher(Scope::Discord("1".into()))
                .await
                .is_err()
        );
        assert!(
            backend
                .test_launcher(LauncherTestRequest::default())
                .await
                .is_err()
        );
        assert!(
            SessionsBackend::list(&backend, SessionListQuery::default())
                .await
                .is_err()
        );
        assert!(SessionsBackend::get(&backend, "s-1").await.is_err());
        assert!(
            backend
                .messages(
                    "s-1",
                    TranscriptQuery {
                        limit: 10,
                        before: None
                    }
                )
                .await
                .is_err()
        );
        assert!(
            SessionsBackend::reset(&backend, Scope::Global, SessionResetRequest::default())
                .await
                .is_err()
        );
        assert!(
            backend
                .compact(Scope::Global, "local".into())
                .await
                .is_err()
        );
        assert!(
            SessionsBackend::delete(&backend, Scope::Global, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_unwired_settings_and_import_calls_fail_loudly() {
        let backend = MissingBackend::new();
        assert!(SettingsBackend::get(&backend).await.is_err());
        assert!(
            SettingsBackend::update(&backend, SettingsPatch::default(), None)
                .await
                .is_err()
        );
        assert!(ImportBackend::get(&backend).await.is_err());
        assert!(backend.plan(ImportPlanQuery::default()).await.is_err());
        assert!(backend.apply(ImportApplyRequest::default()).await.is_err());
    }

    #[tokio::test]
    async fn the_unwired_logs_runs_usage_and_audit_reads_fail_loudly() {
        let backend = MissingBackend::new();
        let tail = backend
            .tail(LogSource::Daemon, LogTailQuery::default())
            .await;
        assert!(tail.is_err());
        assert!(backend.summary().await.is_err());
        assert!(backend.export(LogExportQuery::default()).await.is_err());
        assert!(
            RunsBackend::list(&backend, RunQuery::default())
                .await
                .is_err()
        );
        assert!(RunsBackend::get(&backend, "r1").await.is_err());
        assert!(backend.log("r1", None).await.is_err());
        assert!(
            backend
                .usage(UsageQuery {
                    from: "2026-10-01".parse().unwrap(),
                    to: "2026-10-02".parse().unwrap(),
                    tz: "UTC".into(),
                    group_by: claudeclaw_api::usage::UsageGroup::Scope,
                })
                .await
                .is_err()
        );
        assert!(
            AuditBackend::list(&backend, AuditQuery::default())
                .await
                .is_err()
        );
        // A write is still best-effort Ok: the entry is gone, the call is not.
        assert!(
            backend
                .record(AuditEntry {
                    id: "a".into(),
                    at: "2026-10-13T08:00:00Z".parse().unwrap(),
                    principal: "local".into(),
                    action: "jobs.update".into(),
                    target: None,
                    outcome: claudeclaw_api::logs::AuditOutcome::Ok,
                    code: None,
                    count: None,
                })
                .await
                .is_ok()
        );
    }
}
