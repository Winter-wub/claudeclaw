//! Scopes and Launchers (management-api §2.5): `scopes.list`, `launchers.list`, `launchers.set`,
//! `launchers.unset` and `launchers.test`. A Launcher is `"direct"` or an absolute path (#8); it
//! never appears in a URL path (§1.7).

use std::ops::RangeInclusive;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{PartialSchema, ToSchema};

use crate::deadline::{Call, Deadline};
use crate::scope::Scope;
use crate::sessions::ScopeSession;
use crate::wire::wire_enum;

/// `scopes.list` → `{scopes}` (management-api §2.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ScopeList {
    pub scopes: Vec<ScopeInfo>,
}

/// One Scope for the Chat picker and the "Who uses which launcher" table (management-api §2.5).
/// Listed: `global`, every Discord channel or thread with config, a session or an override,
/// every job and every agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ScopeInfo {
    pub scope: Scope,
    pub kind: ScopeKind,
    /// A display name. The app shows it and copies `scope` (#16).
    pub label: String,
    /// The Scope this one inherits from: a thread's channel. `null` when there is none.
    #[schema(required = true)]
    pub parent: Option<Scope>,
    pub launcher: ScopeLauncher,
    /// The session of the effective Launcher; `null` when there is none.
    #[schema(required = true)]
    pub session: Option<ScopeSession>,
    pub queue: ScopeQueue,
}

wire_enum! {
    /// What a Scope is (management-api §2.5).
    pub enum ScopeKind {
        Global = "global",
        Channel = "channel",
        Thread = "thread",
        Job = "job",
        Agent = "agent",
    }
}

/// A Scope's Turn queue (management-api §2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScopeQueue {
    /// Whether a Turn is running in the Scope now.
    pub running: bool,
    /// Turns waiting behind it.
    pub depth: u32,
}

/// One row of the scope → effective-Launcher table (management-api §2.5, #8, #22). Also embedded
/// in `DiscordChannel` and `AgentInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ScopeLauncher {
    pub scope: Scope,
    /// The override set on this Scope itself: `"direct"`, an absolute path, or `null`.
    #[schema(required = true)]
    pub set_here: Option<String>,
    /// The Launcher a Turn here runs with.
    pub effective: String,
    /// Where `effective` comes from: a Scope up the chain (thread → channel → global), or
    /// `default`. Names this Scope itself when `set_here` or the job frontmatter sets it.
    pub inherited_from: InheritedFrom,
    pub source: LauncherSource,
}

wire_enum! {
    /// Which setting decided a Scope's effective Launcher (management-api §2.5).
    pub enum LauncherSource {
        /// The `launcher` key in `settings.json`.
        Settings = "settings",
        /// A job's frontmatter `launcher:` key (§2.4).
        JobFrontmatter = "job_frontmatter",
        /// No override anywhere in the chain.
        Default = "default",
    }
}

/// `inherited_from`: a Scope, or `"default"` when nothing in the chain sets a Launcher
/// (management-api §2.5).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InheritedFrom {
    Default,
    Scope(Scope),
}

impl Serialize for InheritedFrom {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            InheritedFrom::Default => serializer.serialize_str("default"),
            InheritedFrom::Scope(scope) => scope.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for InheritedFrom {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "default" {
            return Ok(InheritedFrom::Default);
        }
        s.parse()
            .map(InheritedFrom::Scope)
            .map_err(serde::de::Error::custom)
    }
}

impl PartialSchema for InheritedFrom {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .pattern(Some(
                r"^(default|global|discord:[0-9]+|job:[^/]+(/[^/]+)?|agent:[^/]+)$",
            ))
            .description(Some(
                "A Scope, or `default` when nothing in the chain sets one.",
            ))
            .examples(["default", "discord:1234"])
            .into()
    }
}

impl ToSchema for InheritedFrom {}

/// `launchers.list` → the Launchers screen (management-api §2.5, #22 `launcher list`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LauncherOverview {
    /// The default Launcher: `"direct"` or an absolute path.
    pub default: String,
    /// `direct` plus every path in use (#8).
    pub picker: Vec<String>,
    pub launchers: Vec<LauncherInfo>,
    pub scopes: Vec<ScopeLauncher>,
}

/// One Launcher card (management-api §2.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LauncherInfo {
    pub launcher: String,
    /// `true` for `direct`.
    pub builtin: bool,
    pub runnable: Runnable,
    /// The last Test launcher result, kept in `daemon/state/launcher-tests.json`.
    #[schema(required = true)]
    pub last_test: Option<LastTest>,
    pub used_by: Vec<LauncherUse>,
    /// The Claude Code version seen through this Launcher, from its newest run or test (#42).
    #[schema(required = true)]
    pub claude_code: Option<ClaudeCodeSeen>,
}

