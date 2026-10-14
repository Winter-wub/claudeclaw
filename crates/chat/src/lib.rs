//! The chat-adapter seam (architecture §4 `claudeclaw-chat`, §6).
//!
//! Only types and traits, plus the helpers every chat transport needs: message splitting by a
//! size limit ([`split_message`], [`reply_chunks`]) and `[react:x]` directive extraction
//! ([`extract_react_directive`], [`strip_react_directives`]). No tokio runtime and no transport
//! crates; `tokio` is here only for the [`RunHandle`] channels.
//!
//! `core` implements [`ChatHost`] and gives it to every Chat adapter. Each adapter implements
//! [`Adapter`], with a [`Notifier`] and an [`AdapterControl`] that the Worker registers with
//! `core`. `chat` depends on nothing in the workspace (§3.1): adapters see only this crate.

mod adapter;
mod error;
mod host;
mod js;
mod react;
mod run;
mod scope;
mod split;
mod turn;

pub use adapter::{
    Adapter, AdapterControl, AdapterOverview, AdapterState, ChannelInfo, ChannelKind, FireDone,
    GuildInfo, Notifier, OwnerNotice, ThreadInfo, UserInfo,
};
pub use error::{AdapterError, BoxError, ControlError, NotifyError};
pub use host::{
    ChatHost, ContextUsage, LauncherCheck, LauncherControl, LauncherSource, LauncherTest,
    LauncherView, SessionControl, SessionStatus, SettingsWatch, TransportSettings,
};
pub use react::{ReactDirective, extract_react_directive, strip_react_directives};
pub use run::{ChatOutcome, OutcomeKind, RunEvent, RunHandle, RunSender, UtilityHandle};
pub use scope::{ChatScope, Transport};
pub use split::{reply_chunks, split_message};
pub use turn::{Author, LocalFile, Turn};
