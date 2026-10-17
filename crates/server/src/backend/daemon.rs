//! The daemon-control seam (`daemon.*`, `update.check`, `housekeeping.run`) and the event bus
//! (`events.stream`).

use tokio::sync::broadcast;

use claudeclaw_api::DaemonEvent;
use claudeclaw_api::status::{
    HousekeepingReport, HousekeepingRequest, ReexecAccepted, ReexecRequest, RestartAccepted,
    StopAccepted, UpdateInfo,
};

use super::BackendFuture;

/// The Daemon control and maintenance routes (`daemon.*`, `update.check`, `housekeeping.run`).
pub trait DaemonBackend: Send + Sync + 'static {
    fn stop(&self) -> BackendFuture<'_, StopAccepted>;

    fn restart(&self) -> BackendFuture<'_, RestartAccepted>;

    fn reexec(&self, request: ReexecRequest) -> BackendFuture<'_, ReexecAccepted>;

    fn check_update(&self) -> BackendFuture<'_, UpdateInfo>;

    fn housekeeping(&self, request: HousekeepingRequest) -> BackendFuture<'_, HousekeepingReport>;
}

/// The `events.stream` bus (management-api §2.2). The bus has no replay: a receiver sees only
/// events published after it subscribed.
pub trait EventsBackend: Send + Sync + 'static {
    /// Identifies the Worker run; a client that sees a new one re-fetches.
    fn boot_id(&self) -> String;

    fn subscribe(&self) -> broadcast::Receiver<DaemonEvent>;
}
