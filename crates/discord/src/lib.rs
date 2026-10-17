//! The Discord chat adapter, on twilight 0.17 (architecture §4 `claudeclaw-discord`). It depends
//! only on `chat`.
//!
//! - [`seams`]: the traits between the gateway side and the outbound side, and
//!   [`seams::DiscordEndpoints`].

pub mod seams;

pub use seams::{AttachmentFetcher, DiscordEndpoints, FetchError, ReplySink, ReplyTarget};
