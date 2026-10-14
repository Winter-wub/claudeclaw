//! Sessions and Transcripts (management-api §2.5): `sessions.list`, `sessions.get`,
//! `sessions.messages`, `sessions.reset` and `sessions.delete`. A session is keyed by
//! (Scope, Launcher) (#8).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::runs::RunOutcome;
use crate::scope::Scope;
use crate::wire::wire_enum;

wire_enum! {
    /// A session's status (management-api §2.5, #22).
    pub enum SessionStatus {
        /// The session of its Scope's effective Launcher.
        InUse = "in_use",
        /// Kept for a Launcher the Scope no longer uses; resumes if it switches back.
        Parked = "parked",
        /// Its Scope is gone, or Claude Code no longer finds it (#39 D5). Pruned after 30 days.
        Orphaned = "orphaned",
    }
}

/// The session summary embedded in `ScopeInfo`, `DiscordThread` and `AgentInfo`
/// (management-api §2.5, §2.7, §2.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScopeSession {
    pub launcher: String,
    pub session_id: String,
    pub status: SessionStatus,
}

/// `sessions.list` → `{sessions}` (management-api §2.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionList {
    pub sessions: Vec<SessionRecord>,
}

/// One (Scope, Launcher) session (management-api §2.5, #22, #32, #33).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionRecord {
    pub scope: Scope,
    pub scope_label: String,
    pub launcher: String,
    pub session_id: String,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    /// Turns run in this session.
    pub turns: u32,
    /// The session's first message, cut to 160 chars. `null` when it isn't known (an imported
    /// session).
    #[schema(required = true)]
    pub first_message: Option<String>,
    /// When the 30-day prune removes it; `null` while it is `in_use`.
    #[schema(required = true)]
    pub prune_at: Option<DateTime<Utc>>,
    pub timeout_breaker: TimeoutBreakerState,
    pub auto_compact: AutoCompactState,
}

impl fmt::Debug for SessionRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionRecord")
            .field("scope", &self.scope)
            .field("scope_label", &self.scope_label)
            .field("launcher", &self.launcher)
            .field("session_id", &self.session_id)
            .field("status", &self.status)
            .field("created_at", &self.created_at)
            .field("last_used_at", &self.last_used_at)
            .field("turns", &self.turns)
            .field(
                "first_message_len",
                &self.first_message.as_ref().map(String::len),
            )
            .field("prune_at", &self.prune_at)
            .field("timeout_breaker", &self.timeout_breaker)
            .field("auto_compact", &self.auto_compact)
            .finish()
    }
}

/// The session's Timeout breaker (#32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TimeoutBreakerState {
    pub consecutive_timeouts: u32,
    /// The first timeout since the last success; `null` when there is none.
    #[schema(required = true)]
    pub failing_since: Option<DateTime<Utc>>,
    /// Open means no auto-compact and no retry until a Turn succeeds.
    pub open: bool,
    #[schema(required = true)]
    pub opened_at: Option<DateTime<Utc>>,
}

/// The session's Auto-compact budget (#33): one compact per stretch of failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AutoCompactState {
    /// When this stretch's compact was spent; `null` while the budget is unspent.
    #[schema(required = true)]
    pub spent_at: Option<DateTime<Utc>>,
    /// The session's last auto-compact; `null` when it never had one.
    #[schema(required = true)]
    pub last: Option<AutoCompactLast>,
}

/// One auto-compact run (#33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AutoCompactLast {
    pub at: DateTime<Utc>,
    /// The compact run's own outcome (#23, #33).
    pub outcome: RunOutcome,
    pub duration_ms: u64,
}

/// `sessions.get` → the session plus its context meter (management-api §2.5, #39 D4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub record: SessionRecord,
    /// From the session's latest Daemon run; `null` until an imported session's first Rust Turn.
    #[schema(required = true)]
    pub context: Option<SessionContext>,
}

/// How full a session's context window is (#39 D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionContext {
    /// The last main-thread turn's input + cache read + cache creation tokens.
    pub used_tokens: u64,
    /// The model's context window.
    pub window: u64,
}

