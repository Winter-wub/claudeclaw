//! Route constants (management-api §2): every v1 operation with its method, path, who may call
//! it, whether it is audited, and its [`Deadline`]. The server's router, the client, the CLI and
//! the app all read this one table, and a server test asserts the router serves exactly these
//! routes (management-api §5).

use crate::deadline::Deadline;
use crate::permission::TokenPermission::{self, *};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// The "Who" column (management-api §1.2–1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    /// Any authenticated principal, no permission needed.
    Any,
    /// `Local` or a `Device`.
    Admin,
    /// The local socket or pipe only; anything else gets `403 local_only`.
    Local,
    /// `admin`, or an API token holding every listed permission.
    Token(&'static [TokenPermission]),
    /// `admin`, or the API token that created the Turn (`send` and `jobs:run` grant this).
    TurnOwner,
    /// An unknown client cert on the remote listener: `POST /v1/pair` only.
    Pairing,
}

/// Whether a call lands in the audit log (#13). Mutating routes are, unless noted in §2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Audit {
    Never,
    Always,
    /// `housekeeping.run`: audited unless the body has `dry_run: true`.
    UnlessDryRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Route {
    /// The `operationId`, which is also the audit log's `action` (§1.1).
    pub op: &'static str,
    pub method: Method,
    /// An axum 0.8 / OpenAPI path template, e.g. `/v1/jobs/{job}/run`.
    pub path: &'static str,
    pub access: Access,
    pub audit: Audit,
    pub deadline: Deadline,
}

impl Route {
    /// A `Short` route, audited when it isn't a `GET`.
    const fn new(op: &'static str, method: Method, path: &'static str, access: Access) -> Self {
        let audit = match method {
            Method::Get => Audit::Never,
            _ => Audit::Always,
        };
        Route {
            op,
            method,
            path,
            access,
            audit,
            deadline: Deadline::Short,
        }
    }

    const fn deadline(mut self, deadline: Deadline) -> Self {
        self.deadline = deadline;
        self
    }

    const fn audit(mut self, audit: Audit) -> Self {
        self.audit = audit;
        self
    }

    /// Whether the route is an SSE stream (§1.6).
    pub fn is_stream(&self) -> bool {
        self.deadline == Deadline::Stream
    }

    /// The route with this `operationId`.
    pub fn by_op(op: &str) -> Option<&'static Route> {
        ALL.iter().find(|route| route.op == op)
    }
}

use Access::*;
use Method::*;

const fn get(op: &'static str, path: &'static str, access: Access) -> Route {
    Route::new(op, Get, path, access)
}
const fn post(op: &'static str, path: &'static str, access: Access) -> Route {
    Route::new(op, Post, path, access)
}
const fn put(op: &'static str, path: &'static str, access: Access) -> Route {
    Route::new(op, Put, path, access)
}
const fn patch(op: &'static str, path: &'static str, access: Access) -> Route {
    Route::new(op, Patch, path, access)
}
const fn delete(op: &'static str, path: &'static str, access: Access) -> Route {
    Route::new(op, Delete, path, access)
}

