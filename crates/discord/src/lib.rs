//! The Discord chat adapter, on twilight 0.17 (architecture §4 `claudeclaw-discord`). It depends
//! only on `chat`.
//!
//! - [`adapter`]: [`DiscordAdapter`], the value `core` registers and runs.
//! - [`attachments`]: the inbound attachment fetcher ([`attachments::DiscordAttachmentFetcher`]).
//! - [`commands`]: the slash commands and the interaction handler.
//! - [`control`]: the transport calls behind the Management API's Discord routes (overview,
//!   Hire, Fire), on top of [`rest::Rest`].
//! - [`gateway`]: the twilight gateway loop, the cache both sides read, and the message handler.
//! - [`notifier`]: the [`Notifier`](claudeclaw_chat::Notifier) that DMs configured owners.
//! - [`reply`]: the outbound reply sink ([`reply::DiscordReplySink`]) with streaming previews.
//! - [`rest`]: the twilight REST client, its retry policy and the owner DM fan-out.
//! - [`seams`]: the traits between the gateway side and the outbound side, and
//!   [`seams::DiscordEndpoints`].
//! - [`triggers`]: when a message becomes a Turn, and who may send one.
//! - [`turn`]: building a [`chat::Turn`](claudeclaw_chat::Turn) from a Discord message.

pub mod adapter;
pub mod attachments;
pub mod commands;
pub mod control;
pub mod gateway;
pub mod notifier;
pub mod reply;
pub mod rest;
pub mod seams;
pub mod triggers;
pub mod turn;

pub use adapter::DiscordAdapter;
pub use rest::notify_owner;
pub use seams::{AttachmentFetcher, DiscordEndpoints, FetchError, ReplySink, ReplyTarget};
