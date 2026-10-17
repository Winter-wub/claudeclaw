//! Principals and the auth layer (management-api §1.2–1.3, architecture §4).
//!
//! One router serves every listener. Each listener stamps a [`PeerIdentity`] onto the request
//! before the router runs; this layer turns it into exactly one [`Principal`], checks it against
//! the route's `Access`, then applies the lifecycle gate of §1.8. A request that passes carries
//! its `Principal` in its extensions for the audit layer and the handlers.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use claudeclaw_api::log_event::daemon;
use claudeclaw_api::permission::TokenPermission;
use claudeclaw_api::routes::{Access, Method, Route};
use claudeclaw_api::status::HealthState;

use crate::backend::StatusBackend;
use crate::error::{ApiFailure, code_name};

/// Where a request's peer sits. A listener stamps this onto every request before the router runs.
#[derive(Debug, Clone)]
pub enum PeerIdentity {
    /// The 0600 socket or owner-only pipe: fully local (architecture §4).
    Local,
    /// A remote client cert whose SPKI matches a paired Device.
    Device { id: String },
    /// A remote client cert that is not paired yet; only `POST /v1/pair` may use it.
    UnknownCert,
}

impl PeerIdentity {
    /// The listener name for the `api.auth_failed` log event, never a secret.
    pub fn listener(&self) -> &'static str {
        match self {
            PeerIdentity::Local => "local",
            PeerIdentity::Device { .. } | PeerIdentity::UnknownCert => "remote",
        }
    }

    /// A content-free peer label: the Device id, `local`, or `unknown`.
    pub fn peer(&self) -> &str {
        match self {
            PeerIdentity::Local => "local",
            PeerIdentity::Device { id } => id,
            PeerIdentity::UnknownCert => "unknown",
        }
    }
}

/// The one principal the auth layer yields (§1.2).
#[derive(Debug, Clone)]
pub enum Principal {
    Local,
    Device(String),
    ApiToken(String, Vec<TokenPermission>),
    Pairing,
}

impl Principal {
    /// `admin` = `Local` or `Device` (§1.2).
    pub fn is_admin(&self) -> bool {
        matches!(self, Principal::Local | Principal::Device(_))
    }

    /// The audit log's `principal` string: `local`, `device:<id>` or `token:<id>` (§2.9).
    pub fn audit_name(&self) -> String {
        match self {
            Principal::Local => "local".to_owned(),
            Principal::Device(id) => format!("device:{id}"),
            Principal::ApiToken(id, _) => format!("token:{id}"),
            Principal::Pairing => "pairing".to_owned(),
        }
    }
}

/// One stored API token. Only its SHA-256 hash is kept (#13).
#[derive(Debug, Clone)]
pub struct TokenRecord {
    pub id: String,
    pub permissions: Vec<TokenPermission>,
    pub revoked: bool,
    /// The pre-image hash the bearer value is compared against, in constant time.
    pub hash: [u8; 32],
}

/// Where the auth layer reads tokens from. A real store caches `tokens.json`; the composition root
/// implements this over it.
pub trait TokenStore: Send + Sync + 'static {
    fn lookup(&self, hash: &[u8; 32]) -> Option<TokenRecord>;
}

/// The SHA-256 of a bearer value, the only form a token is stored in (#13).
pub fn hash_token(secret: &str) -> [u8; 32] {
    let digest = Sha256::digest(secret.as_bytes());
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&digest[..]);
    hash
}

/// The auth layer's per-route state: where tokens come from, where the lifecycle comes from, and
/// which route is being guarded.
#[derive(Clone)]
pub struct AuthCtx {
    tokens: Arc<dyn TokenStore>,
    status: Arc<dyn StatusBackend>,
    route: Route,
}

impl AuthCtx {
    pub fn new(tokens: Arc<dyn TokenStore>, status: Arc<dyn StatusBackend>, route: Route) -> Self {
        AuthCtx {
            tokens,
            status,
            route,
        }
    }
}

/// The middleware. Authorization first, then the lifecycle gate (§1.8), then the handler.
pub async fn authenticate(
    State(ctx): State<AuthCtx>,
    mut request: Request,
    next: Next,
) -> Response {
    let principal = match authorize(&ctx, &request) {
        Ok(principal) => principal,
        Err(failure) => {
            log_auth_failed(&request, &failure);
            return failure.into_response();
        }
    };
    if let Err(failure) = lifecycle_gate(&ctx) {
        return failure.into_response();
    }
    request.extensions_mut().insert(principal);
    next.run(request).await
}

/// Resolves the peer's principal and checks it against the route's `Access`.
fn authorize(ctx: &AuthCtx, request: &Request) -> Result<Principal, ApiFailure> {
    let Some(identity) = request.extensions().get::<PeerIdentity>().cloned() else {
        return Err(ApiFailure::unauthorized(
            "this request has no peer identity",
        ));
    };
    let principal = match identity {
        PeerIdentity::Local => Principal::Local,
        PeerIdentity::Device { id } => Principal::Device(id),
        // On the local socket an Authorization header is ignored; on the remote listener it only
        // counts for an unpaired cert, the `pair` case (§1.2).
        PeerIdentity::UnknownCert => match bearer(request) {
            Some(secret) => token_principal(ctx, secret)?,
            None => Principal::Pairing,
        },
    };
    check_access(ctx.route.access, &principal)?;
    Ok(principal)
}

