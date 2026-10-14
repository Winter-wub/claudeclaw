//! Launcher resolution, argv, spawn/settle/kill, stream-json parser and result judging.
//!
//! - [`launcher`]: [`Launcher`], [`resolve`] on the Run PATH and the Windows Launcher rules
//!   (#8, #25, #26, #48).
//! - [`env`](mod@env): the run environment: the Run PATH, the Run locale and the env changes every
//!   spawn applies (#26, #43, #47).

pub mod env;
pub mod launcher;

pub use env::{RunEnv, RunPath};
pub use launcher::{Launcher, LauncherError, ResolvedLauncher, resolve};
