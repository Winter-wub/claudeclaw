//! First-start import (management-api §2.11, #17): the import record, the plan that
//! `import.plan` and `claudeclaw import --dry-run --json` both return, and `import.apply`.
//!
//! The plan has one section per step (#17 §7, #22). A section is `null` when the plan's `only`
//! leaves its step out. No section carries a secret or a job's prompt: the legacy `apiToken` is
//! shown only as set and its length, and job lines appear only inside [`ImportFinding`]s.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::jobs::{ImportFinding, ImportLevel};
use crate::scopes::ScopeKind;
use crate::wire::wire_enum;

/// `import.get` → `daemon/state/import.json` plus the last plan (management-api §2.11, #17 §1,
/// §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportRecord {
    pub kind: ImportKind,
    /// The binary version that wrote the record.
    pub version: String,
    /// Every step the import ran or still has to run, in order; empty for a `fresh` dir.
    pub steps: Vec<ImportStepRecord>,
    /// The import report (`import-report-<ts>.md`), relative to the Daemon dir; `null` for a
    /// `fresh` dir.
    #[schema(required = true)]
    pub report: Option<String>,
    /// The backup dir (`import-backup/<ts>/`), relative to the Daemon dir; `null` when nothing
    /// was backed up.
    #[schema(required = true)]
    pub backup_dir: Option<String>,
    /// The last plan made by `import.plan` or the import itself; `null` when there is none.
    #[schema(required = true)]
    pub plan: Option<ImportPlan>,
}

wire_enum! {
    /// What the first start found (#17 §1).
    pub enum ImportKind {
        /// No `settings.json`: nothing to import.
        Fresh = "fresh",
        /// A 1.x data dir, imported.
        Imported = "imported",
    }
}

/// One step in an [`ImportRecord`] (#17 §1). A step with no `done_at` runs again on the next
/// start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportStepRecord {
    pub step: ImportStep,
    /// `null` until the step finishes.
    #[schema(required = true)]
    pub done_at: Option<DateTime<Utc>>,
    /// A one-line, content-free summary, as in the owner notification ("job state for 7
    /// jobs"); `null` until the step finishes.
    #[schema(required = true)]
    pub result: Option<String>,
}

wire_enum! {
    /// A First-start import step, in the order they run (#17 §1, #32).
    pub enum ImportStep {
        Backup = "backup",
        Launchers = "launchers",
        /// The legacy `apiToken`.
        Token = "token",
        /// The renamed settings (`watchdog.*` → `timeout_breaker`, #32).
        Settings = "settings",
        /// The job state seed.
        State = "state",
        /// The Job import (#12).
        Jobs = "jobs",
        /// The session bookkeeping.
        Sessions = "sessions",
        /// The report, audit and owner notification.
        Report = "report",
    }
}

wire_enum! {
    /// A step that `only=` / `--only` can select (management-api §2.11, #17 §7).
    pub enum ImportOnly {
        Launchers = "launchers",
        Token = "token",
        State = "state",
        Settings = "settings",
        Jobs = "jobs",
    }
}

/// `import.plan` → what an import would do, writing nothing (management-api §2.11, #17 §7).
/// `claudeclaw import --dry-run --json` prints the same type, and its `jobs` section is
/// `jobs import --dry-run --json` (#22).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportPlan {
    /// Send it back in [`ImportApplyRequest::plan_hash`] to apply exactly this plan. Opaque.
    pub plan_hash: String,
    /// The `error` items anywhere in the plan; the CLI exits 1 when this is not 0.
    pub errors: u32,
    /// The `warning` items anywhere in the plan.
    pub warnings: u32,
    #[schema(required = true)]
    pub launchers: Option<ImportLaunchers>,
    #[schema(required = true)]
    pub token: Option<ImportToken>,
    #[schema(required = true)]
    pub settings: Option<ImportSettings>,
    #[schema(required = true)]
    pub state: Option<ImportState>,
    #[schema(required = true)]
    pub jobs: Option<ImportJobs>,
    /// `null` when `only` is set.
    #[schema(required = true)]
    pub sessions: Option<ImportSessions>,
    /// The project's Claude Code files (#24); `null` when `only` is set.
    #[schema(required = true)]
    pub claude_code: Option<ImportClaudeCode>,
    /// Everything else: files left in place, adapters 2.x lacks, behaviour changes (#17 §6,
    /// #33, #38, #44). Empty when `only` is set.
    pub notes: Vec<ImportNote>,
}

