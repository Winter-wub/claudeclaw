//! The error envelope every route answers with (management-api §1.4), the same as the CLI's
//! `--json` errors (#22 §4).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{PartialSchema, ToSchema};

use crate::wire::string_enum;

/// `{"error": {"code", "message", "hint", "details"}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ApiError {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// Never echoes a Launcher's arguments, a secret or request content (#8, #23 D6).
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Code-specific fields, e.g. `current` for `revision_mismatch`.
    #[serde(default)]
    pub details: Map<String, Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiError {
            error: ErrorBody {
                code,
                message: message.into(),
                hint: None,
                details: Map::new(),
            },
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.error.hint = Some(hint.into());
        self
    }

    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.error.details.insert(key.into(), value.into());
        self
    }

    /// The HTTP status this error is sent with.
    pub fn status(&self) -> u16 {
        self.error.code.status()
    }
}

macro_rules! error_codes {
    ($($status:literal => [$($code:ident),* $(,)?],)*) => {
        /// `api::ErrorCode`: stable within v1, and the HTTP status each one is sent with.
        ///
        /// The CLI's own codes (`daemon_not_running`, `statusline_foreign`, …) are not here: the
        /// server never sends them (#22 §4, #24).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum ErrorCode {
            $($($code,)*)*
            /// A code added later in v1 that this build doesn't know (§1.1). Never sent, and
            /// not in the schema.
            #[serde(other)]
            Unknown,
        }

        impl ErrorCode {
            /// Every code the server sends, in catalogue order.
            pub const KNOWN: &[ErrorCode] = &[$($(ErrorCode::$code,)*)*];

            pub fn status(self) -> u16 {
                match self {
                    $($(ErrorCode::$code)|* => $status,)*
                    ErrorCode::Unknown => 500,
                }
            }
        }
    };
}

// The catalogue of management-api §1.4, by HTTP status.
error_codes! {
    400 => [BadRequest, InvalidScope, InvalidCursor],
    401 => [Unauthorized, UnknownDevice, DeviceRevoked, PairingCodeInvalid],
    403 => [MissingPermission, LocalOnly],
    404 => [NotFound, TranscriptNotFound],
    409 => [
        RevisionMismatch,
        JobExists,
        AgentExists,
        SessionInUse,
        RemoteDisabled,
        PlanChanged,
        UseJobFrontmatter,
        ExportBusy,
        AdapterUnavailable,
    ],
    410 => [PairingExpired],
    413 => [PayloadTooLarge],
    422 => [
        JobInvalid,
        LauncherNotRunnable,
        TsOnlyKey,
        UnknownKey,
        InvalidValue,
        InvalidPermissionSet,
        ExeCheckFailed,
        InvalidHost,
    ],
    428 => [RevisionRequired],
    500 => [Internal, ExportLeak],
    503 => [DaemonStarting, DaemonStopping],
    504 => [DeadlineExceeded],
    507 => [InsufficientStorage],
}

impl PartialSchema for ErrorCode {
    fn schema() -> RefOr<Schema> {
        string_enum(Self::KNOWN)
    }
}

impl ToSchema for ErrorCode {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn envelope_matches_the_spec_shape() {
        let error = ApiError::new(ErrorCode::RevisionMismatch, "changed on disk")
            .with_hint("reload and retry")
            .with_detail("current", "0123456789abcdef");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"error": {
                "code": "revision_mismatch",
                "message": "changed on disk",
                "hint": "reload and retry",
                "details": {"current": "0123456789abcdef"},
            }})
        );
        assert_eq!(error.status(), 409);
    }

    #[test]
    fn an_unknown_code_still_parses() {
        let error: ApiError =
            serde_json::from_value(json!({"error": {"code": "added_in_v1_9", "message": "m"}}))
                .unwrap();
        assert_eq!(error.error.code, ErrorCode::Unknown);
        assert!(error.error.details.is_empty());
    }

    #[test]
    fn codes_are_snake_case() {
        assert_eq!(
            serde_json::to_value(ErrorCode::UseJobFrontmatter).unwrap(),
            json!("use_job_frontmatter")
        );
        assert_eq!(ErrorCode::InsufficientStorage.status(), 507);
        assert_eq!(
            serde_json::to_value(ErrorCode::AdapterUnavailable).unwrap(),
            json!("adapter_unavailable")
        );
        assert_eq!(ErrorCode::AdapterUnavailable.status(), 409);
    }

    #[test]
    fn the_catalogue_round_trips_and_leaves_unknown_out() {
        assert_eq!(ErrorCode::KNOWN.len(), 37);
        for &code in ErrorCode::KNOWN {
            let name = serde_json::to_value(code).unwrap();
            assert_ne!(name, "unknown");
            assert_eq!(serde_json::from_value::<ErrorCode>(name).unwrap(), code);
        }
    }
}
