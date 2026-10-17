//! When an inbound Discord message becomes a Turn, and who may send one (architecture §4
//! `claudeclaw-discord`). A port of the trigger half of the TS adapter's message handler
//! (`src/commands/discord.ts:951-1003`, `guildTriggerReason` at `:552-574`) and its
//! authorization helper (`isDiscordAuthorized`, `src/allowlist.ts:10-19`).
//!
//! Everything here is a pure function over `twilight-model` values plus a [`DiscordSettings`]
//! slice, so the rules are table-tested with no gateway and no token.

use std::collections::BTreeMap;

use serde::Deserialize;
use twilight_model::channel::Message;
use twilight_model::channel::message::MessageType;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, UserMarker};

/// The `discord` object of `settings.json` as the adapter reads it (TS `DiscordConfig`,
/// `src/config.ts`). `chat` never parses this; the adapter does (§7). Unknown keys are ignored;
/// missing fields take the TS defaults.
///
/// The token is not here: `core` resolves `DISCORD_TOKEN` over `settings.json` and publishes the
/// effective one in [`chat::TransportSettings::token`](claudeclaw_chat::TransportSettings).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DiscordSettings {
    /// Users allowed to talk to the bot anywhere (DMs and every guild channel).
    pub allowed_user_ids: Vec<String>,
    /// Channels where the bot answers every message, no mention needed.
    pub listen_channels: Vec<String>,
    /// Guilds where the bot answers every message in any channel or thread.
    pub listen_guilds: Vec<String>,
    /// Guilds where the bot posts a welcome message on join (empty means silent).
    pub allowed_guilds: Vec<String>,
    /// channel id → friendly name for the channel tag.
    pub channel_names: BTreeMap<String, String>,
    /// channel id → user ids allowed **in that guild channel only** (never DMs).
    pub channel_allowed_user_ids: BTreeMap<String, Vec<String>>,
    /// Absolute path prefixes image uploads may come from.
    pub image_output_roots: Vec<String>,
    /// When true, post a live streaming preview while Claude works.
    pub streaming: bool,
}

impl DiscordSettings {
    /// Parses a transport's `settings.json` object (`TransportSettings::section`, §7). An absent
    /// section is `"{}"`, which parses to the defaults.
    pub fn from_section(section: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(section)
    }

    /// The channel tag the TS handler puts first in the prompt (`src/commands/discord.ts:1231-1232`):
    /// `[Discord Channel: <name>]` in a guild, `[Discord DM]` in a DM. The name falls back to the
    /// channel id, as TS does.
    pub fn channel_tag(&self, is_guild: bool, channel_id: &Id<ChannelMarker>) -> String {
        if is_guild {
            match self.channel_names.get(&channel_id.to_string()) {
                Some(name) => format!("[Discord Channel: {name}]"),
                None => format!("[Discord Channel: {channel_id}]"),
            }
        } else {
            "[Discord DM]".to_owned()
        }
    }
}

/// Why a guild message triggered the bot (TS `guildTriggerReason`, `src/commands/discord.ts:552`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A DM; TS labels it `direct_message` (`src/commands/discord.ts:984`).
    DirectMessage,
    /// The message replies to one of the bot's own messages.
    ReplyToBot,
    /// The bot is in the message's `mentions` array.
    Mention,
    /// The message's content contains `<@bot>` and the mentions array did not carry it.
    MentionInContent,
    /// The message is in a `listenChannels` channel.
    ListenChannel,
    /// The message's guild is in `listenGuilds`.
    ListenGuild,
    /// The message is in a tracked thread whose parent is a `listenChannels` channel.
    ListenChannelThread,
}

/// What the adapter decides about one message (TS `handleMessageCreate`'s early returns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Build and run a Turn.
    Handle {
        trigger: Trigger,
        /// Whether the message came from a guild (its Scope is Channel/Thread, not Direct).
        is_guild: bool,
    },
    /// Drop the message, for one of TS's reasons.
    Ignore(IgnoreReason),
}

/// Why a message was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    /// `message.author.bot` (`src/commands/discord.ts:955`).
    Bot,
    /// A system message type other than Default or Reply (`src/commands/discord.ts:957-959`).
    SystemMessage,
    /// A guild message that matched no trigger rule (`src/commands/discord.ts:985-989`).
    NoTrigger,
    /// The author is not on an allow-list that covers this message
    /// (`src/commands/discord.ts:996-1003`).
    Unauthorized,
}