/// Whether a Launcher can be run now (management-api §2.5, #8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Runnable {
    pub ok: bool,
    /// Why not; `null` when `ok`.
    #[schema(required = true)]
    pub reason: Option<NotRunnable>,
}

wire_enum! {
    /// Why a Launcher path can't be run (management-api §2.5).
    pub enum NotRunnable {
        Missing = "missing",
        NotFile = "not_file",
        NotExecutable = "not_executable",
    }
}

/// The summary of a Launcher's last test (management-api §2.5, #42).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LastTest {
    pub ok: bool,
    pub tested_at: DateTime<Utc>,
    pub failed_checks: Vec<LauncherCheckId>,
    /// The Claude Code version the test saw; `null` when it saw none (#42 D5).
    #[schema(required = true)]
    pub claude_code_version: Option<String>,
}

/// A Scope that uses a Launcher (management-api §2.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LauncherUse {
    pub scope: Scope,
    pub label: String,
    pub via: LauncherVia,
}

wire_enum! {
    /// Where a Scope sets the Launcher it uses (management-api §2.5).
    pub enum LauncherVia {
        Settings = "settings",
        JobFrontmatter = "job_frontmatter",
    }
}

/// The Claude Code version seen through a Launcher (#42 D5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ClaudeCodeSeen {
    pub version: String,
    pub seen_at: DateTime<Utc>,
    /// Below the minimum ClaudeClaw supports (#42 D4).
    pub outdated: bool,
}

/// `launchers.set` body (management-api §2.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LauncherSet {
    /// `"direct"` or an absolute path.
    pub launcher: String,
}

/// `launchers.set` → the Scope's new Launcher and what happened to its session (management-api
/// §2.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LauncherSetResult {
    pub scope: Scope,
    pub launcher: String,
    pub effective: String,
    /// `resumed` when the Scope already had a session with this Launcher (#8).
    pub session: SessionStart,
}

wire_enum! {
    /// Whether a Scope's next Turn resumes a session or starts one (management-api §2.5).
    pub enum SessionStart {
        Resumed = "resumed",
        New = "new",
    }
}

/// `launchers.unset` → the Launcher the Scope now inherits (management-api §2.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LauncherUnset {
    pub scope: Scope,
    pub effective: String,
    pub inherited_from: InheritedFrom,
}

/// `launchers.test` body (management-api §2.5, #40). A request for a Launcher already under test
/// joins that test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LauncherTestRequest {
    /// The Launcher to test; the effective Launcher of `scope` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
    #[serde(default = "global")]
    #[schema(default = "global")]
    pub scope: Scope,
    /// Seconds the test run may take, 10–600 (`422 invalid_value` otherwise).
    #[serde(default = "default_timeout_s")]
    #[schema(default = 120, minimum = 10, maximum = 600)]
    pub timeout_s: u32,
}

fn global() -> Scope {
    Scope::Global
}

fn default_timeout_s() -> u32 {
    Deadline::DEFAULT_TEST_TIMEOUT_S
}

impl LauncherTestRequest {
    /// The `timeout_s` values the server accepts (#40 D6).
    pub const TIMEOUT_S: RangeInclusive<u32> = 10..=600;

    pub fn timeout_valid(&self) -> bool {
        Self::TIMEOUT_S.contains(&self.timeout_s)
    }

    /// What this request's [`Deadline::Test`] depends on.
    pub fn call(&self) -> Call {
        Call {
            timeout_s: Some(self.timeout_s),
            ..Call::default()
        }
    }
}

impl Default for LauncherTestRequest {
    fn default() -> Self {
        LauncherTestRequest {
            launcher: None,
            scope: Scope::Global,
            timeout_s: Deadline::DEFAULT_TEST_TIMEOUT_S,
        }
    }
}

/// `launchers.test` → every check's result (management-api §2.5, #22 `launcher test`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LauncherTestResult {
    pub launcher: String,
    pub scope: Scope,
    /// `false` if any check that isn't `advisory` failed.
    pub ok: bool,
    pub checks: Vec<LauncherCheck>,
    /// The Claude Code version the test run reported; `null` when it reported none (#42).
    #[schema(required = true)]
    pub claude_code: Option<ClaudeCodeSeen>,
    pub duration_ms: u64,
    pub tested_at: DateTime<Utc>,
}

/// One Test launcher check (management-api §2.5). Its `detail` never shows the Launcher's
/// arguments (#8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LauncherCheck {
    pub id: LauncherCheckId,
    pub ok: bool,
    /// An advisory check never makes the result's `ok` false (#39 D6).
    pub advisory: bool,
    pub detail: String,
}

