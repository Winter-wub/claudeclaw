//! The Bug-report export (#28): `logs.export`'s response, its manifest, and the two tables
//! `store::export` scrubs by, [`FIELD_CLASSES`] and [`SETTINGS_VALUE_ALLOW`].

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire::wire_enum;

/// `logs.export`: one reviewable Markdown file, already scrubbed (management-api §2.9, #28 D7).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LogExport {
    pub manifest: ExportManifest,
    /// The Markdown file.
    pub text: String,
    /// The reverse map `{pseudonym: real value}`, only when the caller asked with `map=true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<BTreeMap<String, String>>,
}

/// Sizes only: `map` holds the real identifiers the text hides.
impl fmt::Debug for LogExport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogExport")
            .field("manifest", &self.manifest)
            .field("text", &format_args!("<{} bytes>", self.text.len()))
            .field(
                "map",
                &format_args!("<{} entries>", self.map.as_ref().map_or(0, BTreeMap::len)),
            )
            .finish()
    }
}

/// What an export holds, without its text; `claudeclaw logs export --json` prints it (#28 D6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExportManifest {
    /// `claudeclaw-export/1`.
    pub format: String,
    pub created_at: DateTime<Utc>,
    pub versions: ExportVersions,
    pub window: ExportWindow,
    /// Daemon-log lines included, after trimming.
    pub lines: u32,
    /// Run index records included.
    pub runs: u32,
    /// Pseudonyms assigned, by kind (`discord`, `job`, …; see [`PseudonymKind`]).
    pub pseudonyms: BTreeMap<String, u32>,
    pub trimmed: ExportTrimmed,
    /// The pseudonym key was written (`--map`, or `map=true`).
    pub map_written: bool,
}

/// The versions in an export's Summary (#28 D1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExportVersions {
    /// The `claudeclaw` that made the export, as in `claudeclaw version`.
    pub claudeclaw: String,
    pub commit: String,
    pub target: String,
    /// The running Daemon's version; `null` when none answered.
    #[schema(required = true)]
    pub daemon: Option<String>,
    /// The Desktop app's version, when the app made the export.
    #[schema(required = true)]
    pub app: Option<String>,
}

/// The time window the Daemon log and runs sections cover (#28 D1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExportWindow {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
}

/// Daemon-log lines dropped to fit `max_bytes`, in trimming order; never silent (#28 D6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExportTrimmed {
    pub trace: u32,
    pub debug: u32,
    pub info: u32,
    pub warn_plus: u32,
    /// The oldest line still included; `null` when no line is.
    #[schema(required = true)]
    pub oldest_included: Option<DateTime<Utc>>,
}

wire_enum! {
    /// An export's sections, in file order (#28 D1). `export_leak` names one in
    /// `details.section`.
    pub enum ExportSection {
        Header = "header",
        Summary = "summary",
        Status = "status",
        Settings = "settings",
        Jobs = "jobs",
        Crash = "crash",
        Runs = "runs",
        DaemonLog = "daemon_log",
    }
}

/// What a pseudonym stands for: the `<kind>` of `<kind>:#n` (#28 D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PseudonymKind {
    /// Every Discord snowflake: channel, thread, guild, user, message, application.
    Discord,
    Job,
    Agent,
    /// Claude Code session IDs.
    Session,
    /// Launcher file names; `direct` is kept.
    Launcher,
    Device,
    /// Device labels.
    Label,
    /// API token IDs.
    Token,
    /// API token names.
    TokenName,
    /// `mcp__<server>__<tool>` tool names; built-in tool names are kept.
    Mcp,
    /// Remote peer addresses, except loopback and unspecified.
    Ip,
    Host,
    OsUser,
    /// Paths outside the project dir and the system prefixes.
    Path,
}

impl PseudonymKind {
    /// The `<kind>` prefix, also the key in [`ExportManifest::pseudonyms`].
    pub const fn as_str(self) -> &'static str {
        match self {
            PseudonymKind::Discord => "discord",
            PseudonymKind::Job => "job",
            PseudonymKind::Agent => "agent",
            PseudonymKind::Session => "session",
            PseudonymKind::Launcher => "launcher",
            PseudonymKind::Device => "device",
            PseudonymKind::Label => "label",
            PseudonymKind::Token => "token",
            PseudonymKind::TokenName => "token-name",
            PseudonymKind::Mcp => "mcp",
            PseudonymKind::Ip => "ip",
            PseudonymKind::Host => "host",
            PseudonymKind::OsUser => "os-user",
            PseudonymKind::Path => "path",
        }
    }
}

/// How the export scrubs a Daemon-log or Run index field (#28 D5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldClass {
    /// Kept as is: closed enums, numbers, times, code-given names, kept IDs.
    Structural,
    /// Pseudonymised as this kind.
    Identifier(PseudonymKind),
    /// A Scope key: split, then its name pseudonymised.
    Scope,
    /// `local` and `pairing` kept; Device and token IDs pseudonymised.
    Principal,
    /// The free-text pass. Also every field not in [`FIELD_CLASSES`].
    FreeText,
}