/// One line of a plan that isn't a row of its own (#17 §6, §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportNote {
    pub level: ImportLevel,
    /// A stable `snake_case` code such as `telegram_unused` or `launcher_not_on_path`. New
    /// codes can appear within v1.
    pub code: String,
    /// Human-readable; never a secret or a Launcher's arguments.
    pub message: String,
}

/// The Launchers step (#17 §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportLaunchers {
    pub entries: Vec<ImportLauncher>,
    /// The PATH pre-flight, an ignored `directFallback`, and the like.
    pub notes: Vec<ImportNote>,
}

/// One 1.x `runtime.launcher(s)` entry and what it becomes (#17 §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportLauncher {
    /// `default` for `runtime.launcher`, else the 1.x scope key, verbatim.
    pub scope: String,
    /// The scope's `discord.channelNames` label; `null` when unknown.
    #[schema(required = true)]
    pub label: Option<String>,
    /// How 1.x reads the entry, e.g. `direct` or `ccs <profile>`.
    pub ts: String,
    pub action: LauncherImportAction,
    /// The Launcher the scope will have: `direct` or a path relative to the Daemon dir.
    pub launcher: String,
    pub level: ImportLevel,
}

wire_enum! {
    /// What the import does with a 1.x Launcher entry (#17 §2).
    pub enum LauncherImportAction {
        /// `direct` is written for the scope.
        Direct = "direct",
        /// A ccs script is generated in `daemon/launchers/` and the scope points at it.
        Script = "script",
        /// The scope points at a script that is never created, so its runs are refused as they
        /// failed in 1.x (ccs with no profile, or a profile name Windows can't take).
        Refused = "refused",
        /// The Rust value is already the same; nothing is written.
        Unchanged = "unchanged",
        /// A different Rust value is already set and wins; reported, never changed.
        Kept = "kept",
    }
}

/// The legacy `apiToken` step (#17 §3). The value itself is never shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportToken {
    /// Whether `settings.apiToken` is a non-blank string.
    pub set: bool,
    /// Its length in characters, trimmed; `null` when not set.
    #[schema(required = true)]
    pub length: Option<u32>,
    pub action: TokenImportAction,
    /// A weak token, `web.enabled` with no listener, `web.token` left in place.
    pub notes: Vec<ImportNote>,
}

wire_enum! {
    /// What the import does with the legacy `apiToken` (#17 §3).
    pub enum TokenImportAction {
        /// No `apiToken`: nothing to do.
        Absent = "absent",
        /// It becomes the `legacy-apiToken` token with the `send` permission.
        Import = "import",
        /// Already imported with this value (or revoked with it); nothing changes.
        Unchanged = "unchanged",
        /// The value changed: the old legacy token is revoked and the new one imported.
        Replace = "replace",
        /// The key was removed: the legacy token is revoked.
        Revoke = "revoke",
    }
}

/// The renamed-settings step (#32): valid `watchdog.*` values copied to `timeout_breaker`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportSettings {
    pub keys: Vec<ImportSettingKey>,
    pub notes: Vec<ImportNote>,
}

/// One renamed key (#32). The old key is left in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportSettingKey {
    /// The 1.x key, e.g. `watchdog.timeoutMinutes`.
    pub from: String,
    /// The 2.x key it is copied to.
    pub to: String,
    pub action: SettingImportAction,
    pub level: ImportLevel,
}

wire_enum! {
    /// What the import does with a renamed key (#32).
    pub enum SettingImportAction {
        /// The value is copied.
        Copy = "copy",
        /// Both keys already agree.
        Unchanged = "unchanged",
        /// The 2.x key holds a different value and wins; the difference is reported.
        Kept = "kept",
        /// The 1.x value is not valid for the 2.x key and is not copied.
        Invalid = "invalid",
    }
}

/// The job state step (#17 §4), with the `jobs check` schedule view computed against the merged
/// state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportState {
    pub jobs: Vec<ImportJobState>,
    /// 1.x runs in flight when 1.x stopped (`active-runs`).
    pub in_flight_runs: u32,
    /// Logs stamped in the future, and the like.
    pub notes: Vec<ImportNote>,
}

/// One job's seeded state (#17 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportJobState {
    pub job: String,
    /// The start time in the newest matching Run log name; `null` when the job never ran.
    #[schema(required = true)]
    pub last_started_at: Option<DateTime<Utc>>,
    /// From the newest log's `Exit code:` line; `null` when the job never ran or the line is
    /// missing (the result is unknown).
    #[schema(required = true)]
    pub last_exit_code: Option<i32>,
    /// The due slot before now; `null` when the schedule has none.
    #[schema(required = true)]
    pub previous_slot: Option<DateTime<Utc>>,
    /// Whether a start now would catch the job up.
    pub catch_up: bool,
    /// The next fires under 2.x.
    pub next_fires: Vec<DateTime<Utc>>,
}

