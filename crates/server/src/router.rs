//! The Management API router (architecture §4, `claudeclaw-server`).
//!
//! One router serves every listener; the only per-listener difference is the [`PeerIdentity`]
//! stamped onto each request by [`with_peer_identity`]. Each route is wrapped by three layers,
//! outermost first: auth, then audit, then the deadline backstop. Innermost sit the route's body
//! limit (§1.6) and the mapping that renders an oversized body as §1.4's envelope.

use std::convert::Infallible;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request};
use axum::middleware::{self, Next};
use axum::routing::{MethodRouter, delete, get, patch, post, put};

use claudeclaw_api::routes::{
    AGENTS_ARCHIVE, AGENTS_ARCHIVED, AGENTS_CREATE, AGENTS_LIST, AGENTS_PERSONA_GET,
    AGENTS_PERSONA_PUT, AGENTS_PURGE, AGENTS_RESTORE, AUDIT_LIST, DAEMON_REEXEC, DAEMON_RESTART,
    DAEMON_STOP, DEVICES_LIST, DEVICES_REVOKE, DEVICES_UPDATE, EVENTS_STREAM, HEALTH_GET,
    HOUSEKEEPING_RUN, IMPORT_APPLY, IMPORT_GET, IMPORT_PLAN, JOBS_AUDIT, JOBS_CHECK, JOBS_CREATE,
    JOBS_DELETE, JOBS_GET, JOBS_LIST, JOBS_PREVIEW, JOBS_REPLACE, JOBS_RUN, JOBS_UPDATE,
    LAUNCHERS_LIST, LAUNCHERS_SET, LAUNCHERS_TEST, LAUNCHERS_UNSET, LOGS_EXPORT, LOGS_STREAM,
    LOGS_SUMMARY, LOGS_TAIL, OPENAPI_GET, PAIR, PAIRING_CANCEL, PAIRING_CREATE, PAIRING_GET,
    RUNS_GET, RUNS_LIST, RUNS_LOG, Route, SCOPES_LIST, SESSIONS_COMPACT, SESSIONS_DELETE,
    SESSIONS_GET, SESSIONS_LIST, SESSIONS_MESSAGES, SESSIONS_RESET, SETTINGS_GET, SETTINGS_UPDATE,
    STATUS_GET, TOKENS_CREATE, TOKENS_LIST, TOKENS_REVOKE, TURNS_CREATE, TURNS_EVENTS, TURNS_GET,
    UPDATE_CHECK, UPDATE_GET, USAGE_GET,
};

use crate::audit::AuditCtx;
use crate::auth::{AuthCtx, PeerIdentity};
use crate::deadline::DeadlineCtx;
use crate::error::BodyLimit;
use crate::routes::{
    agents, audit, daemon, events, import, jobs, logs, pairing, runs, scopes, sessions, settings,
    turns, usage,
};
use crate::state::AppState;

