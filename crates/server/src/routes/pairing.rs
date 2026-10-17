//! The §2.12 routes: Pairing, Devices and API tokens (management-api §2.12; #13, #41, #21).
//!
//! Handlers are thin: access is decided by the auth layer (§1.2), the identity the listeners insert
//! ([`PeerIdentity`], [`ClientCertSpki`]) is passed to the [`PairingBackend`], and the backend owns
//! the logic. The three `/v1/pairing*` routes are re-checked here as `[local]` (D3) so the module
//! is safe to mount without the auth layer; `pair` needs the connection's cert, which only the
//! remote listener inserts.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{FromRef, Path, Query, State};
use axum::{Extension, Json};
use claudeclaw_api::pairing::{
    DeviceInfo, DeviceList, DeviceRevoked, DeviceUpdate, PairRequest, PairResult, PairingCancelled,
    PairingCreate, PairingStart, PairingState, TokenCreate, TokenCreated, TokenList, TokenRevoked,
};
use claudeclaw_api::routes::{
    DEVICES_LIST, DEVICES_REVOKE, DEVICES_UPDATE, PAIR, PAIRING_CANCEL, PAIRING_CREATE,
    PAIRING_GET, TOKENS_CREATE, TOKENS_LIST, TOKENS_REVOKE,
};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::PeerIdentity;
use crate::backend::pairing::{PairingBackend, PairingPeer};
use crate::error::ApiFailure;
use crate::tls::identity::{ClientCertSpki, DeviceId, PeerAddr};

pub use crate::backend::pairing::PairingError;

/// The most a `pairing.get` long-poll will wait, as the spec's `wait=<s≤60>`.
pub const MAX_WAIT_SECS: u64 = 60;

/// The Pairing, Devices and API-token routes (management-api §2.12).
pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Arc<dyn PairingBackend>: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(pairing_create))
        .routes(routes!(pairing_get, pairing_cancel))
        .routes(routes!(pair))
        .routes(routes!(devices_list))
        .routes(routes!(devices_update, devices_revoke))
        .routes(routes!(tokens_list, tokens_create))
        .routes(routes!(tokens_revoke))
}

/// The `pairing.get` query: how long to long-poll for a state change.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
pub struct PairingGetQuery {
    /// Seconds to wait; clamped to [`MAX_WAIT_SECS`].
    wait: Option<u64>,
}

/// `pairing.create` (management-api §2.12): a new Pairing code, local-only.
#[utoipa::path(
    post,
    path = "/v1/pairing",
    tag = "pairing",
    operation_id = "pairing.create",
    request_body = PairingCreate,
    responses(
        (status = 200, description = "A live Pairing code", body = PairingStart),
        (status = 403, description = "Not the local socket or pipe", body = claudeclaw_api::ApiError),
        (status = 409, description = "Remote access is off", body = claudeclaw_api::ApiError),
        (status = 422, description = "The host is not a hostname or IP literal", body = claudeclaw_api::ApiError),
    ),
)]
pub async fn pairing_create(
    State(backend): State<Arc<dyn PairingBackend>>,
    identity: Option<Extension<PeerIdentity>>,
    Json(request): Json<PairingCreate>,
) -> Result<Json<PairingStart>, ApiFailure> {
    require_local(identity.as_ref())?;
    Ok(Json(backend.pairing_create(request).await?))
}

/// `pairing.get` (management-api §2.12): long-poll a Pairing's state, local-only.
#[utoipa::path(
    get,
    path = "/v1/pairing/{pairing_id}",
    tag = "pairing",
    operation_id = "pairing.get",
    params(
        ("pairing_id" = String, Path, description = "The Pairing id"),
        ("wait" = Option<u64>, Query, description = "Seconds to wait for a state change, at most 60"),
    ),
    responses(
        (status = 200, description = "Where the pairing stands", body = PairingState),
        (status = 403, description = "Not the local socket or pipe", body = claudeclaw_api::ApiError),
        (status = 404, description = "No such pairing", body = claudeclaw_api::ApiError),
    ),
)]
pub async fn pairing_get(
    State(backend): State<Arc<dyn PairingBackend>>,
    Path(pairing_id): Path<String>,
    Query(query): Query<PairingGetQuery>,
    identity: Option<Extension<PeerIdentity>>,
) -> Result<Json<PairingState>, ApiFailure> {
    require_local(identity.as_ref())?;
    let wait = Duration::from_secs(query.wait.unwrap_or(0).min(MAX_WAIT_SECS));
    Ok(Json(backend.pairing_get(pairing_id, wait).await?))
}

