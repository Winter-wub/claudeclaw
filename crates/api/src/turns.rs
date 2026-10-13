//! Turns (management-api §2.3): `turns.create`, `turns.get` and the `turns.events` stream, plus
//! the result of a manual compact, which is a Turn too (§2.5 `sessions.compact`, #40 D7).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{PartialSchema, ToSchema};

use crate::scope::Scope;
use crate::wire::wire_enum;

/// `turns.create`'s body (management-api §2.3). Only `text` is required.
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TurnRequest {
    #[serde(default = "global")]
    #[schema(default = "global")]
    pub scope: Scope,
    pub text: String,
    /// At most 5, 10 MB each. Written to `inbox/api/` and handed to Claude as files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<AttachmentUpload>>,
    /// Also send the reply to the owner through every adapter's `Notifier` (#22 `send --notify`).
    #[serde(default)]
    #[schema(default = false)]
    pub notify: bool,
}

fn global() -> Scope {
    Scope::Global
}

impl fmt::Debug for TurnRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnRequest")
            .field("scope", &self.scope)
            .field("text_bytes", &self.text.len())
            .field("attachments", &self.attachments)
            .field("notify", &self.notify)
            .finish()
    }
}

/// One file sent with a Turn (management-api §2.3; the CLI's `send --file`).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AttachmentUpload {
    pub name: String,
    pub content_type: String,
    /// The file's bytes, standard base64.
    pub data_base64: String,
}

impl fmt::Debug for AttachmentUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentUpload")
            .field("data_base64_bytes", &self.data_base64.len())
            .finish_non_exhaustive()
    }
}

/// `202` from `turns.create`, `jobs.run` and `sessions.compact` (management-api §2.3, #40 D7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TurnAccepted {
    pub turn_id: String,
    pub scope: Scope,
    pub queue_position: u32,
}

wire_enum! {
    /// Where a Turn is (management-api §2.3).
    pub enum TurnState {
        Queued = "queued",
        Running = "running",
        Done = "done",
    }
}

wire_enum! {
    /// How a finished Turn ended (management-api §2.3, #22 `send --json`).
    pub enum TurnOutcome {
        Ok = "ok",
        Failed = "failed",
        /// A Refused run: the Launcher could not be run, with no fallback (#8).
        Refused = "refused",
        Interrupted = "interrupted",
    }
}

wire_enum! {
    /// Why a Turn exists (#40 D7).
    pub enum TurnPurpose {
        Prompt = "prompt",
        /// A manual compact (`sessions.compact`).
        Compact = "compact",
    }
}

wire_enum! {
    /// Whether a run started a new session or resumed the Scope's one (management-api §2.3).
    pub enum TurnSession {
        New = "new",
        Resumed = "resumed",
    }
}

wire_enum! {
    /// A subagent's state in `TurnEvent::Agent` (management-api §2.3).
    pub enum TurnAgentState {
        Spawned = "spawned",
        Done = "done",
    }
}

/// A Turn as `turns.get` and `?wait=` return it (management-api §2.3, #40). It matches #22's
/// `send --json`.
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TurnRecord {
    pub turn_id: String,
    pub scope: Scope,
    /// Where the Turn came from: `discord`, `scheduler`, `heartbeat`, `api`, `cli`, … (#23).
    pub origin: String,
    /// The API principal that created it (`local`, `device:<id>`, `token:<id>`); `null` when the
    /// Turn didn't come through the API.
    #[schema(required = true)]
    pub principal: Option<String>,
    pub purpose: TurnPurpose,
    pub state: TurnState,
    /// `null` until `state` is `done`.
    #[schema(required = true)]
    pub outcome: Option<TurnOutcome>,
    /// The reply text; `null` until done, or when there is none.
    #[schema(required = true)]
    pub text: Option<String>,
    /// Set when `purpose` is `compact`.
    #[schema(required = true)]
    pub compact: Option<SessionCompacted>,
    /// `null` until the first run has a session.
    #[schema(required = true)]
    pub session_id: Option<String>,
    /// `direct` or a Launcher path; `null` until the Turn's first run resolves it.
    #[schema(required = true)]
    pub launcher: Option<String>,
    /// Every run the Turn spawned, in order: the first attempt, a compact, a retry (#33).
    pub run_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
    #[schema(required = true)]
    pub finished_at: Option<DateTime<Utc>>,
    /// `null` until done.
    #[schema(required = true)]
    pub duration_ms: Option<u64>,
}

impl fmt::Debug for TurnRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnRecord")
            .field("turn_id", &self.turn_id)
            .field("scope", &self.scope)
            .field("origin", &self.origin)
            .field("principal", &self.principal)
            .field("purpose", &self.purpose)
            .field("state", &self.state)
            .field("outcome", &self.outcome)
            .field("text_bytes", &self.text.as_ref().map(String::len))
            .field("compact", &self.compact)
            .field("session_id", &self.session_id)
            .field("launcher", &self.launcher)
            .field("run_ids", &self.run_ids)
            .field("created_at", &self.created_at)
            .field("finished_at", &self.finished_at)
            .field("duration_ms", &self.duration_ms)
            .finish()
    }
}

