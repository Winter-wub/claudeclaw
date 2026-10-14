//! What every Chat adapter implements (architecture §6, #21).

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::SystemTime;

use futures_util::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use crate::{AdapterError, ChatHost, NotifyError, Transport};

/// A Chat adapter: one transport's connection to the Daemon (architecture §6). The Worker
/// registers its [`Notifier`] and [`AdapterControl`] with `core`, then runs it as one task.
pub trait Adapter: Send + 'static {
    /// The transport this adapter connects.
    const TRANSPORT: Transport;

    /// Where `core` sends owner notifications for this transport.
    fn notifier(&self) -> Arc<dyn Notifier>;

    /// The transport calls behind the Management API's channel overview and Hire/Fire.
    fn control(&self) -> Arc<dyn AdapterControl>;

    /// Connects and serves until `shutdown` is cancelled. Implement it as an `async fn`; the
    /// future must be `Send` so the Worker can spawn it.
    fn run(
        self,
        host: Arc<dyn ChatHost>,
        shutdown: CancellationToken,
    ) -> impl Future<Output = Result<(), AdapterError>> + Send;
}

/// Sends owner notifications through one transport (architecture §6). Implemented by each
/// adapter and registered with `core`'s `notify`.
pub trait Notifier: Send + Sync {
    /// Sends `msg` to the transport's owners, such as Discord's allowed users by DM.
    fn notify_owners(&self, msg: OwnerNotice) -> BoxFuture<'_, Result<(), NotifyError>>;
}

/// The transport calls behind the Management API's Discord routes (architecture §6, #21,
/// management-api §2.7). Implemented by each adapter and registered beside its [`Notifier`].
/// `core` adds what it owns: triggers, Launchers, sessions and the `agents/` work of Hire/Fire.
pub trait AdapterControl: Send + Sync {
    /// The connection and the names the transport's cache knows.
    fn overview(&self) -> BoxFuture<'_, Result<AdapterOverview, AdapterError>>;
    /// Every channel the bot could listen in.
    fn available_channels(&self) -> BoxFuture<'_, Result<Vec<ChannelInfo>, AdapterError>>;
    /// Creates one thread per name under `channel` and returns them in order (Hire).
    fn hire(
        &self,
        channel: String,
        names: Vec<String>,
    ) -> BoxFuture<'_, Result<Vec<ThreadInfo>, AdapterError>>;
    /// Deletes `thread` on the transport (Fire).
    fn fire(&self, thread: String) -> BoxFuture<'_, Result<FireDone, AdapterError>>;
}

/// A notice for the owner, from `core`'s `notify` (§4 `claudeclaw-core`): a job result, a
/// Launcher refusal, a breaker opening, and so on. `Debug` prints only sizes.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnerNotice {
    /// What the notice is about, in `snake_case`, such as `job_result` or `disk_low`.
    pub kind: String,
    /// The text to send. It may be longer than one message; the adapter splits it.
    pub text: String,
}

impl fmt::Debug for OwnerNotice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnerNotice")
            .field("kind", &self.kind)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

/// A Chat adapter's connection (the values of management-api §2.7's `AdapterState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterState {
    /// Starting, or logging in for the first time.
    Connecting,
    Connected,
    /// The connection dropped; the adapter is retrying by itself.
    Reconnecting,
    /// Stopped after an error it won't retry, such as a rejected token.
    Disconnected,
    /// Not configured, or turned off in settings.
    Disabled,
}

/// The adapter's side of the channel overview (#21, management-api §2.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterOverview {
    pub state: AdapterState,
    /// When the adapter entered `state`.
    pub since: SystemTime,
    /// The bot's own user; `None` until the transport first reports it.
    pub bot: Option<UserInfo>,
    /// Discord guilds or Slack workspaces the bot is in; empty on Telegram.
    pub guilds: Vec<GuildInfo>,
    /// The channels the bot listens in, with their names.
    pub channels: Vec<ChannelInfo>,
    /// The threads the cache knows under those channels.
    pub threads: Vec<ThreadInfo>,
    /// The users on the adapter's allow-lists, named where the cache knows them.
    pub users: Vec<UserInfo>,
}

/// A guild or workspace (#21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildInfo {
    pub id: String,
    pub name: String,
}

/// A transport user (#21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInfo {
    pub id: String,
    /// `None` when the cache doesn't know the user.
    pub name: Option<String>,
}

/// A channel (#21, management-api §2.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    /// The guild or workspace it is in; `None` on transports without one.
    pub guild_id: Option<String>,
    pub kind: ChannelKind,
}

/// The kind of a channel the bot can listen in (the values of management-api §2.7's
/// `ChannelKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    Text,
    Announcement,
    /// A forum; its posts are threads under it.
    Forum,
    /// A voice channel's text chat.
    Voice,
}

/// A thread under a channel (#21, management-api §2.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadInfo {
    pub id: String,
    pub name: String,
    /// The channel it is under.
    pub parent_id: String,
}

