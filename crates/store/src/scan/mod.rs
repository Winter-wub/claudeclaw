//! The Change scan (architecture §4 `claudeclaw-store`, §7, #46, #50): how the Worker notices
//! edits to job files and `settings.json` without a `notify` watcher.
//!
//! The scan is synchronous and owns no timer. Its caller (the Worker's `core::settings` task)
//! runs [`Scanner::startup_pass`] once, then [`Scanner::stat_pass`] every 2 s and
//! [`Scanner::hash_pass`] every 30 s, and feeds the returned [`ScanChange`]s to the scheduler and
//! core. All filesystem access goes through [`FsView`], so tests drive passes over a fake tree.
//!
//! The rules (#46 D1–D4):
//! - **What is scanned**: `*.md` in the flat jobs dir and in `agents/<agent>/jobs/` (not
//!   recursive), plus `settings.json`. Names starting with `.` are ignored, as are agents whose
//!   name starts with `.` (`agents/.archive/`).
//! - **Stability**: a file whose stat key differs from the snapshot becomes *pending*. It is read
//!   once the next pass sees the same key again, and re-stat'd after the read. Stable bytes equal
//!   to the snapshot's only refresh the stored key. A file that keeps changing for
//!   [`UNSTABLE_AFTER`] is applied by the next hash pass anyway.
//! - **Absence**: a known file missing on one pass is *missing*; it is removed only if it is
//!   still missing on the next pass. A dir or file that can't be read changes nothing.
//! - **The hash pass** also reads every file whose key is unchanged and compares SHA-256, which
//!   catches what a stat key misses (a same-size write inside one timestamp tick).
//! - **The Daemon's own writes** call [`check_unchanged`] just before the rename and
//!   [`Scanner::record_own_write`] after it, so they are never reported back (#46 D5).
//!
//! Within one pass's changes, removals come before additions and modifications, so a consumer
//! keyed by job name ends with the file that is there now (a `jobsDir` re-target can remove one
//! `a.md` and add another).

mod fs;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use claudeclaw_api::Revision;
use claudeclaw_api::log_event::daemon as log_event;
use sha2::{Digest, Sha256};

pub use fs::{FileKind, FileStat, FsView, StatKey, StdFs};

use crate::layout::Layout;

/// How long a file may keep changing on every pass before the hash pass applies it anyway, with
/// WARN `job.file_unstable` once per episode (#46 D3).
pub const UNSTABLE_AFTER: Duration = Duration::from_secs(30);

/// What the scan watches (#46 D2, D8). Every path comes from [`Layout`]; only the flat jobs dir
/// depends on `jobsDir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanTargets {
    layout: Layout,
    jobs_dir: PathBuf,
    jobs_dir_explicit: bool,
}

impl ScanTargets {
    /// The targets of `layout` for a `jobsDir` setting (the raw string, if set).
    pub fn new(layout: &Layout, jobs_dir: Option<&str>) -> Self {
        ScanTargets {
            layout: layout.clone(),
            jobs_dir: layout.jobs_dir_for(jobs_dir),
            jobs_dir_explicit: jobs_dir.is_some_and(|dir| !dir.trim().is_empty()),
        }
    }

    /// The project's layout.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The flat jobs dir: `settings.jobsDir` ([`Layout::jobs_dir_for`]) or
    /// `.claude/claudeclaw/jobs/`.
    pub fn jobs_dir(&self) -> &Path {
        &self.jobs_dir
    }

    /// Whether `jobsDir` was set. A missing explicit jobs dir is reported as unavailable; a
    /// missing default one means zero jobs, as on a fresh install (#46 D4).
    pub fn jobs_dir_explicit(&self) -> bool {
        self.jobs_dir_explicit
    }

    /// The path of `dir`.
    pub fn path_of(&self, dir: &WatchedDir) -> PathBuf {
        match dir {
            WatchedDir::Jobs => self.jobs_dir.clone(),
            WatchedDir::Agents => self.layout.agents_dir(),
            WatchedDir::AgentJobs(agent) => self.layout.agent_jobs_dir(agent),
        }
    }
}

