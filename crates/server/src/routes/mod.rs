//! The route handlers (management-api §2.1–§2.6, §2.7, §2.9–§2.11).
//!
//! [`Params`] reads the query string every list and filter route shares. axum hands it the pairs
//! already percent-decoded; the helpers here apply §1 on top of that — cursors stay opaque, a
//! whole number of pages is 1..=[`MAX_PAGE_LIMIT`], a closed wire set takes only the values
//! §1.1 lists — and each refusal is the §1.4 envelope, not a plain-text rejection.

pub mod agents;
pub mod audit;
pub mod daemon;
pub mod discord;
pub mod events;
pub mod import;
pub mod jobs;
pub mod logs;
pub mod pairing;
pub mod runs;
pub mod scopes;
pub mod sessions;
pub mod settings;
pub mod turns;
pub mod usage;

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;

use claudeclaw_api::error::ErrorCode;
use claudeclaw_api::logs::LogLevel;
use claudeclaw_api::scope::Scope;

use crate::error::ApiFailure;

/// Paging for the lists that grow (§1.6): `?limit=` defaults to this, and never exceeds
/// [`MAX_PAGE_LIMIT`]. Both are server conventions; the spec fixes only that the lists page
/// backwards with `?limit=&before=`.
pub const DEFAULT_PAGE_LIMIT: u32 = 100;
pub const MAX_PAGE_LIMIT: u32 = 1000;

/// A query parameter §1.4 sends as `400 bad_request`.
pub fn bad_request(message: impl Into<String>) -> ApiFailure {
    ApiFailure::new(ErrorCode::BadRequest, message)
}

/// A `before` cursor no list handed out (§1.4).
pub fn invalid_cursor() -> ApiFailure {
    ApiFailure::new(
        ErrorCode::InvalidCursor,
        "the before cursor is not one this route handed out",
    )
}

/// The query string as axum's `Query` hands it over: decoded pairs, duplicates and all.
pub struct Params<'a>(pub &'a [(String, String)]);

impl<'a> Params<'a> {
    pub fn new(pairs: &'a [(String, String)]) -> Self {
        Params(pairs)
    }

    /// The last value of `key`, or `None` when it wasn't sent or was sent empty — so `?before=`
    /// is the first page and `?limit=` the default.
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.0
            .iter()
            .rev()
            .find_map(|(name, value)| (name == key && !value.is_empty()).then_some(value.as_str()))
    }

    /// `?limit=` for a list that grows (§1.6): [`DEFAULT_PAGE_LIMIT`] when absent, otherwise a
    /// whole number of 1..=[`MAX_PAGE_LIMIT`] (`400 bad_request`).
    pub fn page_limit(&self) -> Result<u32, ApiFailure> {
        let Some(raw) = self.get("limit") else {
            return Ok(DEFAULT_PAGE_LIMIT);
        };
        let limit = number::<u32>("limit", raw)?;
        if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
            return Err(bad_request(format!(
                "limit must be between 1 and {MAX_PAGE_LIMIT}"
            )));
        }
        Ok(limit)
    }

    /// A whole count, e.g. `?backfill=` or `?tail_lines=` (`400 bad_request` when it isn't).
    pub fn count(&self, key: &str) -> Result<Option<u32>, ApiFailure> {
        self.get(key).map(|raw| number::<u32>(key, raw)).transpose()
    }

    /// A byte count, e.g. `?max_bytes=` (`400 bad_request` when it isn't one).
    pub fn bytes(&self, key: &str) -> Result<Option<u64>, ApiFailure> {
        self.get(key).map(|raw| number::<u64>(key, raw)).transpose()
    }

    /// A whole day, e.g. `?from=2026-10-01` (`400 bad_request` when it isn't).
    pub fn date(&self, key: &str) -> Result<Option<NaiveDate>, ApiFailure> {
        self.get(key)
            .map(|raw| {
                NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                    .map_err(|_| bad_request(format!("{key} must be a whole day, YYYY-MM-DD")))
            })
            .transpose()
    }

    /// An RFC 3339 UTC instant, e.g. `?since=` (`400 bad_request` when it isn't).
    pub fn timestamp(&self, key: &str) -> Result<Option<DateTime<Utc>>, ApiFailure> {
        self.get(key)
            .map(|raw| {
                DateTime::parse_from_rfc3339(raw)
                    .map(|at| at.with_timezone(&Utc))
                    .map_err(|_| bad_request(format!("{key} must be an RFC 3339 UTC instant")))
            })
            .transpose()
    }

    /// A yes/no flag, e.g. `?map=true` (`400 bad_request` for anything else; absent is `false`).
    pub fn flag(&self, key: &str) -> Result<bool, ApiFailure> {
        match self.get(key) {
            None => Ok(false),
            Some(raw) => raw
                .parse::<bool>()
                .map_err(|_| bad_request(format!("{key} must be true or false"))),
        }
    }

    /// One value of a closed wire set (§1.1): anything the set doesn't list is
    /// `400 bad_request`.
    pub fn closed_set<T>(&self, key: &str, known: &[T]) -> Result<Option<T>, ApiFailure>
    where
        T: Serialize + DeserializeOwned + Copy + PartialEq,
    {
        let Some(raw) = self.get(key) else {
            return Ok(None);
        };
        let value: T = serde_json::from_value(serde_json::Value::String(raw.to_owned()))
            .map_err(|_| bad_request(format!("{key} is not a value this route knows")))?;
        // The round trip matters: a set's `unknown` catch-all parses anything, and only the
        // value the set actually spells out is one it lists.
        let spelled = serde_json::to_value(value)
            .ok()
            .filter(|sent| *sent == serde_json::Value::String(raw.to_owned()));
        if spelled.is_none() || !known.contains(&value) {
            return Err(bad_request(format!(
                "{key} is not a value this route knows"
            )));
        }
        Ok(Some(value))
    }

    /// A Daemon-log level as the log writes it: `ERROR`, `WARN`, `INFO`, `DEBUG` or `TRACE`
    /// (§2.9, `min_level=`).
    pub fn level(&self, key: &str) -> Result<Option<LogLevel>, ApiFailure> {
        self.closed_set(key, LogLevel::KNOWN)
    }

    /// A Scope key used as a filter (§1.7): a malformed one is `400 invalid_scope`.
    pub fn scope(&self, key: &str) -> Result<Option<Scope>, ApiFailure> {
        self.get(key)
            .map(|raw| {
                raw.parse::<Scope>()
                    .map_err(|_| ApiFailure::new(ErrorCode::InvalidScope, "invalid scope key"))
            })
            .transpose()
    }
}

