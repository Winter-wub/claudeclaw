//! Owner notices: to every chat adapter and to the event bus (architecture §4 `claudeclaw-core`
//! `notify`; management-api §2.2, §2.5).
//!
//! A [`Notice`] is one thing the owner should hear about: a job result, a Launcher refusal, an
//! invalid job, a breaker opening, an unplanned restart, `disk_low`, and so on. [`Notices::send`]
//! fans it out to every registered [`Notifier`] (one per Chat transport) **and** publishes it as
//! `api::DaemonEvent::Notice` for the Desktop app. The Notifiers are awaited together, so a slow
//! one never holds up the rest; a failing one is logged and counted, and it never stops the others
//! and never panics.
//!
//! Two policies live here, both from closed tickets, and both persisted in
//! `daemon/state/notices.json` (architecture §9.1) so they outlive a Worker process:
//!
//! - **Invalid jobs, once per distinct content** ([`Notices::send_once`], #12): an identical notice
//!   (same [`NoticeKind`] and same body hash) is not sent twice, even across a restart. The record
//!   is dropped after 30 days unseen ([`NoticeLedger::prune_unseen`], Housekeeping, #44).
//! - **Unplanned restart, at most once per ten minutes** ([`Notices::send_restart`], #27). The
//!   floor is on the wall clock, from the persisted `restart_notice_at`: ten minutes of
//!   `tokio::time::Instant` cannot span two Worker processes.
//!
//! The dedupe key is [`content_hash`], an FNV-1a 64 of the notice's kind and body written as
//! sixteen lowercase hex digits. Unlike `DefaultHasher`, that value is stable across Rust versions,
//! so a hash one build writes still matches after an upgrade. A `notices.json` that will not parse
//! is set aside as `notices.json.unreadable-<ts>` and treated as empty: never a panic, and never a
//! notice blocked by it.
//!
//! Not every [`NoticeKind`] has an `api::NoticeKind` yet (the wire defines ten; core raises
//! thirteen). A kind without a wire value still reaches chat owners through `OwnerNotice`; the bus
//! event is omitted rather than sent as a meaningless `unknown` (see [`NoticeKind::to_api`]).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use claudeclaw_api::log_event::daemon;
use claudeclaw_api::{DaemonEvent, NoticeKind as ApiNoticeKind};
use claudeclaw_chat::{Notifier, OwnerNotice};
use claudeclaw_scheduler::{SystemClock, WallClock};
use claudeclaw_store::{FileMode, Layout, StoreError, write_atomic};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::events::EventBus;
use crate::scope::Scope;

/// The shortest gap between two unplanned-restart notices (#27).
pub const RESTART_MIN_INTERVAL: Duration = Duration::from_secs(600);

/// The `notices.json` schema this build writes (architecture §9.1).
pub const LEDGER_SCHEMA: u32 = 1;

/// What an owner notice is about (architecture §4 `notify`). [`NoticeKind::as_str`] is the
/// `snake_case` value a `chat::OwnerNotice` carries; [`NoticeKind::to_api`] maps onto
/// `api::NoticeKind` where the wire names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NoticeKind {
    JobFailed,
    /// The app decides whether to show it, per job (#16).
    JobFinished,
    /// A job file that no longer parses (#12); sent once per distinct content.
    JobInvalid,
    HeartbeatOutput,
    /// A run was refused, such as an unrunnable Launcher.
    RunRefused,
    DevicePaired,
    DeviceRevoked,
    /// The Worker restarted without being asked (#27); rate-limited by [`Notices::send_restart`].
    WorkerRestarted,
    /// A Timeout breaker opened (#32).
    BreakerOpen,
    /// An outside edit broke a job's frontmatter and the Daemon put it back (#46).
    JobFrontmatterRestored,
    /// Free space fell under `disk.warnFreeMiB` (#44).
    DiskLow,
    /// No Run locale could be chosen (#43).
    RunLocaleUnavailable,
    /// A session's Transcript root moved (#39).
    LauncherRootMoved,
}

impl NoticeKind {
    /// Every kind, in declaration order.
    pub const ALL: [NoticeKind; 13] = [
        NoticeKind::JobFailed,
        NoticeKind::JobFinished,
        NoticeKind::JobInvalid,
        NoticeKind::HeartbeatOutput,
        NoticeKind::RunRefused,
        NoticeKind::DevicePaired,
        NoticeKind::DeviceRevoked,
        NoticeKind::WorkerRestarted,
        NoticeKind::BreakerOpen,
        NoticeKind::JobFrontmatterRestored,
        NoticeKind::DiskLow,
        NoticeKind::RunLocaleUnavailable,
        NoticeKind::LauncherRootMoved,
    ];

