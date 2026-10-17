//! The server's error plumbing (management-api §1.4).
//!
//! Every failure a handler or a layer produces leaves as an [`ApiFailure`], which renders the one
//! shared `api::ApiError` envelope and, on the way out, leaves the [`ErrorCode`] in the response's
//! extensions so the audit layer can record the failed mutation. A `503` also carries
//! `Retry-After`, as §1.8 requires.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::permission::TokenPermission;

/// An [`ApiError`] on its way out of a handler or layer.
///
/// The error is boxed so a `Result<T, ApiFailure>` stays cheap to move. Cloning shares nothing:
/// these are produced and rendered once.
#[derive(Debug, Clone)]
pub struct ApiFailure(pub Box<ApiError>);

impl ApiFailure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiFailure(Box::new(ApiError::new(code, message)))
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// A `[local]` route reached by anything but `Local` (§1.2).
    pub fn local_only() -> Self {
        Self::new(
            ErrorCode::LocalOnly,
            "this route is served on the local socket only",
        )
    }

    /// An `admin`-only route reached by an API token (§1.3).
    pub fn admin_required() -> Self {
        ApiFailure(Box::new(
            ApiError::new(
                ErrorCode::MissingPermission,
                "this route requires the admin principal",
            )
            .with_detail("required", "admin"),
        ))
    }

    /// A token without one of the permissions a route needs (§1.3). The first missing one is
    /// named in `error.details.required`.
    pub fn missing_permission(required: &TokenPermission) -> Self {
        ApiFailure(Box::new(
            ApiError::new(
                ErrorCode::MissingPermission,
                "the token lacks a required permission",
            )
            .with_detail("required", code_name_of_permission(required)),
        ))
    }

    /// The server's Deadline backstop (§1.9).
    pub fn deadline_exceeded() -> Self {
        Self::new(
            ErrorCode::DeadlineExceeded,
            "the request exceeded its deadline",
        )
    }

    /// A mutation while the Worker is starting (§1.8).
    pub fn daemon_starting() -> Self {
        Self::new(ErrorCode::DaemonStarting, "the daemon is starting")
    }

    /// A mutation while the Worker is stopping (§1.8).
    pub fn daemon_stopping() -> Self {
        Self::new(ErrorCode::DaemonStopping, "the daemon is stopping")
    }

    /// The wire code, e.g. for logging or an assertion.
    pub fn code(&self) -> ErrorCode {
        self.0.error.code
    }
}

impl From<ApiError> for ApiFailure {
    fn from(error: ApiError) -> Self {
        ApiFailure(Box::new(error))
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let code = self.0.error.code;
        let status =
            StatusCode::from_u16(self.0.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (status, Json(&self.0)).into_response();
        response.extensions_mut().insert(code);
        if status == StatusCode::SERVICE_UNAVAILABLE {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
        }
        response
    }
}

/// The wire name of an [`ErrorCode`], e.g. `deadline_exceeded`, without a per-code `match`.
pub fn code_name(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The wire name of a [`TokenPermission`], e.g. `status:read`.
fn code_name_of_permission(permission: &TokenPermission) -> String {
    serde_json::to_value(permission)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_envelope_renders_the_shared_shape() {
        let failure = ApiFailure::missing_permission(&TokenPermission::StatusRead);
        let response = failure.into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.extensions().get::<ErrorCode>(),
            Some(&ErrorCode::MissingPermission)
        );
    }

    #[test]
    fn a_503_carries_retry_after() {
        let response = ApiFailure::daemon_starting().into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "5");
    }

    #[test]
    fn code_names_are_snake_case() {
        assert_eq!(code_name(ErrorCode::DeadlineExceeded), "deadline_exceeded");
        assert_eq!(code_name(ErrorCode::LocalOnly), "local_only");
    }
}
