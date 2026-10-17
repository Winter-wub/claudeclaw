//! The outbound (REST) side the gateway itself needs: the transport calls behind
//! [`AdapterControl`] (channel overview, Hire, Fire), the owner [`Notifier`], and the small
//! HTTP wrapper they share. A port of the REST half of the TS adapter
//! (`src/commands/discord.ts:219-336`, `:723-806`, `:1150-1213`).
//!
//! The rendering half of the outbound side — the streaming preview, tool reactions, 2000-char
//! splitting, image upload and `[react:x]` — is the other worker's [`ReplySink`](crate::seams::ReplySink);
//! this module only owns the calls `core` makes directly, outside a Turn: the Management API's
//! Discord routes (§6 [`AdapterControl`]) and owner notifications (§6 [`Notifier`]).

use std::sync::Arc;

use claudeclaw_chat::{
    AdapterControl, AdapterError, AdapterOverview, ChannelInfo, ChannelKind, FireDone, GuildInfo,
    MessageRef, Notifier, NotifyError, OwnerNotice, ThreadInfo, UserInfo, reply_chunks,
};
use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};
use twilight_http::Client;
use twilight_http::request::channel::reaction::RequestReactionType;
use twilight_model::application::command::Command;
use twilight_model::channel::ChannelType;
use twilight_model::channel::message::MessageFlags;
use twilight_model::channel::thread::AutoArchiveDuration;
use twilight_model::http::interaction::{
    InteractionResponse, InteractionResponseData, InteractionResponseType,
};
use twilight_model::id::Id;
use twilight_model::id::marker::{
    ApplicationMarker, ChannelMarker, InteractionMarker, MessageMarker, UserMarker,
};

use crate::gateway::Shared;
use crate::seams::DiscordEndpoints;

/// Discord's per-message limit (`DISCORD_MAX_MESSAGE_LEN`, `src/commands/discord.ts:261`).
pub const DISCORD_MAX_MESSAGE_LEN: usize = 2000;

/// The reaction shown while a Turn is worked on (`THINKING_REACTION`,
/// `src/commands/discord.ts:36`). Also the first of the reactions [`DiscordNotifier::interrupted`]
/// clears.
pub const THINKING_REACTION: &str = "\u{1F440}";

/// Every tool-category reaction the TS adapter can add (`toolReactionEmoji`,
/// `src/commands/discord.ts:39-65`), so [`DiscordNotifier::interrupted`] can clear whichever
/// the [`ReplySink`](crate::seams::ReplySink) left behind (#29).
pub const TOOL_REACTIONS: [&str; 8] = [
    "\u{1F50C}",         // mcp__
    "\u{1F4D6}",         // Read / Grep / Glob / LS
    "\u{270F}\u{FE0F}",  // Edit / Write / …
    "\u{1F4BB}",         // Bash
    "\u{1F310}",         // WebFetch / WebSearch
    "\u{1F916}",         // Agent / Task
    "\u{1F9E7}",         // Skill
    "\u{1F6E0}\u{FE0F}", // default
];

/// The DM the owner gets when a Turn was interrupted (`Notifier::interrupted`, #9). The same
/// text `core` uses for a running turn's outcome, so a chat and a job read alike.
const INTERRUPTED_REPLY: &str = "The run was interrupted; please resend your message.";

/// A transport call failed.
fn transport(error: impl std::error::Error + Send + Sync + 'static) -> AdapterError {
    AdapterError::Transport(Box::new(error))
}

/// The host and scheme [`ClientBuilder::proxy`](twilight_http::ClientBuilder::proxy) wants for
/// `DiscordEndpoints::rest` (`proxy(host, use_http)` builds `{http|https}://host/api/v10/…`).
/// `None` means Discord's own `https://discord.com`.
fn proxy_for(rest: &Option<String>) -> Option<(String, bool)> {
    let rest = rest.as_deref()?;
    let (scheme, remainder) = match rest.split_once("://") {
        Some(("http", rest)) => (true, rest),
        Some(("https", rest)) => (false, rest),
        Some((_, rest)) => (false, rest),
        None => (false, rest),
    };
    // Keep only the authority; twilight appends `/api/v10/…` itself.
    let host = remainder.split('/').next().unwrap_or(remainder);
    if host.is_empty() || host == "discord.com" {
        return None;
    }
    Some((host.to_owned(), scheme))
}

/// The thin HTTP wrapper the gateway's REST calls share. Built with the effective token and
/// `DiscordEndpoints.rest` (§4, #18); rebuilt when the token changes on hot reload (§7).
pub(crate) struct TwilightRest {
    http: Client,
}

impl std::fmt::Debug for TwilightRest {
    /// Content-free: the token lives inside [`Client`] and must never be formatted.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwilightRest").finish_non_exhaustive()
    }
}

impl TwilightRest {
    /// A client for `token`, aimed at `endpoints.rest` when it is set.
    pub(crate) fn new(token: &str, endpoints: &DiscordEndpoints) -> Self {
        let mut builder = Client::builder().token(token.to_owned());
        if let Some((host, use_http)) = proxy_for(&endpoints.rest) {
            builder = builder.proxy(host, use_http);
        }
        Self {
            http: builder.build(),
        }
    }

