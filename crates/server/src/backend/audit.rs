//! The audit seam (management-api §2.9, #13): where the audit layer appends entries, and where
//! `audit.list` reads them back.

use futures_util::future::BoxFuture;

use claudeclaw_api::logs::{AuditEntry, AuditPage};

use super::BackendFuture;

/// A boxed audit-write result.
pub type AuditFuture<'a> = BoxFuture<'a, Result<(), AuditError>>;

/// `audit.list`'s filters (management-api §2.9), already validated by the route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditQuery {
    /// The audit log's own opaque cursor (§1.6); a `before` it didn't hand out is
    /// `400 invalid_cursor`.
    pub before: Option<String>,
    /// The resolved `?limit=`: at least 1 (§1.6).
    pub limit: u32,
    /// An exact principal, e.g. `device:<id>`.
    pub principal: Option<String>,
    /// An exact action, the route's Op (e.g. `jobs.update`).
    pub action: Option<String>,
}

/// The audit log (management-api §2.9, #13). The audit layer calls this for audited mutations
/// and never lets a failure fail the request.
pub trait AuditBackend: Send + Sync + 'static {
    fn record(&self, entry: AuditEntry) -> AuditFuture<'_>;

    /// One page of entries, newest first (`audit.list`). A `before` the log didn't hand out is
    /// `400 invalid_cursor`.
    fn list(&self, query: AuditQuery) -> BackendFuture<'_, AuditPage>;
}

/// A failure to write an audit entry. The audit layer logs it and carries on; the call still
/// succeeds.
#[derive(Debug, thiserror::Error)]
#[error("audit log write failed: {0}")]
pub struct AuditError(pub String);