/// The router: the §2.1–§2.6, §2.9–§2.12 routes, with their layers and state.
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
    let router = mount(router, TURNS_CREATE, post(turns::create), &state);
    let router = mount(router, TURNS_GET, get(turns::get), &state);
    let router = mount(router, TURNS_EVENTS, get(turns::events), &state);
    let router = mount(router, JOBS_LIST, get(jobs::list), &state);
    let router = mount(router, JOBS_GET, get(jobs::get), &state);
    let router = mount(router, JOBS_CREATE, post(jobs::create), &state);
    let router = mount(router, JOBS_UPDATE, patch(jobs::update), &state);
    let router = mount(router, JOBS_REPLACE, put(jobs::replace), &state);
    let router = mount(router, JOBS_DELETE, delete(jobs::delete), &state);
    let router = mount(router, JOBS_RUN, post(jobs::run), &state);
    let router = mount(router, JOBS_PREVIEW, post(jobs::preview), &state);
    let router = mount(router, JOBS_CHECK, get(jobs::check), &state);
    let router = mount(router, JOBS_AUDIT, get(jobs::audit), &state);
    let router = mount(router, AGENTS_LIST, get(agents::list), &state);
    let router = mount(router, AGENTS_ARCHIVED, get(agents::archived), &state);
    let router = mount(router, AGENTS_CREATE, post(agents::create), &state);
    let router = mount(router, AGENTS_PERSONA_GET, get(agents::persona_get), &state);
    let router = mount(router, AGENTS_PERSONA_PUT, put(agents::persona_put), &state);
    let router = mount(router, AGENTS_ARCHIVE, post(agents::archive), &state);
    let router = mount(router, AGENTS_RESTORE, post(agents::restore), &state);
    let router = mount(router, AGENTS_PURGE, delete(agents::purge), &state);
    let router = mount(router, SCOPES_LIST, get(scopes::list), &state);
    let router = mount(router, LAUNCHERS_LIST, get(scopes::launchers), &state);
    let router = mount(router, LAUNCHERS_SET, put(scopes::set_launcher), &state);
    let router = mount(
        router,
        LAUNCHERS_UNSET,
        delete(scopes::unset_launcher),
        &state,
    );
    let router = mount(router, LAUNCHERS_TEST, post(scopes::test_launcher), &state);
    let router = mount(router, SESSIONS_LIST, get(sessions::list), &state);
    let router = mount(router, SESSIONS_GET, get(sessions::get), &state);
    let router = mount(router, SESSIONS_MESSAGES, get(sessions::messages), &state);
    let router = mount(router, SESSIONS_RESET, post(sessions::reset), &state);
    let router = mount(router, SESSIONS_COMPACT, post(sessions::compact), &state);
    let router = mount(router, SESSIONS_DELETE, delete(sessions::delete), &state);
    let router = mount(router, LOGS_TAIL, get(logs::tail), &state);
    let router = mount(router, LOGS_STREAM, get(logs::stream), &state);
    let router = mount(router, LOGS_SUMMARY, get(logs::summary), &state);
    let router = mount(router, LOGS_EXPORT, get(logs::export), &state);
    let router = mount(router, RUNS_LIST, get(runs::list), &state);
    let router = mount(router, RUNS_GET, get(runs::get), &state);
    let router = mount(router, RUNS_LOG, get(runs::log), &state);
    let router = mount(router, AUDIT_LIST, get(audit::list), &state);
    let router = mount(router, USAGE_GET, get(usage::get), &state);
    let router = mount(router, SETTINGS_GET, get(settings::get), &state);
    let router = mount(router, SETTINGS_UPDATE, patch(settings::update), &state);
    let router = mount(router, IMPORT_GET, get(import::get), &state);
    let router = mount(router, IMPORT_PLAN, get(import::plan), &state);
    let router = mount(router, IMPORT_APPLY, post(import::apply), &state);
    let router = mount(
        router,
        PAIRING_CREATE,
        post(pairing::pairing_create),
        &state,
    );
    let router = mount(router, PAIRING_GET, get(pairing::pairing_get), &state);
    let router = mount(
        router,
        PAIRING_CANCEL,
        delete(pairing::pairing_cancel),
        &state,
    );
    let router = mount(router, PAIR, post(pairing::pair), &state);
    let router = mount(router, DEVICES_LIST, get(pairing::devices_list), &state);
    let router = mount(
        router,
        DEVICES_UPDATE,
        patch(pairing::devices_update),
        &state,
    );
    let router = mount(
        router,
        DEVICES_REVOKE,
        delete(pairing::devices_revoke),
        &state,
    );
    let router = mount(router, TOKENS_LIST, get(pairing::tokens_list), &state);
    let router = mount(router, TOKENS_CREATE, post(pairing::tokens_create), &state);
    let router = mount(
        router,
        TOKENS_REVOKE,
        delete(pairing::tokens_revoke),
        &state,
    );
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

/// §1.6: `turns.create` takes a 64 MiB body (the attachments), every other route 1 MiB.
fn body_limit(route: Route) -> usize {
    if route == TURNS_CREATE {
        64 * 1024 * 1024
    } else {
        1024 * 1024
    }
}

/// Registers one route, applying auth, audit and deadline (outermost first), then the body
/// limit around the handler.
fn mount(
    router: Router<AppState>,
    route: Route,
    method: MethodRouter<AppState>,
    state: &AppState,
) -> Router<AppState> {
    let limit = body_limit(route);
    let method = method
        // Innermost: the limit is enforced by the extractors that read the body, and an
        // oversized one comes back as §1.4's envelope, so audit still sees the final response.
        .layer::<_, Infallible>(middleware::from_fn_with_state(
            BodyLimit(limit),
            crate::error::payload_too_large_envelope,
        ))
        .layer(DefaultBodyLimit::max(limit))
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
