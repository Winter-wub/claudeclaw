//! The Discord chat adapter, on twilight 0.17 (architecture §4 `claudeclaw-discord`). It depends
//! only on `chat`.
//!
//! - [`triggers`]: when a message becomes a Turn, and who may send one.
//! - [`turn`]: building a [`chat::Turn`](claudeclaw_chat::Turn) from a Discord message.
//! - [`seams`]: the traits between the gateway side and the outbound side, and
//!   [`seams::DiscordEndpoints`].

pub mod seams;
pub mod triggers;
pub mod turn;

pub use seams::{AttachmentFetcher, DiscordEndpoints, FetchError, ReplySink, ReplyTarget};
