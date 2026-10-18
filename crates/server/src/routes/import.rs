//! The First-start import handlers (management-api §2.11): `import.get`, `import.plan` and
//! `import.apply`.
//!
//! `import.plan` is a `GET` and writes nothing, so its route is never audited; `import.apply`
//! is audited like any mutation. `?only=` takes the steps §2.11 lists, `?job=` names a job, and
//! each may repeat to select several, the way the apply body's `only` and `jobs` do, so a
//! previewed `plan_hash` can be applied for exactly the selection it was made for. `409
//! plan_changed` comes from the backend when the hash no longer matches.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};

use claudeclaw_api::import::{
    ImportApplyRequest, ImportOnly, ImportPlan, ImportRecord, ImportResult,
};

use crate::backend::ImportPlanQuery;
use crate::error::ApiFailure;
use crate::routes::{Params, bad_request};
use crate::state::AppState;

/// `GET /v1/import`: `daemon/state/import.json` plus the last plan (§2.11).
pub async fn get(State(state): State<AppState>) -> Result<Json<ImportRecord>, ApiFailure> {
    state.import.get().await.map(Json).map_err(Into::into)
}

/// `GET /v1/import/plan?only=&job=`: what an import would do, writing nothing (§2.11).
pub async fn plan(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<ImportPlan>, ApiFailure> {
    let mut only = Vec::new();
    for (key, value) in &pairs {
        if key != "only" || value.is_empty() {
            continue;
        }
        let one = [(key.clone(), value.clone())];
        if let Some(step) = Params::new(&one).closed_set("only", ImportOnly::KNOWN)?
            && !only.contains(&step)
        {
            only.push(step);
        }
    }
    let mut jobs: Vec<String> = Vec::new();
    for (key, value) in &pairs {
        if key == "job" && !value.is_empty() && !jobs.contains(value) {
            jobs.push(value.clone());
        }
    }
    state
        .import
        .plan(ImportPlanQuery { only, jobs })
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `POST /v1/import/apply`: runs the selected steps (§2.11). Every field of the body is
/// optional, and an empty body applies every step to every job.
pub async fn apply(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<ImportResult>, ApiFailure> {
    let request = parse_apply(&body)?;
    state
        .import
        .apply(request)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// The apply body, validated: a step `ImportOnly` doesn't list, an empty `only` or `jobs`
/// (which would read as "everything" or as "nothing") and a blank job name are
/// `400 bad_request`.
fn parse_apply(body: &[u8]) -> Result<ImportApplyRequest, ApiFailure> {
    if body.is_empty() {
        return Ok(ImportApplyRequest::default());
    }
    let invalid = || bad_request("the body must be an import apply request");
    // A struct also deserializes from a JSON array, so `[]` must be refused as not an object.
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    let request: ImportApplyRequest = serde_json::from_value(value).map_err(|_| invalid())?;
    if let Some(only) = &request.only {
        if only.is_empty() {
            return Err(bad_request("only must name at least one step"));
        }
        if only.iter().any(|step| !ImportOnly::KNOWN.contains(step)) {
            return Err(bad_request("only names a step this route doesn't know"));
        }
    }
    if let Some(jobs) = &request.jobs {
        if jobs.is_empty() {
            return Err(bad_request("jobs must name at least one job"));
        }
        if jobs.iter().any(|job| job.trim().is_empty()) {
            return Err(bad_request("jobs must not hold a blank name"));
        }
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::error::ErrorCode;

    use super::*;

    #[test]
    fn an_apply_body_is_empty_or_a_valid_request() {
        assert_eq!(parse_apply(b"").unwrap(), ImportApplyRequest::default());
        assert_eq!(parse_apply(b"{}").unwrap(), ImportApplyRequest::default());
        let request = parse_apply(
            br#"{"only":["jobs","state"],"jobs":["researcher/paper-digest"],"plan_hash":"abc"}"#,
        )
        .unwrap();
        assert_eq!(
            request.only,
            Some(vec![ImportOnly::Jobs, ImportOnly::State])
        );
        assert_eq!(request.plan_hash.as_deref(), Some("abc"));
        for bad in [
            &br#"{"only":["inbox"]}"#[..],
            br#"{"only":[]}"#,
            br#"{"jobs":[]}"#,
            br#"{"jobs":[" "]}"#,
            br#"{"only":"jobs"}"#,
            b"[]",
            b"nope",
        ] {
            assert_eq!(
                parse_apply(bad).unwrap_err().code(),
                ErrorCode::BadRequest,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}
