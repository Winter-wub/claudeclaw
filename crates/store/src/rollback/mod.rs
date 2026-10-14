//! `store::rollback`: the #14 rules that shape writes until `main` switches (architecture §8).
//!
//! Each rule lives in its own file, so the maintainer can delete this module at the switch, or
//! keep parts of it on purpose (for example, the Run-log naming).

pub mod job_state_merge;
pub mod run_log_name;
