//! Stable log event names, and the events the Desktop app's webview may log (#40 D1, D3).
//!
//! The names are binding, and new names may be added. Each event is declared with every field
//! it may carry (#69), and each field needs a Field class: [`crate::export::APP_FIELD_CLASSES`]
//! for the App log, [`crate::export::FIELD_CLASSES`] for the Daemon log (#40 D3, #28).

use serde::{Deserialize, Serialize};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{PartialSchema, ToSchema};

/// One catalogue event and every field it may carry besides [`COMMON_FIELDS`] (#69). The canary
/// tests fail when a line carries a field outside this list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventFields {
    pub event: &'static str,
    pub fields: &'static [&'static str],
}

/// Fields any line of either log may carry: #23 D2's fixed keys and the `err` Display chain.
pub const COMMON_FIELDS: &[&str] = &[
    "err",
    "event",
    "level",
    "message",
    "seq",
    "target",
    "timestamp",
];

/// The App log catalogue (#40 D3). The App log holds no owner identifiers: only the app-local
/// daemon ID, `op`, statuses, error codes, durations, byte counts and versions.
pub mod app {
    use super::EventFields;

    // Lifecycle.
    /// `app_version`, `os`, `arch`, `webview_version`.
    pub const APP_START: &str = "app.start";
    pub const APP_READY: &str = "app.ready";
    pub const APP_EXIT: &str = "app.exit";
    /// `location`, `payload_bytes`; then the default panic hook runs (#40 D1).
    pub const APP_PANIC: &str = "app.panic";
    /// `hard`, always `false`: the App log has only the soft cap. The same name and fields as
    /// the Daemon log's (#74).
    pub const LOG_CAPPED: &str = "log.capped";

    // Updater.
    pub const UPDATE_CHECKED: &str = "update.checked";
    pub const UPDATE_AVAILABLE: &str = "update.available";
    pub const UPDATE_INSTALLED: &str = "update.installed";
    /// `kind`.
    pub const UPDATE_FAILED: &str = "update.failed";

    // Daemon list.
    /// `daemon`, `kind: local|remote`.
    pub const DAEMON_ADDED: &str = "daemon.added";
    /// `daemon`, `kind`.
    pub const DAEMON_REMOVED: &str = "daemon.removed";
    /// `daemon`, `from`, `to` (`DaemonReachability`), `class: refused|timeout|dns|tls_pin|revoked`.
    pub const DAEMON_REACHABILITY: &str = "daemon.reachability";

    // Local process duties: `daemon`, `exit_code`, `code`, `duration_ms`. Named `local.*` so no
    // name is shared with the Daemon log's `daemon.start` (#74).
    pub const LOCAL_START: &str = "local.start";
    pub const LOCAL_STOP: &str = "local.stop";
    pub const LOCAL_ATTACH: &str = "local.attach";
    pub const LOCAL_RESTART_ANYWAY: &str = "local.restart_anyway";

    // Offline commands.
    /// `command: logs|export|import_dry_run|start|stop`, `exit_code`, `duration_ms`,
    /// `stderr_bytes`, `outcome: ok|timeout|cancelled|failed`.
    pub const OFFLINE_EXEC: &str = "offline.exec";

    // Bridge.
    /// DEBUG: `daemon`, `op`, `status`, `code?`, `duration_ms`, `deadline_ms`, `req_bytes`,
    /// `resp_bytes`.
    pub const API_CALL: &str = "api.call";
    /// WARN: `daemon`, `op`, `kind`, `elapsed_ms`, `deadline_ms`.
    pub const API_CALL_FAILED: &str = "api.call_failed";
    /// DEBUG.
    pub const API_CANCELLED: &str = "api.cancelled";
    /// `daemon`, `family`.
    pub const SSE_OPEN: &str = "sse.open";
    /// `daemon`, `family`.
    pub const SSE_GAP: &str = "sse.gap";
    /// `daemon`, `family`.
    pub const SSE_IDLE_RECONNECT: &str = "sse.idle_reconnect";
    /// `daemon`, `family`.
    pub const SSE_CLOSED: &str = "sse.closed";

    // Pairing and keys: `device_pairing.*`, so no name is shared with the Daemon log's
    // `pairing.*` (#74).
    pub const DEVICE_PAIRING_STARTED: &str = "device_pairing.started";
    pub const DEVICE_PAIRING_COMPLETED: &str = "device_pairing.completed";
    /// `kind`.
    pub const DEVICE_PAIRING_FAILED: &str = "device_pairing.failed";
    /// `op`, `kind`.
    pub const KEYCHAIN_FAILED: &str = "keychain.failed";

    // Webview: the first four come from [`super::UiLogEvent`].
    /// `screen`, `name`, `frames`, `message_bytes`.
    pub const UI_ERROR: &str = "ui.error";
    /// `screen`, `name`, `frames`, `message_bytes`.
    pub const UI_UNHANDLED_REJECTION: &str = "ui.unhandled_rejection";
    /// `screen`, `name`, `frames`, `message_bytes`.
    pub const UI_RENDER_ERROR: &str = "ui.render_error";
    /// DEBUG: `screen`.
    pub const UI_SCREEN: &str = "ui.screen";
    /// `count`: events over the `app_log` rate limit (100/s), never dropped silently (#40 D1).
    pub const UI_LOG_DROPPED: &str = "ui.log_dropped";