/// The inputs the trigger rules read besides the message itself.
#[derive(Debug, Clone, Copy)]
pub struct TriggerContext<'a> {
    /// The bot's own user id, learned from READY; `None` until then, which disables the
    /// reply/mention rules exactly as TS does before READY.
    pub bot_user_id: Option<Id<UserMarker>>,
    /// The settings slice.
    pub settings: &'a DiscordSettings,
    /// The parent channel of a tracked thread the message's channel is, if the adapter knows it
    /// (TS `knownThreads.get(channel_id).parentId`, `src/commands/discord.ts:570`).
    pub thread_parent: Option<Id<ChannelMarker>>,
}

/// The trigger for a guild message, or `None` when nothing matches
/// (TS `guildTriggerReason`, `src/commands/discord.ts:552-574`).
pub fn guild_trigger(message: &Message, ctx: &TriggerContext<'_>) -> Option<Trigger> {
    let bot = ctx.bot_user_id;

    // Reply to the bot.
    if let Some(bot) = bot
        && message
            .referenced_message
            .as_ref()
            .is_some_and(|ref_msg| ref_msg.author.id == bot)
    {
        return Some(Trigger::ReplyToBot);
    }

    // Mention via the mentions array.
    if let Some(bot) = bot
        && message.mentions.iter().any(|m| m.id == bot)
    {
        return Some(Trigger::Mention);
    }

    // Mention in content, as a fallback (TS checks `<@id>` only; the array covers `<@!id>`).
    if let Some(bot) = bot
        && message.content.contains(&format!("<@{bot}>"))
    {
        return Some(Trigger::MentionInContent);
    }

    let channel = message.channel_id.to_string();
    if ctx.settings.listen_channels.contains(&channel) {
        return Some(Trigger::ListenChannel);
    }

    if let Some(guild) = message.guild_id
        && ctx.settings.listen_guilds.contains(&guild.to_string())
    {
        return Some(Trigger::ListenGuild);
    }

    // A tracked thread whose parent is a listen channel.
    if let Some(parent) = ctx.thread_parent
        && ctx.settings.listen_channels.contains(&parent.to_string())
    {
        return Some(Trigger::ListenChannelThread);
    }

    None
}

/// Whether `user_id` may talk to the bot (`isDiscordAuthorized`, `src/allowlist.ts:10-19`): the
/// global allow-list grants access everywhere; a channel-scoped entry only grants access to guild
/// messages in that channel, never DMs.
pub fn is_authorized(
    user_id: Option<Id<UserMarker>>,
    is_guild: bool,
    channel_id: Id<ChannelMarker>,
    settings: &DiscordSettings,
) -> bool {
    let Some(user_id) = user_id else {
        return false;
    };
    let user = user_id.to_string();
    if settings.allowed_user_ids.contains(&user) {
        return true;
    }
    if is_guild {
        let channel = channel_id.to_string();
        if settings
            .channel_allowed_user_ids
            .get(&channel)
            .is_some_and(|users| users.contains(&user))
        {
            return true;
        }
    }
    false
}

/// TS `isAllowed` (`src/allowlist.ts:1-3`): a user id that is present and on a non-empty list.
/// Used by the interaction handler, which has no guild/channel scoping.
pub fn is_allowed(user_id: Option<Id<UserMarker>>, allowed_user_ids: &[String]) -> bool {
    match user_id {
        Some(id) => !allowed_user_ids.is_empty() && allowed_user_ids.contains(&id.to_string()),
        None => false,
    }
}

