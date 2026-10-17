//! The audit layer (management-api §2.9, #13, architecture §4).
//!
//! One tower layer wraps every route. It logs `api.request` for every call that reaches it, and
//! for an audited mutation it appends one [`AuditEntry`]. It never logs a body, a secret or a
//! value, and a failed audit write never fails the call: the entry is best-effort, the response
//! already existed.
//!
//! A handler can attach [`NotAudited`] to its response to opt out for one call
//! (`housekeeping.run` with `dry_run`), and [`AuditTarget`] to name the entry's `target`.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, TimeZone, Utc};

use claudeclaw_api::error::ErrorCode;
use claudeclaw_api::log_event::daemon;
use claudeclaw_api::logs::{AuditEntry, AuditOutcome};
use claudeclaw_api::routes::{Audit, Route};

use crate::auth::Principal;
use crate::backend::AuditBackend;

/// Header carrying the id this layer made up for the call, echoed for log correlation.
pub const REQUEST_ID: &str = "x-request-id";

/// A response marker: do not audit this call (`housekeeping.run` with `dry_run`, §2.1).
#[derive(Debug, Clone, Copy)]
pub struct NotAudited;

/// A response marker: the audit entry's `target`, e.g. a Housekeeping kind.
#[derive(Debug, Clone)]
pub struct AuditTarget(pub Option<String>);

/// The audit layer's per-route state.
#[derive(Clone)]
pub struct AuditCtx {
    audit: Arc<dyn AuditBackend>,
    route: Route,
}

impl AuditCtx {
    pub fn new(audit: Arc<dyn AuditBackend>, route: Route) -> Self {
        AuditCtx { audit, route }
    }
}

/// Observes the response, logs the request, and records the audit entry for a mutation.
pub async fn observe(State(ctx): State<AuditCtx>, request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let principal = request.extensions().get::<Principal>().cloned();

    let mut response = next.run(request).await;
    let duration_ms = started.elapsed().as_millis() as u64;
    let status = response.status().as_u16();

    let request_id = new_request_id();
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID, value);
    }

    tracing::debug!(
        event = daemon::API_REQUEST,
        method = %method,
        route = ctx.route.path,
        status = status,
        duration_ms = duration_ms,
        "management api request",
    );

    let Some(principal) = principal else {
        return response;
    };
    if !should_audit(&ctx.route, &response) {
        return response;
    }
    let entry = entry_for(&ctx, &response, &principal);
    if let Err(err) = ctx.audit.record(entry).await {
        tracing::warn!(
            event = daemon::API_REQUEST,
            err = %err,
            "audit write failed",
        );
    }
    response
}

fn should_audit(route: &Route, response: &Response) -> bool {
    match route.audit {
        Audit::Never => false,
        Audit::Always => true,
        Audit::UnlessDryRun => response.extensions().get::<NotAudited>().is_none(),
    }
}

fn entry_for(ctx: &AuditCtx, response: &Response, principal: &Principal) -> AuditEntry {
    let code = response.extensions().get::<ErrorCode>().copied();
    let ok = response.status().is_success();
    let target = response
        .extensions()
        .get::<AuditTarget>()
        .and_then(|target| target.0.clone());
    AuditEntry {
        id: new_uuid_v7(),
        at: now(),
        principal: principal.audit_name(),
        action: ctx.route.op.to_owned(),
        target,
        outcome: if ok {
            AuditOutcome::Ok
        } else {
            AuditOutcome::Error
        },
        code: if ok { None } else { code },
        count: None,
    }
}

/// The current UTC time. `chrono`'s `clock` feature is off (architecture §10), so this reads the
/// system clock itself rather than calling `Utc::now`.
pub fn now() -> DateTime<Utc> {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Utc.timestamp_opt(since_epoch.as_secs() as i64, since_epoch.subsec_nanos())
        .single()
        .unwrap_or_default()
}

/// A per-call correlation id: 16 hex characters of OS randomness.
pub fn new_request_id() -> String {
    let mut bytes = [0u8; 8];
    fill_random(&mut bytes);
    hex(&bytes)
}

/// A UUID v7 string for an audit entry's `id` (§1.1). The first 48 bits are the millisecond
/// timestamp, so entries sort by time; the rest is random.
fn new_uuid_v7() -> String {
    let millis = now().timestamp_millis() as u64;
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..8]);
    fill_random(&mut bytes[6..]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70; // version 7
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    let hex = hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32],
    )
}

/// Fills `bytes` from the OS. If that ever fails, falls back to the clock so ids stay unique
/// enough to be useful rather than panicking a request.
fn fill_random(bytes: &mut [u8]) {
    if getrandom::fill(bytes).is_ok() {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = (nanos >> ((index % 16) * 8)) as u8 ^ index as u8;
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_unique_hex() {
        let first = new_request_id();
        let second = new_request_id();
        assert_eq!(first.len(), 16);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn audit_ids_are_version_seven_uuids() {
        let id = new_uuid_v7();
        assert_eq!(id.len(), 36);
        let groups: Vec<&str> = id.split('-').collect();
        assert_eq!(
            groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert_eq!(&groups[2][..1], "7");
        assert!(matches!(&groups[3][..1], "8" | "9" | "a" | "b"));
    }
}
