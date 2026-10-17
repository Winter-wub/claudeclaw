//! The Management API router (architecture §4, `claudeclaw-server`).
//!
//! One router serves every listener; the only per-listener difference is the [`PeerIdentity`]
//! stamped onto each request by [`with_peer_identity`]. Each route is wrapped by three layers,
//! outermost first: auth, then audit, then the deadline backstop.

use axum::Router;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::routing::{MethodRouter, get, post};

use claudeclaw_api::routes::{
    DAEMON_REEXEC, DAEMON_RESTART, DAEMON_STOP, EVENTS_STREAM, HEALTH_GET, HOUSEKEEPING_RUN,
    OPENAPI_GET, Route, STATUS_GET, UPDATE_CHECK, UPDATE_GET,
};

use crate::audit::AuditCtx;
use crate::auth::{AuthCtx, PeerIdentity};
use crate::deadline::DeadlineCtx;
use crate::routes::{daemon, events};
use crate::state::AppState;

/// The H1 router: §2.1 and §2.2 routes, with their layers and state.
pub fn router(state: AppState) -> Router {
    let router = Router::new();
    let router = mount(router, HEALTH_GET, get(daemon::health), &state);
    let router = mount(router, STATUS_GET, get(daemon::status), &state);
    let router = mount(router, OPENAPI_GET, get(daemon::openapi), &state);
    let router = mount(router, DAEMON_STOP, post(daemon::stop), &state);
    let router = mount(router, DAEMON_RESTART, post(daemon::restart), &state);
    let router = mount(router, DAEMON_REEXEC, post(daemon::reexec), &state);
    let router = mount(router, UPDATE_GET, get(daemon::update), &state);
    let router = mount(router, UPDATE_CHECK, post(daemon::check_update), &state);
    let router = mount(router, HOUSEKEEPING_RUN, post(daemon::housekeeping), &state);
    let router = mount(router, EVENTS_STREAM, get(events::stream), &state);
    router.with_state(state)
}

/// The local listener's app: the router with `Local` stamped on every connection.
pub fn local_app(state: AppState) -> Router {
    with_peer_identity(router(state), PeerIdentity::Local)
}

/// Wraps `router` so every request carries `identity` before the auth layer runs.
pub fn with_peer_identity(router: Router, identity: PeerIdentity) -> Router {
    router.layer(middleware::from_fn(
        move |mut request: Request, next: Next| {
            let identity = identity.clone();
            async move {
                request.extensions_mut().insert(identity);
                next.run(request).await
            }
        },
    ))
}

/// Registers one route, applying auth, audit and deadline (outermost first).
fn mount(
    router: Router<AppState>,
    route: Route,
    method: MethodRouter<AppState>,
    state: &AppState,
) -> Router<AppState> {
    let method = method
        .layer(middleware::from_fn_with_state(
            DeadlineCtx {
                deadline: route.deadline,
            },
            crate::deadline::enforce,
        ))
        .layer(middleware::from_fn_with_state(
            AuditCtx::new(state.audit.clone(), route),
            crate::audit::observe,
        ))
        .layer(middleware::from_fn_with_state(
            AuthCtx::new(state.tokens.clone(), state.status.clone(), route),
            crate::auth::authenticate,
        ));
    router.route(route.path, method)
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::routes::Route;

    use super::*;

    #[test]
    fn the_h1_routes_are_the_section_2_1_and_2_2_rows() {
        // Guards against a typo in the mounting list: the ten ops H1 serves, in §2 order.
        let ops = [
            HEALTH_GET.op,
            STATUS_GET.op,
            OPENAPI_GET.op,
            DAEMON_STOP.op,
            DAEMON_RESTART.op,
            DAEMON_REEXEC.op,
            UPDATE_GET.op,
            UPDATE_CHECK.op,
            HOUSEKEEPING_RUN.op,
            EVENTS_STREAM.op,
        ];
        let expected = [
            "health.get",
            "status.get",
            "openapi.get",
            "daemon.stop",
            "daemon.restart",
            "daemon.reexec",
            "update.get",
            "update.check",
            "housekeeping.run",
            "events.stream",
        ];
        assert_eq!(ops, expected);
        for op in expected {
            assert!(Route::by_op(op).is_some(), "{op}");
        }
    }
}
