//! Scopes, Launcher resolution, sessions, Conversations and notifications.
//!
//! - [`scope`]: the typed Scope, its `chat` and API conversions, and the Launcher resolution chain
//!   and picker (architecture §4 `claudeclaw-core`, §6 "Scope keys", §7; #8, #17).
//! - [`events`]: the event bus, a `tokio::sync::broadcast` of `api::DaemonEvent` that feeds SSE
//!   (management-api §1.6, §2.2).
//! - [`notify`]: owner notices, fanned out to each adapter's `Notifier` and to the event bus, with
//!   the invalid-job and unplanned-restart policies (architecture §4, #12, #27).

pub mod events;
pub mod notify;
pub mod scope;
