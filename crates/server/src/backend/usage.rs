//! The usage seam (`usage.get`, management-api §2.10; #39, #44).

use chrono::NaiveDate;

use claudeclaw_api::usage::{UsageGroup, UsageReport};

use super::BackendFuture;

/// `usage.get`'s parameters (management-api §2.10), already validated by the route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageQuery {
    /// First day, inclusive, in `tz`.
    pub from: NaiveDate,
    /// Last day, inclusive, in `tz`.
    pub to: NaiveDate,
    /// The IANA zone the caller chose for day boundaries, so "All daemons" can be summed in the
    /// app (§2.10).
    pub tz: String,
    pub group_by: UsageGroup,
}

/// The usage route (management-api §2.10). The composition root wires this to the Run index;
/// there is no `usage:read` permission, so the route is `admin` (D2).
pub trait UsageBackend: Send + Sync + 'static {
    /// One Daemon's usage between two dates, with the coverage counts that keep a total from
    /// being silently partial (#39 D11).
    fn usage(&self, query: UsageQuery) -> BackendFuture<'_, UsageReport>;
}
