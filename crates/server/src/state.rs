//! The state the router is built over: one erased handle per backend seam.
//!
//! [`AppStateBuilder`] defaults every seam to [`MissingBackend`], so the router can be built
//! before the Daemon is wired; the composition root supplies the real implementations.

use std::sync::Arc;

use axum::extract::FromRef;

use crate::auth::TokenStore;
use crate::backend::pairing::PairingBackend;
use crate::backend::{
    AgentsBackend, AuditBackend, DaemonBackend, EventsBackend, ImportBackend, JobsBackend,
    LogsBackend, MissingBackend, RunsBackend, SettingsBackend, StatusBackend, TurnsBackend,
    UsageBackend,
};

/// Everything the router needs, erased behind `Arc<dyn …>`. Cheap to clone: the router clones it
/// into each route's layers.
#[derive(Clone)]
pub struct AppState {
    pub status: Arc<dyn StatusBackend>,
    pub daemon: Arc<dyn DaemonBackend>,
    pub events: Arc<dyn EventsBackend>,
    pub audit: Arc<dyn AuditBackend>,
    pub tokens: Arc<dyn TokenStore>,
    pub pairing: Arc<dyn PairingBackend>,
    pub turns: Arc<dyn TurnsBackend>,
    pub jobs: Arc<dyn JobsBackend>,
    pub agents: Arc<dyn AgentsBackend>,
    pub logs: Arc<dyn LogsBackend>,
    pub runs: Arc<dyn RunsBackend>,
    pub usage: Arc<dyn UsageBackend>,
    pub settings: Arc<dyn SettingsBackend>,
    pub import: Arc<dyn ImportBackend>,
}

impl AppState {
    pub fn builder() -> AppStateBuilder {
        AppStateBuilder::default()
    }
}

/// Builds an [`AppState`], filling every unset seam with [`MissingBackend`].
#[derive(Default)]
pub struct AppStateBuilder {
    status: Option<Arc<dyn StatusBackend>>,
    daemon: Option<Arc<dyn DaemonBackend>>,
    events: Option<Arc<dyn EventsBackend>>,
    audit: Option<Arc<dyn AuditBackend>>,
    tokens: Option<Arc<dyn TokenStore>>,
    pairing: Option<Arc<dyn PairingBackend>>,
    turns: Option<Arc<dyn TurnsBackend>>,
    jobs: Option<Arc<dyn JobsBackend>>,
    agents: Option<Arc<dyn AgentsBackend>>,
    logs: Option<Arc<dyn LogsBackend>>,
    runs: Option<Arc<dyn RunsBackend>>,
    usage: Option<Arc<dyn UsageBackend>>,
    settings: Option<Arc<dyn SettingsBackend>>,
    import: Option<Arc<dyn ImportBackend>>,
}

impl AppStateBuilder {
    pub fn status(mut self, backend: impl StatusBackend) -> Self {
        self.status = Some(Arc::new(backend));
        self
    }

    pub fn daemon(mut self, backend: impl DaemonBackend) -> Self {
        self.daemon = Some(Arc::new(backend));
        self
    }

    pub fn events(mut self, backend: impl EventsBackend) -> Self {
        self.events = Some(Arc::new(backend));
        self
    }

    pub fn audit(mut self, backend: impl AuditBackend) -> Self {
        self.audit = Some(Arc::new(backend));
        self
    }

    pub fn tokens(mut self, backend: impl TokenStore) -> Self {
        self.tokens = Some(Arc::new(backend));
        self
    }

    pub fn with_pairing(mut self, backend: impl PairingBackend) -> Self {
        self.pairing = Some(Arc::new(backend));
        self
    }

    pub fn with_turns(mut self, backend: impl TurnsBackend) -> Self {
        self.turns = Some(Arc::new(backend));
        self
    }

    pub fn with_jobs(mut self, backend: impl JobsBackend) -> Self {
        self.jobs = Some(Arc::new(backend));
        self
    }

    pub fn with_agents(mut self, backend: impl AgentsBackend) -> Self {
        self.agents = Some(Arc::new(backend));
        self
    }

    pub fn with_logs(mut self, backend: impl LogsBackend) -> Self {
        self.logs = Some(Arc::new(backend));
        self
    }

    pub fn with_runs(mut self, backend: impl RunsBackend) -> Self {
        self.runs = Some(Arc::new(backend));
        self
    }

    pub fn with_usage(mut self, backend: impl UsageBackend) -> Self {
        self.usage = Some(Arc::new(backend));
        self
    }

    pub fn with_settings(mut self, backend: impl SettingsBackend) -> Self {
        self.settings = Some(Arc::new(backend));
        self
    }

    pub fn with_import(mut self, backend: impl ImportBackend) -> Self {
        self.import = Some(Arc::new(backend));
        self
    }

    pub fn build(self) -> AppState {
        AppState {
            status: self.status.unwrap_or_else(missing_status),
            daemon: self.daemon.unwrap_or_else(missing_daemon),
            events: self.events.unwrap_or_else(missing_events),
            audit: self.audit.unwrap_or_else(missing_audit),
            tokens: self.tokens.unwrap_or_else(missing_tokens),
            pairing: self.pairing.unwrap_or_else(missing_pairing),
            turns: self.turns.unwrap_or_else(missing_turns),
            jobs: self.jobs.unwrap_or_else(missing_jobs),
            agents: self.agents.unwrap_or_else(missing_agents),
            logs: self.logs.unwrap_or_else(missing_logs),
            runs: self.runs.unwrap_or_else(missing_runs),
            usage: self.usage.unwrap_or_else(missing_usage),
            settings: self.settings.unwrap_or_else(missing_settings),
            import: self.import.unwrap_or_else(missing_import),
        }
    }
}

fn missing_status() -> Arc<dyn StatusBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_turns() -> Arc<dyn TurnsBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_jobs() -> Arc<dyn JobsBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_agents() -> Arc<dyn AgentsBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_daemon() -> Arc<dyn DaemonBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_events() -> Arc<dyn EventsBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_audit() -> Arc<dyn AuditBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_tokens() -> Arc<dyn TokenStore> {
    Arc::new(MissingBackend::new())
}

fn missing_pairing() -> Arc<dyn PairingBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_logs() -> Arc<dyn LogsBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_runs() -> Arc<dyn RunsBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_usage() -> Arc<dyn UsageBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_settings() -> Arc<dyn SettingsBackend> {
    Arc::new(MissingBackend::new())
}

fn missing_import() -> Arc<dyn ImportBackend> {
    Arc::new(MissingBackend::new())
}

impl FromRef<AppState> for Arc<dyn PairingBackend> {
    fn from_ref(state: &AppState) -> Self {
        state.pairing.clone()
    }
}