    /// Every App log event with its fields, in catalogue order (#40 D3, #69).
    pub const CATALOGUE: &[EventFields] = &[
        EventFields {
            event: APP_START,
            fields: &["app_version", "os", "arch", "webview_version"],
        },
        EventFields {
            event: APP_READY,
            fields: &[],
        },
        EventFields {
            event: APP_EXIT,
            fields: &[],
        },
        EventFields {
            event: APP_PANIC,
            fields: &["location", "payload_bytes"],
        },
        EventFields {
            event: LOG_CAPPED,
            fields: &["hard"],
        },
        EventFields {
            event: UPDATE_CHECKED,
            fields: &[],
        },
        EventFields {
            event: UPDATE_AVAILABLE,
            fields: &[],
        },
        EventFields {
            event: UPDATE_INSTALLED,
            fields: &[],
        },
        EventFields {
            event: UPDATE_FAILED,
            fields: &["kind"],
        },
        EventFields {
            event: DAEMON_ADDED,
            fields: &["daemon", "kind"],
        },
        EventFields {
            event: DAEMON_REMOVED,
            fields: &["daemon", "kind"],
        },
        EventFields {
            event: DAEMON_REACHABILITY,
            fields: &["daemon", "from", "to", "class"],
        },
        EventFields {
            event: LOCAL_START,
            fields: &["daemon", "exit_code", "code", "duration_ms"],
        },
        EventFields {
            event: LOCAL_STOP,
            fields: &["daemon", "exit_code", "code", "duration_ms"],
        },
        EventFields {
            event: LOCAL_ATTACH,
            fields: &["daemon", "exit_code", "code", "duration_ms"],
        },
        EventFields {
            event: LOCAL_RESTART_ANYWAY,
            fields: &["daemon", "exit_code", "code", "duration_ms"],
        },
        EventFields {
            event: OFFLINE_EXEC,
            fields: &[
                "command",
                "exit_code",
                "duration_ms",
                "stderr_bytes",
                "outcome",
            ],
        },
        EventFields {
            event: API_CALL,
            fields: &[
                "daemon",
                "op",
                "status",
                "code",
                "duration_ms",
                "deadline_ms",
                "req_bytes",
                "resp_bytes",
            ],
        },
        EventFields {
            event: API_CALL_FAILED,
            fields: &["daemon", "op", "kind", "elapsed_ms", "deadline_ms"],
        },
        EventFields {
            event: API_CANCELLED,
            fields: &[],
        },
        EventFields {
            event: SSE_OPEN,
            fields: &["daemon", "family"],
        },
        EventFields {
            event: SSE_GAP,
            fields: &["daemon", "family"],
        },
        EventFields {
            event: SSE_IDLE_RECONNECT,
            fields: &["daemon", "family"],
        },
        EventFields {
            event: SSE_CLOSED,
            fields: &["daemon", "family"],
        },
        EventFields {
            event: DEVICE_PAIRING_STARTED,
            fields: &[],
        },
        EventFields {
            event: DEVICE_PAIRING_COMPLETED,
            fields: &[],
        },
        EventFields {
            event: DEVICE_PAIRING_FAILED,
            fields: &["kind"],
        },
        EventFields {
            event: KEYCHAIN_FAILED,
            fields: &["op", "kind"],
        },
        EventFields {
            event: UI_ERROR,
            fields: &["screen", "name", "frames", "message_bytes"],
        },
        EventFields {
            event: UI_UNHANDLED_REJECTION,
            fields: &["screen", "name", "frames", "message_bytes"],
        },
        EventFields {
            event: UI_RENDER_ERROR,
            fields: &["screen", "name", "frames", "message_bytes"],
        },
        EventFields {
            event: UI_SCREEN,
            fields: &["screen"],
        },
        EventFields {
            event: UI_LOG_DROPPED,
            fields: &["count"],
        },
    ];
}

/// The Daemon log catalogue (#23 D2 and later tickets, #69, #74).
///
/// Naming rules (#74): durations are `*_ms`; an event name means one thing, written by one
/// process; a name may appear in both logs only with the same meaning and fields (`log.capped`,
/// `update.available`); an owner notice is named for what the owner sees and its event for the
/// component that writes it (`launcher_root_moved` vs `session.root_moved`).
pub mod daemon {
    use super::EventFields;

    /// Span correlation fields (#23 D2), carried inside `span` on every line within a span.
    /// The log sink writes the tracing span's own name as `span_name`, never `name`, which is a
    /// job name at any depth. `attempt` is only ever a span field (#74).
    pub const SPAN_FIELDS: &[&str] = &[
        "agent",
        "attempt",
        "job",
        "launcher",
        "origin",
        "principal",
        "run_id",
        "scope",
        "span_name",
        "turn_id",
    ];

