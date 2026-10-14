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
//!   (#9, #14 rule 10, #20, #22, #45).
//!
//! Still to come, each as its own module beside these (§4):
//! `sessions` (the (Scope, Launcher) store and the TS mirror), `job_state` (#9, #46),
//! `run_log` and `run_index` (#23, #44), `run_registry` (#29), `audit` (#13), `logs` (#20),
//! `export` (#28) and `rollback` (§8).

pub mod atomic;
pub mod claude_settings;
mod error;
pub mod layout;
pub mod lock;
pub mod pid;
pub mod scan;
pub mod settings;

pub use atomic::{DirMode, FileMode, write_atomic};
pub use error::StoreError;
pub use layout::{ApiEndpoint, Layout, RuntimeEnv};
pub use lock::{DaemonLock, Holder, HolderRole, LockError, LockProbe, LockWait};
pub use pid::{PidCheck, PidError};
pub use scan::{FsView, ScanChange, ScanError, ScanTargets, Scanner, StdFs};
pub use settings::{Secrets, SettingsFile, SettingsRead};