/// The transport side of a Fire (#21, management-api §2.7 `FireResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireDone {
    pub thread_id: String,
    /// `false` when the thread was already gone on the transport.
    pub deleted: bool,
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt as _;

    use super::*;
    use crate::{
        ChatScope, LauncherControl, RunHandle, SessionControl, SettingsWatch, Turn, UtilityHandle,
    };

    /// A do-nothing adapter: proves the traits are implementable, `dyn`-compatible where §2
    /// needs it, and that an `async fn run` gives a `Send` future.
    struct FakeAdapter;

    struct FakeSide;

    impl Notifier for FakeSide {
        fn notify_owners(&self, _msg: OwnerNotice) -> BoxFuture<'_, Result<(), NotifyError>> {
            Box::pin(async { Err(NotifyError::NoOwners) })
        }
    }

    impl AdapterControl for FakeSide {
        fn overview(&self) -> BoxFuture<'_, Result<AdapterOverview, AdapterError>> {
            Box::pin(async {
                Ok(AdapterOverview {
                    state: AdapterState::Disabled,
                    since: SystemTime::UNIX_EPOCH,
                    bot: None,
                    guilds: vec![],
                    channels: vec![],
                    threads: vec![],
                    users: vec![],
                })
            })
        }
        fn available_channels(&self) -> BoxFuture<'_, Result<Vec<ChannelInfo>, AdapterError>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn hire(
            &self,
            channel: String,
            names: Vec<String>,
        ) -> BoxFuture<'_, Result<Vec<ThreadInfo>, AdapterError>> {
            Box::pin(async move {
                Ok(names
                    .into_iter()
                    .enumerate()
                    .map(|(i, name)| ThreadInfo {
                        id: i.to_string(),
                        name,
                        parent_id: channel.clone(),
                    })
                    .collect())
            })
        }
        fn fire(&self, thread: String) -> BoxFuture<'_, Result<FireDone, AdapterError>> {
            Box::pin(async move {
                Err(AdapterError::NotFound {
                    what: "thread",
                    id: thread,
                })
            })
        }
    }

    impl Adapter for FakeAdapter {
        const TRANSPORT: Transport = Transport::Discord;
        fn notifier(&self) -> Arc<dyn Notifier> {
            Arc::new(FakeSide)
        }
        fn control(&self) -> Arc<dyn AdapterControl> {
            Arc::new(FakeSide)
        }
        async fn run(
            self,
            host: Arc<dyn ChatHost>,
            shutdown: CancellationToken,
        ) -> Result<(), AdapterError> {
            let _ = host.converse(Turn {
                scope: ChatScope::Direct {
                    transport: Self::TRANSPORT,
                },
                author: crate::Author {
                    display_name: "a".into(),
                },
                text: "hi".into(),
                context_lines: vec![],
                attachments: vec![],
                agent_dir: None,
            });
            shutdown.cancelled().await;
            Ok(())
        }
    }

    struct FakeHost;

    impl ChatHost for FakeHost {
        fn converse(&self, _turn: Turn) -> RunHandle {
            RunHandle::channel(1).1
        }
        fn utility(&self, _prompt: String) -> UtilityHandle {
            UtilityHandle {
                outcome: tokio::sync::oneshot::channel().1,
            }
        }
        fn sessions(&self) -> &dyn SessionControl {
            unimplemented!()
        }
        fn launchers(&self) -> &dyn LauncherControl {
            unimplemented!()
        }
        fn settings(&self) -> &dyn SettingsWatch {
            unimplemented!()
        }
    }

    // `dyn` handles of every control trait exist.
    #[allow(dead_code)]
    fn dyn_compatible(
        _: &dyn SessionControl,
        _: &dyn LauncherControl,
        _: &dyn SettingsWatch,
        _: &dyn Notifier,
        _: &dyn AdapterControl,
        _: Arc<dyn ChatHost>,
    ) {
    }

    fn assert_send<T: Send>(_: &T) {}

    #[test]
    fn run_future_is_send() {
        let shutdown = CancellationToken::new();
        let future = FakeAdapter.run(Arc::new(FakeHost), shutdown);
        assert_send(&future);
        drop(future);
    }

    #[test]
    fn control_through_dyn() {
        let control: Arc<dyn AdapterControl> = FakeAdapter.control();
        let threads = control
            .hire("7".into(), vec!["one".into(), "two".into()])
            .now_or_never()
            .unwrap()
            .unwrap();
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[1].name, "two");
        assert_eq!(threads[1].parent_id, "7");
        let fired = control.fire("9".into()).now_or_never().unwrap();
        assert!(matches!(fired, Err(AdapterError::NotFound { .. })));
        let notified = FakeAdapter
            .notifier()
            .notify_owners(OwnerNotice {
                kind: "disk_low".into(),
                text: "x".into(),
            })
            .now_or_never()
            .unwrap();
        assert!(matches!(notified, Err(NotifyError::NoOwners)));
    }

    #[test]
    fn notice_debug_is_content_free() {
        let notice = OwnerNotice {
            kind: "job_result".into(),
            text: "secret job output".into(),
        };
        let got = format!("{notice:?}");
        assert!(!got.contains("secret"), "{got}");
        assert!(got.contains("job_result"), "{got}");
    }
}
