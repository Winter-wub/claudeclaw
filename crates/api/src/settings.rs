//! Settings (management-api §2.6): `settings.get` and `settings.update`. Keys keep
//! `settings.json`'s own names, camelCase where the file uses it (§1.1, D10), so a dotted
//! `config set` path and the file agree. Secrets are write-only (#13).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

use crate::revision::Revision;
use crate::wire::wire_enum;

/// `settings.get` → the typed view and its Revision (management-api §2.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SettingsView {
    /// Send it back as `If-Match` on `settings.update` (§1.5).
    pub revision: Revision,
    pub settings: Settings,
    /// Dotted paths of TS-only keys present in the file (`web.port`, `apiToken`, …). They are
    /// read for import only and can't be written here (`422 ts_only_key`).
    pub ts_only_keys: Vec<String>,
    /// Dotted paths of keys an env var wins over, e.g. `discord.token` for `DISCORD_TOKEN`.
    pub env_overrides: Vec<String>,
}

/// `settings.update` → the new view, what changed and what was restarted (management-api §2.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SettingsApplied {
    pub view: SettingsView,
    /// Dotted paths of the keys whose value changed.
    pub changed_keys: Vec<String>,
    pub restarted: Vec<Restarted>,
}

wire_enum! {
    /// A part of the Daemon a settings change restarted (management-api §2.6, architecture §7).
    pub enum Restarted {
        /// A new Discord token restarts only the Discord adapter.
        Discord = "discord",
        /// A `remote.*` change rebinds the remote listener.
        RemoteListener = "remote_listener",
    }
}

/// `settings.update` body: a JSON Merge Patch (RFC 7396) over the file's own key names
/// (management-api §2.6, D10). A secret is set with a string and cleared with `null`.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct SettingsPatch(pub Map<String, Value>);

impl fmt::Debug for SettingsPatch {
    /// Only the top-level key names: a patch can carry a secret or the heartbeat prompt.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SettingsPatch")
            .field(&self.0.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// How a secret reads: whether it is set, never its value (management-api §2.6, #13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct SecretState {
    pub set: bool,
}

/// The typed view of the `settings.json` keys the Rust Daemon reads, after defaults
/// (management-api §2.6, architecture §7). Existing keys keep TS's names and types
/// (`src/config.ts`); Rust-only keys are new top-level keys (#14 rule 1). Unknown and TS-only
/// keys are never shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Settings {
    /// An IANA zone name.
    pub timezone: String,
    pub model: String,
    /// The auth token runs get as `ANTHROPIC_AUTH_TOKEN`.
    pub api: SecretState,
    pub fallback: FallbackSettings,
    pub agentic: AgenticSettings,
    pub heartbeat: HeartbeatSettings,
    pub discord: DiscordSettings,
    pub security: SecuritySettings,
    pub session: SessionSettings,
    /// A manual compact's timeout.
    #[serde(rename = "sessionTimeoutMs")]
    pub session_timeout_ms: u64,
    pub timeouts: TimeoutSettings,
    pub logging: LoggingSettings,
    pub remote: RemoteSettings,
    pub run_path: RunPathSettings,
    pub run_locale: RunLocaleSetting,
    pub timeout_breaker: TimeoutBreakerSettings,
    pub auto_compact: AutoCompactSettings,
    /// Absolute dirs searched for Transcripts when a run's Config dir isn't known (#39 D2).
    pub transcript_roots: Vec<String>,
    pub inbox: InboxSettings,
    pub disk: DiskSettings,
}

/// `fallback`: the model and token a rate-limited run retries with (TS `ModelConfig`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FallbackSettings {
    pub model: String,
    pub api: SecretState,
}

/// `agentic`: model routing by the message's wording (TS `AgenticConfig`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgenticSettings {
    pub enabled: bool,
    pub default_mode: String,
    pub modes: Vec<AgenticMode>,
}

/// One `agentic.modes[]` entry (TS `AgenticMode`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgenticMode {
    pub name: String,
    pub model: String,
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phrases: Option<Vec<String>>,
}

/// `heartbeat` (TS `HeartbeatConfig`).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatSettings {
    pub enabled: bool,
    /// Minutes between heartbeats.
    pub interval: u32,
    pub prompt: String,
    pub exclude_windows: Vec<ExcludeWindow>,
}

impl fmt::Debug for HeartbeatSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeartbeatSettings")
            .field("enabled", &self.enabled)
            .field("interval", &self.interval)
            .field("prompt_len", &self.prompt.len())
            .field("exclude_windows", &self.exclude_windows)
            .finish()
    }
}

