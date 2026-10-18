//! The Run-index handlers (management-api §2.9): `runs.list`, `runs.get` and `runs.log`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};

use claudeclaw_api::runs::{RunOrigin, RunOutcome, RunPage, RunRecord};

use crate::backend::runs::RunQuery;
use crate::error::ApiFailure;
use crate::routes::{Params, bad_request, number};
use crate::state::AppState;

/// `GET /v1/runs`: one page, running runs first and then the index newest first (§2.9). The
/// page carries the Run index Back-fill's progress, so partial history is labelled (#44 D2).
pub async fn list(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<RunPage>, ApiFailure> {
    let params = Params::new(&pairs);
    let query = RunQuery {
        job: params.get("job").map(str::to_owned),
        scope: params.scope("scope")?,
        origin: params.closed_set("origin", RunOrigin::KNOWN)?,
        outcome: params.closed_set("outcome", RunOutcome::KNOWN)?,
        before: params.get("before").map(str::to_owned),
        limit: params.page_limit()?,
    };
    state.runs.list(query).await.map(Json).map_err(Into::into)
}

/// `GET /v1/runs/{run_id}`: one record (§2.9), `404 not_found` when the index has no such run.
pub async fn get(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunRecord>, ApiFailure> {
    state.runs.get(&run_id).await.map(Json).map_err(Into::into)
}

/// `GET /v1/runs/{run_id}/log`: the Run log as `text/markdown`, known secrets already scrubbed
/// by the backend (§2.9). It needs `runs:content` as well as `logs:read` (§1.3).
pub async fn log(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Response, ApiFailure> {
    let params = Params::new(&pairs);
    let tail_lines = match params.get("tail_lines") {
        Some(raw) => {
            let tail_lines = number::<u32>("tail_lines", raw)?;
            if tail_lines == 0 {
                return Err(bad_request("tail_lines must be at least 1"));
            }
            Some(tail_lines)
        }
        None => None,
    };
    let text = state.runs.log(&run_id, tail_lines).await?;
    Ok(([(header::CONTENT_TYPE, "text/markdown")], text).into_response())
}