/// `pairing.cancel` (management-api §2.12), local-only.
#[utoipa::path(
    delete,
    path = "/v1/pairing/{pairing_id}",
    tag = "pairing",
    operation_id = "pairing.cancel",
    params(("pairing_id" = String, Path, description = "The Pairing id")),
    responses(
        (status = 200, description = "Whether a live pairing was cancelled", body = PairingCancelled),
        (status = 403, description = "Not the local socket or pipe", body = claudeclaw_api::ApiError),
        (status = 404, description = "No such pairing", body = claudeclaw_api::ApiError),
    ),
)]
pub async fn pairing_cancel(
    State(backend): State<Arc<dyn PairingBackend>>,
    Path(pairing_id): Path<String>,
    identity: Option<Extension<PeerIdentity>>,
) -> Result<Json<PairingCancelled>, ApiFailure> {
    require_local(identity.as_ref())?;
    Ok(Json(backend.pairing_cancel(pairing_id).await?))
}

/// `pair` (management-api §2.12): redeem a code over the remote listener, pinning the client cert.
#[utoipa::path(
    post,
    path = "/v1/pair",
    tag = "pairing",
    operation_id = "pair",
    request_body = PairRequest,
    responses(
        (status = 200, description = "The new Device and its Daemon", body = PairResult),
        (status = 401, description = "The code is invalid, or no cert was presented", body = claudeclaw_api::ApiError),
        (status = 410, description = "The code expired", body = claudeclaw_api::ApiError),
    ),
)]
pub async fn pair(
    State(backend): State<Arc<dyn PairingBackend>>,
    spki: Option<Extension<ClientCertSpki>>,
    peer: Option<Extension<PeerAddr>>,
    Json(request): Json<PairRequest>,
) -> Result<Json<PairResult>, ApiFailure> {
    let peer = PairingPeer {
        spki: spki.map(|Extension(spki)| spki),
        peer: peer.map(|Extension(peer)| peer),
    };
    Ok(Json(backend.pair(request, peer).await?))
}

/// `devices.list` (management-api §2.12).
#[utoipa::path(
    get,
    path = "/v1/devices",
    tag = "pairing",
    operation_id = "devices.list",
    responses((status = 200, description = "The paired Devices", body = DeviceList)),
)]
pub async fn devices_list(
    State(backend): State<Arc<dyn PairingBackend>>,
    identity: Option<Extension<PeerIdentity>>,
) -> Result<Json<DeviceList>, ApiFailure> {
    Ok(Json(
        backend
            .devices_list(current_device(identity.as_ref()))
            .await?,
    ))
}

/// `devices.update` (management-api §2.12).
#[utoipa::path(
    patch,
    path = "/v1/devices/{device_id}",
    tag = "pairing",
    operation_id = "devices.update",
    params(("device_id" = String, Path, description = "The Device id")),
    request_body = DeviceUpdate,
    responses(
        (status = 200, description = "The renamed Device", body = DeviceInfo),
        (status = 404, description = "No such live Device", body = claudeclaw_api::ApiError),
    ),
)]
pub async fn devices_update(
    State(backend): State<Arc<dyn PairingBackend>>,
    Path(device_id): Path<String>,
    identity: Option<Extension<PeerIdentity>>,
    Json(update): Json<DeviceUpdate>,
) -> Result<Json<DeviceInfo>, ApiFailure> {
    let current = current_device(identity.as_ref());
    Ok(Json(
        backend.devices_update(device_id, update, current).await?,
    ))
}

/// `devices.revoke` (management-api §2.12).
#[utoipa::path(
    delete,
    path = "/v1/devices/{device_id}",
    tag = "pairing",
    operation_id = "devices.revoke",
    params(("device_id" = String, Path, description = "The Device id")),
    responses(
        (status = 200, description = "Whether a Device was revoked, and whether it was disconnected", body = DeviceRevoked),
        (status = 404, description = "No such Device", body = claudeclaw_api::ApiError),
    ),
)]
pub async fn devices_revoke(
    State(backend): State<Arc<dyn PairingBackend>>,
    Path(device_id): Path<String>,
) -> Result<Json<DeviceRevoked>, ApiFailure> {
    Ok(Json(backend.devices_revoke(device_id).await?))
}

/// `tokens.list` (management-api §2.12).
#[utoipa::path(
    get,
    path = "/v1/tokens",
    tag = "pairing",
    operation_id = "tokens.list",
    responses((status = 200, description = "The API tokens", body = TokenList)),
)]
pub async fn tokens_list(
    State(backend): State<Arc<dyn PairingBackend>>,
) -> Result<Json<TokenList>, ApiFailure> {
    Ok(Json(backend.tokens_list().await?))
}

