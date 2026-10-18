//! The runs seam (`runs.list`, `runs.get` and the Run log body `runs.log`, management-api §2.9;
//! #23 D4, #44).

use claudeclaw_api::runs::{RunOrigin, RunOutcome, RunPage, RunRecord};
use claudeclaw_api::scope::Scope;

use super::BackendFuture;

/// `runs.list`'s filters (management-api §2.9), already validated by the route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunQuery {
    pub job: Option<String>,
    pub scope: Option<Scope>,
    pub origin: Option<RunOrigin>,
    pub outcome: Option<RunOutcome>,
    /// The Run index's own opaque cursor (§1.6); a `before` it didn't hand out is
    /// `400 invalid_cursor`.
    pub before: Option<String>,
    /// The resolved `?limit=`: at least 1 (§1.6).
    pub limit: u32,
}

/// The Run-index routes (management-api §2.9). The composition root wires this to `store`'s
/// Run index and Run logs.
pub trait RunsBackend: Send + Sync + 'static {
    /// One page: running runs first, then the index newest first (§2.9). Every page carries the
    /// Run index Back-fill's progress, so a partial answer is labelled (#44 D2).
    fn list(&self, query: RunQuery) -> BackendFuture<'_, RunPage>;

    /// One record, or `404 not_found`.
    fn get(&self, run_id: &str) -> BackendFuture<'_, RunRecord>;

    /// The Run log as Markdown (`text/markdown`), with known secrets already scrubbed — the
    /// backend's job, so the handler never sees them (§2.9). `tail_lines` keeps the last n
    /// lines. `404 not_found` when the run wrote no log.
    fn log(&self, run_id: &str, tail_lines: Option<u32>) -> BackendFuture<'_, String>;
}
