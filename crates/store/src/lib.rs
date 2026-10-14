//! On-disk layout, atomic IO, settings, sessions, job state, logs, lock and pid
//! (architecture §4 `claudeclaw-store`).
//!
//! The only crate that knows paths under `<project-dir>`. It is synchronous (`std::fs`, small
//! files); async callers wrap it in `spawn_blocking` (architecture §2).
//!
//! What is here:
//! - [`layout`]: every §9.1 path, `.claude/settings.json` (#24) and the §9.3 socket rule;
//! - [`atomic`]: temp file, `fsync`, mode, rename (§9.1 "Write discipline");
//! - [`settings`]: the typed read view of `settings.json` and patch writes over the raw JSON
//!   (§7, #14 rules 1–2 and 4, #23);
//! - [`claude_settings`]: the single-key `statusLine` patch of `.claude/settings.json` (#24);
//! - [`scan`]: the Change scan over the job dirs and `settings.json`, no `notify` (#46, #50);
//! - [`lock`] and [`pid`]: `daemon.lock`, its holder record and probe, and `daemon.pid`
//!   (#9, #14 rule 10, #20, #22, #45);
//! - [`run_log`] and [`run_index`]: Run logs in the TS pattern and the Run index, with its
//!   Back-fill and month compaction (#14 rule 5, #23, #44);
//! - [`job_state`]: `daemon/state/jobs.json` (#9, #46), merged with TS Run logs on every start
//!   by [`rollback::job_state_merge`] (#14 rule 6, #17);
//! - [`audit`]: the append-only audit log (#13);
//! - [`logs`]: reading the Daemon log back as `api::LogEntry`, for `logs --offline` (#20, #23);
//! - [`export`]: the Bug-report export, scrubbed with Pseudonyms and failing closed (#28, #69);
//! - [`rollback`]: the cutover-only rules (§8).
//!
//! Still to come, each as its own module beside these (§4): `sessions` (the (Scope, Launcher)
//! store and the TS mirror) and `run_registry` (#29).

pub mod atomic;
pub mod audit;
pub mod claude_settings;
mod error;
pub mod export;
pub mod job_state;
mod jsonl;
pub mod layout;
pub mod lock;
pub mod logs;
pub mod pid;
pub mod rollback;
pub mod run_index;
pub mod run_log;
pub mod scan;
pub mod settings;

pub use atomic::{DirMode, FileMode, write_atomic};
pub use audit::{AuditError, AuditLog, AuditLogPage, PruneReport};
pub use error::StoreError;
pub use job_state::{JobRecord, JobResult, JobState, JobStateError};
pub use layout::{ApiEndpoint, Layout, RuntimeEnv};
pub use lock::{DaemonLock, Holder, HolderRole, LockError, LockProbe, LockWait};
pub use pid::{PidCheck, PidError};
pub use rollback::job_state_merge::MergeReport;
pub use run_index::{BackfillReport, CompactReport, RunIndex, RunIndexError, RunIndexPage};
pub use run_log::{
    KnownSecret, RunLog, RunLogBody, RunLogError, RunLogFile, RunLogHeader, RunLogListing,
    RunSection,
};
pub use scan::{FsView, ScanChange, ScanError, ScanTargets, Scanner, StdFs};
pub use settings::{Secrets, SettingsFile, SettingsRead};
