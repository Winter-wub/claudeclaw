//! The daily Housekeeping pass (architecture §4 `core::housekeeping`; #44 D7).
//!
//! One [`Housekeeping`] per project dir and one Worker task drives it: 10 minutes after Worker
//! start, then every 24 h of wall clock, and after a Resume when a pass is overdue. After a
//! First-start import the first pass waits 24 h ([`Schedule`]). A pass deletes **only files the
//! Daemon wrote itself, under a rule listed here**, in this order (#44 D7):
//!
//! 1. Inbox attachments: older than `inbox.keepDays`, then down to `inbox.maxMiB` (D1);
//! 2. Run logs: opt-in `logging.runLogKeepDays`, keeping the newest per name;
//! 3. Run-index compaction, when `runLogKeepDays` is set (D4);
//! 4. Audit months: opt-in `logging.auditKeepMonths`;
//! 5. Session backups: keep the newest [`KEEP_BACKUPS`] by N (D8);
//! 6. Orphaned Scope state: a documented stub — it needs the sessions layer's Discord liveness
//!    decision (`Unknown Channel`) and deletes nothing here;
//! 7. `notices.json` entries unseen for 30 days, through the injected [`NoticePrune`] — absent
//!    when `core::notify` has no ledger to offer.
//!
//! A real pass logs `housekeeping.done` (INFO) and writes **one** audit entry per kind that
//! deleted something (`principal: system:housekeeping`, `target: <kind>`, a count); a dry run
//! neither logs nor audits. [`Report`] is the pass's shape, in the `housekeeping.run` terms
//! (#44 D7). Nothing outside these rules is ever touched: Transcripts, `outbox/`, `agents/`,
//! `import-backup/`, `daemon/tmp/` and TS-only files stay.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, TimeDelta, Utc};
use claudeclaw_api::log_event::daemon as event;
use claudeclaw_api::logs::{AuditEntry, AuditOutcome};
use claudeclaw_api::settings::Settings;
use claudeclaw_api::status::{HousekeepingReport, HousekeepingTotal};
use claudeclaw_store::StoreError;
use claudeclaw_store::audit::{AuditError, AuditLog};
use claudeclaw_store::layout::Layout;
use claudeclaw_store::run_index::{RunIndex, RunIndexError};
use claudeclaw_store::run_log::RunLogListing;
use claudeclaw_store::sessions::SessionsError;
use claudeclaw_store::sessions::ts::list_backups;

/// The first pass runs this long after Worker start (#44 D7).
pub const FIRST_PASS_DELAY: TimeDelta = TimeDelta::minutes(10);
/// Passes are this far apart on the wall clock (#44 D7).
pub const PASS_INTERVAL: TimeDelta = TimeDelta::hours(24);
/// The deferral after a First-start import, so the owner can react to the inbox preview
/// (#44 D1, D7).
pub const AFTER_IMPORT_DELAY: TimeDelta = TimeDelta::hours(24);
/// A file this young is never deleted, by the age or the cap rule (#44 D1).
pub const YOUNG_PROTECTION: TimeDelta = TimeDelta::hours(24);
/// How many `session_<N>.backup` files survive, newest by N (#44 D8).
pub const KEEP_BACKUPS: usize = 20;
/// `notices.json` entries unseen for this long are pruned (#44 D7, the last step).
pub const NOTICE_UNSEEN_WINDOW: TimeDelta = TimeDelta::days(30);
/// The audit entry's `principal` (#44 D7).
pub const PRINCIPAL: &str = "system:housekeeping";
/// The audit entry's `action` (#44 D7).
pub const ACTION: &str = "housekeeping.run";

/// The `kind` a pass reports and audits deletions under (#44 D7).
pub const KIND_INBOX: &str = "inbox";
/// See [`KIND_INBOX`].
pub const KIND_RUN_LOGS: &str = "run_logs";
/// See [`KIND_INBOX`]. `count` is the files deleted **or rewritten** by compaction, since a
/// rewrite also drops records.
pub const KIND_RUN_INDEX: &str = "run_index";
/// See [`KIND_INBOX`].
pub const KIND_AUDIT: &str = "audit";
/// See [`KIND_INBOX`].
pub const KIND_SESSION_BACKUPS: &str = "session_backups";
/// See [`KIND_INBOX`]. Always zero until the orphan stub is filled in.
pub const KIND_ORPHANS: &str = "orphans";
/// See [`KIND_INBOX`]. Always zero until the `notices.json` stub is filled in.
pub const KIND_NOTICES: &str = "notices";

/// A Housekeeping operation failed.
#[derive(Debug, thiserror::Error)]
pub enum HousekeepingError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    RunIndex(#[from] RunIndexError),
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error(transparent)]
    Sessions(#[from] SessionsError),
    #[error("notices.json: {0}")]
    Notices(#[source] io::Error),
}

/// Prunes unseen `notices.json` entries (#44 D7, the last step of a pass).
///
/// `core::notify` owns that file's format and code, so Housekeeping never reads or writes it
/// directly: it hands the cutoff to whatever implements this trait and records how many entries
/// went. A pass with no pruner configured reports zero, and a dry run must pass `dry_run`
/// through so the pruner deletes nothing.
pub trait NoticePrune: Send + Sync {
    /// Deletes every entry unseen since `older_than`, returning how many were (or, with
    /// `dry_run`, would be) removed.
    fn prune_unseen(&self, older_than: DateTime<Utc>, dry_run: bool) -> Result<u64, io::Error>;
}

/// The pass schedule (#44 D7): the next due time and how many real passes have run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    next: DateTime<Utc>,
    passes: u64,
}

impl Schedule {
    /// The schedule from a Worker start at `started_at`. `after_import` defers the first pass by
    /// [`AFTER_IMPORT_DELAY`] instead of [`FIRST_PASS_DELAY`].
    pub fn new(started_at: DateTime<Utc>, after_import: bool) -> Self {
        let delay = if after_import {
            AFTER_IMPORT_DELAY
        } else {
            FIRST_PASS_DELAY
        };
        Schedule {
            next: started_at + delay,
            passes: 0,
        }
    }

    /// When the next pass is due.
    pub fn next_at(&self) -> DateTime<Utc> {
        self.next
    }

