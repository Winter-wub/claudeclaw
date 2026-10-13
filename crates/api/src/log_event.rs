//! Stable log event names, and the events the Desktop app's webview may log (#40 D1, D3).
//!
//! The names are binding: an App log line's `event` is one of [`app::ALL`]. New names may be
//! added. Every field they carry needs a [`crate::export::FIELD_CLASSES`] entry (#40 D3).

use serde::{Deserialize, Serialize};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{PartialSchema, ToSchema};

/// The App log catalogue (#40 D3). The App log holds no owner identifiers: only the app-local
/// daemon ID, `op`, statuses, error codes, durations, byte counts and versions.
pub mod app {
    // Lifecycle.
    /// `app_version`, `os`, `arch`, `webview_version`.
    pub const APP_START: &str = "app.start";
    pub const APP_READY: &str = "app.ready";
    pub const APP_EXIT: &str = "app.exit";
    /// `location`, `payload_bytes`; then the default panic hook runs (#40 D1).
    pub const APP_PANIC: &str = "app.panic";
    pub const LOG_CAPPED: &str = "log.capped";

    // Updater.
    pub const UPDATE_CHECKED: &str = "update.checked";
    pub const UPDATE_AVAILABLE: &str = "update.available";
    pub const UPDATE_INSTALLED: &str = "update.installed";
    /// `kind`.
    pub const UPDATE_FAILED: &str = "update.failed";

    // Daemon list.
    /// `daemon`, `kind: local|remote`.
    pub const DAEMON_ADDED: &str = "daemon.added";
    /// `daemon`, `kind`.
    pub const DAEMON_REMOVED: &str = "daemon.removed";
    /// `daemon`, `from`, `to` (`DaemonReachability`), `class: refused|timeout|dns|tls_pin|revoked`.
    pub const DAEMON_REACHABILITY: &str = "daemon.reachability";

    // Local process duties: `daemon`, `exit_code`, `code`, `duration_ms`.
    pub const DAEMON_START: &str = "daemon.start";
    pub const DAEMON_STOP: &str = "daemon.stop";
    pub const DAEMON_ATTACH: &str = "daemon.attach";
    pub const DAEMON_RESTART_ANYWAY: &str = "daemon.restart_anyway";

    // Offline commands.
    /// `command: logs|export|import_dry_run|start|stop`, `exit_code`, `duration_ms`,
    /// `stderr_bytes`, `outcome: ok|timeout|cancelled|failed`.
    pub const OFFLINE_EXEC: &str = "offline.exec";

    // Bridge.
    /// DEBUG: `daemon`, `op`, `status`, `code?`, `duration_ms`, `deadline_ms`, `req_bytes`,
    /// `resp_bytes`.
    pub const API_CALL: &str = "api.call";
    /// WARN: `daemon`, `op`, `kind`, `elapsed_ms`, `deadline_ms`.
    pub const API_CALL_FAILED: &str = "api.call_failed";
    /// DEBUG.
    pub const API_CANCELLED: &str = "api.cancelled";
    /// `daemon`, `family`.
    pub const SSE_OPEN: &str = "sse.open";
    /// `daemon`, `family`.
    pub const SSE_GAP: &str = "sse.gap";
    /// `daemon`, `family`.
    pub const SSE_IDLE_RECONNECT: &str = "sse.idle_reconnect";
    /// `daemon`, `family`.
    pub const SSE_CLOSED: &str = "sse.closed";

    // Pairing and keys.
    pub const PAIRING_STARTED: &str = "pairing.started";
    pub const PAIRING_COMPLETED: &str = "pairing.completed";
    /// `kind`.
    pub const PAIRING_FAILED: &str = "pairing.failed";
    /// `op`, `kind`.
    pub const KEYCHAIN_FAILED: &str = "keychain.failed";

    // Webview: the first four come from [`super::UiLogEvent`].
    /// `screen`, `name`, `frames`, `message_bytes`.
    pub const UI_ERROR: &str = "ui.error";
    /// `screen`, `name`, `frames`, `message_bytes`.
    pub const UI_UNHANDLED_REJECTION: &str = "ui.unhandled_rejection";
    /// `screen`, `name`, `frames`, `message_bytes`.
    pub const UI_RENDER_ERROR: &str = "ui.render_error";
    /// DEBUG: `screen`.
    pub const UI_SCREEN: &str = "ui.screen";
    /// `count`: events over the `app_log` rate limit (100/s), never dropped silently (#40 D1).
    pub const UI_LOG_DROPPED: &str = "ui.log_dropped";

