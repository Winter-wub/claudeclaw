//! The Agents handlers (management-api §2.8): the list and archive reads, create, the persona
//! read and whole-file write, archive, restore and purge.
//!
//! An agent's name and an archive entry are directory names under `agents/` and
//! `agents/.archive/`, so [`agent_name`] and [`archive_entry`] refuse anything that could leave
//! them before a backend is called (`400 bad_request`; the spec's catalogue has no code of its
//! own for it). The persona write is a whole-file write: without an `If-Match` it is
//! `428 revision_required` here, and a stale one is the backend's `409 revision_mismatch` with
//! the current revision (§1.5). Every write names the agent or entry as its audit target, on a
//! refusal too, once the name has passed validation; the reads are never audited.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use serde::Serialize;

use claudeclaw_api::agents::{AgentCreate, AgentPersonaPut};
use claudeclaw_api::error::ApiError;

use crate::audit::AuditTarget;
use crate::error::ApiFailure;
use crate::routes::bad_request;
use crate::routes::jobs::{if_match, revision_required};
use crate::routes::scopes::json_object;
use crate::state::AppState;

/// The longest agent name: a directory name is at most 255 bytes, and archiving appends
/// `-<UTC stamp>` (17 bytes) to it.
const MAX_NAME_BYTES: usize = 200;
/// The longest archive entry: one directory name.
const MAX_ENTRY_BYTES: usize = 255;

/// Characters no directory name here may hold: path separators, and the ones Windows reserves,
/// so a name that is safe on one platform is safe on all of them.
const FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// `GET /v1/agents`: every agent (§2.8).
pub async fn list(State(state): State<AppState>) -> Result<Response, ApiFailure> {
    json(state.agents.list().await)
}

/// `GET /v1/archived-agents`: the entries of `agents/.archive/`, fired threads included (§2.8).
pub async fn archived(State(state): State<AppState>) -> Result<Response, ApiFailure> {
    json(state.agents.archived().await)
}

/// `POST /v1/agents`: a new agent, with its persona when the body carried one (§2.8) → `201`.
/// `409 agent_exists` when the name is taken.
pub async fn create(State(state): State<AppState>, body: Bytes) -> Result<Response, ApiFailure> {
    let create: AgentCreate = json_object(&body, "AgentCreate")?;
    let name = agent_name(&create.name)?.to_owned();
    let result = state.agents.create(create).await;
    let created = result
        .map(|info| (StatusCode::CREATED, Json(info)).into_response())
        .map_err(ApiFailure::from);
    Ok(audited(name, created))
}

/// `GET /v1/agents/{name}/persona`: the raw `agents/<name>/CLAUDE.md` and its revision (§2.8).
pub async fn persona_get(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, ApiFailure> {
    let name = agent_name(&name)?;
    json(state.agents.persona_get(name).await)
}

/// `PUT /v1/agents/{name}/persona`: the whole persona (§2.8), which §1.5 requires an `If-Match`
/// for.
pub async fn persona_put(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiFailure> {
    let put: AgentPersonaPut = json_object(&body, "AgentPersonaPut")?;
    let name = agent_name(&name)?.to_owned();
    let Some(expected) = if_match(&headers)? else {
        return Ok(audited(name, Err(revision_required())));
    };
    let result = state.agents.persona_put(&name, put.text, expected).await;
    Ok(audited(name, json(result)))
}

/// `POST /v1/agents/{name}/archive`: moves the agent, its jobs with it, to `agents/.archive/`
/// (§2.8, #16).
pub async fn archive(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, ApiFailure> {
    let name = agent_name(&name)?.to_owned();
    let result = state.agents.archive(&name).await;
    Ok(audited(name, json(result)))
}

/// `POST /v1/archived-agents/{entry}/restore`: an archived entry back under its old name
/// (§2.8). `409 agent_exists` when the name is taken again.
pub async fn restore(
    State(state): State<AppState>,
    Path(entry): Path<String>,
) -> Result<Response, ApiFailure> {
    let entry = archive_entry(&entry)?.to_owned();
    let result = state.agents.restore(&entry).await;
    Ok(audited(entry, json(result)))
}

/// `DELETE /v1/archived-agents/{entry}`: an archived entry, for good (§2.8, #16). Only an
/// entry under `agents/.archive/` can be named here.
pub async fn purge(
    State(state): State<AppState>,
    Path(entry): Path<String>,
) -> Result<Response, ApiFailure> {
    let entry = archive_entry(&entry)?.to_owned();
    let result = state.agents.purge(&entry).await;
    Ok(audited(entry, json(result)))
}

/// An agent name from a path or a body (§2.8): one directory name under `agents/`.
pub(crate) fn agent_name(raw: &str) -> Result<&str, ApiFailure> {
    dir_name(raw, MAX_NAME_BYTES, "an agent name")
}

/// An archive entry from a path (§2.8): one directory name under `agents/.archive/`.
pub(crate) fn archive_entry(raw: &str) -> Result<&str, ApiFailure> {
    dir_name(raw, MAX_ENTRY_BYTES, "an archive entry")
}

/// A single path component that stays inside the directory it is joined to: not empty, no
/// separator, no `..`, not hidden (so neither `.` nor `.archive`), no control character, no
/// character Windows reserves, and no leading or trailing space or trailing dot. The message
/// never echoes the value.
fn dir_name<'a>(raw: &'a str, max_bytes: usize, what: &str) -> Result<&'a str, ApiFailure> {
    let unsafe_name = raw.is_empty()
        || raw.len() > max_bytes
        || raw.starts_with('.')
        || raw.ends_with('.')
        || raw.contains("..")
        || raw.trim() != raw
        || raw
            .chars()
            .any(|c| c.is_control() || FORBIDDEN.contains(&c));
    if unsafe_name {
        return Err(bad_request(format!(
            "{what} must be a plain directory name: 1 to {max_bytes} bytes, no path separators, \
             control characters, or leading dot"
        )));
    }
    Ok(raw)
}

/// A backend result as a `200` JSON body.
fn json<T: Serialize>(result: Result<T, ApiError>) -> Result<Response, ApiFailure> {
    result
        .map(|value| Json(value).into_response())
        .map_err(Into::into)
}

/// Renders a write's outcome with its audit `target`, success or refusal (§2.9), so a failed
/// attempt on a named agent is audited against it too.
fn audited(target: String, result: Result<Response, ApiFailure>) -> Response {
    let mut response = result.unwrap_or_else(IntoResponse::into_response);
    response.extensions_mut().insert(AuditTarget(Some(target)));
    response
}