/// `sessions.messages` → one page of a Transcript, newest first (management-api §1.6, §2.5, #39).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TranscriptPage {
    pub messages: Vec<TranscriptMessage>,
    /// The `before` cursor of the next older page; `null` at the start.
    #[schema(required = true)]
    pub next: Option<String>,
}

/// One Transcript message (management-api §2.5, #39).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TranscriptMessage {
    pub uuid: String,
    pub role: TranscriptRole,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    /// The names of the tools the message called, in order.
    pub tools: Vec<String>,
}

impl fmt::Debug for TranscriptMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TranscriptMessage")
            .field("uuid", &self.uuid)
            .field("role", &self.role)
            .field("text_len", &self.text.len())
            .field("timestamp", &self.timestamp)
            .field("tools", &self.tools.len())
            .finish()
    }
}

wire_enum! {
    /// Who wrote a Transcript message (#39).
    pub enum TranscriptRole {
        User = "user",
        Assistant = "assistant",
    }
}

/// `sessions.reset` body (management-api §2.5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionResetRequest {
    /// The session's Launcher; the Scope's current one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
}

/// `sessions.reset` → the cleared session and its backup, mirror included (management-api
/// §2.5, #22).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionReset {
    pub scope: Scope,
    pub launcher: String,
    /// Where the session was backed up; `null` when there was no session to back up.
    #[schema(required = true)]
    pub backup: Option<String>,
}