    /// Every App log event name, in catalogue order.
    pub const ALL: &[&str] = &[
        APP_START,
        APP_READY,
        APP_EXIT,
        APP_PANIC,
        LOG_CAPPED,
        UPDATE_CHECKED,
        UPDATE_AVAILABLE,
        UPDATE_INSTALLED,
        UPDATE_FAILED,
        DAEMON_ADDED,
        DAEMON_REMOVED,
        DAEMON_REACHABILITY,
        DAEMON_START,
        DAEMON_STOP,
        DAEMON_ATTACH,
        DAEMON_RESTART_ANYWAY,
        OFFLINE_EXEC,
        API_CALL,
        API_CALL_FAILED,
        API_CANCELLED,
        SSE_OPEN,
        SSE_GAP,
        SSE_IDLE_RECONNECT,
        SSE_CLOSED,
        PAIRING_STARTED,
        PAIRING_COMPLETED,
        PAIRING_FAILED,
        KEYCHAIN_FAILED,
        UI_ERROR,
        UI_UNHANDLED_REJECTION,
        UI_RENDER_ERROR,
        UI_SCREEN,
        UI_LOG_DROPPED,
    ];
}

/// One event the webview logs through the `app_log(batch)` command, tagged by `event`
/// (#40 D1, D3). The UI can't log free text: JS error messages are never sent, only the error's
/// `name`, its stack `frames` and the message's size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum UiLogEvent {
    /// `window.onerror`.
    #[serde(rename = "ui.error")]
    Error {
        screen: String,
        /// The JS error's `name`, e.g. `TypeError`.
        name: String,
        /// Stack frames as `assets/index-<hash>.js:<line>:<col>`, symbolicated offline with the
        /// release's source maps.
        frames: Vec<String>,
        message_bytes: u64,
    },
    #[serde(rename = "ui.unhandled_rejection")]
    UnhandledRejection {
        screen: String,
        name: String,
        frames: Vec<String>,
        message_bytes: u64,
    },
    /// A React error boundary caught a render error.
    #[serde(rename = "ui.render_error")]
    RenderError {
        screen: String,
        name: String,
        frames: Vec<String>,
        message_bytes: u64,
    },
    /// The UI moved to another screen (DEBUG).
    #[serde(rename = "ui.screen")]
    Screen { screen: String },
    /// An event added later that this build doesn't know. Never sent, and not in the schema;
    /// the app counts and drops it.
    #[serde(other, rename = "unknown")]
    Unknown,
}

impl UiLogEvent {
    /// The App log event name, one of [`app::ALL`]; `unknown` for the catch-all.
    pub fn event(&self) -> &'static str {
        match self {
            UiLogEvent::Error { .. } => app::UI_ERROR,
            UiLogEvent::UnhandledRejection { .. } => app::UI_UNHANDLED_REJECTION,
            UiLogEvent::RenderError { .. } => app::UI_RENDER_ERROR,
            UiLogEvent::Screen { .. } => app::UI_SCREEN,
            UiLogEvent::Unknown => "unknown",
        }
    }
}

/// [`UiLogEvent`]'s schema without the catch-all. utoipa can't leave a variant out, so the
/// schema is derived from this copy, and a test checks that every known event parses as it.
mod schema {
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    /// One event the webview logs through `app_log(batch)`, tagged by `event`. JS error
    /// messages are never sent, only the error's `name`, its stack `frames` and the message's
    /// size.
    #[derive(Serialize, Deserialize, ToSchema)]
    #[serde(tag = "event")]
    pub(super) enum UiLogEvent {
        #[serde(rename = "ui.error")]
        Error {
            screen: String,
            /// The JS error's `name`, e.g. `TypeError`.
            name: String,
            /// Stack frames as `assets/index-<hash>.js:<line>:<col>`.
            frames: Vec<String>,
            message_bytes: u64,
        },
        #[serde(rename = "ui.unhandled_rejection")]
        UnhandledRejection {
            screen: String,
            name: String,
            frames: Vec<String>,
            message_bytes: u64,
        },
        #[serde(rename = "ui.render_error")]
        RenderError {
            screen: String,
            name: String,
            frames: Vec<String>,
            message_bytes: u64,
        },
        #[serde(rename = "ui.screen")]
        Screen { screen: String },
    }
}

