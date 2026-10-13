//! Usage and cost from the Run index (management-api §2.10; #39, #44).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::runs::RunIndexState;
use crate::wire::wire_enum;

/// `usage.get`: one Daemon's usage between two dates in the caller's `tz` (management-api
/// §2.10). Every cost sum skips `null` costs, and `coverage` counts what was skipped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UsageReport {
    /// First day, inclusive, in `tz`.
    pub from: NaiveDate,
    /// Last day, inclusive, in `tz`.
    pub to: NaiveDate,
    /// The IANA time zone the caller chose for day boundaries.
    pub tz: String,
    /// Always `USD`.
    pub currency: String,
    pub totals: UsageTotals,
    pub days: Vec<UsageDay>,
    pub rows: Vec<UsageRow>,
    pub coverage: UsageCoverage,
}

/// The whole report's sums (#39 D10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UsageTotals {
    pub runs: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Percent (0–100) of input tokens read from cache; `null` with no input tokens.
    #[schema(required = true)]
    pub cache_hit_pct: Option<f64>,
    /// `null` when no run in the report was priced.
    #[schema(required = true)]
    pub cost_usd: Option<f64>,
    /// The weakest trust over the priced runs; `null` with `cost_usd`.
    #[schema(required = true)]
    pub cost_trust: Option<CostTrust>,
}

/// One day's sums, in `tz` (management-api §2.10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UsageDay {
    pub date: NaiveDate,
    pub runs: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[schema(required = true)]
    pub cost_usd: Option<f64>,
}

/// One `group_by` group's sums (management-api §2.10, #39).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UsageRow {
    /// The group's identity: a Scope key, a Launcher, a model ID or a job name.
    pub key: String,
    /// What the app shows, e.g. a channel name for a `discord:<id>` Scope.
    pub label: String,
    pub kind: UsageGroup,
    /// The Launcher behind the row's runs; `null` when they used several.
    #[schema(required = true)]
    pub launcher: Option<String>,
    pub runs: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[schema(required = true)]
    pub cache_hit_pct: Option<f64>,
    #[schema(required = true)]
    pub cost_usd: Option<f64>,
    #[schema(required = true)]
    pub cost_trust: Option<CostTrust>,
}

wire_enum! {
    /// What a `UsageRow` groups by: `usage.get`'s `group_by`.
    pub enum UsageGroup {
        Scope = "scope",
        Launcher = "launcher",
        Model = "model",
        Job = "job",
    }
}

wire_enum! {
    /// How far a cost can be trusted, computed server-side; never "corrected" (#39 D10). A mix
    /// takes the weakest: any `estimate` makes it `estimate`, else any `managed` makes it
    /// `managed`.
    pub enum CostTrust {
        /// Anthropic API list price: `direct`, `firstParty`, an API key and basis `list`.
        ApiList = "api_list",
        /// The organization's configured rates (basis `managed`).
        Managed = "managed",
        /// Everything else, including every non-`direct` Launcher.
        Estimate = "estimate",
    }
}

/// What the sums leave out, so a total is never silently partial (#39 D11, #44).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UsageCoverage {
    /// The oldest run in the Run index; `null` when it is empty.
    #[schema(required = true)]
    pub index_from: Option<DateTime<Utc>>,
    /// Runs with no usage, e.g. back-filled TS records.
    pub runs_without_usage: u32,
    pub runs_no_baseline: u32,
    /// Runs whose basis is `unknown`: tokens counted, cost left out.
    pub runs_unknown_price: u32,
    pub index: RunIndexState,
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    UsageReport,
    UsageTotals,
    UsageDay,
    UsageRow,
    UsageGroup,
    CostTrust,
    UsageCoverage,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::json;
    use utoipa::PartialSchema;

    use super::*;
    use crate::runs::RunIndexPhase;

    #[test]
    fn a_usage_report_matches_the_spec_shape() {
        let report = UsageReport {
            from: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            to: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
            tz: "Europe/London".into(),
            currency: "USD".into(),
            totals: UsageTotals {
                runs: 3,
                tokens_in: 2000,
                tokens_out: 500,
                cache_read_tokens: 6000,
                cache_write_tokens: 0,
                cache_hit_pct: Some(75.0),
                cost_usd: Some(0.5),
                cost_trust: Some(CostTrust::Estimate),
            },
            days: vec![
                UsageDay {
                    date: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                    runs: 2,
                    tokens_in: 2000,
                    tokens_out: 500,
                    cost_usd: Some(0.5),
                },
                UsageDay {
                    date: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
                    runs: 1,
                    tokens_in: 0,
                    tokens_out: 0,
                    cost_usd: None,
                },
            ],
            rows: vec![UsageRow {
                key: "discord:1234".into(),
                label: "#general".into(),
                kind: UsageGroup::Scope,
                launcher: Some("ccs-work".into()),
                runs: 3,
                tokens_in: 2000,
                tokens_out: 500,
                cache_read_tokens: 6000,
                cache_write_tokens: 0,
                cache_hit_pct: Some(75.0),
                cost_usd: Some(0.5),
                cost_trust: Some(CostTrust::Estimate),
            }],
            coverage: UsageCoverage {
                index_from: Some("2026-08-24T09:00:00Z".parse().unwrap()),
                runs_without_usage: 1,
                runs_no_baseline: 0,
                runs_unknown_price: 0,
                index: RunIndexState {
                    state: RunIndexPhase::Ready,
                    done: 5755,
                    total: 5755,
                    since: "2026-10-13T08:01:00Z".parse().unwrap(),
                },
            },
        };
        let json = json!({
            "from": "2026-10-01",
            "to": "2026-10-02",
            "tz": "Europe/London",
            "currency": "USD",
            "totals": {
                "runs": 3,
                "tokens_in": 2000,
                "tokens_out": 500,
                "cache_read_tokens": 6000,
                "cache_write_tokens": 0,
                "cache_hit_pct": 75.0,
                "cost_usd": 0.5,
                "cost_trust": "estimate",
            },
            "days": [
                {"date": "2026-10-01", "runs": 2, "tokens_in": 2000, "tokens_out": 500, "cost_usd": 0.5},
                {"date": "2026-10-02", "runs": 1, "tokens_in": 0, "tokens_out": 0, "cost_usd": null},
            ],
            "rows": [{
                "key": "discord:1234",
                "label": "#general",
                "kind": "scope",
                "launcher": "ccs-work",
                "runs": 3,
                "tokens_in": 2000,
                "tokens_out": 500,
                "cache_read_tokens": 6000,
                "cache_write_tokens": 0,
                "cache_hit_pct": 75.0,
                "cost_usd": 0.5,
                "cost_trust": "estimate",
            }],
            "coverage": {
                "index_from": "2026-08-24T09:00:00Z",
                "runs_without_usage": 1,
                "runs_no_baseline": 0,
                "runs_unknown_price": 0,
                "index": {"state": "ready", "done": 5755, "total": 5755, "since": "2026-10-13T08:01:00Z"},
            },
        });
        assert_eq!(serde_json::to_value(&report).unwrap(), json);
        assert_eq!(serde_json::from_value::<UsageReport>(json).unwrap(), report);
    }

    #[test]
    fn cost_trust_is_the_three_spec_values() {
        assert_eq!(
            serde_json::to_value(CostTrust::schema()).unwrap()["enum"],
            json!(["api_list", "managed", "estimate"])
        );
        assert_eq!(
            serde_json::from_value::<CostTrust>(json!("invoice")).unwrap(),
            CostTrust::Unknown
        );
    }
}