/// The adapter's whole decision for one message: ignore bots and system messages, pick the
/// trigger, then check authorization (TS `handleMessageCreate`,
/// `src/commands/discord.ts:951-1003`).
pub fn decide(message: &Message, ctx: &TriggerContext<'_>) -> Decision {
    if message.author.bot {
        return Decision::Ignore(IgnoreReason::Bot);
    }

    // Only regular messages and replies; thread-creation recaps, pins and the rest are dropped
    // so they don't prompt the parent channel (`src/commands/discord.ts:957-959`).
    if !matches!(message.kind, MessageType::Regular | MessageType::Reply) {
        return Decision::Ignore(IgnoreReason::SystemMessage);
    }

    let is_guild = message.guild_id.is_some();
    let trigger = if is_guild {
        match guild_trigger(message, ctx) {
            Some(trigger) => trigger,
            None => return Decision::Ignore(IgnoreReason::NoTrigger),
        }
    } else {
        Trigger::DirectMessage
    };

    if !is_authorized(
        Some(message.author.id),
        is_guild,
        message.channel_id,
        ctx.settings,
    ) {
        return Decision::Ignore(IgnoreReason::Unauthorized);
    }

    Decision::Handle { trigger, is_guild }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use twilight_model::id::Id;

    use super::*;

    fn settings(json: serde_json::Value) -> DiscordSettings {
        serde_json::from_value(json).unwrap_or_default()
    }

    fn message(json: serde_json::Value) -> Message {
        serde_json::from_value(json).unwrap()
    }

    const BOT: u64 = 100;
    const USER: u64 = 200;
    const CHANNEL: u64 = 7;
    const PARENT: u64 = 8;
    const GUILD: u64 = 9;

    /// A minimal message whose author is `USER`, in `CHANNEL`, in a guild unless `guild` is false.
    fn base(content: &str, guild: bool) -> serde_json::Value {
        let mut value = json!({
            "id": "500",
            "channel_id": CHANNEL.to_string(),
            "author": {"id": USER.to_string(), "username": "hopper", "discriminator": "0"},
            "content": content,
            "timestamp": "2026-10-17T00:00:00+00:00",
            "edited_timestamp": null,
            "tts": false,
            "mention_everyone": false,
            "mentions": [],
            "mention_roles": [],
            "attachments": [],
            "embeds": [],
            "pinned": false,
            "type": 0,
            "flags": 0,
        });
        if guild {
            value["guild_id"] = json!(GUILD.to_string());
        }
        value
    }

    fn ctx<'a>(
        settings: &'a DiscordSettings,
        parent: Option<Id<ChannelMarker>>,
    ) -> TriggerContext<'a> {
        TriggerContext {
            bot_user_id: Some(Id::new(BOT)),
            settings,
            thread_parent: parent,
        }
    }

    #[test]
    fn parses_the_discord_section() {
        let parsed = DiscordSettings::from_section(
            r#"{"token":"ignored-here","allowedUserIds":["1"],"listenChannels":["7"],
                "listenGuilds":["9"],"allowedGuilds":["9"],"channelAllowedUserIds":{"7":["2","3"]},
                "channelNames":{"7":"general"},"imageOutputRoots":["/tmp"],"streaming":true,
                "unknownKey":42}"#,
        )
        .unwrap();
        assert_eq!(parsed.allowed_user_ids, ["1"]);
        assert_eq!(parsed.listen_channels, ["7"]);
        assert_eq!(parsed.listen_guilds, ["9"]);
        assert_eq!(parsed.allowed_guilds, ["9"]);
        assert_eq!(
            parsed.channel_allowed_user_ids.get("7").unwrap(),
            &["2", "3"]
        );
        assert_eq!(parsed.channel_names.get("7").unwrap(), "general");
        assert!(parsed.streaming);
        // Defaults for the missing fields.
        assert_eq!(
            DiscordSettings::from_section("{}").unwrap(),
            Default::default()
        );
    }

    #[test]
    fn channel_tag_matches_ts() {
        let s = settings(json!({"channelNames": {"7": "general"}}));
        assert_eq!(
            s.channel_tag(true, &Id::new(CHANNEL)),
            "[Discord Channel: general]"
        );
        assert_eq!(s.channel_tag(true, &Id::new(999)), "[Discord Channel: 999]");
        assert_eq!(s.channel_tag(false, &Id::new(CHANNEL)), "[Discord DM]");
    }

    #[test]
    fn bots_and_system_messages_are_ignored() {
        let s = settings(json!({"allowedUserIds": [USER.to_string()]}));
        let mut bot_message = base("hi", false);
        bot_message["author"]["bot"] = json!(true);
        assert_eq!(
            decide(&message(bot_message), &ctx(&s, None)),
            Decision::Ignore(IgnoreReason::Bot)
        );

        let mut system = base("thread started", true);
        system["type"] = json!(18);
        assert_eq!(
            decide(&message(system), &ctx(&s, None)),
            Decision::Ignore(IgnoreReason::SystemMessage)
        );

        // A reply (type 19) is processed.
        let mut reply = base("hi", false);
        reply["type"] = json!(19);
        assert_eq!(
            decide(&message(reply), &ctx(&s, None)),
            Decision::Handle {
                trigger: Trigger::DirectMessage,
                is_guild: false
            }
        );
    }

    #[test]
    fn dms_always_trigger_but_still_need_authorization() {
        let allowed = settings(json!({"allowedUserIds": [USER.to_string()]}));
        assert_eq!(
            decide(&message(base("hello", false)), &ctx(&allowed, None)),
            Decision::Handle {
                trigger: Trigger::DirectMessage,
                is_guild: false
            }
        );

        let denied = settings(json!({}));
        assert_eq!(
            decide(&message(base("hello", false)), &ctx(&denied, None)),
            Decision::Ignore(IgnoreReason::Unauthorized)
        );
    }

    #[test]
    fn guild_trigger_rules_in_order() {
        let all = settings(json!({
            "allowedUserIds": [USER.to_string()],
            "listenChannels": [CHANNEL.to_string()],
            "listenGuilds": [GUILD.to_string()],
        }));

        // Reply to the bot.
        let mut reply = base("", true);
        reply["referenced_message"] = json!({
            "id": "400", "channel_id": CHANNEL.to_string(),
            "author": {"id": BOT.to_string(), "username": "claudeclaw", "discriminator": "0"},
            "content": "earlier", "timestamp": "2026-10-17T00:00:00+00:00",
            "edited_timestamp": null, "tts": false, "mention_everyone": false,
            "mentions": [], "mention_roles": [], "attachments": [], "embeds": [],
            "pinned": false, "type": 0,
        });
        assert_eq!(
            decide(&message(reply), &ctx(&all, None)),
            Decision::Handle {
                trigger: Trigger::ReplyToBot,
                is_guild: true
            }
        );

        // Mention via the array.
        let mut mentioned = base("look", true);
        mentioned["mentions"] = json!([
            {"id": BOT.to_string(), "username": "claudeclaw", "discriminator": "0",
             "public_flags": 0, "avatar": null}
        ]);
        assert_eq!(
            decide(&message(mentioned), &ctx(&all, None)),
            Decision::Handle {
                trigger: Trigger::Mention,
                is_guild: true
            }
        );

        // Mention in content (`<@!id>` too, via the raw `<@` form is not enough; only `<@id>`).
        let content_mention = base(&format!("<@{BOT}> hi"), true);
        assert_eq!(
            decide(&message(content_mention), &ctx(&all, None)),
            Decision::Handle {
                trigger: Trigger::MentionInContent,
                is_guild: true
            }
        );

        // A guild message in a listen channel, with no mention at all.
        let plain = base("just talking", true);
        assert_eq!(
            decide(&message(plain), &ctx(&all, None)),
            Decision::Handle {
                trigger: Trigger::ListenChannel,
                is_guild: true
            }
        );

        // Listen guild: a channel that is not in listenChannels.
        let listen_guild_only = settings(json!({
            "allowedUserIds": [USER.to_string()],
            "listenGuilds": [GUILD.to_string()],
        }));
        assert_eq!(
            decide(&message(base("hi", true)), &ctx(&listen_guild_only, None)),
            Decision::Handle {
                trigger: Trigger::ListenGuild,
                is_guild: true
            }
        );

        // Listen channel thread: the message's channel is a tracked thread whose parent listens.
        let thread_only = settings(json!({
            "allowedUserIds": [USER.to_string()],
            "listenChannels": [PARENT.to_string()],
        }));
        let mut in_thread = base("hi", true);
        in_thread["channel_id"] = json!("77");
        assert_eq!(
            decide(
                &message(in_thread),
                &ctx(&thread_only, Some(Id::new(PARENT)))
            ),
            Decision::Handle {
                trigger: Trigger::ListenChannelThread,
                is_guild: true
            }
        );

        // No rule matches.
        let none = settings(json!({"allowedUserIds": [USER.to_string()]}));
        assert_eq!(
            decide(&message(base("hi", true)), &ctx(&none, None)),
            Decision::Ignore(IgnoreReason::NoTrigger)
        );
    }

    #[test]
    fn before_ready_the_bot_id_is_unknown_so_reply_and_mention_rules_are_off() {
        let s = settings(json!({"allowedUserIds": [USER.to_string()]}));
        let ctx = TriggerContext {
            bot_user_id: None,
            settings: &s,
            thread_parent: None,
        };
        let mut mentioned = base(&format!("<@{BOT}> look"), true);
        mentioned["mentions"] = json!([
            {"id": BOT.to_string(), "username": "b", "discriminator": "0", "public_flags": 0}
        ]);
        assert_eq!(
            decide(&message(mentioned), &ctx),
            Decision::Ignore(IgnoreReason::NoTrigger)
        );
    }

    #[test]
    fn channel_scoped_allowlist_never_covers_dms() {
        let s = settings(json!({"channelAllowedUserIds": {"7": ["200"]}}));
        assert!(!is_authorized(Some(Id::new(USER)), false, Id::new(7), &s));
        assert!(is_authorized(Some(Id::new(USER)), true, Id::new(7), &s));
        // A different channel's scoped list does not apply.
        assert!(!is_authorized(Some(Id::new(USER)), true, Id::new(8), &s));
        // An empty global list denies.
        assert!(!is_allowed(Some(Id::new(USER)), &[]));
        assert!(is_allowed(Some(Id::new(USER)), &[USER.to_string()]));
        assert!(!is_allowed(None, &[USER.to_string()]));
    }
}
