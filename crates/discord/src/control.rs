//! The transport calls behind the Management API's Discord routes (architecture §6,
//! management-api §2.7): the channel overview, Hire and Fire. A port of the REST half of the TS
//! adapter (`src/commands/discord.ts:219-336`, `:723-806`, `:1150-1213`).
//!
//! [`DiscordControl`] reads the gateway's [`Shared`] cache for the overview and issues its
//! thread calls through the outbound [`Rest`] client, so there is one REST implementation (and one
//! 429 retry, and one owner DM fan-out) shared with [`crate::notifier`] and [`crate::reply`].
//! `core` owns the session and `agents/` work around these transport calls.

use std::sync::Arc;

use claudeclaw_chat::{
    AdapterControl, AdapterError, AdapterOverview, ChannelInfo, ChannelKind, FireDone, GuildInfo,
    ThreadInfo, UserInfo,
};
use futures_util::future::BoxFuture;
use twilight_model::channel::ChannelType;
use twilight_model::id::Id;
use twilight_model::id::marker::ChannelMarker;

use crate::gateway::Shared;
use crate::rest::{Rest, RestError};

/// A REST failure behind an [`AdapterError`], for the Management API's Discord routes.
impl From<RestError> for AdapterError {
    fn from(error: RestError) -> Self {
        AdapterError::Transport(Box::new(error))
    }
}

/// The Management API's Discord calls (§6, management-api §2.7): the transport side of the
/// channel overview, Hire and Fire. Core owns the session and `agents/` work around them.
pub struct DiscordControl {
    shared: Arc<Shared>,
}

impl DiscordControl {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    fn rest(&self) -> Result<Arc<Rest>, AdapterError> {
        self.shared.rest()
    }
}

impl AdapterControl for DiscordControl {
    fn overview(&self) -> BoxFuture<'_, Result<AdapterOverview, AdapterError>> {
        Box::pin(async move {
            let connection = self.shared.connection();
            let settings = self.shared.settings();
            let overview = AdapterOverview {
                state: connection.state,
                since: connection.since,
                bot: self.shared.bot(),
                guilds: self.shared.guild_infos(),
                channels: self.shared.listened_channels(),
                threads: self.shared.thread_infos(),
                users: self.shared.allow_list_users(&settings),
            };
            Ok(overview)
        })
    }

    fn available_channels(&self) -> BoxFuture<'_, Result<Vec<ChannelInfo>, AdapterError>> {
        Box::pin(async move { Ok(self.shared.available_channels()) })
    }

    fn hire(
        &self,
        channel: String,
        names: Vec<String>,
    ) -> BoxFuture<'_, Result<Vec<ThreadInfo>, AdapterError>> {
        Box::pin(async move {
            let parent = parse_channel(&channel).ok_or(AdapterError::NotFound {
                what: "channel",
                id: channel.clone(),
            })?;
            let rest = self.rest()?;
            let mut created = Vec::with_capacity(names.len());
            for name in names {
                let thread = rest.create_thread(parent, &name).await?;
                self.shared.upsert_thread(
                    thread.id,
                    parent,
                    Some(thread.name.as_deref().unwrap_or(&name)),
                );
                // TS's confirmation post (`src/commands/discord.ts:1170`).
                rest.send_message(
                    thread.id,
                    &format!(
                        "\u{1F9F5} Thread **{name}** created with independent session. Start chatting!"
                    ),
                )
                .await?;
                created.push(ThreadInfo {
                    id: thread.id.to_string(),
                    name,
                    parent_id: parent.to_string(),
                });
            }
            Ok(created)
        })
    }

    fn fire(&self, thread: String) -> BoxFuture<'_, Result<FireDone, AdapterError>> {
        Box::pin(async move {
            let id = parse_channel(&thread).ok_or(AdapterError::NotFound {
                what: "thread",
                id: thread.clone(),
            })?;
            let rest = self.rest()?;
            let deleted = rest.delete_channel(id).await?;
            self.shared.remove_thread(id);
            Ok(FireDone {
                thread_id: thread,
                deleted,
            })
        })
    }
}

/// Parse a snowflake string into a channel id.
fn parse_channel(id: &str) -> Option<Id<ChannelMarker>> {
    id.parse::<u64>().ok().map(Id::new)
}

/// The channel-kind mapping for the Management API (§2.7 `ChannelKind`). `None` for channel
/// types the adapter never listens in.
pub(crate) fn channel_kind(kind: ChannelType) -> Option<ChannelKind> {
    match kind {
        ChannelType::GuildText => Some(ChannelKind::Text),
        ChannelType::GuildAnnouncement => Some(ChannelKind::Announcement),
        ChannelType::GuildForum | ChannelType::GuildMedia => Some(ChannelKind::Forum),
        ChannelType::GuildVoice | ChannelType::GuildStageVoice => Some(ChannelKind::Voice),
        _ => None,
    }
}

/// The users a guild may see, for the Management API's `DiscordUser` list.
pub(crate) fn guild_info(id: &str, name: &str) -> GuildInfo {
    GuildInfo {
        id: id.to_owned(),
        name: name.to_owned(),
    }
}

/// A `DiscordUser` with no cached name.
pub(crate) fn user_info(id: &str, name: Option<String>) -> UserInfo {
    UserInfo {
        id: id.to_owned(),
        name,
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