/// `tokens.create` (management-api §2.12): the only time a token's secret is visible.
#[utoipa::path(
    post,
    path = "/v1/tokens",
    tag = "pairing",
    operation_id = "tokens.create",
    request_body = TokenCreate,
    responses(
        (status = 201, description = "The new token and its secret", body = TokenCreated),
        (status = 422, description = "The permission set is not valid", body = claudeclaw_api::ApiError),
    ),
)]
pub async fn tokens_create(
    State(backend): State<Arc<dyn PairingBackend>>,
    Json(request): Json<TokenCreate>,
) -> Result<(axum::http::StatusCode, Json<TokenCreated>), ApiFailure> {
    Ok((
        axum::http::StatusCode::CREATED,
        Json(backend.tokens_create(request).await?),
    ))
}

/// `tokens.revoke` (management-api §2.12).
#[utoipa::path(
    delete,
    path = "/v1/tokens/{token_id}",
    tag = "pairing",
    operation_id = "tokens.revoke",
    params(("token_id" = String, Path, description = "The token id")),
    responses(
        (status = 200, description = "Whether a token was revoked", body = TokenRevoked),
        (status = 404, description = "No such token", body = claudeclaw_api::ApiError),
    ),
)]
pub async fn tokens_revoke(
    State(backend): State<Arc<dyn PairingBackend>>,
    Path(token_id): Path<String>,
) -> Result<Json<TokenRevoked>, ApiFailure> {
    Ok(Json(backend.tokens_revoke(token_id).await?))
}

/// The three Pairing routes are `[local]` (management-api §1.2, D3): only the local socket, pipe,
/// or an in-process test (no listener extension at all) may call them.
fn require_local(identity: Option<&Extension<PeerIdentity>>) -> Result<(), ApiFailure> {
    match identity.map(|Extension(identity)| identity) {
        None | Some(PeerIdentity::Local) => Ok(()),
        Some(_) => Err(ApiFailure::local_only()),
    }
}

/// The calling Device's id, when there is one, for `DeviceInfo.current`.
fn current_device(identity: Option<&Extension<PeerIdentity>>) -> Option<DeviceId> {
    identity.and_then(|Extension(identity)| match identity {
        PeerIdentity::Device { id } => Some(id.clone()),
        _ => None,
    })
}

