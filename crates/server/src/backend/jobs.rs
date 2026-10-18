//! The jobs seam (management-api §2.4: `jobs.list`, `jobs.get`, `jobs.create`, `jobs.update`,
//! `jobs.replace`, `jobs.delete`, `jobs.run`, `jobs.preview`, `jobs.check` and `jobs.audit`;
//! #12, #17 §5, #22, #44, #46, #50).
//!
//! The writes carry the revision the route read off the request's `If-Match` (§1.5), so a stale
//! write is `409 revision_mismatch` with the current revision in `details.current` instead of a
//! silent lost update. Reads never write; `jobs.preview` writes nothing by construction (§2.4).

use std::time::Duration;

use chrono::{DateTime, Utc};

use claudeclaw_api::jobs::{
    JobCreate, JobDeleted, JobDetail, JobList, JobPatch, JobPreview, JobPreviewRequest, JobRaw,
    JobsAudit, JobsCheck,
};
use claudeclaw_api::revision::Revision;

use super::BackendFuture;
use super::turns::TurnHandle;

/// `jobs.list`'s `?agent=` filter (management-api §2.4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobListQuery {
    /// One agent's jobs when `?agent=` named it; every job when it didn't.
    pub agent: Option<String>,
}

/// `jobs.check`'s `?job=` and `?next=` (management-api §2.4, #22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCheckQuery {
    /// The one job `?job=` named, or `None` for every job read from disk.
    pub job: Option<String>,
    /// The resolved `?next=`: how many next fires to list; at least 1.
    pub next: u32,
}

/// `jobs.audit`'s `?since=`, `?until=` and `?late=` (management-api §2.4, #22, #44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobAuditQuery {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// How late a run may start before it counts as `late`; the resolved `?late=`, default the
    /// CLI's 2 m (§2.4).
    pub late: Duration,
}

/// The Jobs routes (management-api §2.4). The composition root wires this to the Change scan's
/// job snapshot, `claudeclaw-jobs`' validation and the job files themselves.
pub trait JobsBackend: Send + Sync + 'static {
    /// The whole list from the Change scan's snapshot (§2.4, #46 D5), filtered by `agent`.
    fn list(&self, query: JobListQuery) -> BackendFuture<'_, JobList>;

    /// One job read fresh from disk (§46 D5). `404 not_found` when there is no such job.
    fn get(&self, job: &str) -> BackendFuture<'_, JobDetail>;

    /// A new job from the editor's form or raw text. `409 job_exists` when the name is taken,
    /// `422 job_invalid` when the file doesn't load (§2.4).
    fn create(&self, create: JobCreate) -> BackendFuture<'_, JobDetail>;

    /// Line edits over a fresh read (§2.4, #12 §2), validated before anything is written.
    /// `expected` is the optional `If-Match` revision (§1.5): a stale one is
    /// `409 revision_mismatch` with `details.current`.
    fn update(
        &self,
        job: &str,
        patch: JobPatch,
        expected: Option<Revision>,
    ) -> BackendFuture<'_, JobDetail>;

    /// The whole file, validated as it is (§2.4). `expected` is the required `If-Match`
    /// revision; the route has already refused a call without one (§1.5).
    fn replace(&self, job: &str, raw: JobRaw, expected: Revision) -> BackendFuture<'_, JobDetail>;

    /// The file and everything keyed to it: the job's sessions and per-job state (§2.4, #12 §6).
    /// `expected` is the optional `If-Match` revision (§1.5).
    fn delete(&self, job: &str, expected: Option<Revision>) -> BackendFuture<'_, JobDeleted>;

    /// Starts the job's Turn (§2.4 `jobs.run`): the same handle `turns.create` returns, owned by
    /// the API principal that asked for it, so a token holding `jobs:run` can read the Turn it
    /// started (§1.3).
    fn run(&self, job: &str, principal: String) -> BackendFuture<'_, TurnHandle>;

    /// The server-side preview (§2.4, #12, #16, #50): croner's OR day semantics in the Daemon's
    /// timezone. Writes nothing, so the route is never audited.
    fn preview(&self, request: JobPreviewRequest) -> BackendFuture<'_, JobPreview>;

    /// Every job read from disk, never written (§2.4, #12 §4), filtered and counted by the
    /// resolved query.
    fn check(&self, query: JobCheckQuery) -> BackendFuture<'_, JobsCheck>;

    /// Each schedule's expected fires against the Run index (§2.4, #22, #44).
    fn audit(&self, query: JobAuditQuery) -> BackendFuture<'_, JobsAudit>;
}
