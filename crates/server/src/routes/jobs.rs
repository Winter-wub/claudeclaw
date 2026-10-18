//! The Jobs handlers (management-api §2.4): the list and record reads, the editor's create,
//! update and raw replace, delete, `jobs.run`, and the schedule preview, check and audit.
//!
//! Every write carries the revision the request's `If-Match` named (§1.5): a whole-file write
//! without one is `428 revision_required` here, and a stale one is the backend's
//! `409 revision_mismatch` with the current revision, never a silent lost update. `jobs.preview`
//! writes nothing, so the route is never audited (§2.4).

use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::header::IF_MATCH;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use claudeclaw_api::error::ErrorCode;
use claudeclaw_api::jobs::{
    JobCreate, JobDeleted, JobDetail, JobList, JobPatch, JobPreview, JobPreviewRequest, JobRaw,
    JobsAudit, JobsCheck,
};
use claudeclaw_api::revision::Revision;

use crate::auth::Principal;
use crate::backend::jobs::{JobAuditQuery, JobCheckQuery, JobListQuery};
use crate::error::ApiFailure;
use crate::routes::turns;
use crate::routes::{Params, bad_request, number};
use crate::state::AppState;

/// §2.4: `?next=` defaults to 5 and must count at least one fire.
const DEFAULT_CHECK_NEXT: u32 = 5;
/// §2.4: `?late=` defaults to the CLI's 2 minutes.
const DEFAULT_LATE_S: u64 = 120;

