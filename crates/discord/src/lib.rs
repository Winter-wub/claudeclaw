//! The Discord chat adapter, on twilight 0.17 (architecture §4 `claudeclaw-discord`). It depends
//! only on `chat`.
//!
//! - [`attachments`]: the inbound attachment fetcher ([`attachments::DiscordAttachmentFetcher`]).
//! - [`notifier`]: the [`Notifier`](claudeclaw_chat::Notifier) that DMs configured owners.
//! - [`reply`]: the outbound reply sink ([`reply::DiscordReplySink`]) with streaming previews.
//! - [`rest`]: the twilight REST client, its retry policy and the owner DM fan-out.
//! - [`seams`]: the traits between the gateway side and the outbound side, and
//!   [`seams::DiscordEndpoints`].

pub mod attachments;
pub mod notifier;
pub mod reply;
pub mod rest;
pub mod seams;

pub use seams::{AttachmentFetcher, DiscordEndpoints, FetchError, ReplySink, ReplyTarget};