    /// The `snake_case` name, for a `chat::OwnerNotice` and the Daemon log.
    pub const fn as_str(self) -> &'static str {
        match self {
            NoticeKind::JobFailed => "job_failed",
            NoticeKind::JobFinished => "job_finished",
            NoticeKind::JobInvalid => "job_invalid",
            NoticeKind::HeartbeatOutput => "heartbeat_output",
            NoticeKind::RunRefused => "run_refused",
            NoticeKind::DevicePaired => "device_paired",
            NoticeKind::DeviceRevoked => "device_revoked",
            NoticeKind::WorkerRestarted => "worker_restarted",
            NoticeKind::BreakerOpen => "breaker_open",
            NoticeKind::JobFrontmatterRestored => "job_frontmatter_restored",
            NoticeKind::DiskLow => "disk_low",
            NoticeKind::RunLocaleUnavailable => "run_locale_unavailable",
            NoticeKind::LauncherRootMoved => "launcher_root_moved",
        }
    }

    /// The `api::NoticeKind` for `DaemonEvent::Notice`, where the wire defines one. `None` for the
    /// six kinds added by the Rust rewrite that `api::NoticeKind` does not name yet: the notice
    /// still reaches chat owners, but no bus event is sent rather than a bare `unknown`.
    pub const fn to_api(self) -> Option<ApiNoticeKind> {
        match self {
            NoticeKind::JobFailed => Some(ApiNoticeKind::JobFailed),
            NoticeKind::JobFinished => Some(ApiNoticeKind::JobFinished),
            NoticeKind::JobInvalid => Some(ApiNoticeKind::JobInvalid),
            NoticeKind::HeartbeatOutput => Some(ApiNoticeKind::HeartbeatOutput),
            NoticeKind::RunRefused => Some(ApiNoticeKind::RunRefused),
            NoticeKind::JobFrontmatterRestored => Some(ApiNoticeKind::JobFrontmatterRestored),
            NoticeKind::DiskLow => Some(ApiNoticeKind::DiskLow),
            NoticeKind::DevicePaired
            | NoticeKind::DeviceRevoked
            | NoticeKind::WorkerRestarted
            | NoticeKind::BreakerOpen
            | NoticeKind::RunLocaleUnavailable
            | NoticeKind::LauncherRootMoved => None,
        }
    }
}

/// The FNV-1a 64 hash of a notice's kind and body. Stable across Rust versions (unlike
/// `DefaultHasher`), so a value written to `notices.json` still matches after an upgrade. Written
/// as sixteen lowercase hex digits.
fn content_hash(kind: NoticeKind, text: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in kind.as_str().bytes().chain(text.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// One owner notice. `title` is the short label the bus carries; `text` is the body a chat adapter
/// sends. Both are content: `Debug` prints only their sizes.
#[derive(Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    /// A short label, never the body (management-api §2.2).
    pub title: String,
    /// The text a chat adapter sends to the owners.
    pub text: String,
    pub scope: Option<Scope>,
    pub job: Option<String>,
    pub run_id: Option<String>,
}

impl Notice {
    /// A notice with no Scope, job or run.
    pub fn new(kind: NoticeKind, title: impl Into<String>, text: impl Into<String>) -> Self {
        Notice {
            kind,
            title: title.into(),
            text: text.into(),
            scope: None,
            job: None,
            run_id: None,
        }
    }

    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = Some(scope);
        self
    }

    pub fn with_job(mut self, job: impl Into<String>) -> Self {
        self.job = Some(job.into());
        self
    }

    pub fn with_run(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    /// The `notices.json` key of this notice's content: `<kind>:<hash>`, the hash being
    /// [`content_hash`] as sixteen lowercase hex digits.
    fn ledger_key(&self) -> String {
        format!(
            "{}:{:016x}",
            self.kind.as_str(),
            content_hash(self.kind, &self.text)
        )
    }
}

impl fmt::Debug for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Notice")
            .field("kind", &self.kind)
            .field("title_bytes", &self.title.len())
            .field("text_bytes", &self.text.len())
            .field("scope", &self.scope)
            .field("job", &self.job)
            .field("run_id", &self.run_id)
            .finish()
    }
}

/// How many Notifiers a [`Notice`] reached and how many failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Delivery {
    pub sent: usize,
    pub failed: usize,
}