/// A dir the scan lists (#46 D2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WatchedDir {
    /// The flat jobs dir.
    Jobs,
    /// `agents/`: every agent's `jobs/` lies under it.
    Agents,
    /// `agents/<agent>/jobs/`.
    AgentJobs(String),
}

impl WatchedDir {
    /// Whether `job`'s file lies in this dir (for [`WatchedDir::Agents`], in any agent's).
    pub fn holds(&self, job: &JobFile) -> bool {
        match self {
            WatchedDir::Jobs => job.agent.is_none(),
            WatchedDir::Agents => job.agent.is_some(),
            WatchedDir::AgentJobs(agent) => job.agent.as_deref() == Some(agent.as_str()),
        }
    }

    /// The `dir_kind` log field (#46 D9).
    fn kind(&self) -> &'static str {
        match self {
            WatchedDir::Jobs => "jobs",
            WatchedDir::Agents => "agents",
            WatchedDir::AgentJobs(_) => "agent_jobs",
        }
    }
}

/// A job file the scan found (#46 D2): the agent, if it lives in `agents/<agent>/jobs/`, and the
/// label, which is the file stem.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobFile {
    agent: Option<String>,
    label: String,
    path: PathBuf,
}

impl JobFile {
    /// The job name: the label for a flat job, `<agent>/<label>` for an agent job (TS
    /// `src/jobs.ts:118` and `:141`).
    pub fn name(&self) -> String {
        match &self.agent {
            Some(agent) => format!("{agent}/{}", self.label),
            None => self.label.clone(),
        }
    }

    /// The agent whose `jobs/` dir holds the file.
    pub fn agent(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// The file stem.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A file's bytes as the scan read them, with their Revision (management-api §1.5). Its `Debug`
/// shows only the size and Revision: job bodies are prompts and `settings.json` holds secrets
/// (architecture §2).
#[derive(Clone, PartialEq, Eq)]
pub struct FileBytes {
    bytes: Vec<u8>,
    revision: Revision,
}

impl FileBytes {
    fn new(bytes: Vec<u8>) -> Self {
        let revision = Revision::of(&bytes);
        FileBytes { bytes, revision }
    }

    /// The bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The bytes, by value.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// The first 16 hex of SHA-256 over the bytes.
    pub fn revision(&self) -> Revision {
        self.revision
    }
}

impl fmt::Debug for FileBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileBytes")
            .field("len", &self.bytes.len())
            .field("revision", &self.revision)
            .finish()
    }
}

/// One change a pass found (#46 D3, D4, D7, D8). It carries bytes, never parsed jobs: `store`
/// doesn't depend on `jobs` (§3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanChange {
    /// A job file the snapshot didn't hold. `took_effect` is when its schedule took effect: the
    /// file's mtime, clamped between the previous pass and the pass that first saw this
    /// version (#50).
    JobAdded {
        job: JobFile,
        bytes: FileBytes,
        took_effect: SystemTime,
    },
    /// A known job file whose bytes changed (or were read for the first time after
    /// [`ScanChange::JobUnreadable`]). `took_effect` as for [`ScanChange::JobAdded`].
    JobModified {
        job: JobFile,
        bytes: FileBytes,
        took_effect: SystemTime,
    },
    /// A known job file missing on two passes in a row.
    JobRemoved { job: JobFile },
    /// A job file that exists but can't be stat'd or read (`EACCES`, `EIO`) on a full listing
    /// (the startup pass, or a dir's first listing): keep the job as it is (#46 D4). A later
    /// read reports it as [`ScanChange::JobModified`], its absence as
    /// [`ScanChange::JobRemoved`].
    JobUnreadable { job: JobFile, error: io::ErrorKind },
    /// `settings.json` changed; `None` once it has been missing on two passes in a row (it then
    /// reads as `{}`, like [`crate::SettingsFile::read`]).
    SettingsChanged { bytes: Option<FileBytes> },
    /// `settings.json` exists but can't be read, on the startup pass: keep the settings as they
    /// are (#46 D8, §7).
    SettingsUnreadable { error: io::ErrorKind },
    /// A watched dir can't be read (`EACCES`, `EIO`, or an explicit `jobsDir` that is missing).
    /// Its jobs ([`WatchedDir::holds`]) keep their snapshot until it can be read again (#46 D4).
    /// Sent once per episode.
    DirUnavailable {
        dir: WatchedDir,
        path: PathBuf,
        error: io::ErrorKind,
    },
    /// A dir reported by [`ScanChange::DirUnavailable`] is readable again, or no longer watched.
    /// `first_listing` means it was unreadable since the startup pass: the job changes in this
    /// same pass are then its whole contents, as at startup, and a job it holds that they don't
    /// name is gone.
    DirAvailable {
        dir: WatchedDir,
        path: PathBuf,
        first_listing: bool,
    },
}

