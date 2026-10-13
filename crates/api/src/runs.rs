//! The Run index's record and the runs routes (management-api §2.9; #23 D4, #44).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{PartialSchema, ToSchema};

use crate::scope::Scope;
use crate::wire::{string_enum, wire_enum};

/// One spawned run: a line of `daemon/runs/<YYYY-MM>.jsonl`, and what `runs.get` returns
/// (management-api §2.9, #23 D4). Refused runs get one too; every run in a Turn gets its own,
/// sharing `turn_id` (#33). Back-filled TS records (`origin: ts`) leave every Rust-only measure
/// `null` (#44 D2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RunRecord {
    pub run_id: String,
    /// `null` for back-filled TS records.
    #[schema(required = true)]
    pub turn_id: Option<String>,
    /// The Run log name (`<name>-<iso>.log`).
    pub name: String,
    /// `null` for back-filled TS records, whose header names no Scope.
    #[schema(required = true)]
    pub scope: Option<Scope>,
    pub origin: RunOrigin,
    /// `direct` or the Launcher's file name; `null` for back-filled TS records.
    #[schema(required = true)]
    pub launcher: Option<String>,
    #[schema(required = true)]
    pub job: Option<String>,
    #[schema(required = true)]
    pub agent: Option<String>,
    #[schema(required = true)]
    pub session_id: Option<String>,
    #[schema(required = true)]
    pub session: Option<RunSession>,
    #[schema(required = true)]
    pub model: Option<String>,
    pub started_at: DateTime<Utc>,
    #[schema(required = true)]
    pub ended_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub duration_ms: Option<u64>,
    /// `null` only while `state` is `running`.
    #[schema(required = true)]
    pub outcome: Option<RunOutcome>,
    #[schema(required = true)]
    pub exit_code: Option<i32>,
    #[schema(required = true)]
    pub is_error: Option<bool>,
    #[schema(required = true)]
    pub api_error_status: Option<u16>,
    /// Counts up within a Turn (#33); 1 for back-filled TS records.
    pub attempt: u32,
    /// Absent once the run has finished; `running` on the in-memory runs `runs.list` puts first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RunState>,
    #[schema(required = true)]
    pub tokens_in: Option<u64>,
    #[schema(required = true)]
    pub tokens_out: Option<u64>,
    #[schema(required = true)]
    pub cache_read_tokens: Option<u64>,
    #[schema(required = true)]
    pub cache_write_tokens: Option<u64>,
    /// Per run, by difference against the Cost baseline; `null` when it can't be known (#39).
    #[schema(required = true)]
    pub cost_usd: Option<f64>,
    pub cost_status: CostStatus,
    #[schema(required = true)]
    pub cost_basis: Option<CostBasis>,
    /// From `modelUsage`: `firstParty`, …, or `mixed` (#39).
    #[schema(required = true)]
    pub provider: Option<String>,
    /// From `init.apiKeySource`; `null` when there was no `init` (#39).
    #[schema(required = true)]
    pub api_key_source: Option<String>,
    /// Per model, subagents included; empty when there was no usage (#39).
    #[serde(default)]
    #[schema(required = true)]
    pub models: Vec<RunModelUsage>,
    /// The project-relative Run log, or `null` (Refused runs and compact runs write none).
    #[schema(required = true)]
    pub run_log: Option<String>,
    /// The executable the Launcher resolved to (#26).
    #[schema(required = true)]
    pub launcher_resolved: Option<String>,
    /// `null` for back-filled TS records (#33).
    #[schema(required = true)]
    pub purpose: Option<RunPurpose>,
    /// `null` when nothing was spawned to settle (#37).
    #[schema(required = true)]
    pub settle: Option<RunSettle>,
    /// The runner's own measure, without suspended time (#38).
    #[schema(required = true)]
    pub awake_ms: Option<u64>,
    #[schema(required = true)]
    pub suspended_ms: Option<u64>,
    /// A Wake grace moved the run's deadline (#38).
    #[serde(default)]
    #[schema(required = true)]
    pub wake_grace: bool,
    /// `null` for back-filled TS records and Refused runs (#42).
    #[schema(required = true)]
    pub claude_code_version: Option<String>,
    /// `claude` lingered after `result`; not a timeout for Auto-compact or the Timeout breaker
    /// (#47).
    #[serde(default)]
    #[schema(required = true)]
    pub lingered: bool,
}

wire_enum! {
    /// Where a run came from (#23 D2), plus `ts` for back-filled TS records (#23 D4).
    pub enum RunOrigin {
        Discord = "discord",
        Scheduler = "scheduler",
        Heartbeat = "heartbeat",
        Api = "api",
        Cli = "cli",
        Bootstrap = "bootstrap",
        Utility = "utility",
        Test = "test",
        Ts = "ts",
    }
}