use FieldClass::{FreeText, Identifier, Structural};
use PseudonymKind as K;

/// Every Daemon-log and Run index field name, at any depth, with its class (#28 D5.1). Sorted by
/// name. Each new event field needs an entry; one that has none is treated as [`FreeText`].
pub const FIELD_CLASSES: &[(&str, FieldClass)] = &[
    ("added", Structural),
    ("agent", Identifier(K::Agent)),
    ("api_error_status", Structural),
    ("api_key_source", Structural),
    ("attachments", Structural),
    ("attempt", Structural),
    ("author_id", Identifier(K::Discord)),
    ("awake_ms", Structural),
    ("backoff_ms", Structural),
    ("beacon", Structural),
    ("boot_id", Structural),
    ("bytes", Structural),
    ("cache_read_tokens", Structural),
    ("cache_write_tokens", Structural),
    ("catch_up", Structural),
    ("changed_keys", FreeText),
    ("channel_id", Identifier(K::Discord)),
    ("claude_code_version", Structural),
    ("code", Structural),
    ("col", Structural),
    ("console", Structural),
    ("cost_basis", Structural),
    ("cost_status", Structural),
    ("cost_usd", Structural),
    ("count", Structural),
    ("deleted", Structural),
    ("device_id", Identifier(K::Device)),
    ("diagnostic", FreeText),
    ("done", Structural),
    ("duration_ms", Structural),
    ("ended_at", Structural),
    ("err", FreeText),
    ("event", Structural),
    ("exit_code", Structural),
    ("expected", Structural),
    ("fatal", FreeText),
    ("file", FreeText),
    ("free_mib", Structural),
    ("freed_bytes", Structural),
    ("guild_id", Identifier(K::Discord)),
    ("hard", Structural),
    ("has_run_log", Structural),
    ("invalid", Structural),
    ("is_error", Structural),
    ("job", Identifier(K::Job)),
    ("kept", Structural),
    ("kind", Structural),
    ("label", Identifier(K::Label)),
    ("late_ms", Structural),
    ("latest", Structural),
    ("launcher", Identifier(K::Launcher)),
    ("launcher_resolved", FreeText),
    ("level", Structural),
    ("line", FreeText),
    ("linger_ms", Structural),
    ("lingered", Structural),
    ("listener", Structural),
    ("message", FreeText),
    ("message_id", Identifier(K::Discord)),
    ("method", Structural),
    ("missed", Structural),
    ("model", Structural),
    ("models", Structural),
    ("ms", Structural),
    // `RunRecord.name`; the Daemon's fixed run-log names are kept (#28 D1.7).
    ("name", Identifier(K::Job)),
    ("noise_lines", Structural),
    ("origin", Structural),
    ("outcome", Structural),
    ("peer", Identifier(K::Ip)),
    ("pid", Structural),
    ("pipe_held", Structural),
    ("principal", FieldClass::Principal),
    ("provider", Structural),
    ("purpose", Structural),
    ("queue_depth", Structural),
    ("quiet_ms", Structural),
    ("raw", FreeText),
    ("reason", Structural),
    ("result_bytes", Structural),
    ("route", Structural),
    ("run_id", Structural),
    ("run_log", FreeText),
    ("scheduled_for", Structural),
    ("scope", FieldClass::Scope),
    ("seq", Structural),
    ("session", Structural),
    ("session_id", Identifier(K::Session)),
    ("settle", Structural),
    ("settle_ms", Structural),
    ("signal", Structural),
    ("started_at", Structural),
    ("state", Structural),
    ("status", Structural),
    ("stderr_bytes", Structural),
    ("stragglers", Structural),
    ("suspended_ms", Structural),
    ("target", Structural),
    ("text_bytes", Structural),
    ("timestamp", Structural),
    ("token_id", Identifier(K::Token)),
    ("token_name", Identifier(K::TokenName)),
    ("tokens_in", Structural),
    ("tokens_out", Structural),
    ("tool", Identifier(K::Mcp)),
    ("tool_uses", Structural),
    ("total", Structural),
    ("turn_id", Structural),
    ("unknown", Structural),
    ("until", Structural),
    ("uptime_ms", Structural),
    ("user_id", Identifier(K::Discord)),
    ("version", Structural),
    ("wake_grace", Structural),
];

/// The class of a field name; [`FreeText`], the safe side, when it isn't in [`FIELD_CLASSES`].
pub fn field_class(name: &str) -> FieldClass {
    FIELD_CLASSES
        .binary_search_by(|(field, _)| (*field).cmp(name))
        .map_or(FreeText, |i| FIELD_CLASSES[i].1)
}

/// `settings.json` key paths whose values the export keeps (#28 D1.4). Booleans are kept
/// anyway; any other value off this list becomes `"<set>"` or `{"len": n}`. A path ending in
/// `.*` covers every key below it. Code-reviewed: adding a path publishes its value.
pub const SETTINGS_VALUE_ALLOW: &[&str] = &[
    "model",
    "fallback.model",
    "timezone",
    "heartbeat.enabled",
    "heartbeat.interval",
    "security.level",
    "logging.*",
    "remote.enabled",
];

