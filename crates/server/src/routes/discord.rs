//! The Discord handlers (management-api §2.7): the overview and the channel picker, listening
//! in a channel or stopping, and hiring into and firing a thread.
//!
//! Routes parse and validate; the adapter's transport calls and core's session and `agents/`
//! work are the backend's. Path ids are Discord snowflakes and are checked for shape before the
//! backend is called (`400 bad_request`); whether they name a channel or thread is the backend's
//! `404 not_found`. Bodies are parsed here rather than by an extractor so a refusal is §1.4's
//! envelope. A write names the channel or thread in the audit entry's `target` (§2.9), whether
//! it succeeded or not, but only once the id has passed the shape check.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use claudeclaw_api::discord::{
    AvailableChannelList, ChannelTrigger, DiscordChannelPatch, DiscordOverview, HireRequest,
};
use claudeclaw_api::error::ApiError;

use crate::audit::AuditTarget;
use crate::error::ApiFailure;
use crate::routes::bad_request;
use crate::routes::jobs::run_body;
use crate::state::AppState;

/// A snowflake is a u64, so at most 20 decimal digits.
const MAX_SNOWFLAKE_DIGITS: usize = 20;

/// Discord's limit on a thread's name, in characters.
const MAX_THREAD_NAME_CHARS: usize = 100;

/// `discord.hire` creates at most this many threads per call. A server convention: the spec
/// fixes none, and a person hires a handful at once.
const MAX_HIRE_NAMES: usize = 25;

/// `GET /v1/discord`: the Discord screen (§2.7). Not audited; answers with `state: disabled` and
/// empty lists while the adapter is off.
pub async fn get(State(state): State<AppState>) -> Result<Json<DiscordOverview>, ApiFailure> {
    state.discord.get().await.map(Json).map_err(Into::into)
}

