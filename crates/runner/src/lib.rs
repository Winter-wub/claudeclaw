//! Launcher resolution, argv, spawn/settle/kill, stream-json parser and result judging.
//!
//! - [`launcher`]: [`Launcher`], [`resolve`] on the Run PATH and the Windows Launcher rules
//!   (#8, #25, #26, #48).
//! - [`env`](mod@env): the run environment: the Run PATH, the Run locale and the env changes every
//!   spawn applies (#26, #43, #47).
//! - [`argv`]: the typed `claude` argv and its guards (architecture §4 runner `argv`; #25).
//! - [`stream`]: the pure stream-json parser and `judge()` (architecture §4 runner `stream`).
//! - [`version`]: `MIN_CLAUDE_CODE` and the `claude_code_outdated` judgement (#42).

pub mod argv;
pub mod env;
pub mod launcher;
pub mod stream;
pub mod version;

pub use env::{RunEnv, RunPath};
pub use launcher::{Launcher, LauncherError, ResolvedLauncher, resolve};
