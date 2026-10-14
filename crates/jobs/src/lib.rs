//! Job files (envelope, YAML, value grammar, line edits) and cron.
//!
//! Pure job-file logic from #12, as architecture §4 "`claudeclaw-jobs`" describes it. No tokio
//! and no filesystem: `store` hands this crate bytes, and gets text back.
//!
//! - [`cron`]: the cron dialect, croner 4 configured as #12 §1 says.
//! - [`JobFile`]: reading a job file: the envelope split, serde-saphyr, the strict value grammar
//!   and the one-shot rules (#12 §2–§3), with [`JobDiagnostic`]s for what #12 §4 rejects loudly
//!   or warns about.
//! - [`edit`]: [`LineEdit`]s applied to text, including the one-shot finish. Writes never
//!   re-serialise the frontmatter (#12 §2).
//! - [`Lint`]: the hook `ts-compat` uses to add the 1.x divergence lint (#12 §4, architecture §8).

pub mod cron;
mod diagnostic;
pub mod edit;
mod fields;
mod file;
mod lint;

pub use cron::{Schedule, ScheduleError};
pub use diagnostic::{JobDiagnostic, Severity, kind};
pub use edit::{EditError, LineEdit, apply, finish_one_shot, render_job, render_value};
pub use fields::{DEFAULT_RETRY_DELAY_SECS, JobFields, KNOWN_KEYS, Notify};
pub use file::JobFile;
pub use lint::Lint;
