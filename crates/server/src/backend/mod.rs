//! The seams between the server and the Daemon it fronts (architecture §4, `claudeclaw-server`).
//!
//! Each trait is a narrow async API the composition root wires to `core`/`store`; the server
//! itself depends only on `core` and `api`, never `store` (architecture §3.1). [`MissingBackend`]
//! is the builder's default: it answers reads and mutations with `500 internal`, serves an empty
//! event bus and a deny-all [`TokenStore`], so a half-wired server fails loudly rather than
//! panicking.
//!
//! The traits live one per submodule — [`status`], [`daemon`], [`audit`], [`logs`], [`runs`] and
//! [`usage`] — and are re-exported here, so callers use `backend::StatusBackend` regardless of
//! where it is defined.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use tokio::sync::broadcast;

use claudeclaw_api::DaemonEvent;
use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::export::LogExport;
use claudeclaw_api::logs::{AuditEntry, AuditPage, LogPage, LogSummary};
use claudeclaw_api::runs::{RunPage, RunRecord};
use claudeclaw_api::status::{
    DaemonStatus, Health, HealthState, HousekeepingReport, HousekeepingRequest, ReexecAccepted,
    ReexecRequest, RestartAccepted, StopAccepted, UpdateInfo,
};
use claudeclaw_api::usage::UsageReport;

use crate::auth::{TokenRecord, TokenStore};

pub mod audit;
pub mod daemon;
pub mod logs;
pub mod pairing;
pub mod runs;
pub mod status;
pub mod usage;

pub use audit::{AuditBackend, AuditError, AuditFuture, AuditQuery};
pub use daemon::{DaemonBackend, EventsBackend};
pub use logs::{
    LogCursor, LogExportQuery, LogPosition, LogSource, LogStream, LogStreamQuery, LogTailQuery,
    LogsBackend, SequencedLog,
};
pub use runs::{RunQuery, RunsBackend};
pub use status::StatusBackend;
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
        assert!(backend.get("r1").await.is_err());
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
