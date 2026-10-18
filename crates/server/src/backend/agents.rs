//! The agents seam (management-api §2.8: `agents.list`, `agents.archived`, `agents.create`,
//! `agents.persona.get`, `agents.persona.put`, `agents.archive`, `agents.restore` and
//! `agents.purge`; #16, #21 D12, #44).
//!
//! Every name and entry reaching this seam is a single, already-validated directory name: the
//! routes refuse anything that could leave `agents/` or `agents/.archive/` (`/`, `\`, `..`, a
//! leading `.`, control characters, an empty name) before calling in, so a backend never sees
//! one. The persona write carries the revision the route read off the request's `If-Match`
//! (§1.5), so a stale write is `409 revision_mismatch` with the current revision in
//! `details.current` instead of a silent lost update. Nothing is deleted in one step: archiving
//! moves the directory, and only an archived entry can be purged.

use claudeclaw_api::agents::{
    AgentArchived, AgentCreate, AgentInfo, AgentList, AgentPersona, AgentPurged, ArchivedAgentList,
};
use claudeclaw_api::revision::Revision;

use super::BackendFuture;

/// The Agents routes (management-api §2.8). The composition root wires this to the agent
/// directories under `agents/`, the Scope Launchers and sessions, and the job scheduler.
pub trait AgentsBackend: Send + Sync + 'static {
    /// Every agent dir under `agents/`, the archive excluded (§2.8).
    fn list(&self) -> BackendFuture<'_, AgentList>;

    /// The entries of `agents/.archive/`, fired threads included (§2.8, #16).
    fn archived(&self) -> BackendFuture<'_, ArchivedAgentList>;

    /// A new agent dir, with its persona when the body carried one. `409 agent_exists` when the
    /// name is taken (§2.8).
    fn create(&self, create: AgentCreate) -> BackendFuture<'_, AgentInfo>;

    /// The raw `agents/<name>/CLAUDE.md` and its revision. `404 not_found` when there is no
    /// such agent.
    fn persona_get(&self, name: &str) -> BackendFuture<'_, AgentPersona>;

    /// The whole persona. `expected` is the required `If-Match` revision; the route has already
    /// refused a call without one (§1.5). `404 not_found` when there is no such agent.
    fn persona_put(
        &self,
        name: &str,
        text: String,
        expected: Revision,
    ) -> BackendFuture<'_, AgentPersona>;

    /// Moves the agent dir, its `jobs/` with it, to `agents/.archive/` (§2.8, #16), so those
    /// jobs stop being scheduled. Sessions are kept. `404 not_found` when there is no such
    /// agent.
    fn archive(&self, name: &str) -> BackendFuture<'_, AgentArchived>;

    /// Moves an archived entry back under its old name; its sessions resume (§2.8).
    /// `409 agent_exists` when the name is taken again, `404 not_found` when there is no such
    /// entry.
    fn restore(&self, entry: &str) -> BackendFuture<'_, AgentInfo>;

    /// Deletes an archived entry for good (§2.8, #16). Only an entry under `agents/.archive/`
    /// can be reached; `404 not_found` otherwise, a live agent included.
    fn purge(&self, entry: &str) -> BackendFuture<'_, AgentPurged>;
}