/// `sessions.delete` → `{deleted}` (management-api §2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionDeleted {
    pub deleted: bool,
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    SessionStatus,
    ScopeSession,
    SessionList,
    SessionRecord,
    TimeoutBreakerState,
    AutoCompactState,
    AutoCompactLast,
    SessionDetail,
    SessionContext,
    TranscriptPage,
    TranscriptMessage,
    TranscriptRole,
    SessionResetRequest,
    SessionReset,
    SessionDeleted,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, hour, 0, 0).unwrap()
    }

    fn record() -> SessionRecord {
        SessionRecord {
            scope: Scope::Discord("1234".into()),
            scope_label: "general".into(),
            launcher: "direct".into(),
            session_id: "s-1".into(),
            status: SessionStatus::Parked,
            created_at: at(8),
            last_used_at: at(9),
            turns: 12,
            first_message: Some("first words".into()),
            prune_at: Some(at(10)),
            timeout_breaker: TimeoutBreakerState {
                consecutive_timeouts: 3,
                failing_since: Some(at(8)),
                open: true,
                opened_at: Some(at(9)),
            },
            auto_compact: AutoCompactState {
                spent_at: Some(at(8)),
                last: Some(AutoCompactLast {
                    at: at(8),
                    outcome: RunOutcome::Timeout,
                    duration_ms: 300_000,
                }),
            },
        }
    }

    fn record_wire() -> serde_json::Value {
        json!({
            "scope": "discord:1234",
            "scope_label": "general",
            "launcher": "direct",
            "session_id": "s-1",
            "status": "parked",
            "created_at": "2026-10-14T08:00:00Z",
            "last_used_at": "2026-10-14T09:00:00Z",
            "turns": 12,
            "first_message": "first words",
            "prune_at": "2026-10-14T10:00:00Z",
            "timeout_breaker": {
                "consecutive_timeouts": 3,
                "failing_since": "2026-10-14T08:00:00Z",
                "open": true,
                "opened_at": "2026-10-14T09:00:00Z",
            },
            "auto_compact": {
                "spent_at": "2026-10-14T08:00:00Z",
                "last": {"at": "2026-10-14T08:00:00Z", "outcome": "timeout", "duration_ms": 300000},
            },
        })
    }

    #[test]
    fn session_record_shape() {
        let list = SessionList {
            sessions: vec![record()],
        };
        let wire = json!({"sessions": [record_wire()]});
        assert_eq!(serde_json::to_value(&list).unwrap(), wire);
        assert_eq!(serde_json::from_value::<SessionList>(wire).unwrap(), list);
    }

    #[test]
    fn a_fresh_session_sends_its_nulls() {
        let mut record = record();
        record.status = SessionStatus::InUse;
        record.first_message = None;
        record.prune_at = None;
        record.timeout_breaker = TimeoutBreakerState {
            consecutive_timeouts: 0,
            failing_since: None,
            open: false,
            opened_at: None,
        };
        record.auto_compact = AutoCompactState {
            spent_at: None,
            last: None,
        };
        let wire = serde_json::to_value(&record).unwrap();
        assert_eq!(wire["first_message"], json!(null));
        assert_eq!(wire["prune_at"], json!(null));
        assert_eq!(
            wire["timeout_breaker"],
            json!({"consecutive_timeouts": 0, "failing_since": null, "open": false, "opened_at": null})
        );
        assert_eq!(
            wire["auto_compact"],
            json!({"spent_at": null, "last": null})
        );
        assert_eq!(
            serde_json::from_value::<SessionRecord>(wire).unwrap(),
            record
        );
    }

    #[test]
    fn session_detail_adds_context_beside_the_record() {
        let detail = SessionDetail {
            record: record(),
            context: Some(SessionContext {
                used_tokens: 41_000,
                window: 200_000,
            }),
        };
        let mut wire = record_wire();
        wire["context"] = json!({"used_tokens": 41000, "window": 200000});
        assert_eq!(serde_json::to_value(&detail).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<SessionDetail>(wire).unwrap(),
            detail
        );

        let imported = SessionDetail {
            context: None,
            ..detail
        };
        assert_eq!(
            serde_json::to_value(&imported).unwrap()["context"],
            json!(null)
        );
    }

    #[test]
    fn transcript_page_shape() {
        let page = TranscriptPage {
            messages: vec![TranscriptMessage {
                uuid: "m-1".into(),
                role: TranscriptRole::Assistant,
                text: "done".into(),
                timestamp: at(9),
                tools: vec!["Read".into(), "Bash".into()],
            }],
            next: Some("c-2".into()),
        };
        let wire = json!({
            "messages": [{
                "uuid": "m-1",
                "role": "assistant",
                "text": "done",
                "timestamp": "2026-10-14T09:00:00Z",
                "tools": ["Read", "Bash"],
            }],
            "next": "c-2",
        });
        assert_eq!(serde_json::to_value(&page).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<TranscriptPage>(wire).unwrap(),
            page
        );
    }

    #[test]
    fn reset_and_delete_shapes() {
        let request: SessionResetRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(request, SessionResetRequest::default());
        assert_eq!(serde_json::to_value(&request).unwrap(), json!({}));

        let reset = SessionReset {
            scope: Scope::Global,
            launcher: "direct".into(),
            backup: Some("daemon/backups/s-1.jsonl".into()),
        };
        let wire =
            json!({"scope": "global", "launcher": "direct", "backup": "daemon/backups/s-1.jsonl"});
        assert_eq!(serde_json::to_value(&reset).unwrap(), wire);
        assert_eq!(serde_json::from_value::<SessionReset>(wire).unwrap(), reset);

        let deleted = SessionDeleted { deleted: true };
        assert_eq!(
            serde_json::to_value(deleted).unwrap(),
            json!({"deleted": true})
        );
        assert_eq!(
            serde_json::from_value::<SessionDeleted>(json!({"deleted": true})).unwrap(),
            deleted
        );
    }

    #[test]
    fn content_stays_out_of_debug() {
        let message = TranscriptMessage {
            uuid: "m-1".into(),
            role: TranscriptRole::User,
            text: "a private question".into(),
            timestamp: at(9),
            tools: vec![],
        };
        let debug = format!("{message:?}");
        assert!(!debug.contains("private"), "{debug}");
        assert!(debug.contains("text_len: 18"), "{debug}");

        let debug = format!("{:?}", record());
        assert!(!debug.contains("first words"), "{debug}");
    }

    #[test]
    fn unknown_statuses_parse() {
        assert_eq!(
            serde_json::from_value::<SessionStatus>(json!("archived")).unwrap(),
            SessionStatus::Unknown
        );
        assert_eq!(
            serde_json::from_value::<RunOutcome>(json!("lingered")).unwrap(),
            RunOutcome::Unknown
        );
    }
}
