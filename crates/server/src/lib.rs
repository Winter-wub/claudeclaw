//! Management API server: auth, pairing, audit and SSE.
//!
//! The server fronts the Worker's Management API (management-api §1–2). It owns no state of its
//! own: every question about the Worker, the event bus, the audit log and the stored tokens goes
//! through a narrow backend seam ([`backend`]), which the composition root wires to `core` and
//! `store`. That keeps this crate's dependency graph to `core` and `api` only (architecture §3.1).
//!
//! One [`Router`](axum::Router) serves every listener. A listener stamps a [`PeerIdentity`] onto
//! each request, and the per-route layers — auth, audit, deadline — turn that into a principal,
//! a log record and a server-side deadline (management-api §1.2–1.4, §1.9).

pub mod audit;
pub mod auth;
pub mod backend;
pub mod deadline;
pub mod devices;
pub mod error;
pub mod listener;
pub mod pairing;
pub mod random;
pub mod router;
pub mod routes;
pub mod state;
pub mod tls;
pub mod tokens;

#[cfg(test)]
pub mod testing;

#[cfg(test)]
mod testing_settings_import;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_settings_import;

pub use audit::REQUEST_ID;
pub use auth::{PeerIdentity, Principal, TokenRecord, TokenStore, hash_token};
pub use backend::{
    AuditBackend, AuditError, AuditFuture, BackendFuture, DaemonBackend, EventsBackend,
    MissingBackend, StatusBackend,
};
pub use error::ApiFailure;
pub use listener::bind_local;
pub use router::{local_app, router, with_peer_identity};
pub use state::{AppState, AppStateBuilder};
