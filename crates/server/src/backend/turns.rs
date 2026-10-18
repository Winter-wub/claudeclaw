//! The turns seam (`turns.create`, `turns.get` and `turns.events`, management-api §2.3; #22,
//! #40, #44), and the Turn that `jobs.run` starts through it (§2.4).
//!
//! A Turn is started, never awaited: a route gets back an accepted handle whose feed replays the
//! Turn's events from its start and follows them live until `outcome`, so a client can leave and
//! come back (`turns.get`, `turns.events`) without the Turn ever being cancelled (§1.9).

use std::fmt;

use tokio::sync::{broadcast, watch};

use claudeclaw_api::scope::Scope;
use claudeclaw_api::turns::{TurnEvent, TurnRecord};

use super::BackendFuture;

/// One file sent with a Turn (management-api §2.3), base64-decoded by the route before the
/// backend sees it. The backend writes it to `inbox/api/` and hands it to Claude as a file.
#[derive(Clone, PartialEq, Eq)]
pub struct TurnAttachment {
    pub name: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

impl fmt::Debug for TurnAttachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnAttachment")
            .field("name", &self.name)
            .field("content_type", &self.content_type)
            .field("data_bytes", &self.data.len())
            .finish()
    }
}

/// A Turn to start: `turns.create`'s body once the route has validated it (§2.3), or the Turn
/// behind `jobs.run` (§2.4).
#[derive(Clone, PartialEq)]
pub struct TurnCreate {
    pub scope: Scope,
    pub text: String,
    pub attachments: Vec<TurnAttachment>,
    pub notify: bool,
    /// The API principal that asked for the Turn: `local`, `device:<id>` or `token:<id>` (§1.2).
    /// It becomes `TurnRecord.principal`, and is what `Access::TurnOwner` checks.
    pub principal: String,
}

impl fmt::Debug for TurnCreate {
    /// The text's size only; request content never reaches a log (#23 D6).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnCreate")
            .field("scope", &self.scope)
            .field("text_bytes", &self.text.len())
            .field("attachments", &self.attachments)
            .field("notify", &self.notify)
            .field("principal", &self.principal)
            .finish()
    }
}

/// What `turns.create` and `jobs.run` answer with (§2.3): the accepted Turn's id and place in
/// the queue, plus the feed a request follows to its `TurnRecord` or its `outcome`.
#[derive(Debug)]
pub struct TurnHandle {
    pub turn_id: String,
    pub scope: Scope,
    pub queue_position: u32,
    pub feed: TurnFeed,
}

/// One Turn's feed (§2.3): the principal that owns it, its record live (`queued`, then
/// `running`, then `done`), its events replayed from the Turn's start, and the live ones after
/// that.
///
/// The backend fills `live` before it snapshots `replay`, so an event is in exactly one of the
/// two and the route never sees a duplicate or a hole.
pub struct TurnFeed {
    /// The API principal that created the Turn (`local`, `device:<id>`, `token:<id>`); `None`
    /// when the Turn didn't come through the API (§2.3).
    pub principal: Option<String>,
    /// Follow with `changed()`, or `wait_for` a `done` state for `?wait=` (§1.6).
    pub record: watch::Receiver<TurnRecord>,
    /// The Turn's events so far, oldest first.
    pub replay: Vec<TurnEvent>,
    /// Live events; the stream ends after `outcome` (§2.3).
    pub live: broadcast::Receiver<TurnEvent>,
}

impl fmt::Debug for TurnFeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnFeed")
            .field("principal", &self.principal)
            .field("replayed_events", &self.replay.len())
            .finish_non_exhaustive()
    }
}

/// The Turns routes (management-api §2.3). The composition root wires this to the Worker's Turn
/// bus; `jobs.run` starts the same Turn through [`super::jobs::JobsBackend::run`].
pub trait TurnsBackend: Send + Sync + 'static {
    /// Queues the Turn and starts it. The work is handed off before the response (§1.9), so a
    /// client that goes away leaves the Turn running; the handle is accepted, not finished.
    fn create(&self, request: TurnCreate) -> BackendFuture<'_, TurnHandle>;

    /// One Turn's feed with its owning principal, for `turns.get` and `turns.events` (§1.2,
    /// §2.3). `404 not_found` when this Daemon has no such Turn, including one that has aged out
    /// of the "1 h or the newest 256, whichever is more" window (§2.3).
    fn get(&self, turn_id: &str) -> BackendFuture<'_, TurnFeed>;
}
