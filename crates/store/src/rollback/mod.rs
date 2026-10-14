//! `store::rollback`: the #14 rules that shape writes until `main` switches (architecture §8).
//!
//! Cutover-only behaviour. Each rule lives in its own file, so at the switch the maintainer can
//! delete this module, or keep parts of it on purpose, such as the Run-log naming (§8 "What stays
//! after the switch and what goes"). Reading the TS formats is not here: that is the permanent
//! drop-in contract and stays in the store's other modules.

pub mod job_state_merge;
pub mod run_log_name;
pub mod session_mirror;