    /// Whether a pass is due at `now` (also true while one is overdue after a Resume).
    pub fn due(&self, now: DateTime<Utc>) -> bool {
        now >= self.next
    }

    /// Records that a real pass ran at `now`: the next is [`PASS_INTERVAL`] later.
    pub fn done(&mut self, now: DateTime<Utc>) {
        self.next = now + PASS_INTERVAL;
        self.passes += 1;
    }

    /// How many real passes have run.
    pub fn passes(&self) -> u64 {
        self.passes
    }
}

/// The settings one pass reads (#44 D1, D7), taken from a [`Settings`]. `None` keeps a kind
/// forever or sets no cap, exactly as the settings file spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HousekeepingSettings {
    /// `inbox.keepDays`; `None` keeps attachments forever.
    pub inbox_keep_days: Option<u32>,
    /// `inbox.maxMiB`; `None` sets no cap.
    pub inbox_max_mib: Option<u32>,
    /// `logging.runLogKeepDays`; also gates Run-index compaction.
    pub run_log_keep_days: Option<u32>,
    /// `logging.auditKeepMonths`.
    pub audit_keep_months: Option<u32>,
}

impl From<&Settings> for HousekeepingSettings {
    fn from(settings: &Settings) -> Self {
        HousekeepingSettings {
            inbox_keep_days: settings.inbox.keep_days,
            inbox_max_mib: settings.inbox.max_mib,
            run_log_keep_days: settings.logging.run_log_keep_days,
            audit_keep_months: settings.logging.audit_keep_months,
        }
    }
}

/// What one pass did, or would do on a dry run (#44 D7). Deletions are grouped by kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Whether nothing was deleted.
    pub dry_run: bool,
    /// How long the pass took, in milliseconds.
    pub duration_ms: u64,
    /// Space freed, or that would be freed.
    pub freed_bytes: u64,
    /// Per-kind totals; only kinds that changed something appear.
    pub deleted: BTreeMap<String, HousekeepingTotal>,
}

impl Report {
    fn new(dry_run: bool) -> Self {
        Report {
            dry_run,
            duration_ms: 0,
            freed_bytes: 0,
            deleted: BTreeMap::new(),
        }
    }

    /// Adds one kind's contribution. The kind appears only when `count` is non-zero; `bytes` are
    /// always added to the pass total.
    fn add(&mut self, kind: &str, count: u32, bytes: u64) {
        self.freed_bytes = self.freed_bytes.saturating_add(bytes);
        if count == 0 {
            return;
        }
        let total = self
            .deleted
            .entry(kind.to_owned())
            .or_insert(HousekeepingTotal { count: 0, bytes: 0 });
        total.count = total.count.saturating_add(count);
        total.bytes = total.bytes.saturating_add(bytes);
    }

    /// The report in the `housekeeping.run` shape: a dry run sets `would_delete`, a real pass
    /// `deleted`.
    pub fn to_api(&self) -> HousekeepingReport {
        let totals = self.deleted.clone();
        if self.dry_run {
            HousekeepingReport {
                would_delete: Some(totals),
                deleted: None,
                freed_bytes: self.freed_bytes,
            }
        } else {
            HousekeepingReport {
                would_delete: None,
                deleted: Some(totals),
                freed_bytes: self.freed_bytes,
            }
        }
    }
}

/// The Housekeeping task for one project dir (#44 D7).
#[derive(Debug)]
pub struct Housekeeping {
    layout: Layout,
    schedule: Schedule,
    /// Whether the inbox was already over its cap with only protected files left, so
    /// `inbox.over_cap` warns once per episode.
    over_cap: Mutex<bool>,
}

impl Housekeeping {
    /// A Housekeeping for `layout` with a pass `schedule`.
    pub fn new(layout: Layout, schedule: Schedule) -> Self {
        Housekeeping {
            layout,
            schedule,
            over_cap: Mutex::new(false),
        }
    }

    /// The pass schedule.
    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// Whether a pass is due at `now`.
    pub fn due(&self, now: DateTime<Utc>) -> bool {
        self.schedule.due(now)
    }

    /// Runs one pass now (#44 D7). `protected(path)` names a file that must not be deleted (for
    /// example, one attached to a queued or running Turn). `notices`, when present, prunes
    /// `notices.json` as the pass's last step through [`NoticePrune`]. With `dry_run`, nothing is
    /// deleted, nothing is logged and nothing is audited, but the returned [`Report`] is what a
    /// real pass would do.
    pub fn pass(
        &self,
        now: DateTime<Utc>,
        settings: &HousekeepingSettings,
        protected: &dyn Fn(&Path) -> bool,
        notices: Option<Arc<dyn NoticePrune>>,
        dry_run: bool,
    ) -> Result<Report, HousekeepingError> {
        let started = Instant::now();
        let mut report = Report::new(dry_run);
        self.prune_inbox(now, settings, protected, dry_run, &mut report)?;
        self.prune_run_logs(now, settings, dry_run, &mut report)?;
        self.compact_run_index(now, settings, dry_run, &mut report)?;
        self.prune_audit(now, settings, dry_run, &mut report)?;
        self.prune_session_backups(dry_run, &mut report)?;
        // Stub: the orphan prune needs the sessions layer's Discord liveness decision (#44 D8).
        // It deletes nothing today.
        if let Some(notices) = notices {
            let count = notices
                .prune_unseen(now - NOTICE_UNSEEN_WINDOW, dry_run)
                .map_err(HousekeepingError::Notices)?;
            report.add(KIND_NOTICES, u32::try_from(count).unwrap_or(u32::MAX), 0);
        }
        report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if !dry_run {
            log_done(&report);
            self.write_audit(now, &report)?;
        }
        Ok(report)
    }

    /// Runs a pass only when one is due, advancing the schedule on a real pass. A dry run
    /// neither advances the schedule nor audits (#44 D7).
    pub fn run_due(
        &mut self,
        now: DateTime<Utc>,
        settings: &HousekeepingSettings,
        protected: &dyn Fn(&Path) -> bool,
        notices: Option<Arc<dyn NoticePrune>>,
        dry_run: bool,
    ) -> Result<Option<Report>, HousekeepingError> {
        if !self.schedule.due(now) {
            return Ok(None);
        }
        let report = self.pass(now, settings, protected, notices, dry_run)?;
        if !dry_run {
            self.schedule.done(now);
        }
        Ok(Some(report))
    }

