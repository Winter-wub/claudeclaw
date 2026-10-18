//! The First-start import seam (management-api §2.11: `import.get`, `import.plan` and
//! `import.apply`; #17, #22, #24, #32).
//!
//! `import.plan` and `import.get` write nothing. `import.apply` re-plans against the data as it is
//! now: when the request carries a `plan_hash` that no longer matches, nothing is applied and the
//! call is `409 plan_changed`, so the user sees the new plan instead of a surprise (§2.11).

use claudeclaw_api::import::{
    ImportApplyRequest, ImportOnly, ImportPlan, ImportRecord, ImportResult,
};

use super::BackendFuture;

/// `import.plan`'s `?only=` and `?job=` (management-api §2.11, #22).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportPlanQuery {
    /// The steps `?only=` named, each once, in the order they were sent; empty plans every step.
    pub only: Vec<ImportOnly>,
    /// The jobs `?job=` named, each once; empty plans every job.
    pub jobs: Vec<String>,
}

/// The Import routes (management-api §2.11). The composition root wires this to the import
/// planner and its steps (`daemon/state/import.json`, the report and the backup).
pub trait ImportBackend: Send + Sync + 'static {
    /// `daemon/state/import.json` plus the last plan (§2.11).
    fn get(&self) -> BackendFuture<'_, ImportRecord>;

    /// What an import would do now, with its `plan_hash`. Writes nothing, so the route is never
    /// audited (§2.11).
    fn plan(&self, query: ImportPlanQuery) -> BackendFuture<'_, ImportPlan>;

    /// Runs the selected steps. The route has already refused an unknown step, an empty `only`
    /// or `jobs`, and a blank job name; a `plan_hash` that no longer matches is
    /// `409 plan_changed` (§2.11).
    fn apply(&self, request: ImportApplyRequest) -> BackendFuture<'_, ImportResult>;
}