/// A failed Change scan call.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// An OS call failed on `path`.
    #[error("{op} {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The file's stat key moved since the writer read it (#46 D5 step 5).
    #[error("{} changed on disk", path.display())]
    ChangedOnDisk { path: PathBuf },
    /// The path is not a job file or `settings.json` under the current [`ScanTargets`].
    #[error("{} is not a file the Change scan watches", path.display())]
    NotWatched { path: PathBuf },
}

/// The stat key of `path`, or `None` if it doesn't exist. The Daemon's writer takes it when it
/// reads the file (#46 D5 step 1).
pub fn stat_key(fs: &impl FsView, path: &Path) -> Result<Option<StatKey>, ScanError> {
    match fs.stat(path) {
        Ok(stat) => Ok(Some(stat.key)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ScanError::Io {
            op: "stat",
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The re-stat just before the Daemon's rename (#46 D5 step 5): fails with
/// [`ScanError::ChangedOnDisk`] unless `path`'s key is still `before` (`None`: still absent).
pub fn check_unchanged(
    fs: &impl FsView,
    path: &Path,
    before: Option<StatKey>,
) -> Result<(), ScanError> {
    if stat_key(fs, path)? == before {
        Ok(())
    } else {
        Err(ScanError::ChangedOnDisk {
            path: path.to_path_buf(),
        })
    }
}

/// The Change scan's snapshot of every watched file (#46).
#[derive(Debug)]
pub struct Scanner {
    targets: ScanTargets,
    files: BTreeMap<PathBuf, Tracked>,
    /// Dirs in an unavailable episode.
    unavailable: BTreeMap<PathBuf, WatchedDir>,
    /// Dirs unreadable since the startup pass, so never listed.
    unlisted: BTreeSet<PathBuf>,
    last_pass: Option<SystemTime>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Startup,
    Stat,
    Hash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Job(JobFile),
    Settings,
}

#[derive(Debug)]
struct Tracked {
    target: Target,
    /// Whether the consumers know the file exists: it was reported (added, or unreadable on a
    /// full listing) or written by the Daemon. Only a known file's absence is reported.
    known: bool,
    /// The bytes last applied; `None` until the file is first read.
    applied: Option<Applied>,
    state: State,
    /// `job.file_unstable` was logged in this episode.
    unstable_warned: bool,
}

#[derive(Debug, Clone, Copy)]
struct Applied {
    key: StatKey,
    digest: [u8; 32],
    /// When these bytes were last read or written: the earliest a change the stat key misses
    /// can have happened (#50).
    read_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy)]
enum State {
    Steady,
    Pending(Pending),
    Missing,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    key: StatKey,
    /// The pass that first saw `key`, and the pass before it: the window the mtime is clamped
    /// to (#50).
    first_seen: SystemTime,
    prev_pass: Option<SystemTime>,
    /// When this run of changes started, for [`UNSTABLE_AFTER`].
    since: SystemTime,
}

#[derive(Default)]
struct Observed {
    files: BTreeMap<PathBuf, (Target, FileStat)>,
    /// Candidates that are there but can't be stat'd: their tracked state is kept.
    unreadable: BTreeMap<PathBuf, (Target, io::ErrorKind)>,
    /// Dirs that couldn't be listed: their tracked files keep their state.
    blind: Vec<PathBuf>,
    unavailable: BTreeMap<PathBuf, (WatchedDir, io::ErrorKind, Option<i32>)>,
}

impl Observed {
    fn is_blind(&self, path: &Path) -> bool {
        self.unreadable.contains_key(path) || self.blind.iter().any(|b| path.starts_with(b))
    }
}

/// One pass's output, kept apart so removals come first.
#[derive(Default)]
struct Out {
    dirs: Vec<ScanChange>,
    removals: Vec<ScanChange>,
    updates: Vec<ScanChange>,
}

impl Scanner {
    /// An empty snapshot over `targets`.
    pub fn new(targets: ScanTargets) -> Self {
        Scanner {
            targets,
            files: BTreeMap::new(),
            unavailable: BTreeMap::new(),
            unlisted: BTreeSet::new(),
            last_pass: None,
        }
    }

    /// What the scan watches.
    pub fn targets(&self) -> &ScanTargets {
        &self.targets
    }

    /// Re-targets the scan when `jobsDir` changes (#46 D8). Files outside the new targets are
    /// removed by the next two passes; the caller runs a hash pass at once.
    pub fn set_targets(&mut self, targets: ScanTargets) {
        self.targets = targets;
    }

    /// The pass at Worker start (#46 D7): the snapshot is rebuilt from disk with no stability
    /// wait. It reports
    /// - every readable job file as [`ScanChange::JobAdded`] (took effect at its mtime, capped
    ///   at `now`), and every unreadable one as [`ScanChange::JobUnreadable`];
    /// - exactly one of [`ScanChange::SettingsChanged`] or [`ScanChange::SettingsUnreadable`];
    /// - [`ScanChange::DirUnavailable`] for each dir it can't list.
    ///
    /// The caller compares the jobs with the stored revisions. A stored job not named by any of
    /// these, and not held by an unavailable dir, is gone.
    pub fn startup_pass(&mut self, fs: &impl FsView, now: SystemTime) -> Vec<ScanChange> {
        self.files.clear();
        self.unavailable.clear();
        self.unlisted.clear();
        self.pass(fs, now, Mode::Startup)
    }

    /// The 2 s stat pass (#46 D1): lists the watched dirs and stats every candidate. Reads only
    /// files whose key has been stable for two passes.
    pub fn stat_pass(&mut self, fs: &impl FsView, now: SystemTime) -> Vec<ScanChange> {
        self.pass(fs, now, Mode::Stat)
    }

    /// The 30 s hash pass (#46 D1): a stat pass that also reads every file whose key is
    /// unchanged and compares SHA-256, and applies files unstable for [`UNSTABLE_AFTER`]. On a
    /// tick where both passes are due, run only this one. Also the immediate pass on a Resume.
    pub fn hash_pass(&mut self, fs: &impl FsView, now: SystemTime) -> Vec<ScanChange> {
        self.pass(fs, now, Mode::Hash)
    }

    /// Records the Daemon's own write of `bytes` to `path`, just after the rename (#46 D5
    /// step 6), so the next passes don't report it. Returns the key now on disk. If an outside
    /// write landed between the rename and this stat, the hash pass sees that the bytes differ.
    pub fn record_own_write(
        &mut self,
        fs: &impl FsView,
        path: &Path,
        bytes: &[u8],
    ) -> Result<StatKey, ScanError> {
        let target = self.classify(path).ok_or_else(|| ScanError::NotWatched {
            path: path.to_path_buf(),
        })?;
        let stat = fs.stat(path).map_err(|source| ScanError::Io {
            op: "stat",
            path: path.to_path_buf(),
            source,
        })?;
        let tracked = self
            .files
            .entry(path.to_path_buf())
            .or_insert_with(|| Tracked::new(target));
        tracked.known = true;
        tracked.applied = Some(Applied {
            key: stat.key,
            digest: sha256(bytes),
            read_at: self.last_pass,
        });
        tracked.state = State::Steady;
        tracked.unstable_warned = false;
        Ok(stat.key)
    }

    /// Drops `path` from the snapshot, when the Daemon deletes a job through the API (#46 D4,
    /// #12 §6), so the scan has nothing to orphan. Returns whether it was known.
    pub fn forget(&mut self, path: &Path) -> bool {
        self.files.remove(path).is_some()
    }

    fn pass(&mut self, fs: &impl FsView, now: SystemTime, mode: Mode) -> Vec<ScanChange> {
        let observed = self.observe(fs);
        let mut out = Out::default();
        let first_listed = self.dir_episodes(&observed, mode == Mode::Startup, &mut out);
        // Files nobody has been told about yet get the startup treatment: read at once, and
        // reported even when unreadable.
        let full_listing =
            |path: &Path| mode == Mode::Startup || first_listed.iter().any(|d| path.starts_with(d));
        let prev_pass = self.last_pass;

        let mut settings_reported = false;
        for (path, (target, stat)) in &observed.files {
            let tracked = self
                .files
                .entry(path.clone())
                .or_insert_with(|| Tracked::new(target.clone()));
            let ctx = PassCtx {
                fs,
                path,
                now,
                prev_pass,
                mode: if full_listing(path) {
                    Mode::Startup
                } else {
                    mode
                },
            };
            if let Some(change) = ctx.observe_present(tracked, stat) {
                settings_reported |= *target == Target::Settings;
                out.updates.push(change);
            }
        }
        for (path, (target, error)) in &observed.unreadable {
            if !full_listing(path) || self.files.contains_key(path) {
                continue;
            }
            let mut tracked = Tracked::new(target.clone());
            tracked.known = true;
            out.updates.push(tracked.unreadable(*error));
            settings_reported |= *target == Target::Settings;
            self.files.insert(path.clone(), tracked);
        }
        if mode == Mode::Startup && !settings_reported {
            // Absent: there is nothing to keep.
            out.updates
                .push(ScanChange::SettingsChanged { bytes: None });
        }

        let gone: Vec<PathBuf> = self
            .files
            .keys()
            .filter(|p| !observed.files.contains_key(*p) && !observed.is_blind(p))
            .cloned()
            .collect();
        for path in gone {
            let Some(tracked) = self.files.get_mut(&path) else {
                continue;
            };
            match (tracked.known, tracked.state) {
                (true, State::Missing) => {
                    if let Some(tracked) = self.files.remove(&path) {
                        out.removals.push(match tracked.target {
                            Target::Job(job) => ScanChange::JobRemoved { job },
                            Target::Settings => ScanChange::SettingsChanged { bytes: None },
                        });
                    }
                }
                (true, _) => tracked.state = State::Missing,
                (false, _) => {
                    self.files.remove(&path);
                }
            }
        }

        self.last_pass = Some(now);
        let mut changes = out.dirs;
        changes.append(&mut out.removals);
        changes.append(&mut out.updates);
        changes
    }

    /// Lists and stats every candidate (#46 D2). Never writes or creates anything.
    fn observe(&self, fs: &impl FsView) -> Observed {
        let mut obs = Observed::default();
        let t = &self.targets;

        let settings = t.layout.settings();
        match fs.stat(&settings) {
            Ok(stat) if stat.kind == FileKind::File => {
                obs.files.insert(settings, (Target::Settings, stat));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::debug!(errno = e.raw_os_error(), "can't stat settings.json");
                obs.unreadable
                    .insert(settings, (Target::Settings, e.kind()));
            }
        }

        match fs.list_dir(&t.jobs_dir) {
            Ok(names) => observe_jobs(fs, &t.jobs_dir, None, names, &mut obs),
            Err(e) if is_gone(&e) && !t.jobs_dir_explicit => {}
            Err(e) => unavailable(&mut obs, &t.jobs_dir, WatchedDir::Jobs, &e),
        }

        let agents_dir = t.layout.agents_dir();
        match fs.list_dir(&agents_dir) {
            Ok(mut agents) => {
                agents.sort();
                for agent in agents {
                    let Some(agent) = agent.to_str().filter(|a| !a.starts_with('.')) else {
                        continue;
                    };
                    let dir = t.layout.agent_jobs_dir(agent);
                    match fs.list_dir(&dir) {
                        Ok(names) => observe_jobs(fs, &dir, Some(agent), names, &mut obs),
                        Err(e) if is_gone(&e) => {}
                        Err(e) => {
                            let watched = WatchedDir::AgentJobs(agent.to_owned());
                            unavailable(&mut obs, &dir, watched, &e);
                        }
                    }
                }
            }
            Err(e) if is_gone(&e) => {}
            Err(e) => unavailable(&mut obs, &agents_dir, WatchedDir::Agents, &e),
        }
        obs
    }

    /// Opens and closes unavailable-dir episodes, and returns the dirs listed for the first time
    /// since startup. A dir hidden under an unreadable parent keeps its episode open.
    fn dir_episodes(&mut self, obs: &Observed, startup: bool, out: &mut Out) -> Vec<PathBuf> {
        let mut unavailable = BTreeMap::new();
        let mut unlisted = BTreeSet::new();
        for (path, (dir, error, errno)) in &obs.unavailable {
            if !self.unavailable.contains_key(path) {
                tracing::warn!(
                    event = log_event::SCAN_DIR_UNAVAILABLE,
                    dir_kind = dir.kind(),
                    errno = *errno,
                    "a watched job dir can't be read; its jobs are kept as they are"
                );
                out.dirs.push(ScanChange::DirUnavailable {
                    dir: dir.clone(),
                    path: path.clone(),
                    error: *error,
                });
            }
            if startup || self.unlisted.iter().any(|u| path.starts_with(u)) {
                unlisted.insert(path.clone());
            }
            unavailable.insert(path.clone(), dir.clone());
        }
        let mut first_listed = Vec::new();
        for path in &self.unlisted {
            if obs.is_blind(path) {
                unlisted.insert(path.clone());
            } else {
                first_listed.push(path.clone());
            }
        }
        for (path, dir) in &self.unavailable {
            if unavailable.contains_key(path) {
                continue;
            }
            if obs.is_blind(path) {
                unavailable.insert(path.clone(), dir.clone());
            } else {
                out.dirs.push(ScanChange::DirAvailable {
                    dir: dir.clone(),
                    path: path.clone(),
                    first_listing: first_listed.contains(path),
                });
            }
        }
        self.unavailable = unavailable;
        self.unlisted = unlisted;
        first_listed
    }

    /// What `path` is under the current targets, if the scan watches it. The agent is whatever
    /// [`Layout::agent_jobs_dir`] maps back to the file's dir.
    fn classify(&self, path: &Path) -> Option<Target> {
        let t = &self.targets;
        if path == t.layout.settings() {
            return Some(Target::Settings);
        }
        let label = job_label(path.file_name()?.to_str()?)?;
        let dir = path.parent()?;
        let agent = if dir == t.jobs_dir {
            None
        } else {
            let agent = dir.parent()?.file_name()?.to_str()?;
            if agent.starts_with('.') || t.layout.agent_jobs_dir(agent) != dir {
                return None;
            }
            Some(agent.to_owned())
        };
        Some(Target::Job(JobFile {
            agent,
            label: label.to_owned(),
            path: path.to_path_buf(),
        }))
    }
}

impl Tracked {
    fn new(target: Target) -> Self {
        Tracked {
            target,
            known: false,
            applied: None,
            state: State::Steady,
            unstable_warned: false,
        }
    }

    fn name(&self) -> String {
        match &self.target {
            Target::Job(job) => job.name(),
            Target::Settings => "settings.json".to_owned(),
        }
    }

    fn unreadable(&self, error: io::ErrorKind) -> ScanChange {
        tracing::debug!(job = %self.name(), ?error, "a watched file can't be read; kept as is");
        match &self.target {
            Target::Job(job) => ScanChange::JobUnreadable {
                job: job.clone(),
                error,
            },
            Target::Settings => ScanChange::SettingsUnreadable { error },
        }
    }

    /// Takes `bytes` with `key` into the snapshot. Returns the change, if the bytes differ from
    /// what was applied.
    fn commit(
        &mut self,
        key: StatKey,
        bytes: Vec<u8>,
        now: SystemTime,
        took_effect: SystemTime,
    ) -> Option<ScanChange> {
        let digest = sha256(&bytes);
        let before = self.applied.replace(Applied {
            key,
            digest,
            read_at: Some(now),
        });
        self.state = State::Steady;
        let added = !self.known;
        self.known = true;
        if before.is_some_and(|applied| applied.digest == digest) {
            return None;
        }
        let bytes = FileBytes::new(bytes);
        tracing::debug!(
            job = %self.name(),
            change = if added { "added" } else { "modified" },
            revision = %bytes.revision(),
            "a watched file changed"
        );
        Some(match &self.target {
            Target::Job(job) if added => ScanChange::JobAdded {
                job: job.clone(),
                bytes,
                took_effect,
            },
            Target::Job(job) => ScanChange::JobModified {
                job: job.clone(),
                bytes,
                took_effect,
            },
            Target::Settings => ScanChange::SettingsChanged { bytes: Some(bytes) },
        })
    }
}

/// One file's view of the pass it is in.
struct PassCtx<'a, F> {
    fs: &'a F,
    path: &'a Path,
    now: SystemTime,
    prev_pass: Option<SystemTime>,
    mode: Mode,
}

enum Read {
    Settled(Vec<u8>),
    Moved(FileStat),
    Failed,
}

impl<F: FsView> PassCtx<'_, F> {
    /// The stability rule for a file this pass found (#46 D3).
    fn observe_present(&self, tracked: &mut Tracked, stat: &FileStat) -> Option<ScanChange> {
        if self.mode == Mode::Startup {
            return match self.read() {
                Ok(bytes) => tracked.commit(stat.key, bytes, self.now, stat.modified.min(self.now)),
                Err(e) => {
                    tracked.known = true;
                    Some(tracked.unreadable(e.kind()))
                }
            };
        }

        if let Some(applied) = tracked.applied
            && applied.key == stat.key
        {
            // Unchanged, or back to the applied version after a brief absence or change.
            tracked.state = State::Steady;
            tracked.unstable_warned = false;
            if self.mode == Mode::Hash {
                return self.verify_bytes(tracked, stat);
            }
            return None;
        }

        match tracked.state {
            State::Pending(p) if p.key == stat.key => {
                tracked.unstable_warned = false;
                match self.read_settled(stat.key) {
                    Read::Settled(bytes) => {
                        let took_effect = clamp(stat.modified, p.prev_pass, p.first_seen);
                        tracked.commit(stat.key, bytes, self.now, took_effect)
                    }
                    Read::Moved(after) => {
                        tracked.state = State::Pending(self.moved(after, p.since));
                        None
                    }
                    Read::Failed => None,
                }
            }
            State::Pending(p) => {
                let unstable_for = self.now.duration_since(p.since).unwrap_or_default();
                if self.mode == Mode::Hash && unstable_for >= UNSTABLE_AFTER {
                    self.apply_unstable(tracked, stat, unstable_for)
                } else {
                    tracked.state = State::Pending(self.first_sight(stat, p.since));
                    None
                }
            }
            State::Steady | State::Missing => {
                tracked.state = State::Pending(self.first_sight(stat, self.now));
                None
            }
        }
    }

    /// The hash pass over a file whose key is unchanged: bytes that differ are applied at once,
    /// since the key has been the same on two passes (#46 D1).
    fn verify_bytes(&self, tracked: &mut Tracked, stat: &FileStat) -> Option<ScanChange> {
        let applied = tracked.applied.as_mut()?;
        match self.read_settled(stat.key) {
            Read::Settled(bytes) => {
                if sha256(&bytes) == applied.digest {
                    applied.read_at = Some(self.now);
                    return None;
                }
                let took_effect = clamp(stat.modified, applied.read_at, self.now);
                tracked.commit(stat.key, bytes, self.now, took_effect)
            }
            Read::Moved(after) => {
                tracked.state = State::Pending(self.moved(after, self.now));
                None
            }
            Read::Failed => None,
        }
    }

    /// A file that changed on every pass for [`UNSTABLE_AFTER`]: applied as read (#46 D3).
    fn apply_unstable(
        &self,
        tracked: &mut Tracked,
        stat: &FileStat,
        unstable_for: Duration,
    ) -> Option<ScanChange> {
        if !tracked.unstable_warned {
            tracked.unstable_warned = true;
            tracing::warn!(
                event = log_event::JOB_FILE_UNSTABLE,
                job = %tracked.name(),
                unstable_ms = unstable_for.as_millis() as u64,
                "a watched file kept changing; applying it as read"
            );
        }
        let Ok(bytes) = self.read() else {
            tracked.state = State::Pending(self.first_sight(stat, self.now));
            return None;
        };
        let took_effect = clamp(stat.modified, self.prev_pass, self.now);
        tracked.commit(stat.key, bytes, self.now, took_effect)
    }

    fn first_sight(&self, stat: &FileStat, since: SystemTime) -> Pending {
        Pending {
            key: stat.key,
            first_seen: self.now,
            prev_pass: self.prev_pass,
            since,
        }
    }

    /// The key moved during the read: this pass saw both versions.
    fn moved(&self, after: FileStat, since: SystemTime) -> Pending {
        Pending {
            key: after.key,
            first_seen: self.now,
            prev_pass: Some(self.now),
            since,
        }
    }

    /// Reads the file and re-stats it; the bytes count only if the key didn't move (#46 D3).
    fn read_settled(&self, key: StatKey) -> Read {
        let Ok(bytes) = self.read() else {
            return Read::Failed;
        };
        match self.fs.stat(self.path) {
            Ok(after) if after.key == key => Read::Settled(bytes),
            Ok(after) => Read::Moved(after),
            Err(_) => Read::Failed,
        }
    }

    /// Reads the file. A failure leaves its state for the next pass, which sees it again or sees
    /// it gone.
    fn read(&self) -> io::Result<Vec<u8>> {
        self.fs.read(self.path).inspect_err(|e| {
            tracing::debug!(errno = e.raw_os_error(), "can't read a watched file");
        })
    }
}

/// Stats the `.md` candidates in one job dir (#46 D2).
fn observe_jobs(
    fs: &impl FsView,
    dir: &Path,
    agent: Option<&str>,
    names: Vec<std::ffi::OsString>,
    obs: &mut Observed,
) {
    for name in names {
        let Some(label) = name.to_str().and_then(job_label) else {
            continue;
        };
        let path = dir.join(&name);
        let job = || {
            Target::Job(JobFile {
                agent: agent.map(str::to_owned),
                label: label.to_owned(),
                path: path.clone(),
            })
        };
        match fs.stat(&path) {
            Ok(stat) if stat.kind == FileKind::File => {
                obs.files.insert(path.clone(), (job(), stat));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                tracing::debug!(job = %label, "dangling symlink, or removed since the listing");
            }
            Err(e) => {
                tracing::debug!(job = %label, errno = e.raw_os_error(), "can't stat a job file");
                obs.unreadable.insert(path.clone(), (job(), e.kind()));
            }
        }
    }
}

/// The job label of a file name: a `.md` name that doesn't start with `.` (#46 D2; TS
/// `src/jobs.ts:117` takes dot-files too, a divergence).
fn job_label(name: &str) -> Option<&str> {
    if name.starts_with('.') {
        return None;
    }
    name.strip_suffix(".md").filter(|label| !label.is_empty())
}

/// A listing error that means "no dir here".
fn is_gone(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn unavailable(obs: &mut Observed, path: &Path, dir: WatchedDir, e: &io::Error) {
    obs.blind.push(path.to_path_buf());
    obs.unavailable
        .insert(path.to_path_buf(), (dir, e.kind(), e.raw_os_error()));
}

/// When a schedule took effect (#50): `modified`, clamped to `[lower, upper]`.
fn clamp(modified: SystemTime, lower: Option<SystemTime>, upper: SystemTime) -> SystemTime {
    let t = modified.min(upper);
    match lower {
        Some(lower) if t < lower => lower.min(upper),
        _ => t,
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