/// `daemon/state/notices.json` (architecture §9.1): the content hashes already notified, and when
/// the last unplanned-restart notice went out. Written whole and atomically; keys a later build
/// added are kept, at the top level and per entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LedgerFile {
    #[serde(default = "ledger_schema")]
    schema: u32,
    #[serde(default)]
    entries: BTreeMap<String, LedgerEntry>,
    #[serde(default)]
    restart_notice_at: Option<DateTime<Utc>>,
    /// Top-level keys this build doesn't know.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl Default for LedgerFile {
    fn default() -> Self {
        LedgerFile {
            schema: LEDGER_SCHEMA,
            entries: BTreeMap::new(),
            restart_notice_at: None,
            extra: Map::new(),
        }
    }
}

fn ledger_schema() -> u32 {
    LEDGER_SCHEMA
}

/// One content hash already notified (#12): when it was first seen and when it was last seen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LedgerEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_seen_at: Option<DateTime<Utc>>,
    /// Keys this build doesn't know.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl LedgerEntry {
    /// A hash seen for the first time now.
    fn seen(now: DateTime<Utc>) -> Self {
        LedgerEntry {
            first_at: Some(now),
            last_seen_at: Some(now),
            extra: Map::new(),
        }
    }

    /// The time this entry was last seen, falling back to when it was first seen.
    fn last_seen(&self) -> Option<DateTime<Utc>> {
        self.last_seen_at.or(self.first_at)
    }

    /// Whether this entry has not been seen since `older_than`.
    fn is_unseen_before(&self, older_than: DateTime<Utc>) -> bool {
        self.last_seen().is_some_and(|seen| seen < older_than)
    }
}

/// The dedupe facts `daemon/state/notices.json` holds (architecture §9.1, #12, #27). Every method
/// is synchronous, so a caller on the async runtime wraps it in `spawn_blocking`; IO failures are
/// logged and never returned, because a bad ledger must never block a notice.
pub struct NoticeLedger {
    /// `None` when nothing is persisted (an in-memory hub, or tests).
    layout: Option<Layout>,
    state: Mutex<LedgerFile>,
}

impl NoticeLedger {
    /// An empty ledger that persists nothing.
    pub fn ephemeral() -> Self {
        NoticeLedger {
            layout: None,
            state: Mutex::new(LedgerFile::default()),
        }
    }

    /// Reads `daemon/state/notices.json` (sync: call in `spawn_blocking`). A missing file is an
    /// empty ledger; an unreadable or corrupt one is set aside as `notices.json.unreadable-<ts>`,
    /// logged, and treated as empty.
    pub fn load(layout: &Layout) -> Self {
        let ledger = NoticeLedger {
            layout: Some(layout.clone()),
            state: Mutex::new(LedgerFile::default()),
        };
        let path = layout.notices();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return ledger,
            Err(err) => {
                set_aside(&path, &err.to_string());
                return ledger;
            }
        };
        match serde_json::from_slice::<LedgerFile>(&bytes) {
            Ok(file) => *lock(&ledger.state) = file,
            Err(err) => set_aside(&path, &err.to_string()),
        }
        ledger
    }

    /// How many content hashes the ledger holds.
    pub fn entry_count(&self) -> usize {
        lock(&self.state).entries.len()
    }

    /// When the last unplanned-restart notice went out (#27).
    pub fn restart_notice_at(&self) -> Option<DateTime<Utc>> {
        lock(&self.state).restart_notice_at
    }

    /// Records `key` as seen at `now`; `true` when it is new, so its notice should go out. An
    /// existing entry only has `last_seen_at` moved, then the ledger is persisted.
    fn mark_seen(&self, key: &str, now: DateTime<Utc>) -> bool {
        let is_new = {
            let mut file = lock(&self.state);
            match file.entries.get_mut(key) {
                Some(entry) => {
                    entry.last_seen_at = Some(now);
                    false
                }
                None => {
                    file.entries.insert(key.to_owned(), LedgerEntry::seen(now));
                    true
                }
            }
        };
        self.persist();
        is_new
    }

    /// `true` when the restart floor allows a notice now; records `now` as the time when it does
    /// (#27). The comparison is on the wall clock, so it holds across processes.
    fn claim_restart(&self, now: DateTime<Utc>) -> bool {
        let within_floor = {
            let file = lock(&self.state);
            match file.restart_notice_at {
                Some(previous) => {
                    now.signed_duration_since(previous)
                        < chrono::Duration::seconds(RESTART_MIN_INTERVAL.as_secs() as i64)
                }
                None => false,
            }
        };
        if within_floor {
            return false;
        }
        lock(&self.state).restart_notice_at = Some(now);
        self.persist();
        true
    }

    /// Drops entries whose `last_seen_at` is older than `older_than`, returning how many (#44
    /// Housekeeping, architecture §9.1). With `dry_run` the count is computed and nothing is
    /// changed, on disk or in memory. Sync: Housekeeping calls it from `spawn_blocking`.
    pub fn prune_unseen(
        &self,
        older_than: DateTime<Utc>,
        dry_run: bool,
    ) -> Result<u64, std::io::Error> {
        let mut file = lock(&self.state);
        let doomed = file
            .entries
            .values()
            .filter(|entry| entry.is_unseen_before(older_than))
            .count() as u64;
        if dry_run || doomed == 0 {
            return Ok(doomed);
        }
        file.entries
            .retain(|_, entry| !entry.is_unseen_before(older_than));
        drop(file);
        self.save().map_err(std::io::Error::other)?;
        Ok(doomed)
    }

    /// Writes the ledger atomically at 0600 (§9.1). A no-op for an ephemeral ledger.
    fn save(&self) -> Result<(), StoreError> {
        let Some(layout) = &self.layout else {
            return Ok(());
        };
        let file = lock(&self.state);
        if !layout.state_dir().is_dir() {
            layout.create_daemon_dirs()?;
        }
        let path = layout.notices();
        let mut bytes = serde_json::to_vec_pretty(&*file).map_err(|source| StoreError::Json {
            path: path.clone(),
            source,
        })?;
        bytes.push(b'\n');
        write_atomic(&path, &bytes, FileMode::Private)
    }

    /// Best-effort save: a failure is logged and dropped, never returned, because a write that
    /// fails must not stop the notice it is recording.
    fn persist(&self) {
        if let Err(err) = self.save() {
            tracing::warn!(
                event = daemon::NOTIFY_FAILED,
                err = %err,
                "the notice ledger could not be written"
            );
        }
    }
}

