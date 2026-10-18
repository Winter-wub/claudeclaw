//! The usage handler (management-api §2.10): `usage.get`.

use axum::Json;
use axum::extract::{Query, State};

use claudeclaw_api::usage::{UsageGroup, UsageReport};

use crate::backend::usage::UsageQuery;
use crate::error::ApiFailure;
use crate::routes::{Params, bad_request};
use crate::state::AppState;

/// `GET /v1/usage`: one Daemon's usage between two dates, in the caller's `tz` (§2.10).
/// `admin` only — there is no `usage:read` permission (D2) — and "All daemons" is the app
/// fanning out to each daemon and merging (D11).
pub async fn get(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<UsageReport>, ApiFailure> {
    let params = Params::new(&pairs);
    let from = params
        .date("from")?
        .ok_or_else(|| bad_request("from is required, as a whole day: YYYY-MM-DD"))?;
    let to = params
        .date("to")?
        .ok_or_else(|| bad_request("to is required, as a whole day: YYYY-MM-DD"))?;
    if from > to {
        return Err(bad_request("from is after to"));
    }
    let tz = params
        .get("tz")
        .map(str::to_owned)
        .ok_or_else(|| bad_request("tz is required, e.g. Europe/London"))?;
    if !zone_name(&tz) {
        return Err(bad_request(
            "tz is not an IANA zone name, e.g. Europe/London",
        ));
    }
    // §2.10 lists the four groups; a caller that leaves `group_by` out gets the first.
    let group_by = params
        .closed_set("group_by", UsageGroup::KNOWN)?
        .unwrap_or(UsageGroup::Scope);
    let query = UsageQuery {
        from,
        to,
        tz,
        group_by,
    };
    state.usage.usage(query).await.map(Json).map_err(Into::into)
}

/// Whether `tz` looks like an IANA zone name (`UTC`, `Europe/London`, `America/New_York`,
/// `Etc/GMT+5`). Which names exist is the host zoneinfo's question; the caller chose the zone,
/// so this only keeps the query honest (§2.10).
fn zone_name(tz: &str) -> bool {
    !tz.is_empty()
        && tz.len() <= 64
        && tz.split('/').all(|part| {
            part.bytes()
                .next()
                .is_some_and(|first| first.is_ascii_alphanumeric())
                && part.bytes().all(is_zone_byte)
        })
}

fn is_zone_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'-' | b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_names_pass_and_junk_does_not() {
        for tz in [
            "UTC",
            "Europe/London",
            "America/New_York",
            "Etc/GMT+5",
            "Asia/Bangkok",
        ] {
            assert!(zone_name(tz), "{tz}");
        }
        for tz in [
            "",
            "Europe//London",
            "/London",
            "Europe/",
            "Europe/London;rm -rf",
            "Europe/London/../etc",
        ] {
            assert!(!zone_name(tz), "{tz:?}");
        }
    }
}