/// What a manual compact did: `TurnRecord.compact` (management-api §2.5 `sessions.compact`,
/// #40 D7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionCompacted {
    pub scope: Scope,
    pub launcher: String,
    pub ok: bool,
    /// The session's context in tokens before the compact (`context.used_tokens`, #39 D4);
    /// `null` when it isn't known, e.g. an imported session before its first Rust Turn.
    #[schema(required = true)]
    pub context_before: Option<u64>,
    /// The same after the compact; `null` when it failed.
    #[schema(required = true)]
    pub context_after: Option<u64>,
}

/// One event on `turns.events`, tagged by `kind` (management-api §2.3). The stream replays from
/// the Turn's start and ends after `outcome`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnEvent {
    Queued {
        position: u32,
    },
    Started {
        run_id: String,
        attempt: u32,
        session: TurnSession,
        session_id: String,
        launcher: String,
    },
    /// A piece of the reply as it streams.
    Text {
        delta: String,
    },
    Tool {
        name: String,
        category: String,
        summary: String,
    },
    ToolResult {
        name: String,
        ok: bool,
    },
    Agent {
        id: String,
        description: String,
        state: TurnAgentState,
    },
    Retry {
        reason_kind: String,
        attempt: u32,
        at: DateTime<Utc>,
    },
    Refused {
        reason_kind: String,
    },
    Outcome {
        record: Box<TurnRecord>,
    },
    /// An event added later in v1 that this build doesn't know (management-api §1.1). Never
    /// sent, and not in the schema.
    #[serde(other)]
    Unknown,
}

impl fmt::Debug for TurnEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TurnEvent::Queued { position } => f
                .debug_struct("Queued")
                .field("position", position)
                .finish(),
            TurnEvent::Started {
                run_id,
                attempt,
                session,
                session_id,
                launcher,
            } => f
                .debug_struct("Started")
                .field("run_id", run_id)
                .field("attempt", attempt)
                .field("session", session)
                .field("session_id", session_id)
                .field("launcher", launcher)
                .finish(),
            TurnEvent::Text { delta } => f
                .debug_struct("Text")
                .field("delta_bytes", &delta.len())
                .finish(),
            TurnEvent::Tool {
                name,
                category,
                summary,
            } => f
                .debug_struct("Tool")
                .field("name", name)
                .field("category", category)
                .field("summary_bytes", &summary.len())
                .finish(),
            TurnEvent::ToolResult { name, ok } => f
                .debug_struct("ToolResult")
                .field("name", name)
                .field("ok", ok)
                .finish(),
            TurnEvent::Agent {
                id,
                description,
                state,
            } => f
                .debug_struct("Agent")
                .field("id", id)
                .field("description_bytes", &description.len())
                .field("state", state)
                .finish(),
            TurnEvent::Retry {
                reason_kind,
                attempt,
                at,
            } => f
                .debug_struct("Retry")
                .field("reason_kind", reason_kind)
                .field("attempt", attempt)
                .field("at", at)
                .finish(),
            TurnEvent::Refused { reason_kind } => f
                .debug_struct("Refused")
                .field("reason_kind", reason_kind)
                .finish(),
            TurnEvent::Outcome { record } => {
                f.debug_struct("Outcome").field("record", record).finish()
            }
            TurnEvent::Unknown => f.write_str("Unknown"),
        }
    }
}

/// [`TurnEvent`]'s schema without the catch-all. utoipa can't leave a variant out, so the
/// schema is derived from this copy, and a test checks that every known event parses as it.
mod schema {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    use super::{TurnAgentState, TurnRecord, TurnSession};

    /// One event on `turns.events`, tagged by `kind`. The stream replays from the Turn's start
    /// and ends after `outcome`.
    #[derive(Serialize, Deserialize, ToSchema)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub(super) enum TurnEvent {
        Queued {
            position: u32,
        },
        Started {
            run_id: String,
            attempt: u32,
            session: TurnSession,
            session_id: String,
            launcher: String,
        },
        /// A piece of the reply as it streams.
        Text {
            delta: String,
        },
        Tool {
            name: String,
            category: String,
            summary: String,
        },
        ToolResult {
            name: String,
            ok: bool,
        },
        Agent {
            id: String,
            description: String,
            state: TurnAgentState,
        },
        Retry {
            reason_kind: String,
            attempt: u32,
            at: DateTime<Utc>,
        },
        Refused {
            reason_kind: String,
        },
        Outcome {
            record: Box<TurnRecord>,
        },
    }
}