    /// The application id is only known after READY; a call before then can't answer an
    /// interaction.
    fn interaction_client(
        &self,
        application: Id<ApplicationMarker>,
    ) -> twilight_http::client::InteractionClient<'_> {
        self.http.interaction(application)
    }

    /// `POST /channels/{id}/threads` (`src/commands/discord.ts:1157-1166`): a public thread
    /// under `channel`, auto-archiving after 3 days.
    pub(crate) async fn create_thread(
        &self,
        channel: Id<ChannelMarker>,
        name: &str,
    ) -> Result<twilight_model::channel::Channel, AdapterError> {
        let response = self
            .http
            .create_thread(channel, name, ChannelType::PublicThread)
            .auto_archive_duration(AutoArchiveDuration::ThreeDays)
            .await
            .map_err(transport)?;
        response.model().await.map_err(transport)
    }

    /// `DELETE /channels/{id}` (`src/commands/discord.ts:1200`). A thread already gone is a
    /// success with `false`, not an error.
    pub(crate) async fn delete_channel(
        &self,
        channel: Id<ChannelMarker>,
    ) -> Result<bool, AdapterError> {
        match self.http.delete_channel(channel).await {
            Ok(_) => Ok(true),
            Err(error) if is_unknown_channel(&error) => Ok(false),
            Err(error) => Err(transport(error)),
        }
    }

    /// `GET /channels/{id}` (`src/commands/discord.ts:515`, `:972`, `:1189`).
    pub(crate) async fn channel(
        &self,
        channel: Id<ChannelMarker>,
    ) -> Result<twilight_model::channel::Channel, AdapterError> {
        let response = self.http.channel(channel).await.map_err(transport)?;
        response.model().await.map_err(transport)
    }

    /// `POST /channels/{id}/messages` with one already-split chunk
    /// (`sendMessage`, `src/commands/discord.ts:273-289`).
    pub(crate) async fn send_message(
        &self,
        channel: Id<ChannelMarker>,
        content: &str,
    ) -> Result<(), AdapterError> {
        for chunk in reply_chunks(content, DISCORD_MAX_MESSAGE_LEN) {
            self.http
                .create_message(channel)
                .content(&chunk)
                .await
                .map_err(transport)?;
        }
        Ok(())
    }

    /// `POST /channels/{id}/messages` replying to `origin` (`sendMessage` + `message_reference`).
    pub(crate) async fn reply_to_message(
        &self,
        channel: Id<ChannelMarker>,
        origin: Id<MessageMarker>,
        content: &str,
    ) -> Result<(), AdapterError> {
        self.http
            .create_message(channel)
            .reply(origin)
            .content(content)
            .await
            .map_err(transport)?;
        Ok(())
    }

    /// `PUT /users/@me/channels` (`sendMessageToUser`, `src/commands/discord.ts:291-304`).
    pub(crate) async fn create_dm_channel(
        &self,
        user: Id<UserMarker>,
    ) -> Result<Id<ChannelMarker>, AdapterError> {
        let response = self
            .http
            .create_private_channel(user)
            .await
            .map_err(transport)?;
        Ok(response.model().await.map_err(transport)?.id)
    }

    /// `PUT /channels/{id}/thread-members/@me` (`src/commands/discord.ts:1875`, `:535`): the bot
    /// joins a thread so Discord delivers its messages.
    pub(crate) async fn join_thread(&self, thread: Id<ChannelMarker>) -> Result<(), AdapterError> {
        self.http.join_thread(thread).await.map_err(transport)?;
        Ok(())
    }

    /// `PUT /applications/{id}/commands` (`registerSlashCommands`,
    /// `src/commands/discord.ts:723-806`).
    pub(crate) async fn set_global_commands(
        &self,
        application: Id<ApplicationMarker>,
        commands: &[Command],
    ) -> Result<(), AdapterError> {
        self.http
            .interaction(application)
            .set_global_commands(commands)
            .await
            .map_err(transport)?;
        Ok(())
    }

    /// `POST /interactions/{id}/{token}/callback` with type 4 (`respondToInteraction`,
    /// `src/commands/discord.ts:810-825`). `ephemeral` sets the `64` flag.
    pub(crate) async fn respond_to_interaction(
        &self,
        application: Id<ApplicationMarker>,
        interaction: Id<InteractionMarker>,
        token: &str,
        content: &str,
        ephemeral: bool,
    ) -> Result<(), AdapterError> {
        let response = InteractionResponse {
            kind: InteractionResponseType::ChannelMessageWithSource,
            data: Some(InteractionResponseData {
                content: Some(content.to_owned()),
                flags: ephemeral.then_some(MessageFlags::EPHEMERAL),
                ..Default::default()
            }),
        };
        self.interaction_client(application)
            .create_response(interaction, token, &response)
            .await
            .map_err(transport)?;
        Ok(())
    }

    /// `PATCH /webhooks/{application}/{token}/messages/@original` (`src/commands/discord.ts:1421`):
    /// edit the deferred reply to an interaction in place.
    pub(crate) async fn update_interaction_response(
        &self,
        application: Id<ApplicationMarker>,
        token: &str,
        content: &str,
    ) -> Result<(), AdapterError> {
        self.interaction_client(application)
            .update_response(token)
            .content(Some(content))
            .await
            .map_err(transport)?;
        Ok(())
    }

    /// `PUT …/reactions/{emoji}/@me` (`sendReaction`, `src/commands/discord.ts:306-320`).
    /// Best-effort, like TS.
    pub(crate) async fn add_own_reaction(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
        emoji: &str,
    ) {
        let reaction = RequestReactionType::Unicode { name: emoji };
        let _ = self.http.create_reaction(channel, message, &reaction).await;
    }

    /// `DELETE …/reactions/{emoji}/@me` (`removeReaction`, `src/commands/discord.ts:322-336`).
    /// Best-effort: a reaction that is already gone is not an error.
    pub(crate) async fn remove_own_reaction(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
        emoji: &str,
    ) {
        let reaction = RequestReactionType::Unicode { name: emoji };
        let _ = self
            .http
            .delete_current_user_reaction(channel, message, &reaction)
            .await;
    }
}

