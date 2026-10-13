//! Token permissions (management-api §1.3): the final v1 set, in colon form (#22).

use crate::wire::wire_enum;

wire_enum! {
    /// What an API token may do beyond `any` routes. Everything not granted here is `admin` only.
    pub enum TokenPermission {
        /// `turns.create` into the `global` Scope only, plus reading Turns this token created.
        /// The legacy `apiToken` is imported with exactly this (#17 §3).
        Send = "send",
        /// `jobs.run`, plus reading Turns this token created.
        JobsRun = "jobs:run",
        /// Content-free reads: health, status, events, job lists and checks, update info.
        StatusRead = "status:read",
        /// Daemon logs, the Bug-report export, the Run index and the audit log (#23 D5, #28).
        LogsRead = "logs:read",
        /// Run log bodies. Only granted together with `logs:read`.
        RunsContent = "runs:content",
    }
}

impl TokenPermission {
    /// Whether `tokens.create` accepts this set: `runs:content` needs `logs:read`, and an unknown
    /// permission is never granted (`422 invalid_permission_set` otherwise).
    pub fn valid_set(allow: &[TokenPermission]) -> bool {
        !allow.contains(&TokenPermission::Unknown)
            && (!allow.contains(&TokenPermission::RunsContent)
                || allow.contains(&TokenPermission::LogsRead))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use TokenPermission::*;

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