impl PartialSchema for TurnEvent {
    fn schema() -> RefOr<Schema> {
        schema::TurnEvent::schema()
    }
}

impl ToSchema for TurnEvent {
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schema::TurnEvent::schemas(schemas);
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    TurnRequest,
    AttachmentUpload,
    TurnAccepted,
    TurnState,
    TurnOutcome,
    TurnPurpose,
    TurnSession,
    TurnAgentState,
    TurnRecord,
    SessionCompacted,
    TurnEvent,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn record() -> TurnRecord {
        TurnRecord {
            turn_id: "0192-turn".into(),
            scope: Scope::Discord("1234".into()),
            origin: "api".into(),
            principal: Some("token:7".into()),
            purpose: TurnPurpose::Prompt,
            state: TurnState::Done,
            outcome: Some(TurnOutcome::Ok),
            text: Some("hello".into()),
            compact: None,
            session_id: Some("sess-1".into()),
            launcher: Some("direct".into()),
            run_ids: vec!["0192-run".into()],
            created_at: at("2026-10-14T09:00:00Z"),
            finished_at: Some(at("2026-10-14T09:00:05Z")),
            duration_ms: Some(5000),
        }
    }

    fn record_json() -> Value {
        json!({
            "turn_id": "0192-turn",
            "scope": "discord:1234",
            "origin": "api",
            "principal": "token:7",
            "purpose": "prompt",
            "state": "done",
            "outcome": "ok",
            "text": "hello",
            "compact": null,
            "session_id": "sess-1",
            "launcher": "direct",
            "run_ids": ["0192-run"],
            "created_at": "2026-10-14T09:00:00Z",
            "finished_at": "2026-10-14T09:00:05Z",
            "duration_ms": 5000,
        })
    }