    /// #23, #36.
    pub const DAEMON_START: &str = "daemon.start";
    /// #23.
    pub const WORKER_READY: &str = "worker.ready";
    /// #23.
    pub const WORKER_STOPPING: &str = "worker.stopping";
    /// #23.
    pub const SETTINGS_RELOADED: &str = "settings.reloaded";
    /// #23.
    pub const SETTINGS_INVALID: &str = "settings.invalid";
    /// WARN, besides the owner notice of the same name (#32, #74).
    pub const SETTINGS_LEGACY_KEY: &str = "settings.legacy_key";
    /// #23.
    pub const JOBS_LOADED: &str = "jobs.loaded";
    /// #23.
    pub const JOB_INVALID: &str = "job.invalid";
    /// #23.
    pub const UPDATE_AVAILABLE: &str = "update.available";
    /// #23.
    pub const UPDATE_APPLIED: &str = "update.applied";
    /// #23.
    pub const TURN_ACCEPTED: &str = "turn.accepted";
    /// #23, #33.
    pub const RUN_START: &str = "run.start";
    /// #23, #33, #38, #42.
    pub const RUN_FINISH: &str = "run.finish";
    /// #23.
    pub const RUN_REFUSED: &str = "run.refused";
    /// #23, #33.
    pub const RUN_RECOVERY: &str = "run.recovery";
    /// #32, #33.
    pub const RUN_RECOVERY_SKIPPED: &str = "run.recovery_skipped";
    /// #37.
    pub const RUN_SETTLE: &str = "run.settle";
    /// #47.
    pub const RUN_LINGERED: &str = "run.lingered";
    /// #25.
    pub const RUN_STDIN_WRITE_FAILED: &str = "run.stdin_write_failed";
    /// #23.
    pub const RATELIMIT_SET: &str = "ratelimit.set";
    /// #23.
    pub const JOB_FIRE: &str = "job.fire";
    /// #23.
    pub const JOB_SKIP: &str = "job.skip";
    /// #23.
    pub const JOB_RETRY_SCHEDULED: &str = "job.retry_scheduled";
    /// #23.
    pub const JOB_RETRIES_EXHAUSTED: &str = "job.retries_exhausted";
    /// #23.
    pub const JOB_ONESHOT_FINISHED: &str = "job.oneshot_finished";
    /// #46.
    pub const JOB_FILE_CHANGED: &str = "job.file_changed";
    /// #46.
    pub const JOB_FILE_UNSTABLE: &str = "job.file_unstable";
    /// #46.
    pub const JOB_FRONTMATTER_RESTORED: &str = "job.frontmatter_restored";
    /// #46.
    pub const JOB_STATE_CARRIED: &str = "job.state_carried";
    /// #46.
    pub const SCAN_SLOW: &str = "scan.slow";
    /// #46.
    pub const SCAN_DIR_UNAVAILABLE: &str = "scan.dir_unavailable";
    /// #23.
    pub const HEARTBEAT_FIRE: &str = "heartbeat.fire";
    /// #23.
    pub const SCHEDULER_CLOCK_JUMP: &str = "scheduler.clock_jump";
    /// #23.
    pub const DISCORD_CONNECTED: &str = "discord.connected";
    /// #23.
    pub const DISCORD_DISCONNECTED: &str = "discord.disconnected";
    /// #23.
    pub const DISCORD_RESUMED: &str = "discord.resumed";
    /// #23.
    pub const DISCORD_MESSAGE: &str = "discord.message";
    /// #23, #44.
    pub const DISCORD_ATTACHMENT_REJECTED: &str = "discord.attachment_rejected";
    /// #23.
    pub const DISCORD_SEND_FAILED: &str = "discord.send_failed";
    /// #23.
    pub const API_REQUEST: &str = "api.request";
    /// #23.
    pub const API_AUTH_FAILED: &str = "api.auth_failed";
    /// #40.
    pub const API_CLIENT_GONE: &str = "api.client_gone";
    /// #23.
    pub const PAIRING_STARTED: &str = "pairing.started";
    /// #23.
    pub const PAIRING_COMPLETED: &str = "pairing.completed";
    /// #23.
    pub const PAIRING_FAILED: &str = "pairing.failed";
    /// #23.
    pub const DEVICE_REVOKED: &str = "device.revoked";
    /// #23.
    pub const NOTIFY_SENT: &str = "notify.sent";
    /// #23.
    pub const NOTIFY_FAILED: &str = "notify.failed";
    /// #23, #44.
    pub const LOG_DROPPED: &str = "log.dropped";
    /// #23, #44.
    pub const LOG_CAPPED: &str = "log.capped";
    /// #36.
    pub const LOGS_PRUNE_INCOMPLETE: &str = "logs.prune_incomplete";
    /// #23.
    pub const WATCHDOG_START: &str = "watchdog.start";
    /// #23.
    pub const WORKER_SPAWN: &str = "worker.spawn";
    /// #23.
    pub const WORKER_EXIT: &str = "worker.exit";
    /// #23.
    pub const WORKER_RESTART: &str = "worker.restart";
    /// #23.
    pub const WATCHDOG_GAVE_UP: &str = "watchdog.gave_up";
    /// #23.
    pub const WORKER_STDERR: &str = "worker.stderr";
    /// #27.
    pub const WORKER_PING_LATE: &str = "worker.ping_late";
    /// The Worker's ERROR when a beacon goes quiet (#27). The Watchdog's stall report is
    /// [`WATCHDOG_STALL_REPORT`] (#74).
    pub const WORKER_STALLED: &str = "worker.stalled";
    /// WARN: the stall report the Watchdog logs (#27, #74).
    pub const WATCHDOG_STALL_REPORT: &str = "watchdog.stall_report";
    /// ERROR, from the Worker's panic hook before the default hook prints to stderr (#23, #74).
    pub const WORKER_PANIC: &str = "worker.panic";
    /// #27.
    pub const WORKER_HUNG: &str = "worker.hung";
    /// #27.
    pub const WORKER_SHUTDOWN_OVERRUN: &str = "worker.shutdown_overrun";
    /// #27.
    pub const WATCHDOG_HANG_KILL_DISABLED: &str = "watchdog.hang_kill_disabled";
    /// #45.
    pub const WATCHDOG_START_HELD: &str = "watchdog.start_held";
    /// #45.
    pub const WATCHDOG_GIVE_UP_CLEARED: &str = "watchdog.give_up_cleared";
    /// #45.
    pub const WATCHDOG_EXIT: &str = "watchdog.exit";
    /// #45.
    pub const WATCHDOG_SUPERVISOR_UNREADABLE: &str = "watchdog.supervisor_unreadable";
    /// INFO, once per wait for an Offline write's lock (#22, #45).
    pub const LOCK_WAIT_OFFLINE_WRITE: &str = "lock.wait_offline_write";
    /// #29.
    pub const REAP_START: &str = "reap.start";
    /// #29.
    pub const RUN_REAPED: &str = "run.reaped";
    /// #29.
    pub const REAP_DONE: &str = "reap.done";
    /// #29.
    pub const REAP_ORPHAN_WORKER: &str = "reap.orphan_worker";
    /// WARN: the Run registry was not empty after a graceful stop, a Worker bug (#29 D4, #74).
    pub const REAP_LEFTOVER_AFTER_STOP: &str = "reap.leftover_after_stop";
    /// ERROR: `runs.json` could not be read at a Reap (#29 D4, #74).
    pub const REAP_REGISTRY_UNREADABLE: &str = "reap.registry_unreadable";
    /// #32.
    pub const SESSION_BREAKER_OPEN: &str = "session.breaker_open";
    /// #32.
    pub const SESSION_BREAKER_CLOSED: &str = "session.breaker_closed";
    /// #39.
    pub const SESSION_ROOT_MOVED: &str = "session.root_moved";
    /// #38.
    pub const SYSTEM_RESUMED: &str = "system.resumed";
    /// #39.
    pub const TRANSCRIPT_NOT_FOUND: &str = "transcript.not_found";
    /// #39.
    pub const USAGE_BASELINE_RESET: &str = "usage.baseline_reset";
    /// #42.
    pub const RUNNER_CC_VERSION_UNPARSED: &str = "runner.cc_version_unparsed";
    /// #42.
    pub const RUNNER_CLAUDE_CODE_OUTDATED: &str = "runner.claude_code_outdated";
    /// #43.
    pub const RUN_LOCALE_CHOSEN: &str = "run_locale.chosen";
    /// #43.
    pub const RUN_LOCALE_NOT_UTF8: &str = "run_locale.not_utf8";
    /// #43.
    pub const RUN_LOCALE_UNAVAILABLE: &str = "run_locale.unavailable";
    /// #26.
    pub const RUN_PATH_CAPTURED: &str = "run_path.captured";
    /// #26.
    pub const RUN_PATH_CHANGED: &str = "run_path.changed";
    /// #26.
    pub const RUN_PATH_CAPTURE_FAILED: &str = "run_path.capture_failed";
    /// #44.
    pub const RUNINDEX_BACKFILL_START: &str = "runindex.backfill_start";
    /// #44.
    pub const RUNINDEX_BACKFILL_DONE: &str = "runindex.backfill_done";
    /// #44.
    pub const RUNINDEX_BACKFILL_FAILED: &str = "runindex.backfill_failed";
    /// #44.
    pub const RUNINDEX_UNPARSED: &str = "runindex.unparsed";
    /// #44.
    pub const RUNINDEX_TORN: &str = "runindex.torn";
    /// #44.
    pub const INBOX_PRUNED: &str = "inbox.pruned";
    /// #44.
    pub const INBOX_OVER_CAP: &str = "inbox.over_cap";
    /// #44.
    pub const DISK_LOW: &str = "disk.low";
    /// #44.
    pub const DISK_OK: &str = "disk.ok";
    /// #44.
    pub const HOUSEKEEPING_DONE: &str = "housekeeping.done";
    /// #24.
    pub const COMPAT_STATUSLINE_MIGRATED: &str = "compat.statusline_migrated";
    /// #24.
    pub const COMPAT_STATE_JSON_REMOVED: &str = "compat.state_json_removed";
    /// WARN: `.claude/settings.json` is unparseable, so `statusLine` is left alone (#24 D3, #74).
    pub const COMPAT_STATUSLINE_UNREADABLE: &str = "compat.statusline_unreadable";