fn check_access(access: Access, principal: &Principal) -> Result<(), ApiFailure> {
    match access {
        Access::Any => Ok(()),
        Access::Admin => {
            if principal.is_admin() {
                Ok(())
            } else {
                Err(denied(principal))
            }
        }
        Access::Local => {
            if matches!(principal, Principal::Local) {
                Ok(())
            } else {
                Err(ApiFailure::local_only())
            }
        }
        Access::Token(required) => {
            if principal.is_admin() {
                return Ok(());
            }
            match principal {
                Principal::ApiToken(_, granted) => match missing(required, granted) {
                    Some(permission) => Err(ApiFailure::missing_permission(&permission)),
                    None => Ok(()),
                },
                _ => Err(denied(principal)),
            }
        }
        Access::TurnOwner => {
            if principal.is_admin() || matches!(principal, Principal::ApiToken(..)) {
                Ok(())
            } else {
                Err(denied(principal))
            }
        }
        Access::Pairing => {
            if matches!(principal, Principal::Pairing) {
                Ok(())
            } else {
                Err(ApiFailure::unauthorized(
                    "this route requires an unpaired client certificate",
                ))
            }
        }
    }
}

/// The first permission `required` names that `granted` lacks.
fn missing(required: &[TokenPermission], granted: &[TokenPermission]) -> Option<TokenPermission> {
    required
        .iter()
        .copied()
        .find(|permission| !granted.contains(permission))
}

fn denied(principal: &Principal) -> ApiFailure {
    match principal {
        Principal::Pairing => ApiFailure::unauthorized("pair this device first"),
        _ => ApiFailure::admin_required(),
    }
}

fn lifecycle_gate(ctx: &AuthCtx) -> Result<(), ApiFailure> {
    if ctx.route.method == Method::Get {
        return Ok(());
    }
    match ctx.status.health_state() {
        HealthState::Starting => Err(ApiFailure::daemon_starting()),
        HealthState::Stopping => Err(ApiFailure::daemon_stopping()),
        HealthState::Ready | HealthState::Unknown => Ok(()),
    }
}

/// The `Bearer` value of an `Authorization` header, case-insensitively per RFC 6750.
fn bearer(request: &Request) -> Option<&str> {
    let value = request.headers().get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, secret) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let secret = secret.trim();
    if secret.is_empty() {
        return None;
    }
    Some(secret)
}

fn token_principal(ctx: &AuthCtx, secret: &str) -> Result<Principal, ApiFailure> {
    let hash = hash_token(secret);
    let Some(record) = ctx.tokens.lookup(&hash) else {
        return Err(ApiFailure::unauthorized("unknown API token"));
    };
    if record.revoked {
        return Err(ApiFailure::unauthorized("this API token is revoked"));
    }
    // The store should have compared already; re-check in constant time anyway.
    if !bool::from(record.hash.ct_eq(&hash)) {
        return Err(ApiFailure::unauthorized("unknown API token"));
    }
    Ok(Principal::ApiToken(record.id, record.permissions))
}

fn log_auth_failed(request: &Request, failure: &ApiFailure) {
    let (listener, peer) = match request.extensions().get::<PeerIdentity>() {
        Some(identity) => (identity.listener(), identity.peer()),
        None => ("unknown", "unknown"),
    };
    let reason = code_name(failure.code());
    tracing::warn!(
        event = daemon::API_AUTH_FAILED,
        listener = listener,
        reason = %reason,
        peer = peer,
        "management api request refused",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bearer_header_parses_case_insensitively() {
        let request = |value: &str| {
            Request::builder()
                .header(AUTHORIZATION, value)
                .body(axum::body::Body::empty())
                .unwrap()
        };
        assert_eq!(bearer(&request("Bearer abc")), Some("abc"));
        assert_eq!(bearer(&request("bearer abc")), Some("abc"));
        assert_eq!(bearer(&request("Basic abc")), None);
        assert_eq!(bearer(&request("Bearer ")), None);
        assert_eq!(bearer(&request("abc")), None);
    }

    #[test]
    fn hashing_is_stable_and_not_the_secret() {
        let hash = hash_token("secret-value");
        assert_eq!(hash, hash_token("secret-value"));
        assert_ne!(hash, hash_token("other-value"));
    }

    #[test]
    fn access_rules_follow_section_1_2() {
        let token = |perms: Vec<TokenPermission>| Principal::ApiToken("t".into(), perms);
        assert!(check_access(Access::Any, &Principal::Pairing).is_ok());
        assert!(check_access(Access::Admin, &Principal::Local).is_ok());
        assert!(check_access(Access::Admin, &Principal::Device("d".into())).is_ok());
        assert!(check_access(Access::Admin, &token(vec![])).is_err());
        assert!(check_access(Access::Local, &token(vec![])).is_err());
        assert!(check_access(Access::Local, &Principal::Local).is_ok());
        assert!(
            check_access(
                Access::Token(&[TokenPermission::StatusRead]),
                &token(vec![TokenPermission::StatusRead])
            )
            .is_ok()
        );
        assert!(
            check_access(
                Access::Token(&[TokenPermission::StatusRead]),
                &token(vec![])
            )
            .is_err()
        );
        assert!(check_access(Access::Pairing, &Principal::Pairing).is_ok());
        assert!(check_access(Access::Pairing, &Principal::Local).is_err());
        assert!(check_access(Access::TurnOwner, &token(vec![])).is_ok());
    }

    #[test]
    fn the_first_missing_permission_is_named() {
        let missing = missing(
            &[TokenPermission::LogsRead, TokenPermission::RunsContent],
            &[TokenPermission::LogsRead],
        );
        assert_eq!(missing, Some(TokenPermission::RunsContent));
    }
}