/// A `heartbeat.excludeWindows[]` entry (TS `HeartbeatExcludeWindow`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExcludeWindow {
    /// Weekdays, 0 = Sunday; every day when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<Vec<u8>>,
    /// `HH:MM`.
    pub start: String,
    /// `HH:MM`.
    pub end: String,
}

/// `discord` (TS `DiscordConfig`). Snowflakes are strings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiscordSettings {
    pub token: SecretState,
    pub allowed_user_ids: Vec<String>,
    pub listen_channels: Vec<String>,
    pub listen_guilds: Vec<String>,
    pub allowed_guilds: Vec<String>,
    /// Channel snowflake → extra user snowflakes allowed in that channel.
    pub channel_allowed_user_ids: BTreeMap<String, Vec<String>>,
    /// Channel snowflake → friendly name.
    pub channel_names: BTreeMap<String, String>,
    /// Absolute path prefixes images may be uploaded from.
    pub image_output_roots: Vec<String>,
    pub streaming: bool,
}

/// `security` (TS `SecurityConfig`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecuritySettings {
    pub level: SecurityLevel,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
}

wire_enum! {
    /// `security.level` (TS `SecurityLevel`).
    pub enum SecurityLevel {
        Locked = "locked",
        Strict = "strict",
        Moderate = "moderate",
        Unrestricted = "unrestricted",
    }
}

/// `session`: rotation of the global session (TS `SessionConfig`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSettings {
    pub auto_rotate: bool,
    pub max_messages: u32,
    pub max_age_hours: u32,
    /// The dir summaries are written to before a rotation; empty disables them.
    pub summary_path: String,
}

/// `timeouts`: run timeouts in minutes, by category (TS `TimeoutsConfig`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TimeoutSettings {
    pub heartbeat: u32,
    pub job: u32,
    /// Chat and everything else.
    pub default: u32,
}

/// `logging` (#23 D1, #44).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LoggingSettings {
    /// A tracing `EnvFilter` directive, e.g. `info,claudeclaw_discord=debug`.
    pub level: String,
    #[serde(rename = "daemonKeepFiles")]
    pub daemon_keep_files: u32,
    #[serde(rename = "daemonMaxDailyMiB")]
    pub daemon_max_daily_mib: u32,
    #[serde(rename = "daemonMaxTotalMiB")]
    pub daemon_max_total_mib: u32,
    /// `null` keeps Run logs forever.
    #[serde(rename = "runLogKeepDays")]
    #[schema(required = true)]
    pub run_log_keep_days: Option<u32>,
    /// `null` keeps the audit log forever.
    #[serde(rename = "auditKeepMonths")]
    #[schema(required = true)]
    pub audit_keep_months: Option<u32>,
}

/// `remote`: the pinned-TLS remote listener (architecture §7). Changed only locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RemoteSettings {
    pub enabled: bool,
    /// The address it binds.
    pub listen: String,
    pub port: u16,
}

/// `run_path`: where runs get their PATH (#26).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RunPathSettings {
    pub source: RunPathSource,
    /// Absolute dirs put first on the Run PATH.
    pub prepend: Vec<String>,
}

wire_enum! {
    /// `run_path.source` (#26).
    pub enum RunPathSource {
        Auto = "auto",
        LoginShell = "login_shell",
        Registry = "registry",
        Inherit = "inherit",
    }
}

wire_enum! {
    /// `run_locale` (#43): `auto` adds a UTF-8 `LC_CTYPE` when the inherited env has none;
    /// `inherit` passes the env through.
    pub enum RunLocaleSetting {
        Auto = "auto",
        Inherit = "inherit",
    }
}

/// `timeout_breaker` (#32). Each limit is `null` when off; both `null` turns the breaker off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TimeoutBreakerSettings {
    #[schema(required = true)]
    pub max_consecutive_timeouts: Option<u32>,
    #[schema(required = true)]
    pub max_failing_seconds: Option<u64>,
}

/// `auto_compact` (#33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AutoCompactSettings {
    pub enabled: bool,
    pub timeout_secs: u32,
}

/// `inbox`: attachment retention (#44). `null` keeps forever / sets no cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InboxSettings {
    #[serde(rename = "keepDays")]
    #[schema(required = true)]
    pub keep_days: Option<u32>,
    #[serde(rename = "maxMiB")]
    #[schema(required = true)]
    pub max_mib: Option<u32>,
}