    /// Inbox attachments (#44 D1): first everything older than `inbox.keepDays`, then down to
    /// `inbox.maxMiB`, oldest first. A file younger than [`YOUNG_PROTECTION`] or named by
    /// `protected` is never deleted. Files not matching a Daemon pattern are left alone.
    fn prune_inbox(
        &self,
        now: DateTime<Utc>,
        settings: &HousekeepingSettings,
        protected: &dyn Fn(&Path) -> bool,
        dry_run: bool,
        report: &mut Report,
    ) -> Result<(), HousekeepingError> {
        let mut files = self.inbox_files()?;
        files.sort_by(|a, b| {
            a.downloaded_at
                .cmp(&b.downloaded_at)
                .then_with(|| a.path.cmp(&b.path))
        });

        let mut remaining = Vec::with_capacity(files.len());
        let mut age_count = 0u32;
        let mut age_bytes = 0u64;
        if let Some(keep_days) = settings.inbox_keep_days {
            let cutoff = now - TimeDelta::days(i64::from(keep_days));
            for file in files {
                let deletable = file.downloaded_at < cutoff
                    && !young(now, file.downloaded_at)
                    && !protected(&file.path);
                if deletable && let Some(size) = file_size(&file.path)? {
                    if !dry_run {
                        remove(&file.path)?;
                    }
                    age_count += 1;
                    age_bytes = age_bytes.saturating_add(size);
                    continue;
                }
                remaining.push(file);
            }
        } else {
            remaining = files;
        }
        if age_count > 0 {
            tracing::info!(
                event = event::INBOX_PRUNED,
                count = age_count,
                bytes = age_bytes,
                reason = "age",
                "inbox attachments older than the window were pruned"
            );
            report.add(KIND_INBOX, age_count, age_bytes);
        }

        if let Some(max_mib) = settings.inbox_max_mib {
            let cap = u64::from(max_mib).saturating_mul(MIB);
            let mut total = 0u64;
            for file in &remaining {
                if let Some(size) = file_size(&file.path)? {
                    total = total.saturating_add(size);
                }
            }
            let mut cap_count = 0u32;
            let mut cap_bytes = 0u64;
            for file in &remaining {
                if total <= cap {
                    break;
                }
                if young(now, file.downloaded_at) || protected(&file.path) {
                    continue;
                }
                let Some(size) = file_size(&file.path)? else {
                    continue;
                };
                if !dry_run {
                    remove(&file.path)?;
                }
                total = total.saturating_sub(size);
                cap_count += 1;
                cap_bytes = cap_bytes.saturating_add(size);
            }
            if cap_count > 0 {
                tracing::info!(
                    event = event::INBOX_PRUNED,
                    count = cap_count,
                    bytes = cap_bytes,
                    reason = "cap",
                    "inbox attachments over the cap were pruned"
                );
                report.add(KIND_INBOX, cap_count, cap_bytes);
            }
            let mut over = self.over_cap.lock().unwrap_or_else(|e| e.into_inner());
            if total > cap {
                if !*over {
                    tracing::warn!(
                        event = event::INBOX_OVER_CAP,
                        "the inbox is over its cap with only protected files left"
                    );
                    *over = true;
                }
            } else {
                *over = false;
            }
        }
        Ok(())
    }

    /// Run logs (#44 D7): opt-in `logging.runLogKeepDays` deletes logs older than the window,
    /// but always keeps the newest log per run name.
    fn prune_run_logs(
        &self,
        now: DateTime<Utc>,
        settings: &HousekeepingSettings,
        dry_run: bool,
        report: &mut Report,
    ) -> Result<(), HousekeepingError> {
        let Some(keep_days) = settings.run_log_keep_days else {
            return Ok(());
        };
        let cutoff = now - TimeDelta::days(i64::from(keep_days));
        let listing = RunLogListing::scan(&self.layout)?;
        let newest = listing.newest_per_name();
        let mut count = 0u32;
        let mut bytes = 0u64;
        for file in &listing.files {
            if file.started_at >= cutoff
                || newest
                    .get(file.name.as_str())
                    .is_some_and(|newest| newest.rel == file.rel)
            {
                continue;
            }
            let Some(size) = file_size(&file.path)? else {
                continue;
            };
            if !dry_run {
                remove(&file.path)?;
            }
            count += 1;
            bytes = bytes.saturating_add(size);
        }
        if count > 0 {
            report.add(KIND_RUN_LOGS, count, bytes);
        }
        Ok(())
    }

    /// Run-index compaction (#44 D4, D7): only when `logging.runLogKeepDays` is set, so the index
    /// ages with the Run logs it references.
    fn compact_run_index(
        &self,
        now: DateTime<Utc>,
        settings: &HousekeepingSettings,
        dry_run: bool,
        report: &mut Report,
    ) -> Result<(), HousekeepingError> {
        let Some(keep_days) = settings.run_log_keep_days else {
            return Ok(());
        };
        let compacted = RunIndex::new(self.layout.clone()).compact(keep_days, now, dry_run)?;
        let files = compacted
            .files_deleted
            .saturating_add(compacted.files_rewritten);
        if files > 0 {
            report.add(KIND_RUN_INDEX, files, compacted.freed_bytes);
        }
        Ok(())
    }

    /// The audit log (#44 D7): opt-in `logging.auditKeepMonths` drops whole older month files.
    fn prune_audit(
        &self,
        now: DateTime<Utc>,
        settings: &HousekeepingSettings,
        dry_run: bool,
        report: &mut Report,
    ) -> Result<(), HousekeepingError> {
        let Some(months) = settings.audit_keep_months else {
            return Ok(());
        };
        let pruned = AuditLog::new(self.layout.clone()).prune(months, now, dry_run)?;
        if pruned.files_deleted > 0 {
            report.add(KIND_AUDIT, pruned.files_deleted, pruned.freed_bytes);
        }
        Ok(())
    }

