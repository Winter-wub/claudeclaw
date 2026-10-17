//! Daemon and status handlers (management-api §2.1): `health.get`, `status.get`,
//! `openapi.get`, the daemon control routes, `update.*` and `housekeeping.run`.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use claudeclaw_api::status::{
    DaemonStatus, Health, HousekeepingReport, HousekeepingRequest, ReexecRequest, UpdateInfo,
};

use crate::audit::NotAudited;
use crate::error::ApiFailure;
use crate::state::AppState;

pub async fn health(State(state): State<AppState>) -> Result<Json<Health>, ApiFailure> {
    state.status.health().await.map(Json).map_err(Into::into)
}

pub async fn status(State(state): State<AppState>) -> Result<Json<DaemonStatus>, ApiFailure> {
    state.status.status().await.map(Json).map_err(Into::into)
}

/// The committed OpenAPI document, straight from `claudeclaw_api` (management-api §2.1).
pub async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(claudeclaw_api::openapi())
}

pub async fn stop(State(state): State<AppState>) -> Result<Response, ApiFailure> {
    let accepted = state.daemon.stop().await.map_err(ApiFailure::from)?;
    Ok(accepted_response(accepted))
}

pub async fn restart(State(state): State<AppState>) -> Result<Response, ApiFailure> {
    let accepted = state.daemon.restart().await.map_err(ApiFailure::from)?;
    Ok(accepted_response(accepted))
}

pub async fn reexec(
    State(state): State<AppState>,
    Json(request): Json<ReexecRequest>,
) -> Result<Response, ApiFailure> {
    let accepted = state
        .daemon
        .reexec(request)
        .await
        .map_err(ApiFailure::from)?;
    Ok(accepted_response(accepted))
}

pub async fn update(State(state): State<AppState>) -> Result<Json<UpdateInfo>, ApiFailure> {
    state.status.update().await.map(Json).map_err(Into::into)
}

pub async fn check_update(State(state): State<AppState>) -> Result<Json<UpdateInfo>, ApiFailure> {
    state
        .daemon
        .check_update()
        .await
        .map(Json)
        .map_err(Into::into)
}

pub async fn housekeeping(
    State(state): State<AppState>,
    Json(request): Json<HousekeepingRequest>,
) -> Result<Response, ApiFailure> {
    let dry_run = request.dry_run;
    let report: HousekeepingReport = state
        .daemon
        .housekeeping(request)
        .await
        .map_err(ApiFailure::from)?;
    let mut response = Json(report).into_response();
    if dry_run {
        response.extensions_mut().insert(NotAudited);
    }
    Ok(response)
}

fn accepted_response<T: serde::Serialize>(body: T) -> Response {
    (StatusCode::ACCEPTED, Json(body)).into_response()
}