    /// Every Daemon log event with its fields (#69).
    pub const CATALOGUE: &[EventFields] = &[
        EventFields {
            event: DAEMON_START,
            fields: &["version", "pid", "boot_id", "console"],
        },
        EventFields {
            event: WORKER_READY,
            fields: &[],
        },
        EventFields {
            event: WORKER_STOPPING,
            fields: &["reason"],
        },
        EventFields {
            event: SETTINGS_RELOADED,
            fields: &["changed_keys"],
        },
        EventFields {
            event: SETTINGS_INVALID,
            fields: &[],
        },
        EventFields {
            event: SETTINGS_LEGACY_KEY,
            fields: &["key", "replacement"],
        },
        EventFields {
            event: JOBS_LOADED,
            fields: &["count", "invalid"],
        },
        EventFields {
            event: JOB_INVALID,
            fields: &["job", "line", "col", "diagnostic"],
        },
        EventFields {
            event: UPDATE_AVAILABLE,
            fields: &[],
        },
        EventFields {
            event: UPDATE_APPLIED,
            fields: &[],
        },
        EventFields {
            event: TURN_ACCEPTED,
            fields: &["queue_depth"],
        },
        EventFields {
            event: RUN_START,
            fields: &["session", "session_id", "model", "purpose"],
        },
        EventFields {
            event: RUN_FINISH,
            fields: &[
                "outcome",
                "exit_code",
                "is_error",
                "api_error_status",
                "duration_ms",
                "result_bytes",
                "stderr_bytes",
                "noise_lines",
                "tool_uses",
                "tokens_in",
                "tokens_out",
                "cost_usd",
                "purpose",
                "awake_ms",
                "suspended_ms",
                "wake_grace",
                "claude_code_version",
            ],
        },
        EventFields {
            event: RUN_REFUSED,
            fields: &["reason"],
        },
        EventFields {
            event: RUN_RECOVERY,
            fields: &["kind", "compact_timeout_ms", "turn_timeout_ms"],
        },
        EventFields {
            event: RUN_RECOVERY_SKIPPED,
            fields: &["kind", "reason", "spent_ms_ago"],
        },
        EventFields {
            event: RUN_SETTLE,
            fields: &["run_id", "stragglers", "outcome", "pipe_held", "settle_ms"],
        },
        EventFields {
            event: RUN_LINGERED,
            fields: &["run_id", "linger_ms"],
        },
        EventFields {
            event: RUN_STDIN_WRITE_FAILED,
            fields: &[],
        },
        EventFields {
            event: RATELIMIT_SET,
            fields: &["until"],
        },
        EventFields {
            event: JOB_FIRE,
            fields: &["scheduled_for", "late_ms", "catch_up"],
        },
        EventFields {
            event: JOB_SKIP,
            fields: &["reason"],
        },
        EventFields {
            event: JOB_RETRY_SCHEDULED,
            fields: &[],
        },
        EventFields {
            event: JOB_RETRIES_EXHAUSTED,
            fields: &[],
        },
        EventFields {
            event: JOB_ONESHOT_FINISHED,
            fields: &[],
        },
        EventFields {
            event: JOB_FILE_CHANGED,
            fields: &["job", "change", "source", "revision"],
        },
        EventFields {
            event: JOB_FILE_UNSTABLE,
            fields: &["job", "unstable_ms"],
        },
        EventFields {
            event: JOB_FRONTMATTER_RESTORED,
            fields: &["job", "keys"],
        },
        EventFields {
            event: JOB_STATE_CARRIED,
            fields: &["from", "to"],
        },
        EventFields {
            event: SCAN_SLOW,
            fields: &["pass_ms", "interval_ms"],
        },
        EventFields {
            event: SCAN_DIR_UNAVAILABLE,
            fields: &["dir_kind", "errno"],
        },
        EventFields {
            event: HEARTBEAT_FIRE,
            fields: &[],
        },
        EventFields {
            event: SCHEDULER_CLOCK_JUMP,
            fields: &["ms"],
        },
        EventFields {
            event: DISCORD_CONNECTED,
            fields: &[],
        },
        EventFields {
            event: DISCORD_DISCONNECTED,
            fields: &["close_code"],
        },
        EventFields {
            event: DISCORD_RESUMED,
            fields: &[],
        },
        EventFields {
            event: DISCORD_MESSAGE,
            fields: &["scope", "author_id", "text_bytes", "attachments"],
        },
        EventFields {
            event: DISCORD_ATTACHMENT_REJECTED,
            fields: &["reason", "bytes"],
        },
        EventFields {
            event: DISCORD_SEND_FAILED,
            fields: &["status"],
        },
        EventFields {
            event: API_REQUEST,
            fields: &["method", "route", "status", "duration_ms"],
        },
        EventFields {
            event: API_AUTH_FAILED,
            fields: &["listener", "reason", "peer"],
        },
        EventFields {
            event: API_CLIENT_GONE,
            fields: &["elapsed_ms"],
        },
        EventFields {
            event: PAIRING_STARTED,
            fields: &[],
        },
        EventFields {
            event: PAIRING_COMPLETED,
            fields: &[],
        },
        EventFields {
            event: PAIRING_FAILED,
            fields: &[],
        },
        EventFields {
            event: DEVICE_REVOKED,
            fields: &[],
        },
        EventFields {
            event: NOTIFY_SENT,
            fields: &[],
        },
        EventFields {
            event: NOTIFY_FAILED,
            fields: &[],
        },
        EventFields {
            event: LOG_DROPPED,
            fields: &["count"],
        },
        EventFields {
            event: LOG_CAPPED,
            fields: &["hard"],
        },
        EventFields {
            event: LOGS_PRUNE_INCOMPLETE,
            fields: &["kept", "expected"],
        },
        EventFields {
            event: WATCHDOG_START,
            fields: &[],
        },
        EventFields {
            event: WORKER_SPAWN,
            fields: &["pid"],
        },
        EventFields {
            event: WORKER_EXIT,
            fields: &["code", "signal", "uptime_ms"],
        },
        EventFields {
            event: WORKER_RESTART,
            fields: &["backoff_ms"],
        },
        EventFields {
            event: WATCHDOG_GAVE_UP,
            fields: &[],
        },
        EventFields {
            event: WORKER_STDERR,
            fields: &["text"],
        },
        EventFields {
            event: WORKER_PING_LATE,
            fields: &[],
        },
        EventFields {
            event: WORKER_STALLED,
            fields: &["beacon", "quiet_ms"],
        },
        EventFields {
            event: WATCHDOG_STALL_REPORT,
            fields: &[
                "report_seq",
                "waited_ms",
                "workers",
                "alive_tasks",
                "global_queue",
            ],
        },
        EventFields {
            event: WORKER_PANIC,
            fields: &["location"],
        },
        EventFields {
            event: WORKER_HUNG,
            fields: &["silence_ms"],
        },
        EventFields {
            event: WORKER_SHUTDOWN_OVERRUN,
            fields: &[],
        },
        EventFields {
            event: WATCHDOG_HANG_KILL_DISABLED,
            fields: &[],
        },
        EventFields {
            event: WATCHDOG_START_HELD,
            fields: &[],
        },
        EventFields {
            event: WATCHDOG_GIVE_UP_CLEARED,
            fields: &[],
        },
        EventFields {
            event: WATCHDOG_EXIT,
            fields: &["code", "meaning"],
        },
        EventFields {
            event: WATCHDOG_SUPERVISOR_UNREADABLE,
            fields: &[],
        },
        EventFields {
            event: LOCK_WAIT_OFFLINE_WRITE,
            fields: &["holder_pid"],
        },
        EventFields {
            event: REAP_START,
            fields: &["reason"],
        },
        EventFields {
            event: RUN_REAPED,
            fields: &["run_id", "scope", "outcome", "signal", "ms"],
        },
        EventFields {
            event: REAP_DONE,
            fields: &[
                "killed",
                "already_gone",
                "not_ours",
                "unverified",
                "unknown_pid",
                "survived",
            ],
        },
        EventFields {
            event: REAP_ORPHAN_WORKER,
            fields: &["waited_ms", "killed"],
        },
        EventFields {
            event: REAP_LEFTOVER_AFTER_STOP,
            fields: &["count"],
        },
        EventFields {
            event: REAP_REGISTRY_UNREADABLE,
            fields: &["cause"],
        },
        EventFields {
            event: SESSION_BREAKER_OPEN,
            fields: &[
                "scope",
                "launcher",
                "limit",
                "consecutive_timeouts",
                "failing_ms",
            ],
        },
        EventFields {
            event: SESSION_BREAKER_CLOSED,
            fields: &["reason"],
        },
        EventFields {
            event: SESSION_ROOT_MOVED,
            fields: &["scope", "launcher"],
        },
        EventFields {
            event: SYSTEM_RESUMED,
            fields: &["suspended_ms"],
        },
        EventFields {
            event: TRANSCRIPT_NOT_FOUND,
            fields: &["reason", "searched_roots"],
        },
        EventFields {
            event: USAGE_BASELINE_RESET,
            fields: &[],
        },
        EventFields {
            event: RUNNER_CC_VERSION_UNPARSED,
            fields: &[],
        },
        EventFields {
            event: RUNNER_CLAUDE_CODE_OUTDATED,
            fields: &[],
        },
        EventFields {
            event: RUN_LOCALE_CHOSEN,
            fields: &["source", "lc_ctype", "charmap", "verified"],
        },
        EventFields {
            event: RUN_LOCALE_NOT_UTF8,
            fields: &[],
        },
        EventFields {
            event: RUN_LOCALE_UNAVAILABLE,
            fields: &[],
        },
        EventFields {
            event: RUN_PATH_CAPTURED,
            fields: &["source", "shell", "dirs", "duration_ms"],
        },
        EventFields {
            event: RUN_PATH_CHANGED,
            fields: &["added", "removed"],
        },
        EventFields {
            event: RUN_PATH_CAPTURE_FAILED,
            fields: &["kind"],
        },
        EventFields {
            event: RUNINDEX_BACKFILL_START,
            fields: &["total"],
        },
        EventFields {
            event: RUNINDEX_BACKFILL_DONE,
            fields: &["added", "unknown", "duration_ms"],
        },
        EventFields {
            event: RUNINDEX_BACKFILL_FAILED,
            fields: &[],
        },
        EventFields {
            event: RUNINDEX_UNPARSED,
            fields: &["count"],
        },
        EventFields {
            event: RUNINDEX_TORN,
            fields: &["file"],
        },
        EventFields {
            event: INBOX_PRUNED,
            fields: &["count", "bytes", "reason"],
        },
        EventFields {
            event: INBOX_OVER_CAP,
            fields: &[],
        },
        EventFields {
            event: DISK_LOW,
            fields: &["free_mib"],
        },
        EventFields {
            event: DISK_OK,
            fields: &[],
        },
        EventFields {
            event: HOUSEKEEPING_DONE,
            fields: &["deleted", "freed_bytes", "duration_ms"],
        },
        EventFields {
            event: COMPAT_STATUSLINE_MIGRATED,
            fields: &[],
        },
        EventFields {
            event: COMPAT_STATE_JSON_REMOVED,
            fields: &[],
        },
        EventFields {
            event: COMPAT_STATUSLINE_UNREADABLE,
            fields: &[],
        },
    ];
}

