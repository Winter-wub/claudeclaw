//! Sessions (architecture §4 `claudeclaw-store` "Sessions", §9.1).
//!
//! - [`state`]: the Rust (Scope, Launcher) session store in `daemon/state/sessions.json` (#8),
//!   with each session's Transcript location, context and Cost baseline (#39), Timeout breaker
//!   (#32), Auto-compact budget (#33) and orphan clock (#12 §6, #44).
//! - [`ts`]: the TS session files (owner S) in their TS shapes: `session.json`, `sessions.json`,
//!   `agents/<agent>/session.json`, the fallback sessions and `session_<N>.backup`.
//!
//! The store only persists. Which session a Turn uses, and when a session rotates, recovers or
//! trips its breaker, is `core::sessions`'.

pub mod state;
pub mod ts;

use std::path::PathBuf;

use crate::error::StoreError;

pub use state::{
    AutoCompact, CostState, ModelCost, SessionEntry, SessionStore, TimeoutBreaker,
    TranscriptLocation, TranscriptVia,
};
pub use ts::{SessionBackup, TsKey, TsSession};

/// A failed session-file operation (§4). Messages name the file, never its contents.
#[derive(Debug, thiserror::Error)]
pub enum SessionsError {
    /// `daemon/state/sessions.json` was written by a newer build; this one would misread it.
    #[error("{} has schema {found}, newer than this build's {supported}", path.display())]
    NewerSchema {
        path: PathBuf,
        found: u64,
        supported: u32,
    },
    /// The next `session_<N>.backup` number doesn't fit in a `u32`.
    #[error("no session backup number is left after {last}")]
    BackupNumberExhausted { last: u64 },
    #[error(transparent)]
    Store(#[from] StoreError),
}
