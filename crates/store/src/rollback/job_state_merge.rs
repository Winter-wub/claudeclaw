//! Merge job state with TS Run logs on every start (#14 rule 6, #17 §4, architecture §8).
//!
//! Per job, the last run is the newer of `daemon/state/jobs.json` and the newest TS-pattern Run
//! log for that job, in `logs/` or, for an agent job `<agent>/<label>`, in `logs/<agent>/`. TS's
//! own seed reads only the top-level `logs/` and marks every run `ok` (`src/commands/start.ts:
//! 893-912`); reading `logs/<agent>/` fixes trap 6, and the result comes from the newest log's
//! `Exit code:` line.
//!
//! With no `jobs.json` this is the First-start seed, and it rebuilds a lost file. Rust's own runs
//! write TS-pattern logs too (rule 5), so normally the merge changes nothing.

use chrono::{DateTime, TimeDelta, Utc};

use crate::job_state::{JobResult, JobState, JobStateError};
use crate::layout::Layout;
use crate::run_log::{self, RunLogFile, RunLogHeader, RunLogListing};

/// A log stamped further than this in the future is ignored, so it can't suppress a catch-up
/// (#17 §4).
pub const FUTURE_SKEW: TimeDelta = TimeDelta::minutes(5);

/// What a merge changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Jobs whose last run now comes from a Run log (another daemon ran them, or they were
    /// seeded), in name order.
    pub from_logs: Vec<String>,
    /// Jobs with a Run log stamped more than [`FUTURE_SKEW`] ahead of `now`, which was ignored;
    /// the caller reports them (#17 §4: "ignored with a warning").
    pub future_logs: Vec<String>,
}

impl MergeReport {
    /// Whether the state changed, so the caller should save it.
    pub fn changed(&self) -> bool {
        !self.from_logs.is_empty()
    }
}

impl JobState {
    /// Loads `jobs.json` and merges it with the newest Run log of each of `jobs` (the loaded job
    /// names; other names such as chat or heartbeat logs are ignored, #17 §4). Doesn't write:
    /// save the state when [`MergeReport::changed`].
    pub fn load_merged<S: AsRef<str>>(
        layout: &Layout,
        listing: &RunLogListing,
        jobs: impl IntoIterator<Item = S>,
        now: DateTime<Utc>,
    ) -> Result<(JobState, MergeReport), JobStateError> {
        let mut state = JobState::load(layout)?;
        let report = state.merge_logs(listing, jobs, now)?;
        Ok((state, report))
    }

    /// The merge itself (#14 rule 6). Per job, when its newest log started after the recorded
    /// `last_started_at` (or there is none), that log wins: `last_started_at` becomes its name
    /// time and `last_result` its `Exit code:`. The pending retry and an `interrupted_at` older
    /// than the log are cleared, so a run TS already made doesn't re-fire (#17 §4).
    pub fn merge_logs<S: AsRef<str>>(
        &mut self,
        listing: &RunLogListing,
        jobs: impl IntoIterator<Item = S>,
        now: DateTime<Utc>,
    ) -> Result<MergeReport, JobStateError> {
        let limit = now + FUTURE_SKEW;
        let mut report = MergeReport::default();
        let mut jobs: Vec<S> = jobs.into_iter().collect();
        jobs.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
        jobs.dedup_by(|a, b| a.as_ref() == b.as_ref());
        for job in jobs {
            let name = job.as_ref();
            let (newest, future) = newest_log(listing, name, limit);
            if future {
                report.future_logs.push(name.to_owned());
            }
            let Some(log) = newest else { continue };
            if self
                .get(name)
                .and_then(|r| r.last_started_at)
                .is_some_and(|t| t >= log.started_at)
            {
                continue;
            }
            let header = match RunLogHeader::read(&log.path, None) {
                Ok(header) => header,
                // Deleted since the listing.
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(e.into()),
            };
            let record = self.entry(name);
            record.last_started_at = Some(log.started_at);
            record.last_result = Some(JobResult {
                outcome: run_log::exit_outcome(header.exit_code),
                exit_code: header.exit_code,
            });
            // The retry belonged to the older run the log supersedes.
            record.retry_at = None;
            record.fail_count = 0;
            if record.interrupted_at.is_some_and(|t| t < log.started_at) {
                record.interrupted_at = None;
            }
            report.from_logs.push(name.to_owned());
        }
        Ok(report)
    }
}