macro_rules! routes {
    ($($(#[$doc:meta])* $name:ident = $route:expr;)*) => {
        $($(#[$doc])* pub const $name: Route = $route;)*
        /// Every v1 route, in management-api §2 order.
        pub const ALL: &[Route] = &[$($name),*];
    };
}

routes! {
    // §2.1 Daemon and status
    HEALTH_GET = get("health.get", "/v1/health", Any).deadline(Deadline::Probe);
    STATUS_GET = get("status.get", "/v1/status", Token(&[StatusRead]));
    OPENAPI_GET = get("openapi.get", "/v1/openapi.json", Any);
    DAEMON_STOP = post("daemon.stop", "/v1/daemon/stop", Admin);
    DAEMON_RESTART = post("daemon.restart", "/v1/daemon/restart", Admin);
    DAEMON_REEXEC = post("daemon.reexec", "/v1/daemon/reexec", Local);
    UPDATE_GET = get("update.get", "/v1/update", Token(&[StatusRead]));
    UPDATE_CHECK = post("update.check", "/v1/update/check", Admin)
        .audit(Audit::Never)
        .deadline(Deadline::Slow);
    HOUSEKEEPING_RUN = post("housekeeping.run", "/v1/housekeeping", Admin).audit(Audit::UnlessDryRun);

    // §2.2 Events
    EVENTS_STREAM = get("events.stream", "/v1/events", Token(&[StatusRead])).deadline(Deadline::Stream);

    // §2.3 Turns
    /// `send` reaches the `global` Scope only; the handler checks the body's Scope.
    TURNS_CREATE = post("turns.create", "/v1/turns", Token(&[Send])).deadline(Deadline::Upload);
    TURNS_GET = get("turns.get", "/v1/turns/{turn_id}", TurnOwner);
    TURNS_EVENTS = get("turns.events", "/v1/turns/{turn_id}/events", TurnOwner).deadline(Deadline::Stream);

    // §2.4 Jobs
    JOBS_LIST = get("jobs.list", "/v1/jobs", Token(&[StatusRead]));
    JOBS_GET = get("jobs.get", "/v1/jobs/{job}", Admin);
    JOBS_CREATE = post("jobs.create", "/v1/jobs", Admin);
    JOBS_UPDATE = patch("jobs.update", "/v1/jobs/{job}", Admin);
    JOBS_REPLACE = put("jobs.replace", "/v1/jobs/{job}/raw", Admin);
    JOBS_DELETE = delete("jobs.delete", "/v1/jobs/{job}", Admin);
    JOBS_RUN = post("jobs.run", "/v1/jobs/{job}/run", Token(&[JobsRun])).deadline(Deadline::Wait);
    JOBS_PREVIEW = post("jobs.preview", "/v1/schedule/preview", Admin).audit(Audit::Never);
    JOBS_CHECK = get("jobs.check", "/v1/schedule/check", Token(&[StatusRead]));
    JOBS_AUDIT = get("jobs.audit", "/v1/schedule/audit", Token(&[StatusRead])).deadline(Deadline::Slow);

    // §2.5 Scopes, Launchers and sessions
    SCOPES_LIST = get("scopes.list", "/v1/scopes", Admin);
    LAUNCHERS_LIST = get("launchers.list", "/v1/launchers", Admin);
    LAUNCHERS_SET = put("launchers.set", "/v1/scopes/{scope}/launcher", Admin);
    LAUNCHERS_UNSET = delete("launchers.unset", "/v1/scopes/{scope}/launcher", Admin);
    LAUNCHERS_TEST = post("launchers.test", "/v1/launchers/test", Admin).deadline(Deadline::Test);
    SESSIONS_LIST = get("sessions.list", "/v1/sessions", Admin);
    SESSIONS_GET = get("sessions.get", "/v1/sessions/{session_id}", Admin);
    SESSIONS_MESSAGES = get("sessions.messages", "/v1/sessions/{session_id}/messages", Admin);
    SESSIONS_RESET = post("sessions.reset", "/v1/scopes/{scope}/sessions/reset", Admin);
    SESSIONS_COMPACT = post("sessions.compact", "/v1/scopes/{scope}/sessions/compact", Admin)
        .deadline(Deadline::Wait);
    SESSIONS_DELETE = delete("sessions.delete", "/v1/scopes/{scope}/sessions", Admin);

    // §2.6 Settings
    SETTINGS_GET = get("settings.get", "/v1/settings", Admin);
    /// `remote.*` keys are local-only; the handler checks the patch's keys.
    SETTINGS_UPDATE = patch("settings.update", "/v1/settings", Admin);

    // §2.7 Discord
    DISCORD_GET = get("discord.get", "/v1/discord", Admin);
    DISCORD_AVAILABLE = get("discord.available", "/v1/discord/available-channels", Admin);
    DISCORD_CHANNELS_UPDATE = put("discord.channels.update", "/v1/discord/channels/{channel_id}", Admin);
    DISCORD_CHANNELS_REMOVE = delete("discord.channels.remove", "/v1/discord/channels/{channel_id}", Admin);
    DISCORD_HIRE = post("discord.hire", "/v1/discord/channels/{channel_id}/hire", Admin);
    DISCORD_FIRE = post("discord.fire", "/v1/discord/threads/{thread_id}/fire", Admin);

    // §2.8 Agents
    AGENTS_LIST = get("agents.list", "/v1/agents", Admin);
    AGENTS_ARCHIVED = get("agents.archived", "/v1/archived-agents", Admin);
    AGENTS_CREATE = post("agents.create", "/v1/agents", Admin);
    AGENTS_PERSONA_GET = get("agents.persona.get", "/v1/agents/{name}/persona", Admin);
    AGENTS_PERSONA_PUT = put("agents.persona.put", "/v1/agents/{name}/persona", Admin);
    AGENTS_ARCHIVE = post("agents.archive", "/v1/agents/{name}/archive", Admin);
    AGENTS_RESTORE = post("agents.restore", "/v1/archived-agents/{entry}/restore", Admin);
    AGENTS_PURGE = delete("agents.purge", "/v1/archived-agents/{entry}", Admin);

    // §2.9 Logs, runs and audit
    LOGS_TAIL = get("logs.tail", "/v1/logs/{source}", Token(&[LogsRead]));
    LOGS_STREAM = get("logs.stream", "/v1/logs/{source}/stream", Token(&[LogsRead])).deadline(Deadline::Stream);
    LOGS_SUMMARY = get("logs.summary", "/v1/logs/summary", Token(&[LogsRead]));
    LOGS_EXPORT = get("logs.export", "/v1/logs/export", Token(&[LogsRead])).deadline(Deadline::Slow);
    RUNS_LIST = get("runs.list", "/v1/runs", Token(&[LogsRead]));
    RUNS_GET = get("runs.get", "/v1/runs/{run_id}", Token(&[LogsRead]));
    RUNS_LOG = get("runs.log", "/v1/runs/{run_id}/log", Token(&[LogsRead, RunsContent]));
    AUDIT_LIST = get("audit.list", "/v1/audit", Token(&[LogsRead]));

    // §2.10 Usage and cost
    USAGE_GET = get("usage.get", "/v1/usage", Admin).deadline(Deadline::Slow);

    // §2.11 First-start import
    IMPORT_GET = get("import.get", "/v1/import", Admin);
    IMPORT_PLAN = get("import.plan", "/v1/import/plan", Admin).deadline(Deadline::Slow);
    IMPORT_APPLY = post("import.apply", "/v1/import/apply", Admin).deadline(Deadline::Slow);

    // §2.12 Pairing, Devices and API tokens
    PAIRING_CREATE = post("pairing.create", "/v1/pairing", Local);
    PAIRING_GET = get("pairing.get", "/v1/pairing/{pairing_id}", Local).deadline(Deadline::Wait);
    PAIRING_CANCEL = delete("pairing.cancel", "/v1/pairing/{pairing_id}", Local);
    /// Remote listener only.
    PAIR = post("pair", "/v1/pair", Pairing);
    DEVICES_LIST = get("devices.list", "/v1/devices", Admin);
    DEVICES_UPDATE = patch("devices.update", "/v1/devices/{device_id}", Admin);
    DEVICES_REVOKE = delete("devices.revoke", "/v1/devices/{device_id}", Admin);
    TOKENS_LIST = get("tokens.list", "/v1/tokens", Admin);
    TOKENS_CREATE = post("tokens.create", "/v1/tokens", Admin);
    TOKENS_REVOKE = delete("tokens.revoke", "/v1/tokens/{token_id}", Admin);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn ops(filter: impl Fn(&Route) -> bool) -> Vec<&'static str> {
        ALL.iter().filter(|r| filter(r)).map(|r| r.op).collect()
    }

    #[test]
    fn the_counts_in_management_api_section_3() {
        assert_eq!(ALL.len(), 72);
        assert_eq!(
            ops(Route::is_stream),
            ["events.stream", "turns.events", "logs.stream"]
        );
        assert_eq!(
            ops(|r| r.access == Local),
            [
                "daemon.reexec",
                "pairing.create",
                "pairing.get",
                "pairing.cancel"
            ]
        );
        assert_eq!(ops(|r| r.access == Pairing), ["pair"]);
    }

    #[test]
    fn ops_and_routes_are_unique() {
        let ops: BTreeSet<_> = ALL.iter().map(|r| r.op).collect();
        assert_eq!(ops.len(), ALL.len());
        let routes: BTreeSet<_> = ALL.iter().map(|r| (r.method.as_str(), r.path)).collect();
        assert_eq!(routes.len(), ALL.len());
    }

    #[test]
    fn paths_are_v1_templates() {
        for route in ALL {
            assert!(route.path.starts_with("/v1/"), "{}", route.op);
            for segment in route.path.split('/').skip(2) {
                let param = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}'));
                let name = param.unwrap_or(segment);
                assert!(
                    !name.is_empty()
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b"_-.".contains(&b)),
                    "{}: {segment:?}",
                    route.op
                );
            }
        }
    }

    #[test]
    fn deadline_classes_match_d6() {
        let class = |d| ops(|r| r.deadline == d);
        assert_eq!(class(Deadline::Probe), ["health.get"]);
        assert_eq!(class(Deadline::Upload), ["turns.create"]);
        assert_eq!(
            class(Deadline::Wait),
            ["jobs.run", "sessions.compact", "pairing.get"]
        );
        assert_eq!(class(Deadline::Test), ["launchers.test"]);
        assert_eq!(
            class(Deadline::Slow),
            [
                "update.check",
                "jobs.audit",
                "logs.export",
                "usage.get",
                "import.plan",
                "import.apply"
            ]
        );
    }

    #[test]
    fn reads_are_never_audited_and_the_noted_writes_are_exempt() {
        assert!(
            ALL.iter()
                .filter(|r| r.method == Get)
                .all(|r| r.audit == Audit::Never)
        );
        assert_eq!(
            ops(|r| r.method != Get && r.audit != Audit::Always),
            ["update.check", "housekeeping.run", "jobs.preview"]
        );
    }

    #[test]
    fn token_permissions_reach_the_routes_section_1_3_lists() {
        let granted = |p| ops(|r| matches!(r.access, Token(ps) if ps.contains(&p)));
        assert_eq!(granted(Send), ["turns.create"]);
        assert_eq!(granted(JobsRun), ["jobs.run"]);
        assert_eq!(
            granted(StatusRead),
            [
                "status.get",
                "update.get",
                "events.stream",
                "jobs.list",
                "jobs.check",
                "jobs.audit"
            ]
        );
        assert_eq!(granted(RunsContent), ["runs.log"]);
        assert_eq!(Route::by_op("jobs.run"), Some(&JOBS_RUN));
        assert_eq!(Route::by_op("jobs.rename"), None);
    }
}
