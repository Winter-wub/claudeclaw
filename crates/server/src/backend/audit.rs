//! The audit seam (management-api §2.9, #13): where the audit layer appends entries.

use futures_util::future::BoxFuture;

use claudeclaw_api::logs::AuditEntry;

/// A boxed audit-write result.
pub type AuditFuture<'a> = BoxFuture<'a, Result<(), AuditError>>;

/// The audit log (management-api §2.9, #13). The audit layer calls this for audited mutations and
/// never lets a failure fail the request.
pub trait AuditBackend: Send + Sync + 'static {
    fn record(&self, entry: AuditEntry) -> AuditFuture<'_>;
}

/// A failure to write an audit entry. The audit layer logs it and carries on; the call still
/// succeeds.
#[derive(Debug, thiserror::Error)]
#[error("audit log write failed: {0}")]
pub struct AuditError(pub String);