    /// Session backups (#44 D8): keep the newest [`KEEP_BACKUPS`] by N, delete the rest.
    fn prune_session_backups(
        &self,
        dry_run: bool,
        report: &mut Report,
    ) -> Result<(), HousekeepingError> {
        let backups = list_backups(&self.layout)?;
        let mut count = 0u32;
        let mut bytes = 0u64;
        for backup in backups.iter().skip(KEEP_BACKUPS) {
            let Some(size) = file_size(&backup.path)? else {
                continue;
            };
            if !dry_run {
                remove(&backup.path)?;
            }
            count += 1;
            bytes = bytes.saturating_add(size);
        }
        if count > 0 {
            report.add(KIND_SESSION_BACKUPS, count, bytes);
        }
        Ok(())
    }

    /// One audit entry per kind that deleted something (#44 D7), never one per file.
    fn write_audit(&self, now: DateTime<Utc>, report: &Report) -> Result<(), HousekeepingError> {
        let log = AuditLog::new(self.layout.clone());
        for (seq, (kind, total)) in report.deleted.iter().enumerate() {
            let entry = AuditEntry {
                id: audit_id(now, u32::try_from(seq).unwrap_or(u32::MAX)),
                at: now,
                principal: PRINCIPAL.to_owned(),
                action: ACTION.to_owned(),
                target: Some(kind.clone()),
                outcome: AuditOutcome::Ok,
                code: None,
                count: Some(total.count),
            };
            log.append(&entry)?;
        }
        Ok(())
    }

    /// Every Daemon-written file under `inbox/*` with the download time its name carries (#44
    /// D1). Foreign files are left out.
    fn inbox_files(&self) -> Result<Vec<InboxFile>, HousekeepingError> {
        let mut files = Vec::new();
        for dir in [self.layout.discord_inbox(), self.layout.api_inbox()] {
            for entry in read_dir(&dir)? {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                let Some(downloaded_at) = downloaded_at(&name) else {
                    continue;
                };
                let file_type = entry
                    .file_type()
                    .map_err(|e| store_io("stat", &entry.path(), e))?;
                if !file_type.is_file() {
                    continue;
                }
                files.push(InboxFile {
                    path: entry.path(),
                    downloaded_at,
                });
            }
        }
        Ok(files)
    }
}

/// One Daemon-written inbox file and the download time its name carries.
#[derive(Debug)]
struct InboxFile {
    path: PathBuf,
    downloaded_at: DateTime<Utc>,
}

/// The `deleted` counts for the Daemon log (#44 D7).
fn log_done(report: &Report) {
    let deleted: BTreeMap<&str, u32> = report
        .deleted
        .iter()
        .map(|(kind, total)| (kind.as_str(), total.count))
        .collect();
    tracing::info!(
        event = event::HOUSEKEEPING_DONE,
        deleted = ?deleted,
        freed_bytes = report.freed_bytes,
        duration_ms = report.duration_ms,
        "housekeeping pass done"
    );
}

/// An audit id shaped like a v7 UUID from the pass time and the kind's sequence (#44 D7). The
/// audit cursor is `month:line`, so nothing parses this; it only has to be a distinct, opaque id.
fn audit_id(now: DateTime<Utc>, seq: u32) -> String {
    let millis = now.timestamp_millis().max(0) as u128;
    let time_low = (millis >> 16) & 0xffff_ffff;
    let time_mid = millis & 0xffff;
    let tail = ((u128::from(seq) << 32) ^ (millis & 0xffff_ffff)) & 0xffff_ffff_ffff;
    format!("{time_low:08x}-{time_mid:04x}-7000-8000-{tail:012x}")
}

/// An OS operation that failed, as a [`StoreError::Io`].
fn store_io(op: &'static str, path: &Path, source: io::Error) -> HousekeepingError {
    StoreError::Io {
        op,
        path: path.to_path_buf(),
        source,
    }
    .into()
}

/// `dir`'s entries, or none when it doesn't exist.
fn read_dir(dir: &Path) -> Result<Vec<fs::DirEntry>, HousekeepingError> {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .collect::<Result<_, _>>()
            .map_err(|e| store_io("list", dir, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(store_io("list", dir, e)),
    }
}

/// A file's size, or `None` when it vanished since it was listed.
fn file_size(path: &Path) -> Result<Option<u64>, HousekeepingError> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(meta.len())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(store_io("stat", path, e)),
    }
}

/// Removes a file, treating a file that is already gone as success.
fn remove(path: &Path) -> Result<(), HousekeepingError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(store_io("delete", path, e)),
    }
}

/// Whether a file downloaded at `downloaded_at` is too young to delete (#44 D1). A future time
/// counts as young, so a bad clock can never delete.
fn young(now: DateTime<Utc>, downloaded_at: DateTime<Utc>) -> bool {
    now - downloaded_at < YOUNG_PROTECTION
}

/// The download time a Daemon-written inbox file name carries, or `None` for a foreign name
/// (#44 D1). The age is the name's, never the mtime.
fn downloaded_at(name: &str) -> Option<DateTime<Utc>> {
    let millis = discord_ms(name).or_else(|| api_ms(name))?;
    DateTime::from_timestamp_millis(millis)
}

/// The epoch ms in a Discord attachment name, `^\d{17,20}-\d{13}(\.[A-Za-z0-9]{1,10})?$` (#44 D1).
fn discord_ms(name: &str) -> Option<i64> {
    let stem = strip_extension(name)?;
    let (id, millis) = stem.split_once('-')?;
    if !(17..=20).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    (millis.len() == 13 && millis.bytes().all(|b| b.is_ascii_digit()))
        .then(|| millis.parse().ok())
        .flatten()
}

/// The UUID v7 download time in an API attachment name, `<turn_id>-<index><ext>` (#44 D1).
fn api_ms(name: &str) -> Option<i64> {
    let (turn_id, rest) = name.rsplit_once('-')?;
    let index = match rest.split_once('.') {
        Some((_, ext)) if !valid_extension(ext) => return None,
        Some((index, _)) => index,
        None => rest,
    };
    if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    uuid_v7_ms(turn_id)
}

/// `name` without a `.<1..=10 alphanumerics>` extension, or `None` when it has a bad one.
fn strip_extension(name: &str) -> Option<&str> {
    match name.split_once('.') {
        Some((stem, ext)) if valid_extension(ext) => Some(stem),
        Some(_) => None,
        None => Some(name),
    }
}