/// `GET /v1/discord/available-channels`: the channels the bot could listen in, from the gateway
/// cache (§2.7).
pub async fn available(
    State(state): State<AppState>,
) -> Result<Json<AvailableChannelList>, ApiFailure> {
    state
        .discord
        .available()
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `PUT /v1/discord/channels/{channel_id}`: listen in a channel, or change how (§2.7).
pub async fn update_channel(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiFailure> {
    let channel_id = snowflake("channel_id", channel_id)?;
    let patch = parse_patch(&body)?;
    let result = state.discord.update(&channel_id, patch).await;
    Ok(written(channel_id, result))
}

/// `DELETE /v1/discord/channels/{channel_id}`: stop listening (§2.7). Sessions are untouched.
pub async fn remove_channel(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
) -> Result<Response, ApiFailure> {
    let channel_id = snowflake("channel_id", channel_id)?;
    let result = state.discord.remove(&channel_id).await;
    Ok(written(channel_id, result))
}

/// `POST /v1/discord/channels/{channel_id}/hire`: one public thread per name (§2.7).
pub async fn hire(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiFailure> {
    let channel_id = snowflake("channel_id", channel_id)?;
    let names = parse_hire(&body)?;
    let result = state.discord.hire(&channel_id, names).await;
    Ok(written(channel_id, result))
}

/// `POST /v1/discord/threads/{thread_id}/fire`: delete the thread and archive its agent dir
/// (§2.7, #16). The body is `{}`; an empty one is accepted too.
pub async fn fire(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiFailure> {
    let thread_id = snowflake("thread_id", thread_id)?;
    run_body(&body)?;
    let result = state.discord.fire(&thread_id).await;
    Ok(written(thread_id, result))
}

/// The backend's answer to a write as a response, with the id the write was about as the audit
/// entry's `target` on success and on failure alike.
fn written<T: Serialize>(target: String, result: Result<T, ApiError>) -> Response {
    let mut response = match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => ApiFailure::from(error).into_response(),
    };
    response.extensions_mut().insert(AuditTarget(Some(target)));
    response
}

/// A Discord snowflake as it is sent: 1..=20 ASCII digits that fit a u64 (`400 bad_request`
/// otherwise). The message never quotes the value.
fn snowflake(what: &str, raw: String) -> Result<String, ApiFailure> {
    if is_snowflake(&raw) {
        Ok(raw)
    } else {
        Err(bad_request(format!("{what} must be a Discord snowflake")))
    }
}

fn is_snowflake(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= MAX_SNOWFLAKE_DIGITS
        && raw.bytes().all(|byte| byte.is_ascii_digit())
        && raw.parse::<u64>().is_ok()
}

/// `discord.channels.update`'s body (§2.7): a known `trigger`, and an `allowed_user_ids` that is
/// `null` (the global list) or a list of snowflakes.
fn parse_patch(body: &[u8]) -> Result<DiscordChannelPatch, ApiFailure> {
    const EXPECTED: &str = "the body must be {\"trigger\", \"allowed_user_ids\"}";
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| bad_request(EXPECTED))?;
    // Both keys are required: leaving `allowed_user_ids` out must not quietly reset a channel's
    // own allow-list to the global one; only an explicit `null` does that.
    if value.get("allowed_user_ids").is_none() {
        return Err(bad_request(EXPECTED));
    }
    let patch: DiscordChannelPatch =
        serde_json::from_value(value).map_err(|_| bad_request(EXPECTED))?;
    if !ChannelTrigger::KNOWN.contains(&patch.trigger) {
        return Err(bad_request(
            "trigger must be mention or every_message".to_owned(),
        ));
    }
    if let Some(ids) = &patch.allowed_user_ids
        && !ids.iter().all(|id| is_snowflake(id))
    {
        return Err(bad_request(
            "allowed_user_ids must be Discord snowflakes".to_owned(),
        ));
    }
    Ok(patch)
}

/// `discord.hire`'s body (§2.7): 1..=[`MAX_HIRE_NAMES`] thread names, each trimmed and 1..=100
/// characters without control characters. Returns the trimmed names.
fn parse_hire(body: &[u8]) -> Result<Vec<String>, ApiFailure> {
    let request: HireRequest = serde_json::from_slice(body)
        .map_err(|_| bad_request("the body must be {\"names\": [string]}"))?;
    if request.names.is_empty() || request.names.len() > MAX_HIRE_NAMES {
        return Err(bad_request(format!(
            "names must list between 1 and {MAX_HIRE_NAMES} thread names"
        )));
    }
    request
        .names
        .iter()
        .map(|name| {
            let name = name.trim();
            let length = name.chars().count();
            if length == 0 || length > MAX_THREAD_NAME_CHARS || name.chars().any(char::is_control) {
                Err(bad_request(format!(
                    "a thread name must be 1 to {MAX_THREAD_NAME_CHARS} characters without \
                     control characters"
                )))
            } else {
                Ok(name.to_owned())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::error::ErrorCode;

    use super::*;

    #[test]
    fn a_snowflake_is_up_to_twenty_digits_that_fit_a_u64() {
        let largest = u64::MAX.to_string();
        let past_largest = format!("{largest}0");
        let one_over = (u128::from(u64::MAX) + 1).to_string();
        for good in ["1", "1001", largest.as_str()] {
            assert!(is_snowflake(good), "{good}");
        }
        for bad in [
            past_largest.as_str(),
            one_over.as_str(),
            "",
            "abc",
            "12a",
            "+12",
            "-12",
            " 12",
            "1.5",
            "١٢٣",
            "../1",
        ] {
            assert!(!is_snowflake(bad), "{bad:?}");
        }
        let failure = snowflake("channel_id", "nope".into()).unwrap_err();
        assert_eq!(failure.code(), ErrorCode::BadRequest);
        assert!(!format!("{failure:?}").contains("nope"));
    }

    #[test]
    fn a_patch_takes_a_known_trigger_and_snowflake_users() {
        let ok = parse_patch(br#"{"trigger":"mention","allowed_user_ids":null}"#).unwrap();
        assert_eq!(ok.trigger, ChannelTrigger::Mention);
        assert!(parse_patch(br#"{"trigger":"every_message","allowed_user_ids":["12"]}"#).is_ok());
        for bad in [
            &br#"{"trigger":"reaction","allowed_user_ids":null}"#[..],
            br#"{"trigger":"mention","allowed_user_ids":["x"]}"#,
            br#"{"trigger":"mention"}"#,
            b"[]",
            b"",
        ] {
            assert_eq!(parse_patch(bad).unwrap_err().code(), ErrorCode::BadRequest);
        }
    }

    #[test]
    fn hire_names_are_counted_trimmed_and_bounded() {
        assert_eq!(
            parse_hire(br#"{"names":["  paper-digest ","notes"]}"#).unwrap(),
            ["paper-digest", "notes"]
        );
        let long = "a".repeat(MAX_THREAD_NAME_CHARS + 1);
        let many = vec!["x"; MAX_HIRE_NAMES + 1];
        for bad in [
            r#"{"names":[]}"#.to_owned(),
            r#"{"names":["  "]}"#.to_owned(),
            r#"{"names":["a\nb"]}"#.to_owned(),
            format!(r#"{{"names":["{long}"]}}"#),
            serde_json::json!({"names": many}).to_string(),
            r#"{"names":"one"}"#.to_owned(),
            "{}".to_owned(),
        ] {
            assert_eq!(
                parse_hire(bad.as_bytes()).unwrap_err().code(),
                ErrorCode::BadRequest,
                "{bad}"
            );
        }
        let edge = "é".repeat(MAX_THREAD_NAME_CHARS);
        assert!(parse_hire(format!(r#"{{"names":["{edge}"]}}"#).as_bytes()).is_ok());
    }
}
