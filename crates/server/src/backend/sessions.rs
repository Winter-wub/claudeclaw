//! The sessions seam (management-api §2.5: `sessions.list`, `sessions.get`, `sessions.messages`,
//! `sessions.reset`, `sessions.compact` and `sessions.delete`; #8, #22, #32, #33, #39, #40).
//!
//! A session is keyed by (Scope, Launcher) (#8). `sessions.compact` is a Turn: the backend queues
//! it on the Scope and hands back the same [`TurnHandle`] `turns.create` does, so the route
//! shares the `202` / `200 ?wait=` exchange and progress is the existing `turns.events`.

use claudeclaw_api::scope::Scope;
use claudeclaw_api::sessions::{
    SessionDeleted, SessionDetail, SessionList, SessionReset, SessionResetRequest, SessionStatus,
    TranscriptPage,
};

use super::BackendFuture;
use super::turns::TurnHandle;

/// `sessions.list`'s `?scope=` and `?status=` filters (management-api §2.5), already validated
/// by the route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionListQuery {
    pub scope: Option<Scope>,
    pub status: Option<SessionStatus>,
}

/// `sessions.messages`' paging (management-api §1.6, §2.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptQuery {
    /// The resolved `?limit=`: at least 1.
    pub limit: u32,
    /// The Transcript's own opaque cursor (§1.6); a `before` it didn't hand out is
    /// `400 invalid_cursor`.
    pub before: Option<String>,
}

/// The Sessions routes (management-api §2.5). The composition root wires this to the session
/// index, the Transcript reader and the Worker's Turn bus.
pub trait SessionsBackend: Send + Sync + 'static {
    /// Every (Scope, Launcher) session matching `query` (§2.5).
    fn list(&self, query: SessionListQuery) -> BackendFuture<'_, SessionList>;

    /// One session plus its context meter (#39 D4). `404 not_found` when there is no such
    /// session.
    fn get(&self, session_id: &str) -> BackendFuture<'_, SessionDetail>;

    /// One page of the session's Transcript, newest first (§1.6).
    ///
    /// A Transcript that cannot be read is `404 transcript_not_found` with `details.reason`
    /// (`expired`, `not_located` or `unreadable`) and a hint — never an empty page (§2.5, #39).
    fn messages(
        &self,
        session_id: &str,
        query: TranscriptQuery,
    ) -> BackendFuture<'_, TranscriptPage>;

    /// Clears the Scope's session for `request.launcher` (the current one when absent) and
    /// keeps a backup, mirror included (§2.5, rule 7).
    fn reset(&self, scope: Scope, request: SessionResetRequest) -> BackendFuture<'_, SessionReset>;

    /// Queues a manual compact on the Scope as a Turn with `purpose: compact` (§2.5, #40),
    /// owned by `principal` (`local`, `device:<id>` or `token:<id>`). The work is handed off
    /// before the response (§1.9): the handle is accepted, not finished.
    fn compact(&self, scope: Scope, principal: String) -> BackendFuture<'_, TurnHandle>;

    /// Deletes the Scope's session for `launcher` (the current one when absent). A session that
    /// is `in_use` is `409 session_in_use`: reset it instead (§2.5).
    fn delete(&self, scope: Scope, launcher: Option<String>) -> BackendFuture<'_, SessionDeleted>;
}
