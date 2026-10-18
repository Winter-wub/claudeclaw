//! The Scopes and Launchers handlers (management-api §2.5): `scopes.list`, `launchers.list`,
//! `launchers.set`, `launchers.unset` and `launchers.test`.
//!
//! A Scope in a path is its typed key, percent-encoded (§1.7): a malformed one is
//! `400 invalid_scope` before the backend is asked. A Launcher is `"direct"` or an absolute path
//! and never appears in a URL path. The Scope-centric writes name the Scope as the audit entry's
//! `target`, success or failure.

use std::path::Path as FsPath;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use claudeclaw_api::deadline::Deadline;
use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::scope::Scope;
use claudeclaw_api::scopes::{
    LauncherOverview, LauncherSet, LauncherTestRequest, LauncherTestResult, ScopeKind, ScopeList,
};

use crate::audit::AuditTarget;
use crate::backend::{BackendFuture, ScopeListQuery};
use crate::deadline;
use crate::error::ApiFailure;
use crate::routes::{Params, bad_request};
use crate::state::AppState;

/// `GET /v1/scopes?kind=`: every Scope, or one kind of them (§2.5).
pub async fn list(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<ScopeList>, ApiFailure> {
    let query = ScopeListQuery {
        kind: Params::new(&pairs).closed_set("kind", ScopeKind::KNOWN)?,
    };
    state.scopes.list(query).await.map(Json).map_err(Into::into)
}

/// `GET /v1/launchers`: the Launchers screen (§2.5, #22 `launcher list`).
pub async fn launchers(
    State(state): State<AppState>,
) -> Result<Json<LauncherOverview>, ApiFailure> {
    state.scopes.launchers().await.map(Json).map_err(Into::into)
}

/// `PUT /v1/scopes/{scope}/launcher`: sets the Scope's own Launcher (§2.5).
///
/// A `job:` Scope keeps its Launcher in the job's frontmatter, so it is `409 use_job_frontmatter`
/// here (D9). A path that cannot be run is the backend's `422 launcher_not_runnable`.
pub async fn set_launcher(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    body: Bytes,
) -> Response {
    let scope = match scope_key(&raw) {
        Ok(scope) => scope,
        Err(failure) => return failure.into_response(),
    };
    let result = async {
        let set: LauncherSet = json_object(&body, "LauncherSet")?;
        launcher_value(&set.launcher)?;
        refuse_job(&scope)?;
        Ok::<_, ApiFailure>(
            state
                .scopes
                .set_launcher(scope.clone(), set.launcher)
                .await?,
        )
    }
    .await;
    audited(&scope, result)
}

/// `DELETE /v1/scopes/{scope}/launcher`: the Scope inherits again (§2.5). `global` has nothing to
/// inherit from (`400 invalid_scope`: set `direct` instead), and a `job:` Scope's Launcher is its
/// frontmatter's.
pub async fn unset_launcher(State(state): State<AppState>, Path(raw): Path<String>) -> Response {
    let scope = match scope_key(&raw) {
        Ok(scope) => scope,
        Err(failure) => return failure.into_response(),
    };
    let result = async {
        if scope == Scope::Global {
            return Err(ApiFailure::from(
                ApiError::new(
                    ErrorCode::InvalidScope,
                    "global has no Launcher to inherit; set direct instead",
                )
                .with_hint("PUT the Launcher \"direct\" on the global Scope"),
            ));
        }
        refuse_job(&scope)?;
        Ok::<_, ApiFailure>(state.scopes.unset_launcher(scope.clone()).await?)
    }
    .await;
    audited(&scope, result)
}

/// `POST /v1/launchers/test`: runs the Test launcher and answers when it is done (§2.5, #40).
///
/// The test is handed to [`deadline::detach`] (§1.9), so a client that goes away leaves it
/// running for the requests that joined it; the seam decides whether a request starts a test or
/// joins one, and this route calls it exactly once. The server's backstop is `timeout_s` + 40 s
/// (`Deadline::Test`). It lives here, not in the deadline layer, because `timeout_s` is in the
/// body, which only this handler has read.
pub async fn test_launcher(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, ApiFailure> {
    let request = test_request(&body)?;
    let scope = request.scope.clone();
    let limit = Deadline::Test.server(request.call());
    let scopes = state.scopes.clone();
    let work: BackendFuture<'static, LauncherTestResult> =
        Box::pin(async move { scopes.test_launcher(request).await });
    let (task, mut guard) = deadline::detach(work);
    let settled = match limit {
        Some(limit) => tokio::time::timeout(limit, task).await,
        None => Ok(task.await),
    };
    guard.disarm();
    let result = match settled {
        Ok(Ok(result)) => result.map_err(ApiFailure::from),
        Ok(Err(_stopped)) => Err(ApiFailure::internal("the launcher test task stopped")),
        Err(_elapsed) => Err(ApiFailure::deadline_exceeded()),
    };
    Ok(audited(&scope, result))
}

/// `launchers.test`'s body: `{}` or nothing is the defaults (§2.5). An out-of-range `timeout_s`
/// is `422 invalid_value` and a malformed `scope` is `400 invalid_scope`, both before any test
/// starts.
fn test_request(body: &[u8]) -> Result<LauncherTestRequest, ApiFailure> {
    if body.is_empty() {
        return Ok(LauncherTestRequest::default());
    }
    let value: serde_json::Value = json_object(body, "LauncherTestRequest")?;
    if let Some(scope) = value.get("scope").and_then(serde_json::Value::as_str) {
        scope_key(scope)?;
    }
    if let Some(timeout) = value.get("timeout_s")
        && timeout.is_number()
    {
        let valid = timeout
            .as_i64()
            .and_then(|n| u32::try_from(n).ok())
            .is_some_and(|n| LauncherTestRequest::TIMEOUT_S.contains(&n));
        if !valid {
            return Err(invalid_timeout());
        }
    }
    let request: LauncherTestRequest = serde_json::from_value(value)
        .map_err(|_| bad_request("the body is not a LauncherTestRequest"))?;
    if !request.timeout_valid() {
        return Err(invalid_timeout());
    }
    if let Some(launcher) = &request.launcher {
        launcher_value(launcher)?;
    }
    Ok(request)
}

/// A body that must be a JSON object shaped like `T`: `400 bad_request` otherwise. serde reads a
/// short JSON array into a struct whose fields all have defaults, so the object check is the
/// route's, not serde's.
pub(super) fn json_object<T: serde::de::DeserializeOwned>(
    body: &[u8],
    what: &str,
) -> Result<T, ApiFailure> {
    let refused = || bad_request(format!("the body is not a {what}"));
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| refused())?;
    if !value.is_object() {
        return Err(refused());
    }
    serde_json::from_value(value).map_err(|_| refused())
}

fn invalid_timeout() -> ApiFailure {
    let range = LauncherTestRequest::TIMEOUT_S;
    ApiFailure::new(
        ErrorCode::InvalidValue,
        format!(
            "timeout_s must be between {} and {} seconds",
            range.start(),
            range.end()
        ),
    )
}

/// A Scope key from a path segment (§1.7): axum has already percent-decoded it, and a malformed
/// one is `400 invalid_scope`.
pub(super) fn scope_key(raw: &str) -> Result<Scope, ApiFailure> {
    raw.parse::<Scope>()
        .map_err(|e| ApiFailure::new(ErrorCode::InvalidScope, e.to_string()))
}

/// A Launcher value (§2.5): `"direct"` or an absolute path. Anything else is
/// `422 invalid_value`; whether the path can be run is the backend's `launcher_not_runnable`.
pub(super) fn launcher_value(launcher: &str) -> Result<(), ApiFailure> {
    let shaped = launcher == "direct"
        || (FsPath::new(launcher).is_absolute() && !launcher.chars().any(char::is_control));
    if shaped {
        Ok(())
    } else {
        Err(ApiFailure::new(
            ErrorCode::InvalidValue,
            "a Launcher is \"direct\" or an absolute path",
        ))
    }
}

/// A `job:` Scope's Launcher is its frontmatter's `launcher:` key, edited through `jobs.update`
/// (§2.4, §2.5): `409 use_job_frontmatter`.
fn refuse_job(scope: &Scope) -> Result<(), ApiFailure> {
    if matches!(scope, Scope::Job(_)) {
        return Err(ApiFailure::from(
            ApiError::new(
                ErrorCode::UseJobFrontmatter,
                "a job's Launcher is set in its frontmatter",
            )
            .with_hint("edit the job's launcher: key with jobs.update"),
        ));
    }
    Ok(())
}

/// The response for a Scope-centric call, with the Scope as the audit entry's `target` whether
/// the call succeeded or failed.
pub(super) fn audited<T: Serialize>(scope: &Scope, result: Result<T, ApiFailure>) -> Response {
    let mut response = match result {
        Ok(value) => Json(value).into_response(),
        Err(failure) => failure.into_response(),
    };
    response
        .extensions_mut()
        .insert(AuditTarget(Some(scope.to_string())));
    response
}
