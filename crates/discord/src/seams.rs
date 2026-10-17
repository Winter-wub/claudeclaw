//! The seams between the adapter's gateway side and its outbound (REST) side, so each can be
//! built and tested alone (architecture §4 `claudeclaw-discord`).
//!
//! The gateway side turns a message into a [`chat::Turn`](claudeclaw_chat::Turn), calls
//! `ChatHost::converse`, and hands the [`RunHandle`] to a [`ReplySink`]. It fetches attachments
//! through an [`AttachmentFetcher`] before building the Turn. The outbound side implements both.

use std::fmt;

use claudeclaw_chat::{LocalFile, RunHandle};
use futures_util::future::BoxFuture;
use twilight_model::channel::Attachment;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, MessageMarker};

/// Where the adapter connects. Not a settings key: the Parity suite's replay redirects it (#18).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordEndpoints {
    /// The gateway URL; `None` uses the one Discord's `GET /gateway/bot` returns.
    pub gateway: Option<String>,
    /// The REST API base URL; `None` uses `https://discord.com`.
    pub rest: Option<String>,
}

/// Where a Turn's reply goes: the channel or thread, and the message it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyTarget {
    pub channel: Id<ChannelMarker>,
    /// The user's message: the reply references it and the progress reactions go on it.
    pub origin: Id<MessageMarker>,
}

/// The outbound side of one Turn: the streaming preview, tool-category reactions, the final
/// reply split at 2000 chars, images from the outbox, and clearing the bot's own reactions.
pub trait ReplySink: Send + Sync {
    /// Drives `handle` to its end. Never fails the caller: delivery errors are logged.
    fn deliver(&self, target: ReplyTarget, handle: RunHandle) -> BoxFuture<'_, ()>;
}

/// Downloads one attachment into `inbox/discord/` (25 MB cap, refused below the disk floor, #1,
/// #44).
pub trait AttachmentFetcher: Send + Sync {
    fn fetch<'a>(
        &'a self,
        attachment: &'a Attachment,
    ) -> BoxFuture<'a, Result<LocalFile, FetchError>>;
}

/// Why an attachment wasn't fetched. The Turn still runs, without the file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    #[error("the attachment is over the 25 MB cap")]
    TooLarge,
    #[error("free disk space is below the floor")]
    DiskLow,
    #[error("the download failed")]
    Download,
    #[error("the file could not be written")]
    Write,
}

impl fmt::Display for ReplyTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.channel, self.origin)
    }
}