impl PartialSchema for UiLogEvent {
    fn schema() -> RefOr<Schema> {
        schema::UiLogEvent::schema()
    }
}

impl ToSchema for UiLogEvent {
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schema::UiLogEvent::schemas(schemas);
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(UiLogEvent)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn app_event_names_are_unique_dotted_snake_case() {
        let unique: BTreeSet<_> = app::ALL.iter().collect();
        assert_eq!(unique.len(), app::ALL.len());
        for name in app::ALL {
            let parts: Vec<_> = name.split('.').collect();
            assert_eq!(parts.len(), 2, "{name}");
            assert!(
                parts.iter().all(
                    |p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                ),
                "{name}"
            );
        }
    }

    fn every_event() -> Vec<(UiLogEvent, Value)> {
        let frames = vec!["assets/index-3f2a.js:1:2345".to_string()];
        vec![
            (
                UiLogEvent::Error {
                    screen: "jobs".into(),
                    name: "TypeError".into(),
                    frames: frames.clone(),
                    message_bytes: 61,
                },
                json!({
                    "event": "ui.error",
                    "screen": "jobs",
                    "name": "TypeError",
                    "frames": ["assets/index-3f2a.js:1:2345"],
                    "message_bytes": 61,
                }),
            ),
            (
                UiLogEvent::UnhandledRejection {
                    screen: "chat".into(),
                    name: "AbortError".into(),
                    frames: vec![],
                    message_bytes: 0,
                },
                json!({
                    "event": "ui.unhandled_rejection",
                    "screen": "chat",
                    "name": "AbortError",
                    "frames": [],
                    "message_bytes": 0,
                }),
            ),
            (
                UiLogEvent::RenderError {
                    screen: "settings".into(),
                    name: "Error".into(),
                    frames,
                    message_bytes: 12,
                },
                json!({
                    "event": "ui.render_error",
                    "screen": "settings",
                    "name": "Error",
                    "frames": ["assets/index-3f2a.js:1:2345"],
                    "message_bytes": 12,
                }),
            ),
            (
                UiLogEvent::Screen {
                    screen: "logs".into(),
                },
                json!({"event": "ui.screen", "screen": "logs"}),
            ),
        ]
    }

    #[test]
    fn every_ui_event_round_trips_and_is_in_the_catalogue() {
        for (event, wire) in every_event() {
            assert_eq!(serde_json::to_value(&event).unwrap(), wire);
            assert_eq!(wire["event"], event.event());
            assert!(app::ALL.contains(&event.event()));
            assert_eq!(
                serde_json::from_value::<UiLogEvent>(wire.clone()).unwrap(),
                event
            );
            let copy: schema::UiLogEvent = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(copy).unwrap(), wire);
        }
    }

    #[test]
    fn an_unknown_ui_event_parses_as_the_catch_all() {
        let event: UiLogEvent =
            serde_json::from_value(json!({"event": "ui.click", "target": "save"})).unwrap();
        assert_eq!(event, UiLogEvent::Unknown);
    }

    #[test]
    fn a_ui_event_cannot_carry_a_message() {
        // Unknown fields are ignored, so a stray `message` never reaches the App log.
        let event: UiLogEvent = serde_json::from_value(json!({
            "event": "ui.error", "screen": "jobs", "name": "SyntaxError", "frames": [],
            "message_bytes": 40, "message": "JSON Parse error",
        }))
        .unwrap();
        assert!(!serde_json::to_string(&event).unwrap().contains("Parse"));
    }

    #[test]
    fn the_ui_event_schema_leaves_the_catch_all_out() {
        let schema = serde_json::to_string(&UiLogEvent::schema()).unwrap();
        assert!(schema.contains("ui.render_error"));
        assert!(!schema.contains("\"unknown\""));
    }
}