impl fmt::Debug for NoticeLedger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = lock(&self.state);
        f.debug_struct("NoticeLedger")
            .field("persisted", &self.layout.is_some())
            .field("entries", &file.entries.len())
            .field("restart_notice_at", &file.restart_notice_at)
            .finish()
    }
}

/// Renames the ledger aside so a bad file is not re-read on the next start (the
/// `supervisor.json.<…>` pattern, architecture §9.1), logging why. A rename that fails is logged
/// and ignored: a bad ledger never blocks a notice.
fn set_aside(path: &Path, reason: &str) {
    tracing::warn!(
        event = daemon::NOTIFY_FAILED,
        err = reason,
        "notices.json is unreadable; starting an empty ledger"
    );
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("notices.json");
    let aside = path.with_file_name(format!("{name}.unreadable-{}", now_stamp()));
    if let Err(err) = std::fs::rename(path, &aside) {
        tracing::warn!(
            event = daemon::NOTIFY_FAILED,
            err = %err,
            "notices.json could not be set aside"
        );
    }
}

/// The Unix-seconds stamp a set-aside ledger is named with. Uses `SystemTime`, so it needs no
/// wall clock (chrono's `clock` feature is off).
fn now_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Owner notices for the Worker: the registered Notifiers, the event bus, the persisted dedupe
/// ledger and the wall clock the restart floor is on.
pub struct Notices {
    bus: EventBus,
    ledger: Arc<NoticeLedger>,
    clock: Arc<dyn WallClock>,
    notifiers: Mutex<Vec<Arc<dyn Notifier>>>,
}

impl Notices {
    /// A hub on `bus` that persists dedupe facts in `layout`'s `notices.json` (architecture §9.1).
    /// The synchronous read runs on `spawn_blocking`; a read that cannot finish leaves an empty
    /// ledger rather than a panic.
    pub async fn open(bus: EventBus, layout: &Layout) -> Self {
        let layout = layout.clone();
        let ledger = match tokio::task::spawn_blocking(move || NoticeLedger::load(&layout)).await {
            Ok(ledger) => ledger,
            Err(err) => {
                tracing::warn!(
                    event = daemon::NOTIFY_FAILED,
                    err = %err,
                    "the notices read did not finish; starting an empty ledger"
                );
                NoticeLedger::ephemeral()
            }
        };
        Notices::with_ledger(bus, ledger)
    }

    /// A hub with an in-memory ledger: dedupe holds for this Worker only, nothing is persisted.
    pub fn in_memory(bus: EventBus) -> Self {
        Notices::with_ledger(bus, NoticeLedger::ephemeral())
    }

    fn with_ledger(bus: EventBus, ledger: NoticeLedger) -> Self {
        Notices {
            bus,
            ledger: Arc::new(ledger),
            clock: Arc::new(SystemClock),
            notifiers: Mutex::new(Vec::new()),
        }
    }