wire_enum! {
    /// Whether a run started a new session or resumed one (#23 D2).
    pub enum RunSession {
        New = "new",
        Resumed = "resumed",
    }
}

/// How a run ended (#23 D2, #44 D2). Unlike [`wire_enum!`] sets, `unknown` is a real value here:
/// a back-filled TS log with no `Exit code:` line. A value added later in v1 parses as `Unknown`
/// too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Ok,
    Failed,
    Timeout,
    /// Stopped by a Daemon shutdown.
    Interrupted,
    RateLimited,
    /// Sent only for `origin: ts` records (#44).
    #[serde(other)]
    Unknown,
}

impl RunOutcome {
    /// Every value the server sends, `unknown` included.
    pub const KNOWN: &[RunOutcome] = &[
        RunOutcome::Ok,
        RunOutcome::Failed,
        RunOutcome::Timeout,
        RunOutcome::Interrupted,
        RunOutcome::RateLimited,
        RunOutcome::Unknown,
    ];
}

impl PartialSchema for RunOutcome {
    fn schema() -> RefOr<Schema> {
        string_enum(Self::KNOWN)
    }
}

impl ToSchema for RunOutcome {}

wire_enum! {
    /// Set on records that are still running (management-api §2.9).
    pub enum RunState {
        Running = "running",
    }
}

wire_enum! {
    /// Why a run was spawned within its Turn (#33).
    pub enum RunPurpose {
        /// The first attempt, and the other recovery re-runs.
        Turn = "turn",
        /// Any `/compact` run, automatic or manual.
        Compact = "compact",
        /// The re-run after an auto-compact.
        CompactRetry = "compact_retry",
    }
}

/// What happened to the run's process group after the leader exited (#37).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RunSettle {
    /// Group members alive at leader exit; best-effort on Windows.
    pub stragglers: u32,
    pub outcome: SettleOutcome,
    /// Something outside the group still held the output pipe.
    pub pipe_held: bool,
    pub settle_ms: u64,
}

wire_enum! {
    /// How the stragglers ended (#37).
    pub enum SettleOutcome {
        Clean = "clean",
        Exited = "exited",
        Terminated = "terminated",
        Killed = "killed",
        Survived = "survived",
    }
}

wire_enum! {
    /// Whether `cost_usd` could be computed (#39).
    pub enum CostStatus {
        Priced = "priced",
        /// A resumed session with no Cost baseline yet.
        NoBaseline = "no_baseline",
        /// No usage reported, e.g. back-filled TS records.
        NoUsage = "no_usage",
        Refused = "refused",
    }
}

/// Claude Code's `costBasis` (#39). `unknown` is a real value (Claude Code had no price for the
/// model); a value added later in v1 parses as `Unknown` too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostBasis {
    List,
    Managed,
    #[serde(other)]
    Unknown,
}

impl CostBasis {
    /// Every value the server sends, `unknown` included.
    pub const KNOWN: &[CostBasis] = &[CostBasis::List, CostBasis::Managed, CostBasis::Unknown];
}

impl PartialSchema for CostBasis {
    fn schema() -> RefOr<Schema> {
        string_enum(Self::KNOWN)
    }
}

impl ToSchema for CostBasis {}

/// One model's share of a run, per run by difference (#39 D8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RunModelUsage {
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[schema(required = true)]
    pub cost_usd: Option<f64>,
    pub cost_basis: CostBasis,
}

/// `runs.list`: running runs first, then the index newest first (management-api §2.9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RunPage {
    pub runs: Vec<RunRecord>,
    /// The `before` cursor for the next page, or `null` at the end.
    #[schema(required = true)]
    pub next: Option<String>,
    pub index: RunIndexState,
}

/// The Run index Back-fill's progress, so partial answers are labelled (#44 D2). Also served in
/// `DaemonStatus.run_index`, `UsageReport.coverage.index` and `JobsAudit.index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RunIndexState {
    pub state: RunIndexPhase,
    /// TS Run logs indexed so far by the current Back-fill.
    pub done: u32,
    /// TS Run logs the current Back-fill found missing.
    pub total: u32,
    /// When the index entered `state`.
    pub since: DateTime<Utc>,
}

