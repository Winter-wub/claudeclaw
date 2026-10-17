//! The status seam: what the Worker knows about its own life (`health.get`, `status.get`,
//! `update.get`).

use claudeclaw_api::status::{DaemonStatus, Health, HealthState, UpdateInfo};

use super::BackendFuture;

/// Reads about the Worker's life and status (`health.get`, `status.get`, `update.get`).
pub trait StatusBackend: Send + Sync + 'static {
    /// Synchronous and cheap: the auth layer's lifecycle gate consults this on every mutation
    /// (management-api §1.8).
    fn health_state(&self) -> HealthState;

    fn health(&self) -> BackendFuture<'_, Health>;

    fn status(&self) -> BackendFuture<'_, DaemonStatus>;

    fn update(&self) -> BackendFuture<'_, UpdateInfo>;
}