    /// Uses `clock` for the unplanned-restart floor (#27). The default is the system clock.
    pub fn with_clock(mut self, clock: Arc<dyn WallClock>) -> Self {
        self.clock = clock;
        self
    }

    /// Adds a Notifier to the fan-out, once per adapter as it registers with the Worker.
    pub fn register(&self, notifier: Arc<dyn Notifier>) {
        lock(&self.notifiers).push(notifier);
    }

    /// How many Notifiers are registered.
    pub fn notifier_count(&self) -> usize {
        lock(&self.notifiers).len()
    }

    /// Publishes `notice` to the event bus and sends it to every Notifier together. One failing
    /// Notifier logs `notify.failed` and is counted; it never stops the rest, and a slow one never
    /// delays the others.
    pub async fn send(&self, notice: Notice) -> Delivery {
        self.publish(&notice);
        let notifiers: Vec<Arc<dyn Notifier>> = lock(&self.notifiers).clone();
        let sends = notifiers.into_iter().map(|notifier| {
            let msg = owner_notice(&notice);
            async move { notifier.notify_owners(msg).await }
        });
        let mut delivery = Delivery::default();
        for result in join_all(sends).await {
            match result {
                Ok(()) => {
                    delivery.sent += 1;
                    tracing::debug!(event = daemon::NOTIFY_SENT, "owner notice delivered");
                }
                Err(err) => {
                    delivery.failed += 1;
                    tracing::warn!(
                        event = daemon::NOTIFY_FAILED,
                        err = %err,
                        "an owner notice could not be delivered"
                    );
                }
            }
        }
        delivery
    }

    /// Sends `notice` unless an identical one (same kind and content) was already recorded, even
    /// by an earlier Worker (#12). `None` means it was suppressed.
    pub async fn send_once(&self, notice: Notice) -> Option<Delivery> {
        let key = notice.ledger_key();
        let now = self.clock.now();
        let ledger = Arc::clone(&self.ledger);
        let is_new = match tokio::task::spawn_blocking(move || ledger.mark_seen(&key, now)).await {
            Ok(is_new) => is_new,
            Err(err) => {
                tracing::warn!(
                    event = daemon::NOTIFY_FAILED,
                    err = %err,
                    "the notice ledger write did not finish; sending anyway"
                );
                true
            }
        };
        if !is_new {
            return None;
        }
        Some(self.send(notice).await)
    }

    /// Sends an unplanned-restart notice, at most once per [`RESTART_MIN_INTERVAL`] (#27). The
    /// floor is read from the persisted `restart_notice_at` on the wall clock, so it holds across
    /// Worker processes. `None` means it was suppressed.
    pub async fn send_restart(&self, notice: Notice) -> Option<Delivery> {
        let now = self.clock.now();
        let ledger = Arc::clone(&self.ledger);
        let allowed = match tokio::task::spawn_blocking(move || ledger.claim_restart(now)).await {
            Ok(allowed) => allowed,
            Err(err) => {
                tracing::warn!(
                    event = daemon::NOTIFY_FAILED,
                    err = %err,
                    "the notice ledger write did not finish; sending anyway"
                );
                true
            }
        };
        if !allowed {
            return None;
        }
        Some(self.send(notice).await)
    }

    /// Publishes `notice` as `DaemonEvent::Notice`, when its kind has a wire value.
    fn publish(&self, notice: &Notice) {
        let Some(notice_kind) = notice.kind.to_api() else {
            return;
        };
        self.bus.publish(DaemonEvent::Notice {
            notice_kind,
            scope: notice.scope.as_ref().and_then(|scope| scope.to_api().ok()),
            job: notice.job.clone(),
            run_id: notice.run_id.clone(),
            title: notice.title.clone(),
        });
    }
}

impl fmt::Debug for Notices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Notices")
            .field("notifiers", &lock(&self.notifiers).len())
            .field("ledger", &self.ledger)
            .finish()
    }
}

/// The `chat::OwnerNotice` for a Notice: its kind's name and its body.
fn owner_notice(notice: &Notice) -> OwnerNotice {
    OwnerNotice {
        kind: notice.kind.as_str().to_owned(),
        text: notice.text.clone(),
    }
}

