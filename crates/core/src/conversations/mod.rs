//! Conversations: the per-Scope serial Turn queues and the Run pipeline behind them
//! (architecture §4 `claudeclaw-core` → `conversations`, §5 run pipeline, §6).
//!
//! - [`queue`]: one serial queue per Scope; a task per active Scope exits when idle, and the
//!   next Turn starts only once the previous run is settled (#37).
//! - [`pipeline`]: one Chat Turn end to end — Launcher resolution, the (Scope, Launcher) session
//!   look-up and `--resume`, planning, the run itself, the session record and recovery, the Run
//!   log and Run-index record, and the outcome back to the adapter (#8, #9, #25, #29, #33, #37,
//!   #79).
//! - [`prompt`]: the pure prompt shaping — the clock prefix, `<untrusted-…>` wrapping, the
//!   System-prompt file text, model choice (agentic routing) and the timeout category.

pub mod pipeline;
pub mod prompt;
pub mod queue;
