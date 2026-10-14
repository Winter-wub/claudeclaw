//! `store::rollback`: the #14 rules that shape writes until `main` switches (architecture §8).
//!
//! Cutover-only behaviour. At the switch the maintainer deletes this module, or keeps parts of it
//! on purpose (§8 "What stays after the switch and what goes"). Reading the TS formats is not
//! here: that is the permanent drop-in contract and stays in the store's other modules.

pub mod session_mirror;