/// `GET /v1/jobs?agent=`: the whole list from the Change scan's snapshot (§2.4, #46 D5),
/// filtered by `?agent=` when it named one.
pub async fn list(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<JobList>, ApiFailure> {
    let params = Params::new(&pairs);
    let query = JobListQuery {
        agent: params.get("agent").map(str::to_owned),
    };
    state.jobs.list(query).await.map(Json).map_err(Into::into)
}

/// `GET /v1/jobs/{job}`: one job read fresh from disk (§2.4), `404 not_found` when there is no
/// such job.
pub async fn get(
    State(state): State<AppState>,
    Path(job): Path<String>,
) -> Result<Json<JobDetail>, ApiFailure> {
    state.jobs.get(&job).await.map(Json).map_err(Into::into)
}

/// `POST /v1/jobs`: a new job from the editor's form or raw text (§2.4) → `201`.
/// `409 job_exists` when the name is taken, `422 job_invalid` when the file doesn't load.
pub async fn create(
    State(state): State<AppState>,
    Json(create): Json<JobCreate>,
) -> Result<Response, ApiFailure> {
    let detail = state.jobs.create(create).await.map_err(ApiFailure::from)?;
    Ok((StatusCode::CREATED, Json(detail)).into_response())
}

/// `PATCH /v1/jobs/{job}`: line edits over a fresh read (§2.4, #12), with an optional
/// `If-Match` (§1.5) the backend checks against the file it read.
pub async fn update(
    State(state): State<AppState>,
    Path(job): Path<String>,
    headers: HeaderMap,
    Json(patch): Json<JobPatch>,
) -> Result<Json<JobDetail>, ApiFailure> {
    let expected = if_match(&headers)?;
    state
        .jobs
        .update(&job, patch, expected)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `PUT /v1/jobs/{job}/raw`: the whole file (§2.4), which §1.5 requires an `If-Match` for.
pub async fn replace(
    State(state): State<AppState>,
    Path(job): Path<String>,
    headers: HeaderMap,
    Json(raw): Json<JobRaw>,
) -> Result<Json<JobDetail>, ApiFailure> {
    let expected = if_match(&headers)?.ok_or_else(revision_required)?;
    state
        .jobs
        .replace(&job, raw, expected)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `DELETE /v1/jobs/{job}`: the file and everything keyed to it (§2.4, #12 §6), with an
/// optional `If-Match` (§1.5).
pub async fn delete(
    State(state): State<AppState>,
    Path(job): Path<String>,
    headers: HeaderMap,
) -> Result<Json<JobDeleted>, ApiFailure> {
    let expected = if_match(&headers)?;
    state
        .jobs
        .delete(&job, expected)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `POST /v1/jobs/{job}/run?wait=<s>`: starts the job's Turn and answers at once (§2.4,
/// §1.9). The Turn is owned by the principal that asked for it, so a token holding `jobs:run`
/// can read the Turn it started (§1.3).
pub async fn run(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(job): Path<String>,
    Query(pairs): Query<Vec<(String, String)>>,
    body: Bytes,
) -> Result<Response, ApiFailure> {
    run_body(&body)?;
    let wait = turns::wait_seconds(&pairs);
    let owner = principal.audit_name();
    let target = job.clone();
    let jobs = state.jobs.clone();
    let work: crate::backend::BackendFuture<'static, crate::backend::TurnHandle> =
        Box::pin(async move { jobs.run(&job, owner).await });
    turns::start(work, wait, Some(target)).await
}

/// `POST /v1/schedule/preview`: the server-side preview (§2.4, #12, #50). Writes nothing, so
/// the route carries `Audit::Never` and this handler adds no opt-out of its own.
pub async fn preview(
    State(state): State<AppState>,
    Json(request): Json<JobPreviewRequest>,
) -> Result<Json<JobPreview>, ApiFailure> {
    state
        .jobs
        .preview(request)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `GET /v1/schedule/check?job=&next=5`: every job read from disk, never written (§2.4).
pub async fn check(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<JobsCheck>, ApiFailure> {
    let params = Params::new(&pairs);
    let next = match params.get("next") {
        Some(raw) => {
            let next = number::<u32>("next", raw)?;
            if next < 1 {
                return Err(bad_request("next must be at least 1"));
            }
            next
        }
        None => DEFAULT_CHECK_NEXT,
    };
    let query = JobCheckQuery {
        job: params.get("job").map(str::to_owned),
        next,
    };
    state.jobs.check(query).await.map(Json).map_err(Into::into)
}

/// `GET /v1/schedule/audit?since=&until=&late=`: each schedule's expected fires against the Run
/// index (§2.4, #22, #44).
pub async fn audit(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<JobsAudit>, ApiFailure> {
    let params = Params::new(&pairs);
    let since = params.timestamp("since")?;
    let until = params.timestamp("until")?;
    if let (Some(since), Some(until)) = (since, until)
        && since > until
    {
        return Err(bad_request("since must not be after until"));
    }
    let late = match params.get("late") {
        Some(raw) => Duration::from_secs(number::<u64>("late", raw)?),
        None => Duration::from_secs(DEFAULT_LATE_S),
    };
    let query = JobAuditQuery { since, until, late };
    state.jobs.audit(query).await.map(Json).map_err(Into::into)
}

/// `jobs.run`'s body: `{}` (§2.4). An empty body is accepted too, so a bare `POST` works;
/// anything that is not a JSON object is `400 bad_request`.
fn run_body(body: &[u8]) -> Result<(), ApiFailure> {
    if body.is_empty() {
        return Ok(());
    }
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| bad_request("the body must be {}"))?;
    if !value.is_object() {
        return Err(bad_request("the body must be {}"));
    }
    Ok(())
}

/// The request's `If-Match` (§1.5): absent is `None`, and anything that is not `Revision`'s own
/// shape is `400 bad_request`, never a silently ignored header.
fn if_match(headers: &HeaderMap) -> Result<Option<Revision>, ApiFailure> {
    const EXPECTED: &str = "If-Match must be 16 lowercase hex digits";
    let Some(value) = headers.get(IF_MATCH) else {
        return Ok(None);
    };
    let raw = value.to_str().map_err(|_| bad_request(EXPECTED))?;
    raw.trim()
        .parse::<Revision>()
        .map(Some)
        .map_err(|_| bad_request(EXPECTED))
}

/// A whole-file write without `If-Match` (§1.5): `428 revision_required`.
fn revision_required() -> ApiFailure {
    ApiFailure::new(
        ErrorCode::RevisionRequired,
        "a whole-file write needs the If-Match revision it replaces",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(raw: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(raw) = raw {
            headers.insert(IF_MATCH, raw.parse().unwrap());
        }
        headers
    }

    #[test]
    fn if_match_is_absent_or_a_revision_or_refused() {
        assert_eq!(if_match(&headers(None)).unwrap(), None);
        let revision = Revision::of(b"---\n---\n");
        assert_eq!(
            if_match(&headers(Some(&revision.to_string()))).unwrap(),
            Some(revision)
        );
        for bad in [
            "",
            "xyz",
            "E3B0C44298FC1C14",
            "e3b0c44298fc1c1",
            "0123456789abcdef0",
        ] {
            assert_eq!(
                if_match(&headers(Some(bad))).unwrap_err().code(),
                ErrorCode::BadRequest,
                "{bad}"
            );
        }
        assert_eq!(revision_required().code(), ErrorCode::RevisionRequired);
    }

    #[test]
    fn a_run_body_is_an_object_or_nothing() {
        assert!(run_body(b"").is_ok());
        assert!(run_body(b"{}").is_ok());
        assert_eq!(run_body(b"[]").unwrap_err().code(), ErrorCode::BadRequest);
        assert_eq!(
            run_body(b"not json").unwrap_err().code(),
            ErrorCode::BadRequest
        );
    }
}
