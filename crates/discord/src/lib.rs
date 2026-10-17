//! The Discord chat adapter, on twilight 0.17 (architecture §4 `claudeclaw-discord`). It depends
//! only on `chat`.
//!
//! - [`adapter`]: [`DiscordAdapter`], the value `core` registers and runs.
//! - [`gateway`]: the twilight gateway loop, the cache both sides read, and the message handler.
//! - [`commands`]: the slash commands and the interaction handler.
//! - [`triggers`]: when a message becomes a Turn, and who may send one.
//! - [`turn`]: building a [`chat::Turn`](claudeclaw_chat::Turn) from a Discord message.
//! - [`control`]: the outbound REST side behind the Management API's Discord routes, and the
//!   owner notifier.
//! - [`seams`]: the traits between the gateway side and the outbound side, and
//!   [`seams::DiscordEndpoints`].

pub mod adapter;
pub mod commands;
pub mod control;
pub mod gateway;
pub mod seams;
pub mod triggers;
pub mod turn;

pub use adapter::DiscordAdapter;
pub use control::notify_owner;
pub use seams::{AttachmentFetcher, DiscordEndpoints, FetchError, ReplySink, ReplyTarget};
