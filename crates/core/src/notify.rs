//! Owner notices: to every chat adapter and to the event bus (architecture §4 `claudeclaw-core`
//! `notify`; management-api §2.2, §2.5).
//!
//! A [`Notice`] is one thing the owner should hear about: a job result, a Launcher refusal, an
//! invalid job, a breaker opening, an unplanned restart, `disk_low`, and so on. [`Notices::send`]
//! fans it out to every registered [`Notifier`] (one per Chat transport) **and** publishes it as
//! `api::DaemonEvent::Notice` for the Desktop app. A failing Notifier is logged and counted; it
//! never stops the others and never panics.
//!
//! Two policies live here, both from closed tickets:
//!
//! - **Invalid jobs, once per distinct content** ([`Notices::send_once`], #12): an identical notice
//!   (same [`NoticeKind`] and same title-and-body hash) is not sent twice in a Worker's lifetime.
//!   The invalid-job path calls `send_once`; `send` always sends.
//! - **Unplanned restart, at most once per ten minutes** ([`Notices::send_restart`], #27).
//!
//! Not every [`NoticeKind`] has an `api::NoticeKind` yet (the wire defines ten; core raises
//! thirteen). A kind without a wire value still reaches chat owners through `OwnerNotice`; the bus
//! event is omitted rather than sent as a meaningless `unknown` (see [`NoticeKind::to_api`]).

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use claudeclaw_api::log_event::daemon;
use claudeclaw_api::{DaemonEvent, NoticeKind as ApiNoticeKind};
use claudeclaw_chat::{Notifier, OwnerNotice};
use tokio::time::Instant;

use crate::events::EventBus;
use crate::scope::Scope;

/// The shortest gap between two unplanned-restart notices (#27).
pub const RESTART_MIN_INTERVAL: Duration = Duration::from_secs(600);

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

    /// A stable hash of the notice's content, for [`Notices::send_once`]. `DefaultHasher::new` is
    /// deterministic, and the value never leaves the process.
    fn content_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.title.hash(&mut hasher);
        self.text.hash(&mut hasher);
        hasher.finish()
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

/// Owner notices for the Worker: the registered Notifiers, the event bus, and the two dedupe
/// policies.
pub struct Notices {
    bus: EventBus,
    notifiers: Mutex<Vec<Arc<dyn Notifier>>>,
    /// `(kind, content hash)` of every `send_once` notice, for the Worker's lifetime.
    seen: Mutex<HashSet<(NoticeKind, u64)>>,
    /// When the last unplanned-restart notice went out.
    last_restart: Mutex<Option<Instant>>,
}

impl Notices {
    /// A notice hub publishing to `bus`; the caller shares the same [`EventBus`] with the SSE
    /// route.
    pub fn new(bus: EventBus) -> Self {
        Notices {
            bus,
            notifiers: Mutex::new(Vec::new()),
            seen: Mutex::new(HashSet::new()),
            last_restart: Mutex::new(None),
        }
    }

    /// Adds a Notifier to the fan-out, once per adapter as it registers with the Worker.
    pub fn register(&self, notifier: Arc<dyn Notifier>) {
        lock(&self.notifiers).push(notifier);
    }

    /// How many Notifiers are registered.
    pub fn notifier_count(&self) -> usize {
        lock(&self.notifiers).len()
    }

    /// Publishes `notice` to the event bus and sends it to every Notifier. One failing Notifier
    /// logs `notify.failed` and is counted; it never stops the rest.
    pub async fn send(&self, notice: Notice) -> Delivery {
        self.publish(&notice);
        let notifiers: Vec<Arc<dyn Notifier>> = lock(&self.notifiers).clone();
        let mut delivery = Delivery::default();
        for notifier in notifiers {
            match notifier.notify_owners(owner_notice(&notice)).await {
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

    /// Sends `notice` unless an identical one (same kind and content) already went out in this
    /// Worker's lifetime (#12). `None` means it was suppressed.
    pub async fn send_once(&self, notice: Notice) -> Option<Delivery> {
        let key = (notice.kind, notice.content_hash());
        if !lock(&self.seen).insert(key) {
            return None;
        }
        Some(self.send(notice).await)
    }

    /// Sends an unplanned-restart notice, at most once per [`RESTART_MIN_INTERVAL`] (#27). `None`
    /// means it was suppressed.
    pub async fn send_restart(&self, notice: Notice) -> Option<Delivery> {
        let now = Instant::now();
        {
            let mut last = lock(&self.last_restart);
            match *last {
                Some(previous) if now.duration_since(previous) < RESTART_MIN_INTERVAL => {
                    return None;
                }
                _ => *last = Some(now),
            }
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
            .field("seen", &lock(&self.seen).len())
            .field("restart_sent", &lock(&self.last_restart).is_some())
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

    use claudeclaw_chat::{MessageRef, NotifyError};
    use futures_util::future::BoxFuture;
    use tokio::sync::broadcast::error::TryRecvError;

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

    fn notice(kind: NoticeKind, text: &str) -> Notice {
        Notice::new(kind, "title", text)
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

    #[tokio::test]
    async fn notices_fan_out_to_every_notifier_and_the_bus() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let notices = Notices::new(bus.clone());
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
        let notices = Notices::new(EventBus::new());
        let (failing, _) = RecordingNotifier::new(true);
        let (working, working_seen) = RecordingNotifier::new(false);
        notices.register(failing);
        notices.register(working);

        let delivery = notices.send(notice(NoticeKind::DiskLow, "low disk")).await;
        assert_eq!(delivery, Delivery { sent: 1, failed: 1 });
        assert_eq!(working_seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn identical_invalid_job_notices_are_sent_once() {
        let notices = Notices::new(EventBus::new());
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

    #[tokio::test(start_paused = true)]
    async fn a_restart_notice_is_sent_at_most_once_per_ten_minutes() {
        let notices = Notices::new(EventBus::new());
        let (notifier, seen) = RecordingNotifier::new(false);
        notices.register(notifier);
        let restart = || notice(NoticeKind::WorkerRestarted, "restarted after a hang");

        assert!(notices.send_restart(restart()).await.is_some());
        assert!(notices.send_restart(restart()).await.is_none());
        tokio::time::advance(RESTART_MIN_INTERVAL).await;
        assert!(notices.send_restart(restart()).await.is_some());
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn kinds_without_a_wire_value_still_reach_chat_owners() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let notices = Notices::new(bus.clone());
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