/// `name`'s newest log at or before `limit`, and whether a newer one past `limit` was skipped.
fn newest_log<'a>(
    listing: &'a RunLogListing,
    name: &str,
    limit: DateTime<Utc>,
) -> (Option<&'a RunLogFile>, bool) {
    let mut newest: Option<&RunLogFile> = None;
    let mut future = false;
    for file in listing.files.iter().filter(|f| f.name == name) {
        if file.started_at > limit {
            future = true;
        } else if newest.is_none_or(|n| file.started_at > n.started_at) {
            newest = Some(file);
        }
    }
    (newest, future)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use claudeclaw_api::runs::RunOutcome;

    use super::*;
    use crate::rollback::run_log_name;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        fs::create_dir_all(layout.run_logs_dir()).unwrap();
        (dir, layout)
    }

    /// Writes a TS log for `name` (`<agent>/<label>` lands in `logs/<agent>/`).
    fn ts_log(layout: &Layout, name: &str, start: &str, exit: &str) {
        let (dir, label) = match name.split_once('/') {
            Some((agent, label)) => (layout.agent_run_logs_dir(agent), label),
            None => (layout.run_logs_dir(), name),
        };
        fs::create_dir_all(&dir).unwrap();
        // The TS body (`src/runner.ts:1350-1360`), with a prompt line that mimics the key.
        let text = format!(
            "# {name}\nDate: {start}\nSession: s (new)\nModel config: primary\n\
             Prompt: say Exit code: 0\n{exit}\n## Output\nok"
        );
        fs::write(dir.join(run_log_name::file_name(label, at(start))), text).unwrap();
    }

    fn result(outcome: RunOutcome, exit_code: Option<i32>) -> Option<JobResult> {
        Some(JobResult { outcome, exit_code })
    }

    #[test]
    fn with_no_state_the_newest_log_per_job_seeds_it() {
        let (_dir, layout) = layout();
        ts_log(&layout, "daily", "2026-10-12T09:00:00Z", "Exit code: 0\n");
        ts_log(&layout, "daily", "2026-10-13T09:00:00Z", "Exit code: 1\n");
        // Trap 6: TS never read `logs/<agent>/`.
        ts_log(
            &layout,
            "ops/digest",
            "2026-10-13T10:00:00Z",
            "Exit code: 124\n",
        );
        ts_log(&layout, "weekly", "2026-10-10T09:00:00Z", "");
        // Not a loaded job: ignored.
        ts_log(&layout, "chat", "2026-10-13T11:00:00Z", "Exit code: 0\n");
        let listing = RunLogListing::scan(&layout).unwrap();
        let now = at("2026-10-14T00:00:00Z");

        let (state, report) = JobState::load_merged(
            &layout,
            &listing,
            ["daily", "ops/digest", "weekly", "never-ran"],
            now,
        )
        .unwrap();
        assert_eq!(report.from_logs, ["daily", "ops/digest", "weekly"]);
        assert!(report.changed());
        let got = |n: &str| {
            let r = state.get(n).unwrap();
            (r.last_started_at.unwrap(), r.last_result)
        };
        assert_eq!(
            got("daily"),
            (
                at("2026-10-13T09:00:00Z"),
                result(RunOutcome::Failed, Some(1))
            )
        );
        assert_eq!(
            got("ops/digest"),
            (
                at("2026-10-13T10:00:00Z"),
                result(RunOutcome::Timeout, Some(124))
            )
        );
        assert_eq!(
            got("weekly"),
            (
                at("2026-10-10T09:00:00Z"),
                result(RunOutcome::Unknown, None)
            )
        );
        assert!(state.get("never-ran").is_none());
        assert!(state.get("chat").is_none());
        // Nothing written.
        assert!(!layout.job_state().exists());
    }

    #[test]
    fn the_newer_side_wins() {
        let (_dir, layout) = layout();
        ts_log(
            &layout,
            "rust-newer",
            "2026-10-13T09:00:00Z",
            "Exit code: 1\n",
        );
        ts_log(
            &layout,
            "ts-newer",
            "2026-10-13T09:00:00Z",
            "Exit code: 0\n",
        );
        let listing = RunLogListing::scan(&layout).unwrap();

        let mut state = JobState::default();
        let rust = state.entry("rust-newer");
        rust.last_started_at = Some(at("2026-10-13T10:00:00Z"));
        rust.last_result = result(RunOutcome::Ok, Some(0));
        rust.retry_at = Some(at("2026-10-13T10:05:00Z"));
        rust.fail_count = 1;
        // Rust ran, failed and was interrupted; then TS (after a rollback) ran it again.
        let ts = state.entry("ts-newer");
        ts.last_started_at = Some(at("2026-10-13T08:00:00Z"));
        ts.last_result = result(RunOutcome::Failed, Some(1));
        ts.retry_at = Some(at("2026-10-13T08:05:00Z"));
        ts.fail_count = 2;
        ts.interrupted_at = Some(at("2026-10-13T08:01:00Z"));
        ts.revision = Some("00d74baf14ea415c".parse().unwrap());
        state.save(&layout).unwrap();

        let (merged, report) = JobState::load_merged(
            &layout,
            &listing,
            ["ts-newer", "rust-newer"],
            at("2026-10-14T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.from_logs, ["ts-newer"]);
        assert_eq!(merged.get("rust-newer"), state.get("rust-newer"));
        let ts = merged.get("ts-newer").unwrap();
        assert_eq!(ts.last_started_at, Some(at("2026-10-13T09:00:00Z")));
        assert_eq!(ts.last_result, result(RunOutcome::Ok, Some(0)));
        assert_eq!(
            (ts.retry_at, ts.fail_count, ts.interrupted_at),
            (None, 0, None)
        );
        assert_eq!(ts.revision, state.get("ts-newer").unwrap().revision);

        // Once saved, merging again changes nothing: Rust's record now equals the newest log.
        merged.save(&layout).unwrap();
        let (_, again) =
            JobState::load_merged(&layout, &listing, ["ts-newer"], at("2026-10-14T00:00:00Z"))
                .unwrap();
        assert_eq!(again, MergeReport::default());
    }

    #[test]
    fn an_interruption_after_the_log_is_kept() {
        let (_dir, layout) = layout();
        ts_log(&layout, "daily", "2026-10-13T09:00:00Z", "Exit code: 0\n");
        let listing = RunLogListing::scan(&layout).unwrap();
        let mut state = JobState::default();
        state.entry("daily").interrupted_at = Some(at("2026-10-13T09:30:00Z"));
        state
            .merge_logs(&listing, ["daily"], at("2026-10-14T00:00:00Z"))
            .unwrap();
        assert_eq!(
            state.get("daily").unwrap().interrupted_at,
            Some(at("2026-10-13T09:30:00Z"))
        );
    }

    #[test]
    fn a_log_from_the_future_is_ignored_and_reported() {
        let (_dir, layout) = layout();
        ts_log(&layout, "daily", "2026-10-13T09:00:00Z", "Exit code: 0\n");
        ts_log(&layout, "daily", "2026-10-14T00:06:00Z", "Exit code: 1\n");
        ts_log(&layout, "soon", "2026-10-14T00:04:59Z", "Exit code: 0\n");
        let listing = RunLogListing::scan(&layout).unwrap();
        let mut state = JobState::default();
        let report = state
            .merge_logs(&listing, ["daily", "soon"], at("2026-10-14T00:00:00Z"))
            .unwrap();
        assert_eq!(report.future_logs, ["daily"]);
        assert_eq!(
            state.get("daily").unwrap().last_started_at,
            Some(at("2026-10-13T09:00:00Z"))
        );
        // Within 5 minutes still counts.
        assert_eq!(
            state.get("soon").unwrap().last_started_at,
            Some(at("2026-10-14T00:04:59Z"))
        );
    }
}