/// One event the webview logs through the `app_log(batch)` command, tagged by `event`
/// (#40 D1, D3). The UI can't log free text: JS error messages are never sent, only the error's
/// `name`, its stack `frames` and the message's size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum UiLogEvent {
    /// `window.onerror`.
    #[serde(rename = "ui.error")]
    Error {
        screen: String,
        /// The JS error's `name`, e.g. `TypeError`.
        name: String,
        /// Stack frames as `assets/index-<hash>.js:<line>:<col>`, symbolicated offline with the
        /// release's source maps.
        frames: Vec<String>,
        message_bytes: u64,
    },
    #[serde(rename = "ui.unhandled_rejection")]
    UnhandledRejection {
        screen: String,
        name: String,
        frames: Vec<String>,
        message_bytes: u64,
    },
    /// A React error boundary caught a render error.
    #[serde(rename = "ui.render_error")]
    RenderError {
        screen: String,
        name: String,
        frames: Vec<String>,
        message_bytes: u64,
    },
    /// The UI moved to another screen (DEBUG).
    #[serde(rename = "ui.screen")]
    Screen { screen: String },
    /// An event added later that this build doesn't know. Never sent, and not in the schema;
    /// the app counts and drops it.
    #[serde(other, rename = "unknown")]
    Unknown,
}

