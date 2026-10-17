//! Scopes, Launcher resolution, sessions, Conversations and notifications.
//!
//! - [`conversations`]: the per-Scope serial Turn queues ([`conversations::queue`]), the Run
//!   pipeline behind them ([`conversations::pipeline`]) and the pure prompt shaping
//!   ([`conversations::prompt`]) (architecture §4, §5, §6; #8, #9, #25, #29, #33, #37, #79).
//! - [`disk`]: the free-space guard ([`disk::DiskGuard`]) — probes, warns and gates intake.
//! - [`events`]: the event bus, a `tokio::sync::broadcast` of `api::DaemonEvent` that feeds SSE
//!   (management-api §1.6, §2.2).
//! - [`health`]: the stall monitor ([`health::HealthMonitor`]) — named and Turn beacons.
//! - [`housekeeping`]: the daily pass ([`housekeeping::Housekeeping`]) — retention rules.
//! - [`notify`]: owner notices, fanned out to each adapter's `Notifier` and to the event bus, with
//!   the invalid-job and unplanned-restart policies kept in `daemon/state/notices.json`
//!   (architecture §4, §9.1; #12, #27).
//! - [`runs`]: the Run registry service ([`runs::RunRegistry`]), which keeps
//!   `daemon/state/runs.json` through a spawn hook (architecture §4, §9.1; #29).
//! - [`scope`]: the typed Scope, its `chat` and API conversions, and the Launcher resolution chain
//!   and picker (architecture §4 `claudeclaw-core`, §6 "Scope keys", §7; #8, #17).
//! - [`sessions`]: the (Scope, Launcher) session lifecycle and its decisions — recovery, the
//!   Timeout breaker, the Auto-compact budget, the Cost baseline, parking and orphan pruning
//!   (architecture §4, §9.1; #8, #12 §6, #32, #33, #39, #44).

pub mod conversations;
pub mod disk;
pub mod events;
pub mod health;
pub mod housekeeping;
pub mod notify;
pub mod runs;
pub mod scope;
pub mod sessions;
