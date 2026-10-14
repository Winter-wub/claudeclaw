//! The job changes the scheduler consumes (#46 D4, D5, D6; #50).

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use claudeclaw_jobs::JobFile;
use claudeclaw_store::ScanChange;

/// One change to the job set (architecture §4 `scheduler` "Inputs", #46). `core` hands the
/// scheduler one Change scan pass at a time through
/// [`SchedulerHandle::apply`](crate::SchedulerHandle::apply), converted with
/// [`JobChange::from_scan`], plus the Daemon's own writes (#46 D5 step 6) as
/// [`JobChange::Updated`].
///
/// Its `Debug` shows only names, sizes and times: a job body is a prompt (architecture §2).
#[derive(Clone)]
pub enum JobChange {
    /// A job file's current bytes: added, modified, or written by the Daemon.
    Updated {
        /// The job name: the stem, or `<agent>/<stem>`.
        name: String,
        /// The parsed file. An invalid one unschedules the job (#12 §4).
        file: Arc<JobFile>,
        /// When this version took effect: the scan's clamped mtime (#50), or the write time for
        /// the Daemon's own writes. The scheduler keeps the earlier time while the schedule,
        /// `recurring` and `enabled` stay the same, so a body-only edit doesn't move it.
        took_effect: DateTime<Utc>,
        /// The file's Revision (management-api §1.5) as text, recorded in `jobs.json` as the
        /// revision last applied (#46 D7). `None` leaves the recorded one.
        revision: Option<String>,
    },
    /// The job file is gone (missing on two passes, #46 D3). The job becomes an Orphaned job.
    Removed {
        /// The job name.
        name: String,
    },
}

impl JobChange {
    /// The scheduler's view of one Change scan event, or `None` for events that change nothing
    /// here: an unreadable job or dir keeps the job as it is (#46 D4), and settings changes go
    /// through [`SchedulerSettings`](crate::SchedulerSettings).
    ///
    /// `core` turns a [`ScanChange::DirAvailable`] with `first_listing` into
    /// [`JobChange::Removed`] for the jobs that dir held and the pass didn't name, and does the
    /// same at Worker start for `jobs.json` records whose file vanished while stopped (#46 D7).
    pub fn from_scan(change: &ScanChange) -> Option<JobChange> {
        match change {
            ScanChange::JobAdded {
                job,
                bytes,
                took_effect,
            }
            | ScanChange::JobModified {
                job,
                bytes,
                took_effect,
            } => Some(JobChange::Updated {
                name: job.name(),
                file: Arc::new(JobFile::parse(bytes.as_bytes())),
                took_effect: DateTime::<Utc>::from(*took_effect),
                revision: Some(bytes.revision().to_string()),
            }),
            ScanChange::JobRemoved { job } => Some(JobChange::Removed { name: job.name() }),
            _ => None,
        }
    }

    /// The job this change is about.
    pub fn name(&self) -> &str {
        match self {
            JobChange::Updated { name, .. } | JobChange::Removed { name } => name,
        }
    }
}

impl fmt::Debug for JobChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobChange::Updated {
                name,
                file,
                took_effect,
                revision,
            } => f
                .debug_struct("Updated")
                .field("name", name)
                .field("text_bytes", &file.text().len())
                .field("valid", &file.is_valid())
                .field("took_effect", took_effect)
                .field("revision", revision)
                .finish(),
            JobChange::Removed { name } => f.debug_struct("Removed").field("name", name).finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_no_prompt() {
        let text = "---\nschedule: \"0 9 * * *\"\nrecurring: true\n---\nsecret prompt words\n";
        let change = JobChange::Updated {
            name: "daily".into(),
            file: Arc::new(JobFile::parse(text.as_bytes())),
            took_effect: DateTime::<Utc>::UNIX_EPOCH,
            revision: None,
        };
        let shown = format!("{change:?}");
        assert!(shown.contains("daily"));
        assert!(!shown.contains("secret"), "{shown}");
        assert_eq!(change.name(), "daily");
    }
}