/// Whether a twilight error is Discord's `10003 Unknown Channel`, so a Fire can report the
/// thread as already gone (`src/commands/discord.ts:1200`).
fn is_unknown_channel(error: &twilight_http::Error) -> bool {
    matches!(
        error.kind(),
        twilight_http::error::ErrorType::Response { status, .. } if status.get() == 404
    )
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

    fn rest(&self) -> Result<Arc<TwilightRest>, AdapterError> {
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

/// The adapter's [`Notifier`] (§6): owner notices by DM, and the interrupted reply (#9, #29).
pub struct DiscordNotifier {
    shared: Arc<Shared>,
}

impl DiscordNotifier {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl Notifier for DiscordNotifier {
    fn notify_owners(&self, msg: OwnerNotice) -> BoxFuture<'_, Result<(), NotifyError>> {
        Box::pin(async move {
            let owners = self.shared.settings().allowed_user_ids;
            if owners.is_empty() {
                return Err(NotifyError::NoOwners);
            }
            let rest = self.shared.rest().map_err(|_| NotifyError::NotConnected)?;
            let text = format!("[{}]\n{}", msg.kind, msg.text);
            let mut sent = false;
            for owner in owners {
                let Ok(user) = owner.parse::<u64>().map(Id::new) else {
                    continue;
                };
                let channel = rest
                    .create_dm_channel(user)
                    .await
                    .map_err(|error| NotifyError::Transport(boxed(error)))?;
                rest.send_message(channel, &text)
                    .await
                    .map_err(|error| NotifyError::Transport(boxed(error)))?;
                sent = true;
            }
            if sent {
                Ok(())
            } else {
                Err(NotifyError::NoOwners)
            }
        })
    }

    fn interrupted(&self, origin: MessageRef) -> BoxFuture<'_, Result<(), NotifyError>> {
        Box::pin(async move {
            let rest = self.shared.rest().map_err(|_| NotifyError::NotConnected)?;
            let channel = parse_channel(&origin.channel).ok_or(NotifyError::NotConnected)?;
            let message = origin
                .message
                .parse::<u64>()
                .map(Id::new)
                .map_err(|_| NotifyError::NotConnected)?;
            // The reply first, then clear the adapter's own stuck reactions (#29). A message
            // that no longer exists is not an error.
            rest.reply_to_message(channel, message, INTERRUPTED_REPLY)
                .await
                .map_err(|error| NotifyError::Transport(boxed(error)))?;
            for emoji in std::iter::once(THINKING_REACTION).chain(TOOL_REACTIONS) {
                rest.remove_own_reaction(channel, message, emoji).await;
            }
            Ok(())
        })
    }
}

/// Unwrap an [`AdapterError`] into the boxed source `NotifyError::Transport` wants.
fn boxed(error: AdapterError) -> Box<dyn std::error::Error + Send + Sync + 'static> {
    match error {
        AdapterError::Transport(source) => source,
        other => Box::new(other),
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

/// The Watchdog's REST-only owner DM (`notify_owner`, architecture §4 `claudeclaw-discord`):
/// Give-up and Held-start notices, sent with no running gateway. One DM per user id; ids that
/// aren't snowflakes are skipped.
pub async fn notify_owner(
    token: &SecretString,
    user_ids: &[String],
    text: &str,
) -> Result<(), AdapterError> {
    let rest = TwilightRest::new(token.expose_secret(), &DiscordEndpoints::default());
    let mut sent = false;
    for owner in user_ids {
        let Ok(user) = owner.parse::<u64>().map(Id::new) else {
            continue;
        };
        let channel = rest.create_dm_channel(user).await?;
        rest.send_message(channel, text).await?;
        sent = true;
    }
    if sent {
        Ok(())
    } else {
        Err(AdapterError::Settings("no owner to notify".to_owned()))
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
