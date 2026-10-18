//! The Settings handlers (management-api §2.6): `settings.get` and `settings.update`.
//!
//! `settings.update` is a JSON Merge Patch over the file's own key names with an optional
//! `If-Match` (§1.5). Which keys exist, which are TS-only and which value is valid is the
//! backend's catalogue (it sits beside the file); the one rule the route owns is the caller's:
//! `remote.*` changes are `[local]`, so a Device can't turn the remote listener off under itself
//! or move it (§2.6), while the rest of its patch is allowed. Secrets are write-only: the body
//! is parsed here rather than by an extractor so no refusal can quote it back, and nothing
//! below logs it.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use serde_json::Value;

use claudeclaw_api::settings::{SettingsApplied, SettingsPatch, SettingsView};

use crate::auth::Principal;
use crate::error::ApiFailure;
use crate::routes::bad_request;
use crate::routes::jobs::if_match;
use crate::state::AppState;

/// `GET /v1/settings`: the typed view, its Revision and the TS-only and env-override keys
/// (§2.6). Secrets read as `{set}`.
pub async fn get(State(state): State<AppState>) -> Result<Json<SettingsView>, ApiFailure> {
    state.settings.get().await.map(Json).map_err(Into::into)
}

/// `PATCH /v1/settings`: a JSON Merge Patch with an optional `If-Match` (§2.6, §1.5).
pub async fn update(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<SettingsApplied>, ApiFailure> {
    let expected = if_match(&headers)?;
    let patch = parse_patch(&body)?;
    if touches_remote(&patch) && !matches!(principal, Principal::Local) {
        return Err(ApiFailure::local_only());
    }
    state
        .settings
        .update(patch, expected)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// The body as a Merge Patch object. The message never quotes the body: it can carry a secret.
fn parse_patch(body: &[u8]) -> Result<SettingsPatch, ApiFailure> {
    serde_json::from_slice(body).map_err(|_| bad_request("the body must be a JSON object"))
}

/// Whether the patch writes any `remote.*` key (§2.6): a non-empty `remote` object, or a
/// non-object `remote` (`null` removes the whole table).
fn touches_remote(patch: &SettingsPatch) -> bool {
    match patch.0.get("remote") {
        None => false,
        Some(Value::Object(keys)) => !keys.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::error::ErrorCode;
    use serde_json::json;

    use super::*;

    fn patch(value: Value) -> SettingsPatch {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_patch_is_a_json_object_and_a_refusal_never_quotes_it() {
        assert!(parse_patch(br#"{"model":"opus"}"#).is_ok());
        assert!(parse_patch(b"{}").is_ok());
        for bad in [&b""[..], b"[]", b"\"hunter2\"", b"not json", b"7"] {
            let failure = parse_patch(bad).unwrap_err();
            assert_eq!(failure.code(), ErrorCode::BadRequest);
            assert!(!format!("{failure:?}").contains("hunter2"));
        }
    }

    #[test]
    fn only_a_write_to_a_remote_key_is_local_only() {
        assert!(touches_remote(&patch(json!({"remote": {"port": 4700}}))));
        assert!(touches_remote(&patch(json!({"remote": {"enabled": null}}))));
        assert!(touches_remote(&patch(json!({"remote": null}))));
        assert!(touches_remote(&patch(json!({"remote": "off"}))));
        assert!(!touches_remote(&patch(json!({"remote": {}}))));
        assert!(!touches_remote(&patch(json!({"model": "opus"}))));
        assert!(!touches_remote(&patch(json!({"discord": {"remote": 1}}))));
    }
}