wire_enum! {
    /// The Test launcher's checks, in the order they run (management-api §2.5).
    pub enum LauncherCheckId {
        Runnable = "runnable",
        PrintStreamJson = "print_stream_json",
        CleanStdout = "clean_stdout",
        SystemPrompt = "system_prompt",
        ResultEvent = "result_event",
        /// #25.
        PromptStdin = "prompt_stdin",
        /// #42.
        ClaudeCodeVersion = "claude_code_version",
        /// #39, advisory.
        Transcript = "transcript",
        /// #37, a warning.
        Stragglers = "stragglers",
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    ScopeList,
    ScopeInfo,
    ScopeKind,
    ScopeQueue,
    ScopeLauncher,
    LauncherSource,
    InheritedFrom,
    LauncherOverview,
    LauncherInfo,
    Runnable,
    NotRunnable,
    LastTest,
    LauncherUse,
    LauncherVia,
    ClaudeCodeSeen,
    LauncherSet,
    LauncherSetResult,
    SessionStart,
    LauncherUnset,
    LauncherTestRequest,
    LauncherTestResult,
    LauncherCheck,
    LauncherCheckId,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::sessions::SessionStatus;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, hour, 12, 0).unwrap()
    }

    fn channel_launcher() -> ScopeLauncher {
        ScopeLauncher {
            scope: Scope::Discord("1234".into()),
            set_here: None,
            effective: "direct".into(),
            inherited_from: InheritedFrom::Scope(Scope::Global),
            source: LauncherSource::Settings,
        }
    }

    #[test]
    fn scope_info_shape() {
        let info = ScopeInfo {
            scope: Scope::Discord("5678".into()),
            kind: ScopeKind::Thread,
            label: "research".into(),
            parent: Some(Scope::Discord("1234".into())),
            launcher: channel_launcher(),
            session: Some(ScopeSession {
                launcher: "direct".into(),
                session_id: "s-1".into(),
                status: SessionStatus::InUse,
            }),
            queue: ScopeQueue {
                running: true,
                depth: 2,
            },
        };
        let wire = json!({
            "scope": "discord:5678",
            "kind": "thread",
            "label": "research",
            "parent": "discord:1234",
            "launcher": {
                "scope": "discord:1234",
                "set_here": null,
                "effective": "direct",
                "inherited_from": "global",
                "source": "settings",
            },
            "session": {"launcher": "direct", "session_id": "s-1", "status": "in_use"},
            "queue": {"running": true, "depth": 2},
        });
        assert_eq!(serde_json::to_value(&info).unwrap(), wire);
        assert_eq!(serde_json::from_value::<ScopeInfo>(wire).unwrap(), info);
    }

    #[test]
    fn inherited_from_is_a_scope_or_default() {
        for (value, wire) in [
            (InheritedFrom::Default, "default"),
            (InheritedFrom::Scope(Scope::Global), "global"),
            (InheritedFrom::Scope(Scope::Job("a/b".into())), "job:a/b"),
        ] {
            assert_eq!(serde_json::to_value(&value).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<InheritedFrom>(json!(wire)).unwrap(),
                value
            );
        }
        for bad in ["", "Default", "discord:x", "settings"] {
            assert!(serde_json::from_value::<InheritedFrom>(json!(bad)).is_err());
        }
    }

    #[test]
    fn launcher_overview_shape() {
        let overview = LauncherOverview {
            default: "direct".into(),
            picker: vec!["direct".into(), "/opt/launchers/work".into()],
            launchers: vec![LauncherInfo {
                launcher: "/opt/launchers/work".into(),
                builtin: false,
                runnable: Runnable {
                    ok: false,
                    reason: Some(NotRunnable::NotExecutable),
                },
                last_test: Some(LastTest {
                    ok: false,
                    tested_at: at(9),
                    failed_checks: vec![LauncherCheckId::ClaudeCodeVersion],
                    claude_code_version: Some("2.1.0".into()),
                }),
                used_by: vec![LauncherUse {
                    scope: Scope::Job("daily".into()),
                    label: "daily".into(),
                    via: LauncherVia::JobFrontmatter,
                }],
                claude_code: Some(ClaudeCodeSeen {
                    version: "2.1.0".into(),
                    seen_at: at(9),
                    outdated: true,
                }),
            }],
            scopes: vec![channel_launcher()],
        };
        let wire = json!({
            "default": "direct",
            "picker": ["direct", "/opt/launchers/work"],
            "launchers": [{
                "launcher": "/opt/launchers/work",
                "builtin": false,
                "runnable": {"ok": false, "reason": "not_executable"},
                "last_test": {
                    "ok": false,
                    "tested_at": "2026-10-14T09:12:00Z",
                    "failed_checks": ["claude_code_version"],
                    "claude_code_version": "2.1.0",
                },
                "used_by": [{"scope": "job:daily", "label": "daily", "via": "job_frontmatter"}],
                "claude_code": {
                    "version": "2.1.0",
                    "seen_at": "2026-10-14T09:12:00Z",
                    "outdated": true,
                },
            }],
            "scopes": [{
                "scope": "discord:1234",
                "set_here": null,
                "effective": "direct",
                "inherited_from": "global",
                "source": "settings",
            }],
        });
        assert_eq!(serde_json::to_value(&overview).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<LauncherOverview>(wire).unwrap(),
            overview
        );
    }

    #[test]
    fn set_and_unset_shapes() {
        let set: LauncherSet = serde_json::from_value(json!({"launcher": "direct"})).unwrap();
        assert_eq!(set.launcher, "direct");

        let result = LauncherSetResult {
            scope: Scope::Agent("researcher".into()),
            launcher: "direct".into(),
            effective: "direct".into(),
            session: SessionStart::Resumed,
        };
        let wire = json!({
            "scope": "agent:researcher",
            "launcher": "direct",
            "effective": "direct",
            "session": "resumed",
        });
        assert_eq!(serde_json::to_value(&result).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<LauncherSetResult>(wire).unwrap(),
            result
        );

        let unset = LauncherUnset {
            scope: Scope::Discord("1234".into()),
            effective: "direct".into(),
            inherited_from: InheritedFrom::Default,
        };
        let wire =
            json!({"scope": "discord:1234", "effective": "direct", "inherited_from": "default"});
        assert_eq!(serde_json::to_value(&unset).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<LauncherUnset>(wire).unwrap(),
            unset
        );
    }

    #[test]
    fn test_request_defaults_and_bounds() {
        let request: LauncherTestRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(request, LauncherTestRequest::default());
        assert_eq!(request.timeout_s, 120);
        assert_eq!(request.scope, Scope::Global);
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"scope": "global", "timeout_s": 120})
        );
        assert_eq!(
            Deadline::Test.client(request.call()).map(|d| d.as_secs()),
            Some(165)
        );

        let request: LauncherTestRequest = serde_json::from_value(
            json!({"launcher": "/opt/launchers/work", "scope": "job:daily", "timeout_s": 600}),
        )
        .unwrap();
        assert_eq!(request.launcher.as_deref(), Some("/opt/launchers/work"));
        assert!(request.timeout_valid());
        for (timeout_s, valid) in [(9, false), (10, true), (600, true), (601, false)] {
            let request = LauncherTestRequest {
                timeout_s,
                ..LauncherTestRequest::default()
            };
            assert_eq!(request.timeout_valid(), valid, "{timeout_s}");
        }
    }

    #[test]
    fn test_result_shape() {
        let result = LauncherTestResult {
            launcher: "direct".into(),
            scope: Scope::Global,
            ok: true,
            checks: vec![
                LauncherCheck {
                    id: LauncherCheckId::Runnable,
                    ok: true,
                    advisory: false,
                    detail: "found on the Run PATH".into(),
                },
                LauncherCheck {
                    id: LauncherCheckId::Transcript,
                    ok: false,
                    advisory: true,
                    detail: "not located".into(),
                },
            ],
            claude_code: Some(ClaudeCodeSeen {
                version: "2.1.296".into(),
                seen_at: at(9),
                outdated: false,
            }),
            duration_ms: 8123,
            tested_at: at(9),
        };
        let wire = json!({
            "launcher": "direct",
            "scope": "global",
            "ok": true,
            "checks": [
                {"id": "runnable", "ok": true, "advisory": false, "detail": "found on the Run PATH"},
                {"id": "transcript", "ok": false, "advisory": true, "detail": "not located"},
            ],
            "claude_code": {"version": "2.1.296", "seen_at": "2026-10-14T09:12:00Z", "outdated": false},
            "duration_ms": 8123,
            "tested_at": "2026-10-14T09:12:00Z",
        });
        assert_eq!(serde_json::to_value(&result).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<LauncherTestResult>(wire).unwrap(),
            result
        );
    }

    #[test]
    fn the_nine_check_ids() {
        assert_eq!(
            serde_json::to_value(LauncherCheckId::KNOWN).unwrap(),
            json!([
                "runnable",
                "print_stream_json",
                "clean_stdout",
                "system_prompt",
                "result_event",
                "prompt_stdin",
                "claude_code_version",
                "transcript",
                "stragglers",
            ])
        );
        assert_eq!(
            serde_json::from_value::<LauncherCheckId>(json!("added_later")).unwrap(),
            LauncherCheckId::Unknown
        );
    }
}