/// Whether the export keeps the value at this dotted `settings.json` key path.
pub fn settings_value_allowed(path: &str) -> bool {
    SETTINGS_VALUE_ALLOW
        .iter()
        .any(|allowed| match allowed.strip_suffix(".*") {
            Some(prefix) => path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1),
            None => path == *allowed,
        })
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    LogExport,
    ExportManifest,
    ExportVersions,
    ExportWindow,
    ExportTrimmed,
    ExportSection,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn manifest() -> ExportManifest {
        ExportManifest {
            format: "claudeclaw-export/1".into(),
            created_at: "2026-10-14T10:00:00Z".parse().unwrap(),
            versions: ExportVersions {
                claudeclaw: "2.0.0".into(),
                commit: "88ec5c0".into(),
                target: "aarch64-apple-darwin".into(),
                daemon: Some("2.0.0".into()),
                app: None,
            },
            window: ExportWindow {
                since: "2026-10-13T10:00:00Z".parse().unwrap(),
                until: "2026-10-14T10:00:00Z".parse().unwrap(),
            },
            lines: 3000,
            runs: 70,
            pseudonyms: BTreeMap::from([("discord".into(), 3), ("job".into(), 5)]),
            trimmed: ExportTrimmed {
                trace: 0,
                debug: 120,
                info: 0,
                warn_plus: 0,
                oldest_included: Some("2026-10-13T10:00:01Z".parse().unwrap()),
            },
            map_written: false,
        }
    }

    #[test]
    fn a_log_export_matches_the_spec_shape() {
        let export = LogExport {
            manifest: manifest(),
            text: "# claudeclaw-export/1\n".into(),
            map: None,
        };
        let json = json!({
            "manifest": {
                "format": "claudeclaw-export/1",
                "created_at": "2026-10-14T10:00:00Z",
                "versions": {
                    "claudeclaw": "2.0.0",
                    "commit": "88ec5c0",
                    "target": "aarch64-apple-darwin",
                    "daemon": "2.0.0",
                    "app": null,
                },
                "window": {"since": "2026-10-13T10:00:00Z", "until": "2026-10-14T10:00:00Z"},
                "lines": 3000,
                "runs": 70,
                "pseudonyms": {"discord": 3, "job": 5},
                "trimmed": {
                    "trace": 0,
                    "debug": 120,
                    "info": 0,
                    "warn_plus": 0,
                    "oldest_included": "2026-10-13T10:00:01Z",
                },
                "map_written": false,
            },
            "text": "# claudeclaw-export/1\n",
        });
        assert_eq!(serde_json::to_value(&export).unwrap(), json);
        assert_eq!(serde_json::from_value::<LogExport>(json).unwrap(), export);
    }

    #[test]
    fn debug_hides_the_text_and_the_map() {
        let export = LogExport {
            manifest: manifest(),
            text: "CANARY-TEXT".into(),
            map: Some(BTreeMap::from([("job:#1".into(), "CANARY-JOB".into())])),
        };
        let debug = format!("{export:?}");
        assert!(!debug.contains("CANARY-"), "{debug}");
        assert!(debug.contains("<11 bytes>") && debug.contains("<1 entries>"));
        assert_eq!(
            serde_json::to_value(&export).unwrap()["map"]["job:#1"],
            "CANARY-JOB"
        );
    }

    #[test]
    fn sections_are_in_file_order() {
        assert_eq!(
            serde_json::to_value(ExportSection::KNOWN).unwrap(),
            json!([
                "header",
                "summary",
                "status",
                "settings",
                "jobs",
                "crash",
                "runs",
                "daemon_log"
            ])
        );
    }

    #[test]
    fn field_classes_are_sorted_and_unique() {
        assert!(FIELD_CLASSES.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn field_classes_cover_the_spec_examples() {
        assert_eq!(field_class("event"), Structural);
        assert_eq!(field_class("job"), Identifier(K::Job));
        assert_eq!(field_class("author_id"), Identifier(K::Discord));
        assert_eq!(field_class("session_id"), Identifier(K::Session));
        assert_eq!(field_class("scope"), FieldClass::Scope);
        assert_eq!(field_class("principal"), FieldClass::Principal);
        assert_eq!(field_class("message"), FreeText);
        assert_eq!(field_class("never_classified"), FreeText);
    }

    #[test]
    fn pseudonym_kinds_are_the_spec_prefixes() {
        assert_eq!(K::TokenName.as_str(), "token-name");
        assert_eq!(K::OsUser.as_str(), "os-user");
    }

    #[test]
    fn settings_allowlist_matching() {
        for path in [
            "model",
            "fallback.model",
            "timezone",
            "heartbeat.interval",
            "security.level",
            "logging.level",
            "logging.runLogKeepDays",
            "remote.enabled",
        ] {
            assert!(settings_value_allowed(path), "{path}");
        }
        for path in [
            "",
            "logging",
            "logging.",
            "loggingx.level",
            "heartbeat.prompt",
            "discord.token",
            "discord.listenChannels",
            "fallback.api",
            "remote.listen",
            "model.extra",
        ] {
            assert!(!settings_value_allowed(path), "{path}");
        }
    }
}
