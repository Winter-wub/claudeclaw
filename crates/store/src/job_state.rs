//! Job state: `daemon/state/jobs.json`, the per-job record of the last run, its result, the
//! in-flight guard, retries and interrupted runs (#9), with the `revision` last applied and
//! `orphaned_since` (#46 D4, D7).
//!
//! Rust-only, so the store owns its shape. It is loaded and saved whole, atomically, whenever
//! it changes (#9), and keys this build doesn't know are kept, at the top level and per job.
//! The per-start merge with TS Run logs (#14 rule 6) is `JobState::load_merged` in
//! [`crate::rollback::job_state_merge`].

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use claudeclaw_api::revision::Revision;
use claudeclaw_api::runs::RunOutcome;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::atomic::{self, FileMode};
use crate::error::StoreError;
use crate::layout::Layout;

/// `jobs.json` could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum JobStateError {
    /// The file is not valid JSON or not job state. The serde error carries only a line and
    /// column.
    #[error("{} is not valid job state: {source}", path.display())]
    Invalid {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Every job's state, keyed by job name (the file stem, or `<agent>/<stem>`) (#9, #46).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobState {
    #[serde(default)]
    pub jobs: BTreeMap<String, JobRecord>,
    /// Top-level keys this build doesn't know.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// One job's state (#9, #46 D4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    /// When the job's last run started: what catch-up compares with the previous due slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at: Option<DateTime<Utc>>,
    /// How that run ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<JobResult>,
    /// When the run in flight started; `None` when none is (the in-flight guard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_flight_since: Option<DateTime<Utc>>,
    /// When a failed run is retried; `None` when no retry is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<DateTime<Utc>>,
    /// Failed attempts so far toward the job's `retry` count.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fail_count: u32,
    /// When a run of this job was interrupted by a shutdown or proved dead by a Reap, so
    /// catch-up re-runs it (#9, #29); `None` when nothing is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<DateTime<Utc>>,
    /// The job file's Revision last applied (#46 D7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<Revision>,
    /// When the job file was confirmed removed; cleared if it comes back (#46 D4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orphaned_since: Option<DateTime<Utc>>,
    /// Keys this build doesn't know.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// How a job's last run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    /// `unknown` when seeded from a TS log with no `Exit code:` line (#17 §4).
    pub outcome: RunOutcome,
    /// `None` when there was no exit code (Refused, killed, or unknown).
    #[serde(default)]
    pub exit_code: Option<i32>,
}

impl JobState {
    /// Reads `jobs.json`; a missing file is empty state.
    pub fn load(layout: &Layout) -> Result<Self, JobStateError> {
        let path = layout.job_state();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(JobState::default()),
            Err(e) => return Err(StoreError::io("read", &path)(e).into()),
        };
        serde_json::from_slice(&bytes).map_err(|source| JobStateError::Invalid { path, source })
    }

    /// Writes `jobs.json` atomically (§9.1), creating `daemon/` (0700) if needed.
    pub fn save(&self, layout: &Layout) -> Result<(), JobStateError> {
        if !layout.state_dir().is_dir() {
            layout.create_daemon_dirs()?;
        }
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|source| JobStateError::Invalid {
                path: layout.job_state(),
                source,
            })?;
        bytes.push(b'\n');
        atomic::write_atomic(&layout.job_state(), &bytes, FileMode::Keep)?;
        Ok(())
    }

    /// `name`'s record, if it has one.
    pub fn get(&self, name: &str) -> Option<&JobRecord> {
        self.jobs.get(name)
    }

    /// `name`'s record, created empty if missing.
    pub fn entry(&mut self, name: &str) -> &mut JobRecord {
        self.jobs.entry(name.to_owned()).or_default()
    }

    /// Drops `name`'s record (an API delete, or the 30-day orphan prune, #46 D4).
    pub fn remove(&mut self, name: &str) -> Option<JobRecord> {
        self.jobs.remove(name)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.root()).unwrap();
        (dir, layout)
    }

    #[test]
    fn a_missing_file_is_empty_state() {
        let (_dir, layout) = layout();
        assert_eq!(JobState::load(&layout).unwrap(), JobState::default());
    }

    #[test]
    fn state_round_trips_and_keeps_unknown_keys() {
        let (_dir, layout) = layout();
        std::fs::create_dir_all(layout.state_dir()).unwrap();
        let text = json!({
            "added_in_2_9": {"x": 1},
            "jobs": {
                "ops/digest": {
                    "last_started_at": "2026-10-13T08:30:27.512Z",
                    "last_result": {"outcome": "timeout", "exit_code": 124},
                    "retry_at": "2026-10-13T08:35:27Z",
                    "fail_count": 1,
                    "revision": "00d74baf14ea415c",
                    "future_key": [1, 2],
                },
            },
        });
        std::fs::write(layout.job_state(), text.to_string()).unwrap();

        let mut state = JobState::load(&layout).unwrap();
        let digest = state.get("ops/digest").unwrap();
        assert_eq!(digest.last_started_at, Some(at("2026-10-13T08:30:27.512Z")));
        assert_eq!(
            digest.last_result,
            Some(JobResult {
                outcome: RunOutcome::Timeout,
                exit_code: Some(124)
            })
        );
        assert_eq!(digest.fail_count, 1);
        assert_eq!(digest.revision.unwrap().to_string(), "00d74baf14ea415c");

        let daily = state.entry("daily");
        daily.in_flight_since = Some(at("2026-10-14T00:00:00Z"));
        state.save(&layout).unwrap();

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(layout.job_state()).unwrap()).unwrap();
        assert_eq!(saved["added_in_2_9"], json!({"x": 1}));
        assert_eq!(saved["jobs"]["ops/digest"]["future_key"], json!([1, 2]));
        assert_eq!(
            saved["jobs"]["daily"],
            json!({"in_flight_since": "2026-10-14T00:00:00Z"})
        );
        assert_eq!(JobState::load(&layout).unwrap(), state);

        assert!(state.remove("daily").is_some());
        assert!(state.get("daily").is_none());
    }

    #[test]
    fn save_creates_the_private_daemon_dir() {
        let (_dir, layout) = layout();
        JobState::default().save(&layout).unwrap();
        assert!(layout.job_state().is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(layout.daemon_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[test]
    fn invalid_json_is_an_error_naming_only_the_file() {
        let (_dir, layout) = layout();
        std::fs::create_dir_all(layout.state_dir()).unwrap();
        std::fs::write(layout.job_state(), "{\"jobs\": secret-ish").unwrap();
        let err = JobState::load(&layout).unwrap_err();
        assert!(matches!(err, JobStateError::Invalid { .. }));
        assert!(!err.to_string().contains("secret-ish"));
    }
}