/// A mutex guard, recovering from poisoning rather than panicking: nothing here panics while
/// holding one, so a poison can only come from a bug elsewhere, and dropping the lock is worse.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::TimeZone;
    use claudeclaw_chat::{MessageRef, NotifyError};
    use futures_util::future::BoxFuture;
    use serde_json::json;
    use tokio::sync::broadcast::error::TryRecvError;
    use tokio::time::Instant;

    use super::*;

    /// A Notifier that records what it was sent, or always fails.
    struct RecordingNotifier {
        seen: Arc<Mutex<Vec<OwnerNotice>>>,
        fail: bool,
    }

    impl RecordingNotifier {
        fn new(fail: bool) -> (Arc<Self>, Arc<Mutex<Vec<OwnerNotice>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let notifier = Arc::new(RecordingNotifier {
                seen: seen.clone(),
                fail,
            });
            (notifier, seen)
        }
    }

    impl Notifier for RecordingNotifier {
        fn notify_owners(&self, msg: OwnerNotice) -> BoxFuture<'_, Result<(), NotifyError>> {
            Box::pin(async move {
                if self.fail {
                    return Err(NotifyError::NotConnected);
                }
                self.seen.lock().unwrap().push(msg);
                Ok(())
            })
        }

        fn interrupted(&self, _origin: MessageRef) -> BoxFuture<'_, Result<(), NotifyError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A Notifier that takes `delay` before recording, to show the others are not held up.
    struct SlowNotifier {
        seen: Arc<Mutex<Vec<OwnerNotice>>>,
        delay: Duration,
    }

    impl Notifier for SlowNotifier {
        fn notify_owners(&self, msg: OwnerNotice) -> BoxFuture<'_, Result<(), NotifyError>> {
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                self.seen.lock().unwrap().push(msg);
                Ok(())
            })
        }

        fn interrupted(&self, _origin: MessageRef) -> BoxFuture<'_, Result<(), NotifyError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A wall clock that moves with tokio's paused clock.
    struct FakeClock {
        base: DateTime<Utc>,
        start: Instant,
    }

    impl FakeClock {
        fn new(base: DateTime<Utc>) -> Arc<Self> {
            Arc::new(FakeClock {
                base,
                start: Instant::now(),
            })
        }
    }

    impl WallClock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            self.base + chrono::Duration::from_std(self.start.elapsed()).unwrap()
        }
    }

    fn base_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
    }

    fn notice(kind: NoticeKind, text: &str) -> Notice {
        Notice::new(kind, "title", text)
    }

    /// A Layout under a fresh tempdir, with `daemon/state/` created.
    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.create_daemon_dirs().unwrap();
        (dir, layout)
    }

    #[test]
    fn every_kind_is_named_and_maps_to_the_wire_where_it_exists() {
        assert_eq!(NoticeKind::ALL.len(), 13);
        let mut names = BTreeSet::new();
        for kind in NoticeKind::ALL {
            assert!(names.insert(kind.as_str()), "duplicate {:?}", kind);
        }
        assert_eq!(
            NoticeKind::JobFailed.to_api(),
            Some(ApiNoticeKind::JobFailed)
        );
        assert_eq!(NoticeKind::DiskLow.as_str(), "disk_low");
        assert_eq!(
            NoticeKind::JobFrontmatterRestored.to_api(),
            Some(ApiNoticeKind::JobFrontmatterRestored)
        );
        assert_eq!(NoticeKind::LauncherRootMoved.to_api(), None);
        assert_eq!(NoticeKind::WorkerRestarted.to_api(), None);
    }

    #[test]
    fn the_content_hash_is_a_pinned_fnv1a_of_kind_and_text() {
        // Pinned so a change to the algorithm is a deliberate change to persisted keys.
        assert_eq!(
            content_hash(NoticeKind::JobInvalid, "missing schedule"),
            0x1a02_28a2_aec0_2259
        );
        let key = notice(NoticeKind::JobInvalid, "missing schedule").ledger_key();
        let (kind, hash) = key.split_once(':').unwrap();
        assert_eq!(kind, "job_invalid");
        assert_eq!(hash, "1a0228a2aec02259");
        assert_eq!(hash.len(), 16);
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[tokio::test]
    async fn notices_fan_out_to_every_notifier_and_the_bus() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let notices = Notices::in_memory(bus.clone());
        let (first, first_seen) = RecordingNotifier::new(false);
        let (second, second_seen) = RecordingNotifier::new(false);
        notices.register(first);
        notices.register(second);
        assert_eq!(notices.notifier_count(), 2);

        let delivery = notices
            .send(
                Notice::new(
                    NoticeKind::JobFailed,
                    "daily-report failed",
                    "the run failed",
                )
                .with_scope(Scope::Job("daily-report".into()))
                .with_job("daily-report")
                .with_run("run-1"),
            )
            .await;
        assert_eq!(delivery, Delivery { sent: 2, failed: 0 });
        assert_eq!(first_seen.lock().unwrap().len(), 1);
        assert_eq!(second_seen.lock().unwrap()[0].kind, "job_failed");

        assert_eq!(
            rx.recv().await.unwrap(),
            DaemonEvent::Notice {
                notice_kind: ApiNoticeKind::JobFailed,
                scope: Some(claudeclaw_api::Scope::Job("daily-report".into())),
                job: Some("daily-report".into()),
                run_id: Some("run-1".into()),
                title: "daily-report failed".into(),
            }
        );
    }

    #[tokio::test]
    async fn a_failing_notifier_does_not_stop_the_others() {
        let notices = Notices::in_memory(EventBus::new());
        let (failing, _) = RecordingNotifier::new(true);
        let (working, working_seen) = RecordingNotifier::new(false);
        notices.register(failing);
        notices.register(working);

        let delivery = notices.send(notice(NoticeKind::DiskLow, "low disk")).await;
        assert_eq!(delivery, Delivery { sent: 1, failed: 1 });
        assert_eq!(working_seen.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_notifier_does_not_delay_a_fast_one() {
        let notices = Arc::new(Notices::in_memory(EventBus::new()));
        let slow_seen = Arc::new(Mutex::new(Vec::new()));
        notices.register(Arc::new(SlowNotifier {
            seen: slow_seen.clone(),
            delay: Duration::from_secs(60),
        }));
        let (fast, fast_seen) = RecordingNotifier::new(false);
        notices.register(fast);

        let send = tokio::spawn({
            let notices = notices.clone();
            async move { notices.send(notice(NoticeKind::DiskLow, "low disk")).await }
        });
        // The fast Notifier is registered last: sequentially it would wait 60 s behind the slow one.
        tokio::task::yield_now().await;
        assert_eq!(
            fast_seen.lock().unwrap().len(),
            1,
            "the fast notifier was held up by the slow one"
        );
        assert_eq!(slow_seen.lock().unwrap().len(), 0);

        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(send.await.unwrap(), Delivery { sent: 2, failed: 0 });
    }

    #[tokio::test]
    async fn identical_invalid_job_notices_are_sent_once() {
        let notices = Notices::in_memory(EventBus::new());
        let (notifier, seen) = RecordingNotifier::new(false);
        notices.register(notifier);

        let first = notice(NoticeKind::JobInvalid, "missing schedule");
        let duplicate = notice(NoticeKind::JobInvalid, "missing schedule");
        assert!(notices.send_once(first).await.is_some());
        assert!(notices.send_once(duplicate).await.is_none());
        // A different body is a different notice.
        assert!(
            notices
                .send_once(notice(NoticeKind::JobInvalid, "bad cron"))
                .await
                .is_some()
        );
        // So is the same body under another kind.
        assert!(
            notices
                .send_once(notice(NoticeKind::JobFailed, "missing schedule"))
                .await
                .is_some()
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn an_invalid_job_notice_survives_a_reload() {
        let (_dir, layout) = layout();
        let first = Notices::open(EventBus::new(), &layout).await;
        let (notifier, seen) = RecordingNotifier::new(false);
        first.register(notifier);
        assert!(
            first
                .send_once(notice(NoticeKind::JobInvalid, "missing schedule"))
                .await
                .is_some()
        );
        assert_eq!(seen.lock().unwrap().len(), 1);

        // A fresh Worker process reads the same ledger and suppresses the same notice.
        let second = Notices::open(EventBus::new(), &layout).await;
        let (reloaded, reloaded_seen) = RecordingNotifier::new(false);
        second.register(reloaded);
        assert!(
            second
                .send_once(notice(NoticeKind::JobInvalid, "missing schedule"))
                .await
                .is_none()
        );
        assert_eq!(reloaded_seen.lock().unwrap().len(), 0);
        // A different body still goes out.
        assert!(
            second
                .send_once(notice(NoticeKind::JobInvalid, "bad cron"))
                .await
                .is_some()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_restart_floor_holds_across_a_reload() {
        let (_dir, layout) = layout();
        let clock = FakeClock::new(base_time());
        let restart = || notice(NoticeKind::WorkerRestarted, "restarted after a hang");

        let first = Notices::open(EventBus::new(), &layout)
            .await
            .with_clock(clock.clone());
        let (notifier, seen) = RecordingNotifier::new(false);
        first.register(notifier);
        assert!(first.send_restart(restart()).await.is_some());
        assert_eq!(seen.lock().unwrap().len(), 1);

        // Five minutes later, a reloaded ledger still suppresses it.
        tokio::time::advance(Duration::from_secs(300)).await;
        let second = Notices::open(EventBus::new(), &layout)
            .await
            .with_clock(clock.clone());
        let (suppressed, suppressed_seen) = RecordingNotifier::new(false);
        second.register(suppressed);
        assert!(second.send_restart(restart()).await.is_none());
        assert_eq!(suppressed_seen.lock().unwrap().len(), 0);

        // Eleven minutes after the first, it goes out again.
        tokio::time::advance(Duration::from_secs(360)).await;
        let third = Notices::open(EventBus::new(), &layout)
            .await
            .with_clock(clock.clone());
        let (again, again_seen) = RecordingNotifier::new(false);
        third.register(again);
        assert!(third.send_restart(restart()).await.is_some());
        assert_eq!(again_seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_corrupt_ledger_is_set_aside_and_the_notice_still_sends() {
        let (_dir, layout) = layout();
        tokio::fs::write(layout.notices(), b"{ not json")
            .await
            .unwrap();

        let notices = Notices::open(EventBus::new(), &layout).await;
        let (notifier, seen) = RecordingNotifier::new(false);
        notices.register(notifier);
        assert!(
            notices
                .send_once(notice(NoticeKind::JobInvalid, "missing schedule"))
                .await
                .is_some()
        );
        assert_eq!(seen.lock().unwrap().len(), 1);

        // The bad file was renamed aside, not left to be re-read.
        let mut aside = false;
        let mut entries = tokio::fs::read_dir(layout.state_dir()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("notices.json.unreadable-")
            {
                aside = true;
            }
        }
        assert!(aside, "the corrupt ledger was not set aside");
    }

    #[test]
    fn a_ledger_keeps_keys_a_later_build_added() {
        let (_dir, layout) = layout();
        std::fs::write(
            layout.notices(),
            r#"{"schema":1,"future":true,"entries":{"job_invalid:0000000000000000":{"first_at":"2026-01-01T00:00:00Z","last_seen_at":"2026-01-01T00:00:00Z","from_later":7}},"restart_notice_at":null}"#,
        )
        .unwrap();

        let ledger = NoticeLedger::load(&layout);
        let key = format!(
            "job_invalid:{:016x}",
            content_hash(NoticeKind::JobInvalid, "x")
        );
        ledger.mark_seen(&key, base_time());

        let value: Value =
            serde_json::from_slice(&std::fs::read(layout.notices()).unwrap()).unwrap();
        assert_eq!(value["future"], json!(true));
        assert_eq!(
            value["entries"]["job_invalid:0000000000000000"]["from_later"],
            json!(7)
        );
    }

    #[test]
    fn pruning_drops_unseen_entries_and_dry_run_keeps_the_file() {
        let (_dir, layout) = layout();
        let ledger = NoticeLedger::load(&layout);
        let old = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        ledger.mark_seen(
            &format!(
                "job_invalid:{:016x}",
                content_hash(NoticeKind::JobInvalid, "old")
            ),
            old,
        );
        ledger.mark_seen(
            &format!(
                "job_invalid:{:016x}",
                content_hash(NoticeKind::JobInvalid, "new")
            ),
            now,
        );
        let before = std::fs::read(layout.notices()).unwrap();
        let cutoff = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();

        // A dry run reports the count and touches nothing.
        assert_eq!(ledger.prune_unseen(cutoff, true).unwrap(), 1);
        assert_eq!(std::fs::read(layout.notices()).unwrap(), before);
        // The real one reports the same count and writes.
        assert_eq!(ledger.prune_unseen(cutoff, false).unwrap(), 1);
        assert_ne!(std::fs::read(layout.notices()).unwrap(), before);
        // A reload sees only the fresh entry.
        assert_eq!(NoticeLedger::load(&layout).entry_count(), 1);
    }

    #[tokio::test]
    async fn kinds_without_a_wire_value_still_reach_chat_owners() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let notices = Notices::in_memory(bus.clone());
        let (notifier, seen) = RecordingNotifier::new(false);
        notices.register(notifier);

        let delivery = notices
            .send(notice(NoticeKind::BreakerOpen, "the breaker opened"))
            .await;
        assert_eq!(delivery, Delivery { sent: 1, failed: 0 });
        assert_eq!(seen.lock().unwrap()[0].kind, "breaker_open");
        // No `api::NoticeKind` exists, so no bus event is sent rather than a bare `unknown`.
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn notice_debug_hides_the_body() {
        let notice = Notice::new(
            NoticeKind::JobFailed,
            "daily-report failed",
            "secret output",
        );
        let got = format!("{notice:?}");
        assert!(!got.contains("secret"), "{got}");
        assert!(got.contains("JobFailed"), "{got}");
    }
}