/// The Job import step (#12 §5, #22 `jobs import`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportJobs {
    pub files: Vec<ImportJobFile>,
}

/// One job file in the Job import (#22 `jobs import`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportJobFile {
    /// The job name: the file stem, or `<agent>/<stem>`.
    pub job: String,
    /// Relative to the Daemon dir.
    pub path: String,
    pub action: JobImportAction,
    /// The findings with their line diffs (#17 §5).
    pub findings: Vec<ImportFinding>,
}

wire_enum! {
    /// What the Job import does with a file (#22 `jobs import`).
    pub enum JobImportAction {
        /// Kept as is.
        Unchanged = "unchanged",
        /// Lines are rewritten, inserted or commented out.
        Rewrite = "rewrite",
        /// Disabled because it never fired in 1.x.
        Disable = "disable",
        /// Invalid in both daemons; left alone.
        Invalid = "invalid",
    }
}

/// The session bookkeeping step (#17 §6): 1.x sessions are read in place and resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportSessions {
    /// Sessions that will resume, per Scope kind.
    pub resume: Vec<ImportSessionCount>,
    /// Whether a `pending-resume.json` will be consumed.
    pub pending_resume: bool,
}

/// How many sessions of one Scope kind resume (#17 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportSessionCount {
    pub kind: ScopeKind,
    pub sessions: u32,
}

/// The project's Claude Code files (#24 D3, D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportClaudeCode {
    pub statusline: StatuslineAction,
    pub state_json: StateJsonAction,
}

wire_enum! {
    /// What happens to `.claude/settings.json`'s `statusLine` (#24 D3).
    pub enum StatuslineAction {
        /// Absent: `claudeclaw statusline` is installed.
        Install = "install",
        /// TS's entry is switched to `claudeclaw statusline`.
        Migrate = "migrate",
        /// Already ours.
        Unchanged = "unchanged",
        /// Someone else's; left alone and reported as info.
        LeaveForeign = "leave_foreign",
        /// The file doesn't parse; never written.
        Unreadable = "unreadable",
    }
}

wire_enum! {
    /// What happens to TS's `state.json` (#24 D4).
    pub enum StateJsonAction {
        /// A leftover is deleted at start.
        Remove = "remove",
        /// There is none.
        Absent = "absent",
    }
}

/// `import.apply`'s body (management-api §2.11). Every field is optional: an empty body
/// applies every step to every job.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct ImportApplyRequest {
    /// Only these steps (`--only`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<ImportOnly>>,
    /// Only these jobs, by name (`jobs import <job>…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jobs: Option<Vec<String>>,
    /// The previewed [`ImportPlan::plan_hash`]; `409 plan_changed` when it no longer matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,
}

/// `import.apply` → the updated record (management-api §2.11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportResult {
    pub record: ImportRecord,
    /// This run's report, relative to the Daemon dir.
    pub report: String,
    /// This run's backup dir, relative to the Daemon dir; `null` when nothing was modified.
    #[schema(required = true)]
    pub backup_dir: Option<String>,
}