    fn round_trips<T>(value: &T, wire: Value)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + fmt::Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), wire);
        assert_eq!(&serde_json::from_value::<T>(wire).unwrap(), value);
    }

    #[test]
    fn a_text_only_request_gets_its_defaults() {
        let request: TurnRequest = serde_json::from_value(json!({"text": "hi"})).unwrap();
        assert_eq!(
            request,
            TurnRequest {
                scope: Scope::Global,
                text: "hi".into(),
                attachments: None,
                notify: false,
            }
        );
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"scope": "global", "text": "hi", "notify": false})
        );
    }

    #[test]
    fn request_shape() {
        let request = TurnRequest {
            scope: Scope::Job("daily-report".into()),
            text: "summarise".into(),
            attachments: Some(vec![AttachmentUpload {
                name: "a.png".into(),
                content_type: "image/png".into(),
                data_base64: "aGk=".into(),
            }]),
            notify: true,
        };
        round_trips(
            &request,
            json!({
                "scope": "job:daily-report",
                "text": "summarise",
                "attachments": [
                    {"name": "a.png", "content_type": "image/png", "data_base64": "aGk="},
                ],
                "notify": true,
            }),
        );
    }

    #[test]
    fn accepted_shape() {
        round_trips(
            &TurnAccepted {
                turn_id: "0192-turn".into(),
                scope: Scope::Global,
                queue_position: 2,
            },
            json!({"turn_id": "0192-turn", "scope": "global", "queue_position": 2}),
        );
    }

    #[test]
    fn record_shape() {
        round_trips(&record(), record_json());
    }

    #[test]
    fn a_queued_record_sends_its_nulls() {
        let queued = TurnRecord {
            state: TurnState::Queued,
            outcome: None,
            text: None,
            session_id: None,
            launcher: None,
            run_ids: vec![],
            finished_at: None,
            duration_ms: None,
            ..record()
        };
        let wire = serde_json::to_value(&queued).unwrap();
        for key in [
            "outcome",
            "text",
            "compact",
            "session_id",
            "launcher",
            "finished_at",
            "duration_ms",
        ] {
            assert_eq!(wire[key], Value::Null, "{key}");
        }
    }

    #[test]
    fn a_compact_record_carries_session_compacted() {
        let compact = TurnRecord {
            purpose: TurnPurpose::Compact,
            text: None,
            compact: Some(SessionCompacted {
                scope: Scope::Global,
                launcher: "direct".into(),
                ok: true,
                context_before: Some(180_000),
                context_after: Some(20_000),
            }),
            ..record()
        };
        let mut wire = record_json();
        wire["purpose"] = json!("compact");
        wire["text"] = Value::Null;
        wire["compact"] = json!({
            "scope": "global",
            "launcher": "direct",
            "ok": true,
            "context_before": 180_000,
            "context_after": 20_000,
        });
        round_trips(&compact, wire);
    }

    fn every_event() -> Vec<(TurnEvent, Value)> {
        vec![
            (
                TurnEvent::Queued { position: 1 },
                json!({"kind": "queued", "position": 1}),
            ),
            (
                TurnEvent::Started {
                    run_id: "0192-run".into(),
                    attempt: 1,
                    session: TurnSession::Resumed,
                    session_id: "sess-1".into(),
                    launcher: "direct".into(),
                },
                json!({
                    "kind": "started",
                    "run_id": "0192-run",
                    "attempt": 1,
                    "session": "resumed",
                    "session_id": "sess-1",
                    "launcher": "direct",
                }),
            ),
            (
                TurnEvent::Text {
                    delta: "hel".into(),
                },
                json!({"kind": "text", "delta": "hel"}),
            ),
            (
                TurnEvent::Tool {
                    name: "Read".into(),
                    category: "read".into(),
                    summary: "notes.md".into(),
                },
                json!({"kind": "tool", "name": "Read", "category": "read", "summary": "notes.md"}),
            ),
            (
                TurnEvent::ToolResult {
                    name: "Read".into(),
                    ok: true,
                },
                json!({"kind": "tool_result", "name": "Read", "ok": true}),
            ),
            (
                TurnEvent::Agent {
                    id: "agent-1".into(),
                    description: "look things up".into(),
                    state: TurnAgentState::Spawned,
                },
                json!({
                    "kind": "agent",
                    "id": "agent-1",
                    "description": "look things up",
                    "state": "spawned",
                }),
            ),
            (
                TurnEvent::Retry {
                    reason_kind: "auto_compact".into(),
                    attempt: 3,
                    at: at("2026-10-14T09:05:00Z"),
                },
                json!({
                    "kind": "retry",
                    "reason_kind": "auto_compact",
                    "attempt": 3,
                    "at": "2026-10-14T09:05:00Z",
                }),
            ),
            (
                TurnEvent::Refused {
                    reason_kind: "launcher_missing".into(),
                },
                json!({"kind": "refused", "reason_kind": "launcher_missing"}),
            ),
            (
                TurnEvent::Outcome {
                    record: Box::new(record()),
                },
                json!({"kind": "outcome", "record": record_json()}),
            ),
        ]
    }

    /// Fails to compile when a variant is added, so `every_event` is kept complete.
    fn kind(event: &TurnEvent) -> &'static str {
        match event {
            TurnEvent::Queued { .. } => "queued",
            TurnEvent::Started { .. } => "started",
            TurnEvent::Text { .. } => "text",
            TurnEvent::Tool { .. } => "tool",
            TurnEvent::ToolResult { .. } => "tool_result",
            TurnEvent::Agent { .. } => "agent",
            TurnEvent::Retry { .. } => "retry",
            TurnEvent::Refused { .. } => "refused",
            TurnEvent::Outcome { .. } => "outcome",
            TurnEvent::Unknown => "unknown",
        }
    }

    #[test]
    fn every_event_round_trips_and_matches_its_schema_copy() {
        let events = every_event();
        let kinds: Vec<_> = events.iter().map(|(event, _)| kind(event)).collect();
        assert_eq!(
            kinds,
            [
                "queued",
                "started",
                "text",
                "tool",
                "tool_result",
                "agent",
                "retry",
                "refused",
                "outcome"
            ]
        );
        for (event, wire) in events {
            round_trips(&event, wire.clone());
            let copy: schema::TurnEvent = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(copy).unwrap(), wire);
        }
    }

    #[test]
    fn an_unknown_event_parses_as_the_catch_all() {
        let event: TurnEvent =
            serde_json::from_value(json!({"kind": "added_in_v1_9", "anything": [1, 2]})).unwrap();
        assert_eq!(event, TurnEvent::Unknown);
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({"kind": "unknown"})
        );
    }

    #[test]
    fn the_event_schema_leaves_the_catch_all_out() {
        let schema = serde_json::to_string(&TurnEvent::schema()).unwrap();
        assert!(schema.contains("tool_result"));
        assert!(!schema.contains("unknown"));
    }

    #[test]
    fn debug_prints_content_sizes_only() {
        let request = TurnRequest {
            scope: Scope::Global,
            text: "secret prompt".into(),
            attachments: Some(vec![AttachmentUpload {
                name: "private.pdf".into(),
                content_type: "application/pdf".into(),
                data_base64: "c2VjcmV0".into(),
            }]),
            notify: false,
        };
        let event = TurnEvent::Text {
            delta: "secret reply".into(),
        };
        let outcome = TurnEvent::Outcome {
            record: Box::new(TurnRecord {
                text: Some("secret reply".into()),
                ..record()
            }),
        };
        for debug in [
            format!("{request:?}"),
            format!("{event:?}"),
            format!("{outcome:?}"),
        ] {
            assert!(!debug.contains("secret"), "{debug}");
            assert!(!debug.contains("private"), "{debug}");
            assert!(!debug.contains("c2VjcmV0"), "{debug}");
        }
    }
}