/// `disk`: the free-space floors (#44).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiskSettings {
    #[serde(rename = "warnFreeMiB")]
    pub warn_free_mib: u32,
    #[serde(rename = "minFreeMiB")]
    pub min_free_mib: u32,
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    SettingsView,
    SettingsApplied,
    Restarted,
    SettingsPatch,
    SecretState,
    Settings,
    FallbackSettings,
    AgenticSettings,
    AgenticMode,
    HeartbeatSettings,
    ExcludeWindow,
    DiscordSettings,
    SecuritySettings,
    SecurityLevel,
    SessionSettings,
    TimeoutSettings,
    LoggingSettings,
    RemoteSettings,
    RunPathSettings,
    RunPathSource,
    RunLocaleSetting,
    TimeoutBreakerSettings,
    AutoCompactSettings,
    InboxSettings,
    DiskSettings,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn settings() -> Settings {
        Settings {
            timezone: "UTC".into(),
            model: "opus".into(),
            api: SecretState { set: false },
            fallback: FallbackSettings {
                model: "sonnet".into(),
                api: SecretState { set: true },
            },
            agentic: AgenticSettings {
                enabled: true,
                default_mode: "implementation".into(),
                modes: vec![AgenticMode {
                    name: "planning".into(),
                    model: "opus".into(),
                    keywords: vec!["plan".into()],
                    phrases: None,
                }],
            },
            heartbeat: HeartbeatSettings {
                enabled: true,
                interval: 15,
                prompt: "check the inbox".into(),
                exclude_windows: vec![ExcludeWindow {
                    days: Some(vec![0, 6]),
                    start: "23:00".into(),
                    end: "07:00".into(),
                }],
            },
            discord: DiscordSettings {
                token: SecretState { set: true },
                allowed_user_ids: vec!["1111".into()],
                listen_channels: vec!["2222".into()],
                listen_guilds: vec![],
                allowed_guilds: vec!["3333".into()],
                channel_allowed_user_ids: BTreeMap::from([("2222".into(), vec!["4444".into()])]),
                channel_names: BTreeMap::from([("2222".into(), "general".into())]),
                image_output_roots: vec![],
                streaming: false,
            },
            security: SecuritySettings {
                level: SecurityLevel::Moderate,
                allowed_tools: vec![],
                disallowed_tools: vec!["WebFetch".into()],
            },
            session: SessionSettings {
                auto_rotate: false,
                max_messages: 50,
                max_age_hours: 24,
                summary_path: String::new(),
            },
            session_timeout_ms: 1_800_000,
            timeouts: TimeoutSettings {
                heartbeat: 15,
                job: 30,
                default: 5,
            },
            logging: LoggingSettings {
                level: "info".into(),
                daemon_keep_files: 14,
                daemon_max_daily_mib: 256,
                daemon_max_total_mib: 1024,
                run_log_keep_days: None,
                audit_keep_months: Some(12),
            },
            remote: RemoteSettings {
                enabled: false,
                listen: "0.0.0.0".into(),
                port: 4633,
            },
            run_path: RunPathSettings {
                source: RunPathSource::Auto,
                prepend: vec!["/opt/tools/bin".into()],
            },
            run_locale: RunLocaleSetting::Auto,
            timeout_breaker: TimeoutBreakerSettings {
                max_consecutive_timeouts: Some(3),
                max_failing_seconds: None,
            },
            auto_compact: AutoCompactSettings {
                enabled: true,
                timeout_secs: 300,
            },
            transcript_roots: vec![],
            inbox: InboxSettings {
                keep_days: Some(30),
                max_mib: None,
            },
            disk: DiskSettings {
                warn_free_mib: 2048,
                min_free_mib: 256,
            },
        }
    }

    fn settings_wire() -> Value {
        json!({
            "timezone": "UTC",
            "model": "opus",
            "api": {"set": false},
            "fallback": {"model": "sonnet", "api": {"set": true}},
            "agentic": {
                "enabled": true,
                "defaultMode": "implementation",
                "modes": [{"name": "planning", "model": "opus", "keywords": ["plan"]}],
            },
            "heartbeat": {
                "enabled": true,
                "interval": 15,
                "prompt": "check the inbox",
                "excludeWindows": [{"days": [0, 6], "start": "23:00", "end": "07:00"}],
            },
            "discord": {
                "token": {"set": true},
                "allowedUserIds": ["1111"],
                "listenChannels": ["2222"],
                "listenGuilds": [],
                "allowedGuilds": ["3333"],
                "channelAllowedUserIds": {"2222": ["4444"]},
                "channelNames": {"2222": "general"},
                "imageOutputRoots": [],
                "streaming": false,
            },
            "security": {"level": "moderate", "allowedTools": [], "disallowedTools": ["WebFetch"]},
            "session": {"autoRotate": false, "maxMessages": 50, "maxAgeHours": 24, "summaryPath": ""},
            "sessionTimeoutMs": 1800000,
            "timeouts": {"heartbeat": 15, "job": 30, "default": 5},
            "logging": {
                "level": "info",
                "daemonKeepFiles": 14,
                "daemonMaxDailyMiB": 256,
                "daemonMaxTotalMiB": 1024,
                "runLogKeepDays": null,
                "auditKeepMonths": 12,
            },
            "remote": {"enabled": false, "listen": "0.0.0.0", "port": 4633},
            "run_path": {"source": "auto", "prepend": ["/opt/tools/bin"]},
            "run_locale": "auto",
            "timeout_breaker": {"max_consecutive_timeouts": 3, "max_failing_seconds": null},
            "auto_compact": {"enabled": true, "timeout_secs": 300},
            "transcript_roots": [],
            "inbox": {"keepDays": 30, "maxMiB": null},
            "disk": {"warnFreeMiB": 2048, "minFreeMiB": 256},
        })
    }

    #[test]
    fn settings_keep_the_files_own_key_names() {
        assert_eq!(serde_json::to_value(settings()).unwrap(), settings_wire());
        assert_eq!(
            serde_json::from_value::<Settings>(settings_wire()).unwrap(),
            settings()
        );
    }

    #[test]
    fn a_secret_reads_as_set_and_never_its_value() {
        let wire = serde_json::to_value(settings()).unwrap();
        assert_eq!(wire["discord"]["token"], json!({"set": true}));
        assert_eq!(wire["fallback"]["api"], json!({"set": true}));
        assert_eq!(wire["api"], json!({"set": false}));
        // A token string in the file can't be read into the view: only `{set}` parses.
        let mut leaked = settings_wire();
        leaked["discord"]["token"] = json!("not-a-real-token");
        assert!(serde_json::from_value::<Settings>(leaked).is_err());
    }

    #[test]
    fn view_and_applied_shapes() {
        let view = SettingsView {
            revision: Revision::of(b"{}\n"),
            settings: settings(),
            ts_only_keys: vec!["web.port".into()],
            env_overrides: vec!["discord.token".into()],
        };
        let view_wire = json!({
            "revision": Revision::of(b"{}\n").to_string(),
            "settings": settings_wire(),
            "ts_only_keys": ["web.port"],
            "env_overrides": ["discord.token"],
        });
        assert_eq!(serde_json::to_value(&view).unwrap(), view_wire);
        assert_eq!(
            serde_json::from_value::<SettingsView>(view_wire.clone()).unwrap(),
            view
        );

        let applied = SettingsApplied {
            view,
            changed_keys: vec!["discord.token".into()],
            restarted: vec![Restarted::Discord],
        };
        let wire = json!({
            "view": view_wire,
            "changed_keys": ["discord.token"],
            "restarted": ["discord"],
        });
        assert_eq!(serde_json::to_value(&applied).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<SettingsApplied>(wire).unwrap(),
            applied
        );
    }

    #[test]
    fn a_patch_is_any_merge_patch_object() {
        let wire = json!({"discord": {"token": "secret-value"}, "inbox": {"keepDays": null}});
        let patch: SettingsPatch = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(patch.0["inbox"], json!({"keepDays": null}));
        assert_eq!(serde_json::to_value(&patch).unwrap(), wire);
        assert!(serde_json::from_value::<SettingsPatch>(json!([1])).is_err());

        let debug = format!("{patch:?}");
        assert!(!debug.contains("secret-value"), "{debug}");
        assert_eq!(debug, r#"SettingsPatch(["discord", "inbox"])"#);
    }

    #[test]
    fn content_stays_out_of_debug() {
        let debug = format!("{:?}", settings());
        assert!(!debug.contains("check the inbox"), "{debug}");
        assert!(debug.contains("prompt_len: 15"), "{debug}");
    }

    #[test]
    fn closed_sets_take_unknown_values() {
        assert_eq!(
            serde_json::from_value::<RunLocaleSetting>(json!("posix")).unwrap(),
            RunLocaleSetting::Unknown
        );
        assert_eq!(
            serde_json::to_value(RunPathSource::KNOWN).unwrap(),
            json!(["auto", "login_shell", "registry", "inherit"])
        );
        assert_eq!(
            serde_json::from_value::<SecurityLevel>(json!("paranoid")).unwrap(),
            SecurityLevel::Unknown
        );
    }
}