/// The schemas of this module, merged by [`crate::openapi`].
#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    ImportRecord,
    ImportKind,
    ImportStepRecord,
    ImportStep,
    ImportOnly,
    ImportPlan,
    ImportNote,
    ImportLaunchers,
    ImportLauncher,
    LauncherImportAction,
    ImportToken,
    TokenImportAction,
    ImportSettings,
    ImportSettingKey,
    SettingImportAction,
    ImportState,
    ImportJobState,
    ImportJobs,
    ImportJobFile,
    JobImportAction,
    ImportSessions,
    ImportSessionCount,
    ImportClaudeCode,
    StatuslineAction,
    StateJsonAction,
    ImportApplyRequest,
    ImportResult,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::jobs::{ImportFindingKind, ImportLineEdit};

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, h, 0, 0).unwrap()
    }

    fn note() -> ImportNote {
        ImportNote {
            level: ImportLevel::Warning,
            code: "launcher_not_on_path".into(),
            message: "ccs is not on the Run PATH".into(),
        }
    }

    fn note_wire() -> serde_json::Value {
        json!({
            "level": "warning",
            "code": "launcher_not_on_path",
            "message": "ccs is not on the Run PATH",
        })
    }

    fn plan() -> ImportPlan {
        ImportPlan {
            plan_hash: "0123456789abcdef".into(),
            errors: 0,
            warnings: 2,
            launchers: Some(ImportLaunchers {
                entries: vec![ImportLauncher {
                    scope: "1234".into(),
                    label: Some("general".into()),
                    ts: "ccs work".into(),
                    action: LauncherImportAction::Script,
                    launcher: "daemon/launchers/ccs-work".into(),
                    level: ImportLevel::Warning,
                }],
                notes: vec![note()],
            }),
            token: Some(ImportToken {
                set: true,
                length: Some(43),
                action: TokenImportAction::Import,
                notes: vec![],
            }),
            settings: Some(ImportSettings {
                keys: vec![ImportSettingKey {
                    from: "watchdog.timeoutMinutes".into(),
                    to: "timeout_breaker.minutes".into(),
                    action: SettingImportAction::Copy,
                    level: ImportLevel::Info,
                }],
                notes: vec![],
            }),
            state: Some(ImportState {
                jobs: vec![ImportJobState {
                    job: "daily-report".into(),
                    last_started_at: Some(at(9)),
                    last_exit_code: Some(0),
                    previous_slot: Some(at(9)),
                    catch_up: false,
                    next_fires: vec![at(21)],
                }],
                in_flight_runs: 0,
                notes: vec![],
            }),
            jobs: Some(ImportJobs {
                files: vec![ImportJobFile {
                    job: "daily-report".into(),
                    path: "jobs/daily-report.md".into(),
                    action: JobImportAction::Rewrite,
                    findings: vec![ImportFinding {
                        level: ImportLevel::Warning,
                        kind: ImportFindingKind::DailyConverted,
                        edits: vec![ImportLineEdit {
                            line: 3,
                            before: Some("daily: true".into()),
                            after: Some("recurring: true".into()),
                        }],
                        ts_reading: None,
                        rust_reading: None,
                        ts_next_fires: vec![],
                        rust_next_fires: vec![],
                        reason: "#12 §3".into(),
                    }],
                }],
            }),
            sessions: Some(ImportSessions {
                resume: vec![ImportSessionCount {
                    kind: ScopeKind::Channel,
                    sessions: 3,
                }],
                pending_resume: false,
            }),
            claude_code: Some(ImportClaudeCode {
                statusline: StatuslineAction::Migrate,
                state_json: StateJsonAction::Remove,
            }),
            notes: vec![ImportNote {
                level: ImportLevel::Info,
                code: "inbox_retention".into(),
                message: "Inbox files older than 30 days will be deleted".into(),
            }],
        }
    }

    fn plan_wire() -> serde_json::Value {
        json!({
            "plan_hash": "0123456789abcdef",
            "errors": 0,
            "warnings": 2,
            "launchers": {
                "entries": [{
                    "scope": "1234",
                    "label": "general",
                    "ts": "ccs work",
                    "action": "script",
                    "launcher": "daemon/launchers/ccs-work",
                    "level": "warning",
                }],
                "notes": [note_wire()],
            },
            "token": {"set": true, "length": 43, "action": "import", "notes": []},
            "settings": {
                "keys": [{
                    "from": "watchdog.timeoutMinutes",
                    "to": "timeout_breaker.minutes",
                    "action": "copy",
                    "level": "info",
                }],
                "notes": [],
            },
            "state": {
                "jobs": [{
                    "job": "daily-report",
                    "last_started_at": "2026-10-14T09:00:00Z",
                    "last_exit_code": 0,
                    "previous_slot": "2026-10-14T09:00:00Z",
                    "catch_up": false,
                    "next_fires": ["2026-10-14T21:00:00Z"],
                }],
                "in_flight_runs": 0,
                "notes": [],
            },
            "jobs": {
                "files": [{
                    "job": "daily-report",
                    "path": "jobs/daily-report.md",
                    "action": "rewrite",
                    "findings": [{
                        "level": "warning",
                        "kind": "daily_converted",
                        "edits": [{"line": 3, "before": "daily: true", "after": "recurring: true"}],
                        "ts_reading": null,
                        "rust_reading": null,
                        "ts_next_fires": [],
                        "rust_next_fires": [],
                        "reason": "#12 §3",
                    }],
                }],
            },
            "sessions": {"resume": [{"kind": "channel", "sessions": 3}], "pending_resume": false},
            "claude_code": {"statusline": "migrate", "state_json": "remove"},
            "notes": [{
                "level": "info",
                "code": "inbox_retention",
                "message": "Inbox files older than 30 days will be deleted",
            }],
        })
    }

    fn record() -> ImportRecord {
        ImportRecord {
            kind: ImportKind::Imported,
            version: "2.0.0".into(),
            steps: vec![
                ImportStepRecord {
                    step: ImportStep::Backup,
                    done_at: Some(at(9)),
                    result: Some("2 files".into()),
                },
                ImportStepRecord {
                    step: ImportStep::Launchers,
                    done_at: None,
                    result: None,
                },
            ],
            report: Some("import-report-20261014T090000Z.md".into()),
            backup_dir: Some("import-backup/20261014T090000Z".into()),
            plan: None,
        }
    }

    fn record_wire() -> serde_json::Value {
        json!({
            "kind": "imported",
            "version": "2.0.0",
            "steps": [
                {"step": "backup", "done_at": "2026-10-14T09:00:00Z", "result": "2 files"},
                {"step": "launchers", "done_at": null, "result": null},
            ],
            "report": "import-report-20261014T090000Z.md",
            "backup_dir": "import-backup/20261014T090000Z",
            "plan": null,
        })
    }

    fn round_trip<T>(value: &T, wire: serde_json::Value)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), wire);
        assert_eq!(&serde_json::from_value::<T>(wire).unwrap(), value);
    }

    #[test]
    fn plan_shape() {
        round_trip(&plan(), plan_wire());
    }

    #[test]
    fn an_only_plan_sends_its_nulls() {
        let only_jobs = ImportPlan {
            launchers: None,
            token: None,
            settings: None,
            state: None,
            sessions: None,
            claude_code: None,
            notes: vec![],
            ..plan()
        };
        let wire = serde_json::to_value(&only_jobs).unwrap();
        for section in [
            "launchers",
            "token",
            "settings",
            "state",
            "sessions",
            "claude_code",
        ] {
            assert_eq!(wire[section], json!(null), "{section}");
        }
        assert_eq!(
            serde_json::from_value::<ImportPlan>(wire).unwrap(),
            only_jobs
        );
    }

    #[test]
    fn record_shapes() {
        round_trip(&record(), record_wire());

        let fresh = ImportRecord {
            kind: ImportKind::Fresh,
            version: "2.0.0".into(),
            steps: vec![],
            report: None,
            backup_dir: None,
            plan: None,
        };
        round_trip(
            &fresh,
            json!({
                "kind": "fresh",
                "version": "2.0.0",
                "steps": [],
                "report": null,
                "backup_dir": null,
                "plan": null,
            }),
        );

        let with_plan = ImportRecord {
            plan: Some(plan()),
            ..record()
        };
        let mut wire = record_wire();
        wire["plan"] = plan_wire();
        round_trip(&with_plan, wire);
    }

    #[test]
    fn apply_request_shapes() {
        round_trip(&ImportApplyRequest::default(), json!({}));
        round_trip(
            &ImportApplyRequest {
                only: Some(vec![ImportOnly::Jobs]),
                jobs: Some(vec!["researcher/paper-digest".into()]),
                plan_hash: Some("0123456789abcdef".into()),
            },
            json!({
                "only": ["jobs"],
                "jobs": ["researcher/paper-digest"],
                "plan_hash": "0123456789abcdef",
            }),
        );
    }

    #[test]
    fn result_shape() {
        round_trip(
            &ImportResult {
                record: record(),
                report: "import-report-20261014T090000Z.md".into(),
                backup_dir: None,
            },
            json!({
                "record": record_wire(),
                "report": "import-report-20261014T090000Z.md",
                "backup_dir": null,
            }),
        );
    }

    #[test]
    fn unknown_values_parse() {
        assert_eq!(
            serde_json::from_value::<ImportKind>(json!("partial")).unwrap(),
            ImportKind::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ImportStep>(json!("inbox")).unwrap(),
            ImportStep::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ImportOnly>(json!("inbox")).unwrap(),
            ImportOnly::Unknown
        );
        assert_eq!(
            serde_json::from_value::<LauncherImportAction>(json!("ask")).unwrap(),
            LauncherImportAction::Unknown
        );
        assert_eq!(
            serde_json::from_value::<TokenImportAction>(json!("rotate")).unwrap(),
            TokenImportAction::Unknown
        );
        assert_eq!(
            serde_json::from_value::<SettingImportAction>(json!("merge")).unwrap(),
            SettingImportAction::Unknown
        );
        assert_eq!(
            serde_json::from_value::<JobImportAction>(json!("split")).unwrap(),
            JobImportAction::Unknown
        );
        assert_eq!(
            serde_json::from_value::<StatuslineAction>(json!("compose")).unwrap(),
            StatuslineAction::Unknown
        );
        assert_eq!(
            serde_json::from_value::<StateJsonAction>(json!("rename")).unwrap(),
            StateJsonAction::Unknown
        );
    }
}
