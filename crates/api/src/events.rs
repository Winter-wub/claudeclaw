//! The `events.stream` bus and the frames every SSE stream shares (management-api §1.6, §2.2;
//! #23 D5, D6).
//!
//! [`DaemonEvent`] is content-free, the same rule as Daemon logs (#23 D6): no reply text,
//! prompts, job bodies or heartbeat output. The app links to the Run log or Turn instead.

use serde::{Deserialize, Serialize};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{PartialSchema, ToSchema};

use crate::adapter::{AdapterState, Transport};
use crate::revision::Revision;
use crate::runs::RunRecord;
use crate::scope::Scope;
use crate::status::{DaemonHealth, Problem};
use crate::turns::TurnOutcome;
use crate::wire::wire_enum;

/// One `event: daemon` on `events.stream`, tagged by `kind` (management-api §2.2). Content-free
/// (#23 D6). `?kinds=` filters by a prefix of [`DaemonEvent::kind`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum DaemonEvent {
    /// `DaemonStatus.health` or `problems[]` changed.
    #[serde(rename = "status.changed")]
    StatusChanged {
        health: DaemonHealth,
        problems: Vec<Problem>,
    },
    #[serde(rename = "adapter.changed")]
    AdapterChanged {
        transport: Transport,
        state: AdapterState,
    },
    #[serde(rename = "turn.queued")]
    TurnQueued {
        turn_id: String,
        scope: Scope,
        position: u32,
    },
    #[serde(rename = "run.started")]
    RunStarted {
        run_id: String,
        turn_id: String,
        scope: Scope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        job: Option<String>,
        /// `direct` or the Launcher's file name.
        launcher: String,
    },
    #[serde(rename = "run.finished")]
    RunFinished { record: Box<RunRecord> },
    #[serde(rename = "turn.finished")]
    TurnFinished {
        turn_id: String,
        scope: Scope,
        outcome: TurnOutcome,
    },
    #[serde(rename = "job.fired")]
    JobFired {
        job: String,
        turn_id: String,
        /// A missed fire run late by catch-up (#50).
        catch_up: bool,
    },
    /// A job file changed, through the API, the Daemon itself, an outside edit or the start-up
    /// scan (#46).
    #[serde(rename = "job.changed")]
    JobChanged {
        job: String,
        change: JobChange,
        source: JobChangeSource,
        /// The file's revision now; `null` once it is deleted.
        revision: Option<Revision>,
        /// The old name, when an identical-bytes rename carried the job's state over (#46 D4).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        carried_from: Option<String>,
    },
    /// An owner notice. Only its kind and a short title the Daemon composes; never the body
    /// (reply text, heartbeat output).
    #[serde(rename = "notice")]
    Notice {
        /// `kind` on the wire would clash with the event tag, so the notice's own kind is
        /// `notice_kind`.
        notice_kind: NoticeKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<Scope>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        job: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        title: String,
    },
    #[serde(rename = "session.changed")]
    SessionChanged { scope: Scope, launcher: String },
    #[serde(rename = "launcher.changed")]
    LauncherChanged { scope: Scope },
    /// A `launchers.test` finished (#40).
    #[serde(rename = "launcher.tested")]
    LauncherTested { launcher: String, ok: bool },
    /// `settings.json` was re-read; the key paths whose values changed, never the values.
    #[serde(rename = "settings.reloaded")]
    SettingsReloaded { changed_keys: Vec<String> },
    /// Also raises an owner notice (#41).
    #[serde(rename = "device.paired")]
    DevicePaired { device_id: String },
    /// Also raises an owner notice (#41).
    #[serde(rename = "device.revoked")]
    DeviceRevoked { device_id: String },
    /// A First-start import step finished (#17).
    #[serde(rename = "import.finished")]
    ImportFinished {
        /// The `ImportRecord.steps[].step` that finished.
        record_step: String,
    },
    /// An event added later in v1 that this build doesn't know (management-api §1.1). Never
    /// sent, and not in the schema.
    #[serde(other, rename = "unknown")]
    Unknown,
}

