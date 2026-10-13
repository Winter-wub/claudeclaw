//! Token permissions (management-api §1.3): the final v1 set, in colon form (#22).

use serde::{Deserialize, Serialize};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{PartialSchema, ToSchema};

use crate::error::string_enum;

/// What an API token may do beyond `any` routes. Everything not granted here is `admin` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TokenPermission {
    /// `turns.create` into the `global` Scope only, plus reading Turns this token created. The
    /// legacy `apiToken` is imported with exactly this (#17 §3).
    #[serde(rename = "send")]
    Send,
    /// `jobs.run`, plus reading Turns this token created.
    #[serde(rename = "jobs:run")]
    JobsRun,
    /// Content-free reads: health, status, events, job lists and checks, update info.
    #[serde(rename = "status:read")]
    StatusRead,
    /// Daemon logs, the Bug-report export, the Run index and the audit log (#23 D5, #28).
    #[serde(rename = "logs:read")]
    LogsRead,
    /// Run log bodies. Only granted together with `logs:read`.
    #[serde(rename = "runs:content")]
    RunsContent,
    /// A permission added later in v1 that this build doesn't know (§1.1). Never granted, and
    /// not in the schema.
    #[serde(other)]
    Unknown,
}

use TokenPermission::*;

impl TokenPermission {
    /// Every permission a token can hold.
    pub const KNOWN: &[TokenPermission] = &[Send, JobsRun, StatusRead, LogsRead, RunsContent];

    /// Whether `tokens.create` accepts this set: `runs:content` needs `logs:read`, and an unknown
    /// permission is never granted (`422 invalid_permission_set` otherwise).
    pub fn valid_set(allow: &[TokenPermission]) -> bool {
        !allow.contains(&TokenPermission::Unknown)
            && (!allow.contains(&TokenPermission::RunsContent)
                || allow.contains(&TokenPermission::LogsRead))
    }
}

impl PartialSchema for TokenPermission {
    fn schema() -> RefOr<Schema> {
        string_enum(Self::KNOWN)
    }
}

impl ToSchema for TokenPermission {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn wire_names_are_the_colon_form() {
        assert_eq!(
            serde_json::to_value(TokenPermission::KNOWN).unwrap(),
            json!([
                "send",
                "jobs:run",
                "status:read",
                "logs:read",
                "runs:content"
            ])
        );
    }

    #[test]
    fn runs_content_needs_logs_read() {
        assert!(TokenPermission::valid_set(&[LogsRead, RunsContent]));
        assert!(!TokenPermission::valid_set(&[RunsContent]));
        assert!(!TokenPermission::valid_set(&[Send, Unknown]));
        assert!(TokenPermission::valid_set(&[]));
    }
}