wire_enum! {
    /// Where the Run index Back-fill is (#44 D2).
    pub enum RunIndexPhase {
        Ready = "ready",
        Backfilling = "backfilling",
        /// Retried at the next start.
        Failed = "failed",
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    RunRecord,
    RunOrigin,
    RunSession,
    RunOutcome,
    RunState,
    RunPurpose,
    RunSettle,
    SettleOutcome,
    CostStatus,
    CostBasis,
    RunModelUsage,
    RunPage,
    RunIndexState,
    RunIndexPhase,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn rust_run() -> (RunRecord, Value) {
        let record = RunRecord {
            run_id: "0199e0c2-0000-7000-8000-000000000001".into(),
            turn_id: Some("0199e0c2-0000-7000-8000-000000000000".into()),
            name: "daily-report".into(),
            scope: Some("job:daily-report".parse().unwrap()),
            origin: RunOrigin::Scheduler,
            launcher: Some("direct".into()),
            job: Some("daily-report".into()),
            agent: None,
            session_id: Some("5e55-1d".into()),
            session: Some(RunSession::Resumed),
            model: Some("claude-sonnet-5-5".into()),
            started_at: at("2026-10-13T08:30:27.512Z"),
            ended_at: Some(at("2026-10-13T08:31:02Z")),
            duration_ms: Some(34_488),
            outcome: Some(RunOutcome::Ok),
            exit_code: Some(0),
            is_error: Some(false),
            api_error_status: None,
            attempt: 1,
            state: None,
            tokens_in: Some(1200),
            tokens_out: Some(300),
            cache_read_tokens: Some(9000),
            cache_write_tokens: Some(400),
            cost_usd: Some(0.0125),
            cost_status: CostStatus::Priced,
            cost_basis: Some(CostBasis::List),
            provider: Some("firstParty".into()),
            api_key_source: Some("ANTHROPIC_API_KEY".into()),
            models: vec![RunModelUsage {
                model: "claude-sonnet-5-5".into(),
                tokens_in: 1200,
                tokens_out: 300,
                cache_read_tokens: 9000,
                cache_write_tokens: 400,
                cost_usd: Some(0.0125),
                cost_basis: CostBasis::List,
            }],
            run_log: Some("logs/daily-report-2026-10-13T08-30-27-512Z.log".into()),
            launcher_resolved: Some("~/.local/bin/claude".into()),
            purpose: Some(RunPurpose::Turn),
            settle: Some(RunSettle {
                stragglers: 0,
                outcome: SettleOutcome::Clean,
                pipe_held: false,
                settle_ms: 3,
            }),
            awake_ms: Some(34_488),
            suspended_ms: Some(0),
            wake_grace: false,
            claude_code_version: Some("2.1.296".into()),
            lingered: false,
        };
        let json = json!({
            "run_id": "0199e0c2-0000-7000-8000-000000000001",
            "turn_id": "0199e0c2-0000-7000-8000-000000000000",
            "name": "daily-report",
            "scope": "job:daily-report",
            "origin": "scheduler",
            "launcher": "direct",
            "job": "daily-report",
            "agent": null,
            "session_id": "5e55-1d",
            "session": "resumed",
            "model": "claude-sonnet-5-5",
            "started_at": "2026-10-13T08:30:27.512Z",
            "ended_at": "2026-10-13T08:31:02Z",
            "duration_ms": 34488,
            "outcome": "ok",
            "exit_code": 0,
            "is_error": false,
            "api_error_status": null,
            "attempt": 1,
            "tokens_in": 1200,
            "tokens_out": 300,
            "cache_read_tokens": 9000,
            "cache_write_tokens": 400,
            "cost_usd": 0.0125,
            "cost_status": "priced",
            "cost_basis": "list",
            "provider": "firstParty",
            "api_key_source": "ANTHROPIC_API_KEY",
            "models": [{
                "model": "claude-sonnet-5-5",
                "tokens_in": 1200,
                "tokens_out": 300,
                "cache_read_tokens": 9000,
                "cache_write_tokens": 400,
                "cost_usd": 0.0125,
                "cost_basis": "list",
            }],
            "run_log": "logs/daily-report-2026-10-13T08-30-27-512Z.log",
            "launcher_resolved": "~/.local/bin/claude",
            "purpose": "turn",
            "settle": {"stragglers": 0, "outcome": "clean", "pipe_held": false, "settle_ms": 3},
            "awake_ms": 34488,
            "suspended_ms": 0,
            "wake_grace": false,
            "claude_code_version": "2.1.296",
            "lingered": false,
        });
        (record, json)
    }

    /// A back-filled TS record (#23 D4, #44 D2): only the filename and header are known.
    fn ts_json(exit_code: Value, outcome: &str) -> Value {
        json!({
            "run_id": "6f1c2a4e-0000-5000-8000-000000000000",
            "turn_id": null,
            "name": "daily-report",
            "scope": null,
            "origin": "ts",
            "launcher": null,
            "job": null,
            "agent": null,
            "session_id": "5e55-1d",
            "session": "new",
            "model": null,
            "started_at": "2026-09-01T09:00:00.123Z",
            "ended_at": null,
            "duration_ms": null,
            "outcome": outcome,
            "exit_code": exit_code,
            "is_error": null,
            "api_error_status": null,
            "attempt": 1,
            "tokens_in": null,
            "tokens_out": null,
            "cache_read_tokens": null,
            "cache_write_tokens": null,
            "cost_usd": null,
            "cost_status": "no_usage",
            "cost_basis": null,
            "provider": null,
            "api_key_source": null,
            "models": [],
            "run_log": "logs/daily-report-2026-09-01T09-00-00-123Z.log",
            "launcher_resolved": null,
            "purpose": null,
            "settle": null,
            "awake_ms": null,
            "suspended_ms": null,
            "wake_grace": false,
            "claude_code_version": null,
            "lingered": false,
        })
    }

    #[test]
    fn a_rust_record_matches_the_spec_shape() {
        let (record, json) = rust_run();
        assert_eq!(serde_json::to_value(&record).unwrap(), json);
        assert_eq!(serde_json::from_value::<RunRecord>(json).unwrap(), record);
    }

    #[test]
    fn a_ts_record_round_trips() {
        let json = ts_json(json!(124), "timeout");
        let record: RunRecord = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(record.origin, RunOrigin::Ts);
        assert_eq!(record.outcome, Some(RunOutcome::Timeout));
        assert_eq!(record.cost_status, CostStatus::NoUsage);
        assert_eq!(record.scope, None);
        assert_eq!(serde_json::to_value(&record).unwrap(), json);
    }

    #[test]
    fn an_unparsed_ts_record_has_outcome_unknown() {
        let json = ts_json(Value::Null, "unknown");
        let record: RunRecord = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(record.outcome, Some(RunOutcome::Unknown));
        assert_eq!(record.exit_code, None);
        // `unknown` is a value the server sends, so it is serialised back as is.
        assert_eq!(serde_json::to_value(&record).unwrap(), json);
    }

    #[test]
    fn a_running_record_carries_state_and_no_outcome() {
        let (mut record, mut json) = rust_run();
        record.state = Some(RunState::Running);
        record.outcome = None;
        json["state"] = json!("running");
        json["outcome"] = Value::Null;
        assert_eq!(serde_json::to_value(&record).unwrap(), json);
        assert_eq!(serde_json::from_value::<RunRecord>(json).unwrap(), record);
    }

    #[test]
    fn later_fields_may_be_missing_and_later_values_parse() {
        let (_, mut json) = rust_run();
        let object = json.as_object_mut().unwrap();
        for key in ["models", "wake_grace", "lingered", "settle", "purpose"] {
            object.remove(key);
        }
        json["outcome"] = json!("added_in_v1_9");
        json["cost_basis"] = json!("added_in_v1_9");
        json["origin"] = json!("telegram");
        let record: RunRecord = serde_json::from_value(json).unwrap();
        assert_eq!(record.outcome, Some(RunOutcome::Unknown));
        assert_eq!(record.cost_basis, Some(CostBasis::Unknown));
        assert_eq!(record.origin, RunOrigin::Unknown);
        assert!(record.models.is_empty() && !record.lingered && record.settle.is_none());
    }

    #[test]
    fn outcome_and_cost_basis_schemas_list_unknown() {
        assert_eq!(
            serde_json::to_value(RunOutcome::schema()).unwrap()["enum"],
            json!([
                "ok",
                "failed",
                "timeout",
                "interrupted",
                "rate_limited",
                "unknown"
            ])
        );
        assert_eq!(
            serde_json::to_value(CostBasis::schema()).unwrap()["enum"],
            json!(["list", "managed", "unknown"])
        );
        assert_eq!(
            serde_json::to_value(RunOrigin::schema()).unwrap()["enum"],
            json!([
                "discord",
                "scheduler",
                "heartbeat",
                "api",
                "cli",
                "bootstrap",
                "utility",
                "test",
                "ts"
            ])
        );
    }

    #[test]
    fn a_run_page_matches_the_spec_shape() {
        let (record, record_json) = rust_run();
        let page = RunPage {
            runs: vec![record],
            next: Some("2026-10:42".into()),
            index: RunIndexState {
                state: RunIndexPhase::Backfilling,
                done: 3200,
                total: 5755,
                since: at("2026-10-13T08:00:00Z"),
            },
        };
        let json = json!({
            "runs": [record_json],
            "next": "2026-10:42",
            "index": {
                "state": "backfilling",
                "done": 3200,
                "total": 5755,
                "since": "2026-10-13T08:00:00Z",
            },
        });
        assert_eq!(serde_json::to_value(&page).unwrap(), json);
        assert_eq!(serde_json::from_value::<RunPage>(json).unwrap(), page);
    }
}