/// A whole number a query parameter carries (`400 bad_request` when it isn't one).
pub fn number<T>(key: &str, raw: &str) -> Result<T, ApiFailure>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    raw.parse::<T>()
        .map_err(|_| bad_request(format!("{key} must be a whole number")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params<'a>(raw: &'a [(&'a str, &'a str)]) -> Vec<(String, String)> {
        raw.iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_last_value_wins_and_empty_is_absent() {
        let pairs = params(&[("limit", "10"), ("limit", "20"), ("before", "")]);
        let query = Params::new(&pairs);
        assert_eq!(query.get("limit"), Some("20"));
        assert_eq!(query.get("before"), None);
        assert_eq!(query.get("q"), None);
    }

    #[test]
    fn a_page_limit_is_defaulted_and_range_checked() {
        let pairs = params(&[("limit", "50")]);
        assert_eq!(Params::new(&pairs).page_limit().unwrap(), 50);
        assert_eq!(
            Params::new(&[]).page_limit().unwrap(),
            DEFAULT_PAGE_LIMIT,
            "absent means the default"
        );
        for bad in ["0", "1001", "x", "-1", "1.5"] {
            let pairs = params(&[("limit", bad)]);
            let failure = Params::new(&pairs).page_limit().unwrap_err();
            assert_eq!(failure.code(), ErrorCode::BadRequest, "limit={bad}");
        }
    }

    #[test]
    fn flags_dates_and_timestamps_take_only_their_own_shape() {
        let pairs = params(&[
            ("map", "true"),
            ("from", "2026-10-01"),
            ("since", "2026-10-13T08:30:27Z"),
        ]);
        let query = Params::new(&pairs);
        assert!(query.flag("map").unwrap());
        assert_eq!(
            query.date("from").unwrap(),
            Some("2026-10-01".parse().unwrap())
        );
        assert!(query.timestamp("since").unwrap().is_some());
        assert!(!query.flag("map_absent").unwrap());

        let bad = params(&[("map", "yes"), ("from", "October 1")]);
        let bad = Params::new(&bad);
        assert_eq!(bad.flag("map").unwrap_err().code(), ErrorCode::BadRequest);
        assert_eq!(bad.date("from").unwrap_err().code(), ErrorCode::BadRequest);
    }

    #[test]
    fn a_closed_set_refuses_values_it_does_not_list() {
        let pairs = params(&[("min_level", "INFO"), ("event", "run.")]);
        let query = Params::new(&pairs);
        assert_eq!(query.level("min_level").unwrap(), Some(LogLevel::Info));
        assert_eq!(
            query.level("absent").unwrap(),
            None,
            "an absent key carries no level"
        );
        let pairs = params(&[("min_level", "notice")]);
        assert_eq!(
            Params::new(&pairs).level("min_level").unwrap_err().code(),
            ErrorCode::BadRequest
        );
    }

    #[test]
    fn a_scope_filter_must_be_a_typed_key() {
        let pairs = params(&[("scope", "job:daily-report")]);
        assert_eq!(
            Params::new(&pairs)
                .scope("scope")
                .unwrap()
                .unwrap()
                .to_string(),
            "job:daily-report"
        );
        let pairs = params(&[("scope", "nope")]);
        assert_eq!(
            Params::new(&pairs).scope("scope").unwrap_err().code(),
            ErrorCode::InvalidScope
        );
    }
}