impl DaemonEvent {
    /// The wire `kind`, which `?kinds=<prefix>` matches against; `unknown` for the catch-all.
    pub fn kind(&self) -> &'static str {
        match self {
            DaemonEvent::StatusChanged { .. } => "status.changed",
            DaemonEvent::AdapterChanged { .. } => "adapter.changed",
            DaemonEvent::TurnQueued { .. } => "turn.queued",
            DaemonEvent::RunStarted { .. } => "run.started",
            DaemonEvent::RunFinished { .. } => "run.finished",
            DaemonEvent::TurnFinished { .. } => "turn.finished",
            DaemonEvent::JobFired { .. } => "job.fired",
            DaemonEvent::JobChanged { .. } => "job.changed",
            DaemonEvent::Notice { .. } => "notice",
            DaemonEvent::SessionChanged { .. } => "session.changed",
            DaemonEvent::LauncherChanged { .. } => "launcher.changed",
            DaemonEvent::LauncherTested { .. } => "launcher.tested",
            DaemonEvent::SettingsReloaded { .. } => "settings.reloaded",
            DaemonEvent::DevicePaired { .. } => "device.paired",
            DaemonEvent::DeviceRevoked { .. } => "device.revoked",
            DaemonEvent::ImportFinished { .. } => "import.finished",
            DaemonEvent::Unknown => "unknown",
        }
    }
}

wire_enum! {
    /// What happened to a job file in `job.changed` (#46).
    pub enum JobChange {
        Created = "created",
        Updated = "updated",
        Deleted = "deleted",
        /// The file no longer parses; the job is held at its last good state.
        Invalid = "invalid",
        /// The Daemon put back frontmatter keys an outside edit broke (#46).
        Restored = "restored",
    }
}

wire_enum! {
    /// Who changed a job file in `job.changed` (#46).
    pub enum JobChangeSource {
        /// A Management API write.
        Api = "api",
        /// The Daemon's own write, such as a restore.
        Daemon = "daemon",
        /// An outside edit the Change scan found.
        External = "external",
        /// The start-up scan found the file changed while the Worker was stopped.
        Startup = "startup",
    }
}

wire_enum! {
    /// What an owner notice is about (management-api §2.2).
    pub enum NoticeKind {
        JobFailed = "job_failed",
        /// The app shows it only for jobs whose "notify when finished" flag is on (#16).
        JobFinished = "job_finished",
        RunRefused = "run_refused",
        JobInvalid = "job_invalid",
        SettingsInvalid = "settings_invalid",
        UpdateAvailable = "update_available",
        ImportDone = "import_done",
        HeartbeatOutput = "heartbeat_output",
        /// #46.
        JobFrontmatterRestored = "job_frontmatter_restored",
        /// #44.
        DiskLow = "disk_low",
    }
}

/// [`DaemonEvent`]'s schema without the catch-all. utoipa can't leave a variant out, so the
/// schema is derived from this copy, and a test checks that every known event parses as it.
mod schema {
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    use super::{JobChange, JobChangeSource, NoticeKind};
    use crate::adapter::{AdapterState, Transport};
    use crate::revision::Revision;
    use crate::runs::RunRecord;
    use crate::scope::Scope;
    use crate::status::{DaemonHealth, Problem};
    use crate::turns::TurnOutcome;

    /// One `event: daemon` on `events.stream`, tagged by `kind`. Content-free: no reply text,
    /// prompts, job bodies or heartbeat output.
    #[derive(Serialize, Deserialize, ToSchema)]
    #[serde(tag = "kind")]
    pub(super) enum DaemonEvent {
        #[serde(rename = "status.changed")]
        StatusChanged {
            health: DaemonHealth,
            problems: Vec<Problem>,
        },
        #[serde(rename = "adapter.changed")]
        AdapterChanged {
            transport: Transport,
            state: AdapterState,
        },
        #[serde(rename = "turn.queued")]
        TurnQueued {
            turn_id: String,
            scope: Scope,
            position: u32,
        },
        #[serde(rename = "run.started")]
        RunStarted {
            run_id: String,
            turn_id: String,
            scope: Scope,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            job: Option<String>,
            /// `direct` or the Launcher's file name.
            launcher: String,
        },
        #[serde(rename = "run.finished")]
        RunFinished { record: Box<RunRecord> },
        #[serde(rename = "turn.finished")]
        TurnFinished {
            turn_id: String,
            scope: Scope,
            outcome: TurnOutcome,
        },
        #[serde(rename = "job.fired")]
        JobFired {
            job: String,
            turn_id: String,
            /// A missed fire run late by catch-up.
            catch_up: bool,
        },
        #[serde(rename = "job.changed")]
        JobChanged {
            job: String,
            change: JobChange,
            source: JobChangeSource,
            /// The file's revision now; `null` once it is deleted.
            #[schema(required = true)]
            revision: Option<Revision>,
            /// The old name, when an identical-bytes rename carried the job's state over.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            carried_from: Option<String>,
        },
        #[serde(rename = "notice")]
        Notice {
            notice_kind: NoticeKind,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            scope: Option<Scope>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            job: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            run_id: Option<String>,
            title: String,
        },
        #[serde(rename = "session.changed")]
        SessionChanged { scope: Scope, launcher: String },
        #[serde(rename = "launcher.changed")]
        LauncherChanged { scope: Scope },
        #[serde(rename = "launcher.tested")]
        LauncherTested { launcher: String, ok: bool },
        /// The key paths whose values changed, never the values.
        #[serde(rename = "settings.reloaded")]
        SettingsReloaded { changed_keys: Vec<String> },
        #[serde(rename = "device.paired")]
        DevicePaired { device_id: String },
        #[serde(rename = "device.revoked")]
        DeviceRevoked { device_id: String },
        #[serde(rename = "import.finished")]
        ImportFinished {
            /// The `ImportRecord.steps[].step` that finished.
            record_step: String,
        },
    }
}