/// Whether `ext` is a `1..=10`-character ASCII-alphanumeric extension.
fn valid_extension(ext: &str) -> bool {
    !ext.is_empty() && ext.len() <= 10 && ext.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The Unix-ms time in a v7 UUID's first 12 hex digits, or `None` when `id` isn't a v7 UUID.
fn uuid_v7_ms(id: &str) -> Option<i64> {
    let groups: Vec<&str> = id.split('-').collect();
    if groups.len() != 5 {
        return None;
    }
    for (group, len) in groups.iter().zip([8usize, 4, 4, 4, 12]) {
        if group.len() != len || !group.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
    }
    if !groups[2].starts_with('7') {
        return None;
    }
    if !matches!(
        groups[3].as_bytes().first(),
        Some(b'8' | b'9' | b'a' | b'b' | b'A' | b'B')
    ) {
        return None;
    }
    let millis = format!("{}{}", &groups[0], &groups[1]);
    i64::from_str_radix(&millis, 16).ok()
}

/// One MiB, for `inbox.maxMiB`.
const MIB: u64 = 1024 * 1024;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use claudeclaw_store::run_log::RunLog;

    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn ms(s: &str) -> i64 {
        at(s).timestamp_millis()
    }

    fn write_file(path: &Path, bytes: usize) {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    /// A Discord name, built at run time: never a literal 17-digit token in the source.
    fn discord_name(when: &str, ext: Option<&str>) -> String {
        let id = "1".repeat(18);
        let name = format!("{id}-{:013}", ms(when));
        match ext {
            Some(ext) => format!("{name}.{ext}"),
            None => name,
        }
    }

    /// An API name with a v7 `turn_id`, built at run time.
    fn api_name(when: &str, index: u32, ext: Option<&str>) -> String {
        let hex = format!("{:012x}", ms(when));
        let turn = format!("{}-{}-7000-8000-000000000000", &hex[..8], &hex[8..12]);
        let name = format!("{turn}-{index}");
        match ext {
            Some(ext) => format!("{name}.{ext}"),
            None => name,
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        layout: Layout,
    }

    fn setup() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        fs::create_dir_all(layout.root()).unwrap();
        Fixture { _dir: dir, layout }
    }

    fn settings(
        inbox_keep_days: Option<u32>,
        inbox_max_mib: Option<u32>,
        run_log_keep_days: Option<u32>,
        audit_keep_months: Option<u32>,
    ) -> HousekeepingSettings {
        HousekeepingSettings {
            inbox_keep_days,
            inbox_max_mib,
            run_log_keep_days,
            audit_keep_months,
        }
    }

    fn none() -> HousekeepingSettings {
        settings(None, None, None, None)
    }

    fn housekeeping(layout: &Layout) -> Housekeeping {
        Housekeeping::new(
            layout.clone(),
            Schedule::new(at("2026-10-15T00:00:00Z"), false),
        )
    }

    fn totals(report: &Report, kind: &str) -> Option<HousekeepingTotal> {
        report.deleted.get(kind).copied()
    }

    /// A [`NoticePrune`] that records its calls and reports a fixed count.
    struct FakeNotices {
        calls: Mutex<Vec<(DateTime<Utc>, bool)>>,
        count: u64,
    }

    impl NoticePrune for FakeNotices {
        fn prune_unseen(&self, older_than: DateTime<Utc>, dry_run: bool) -> Result<u64, io::Error> {
            self.calls.lock().unwrap().push((older_than, dry_run));
            Ok(self.count)
        }
    }

    #[test]
    fn a_dry_run_deletes_nothing_and_reports_what_a_pass_would() {
        let fixture = setup();
        let now = at("2026-10-15T00:00:00Z");
        let old_discord = fixture
            .layout
            .discord_inbox()
            .join(discord_name("2026-08-01T00:00:00Z", Some("png")));
        let old_api =
            fixture
                .layout
                .api_inbox()
                .join(api_name("2026-08-02T00:00:00Z", 1, Some("jpg")));
        write_file(&old_discord, 100);
        write_file(&old_api, 200);
        let hk = housekeeping(&fixture.layout);
        let keep = settings(Some(30), None, None, None);

        let dry = hk.pass(now, &keep, &|_| false, None, true).unwrap();
        assert!(dry.dry_run);
        assert!(
            old_discord.is_file() && old_api.is_file(),
            "a dry run deletes nothing"
        );
        assert_eq!(
            totals(&dry, KIND_INBOX),
            Some(HousekeepingTotal {
                count: 2,
                bytes: 300
            })
        );
        assert_eq!(dry.freed_bytes, 300);
        let api = dry.to_api();
        assert!(api.would_delete.is_some() && api.deleted.is_none());

        let real = hk.pass(now, &keep, &|_| false, None, false).unwrap();
        assert!(!old_discord.exists() && !old_api.exists());
        assert_eq!(totals(&real, KIND_INBOX), totals(&dry, KIND_INBOX));
        let api = real.to_api();
        assert!(api.would_delete.is_none() && api.deleted.is_some());
    }

    #[test]
    fn the_age_rule_keeps_young_protected_and_foreign_files() {
        let fixture = setup();
        let now = at("2026-10-15T00:00:00Z");
        let inbox = fixture.layout.discord_inbox();
        let old = inbox.join(discord_name("2026-08-01T00:00:00Z", Some("png")));
        let young = inbox.join(discord_name("2026-10-14T12:00:00Z", Some("png")));
        let protected = inbox.join(discord_name("2026-08-03T00:00:00Z", Some("pdf")));
        let foreign = inbox.join("notes.txt");
        for path in [&old, &young, &protected, &foreign] {
            write_file(path, 10);
        }
        let guard = protected.clone();
        let is_protected = move |path: &Path| path == guard.as_path();
        let hk = housekeeping(&fixture.layout);
        let report = hk
            .pass(
                now,
                &settings(Some(30), None, None, None),
                &is_protected,
                None,
                false,
            )
            .unwrap();

        assert!(!old.exists(), "an old Daemon file is deleted");
        assert!(young.is_file(), "a file younger than 24 h is protected");
        assert!(protected.is_file(), "a Turn-protected file is kept");
        assert!(foreign.is_file(), "a foreign file is never deleted");
        assert_eq!(
            totals(&report, KIND_INBOX),
            Some(HousekeepingTotal {
                count: 1,
                bytes: 10
            })
        );
    }

    #[test]
    fn the_cap_deletes_the_oldest_first() {
        let fixture = setup();
        let now = at("2026-10-15T00:00:00Z");
        let inbox = fixture.layout.discord_inbox();
        let oldest = inbox.join(discord_name("2026-09-01T00:00:00Z", Some("png")));
        let middle = inbox.join(discord_name("2026-09-20T00:00:00Z", Some("png")));
        let newest = inbox.join(discord_name("2026-10-01T00:00:00Z", Some("png")));
        let half = (MIB / 2) as usize;
        for path in [&oldest, &middle, &newest] {
            write_file(path, half);
        }
        let hk = housekeeping(&fixture.layout);
        let report = hk
            .pass(
                now,
                &settings(None, Some(1), None, None),
                &|_| false,
                None,
                false,
            )
            .unwrap();

        assert!(!oldest.exists(), "the oldest goes first");
        assert!(middle.is_file() && newest.is_file());
        assert_eq!(
            totals(&report, KIND_INBOX),
            Some(HousekeepingTotal {
                count: 1,
                bytes: MIB / 2
            })
        );
    }

    #[test]
    fn only_protected_files_over_cap_are_kept_and_end_the_episode() {
        let fixture = setup();
        let now = at("2026-10-15T00:00:00Z");
        let inbox = fixture.layout.discord_inbox();
        let first = inbox.join(discord_name("2026-10-14T12:00:00Z", Some("png")));
        let second = inbox.join(discord_name("2026-10-14T13:00:00Z", Some("png")));
        write_file(&first, (MIB as usize) * 3 / 5);
        write_file(&second, (MIB as usize) * 3 / 5);
        let hk = housekeeping(&fixture.layout);
        let cap = settings(None, Some(1), None, None);

        let report = hk.pass(now, &cap, &|_| false, None, false).unwrap();
        assert!(
            first.is_file() && second.is_file(),
            "young files are protected"
        );
        assert_eq!(totals(&report, KIND_INBOX), None);
        assert!(*hk.over_cap.lock().unwrap(), "the episode is open");

        // The same episode doesn't reopen.
        hk.pass(now, &cap, &|_| false, None, false).unwrap();
        assert!(*hk.over_cap.lock().unwrap());

        // Deleting one closes it.
        remove(&first).unwrap();
        hk.pass(now, &cap, &|_| false, None, false).unwrap();
        assert!(
            !*hk.over_cap.lock().unwrap(),
            "the episode ends under the cap"
        );
    }

    #[test]
    fn session_backups_keep_the_newest_twenty() {
        let fixture = setup();
        let hk = housekeeping(&fixture.layout);
        for n in 1..=25 {
            write_file(&fixture.layout.session_backup(n), 10);
        }
        let dry = hk
            .pass(at("2026-10-15T00:00:00Z"), &none(), &|_| false, None, true)
            .unwrap();
        assert_eq!(
            totals(&dry, KIND_SESSION_BACKUPS),
            Some(HousekeepingTotal {
                count: 5,
                bytes: 50
            })
        );
        assert!(
            fixture.layout.session_backup(1).is_file(),
            "a dry run deletes nothing"
        );

        let real = hk
            .pass(at("2026-10-15T00:00:00Z"), &none(), &|_| false, None, false)
            .unwrap();
        assert_eq!(
            totals(&real, KIND_SESSION_BACKUPS),
            totals(&dry, KIND_SESSION_BACKUPS)
        );
        for n in 1..=5 {
            assert!(
                !fixture.layout.session_backup(n).exists(),
                "backup {n} is removed"
            );
        }
        for n in 6..=25 {
            assert!(
                fixture.layout.session_backup(n).is_file(),
                "backup {n} is kept"
            );
        }
    }

    fn write_run_log(layout: &Layout, name: &str, start: &str) {
        let log = RunLog::create(layout, name, at(start)).unwrap();
        write_file(log.path(), 10);
    }

    #[test]
    fn run_logs_are_pruned_only_when_opted_in_keeping_the_newest_per_name() {
        let fixture = setup();
        let now = at("2026-10-15T00:00:00Z");
        write_run_log(&fixture.layout, "daily", "2026-06-01T00:00:00Z");
        write_run_log(&fixture.layout, "daily", "2026-10-01T00:00:00Z");
        write_run_log(&fixture.layout, "weekly", "2026-05-01T00:00:00Z");
        let hk = housekeeping(&fixture.layout);

        let off = hk.pass(now, &none(), &|_| false, None, false).unwrap();
        assert_eq!(totals(&off, KIND_RUN_LOGS), None, "kept forever by default");
        assert_eq!(RunLogListing::scan(&fixture.layout).unwrap().files.len(), 3);

        let report = hk
            .pass(
                now,
                &settings(None, None, Some(30), None),
                &|_| false,
                None,
                false,
            )
            .unwrap();
        assert_eq!(
            totals(&report, KIND_RUN_LOGS),
            Some(HousekeepingTotal {
                count: 1,
                bytes: 10
            })
        );
        let mut remaining: Vec<_> = RunLogListing::scan(&fixture.layout)
            .unwrap()
            .files
            .into_iter()
            .map(|file| (file.name, file.started_at))
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            [
                ("daily".to_owned(), at("2026-10-01T00:00:00Z")),
                ("weekly".to_owned(), at("2026-05-01T00:00:00Z")),
            ],
            "the newest per name survives"
        );
    }

    #[test]
    fn the_run_index_is_compacted_only_when_run_logs_age() {
        let fixture = setup();
        let now = at("2026-10-15T00:00:00Z");
        fs::create_dir_all(fixture.layout.run_index_dir()).unwrap();
        let month = fixture.layout.run_index("2026-08");
        fs::write(
            &month,
            "{\"run_log\":\"logs/gone-2026-08-01T00-00-00-000Z.log\"}\n",
        )
        .unwrap();
        let hk = housekeeping(&fixture.layout);

        let off = hk.pass(now, &none(), &|_| false, None, false).unwrap();
        assert_eq!(totals(&off, KIND_RUN_INDEX), None);
        assert!(month.is_file(), "no keep-days keeps the index forever");

        let report = hk
            .pass(
                now,
                &settings(None, None, Some(30), None),
                &|_| false,
                None,
                false,
            )
            .unwrap();
        assert_eq!(
            totals(&report, KIND_RUN_INDEX),
            Some(HousekeepingTotal {
                count: 1,
                bytes: 53
            })
        );
        assert!(!month.exists(), "a month with no live Run log is deleted");
    }

    fn audit_entry(id: &str, when: &str) -> AuditEntry {
        AuditEntry {
            id: id.into(),
            at: at(when),
            principal: "device:1".into(),
            action: "jobs.update".into(),
            target: None,
            outcome: AuditOutcome::Ok,
            code: None,
            count: None,
        }
    }

    #[test]
    fn the_audit_log_is_pruned_only_when_opted_in() {
        let fixture = setup();
        let now = at("2026-10-15T00:00:00Z");
        let log = AuditLog::new(fixture.layout.clone());
        log.append(&audit_entry("a", "2026-06-15T00:00:00Z"))
            .unwrap();
        log.append(&audit_entry("b", "2026-10-01T00:00:00Z"))
            .unwrap();
        let hk = housekeeping(&fixture.layout);

        let off = hk.pass(now, &none(), &|_| false, None, false).unwrap();
        assert_eq!(totals(&off, KIND_AUDIT), None);
        assert!(fixture.layout.audit_log("2026-06").is_file());

        let report = hk
            .pass(
                now,
                &settings(None, None, None, Some(3)),
                &|_| false,
                None,
                false,
            )
            .unwrap();
        assert!(matches!(totals(&report, KIND_AUDIT), Some(total) if total.count == 1));
        assert!(!fixture.layout.audit_log("2026-06").exists());
        assert!(fixture.layout.audit_log("2026-10").is_file());
    }

    #[test]
    fn a_real_pass_writes_one_audit_entry_per_kind_and_a_dry_run_writes_none() {
        let fixture = setup();
        let now = at("2026-10-15T00:00:00Z");
        for n in [1, 2] {
            write_file(
                &fixture.layout.discord_inbox().join(discord_name(
                    &format!("2026-08-0{n}T00:00:00Z"),
                    Some("png"),
                )),
                10,
            );
        }
        for n in 1..=25 {
            write_file(&fixture.layout.session_backup(n), 10);
        }
        let hk = housekeeping(&fixture.layout);
        let keep = settings(Some(30), None, None, None);

        hk.pass(now, &keep, &|_| false, None, true).unwrap();
        let log = AuditLog::new(fixture.layout.clone());
        assert!(
            log.page(None, 100, |_| true).unwrap().entries.is_empty(),
            "a dry run is not audited"
        );

        hk.pass(now, &keep, &|_| false, None, false).unwrap();
        let entries = log.page(None, 100, |_| true).unwrap().entries;
        let mut got: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.action.as_str(),
                    entry.principal.as_str(),
                    entry.target.as_deref(),
                    entry.count,
                )
            })
            .collect();
        got.reverse(); // `page` is newest first; compare in append order
        assert_eq!(
            got,
            [
                (ACTION, PRINCIPAL, Some(KIND_INBOX), Some(2)),
                (ACTION, PRINCIPAL, Some(KIND_SESSION_BACKUPS), Some(5)),
            ]
        );
    }

    #[test]
    fn notices_are_pruned_by_the_injected_pruner_with_a_thirty_day_cutoff() {
        let fixture = setup();
        let now = at("2026-10-15T00:00:00Z");
        let hk = housekeeping(&fixture.layout);

        // No pruner means nothing to do and nothing to report.
        let off = hk.pass(now, &none(), &|_| false, None, false).unwrap();
        assert_eq!(totals(&off, KIND_NOTICES), None);

        let fake = Arc::new(FakeNotices {
            calls: Mutex::new(Vec::new()),
            count: 4,
        });
        let dry = hk
            .pass(now, &none(), &|_| false, Some(fake.clone()), true)
            .unwrap();
        let real = hk
            .pass(now, &none(), &|_| false, Some(fake.clone()), false)
            .unwrap();

        let total = Some(HousekeepingTotal { count: 4, bytes: 0 });
        assert_eq!(totals(&dry, KIND_NOTICES), total);
        assert_eq!(totals(&real, KIND_NOTICES), total);

        let calls = fake.calls.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            2,
            "the pruner runs on a dry run and a real pass"
        );
        assert_eq!(
            calls[0].0,
            now - NOTICE_UNSEEN_WINDOW,
            "the cutoff is thirty days back"
        );
        assert!(calls[0].1, "a dry run is passed through");
        assert!(!calls[1].1);
    }

    #[test]
    fn the_schedule_waits_ten_minutes_then_a_day() {
        let start = at("2026-10-15T00:00:00Z");
        let mut schedule = Schedule::new(start, false);
        assert_eq!(schedule.next_at(), start + FIRST_PASS_DELAY);
        assert!(!schedule.due(start));
        assert!(schedule.due(start + FIRST_PASS_DELAY));
        schedule.done(start + FIRST_PASS_DELAY);
        assert_eq!(schedule.next_at(), start + FIRST_PASS_DELAY + PASS_INTERVAL);
        assert_eq!(schedule.passes(), 1);

        let after_import = Schedule::new(start, true);
        assert_eq!(after_import.next_at(), start + AFTER_IMPORT_DELAY);
    }

    #[test]
    fn run_due_advances_the_schedule_only_on_a_real_pass() {
        let fixture = setup();
        let start = at("2026-10-15T00:00:00Z");
        let due = start + FIRST_PASS_DELAY;
        let mut hk = Housekeeping::new(fixture.layout.clone(), Schedule::new(start, false));

        assert!(
            hk.run_due(start, &none(), &|_| false, None, false)
                .unwrap()
                .is_none()
        );

        let dry = hk.run_due(due, &none(), &|_| false, None, true).unwrap();
        assert!(dry.is_some());
        assert_eq!(
            hk.schedule().next_at(),
            due,
            "a dry run doesn't advance the schedule"
        );
        assert!(hk.due(due));

        let real = hk.run_due(due, &none(), &|_| false, None, false).unwrap();
        assert!(real.is_some());
        assert_eq!(hk.schedule().next_at(), due + PASS_INTERVAL);
        assert_eq!(hk.schedule().passes(), 1);
        assert!(
            hk.run_due(due, &none(), &|_| false, None, false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn inbox_names_follow_the_daemon_patterns() {
        let valid = [
            discord_name("2026-10-01T00:00:00Z", None),
            discord_name("2026-10-01T00:00:00Z", Some("png")),
            api_name("2026-10-01T00:00:00Z", 3, None),
            api_name("2026-10-01T00:00:00Z", 12, Some("jpeg")),
        ];
        for name in valid {
            assert!(downloaded_at(&name).is_some(), "{name} should parse");
        }
        assert_eq!(
            downloaded_at(&discord_name("2026-10-01T00:00:00Z", Some("png"))),
            Some(at("2026-10-01T00:00:00Z"))
        );

        let foreign = [
            "notes.txt".to_owned(),
            format!("{}-{}", "1".repeat(16), "1700000000000"),
            format!("{}-{}", "1".repeat(21), "1700000000000"),
            format!("{}-{}", "1".repeat(18), "170000000000"),
            format!("{}-{}", "1".repeat(18), "1700000000000.bad-ext"),
            // A v4 UUID, not v7.
            "0199e0c2-0000-4000-8000-00000000000c-1.png".to_owned(),
            // A non-digit index.
            "0199e0c2-0000-7000-8000-00000000000c-x.png".to_owned(),
        ];
        for name in foreign {
            assert!(downloaded_at(&name).is_none(), "{name} should be refused");
        }
    }

    #[test]
    fn to_api_groups_kinds_and_reports_the_total() {
        let mut report = Report::new(false);
        report.add(KIND_INBOX, 2, 4096);
        report.add(KIND_SESSION_BACKUPS, 1, 512);
        report.add(KIND_RUN_LOGS, 0, 0);
        let api = report.to_api();
        assert_eq!(api.freed_bytes, 4608);
        let deleted = api.deleted.unwrap();
        assert_eq!(deleted.len(), 2, "a kind with no deletions doesn't appear");
        assert_eq!(
            deleted[KIND_INBOX],
            HousekeepingTotal {
                count: 2,
                bytes: 4096
            }
        );
    }

    #[test]
    fn the_settings_view_is_read_as_spelled() {
        use claudeclaw_api::settings::{
            DiskSettings, InboxSettings, LoggingSettings, Settings, TimeoutSettings,
        };
        // Only the fields `HousekeepingSettings` reads matter here.
        fn settings_with(inbox: InboxSettings, logging: LoggingSettings) -> Settings {
            Settings {
                timezone: "UTC".into(),
                model: "m".into(),
                api: Default::default(),
                fallback: claudeclaw_api::settings::FallbackSettings {
                    model: "m".into(),
                    api: Default::default(),
                },
                agentic: claudeclaw_api::settings::AgenticSettings {
                    enabled: false,
                    default_mode: "m".into(),
                    modes: vec![],
                },
                heartbeat: claudeclaw_api::settings::HeartbeatSettings {
                    enabled: false,
                    interval: 1,
                    prompt: String::new(),
                    exclude_windows: vec![],
                },
                discord: claudeclaw_api::settings::DiscordSettings {
                    token: Default::default(),
                    allowed_user_ids: vec![],
                    listen_channels: vec![],
                    listen_guilds: vec![],
                    allowed_guilds: vec![],
                    channel_allowed_user_ids: BTreeMap::new(),
                    channel_names: BTreeMap::new(),
                    image_output_roots: vec![],
                    streaming: false,
                },
                security: claudeclaw_api::settings::SecuritySettings {
                    level: claudeclaw_api::settings::SecurityLevel::Moderate,
                    allowed_tools: vec![],
                    disallowed_tools: vec![],
                },
                session: claudeclaw_api::settings::SessionSettings {
                    auto_rotate: false,
                    max_messages: 1,
                    max_age_hours: 1,
                    summary_path: String::new(),
                },
                session_timeout_ms: 0,
                timeouts: TimeoutSettings {
                    heartbeat: 0,
                    job: 0,
                    default: 0,
                },
                logging,
                remote: claudeclaw_api::settings::RemoteSettings {
                    enabled: false,
                    listen: "127.0.0.1".into(),
                    port: 1,
                },
                run_path: claudeclaw_api::settings::RunPathSettings {
                    source: claudeclaw_api::settings::RunPathSource::Auto,
                    prepend: vec![],
                },
                run_locale: claudeclaw_api::settings::RunLocaleSetting::Auto,
                timeout_breaker: claudeclaw_api::settings::TimeoutBreakerSettings {
                    max_consecutive_timeouts: None,
                    max_failing_seconds: None,
                },
                auto_compact: claudeclaw_api::settings::AutoCompactSettings {
                    enabled: false,
                    timeout_secs: 0,
                },
                transcript_roots: vec![],
                inbox,
                disk: DiskSettings {
                    warn_free_mib: 1,
                    min_free_mib: 1,
                },
            }
        }
        let view = settings_with(
            InboxSettings {
                keep_days: Some(30),
                max_mib: Some(1024),
            },
            LoggingSettings {
                level: "info".into(),
                daemon_keep_files: 1,
                daemon_max_daily_mib: 1,
                daemon_max_total_mib: 1,
                run_log_keep_days: Some(90),
                audit_keep_months: Some(12),
            },
        );
        let read = HousekeepingSettings::from(&view);
        assert_eq!(read, settings(Some(30), Some(1024), Some(90), Some(12)));
    }

    #[test]
    fn audit_ids_are_distinct_and_v7_shaped() {
        let now = at("2026-10-15T00:00:00Z");
        let first = audit_id(now, 0);
        let second = audit_id(now, 1);
        assert_ne!(first, second);
        for id in [first, second] {
            assert_eq!(id.len(), 36, "{id}");
            assert_eq!(&id[14..18], "7000", "{id}");
            assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
        }
        let _ = Arc::new(());
    }
}
