//! Launcher resolution, argv, spawn/settle/kill, stream-json parser and result judging.
//!
//! - [`argv`]: the typed `claude` argv and its guards (architecture §4 runner `argv`; #25).
//! - [`stream`]: the pure stream-json parser and `judge()` (architecture §4 runner `stream`).
//! - [`version`]: `MIN_CLAUDE_CODE` and the `claude_code_outdated` judgement (#42).

pub mod argv;
pub mod stream;
pub mod version;