impl PartialSchema for DaemonEvent {
    fn schema() -> RefOr<Schema> {
        schema::DaemonEvent::schema()
    }
}

impl ToSchema for DaemonEvent {
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schema::DaemonEvent::schemas(schemas);
    }
}

/// `event: hello`, the first frame of `events.stream` and `turns.events` (management-api §1.6).
/// A client that sees a new `boot_id` re-fetches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StreamHello {
    pub boot_id: String,
}

/// `event: hello` on `logs.stream` (management-api §2.9, #23 D5): also the newest `seq` the
/// ring holds, so `Last-Event-ID` resumes and backfill can be lined up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogStreamHello {
    pub boot_id: String,
    pub last_seq: u64,
}

/// `event: gap`, sent on every SSE stream instead of skipping events silently
/// (management-api §1.6, §2.2; #23 D5). After it, the app re-fetches what it shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StreamGap {
    pub reason: GapReason,
    /// How many events were lost, when the server knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed: Option<u32>,
}

wire_enum! {
    /// Why a stream lost events (management-api §2.2, #23 D5).
    pub enum GapReason {
        /// The client fell behind the broadcast.
        Lagged = "lagged",
        /// The Worker restarted; `Last-Event-ID` named another boot.
        Restart = "restart",
        /// `logs.stream` only: the resumed `seq` is no longer in the ring.
        Evicted = "evicted",
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    DaemonEvent,
    JobChange,
    JobChangeSource,
    NoticeKind,
    StreamHello,
    LogStreamHello,
    StreamGap,
    GapReason,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn run_record() -> (RunRecord, Value) {
        let record: RunRecord = serde_json::from_value(json!({
            "run_id": "0199e0c2-0000-7000-8000-000000000001",
            "turn_id": "0199e0c2-0000-7000-8000-000000000000",
            "name": "daily-report",
            "scope": "job:daily-report",
            "origin": "scheduler",
            "launcher": "direct",
            "job": "daily-report",
            "started_at": "2026-10-13T08:30:27Z",
            "ended_at": "2026-10-13T08:31:02Z",
            "duration_ms": 35000,
            "outcome": "ok",
            "attempt": 1,
            "cost_status": "no_usage",
        }))
        .unwrap();
        let wire = serde_json::to_value(&record).unwrap();
        (record, wire)
    }

    fn every_event() -> Vec<(DaemonEvent, Value)> {
        let (record, record_json) = run_record();
        let revision = Revision::of(b"schedule: 0 9 * * *\n");
        vec![
            (
                DaemonEvent::StatusChanged {
                    health: DaemonHealth::Degraded,
                    problems: vec![Problem {
                        kind: crate::status::ProblemKind::JobInvalid,
                        scope: None,
                        job: Some("daily-report".into()),
                        launcher: None,
                        since: "2026-10-13T08:00:00Z".parse().unwrap(),
                    }],
                },
                json!({
                    "kind": "status.changed",
                    "health": "degraded",
                    "problems": [
                        {"kind": "job_invalid", "job": "daily-report", "since": "2026-10-13T08:00:00Z"},
                    ],
                }),
            ),
            (
                DaemonEvent::AdapterChanged {
                    transport: Transport::Discord,
                    state: AdapterState::Reconnecting,
                },
                json!({"kind": "adapter.changed", "transport": "discord", "state": "reconnecting"}),
            ),
            (
                DaemonEvent::TurnQueued {
                    turn_id: "0199e0c2-0000-7000-8000-000000000000".into(),
                    scope: "discord:1234".parse().unwrap(),
                    position: 2,
                },
                json!({
                    "kind": "turn.queued",
                    "turn_id": "0199e0c2-0000-7000-8000-000000000000",
                    "scope": "discord:1234",
                    "position": 2,
                }),
            ),
            (
                DaemonEvent::RunStarted {
                    run_id: "0199e0c2-0000-7000-8000-000000000001".into(),
                    turn_id: "0199e0c2-0000-7000-8000-000000000000".into(),
                    scope: "job:daily-report".parse().unwrap(),
                    job: Some("daily-report".into()),
                    launcher: "direct".into(),
                },
                json!({
                    "kind": "run.started",
                    "run_id": "0199e0c2-0000-7000-8000-000000000001",
                    "turn_id": "0199e0c2-0000-7000-8000-000000000000",
                    "scope": "job:daily-report",
                    "job": "daily-report",
                    "launcher": "direct",
                }),
            ),
            (
                DaemonEvent::RunFinished {
                    record: Box::new(record),
                },
                json!({"kind": "run.finished", "record": record_json}),
            ),
            (
                DaemonEvent::TurnFinished {
                    turn_id: "0199e0c2-0000-7000-8000-000000000000".into(),
                    scope: Scope::Global,
                    outcome: TurnOutcome::Refused,
                },
                json!({
                    "kind": "turn.finished",
                    "turn_id": "0199e0c2-0000-7000-8000-000000000000",
                    "scope": "global",
                    "outcome": "refused",
                }),
            ),
            (
                DaemonEvent::JobFired {
                    job: "researcher/paper-digest".into(),
                    turn_id: "0199e0c2-0000-7000-8000-000000000002".into(),
                    catch_up: true,
                },
                json!({
                    "kind": "job.fired",
                    "job": "researcher/paper-digest",
                    "turn_id": "0199e0c2-0000-7000-8000-000000000002",
                    "catch_up": true,
                }),
            ),
            (
                DaemonEvent::JobChanged {
                    job: "daily-report".into(),
                    change: JobChange::Updated,
                    source: JobChangeSource::External,
                    revision: Some(revision),
                    carried_from: Some("old-report".into()),
                },
                json!({
                    "kind": "job.changed",
                    "job": "daily-report",
                    "change": "updated",
                    "source": "external",
                    "revision": revision.to_string(),
                    "carried_from": "old-report",
                }),
            ),
            (
                DaemonEvent::Notice {
                    notice_kind: NoticeKind::JobFailed,
                    scope: Some("job:daily-report".parse().unwrap()),
                    job: Some("daily-report".into()),
                    run_id: Some("0199e0c2-0000-7000-8000-000000000001".into()),
                    title: "daily-report failed".into(),
                },
                json!({
                    "kind": "notice",
                    "notice_kind": "job_failed",
                    "scope": "job:daily-report",
                    "job": "daily-report",
                    "run_id": "0199e0c2-0000-7000-8000-000000000001",
                    "title": "daily-report failed",
                }),
            ),
            (
                DaemonEvent::SessionChanged {
                    scope: "agent:researcher".parse().unwrap(),
                    launcher: "ccs-work.sh".into(),
                },
                json!({"kind": "session.changed", "scope": "agent:researcher", "launcher": "ccs-work.sh"}),
            ),
            (
                DaemonEvent::LauncherChanged {
                    scope: Scope::Global,
                },
                json!({"kind": "launcher.changed", "scope": "global"}),
            ),
            (
                DaemonEvent::LauncherTested {
                    launcher: "ccs-work.sh".into(),
                    ok: false,
                },
                json!({"kind": "launcher.tested", "launcher": "ccs-work.sh", "ok": false}),
            ),
            (
                DaemonEvent::SettingsReloaded {
                    changed_keys: vec!["heartbeat.enabled".into(), "model".into()],
                },
                json!({"kind": "settings.reloaded", "changed_keys": ["heartbeat.enabled", "model"]}),
            ),
            (
                DaemonEvent::DevicePaired {
                    device_id: "1234".into(),
                },
                json!({"kind": "device.paired", "device_id": "1234"}),
            ),
            (
                DaemonEvent::DeviceRevoked {
                    device_id: "1234".into(),
                },
                json!({"kind": "device.revoked", "device_id": "1234"}),
            ),
            (
                DaemonEvent::ImportFinished {
                    record_step: "jobs".into(),
                },
                json!({"kind": "import.finished", "record_step": "jobs"}),
            ),
        ]
    }

    #[test]
    fn every_event_round_trips_and_matches_its_schema_copy() {
        let events = every_event();
        assert_eq!(events.len(), 16, "one case per known kind");
        for (event, wire) in events {
            assert_eq!(serde_json::to_value(&event).unwrap(), wire, "{event:?}");
            assert_eq!(wire["kind"], event.kind());
            assert_eq!(
                serde_json::from_value::<DaemonEvent>(wire.clone()).unwrap(),
                event
            );
            let copy: schema::DaemonEvent = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(copy).unwrap(), wire);
        }
    }

    #[test]
    fn a_job_change_without_a_revision_sends_null_and_omits_carried_from() {
        let event = DaemonEvent::JobChanged {
            job: "daily-report".into(),
            change: JobChange::Deleted,
            source: JobChangeSource::Api,
            revision: None,
            carried_from: None,
        };
        let wire = json!({
            "kind": "job.changed",
            "job": "daily-report",
            "change": "deleted",
            "source": "api",
            "revision": null,
        });
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
        assert_eq!(serde_json::from_value::<DaemonEvent>(wire).unwrap(), event);
    }

    #[test]
    fn an_unknown_event_parses_as_the_catch_all() {
        let event: DaemonEvent =
            serde_json::from_value(json!({"kind": "agent.hired", "agent": "researcher"})).unwrap();
        assert_eq!(event, DaemonEvent::Unknown);
        assert_eq!(event.kind(), "unknown");
    }

    #[test]
    fn later_enum_values_parse_as_unknown() {
        let event: DaemonEvent = serde_json::from_value(json!({
            "kind": "job.changed", "job": "j", "change": "moved", "source": "sync", "revision": null,
        }))
        .unwrap();
        let DaemonEvent::JobChanged { change, source, .. } = event else {
            panic!("expected job.changed")
        };
        assert_eq!(
            (change, source),
            (JobChange::Unknown, JobChangeSource::Unknown)
        );
        let event: DaemonEvent = serde_json::from_value(json!({
            "kind": "notice", "notice_kind": "agent_hired", "title": "researcher hired",
        }))
        .unwrap();
        let DaemonEvent::Notice { notice_kind, .. } = event else {
            panic!("expected notice")
        };
        assert_eq!(notice_kind, NoticeKind::Unknown);
    }

    /// Field names that would carry content (#23 D6). None may appear anywhere in an event.
    const CONTENT_FIELDS: &[&str] = &[
        "body",
        "content",
        "delta",
        "description",
        "message",
        "output",
        "persona",
        "prompt",
        "raw",
        "reply",
        "stderr",
        "stdout",
        "summary",
        "text",
    ];

    fn field_names(value: &Value, names: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (name, inner) in map {
                    names.push(name.clone());
                    field_names(inner, names);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| field_names(item, names)),
            _ => {}
        }
    }

    #[test]
    fn no_event_carries_a_content_field() {
        for (event, _) in every_event() {
            let mut names = Vec::new();
            field_names(&serde_json::to_value(&event).unwrap(), &mut names);
            for name in names {
                assert!(
                    !CONTENT_FIELDS.contains(&name.as_str()),
                    "{} carries `{name}`",
                    event.kind()
                );
            }
        }
    }

    #[test]
    fn the_event_schema_leaves_the_catch_all_out() {
        let schema = serde_json::to_string(&DaemonEvent::schema()).unwrap();
        assert!(schema.contains("job.changed"));
        assert!(!schema.contains("\"unknown\""));
    }

    #[test]
    fn hello_and_gap_frames_match_the_spec_shape() {
        let hello = StreamHello {
            boot_id: "0199e0c2-0000-7000-8000-0000000000b0".into(),
        };
        let wire = json!({"boot_id": "0199e0c2-0000-7000-8000-0000000000b0"});
        assert_eq!(serde_json::to_value(&hello).unwrap(), wire);
        assert_eq!(serde_json::from_value::<StreamHello>(wire).unwrap(), hello);

        let hello = LogStreamHello {
            boot_id: "0199e0c2-0000-7000-8000-0000000000b0".into(),
            last_seq: 4096,
        };
        let wire = json!({"boot_id": "0199e0c2-0000-7000-8000-0000000000b0", "last_seq": 4096});
        assert_eq!(serde_json::to_value(&hello).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<LogStreamHello>(wire).unwrap(),
            hello
        );

        for (gap, wire) in [
            (
                StreamGap {
                    reason: GapReason::Lagged,
                    missed: Some(17),
                },
                json!({"reason": "lagged", "missed": 17}),
            ),
            (
                StreamGap {
                    reason: GapReason::Restart,
                    missed: None,
                },
                json!({"reason": "restart"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&gap).unwrap(), wire);
            assert_eq!(serde_json::from_value::<StreamGap>(wire).unwrap(), gap);
        }
        let gap: StreamGap = serde_json::from_value(json!({"reason": "closed"})).unwrap();
        assert_eq!(gap.reason, GapReason::Unknown);
    }
}