impl UiLogEvent {
    /// The App log event name, one of [`app::CATALOGUE`]'s; `unknown` for the catch-all.
    pub fn event(&self) -> &'static str {
        match self {
            UiLogEvent::Error { .. } => app::UI_ERROR,
            UiLogEvent::UnhandledRejection { .. } => app::UI_UNHANDLED_REJECTION,
            UiLogEvent::RenderError { .. } => app::UI_RENDER_ERROR,
            UiLogEvent::Screen { .. } => app::UI_SCREEN,
            UiLogEvent::Unknown => "unknown",
        }
    }
}

/// [`UiLogEvent`]'s schema without the catch-all. utoipa can't leave a variant out, so the
/// schema is derived from this copy, and a test checks that every known event parses as it.
mod schema {
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    /// One event the webview logs through `app_log(batch)`, tagged by `event`. JS error
    /// messages are never sent, only the error's `name`, its stack `frames` and the message's
    /// size.
    #[derive(Serialize, Deserialize, ToSchema)]
    #[serde(tag = "event")]
    pub(super) enum UiLogEvent {
        #[serde(rename = "ui.error")]
        Error {
            screen: String,
            /// The JS error's `name`, e.g. `TypeError`.
            name: String,
            /// Stack frames as `assets/index-<hash>.js:<line>:<col>`.
            frames: Vec<String>,
            message_bytes: u64,
        },
        #[serde(rename = "ui.unhandled_rejection")]
        UnhandledRejection {
            screen: String,
            name: String,
            frames: Vec<String>,
            message_bytes: u64,
        },
        #[serde(rename = "ui.render_error")]
        RenderError {
            screen: String,
            name: String,
            frames: Vec<String>,
            message_bytes: u64,
        },
        #[serde(rename = "ui.screen")]
        Screen { screen: String },
    }
}

