//! The Sessions handlers (management-api §2.5): `sessions.list`, `sessions.get`,
//! `sessions.messages`, `sessions.reset`, `sessions.compact` and `sessions.delete`.
//!
//! Reads are addressed by session id; the writes are Scope-centric (D9), so their path carries
//! the Scope key (§1.7). `sessions.compact` is a Turn: it shares `turns.create`'s `202` /
//! `200 ?wait=` exchange and its [`deadline::detach`] hand-off (§1.9), and progress is the
//! existing `turns.events`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Response};

use claudeclaw_api::error::ErrorCode;
use claudeclaw_api::sessions::{
    SessionDeleted, SessionDetail, SessionList, SessionReset, SessionResetRequest, SessionStatus,
    TranscriptPage,
};

use crate::auth::Principal;
use crate::backend::{BackendFuture, SessionListQuery, TranscriptQuery, TurnHandle};
use crate::error::ApiFailure;
use crate::routes::jobs::run_body;
use crate::routes::scopes::{audited, json_object, launcher_value, scope_key};
use crate::routes::{Params, bad_request, turns};
use crate::state::AppState;

/// The longest session id a path may carry. Claude Code's are UUIDs; the bound only keeps a
/// hostile path segment out of the backend's file lookups.
const MAX_SESSION_ID: usize = 128;

/// `GET /v1/sessions?scope=&status=`: every (Scope, Launcher) session matching the filters
/// (§2.5).
pub async fn list(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<SessionList>, ApiFailure> {
    let params = Params::new(&pairs);
    let query = SessionListQuery {
        scope: params.scope("scope")?,
        status: params.closed_set("status", SessionStatus::KNOWN)?,
    };
    state
        .sessions
        .list(query)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `GET /v1/sessions/{session_id}`: one session plus its context meter (§2.5, #39 D4).
pub async fn get(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionDetail>, ApiFailure> {
    session_id_key(&session_id)?;
    state
        .sessions
        .get(&session_id)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `GET /v1/sessions/{session_id}/messages?limit=&before=`: one page of the Transcript, newest
/// first (§1.6, §2.5). A Transcript that cannot be read is the backend's
/// `404 transcript_not_found` with `details.reason`, passed through as it is — never an empty
/// page.
pub async fn messages(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<TranscriptPage>, ApiFailure> {
    session_id_key(&session_id)?;
    let params = Params::new(&pairs);
    let query = TranscriptQuery {
        limit: params.page_limit()?,
        before: params.get("before").map(str::to_owned),
    };
    state
        .sessions
        .messages(&session_id, query)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `POST /v1/scopes/{scope}/sessions/reset`: clears the Scope's session and keeps a backup
/// (§2.5). The body is `{launcher?}`, and an empty body means the current Launcher.
pub async fn reset(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    body: Bytes,
) -> Response {
    let scope = match scope_key(&raw) {
        Ok(scope) => scope,
        Err(failure) => return failure.into_response(),
    };
    let result = async {
        let request = reset_request(&body)?;
        Ok::<SessionReset, ApiFailure>(state.sessions.reset(scope.clone(), request).await?)
    }
    .await;
    audited(&scope, result)
}

/// `POST /v1/scopes/{scope}/sessions/compact?wait=<s>`: queues a compact on the Scope as a Turn
/// and answers at once (§2.5, §1.9), or with the `TurnRecord` when `?wait=` held the request
/// until it finished (§1.6).
pub async fn compact(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(raw): Path<String>,
    Query(pairs): Query<Vec<(String, String)>>,
    body: Bytes,
) -> Result<Response, ApiFailure> {
    let scope = scope_key(&raw)?;
    run_body(&body)?;
    let wait = turns::wait_seconds(&pairs);
    let owner = principal.audit_name();
    let target = scope.to_string();
    let sessions = state.sessions.clone();
    let work: BackendFuture<'static, TurnHandle> =
        Box::pin(async move { sessions.compact(scope, owner).await });
    turns::start(work, wait, Some(target)).await
}

/// `DELETE /v1/scopes/{scope}/sessions?launcher=`: deletes a Parked or orphaned session (§2.5).
/// `409 session_in_use` when it is the Scope's `in_use` one: reset it instead.
pub async fn delete(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let scope = match scope_key(&raw) {
        Ok(scope) => scope,
        Err(failure) => return failure.into_response(),
    };
    let result = async {
        let launcher = Params::new(&pairs).get("launcher").map(str::to_owned);
        if let Some(launcher) = &launcher {
            launcher_value(launcher)
                .map_err(|_| bad_request("launcher must be \"direct\" or an absolute path"))?;
        }
        Ok::<SessionDeleted, ApiFailure>(state.sessions.delete(scope.clone(), launcher).await?)
    }
    .await;
    audited(&scope, result)
}

/// `sessions.reset`'s body: `{launcher?}`, or nothing for the current Launcher (§2.5).
fn reset_request(body: &[u8]) -> Result<SessionResetRequest, ApiFailure> {
    if body.is_empty() {
        return Ok(SessionResetRequest::default());
    }
    let request: SessionResetRequest = json_object(body, "SessionResetRequest")?;
    if let Some(launcher) = &request.launcher {
        launcher_value(launcher)?;
    }
    Ok(request)
}

/// A session id from a path (§1.7): letters, digits, `-` and `_` only. Nothing else can name a
/// session, so anything else is `404 not_found` without asking the backend, and an id can never
/// carry a path separator into a Transcript lookup.
fn session_id_key(session_id: &str) -> Result<(), ApiFailure> {
    let shaped = !session_id.is_empty()
        && session_id.len() <= MAX_SESSION_ID
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if shaped {
        Ok(())
    } else {
        Err(ApiFailure::new(ErrorCode::NotFound, "no such session"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_id_is_a_plain_token() {
        for good in ["s-1", "0192f3a4-7c1e-7e2a-9b8d-0123456789ab", "A_b-9"] {
            assert!(session_id_key(good).is_ok(), "{good}");
        }
        let long = "a".repeat(MAX_SESSION_ID + 1);
        for bad in [
            "",
            "..",
            "a/b",
            "a\\b",
            "a.jsonl",
            "a b",
            "é",
            long.as_str(),
        ] {
            assert_eq!(
                session_id_key(bad).unwrap_err().code(),
                ErrorCode::NotFound,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_reset_body_is_a_launcher_or_nothing() {
        assert_eq!(reset_request(b"").unwrap(), SessionResetRequest::default());
        assert_eq!(
            reset_request(b"{}").unwrap(),
            SessionResetRequest::default()
        );
        assert_eq!(
            reset_request(br#"{"launcher":"direct"}"#)
                .unwrap()
                .launcher
                .as_deref(),
            Some("direct")
        );
        assert_eq!(
            reset_request(br#"{"launcher":"relative/path"}"#)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidValue
        );
        assert_eq!(
            reset_request(b"[]").unwrap_err().code(),
            ErrorCode::BadRequest
        );
    }
}