/// The route constants this module serves, in management-api §2.12 order.
pub const ROUTES: &[claudeclaw_api::Route] = &[
    PAIRING_CREATE,
    PAIRING_GET,
    PAIRING_CANCEL,
    PAIR,
    DEVICES_LIST,
    DEVICES_UPDATE,
    DEVICES_REVOKE,
    TOKENS_LIST,
    TOKENS_CREATE,
    TOKENS_REVOKE,
];

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use chrono::{DateTime, Utc};
    use claudeclaw_api::pairing::{
        ApiTokenInfo, DeviceInfo as WireDeviceInfo, DeviceRevoked, DeviceUpdate, PairingCancelled,
        PairingStart, PairingState, PairingStatus, TokenCreated, TokenList, TokenRevoked,
    };
    use claudeclaw_api::permission::TokenPermission;
    use tower::ServiceExt;

    use axum::extract::FromRef;

    use super::*;
    use crate::backend::pairing::{PairFuture, PairingBackend, PairingError, PairingPeer};

    /// A backend that answers canned §2.12 values, so the route wiring is tested on its own.
    struct FakeBackend;

    fn at() -> DateTime<Utc> {
        "2026-10-16T09:00:00Z".parse().unwrap()
    }

    impl PairingBackend for FakeBackend {
        fn pairing_create(&self, _request: PairingCreate) -> PairFuture<'_, PairingStart> {
            Box::pin(async {
                Ok(PairingStart {
                    pairing_id: "pair_1".into(),
                    code: "ABCD-EFGH".into(),
                    expires_at: at(),
                    fingerprint: "FP".into(),
                    fingerprint_curl: "sha256//FP".into(),
                    listen: vec!["0.0.0.0:4633".into()],
                    pairing_string: "ccpair1.ABCDEFGH.FP.4633".into(),
                })
            })
        }

        fn pairing_get(&self, _id: String, _wait: Duration) -> PairFuture<'_, PairingState> {
            Box::pin(async {
                Ok(PairingState {
                    state: PairingStatus::Pending,
                    device: None,
                })
            })
        }

        fn pairing_cancel(&self, _id: String) -> PairFuture<'_, PairingCancelled> {
            Box::pin(async { Ok(PairingCancelled { cancelled: true }) })
        }

        fn pair(&self, _request: PairRequest, _peer: PairingPeer) -> PairFuture<'_, PairResult> {
            Box::pin(async { Err(PairingError::pairing_code_invalid()) })
        }

        fn devices_list(&self, _current: Option<DeviceId>) -> PairFuture<'_, DeviceList> {
            Box::pin(async { Ok(DeviceList { devices: vec![] }) })
        }

        fn devices_update(
            &self,
            _id: String,
            _update: DeviceUpdate,
            _current: Option<DeviceId>,
        ) -> PairFuture<'_, WireDeviceInfo> {
            Box::pin(async { Err(PairingError::not_found("device")) })
        }

        fn devices_revoke(&self, _id: String) -> PairFuture<'_, DeviceRevoked> {
            Box::pin(async {
                Ok(DeviceRevoked {
                    revoked: true,
                    disconnected: false,
                })
            })
        }

        fn tokens_list(&self) -> PairFuture<'_, TokenList> {
            Box::pin(async {
                Ok(TokenList {
                    tokens: vec![ApiTokenInfo {
                        id: "tok_1".into(),
                        name: "ci".into(),
                        allow: vec![TokenPermission::Send],
                        created_at: at(),
                        last_used_at: None,
                        legacy: false,
                        source: None,
                    }],
                })
            })
        }

        fn tokens_create(&self, _request: TokenCreate) -> PairFuture<'_, TokenCreated> {
            Box::pin(async { Err(PairingError::invalid_permission_set()) })
        }

        fn tokens_revoke(&self, _id: String) -> PairFuture<'_, TokenRevoked> {
            Box::pin(async { Ok(TokenRevoked { revoked: true }) })
        }
    }

    #[derive(Clone)]
    struct TestState {
        backend: Arc<dyn PairingBackend>,
    }

    impl FromRef<TestState> for Arc<dyn PairingBackend> {
        fn from_ref(state: &TestState) -> Self {
            state.backend.clone()
        }
    }

    fn app() -> Router {
        let (router, _api) = routes::<TestState>().split_for_parts();
        router.with_state(TestState {
            backend: Arc::new(FakeBackend),
        })
    }

    async fn send(request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    fn json_request(method: Method, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn pairing_create_serves_the_backend() {
        let (status, body) = send(json_request(Method::POST, "/v1/pairing", "{}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["code"], "ABCD-EFGH");
        assert_eq!(body["pairing_id"], "pair_1");
    }

    #[tokio::test]
    async fn pairing_routes_are_local_only() {
        let device = PeerIdentity::Device { id: "dev_1".into() };
        let mut request = json_request(Method::POST, "/v1/pairing", "{}");
        request.extensions_mut().insert(device.clone());
        let (status, body) = send(request).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "local_only");

        let mut request = Request::builder()
            .method(Method::GET)
            .uri("/v1/pairing/pair_1")
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(device);
        let (status, _) = send(request).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let mut request = Request::builder()
            .method(Method::DELETE)
            .uri("/v1/pairing/pair_1")
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(PeerIdentity::UnknownCert);
        let (status, _) = send(request).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn pairing_get_is_pending_without_a_waiter() {
        let (status, body) = send(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/pairing/pair_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "pending");
        assert_eq!(body["device"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn pair_maps_a_backend_error_to_the_envelope() {
        let (status, body) = send(json_request(
            Method::POST,
            "/v1/pair",
            r#"{"code":"ZZZZ-ZZZZ","label":"x","platform":"y"}"#,
        ))
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "pairing_code_invalid");
    }

    #[tokio::test]
    async fn devices_revoke_and_tokens_list_serve_the_backend() {
        let (status, body) = send(
            Request::builder()
                .method(Method::DELETE)
                .uri("/v1/devices/dev_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["revoked"], true);

        let (status, body) = send(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/tokens")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["tokens"][0]["name"], "ci");
    }

    #[tokio::test]
    async fn tokens_create_answers_201_or_the_envelope() {
        let (status, body) = send(json_request(
            Method::POST,
            "/v1/tokens",
            r#"{"name":"bad","allow":["runs:content"]}"#,
        ))
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "invalid_permission_set");
    }

    #[tokio::test]
    async fn the_router_serves_exactly_the_section_2_12_routes() {
        let (_router, api) = routes::<TestState>().split_for_parts();
        let paths = &api.paths.paths;
        let has = |path: &str, method: &str| {
            paths.get(path).is_some_and(|item| match method {
                "GET" => item.get.is_some(),
                "POST" => item.post.is_some(),
                "PATCH" => item.patch.is_some(),
                "DELETE" => item.delete.is_some(),
                _ => false,
            })
        };
        for route in ROUTES {
            assert!(
                has(route.path, route.method.as_str()),
                "missing {} {}",
                route.method.as_str(),
                route.path
            );
        }
        // No §2.12 path carries an unexpected method.
        for (path, item) in paths {
            let mut methods = 0;
            methods += usize::from(item.get.is_some());
            methods += usize::from(item.post.is_some());
            methods += usize::from(item.patch.is_some());
            methods += usize::from(item.delete.is_some());
            assert_eq!(
                methods,
                ROUTES.iter().filter(|r| r.path == path).count(),
                "{path} has a stray method"
            );
        }
    }
}