impl PartialSchema for UiLogEvent {
    fn schema() -> RefOr<Schema> {
        schema::UiLogEvent::schema()
    }
}

impl ToSchema for UiLogEvent {
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schema::UiLogEvent::schemas(schemas);
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(UiLogEvent)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn app_event_names_are_unique_dotted_snake_case() {
        let names: Vec<_> = app::CATALOGUE.iter().map(|e| e.event).collect();
        let unique: BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
        for name in names {
            let parts: Vec<_> = name.split('.').collect();
            assert_eq!(parts.len(), 2, "{name}");
            assert!(
                parts.iter().all(
                    |p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn every_app_field_has_exactly_one_declared_class() {
        let used: BTreeSet<&str> = app::CATALOGUE
            .iter()
            .flat_map(|e| e.fields.iter().copied())
            .chain(COMMON_FIELDS.iter().copied())
            .collect();
        for field in &used {
            assert!(
                crate::export::app_field_class(field).is_some(),
                "App log field `{field}` has no APP_FIELD_CLASSES entry"
            );
        }
        let classed: BTreeSet<&str> = crate::export::APP_FIELD_CLASSES
            .iter()
            .map(|(field, _)| *field)
            .collect();
        assert_eq!(
            classed, used,
            "APP_FIELD_CLASSES lists a field no event carries"
        );
    }

    #[test]
    fn every_daemon_field_has_a_field_class() {
        let names: Vec<_> = daemon::CATALOGUE.iter().map(|e| e.event).collect();
        let unique: BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "duplicate Daemon log event");
        let missing: BTreeSet<&str> = daemon::CATALOGUE
            .iter()
            .flat_map(|e| e.fields.iter().copied())
            .chain(COMMON_FIELDS.iter().copied())
            .chain(daemon::SPAN_FIELDS.iter().copied())
            .filter(|field| {
                crate::export::FIELD_CLASSES
                    .binary_search_by(|(f, _)| f.cmp(field))
                    .is_err()
            })
            .collect();
        assert!(
            missing.is_empty(),
            "Daemon log fields with no FIELD_CLASSES entry: {missing:?}"
        );
    }

    #[test]
    fn ui_events_carry_only_their_declared_fields() {
        for (_, wire) in every_event() {
            let event = wire["event"].as_str().unwrap();
            let declared = app::CATALOGUE.iter().find(|e| e.event == event).unwrap();
            for key in wire.as_object().unwrap().keys() {
                assert!(
                    key == "event" || declared.fields.contains(&key.as_str()),
                    "{event} carries undeclared `{key}`"
                );
            }
        }
    }

    fn every_event() -> Vec<(UiLogEvent, Value)> {
        let frames = vec!["assets/index-3f2a.js:1:2345".to_string()];
        vec![
            (
                UiLogEvent::Error {
                    screen: "jobs".into(),
                    name: "TypeError".into(),
                    frames: frames.clone(),
                    message_bytes: 61,
                },
                json!({
                    "event": "ui.error",
                    "screen": "jobs",
                    "name": "TypeError",
                    "frames": ["assets/index-3f2a.js:1:2345"],
                    "message_bytes": 61,
                }),
            ),
            (
                UiLogEvent::UnhandledRejection {
                    screen: "chat".into(),
                    name: "AbortError".into(),
                    frames: vec![],
                    message_bytes: 0,
                },
                json!({
                    "event": "ui.unhandled_rejection",
                    "screen": "chat",
                    "name": "AbortError",
                    "frames": [],
                    "message_bytes": 0,
                }),
            ),
            (
                UiLogEvent::RenderError {
                    screen: "settings".into(),
                    name: "Error".into(),
                    frames,
                    message_bytes: 12,
                },
                json!({
                    "event": "ui.render_error",
                    "screen": "settings",
                    "name": "Error",
                    "frames": ["assets/index-3f2a.js:1:2345"],
                    "message_bytes": 12,
                }),
            ),
            (
                UiLogEvent::Screen {
                    screen: "logs".into(),
                },
                json!({"event": "ui.screen", "screen": "logs"}),
            ),
        ]
    }

    #[test]
    fn every_ui_event_round_trips_and_is_in_the_catalogue() {
        for (event, wire) in every_event() {
            assert_eq!(serde_json::to_value(&event).unwrap(), wire);
            assert_eq!(wire["event"], event.event());
            assert!(app::CATALOGUE.iter().any(|e| e.event == event.event()));
            assert_eq!(
                serde_json::from_value::<UiLogEvent>(wire.clone()).unwrap(),
                event
            );
            let copy: schema::UiLogEvent = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(copy).unwrap(), wire);
        }
    }

    #[test]
    fn an_unknown_ui_event_parses_as_the_catch_all() {
        let event: UiLogEvent =
            serde_json::from_value(json!({"event": "ui.click", "target": "save"})).unwrap();
        assert_eq!(event, UiLogEvent::Unknown);
    }

    #[test]
    fn a_ui_event_cannot_carry_a_message() {
        // Unknown fields are ignored, so a stray `message` never reaches the App log.
        let event: UiLogEvent = serde_json::from_value(json!({
            "event": "ui.error", "screen": "jobs", "name": "SyntaxError", "frames": [],
            "message_bytes": 40, "message": "JSON Parse error",
        }))
        .unwrap();
        assert!(!serde_json::to_string(&event).unwrap().contains("Parse"));
    }

    #[test]
    fn the_ui_event_schema_leaves_the_catch_all_out() {
        let schema = serde_json::to_string(&UiLogEvent::schema()).unwrap();
        assert!(schema.contains("ui.render_error"));
        assert!(!schema.contains("\"unknown\""));
    }
}
