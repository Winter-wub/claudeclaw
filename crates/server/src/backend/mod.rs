//! The seams between the server and the Daemon it fronts (architecture §4, `claudeclaw-server`).
//!
//! Each trait is a narrow async API the composition root wires to `core`/`store`; the server
//! itself depends only on `core` and `api`, never `store` (architecture §3.1). [`MissingBackend`]
//! is the builder's default: it answers reads and mutations with `500 internal`, serves an empty
//! event bus and a deny-all [`TokenStore`], so a half-wired server fails loudly rather than
//! panicking.
//!
//! The traits live one per submodule — [`status`], [`daemon`] and [`audit`] — and are re-exported
//! here, so callers use `backend::StatusBackend` regardless of where it is defined.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use tokio::sync::broadcast;

use claudeclaw_api::DaemonEvent;
use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::status::{
    DaemonStatus, Health, HealthState, HousekeepingReport, HousekeepingRequest, ReexecAccepted,
    ReexecRequest, RestartAccepted, StopAccepted, UpdateInfo,
};

use crate::auth::{TokenRecord, TokenStore};

pub mod audit;
pub mod daemon;
pub mod pairing;
pub mod status;

pub use audit::{AuditBackend, AuditError, AuditFuture};
pub use daemon::{DaemonBackend, EventsBackend};
pub use status::StatusBackend;

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
    fn record(&self, _entry: claudeclaw_api::logs::AuditEntry) -> AuditFuture<'_> {
        Box::pin(async { Ok(()) })
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
}
