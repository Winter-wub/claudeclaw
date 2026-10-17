//! The gateway loop and the cache both sides read (architecture §4 `claudeclaw-discord`, §6, §7).
//!
//! This is the transport half the TS adapter splits across `handleMessageCreate`
//! (`src/commands/discord.ts:951-1376`), `handleDispatch` (`:1803-1914`) and the WebSocket
//! plumbing (`:1707-2013`): the twilight [`Shard`] replaces the hand-rolled reconnect, heartbeat
//! and resume, but the event handling — triggers, the Turn, the classifier, the cache updates —
//! is ported here.
//!
//! [`Shared`] is the mutable state the gateway owns and the outbound side reads: the settings
//! slice, the connection state, the cached REST client, the bot identity and the guild/channel/
//! thread names the Management API's overview asks for (§2.7). It also holds the effective token
//! (a [`SecretString`], never logged) only as the shard's own copy; it is never formatted.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use claudeclaw_chat::{
    AdapterError, AdapterState, ChannelInfo, ChatHost, ChatScope, GuildInfo, ThreadInfo, Transport,
    UserInfo, UtilityHandle,
};
use secrecy::SecretString;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use twilight_gateway::{
    ConfigBuilder, Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _,
    error::ReceiveMessageError,
};
use twilight_model::application::interaction::Interaction;
use twilight_model::channel::Channel;
use twilight_model::gateway::payload::incoming::GuildCreate;
use twilight_model::gateway::payload::incoming::{
    Ready, ThreadDelete, ThreadListSync, ThreadUpdate,
};
use twilight_model::id::Id;
use twilight_model::id::marker::{ApplicationMarker, ChannelMarker, UserMarker};

use crate::commands;
use crate::control::{channel_kind, guild_info, user_info};
use crate::rest::Rest;
use crate::seams::{AttachmentFetcher, DiscordEndpoints, ReplySink, ReplyTarget};
use crate::triggers::{Decision, DiscordSettings, IgnoreReason, TriggerContext};
use crate::turn::{ThreadTrack, agent_dir_key, clean_content};

/// The close codes Discord won't let the bot recover from (`FATAL_CLOSE_CODES`,
/// `src/commands/discord.ts:1784`), so the adapter stays [`AdapterState::Disconnected`] until the
/// token changes.
const FATAL_CLOSE_CODES: [u16; 6] = [4004, 4010, 4011, 4012, 4013, 4014];

/// How long a graceful shutdown waits for in-flight message/interaction tasks.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// The adapter's connection state and when it entered it (management-api §2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Connection {
    pub state: AdapterState,
    pub since: SystemTime,
}

impl Connection {
    fn new(state: AdapterState) -> Self {
        Self {
            state,
            since: SystemTime::now(),
        }
    }
}

/// Everything the gateway and the outbound side share.
#[derive(Debug)]
struct Cache {
    settings: DiscordSettings,
    connection: Connection,
    /// The cached REST client; rebuilt when the token changes (§7).
    rest: Option<Arc<Rest>>,
    /// The application id, learned from READY; needed to answer interactions.
    application: Option<Id<ApplicationMarker>>,
    /// The bot's own user, for the overview and the trigger rules.
    bot: Option<UserInfo>,
    bot_id: Option<Id<UserMarker>>,
    /// The guilds the bot was already in at READY, so a reconnect doesn't re-welcome them
    /// (`readyGuildIds`, `src/commands/discord.ts:188`).
    ready_guilds: BTreeSet<String>,
    guilds: BTreeMap<String, GuildInfo>,
    channels: BTreeMap<String, ChannelInfo>,
    threads: BTreeMap<String, ThreadTrack>,
    /// Discord user ids → usernames, for the overview's `DiscordUser` list.
    user_names: BTreeMap<String, String>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            settings: DiscordSettings::default(),
            connection: Connection::new(AdapterState::Disabled),
            rest: None,
            application: None,
            bot: None,
            bot_id: None,
            ready_guilds: BTreeSet::new(),
            guilds: BTreeMap::new(),
            channels: BTreeMap::new(),
            threads: BTreeMap::new(),
            user_names: BTreeMap::new(),
        }
    }
}

/// The mutex-guarded cache behind [`Shared`]. Every accessor takes the lock only to clone what
/// it needs, so no caller holds it across an await.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    cache: Mutex<Cache>,
}

impl Shared {
    /// A disconnected adapter with default settings, before [`run`] applies the published ones.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The cached REST client, or [`AdapterError::NotConnected`] before a token is published.
    pub(crate) fn rest(&self) -> Result<Arc<Rest>, AdapterError> {
        self.lock().rest.clone().ok_or(AdapterError::NotConnected)
    }

    /// Replaces the REST client, or clears it when the token is gone.
    pub(crate) fn set_rest(&self, rest: Option<Arc<Rest>>) {
        self.lock().rest = rest;
    }

    /// The connection state and when it was entered.
    pub(crate) fn connection(&self) -> Connection {
        self.lock().connection
    }

    /// Moves the connection state, stamping `since` only when it actually changes.
    pub(crate) fn set_state(&self, state: AdapterState) {
        let mut cache = self.lock();
        if cache.connection.state != state {
            cache.connection = Connection::new(state);
        }
    }

    /// The adapter's settings slice (`settings.json`'s `discord` object, §7).
    pub(crate) fn settings(&self) -> DiscordSettings {
        self.lock().settings.clone()
    }

    pub(crate) fn set_settings(&self, settings: DiscordSettings) {
        self.lock().settings = settings;
    }

    /// The bot's own user, for the overview; `None` before READY.
    pub(crate) fn bot(&self) -> Option<UserInfo> {
        self.lock().bot.clone()
    }

    /// The bot's own user id, for the reply/mention trigger rules (`botUserId`,
    /// `src/commands/discord.ts:183`).
    pub(crate) fn bot_user_id(&self) -> Option<Id<UserMarker>> {
        self.lock().bot_id
    }

    /// The application id, for answering interactions.
    pub(crate) fn application(&self) -> Option<Id<ApplicationMarker>> {
        self.lock().application
    }

    /// Every guild the bot is in, named where GUILD_CREATE reported it.
    pub(crate) fn guild_infos(&self) -> Vec<GuildInfo> {
        self.lock().guilds.values().cloned().collect()
    }

    /// The cached channels the adapter listens in (management-api §2.7's `DiscordChannel` ids).
    pub(crate) fn listened_channels(&self) -> Vec<ChannelInfo> {
        let cache = self.lock();
        cache
            .channels
            .iter()
            .filter(|(id, _)| cache.settings.listen_channels.contains(id))
            .map(|(_, channel)| channel.clone())
            .collect()
    }

    /// Every channel the bot could listen in, from the gateway cache (§2.7 `discord.available`).
    pub(crate) fn available_channels(&self) -> Vec<ChannelInfo> {
        self.lock().channels.values().cloned().collect()
    }

    /// The threads the cache knows, as the overview's `DiscordThread` ids.
    pub(crate) fn thread_infos(&self) -> Vec<ThreadInfo> {
        self.lock()
            .threads
            .iter()
            .map(|(id, track)| ThreadInfo {
                id: id.clone(),
                name: track.name.clone().unwrap_or_else(|| id.clone()),
                parent_id: track.parent.to_string(),
            })
            .collect()
    }

    /// The users the adapter's allow-lists name, deduplicated, with names where the cache knows
    /// them (§2.7's `DiscordUser` list).
    pub(crate) fn allow_list_users(&self, settings: &DiscordSettings) -> Vec<UserInfo> {
        let cache = self.lock();
        let mut ids: BTreeSet<String> = settings.allowed_user_ids.iter().cloned().collect();
        for users in settings.channel_allowed_user_ids.values() {
            ids.extend(users.iter().cloned());
        }
        ids.into_iter()
            .map(|id| {
                let name = cache.user_names.get(&id).cloned();
                user_info(&id, name)
            })
            .collect()
    }

    /// The tracked thread `id` is the channel of, if any (`knownThreads.get(id)`,
    /// `src/commands/discord.ts:570`).
    pub(crate) fn thread(&self, id: &Id<ChannelMarker>) -> Option<ThreadTrack> {
        self.lock().threads.get(&id.to_string()).cloned()
    }

    /// A snapshot of every tracked thread, for the Fire-by-name search
    /// (`src/commands/discord.ts:1186`).
    pub(crate) fn threads(&self) -> Vec<(String, ThreadTrack)> {
        self.lock()
            .threads
            .iter()
            .map(|(id, track)| (id.clone(), track.clone()))
            .collect()
    }

    /// Upserts a tracked thread (`upsertThread`, `src/commands/discord.ts:201-208`), preserving
    /// the existing agent key and name when the transport didn't report new ones.
    pub(crate) fn upsert_thread(
        &self,
        id: Id<ChannelMarker>,
        parent: Id<ChannelMarker>,
        raw_name: Option<&str>,
    ) {
        let key = id.to_string();
        let mut cache = self.lock();
        let existing = cache.threads.get(&key).cloned();
        let agent_name = raw_name
            .and_then(|name| agent_dir_key(name, &key))
            .or_else(|| existing.as_ref().and_then(|track| track.agent_name.clone()));
        let name = raw_name
            .map(str::to_owned)
            .or_else(|| existing.as_ref().and_then(|track| track.name.clone()));
        cache.threads.insert(
            key,
            ThreadTrack {
                parent,
                agent_name,
                name,
            },
        );
    }

    pub(crate) fn remove_thread(&self, id: Id<ChannelMarker>) {
        self.lock().threads.remove(&id.to_string());
    }

    fn upsert_guild(&self, guild: GuildInfo) {
        self.lock().guilds.insert(guild.id.clone(), guild);
    }

    fn upsert_channel(&self, channel: &Channel) {
        let name = channel
            .name
            .clone()
            .unwrap_or_else(|| channel.id.to_string());
        let Some(kind) = channel_kind(channel.kind) else {
            return;
        };
        self.lock().channels.insert(
            channel.id.to_string(),
            ChannelInfo {
                id: channel.id.to_string(),
                name,
                guild_id: channel.guild_id.map(|id| id.to_string()),
                kind,
            },
        );
    }

    fn remember_user(&self, id: &Id<UserMarker>, name: &str) {
        self.lock()
            .user_names
            .insert(id.to_string(), name.to_owned());
    }
}

/// The gateway's per-event context: the shared cache plus the seams core gave the adapter.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub shared: Arc<Shared>,
    pub host: Arc<dyn ChatHost>,
    pub reply: Arc<dyn ReplySink>,
    pub fetcher: Arc<dyn AttachmentFetcher>,
}

/// Connect and serve until `shutdown` is cancelled (architecture §6, §7).
pub(crate) async fn run(
    ctx: Ctx,
    endpoints: DiscordEndpoints,
    shutdown: CancellationToken,
) -> Result<(), AdapterError> {
    let mut settings_rx = ctx.host.settings().subscribe(Transport::Discord);
    let current = settings_rx.borrow_and_update().clone();
    apply_section(&ctx.shared, &current);

    let tracker = TaskTracker::new();
    let mut token = current.token.clone();
    let mut shard: Option<Shard> = None;
    // A fatal close parks the adapter: no rebuild until the token changes, like TS's
    // `FATAL_CLOSE_CODES` short-circuit (`src/commands/discord.ts:1991`).
    let mut fatal = false;

    loop {
        if shard.is_none() && !fatal {
            if let Some(secret) = token.as_ref() {
                ctx.shared
                    .set_rest(Some(Arc::new(Rest::connect(secret, &endpoints))));
                ctx.shared.set_state(AdapterState::Connecting);
                shard = Some(build_shard(secret, &endpoints));
                tracing::info!("discord.connecting");
            } else {
                ctx.shared.set_state(AdapterState::Disabled);
            }
        }

        let mut reconnect = false;
        tokio::select! {
            biased;

            _ = shutdown.cancelled() => break,

            changed = settings_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let next = settings_rx.borrow_and_update().clone();
                apply_section(&ctx.shared, &next);
                if token_differs(token.as_ref(), next.token.as_ref()) {
                    token = next.token.clone();
                    fatal = false;
                    reconnect = true;
                }
            }

            event = next_shard(&mut shard), if shard.is_some() => {
                match event {
                    None => {
                        // twilight returns `None` only once a shard is fatally closed.
                        fatal = true;
                        ctx.shared.set_state(AdapterState::Disconnected);
                        tracing::error!("discord.disconnected_fatal");
                    }
                    Some(Err(error)) => {
                        tracing::warn!(error = %error, "discord.receive_error");
                    }
                    Some(Ok(event)) => {
                        dispatch(&ctx, &tracker, event, &mut fatal);
                    }
                }
            }
        }

        if reconnect {
            shard = None;
            ctx.shared.set_rest(None);
            ctx.shared.set_state(AdapterState::Reconnecting);
        }
    }

    // Let in-flight message and interaction tasks drain, bounded so shutdown can't hang.
    tracker.close();
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, tracker.wait()).await;
    tracing::info!("discord.stopped");
    Ok(())
}

/// The event types the adapter asks the gateway to deserialize.
fn wanted_events() -> EventTypeFlags {
    EventTypeFlags::READY
        | EventTypeFlags::RESUMED
        | EventTypeFlags::GUILD_CREATE
        | EventTypeFlags::MESSAGE_CREATE
        | EventTypeFlags::INTERACTION_CREATE
        | EventTypeFlags::THREAD_CREATE
        | EventTypeFlags::THREAD_UPDATE
        | EventTypeFlags::THREAD_DELETE
        | EventTypeFlags::THREAD_LIST_SYNC
}

/// The next event from a live shard, or `None` when it has fatally closed.
async fn next_shard(shard: &mut Option<Shard>) -> Option<Result<Event, ReceiveMessageError>> {
    match shard.as_mut() {
        Some(shard) => shard.next_event(wanted_events()).await,
        None => None,
    }
}

/// The intents TS asks for (`INTENTS`, `src/commands/discord.ts:79-84`).
fn intents() -> Intents {
    Intents::GUILDS
        | Intents::GUILD_MESSAGES
        | Intents::GUILD_MESSAGE_REACTIONS
        | Intents::DIRECT_MESSAGES
        | Intents::MESSAGE_CONTENT
}

/// A shard aimed at `endpoints.gateway` when the Parity suite redirected it (#18), else Discord's
/// own gateway. twilight appends `/?v=10&encoding=json` to the base.
fn build_shard(token: &SecretString, endpoints: &DiscordEndpoints) -> Shard {
    let mut builder = ConfigBuilder::new(
        secrecy::ExposeSecret::expose_secret(token).to_owned(),
        intents(),
    );
    if let Some(base) = gateway_base(&endpoints.gateway) {
        builder = builder.proxy_url(base);
    }
    Shard::with_config(ShardId::ONE, builder.build())
}

/// The base URL twilight's `proxy_url` wants: the endpoint without its query or trailing slash.
/// `None` for an unset or empty endpoint, so twilight uses `wss://gateway.discord.gg`.
fn gateway_base(gateway: &Option<String>) -> Option<String> {
    let raw = gateway.as_deref()?;
    let base = raw
        .split(['?', '#'])
        .next()
        .unwrap_or(raw)
        .trim_end_matches('/');
    (!base.is_empty()).then(|| base.to_owned())
}

/// Whether two published settings blocks carry different tokens (the only change, §7, that
/// restarts the adapter).
fn token_differs(current: Option<&SecretString>, next: Option<&SecretString>) -> bool {
    use secrecy::ExposeSecret as _;
    match (current, next) {
        (None, None) => false,
        (Some(a), Some(b)) => a.expose_secret() != b.expose_secret(),
        _ => true,
    }
}

/// Parse and apply the transport's `settings.json` slice, keeping the previous one on a parse
/// error so a half-written file never breaks the adapter (§7).
fn apply_section(shared: &Shared, settings: &claudeclaw_chat::TransportSettings) {
    match DiscordSettings::from_section(&settings.section) {
        Ok(parsed) => shared.set_settings(parsed),
        Err(error) => tracing::warn!(error = %error, "discord.settings_invalid"),
    }
}

/// One dispatch event, handled as TS's `handleDispatch` switch (`src/commands/discord.ts:1803`).
fn dispatch(ctx: &Ctx, tracker: &TaskTracker, event: Event, fatal: &mut bool) {
    match event {
        Event::Ready(ready) => {
            ctx.shared.set_state(AdapterState::Connected);
            tracing::info!("discord.connected");
            handle_ready(ctx, ready, tracker);
        }
        Event::Resumed => {
            ctx.shared.set_state(AdapterState::Connected);
            tracing::info!("discord.resumed");
        }
        Event::GuildCreate(create) => handle_guild_create(ctx, tracker, *create),
        Event::MessageCreate(message) => {
            let ctx = ctx.clone();
            tracker.spawn(async move { handle_message(ctx, Box::new(message.0)).await });
        }
        Event::InteractionCreate(interaction) => {
            let ctx = ctx.clone();
            tracker.spawn(async move { commands::handle_interaction(ctx, interaction.0).await });
        }
        Event::ThreadCreate(thread) => handle_thread_upsert(ctx, tracker, &thread.0),
        Event::ThreadUpdate(thread) => handle_thread_update(ctx, *thread),
        Event::ThreadDelete(delete) => handle_thread_delete(ctx, delete),
        Event::ThreadListSync(sync) => handle_thread_list_sync(ctx, sync),
        Event::GatewayClose(frame) => {
            if let Some(code) = frame.as_ref().map(|frame| frame.code) {
                if FATAL_CLOSE_CODES.contains(&code) {
                    *fatal = true;
                    ctx.shared.set_state(AdapterState::Disconnected);
                    tracing::warn!(close_code = code, "discord.disconnected");
                } else {
                    ctx.shared.set_state(AdapterState::Reconnecting);
                    tracing::warn!(close_code = code, "discord.reconnecting");
                }
            } else {
                ctx.shared.set_state(AdapterState::Reconnecting);
                tracing::warn!("discord.reconnecting");
            }
        }
        _ => {}
    }
}

/// READY (`src/commands/discord.ts:1807-1822`): record the identity, snapshot the guilds we were
/// already in, and register the slash commands off the loop. Pending-resume replay
/// (`runPendingResume`, `:1786`) has no Rust seam and is not ported.
fn handle_ready(ctx: &Ctx, ready: Ready, tracker: &TaskTracker) {
    let bot = UserInfo {
        id: ready.user.id.to_string(),
        name: Some(ready.user.name.clone()),
    };
    {
        let mut cache = ctx.shared.lock();
        cache.bot = Some(bot);
        cache.bot_id = Some(ready.user.id);
        cache.application = Some(ready.application.id);
        cache.ready_guilds = ready
            .guilds
            .iter()
            .map(|guild| guild.id.to_string())
            .collect();
    }
    let ctx = ctx.clone();
    tracker.spawn(async move { commands::register(&ctx).await });
}

/// GUILD_CREATE (`src/commands/discord.ts:1844-1868`): cache the guild, its channels and active
/// threads, then welcome it once if it is an `allowedGuilds` entry.
fn handle_guild_create(ctx: &Ctx, tracker: &TaskTracker, create: GuildCreate) {
    let GuildCreate::Available(guild) = create else {
        return;
    };
    ctx.shared
        .upsert_guild(guild_info(&guild.id.to_string(), &guild.name));
    for channel in &guild.channels {
        ctx.shared.upsert_channel(channel);
    }
    for thread in &guild.threads {
        if let Some(parent) = thread.parent_id {
            ctx.shared
                .upsert_thread(thread.id, parent, thread.name.as_deref());
        }
    }

    if ctx
        .shared
        .lock()
        .ready_guilds
        .contains(&guild.id.to_string())
    {
        return;
    }
    let settings = ctx.shared.settings();
    if settings.allowed_guilds.is_empty()
        || !settings.allowed_guilds.contains(&guild.id.to_string())
    {
        tracing::debug!(guild_id = %guild.id, "discord.guild_joined_silent");
        return;
    }
    let Some(channel) = guild.system_channel_id else {
        return;
    };
    let prompt = guild_welcome_prompt(&guild.name, &guild.id.to_string());
    let ctx = ctx.clone();
    tracker.spawn(async move { send_welcome(ctx, channel, prompt).await });
}

/// The GUILD_CREATE welcome prompt (`src/commands/discord.ts:1689-1693`).
fn guild_welcome_prompt(name: &str, id: &str) -> String {
    format!(
        "[Discord system event] I was added to a guild.\nGuild name: {}\nGuild id: {id}\nWrite a \
         short first message for the server. Confirm I was added and explain how to trigger me \
         (mention or reply).",
        wrap_untrusted("guild-name", name, 8_000),
    )
}

/// Run the welcome prompt sessionlessly and post it, falling back to a fixed line
/// (`src/commands/discord.ts:1695-1704`).
async fn send_welcome(ctx: Ctx, channel: Id<ChannelMarker>, prompt: String) {
    let UtilityHandle { outcome } = ctx.host.utility(prompt);
    let text = match outcome.await {
        Ok(outcome) => outcome.text,
        Err(_) => String::new(),
    };
    let text = if text.trim().is_empty() {
        "I was added to this server. Mention me to start.".to_owned()
    } else {
        text
    };
    if let Ok(rest) = ctx.shared.rest() {
        let _ = rest.send_message(channel, &text).await;
    }
}

/// THREAD_CREATE (`src/commands/discord.ts:1870-1880`): track the thread and, when its parent is
/// a listen channel, join it so Discord delivers its messages.
fn handle_thread_upsert(ctx: &Ctx, tracker: &TaskTracker, thread: &Channel) {
    let Some(parent) = thread.parent_id else {
        return;
    };
    ctx.shared
        .upsert_thread(thread.id, parent, thread.name.as_deref());
    if !ctx
        .shared
        .settings()
        .listen_channels
        .contains(&parent.to_string())
    {
        return;
    }
    let rest = ctx.shared.rest().ok();
    if let Some(rest) = rest {
        let thread_id = thread.id;
        tracker.spawn(async move {
            let _ = rest.join_thread(thread_id).await;
        });
    }
}

/// THREAD_UPDATE (`src/commands/discord.ts:1892-1904`): an archived thread is dropped; otherwise
/// it is re-upserted.
fn handle_thread_update(ctx: &Ctx, ThreadUpdate(thread): ThreadUpdate) {
    let archived = thread
        .thread_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.archived);
    if archived {
        ctx.shared.remove_thread(thread.id);
        tracing::debug!(thread_id = %thread.id, "discord.thread_archived");
    } else if let Some(parent) = thread.parent_id {
        ctx.shared
            .upsert_thread(thread.id, parent, thread.name.as_deref());
    }
}

/// THREAD_DELETE (`src/commands/discord.ts:1882-1890`). Removing the thread's session is core's
/// job; the classifier path has no seam to ask for it (see the worker's gap note).
fn handle_thread_delete(ctx: &Ctx, delete: ThreadDelete) {
    ctx.shared.remove_thread(delete.id);
    tracing::debug!(thread_id = %delete.id, "discord.thread_deleted");
}

/// THREAD_LIST_SYNC (`src/commands/discord.ts:1906-1912`).
fn handle_thread_list_sync(ctx: &Ctx, sync: ThreadListSync) {
    for thread in sync.threads {
        if let Some(parent) = thread.parent_id {
            ctx.shared
                .upsert_thread(thread.id, parent, thread.name.as_deref());
        }
    }
}

/// MESSAGE_CREATE (`src/commands/discord.ts:951-1376`): decide, authorize, optionally classify a
/// thread intent, then run the Turn through the seam. The streaming preview, tool reactions,
/// 2000-char splitting and image upload are the [`ReplySink`]'s; the thinking reaction and the
/// interrupted clear are this side's.
async fn handle_message(ctx: Ctx, message: Box<twilight_model::channel::Message>) {
    let message = *message;
    // Cache the author's name for the Management API's `DiscordUser` list (§2.7).
    ctx.shared
        .remember_user(&message.author.id, &message.author.name);
    let settings = ctx.shared.settings();
    let bot = ctx.shared.bot_user_id();
    let track = ctx.shared.thread(&message.channel_id);
    let is_guild = message.guild_id.is_some();

    let decision = crate::triggers::decide(
        &message,
        &TriggerContext {
            bot_user_id: bot,
            settings: &settings,
            thread_parent: track.as_ref().map(|track| track.parent),
        },
    );
    match decision {
        Decision::Ignore(IgnoreReason::Unauthorized) => {
            // TS answers a DM's rejection and stays quiet in a guild (`:996-1003`).
            if !is_guild && let Ok(rest) = ctx.shared.rest() {
                let _ = rest.send_message(message.channel_id, "Unauthorized.").await;
            }
            return;
        }
        Decision::Ignore(_) => return,
        Decision::Handle { .. } => {}
    }

    if !has_something_to_answer(&message) {
        return;
    }

    let clean = clean_content(&message.content, bot);
    if is_guild
        && clean.chars().count() < 200
        && handle_thread_intent(&ctx, &message, &settings).await
    {
        return;
    }

    // The thinking reaction (👀) and its clear are the [`ReplySink`]'s: it adds 👀 before the
    // first event and removes it last, so a Turn gets exactly one (#29).
    let turn = crate::turn::build(
        &message,
        bot,
        &settings,
        track.as_ref(),
        ctx.fetcher.as_ref(),
    )
    .await;
    let target = ReplyTarget {
        channel: message.channel_id,
        origin: message.id,
    };
    ctx.reply.deliver(target, ctx.host.converse(turn)).await;
}

/// Whether a message carries anything to answer: text, attachments, or a forwarded snapshot
/// (`src/commands/discord.ts:1016`).
fn has_something_to_answer(message: &twilight_model::channel::Message) -> bool {
    if !message.content.trim().is_empty() || !message.attachments.is_empty() {
        return true;
    }
    message
        .message_snapshots
        .first()
        .is_some_and(|snapshot| !snapshot.message.content.is_empty())
}

/// The parsed thread-intent classifier output (`ThreadIntent`, `src/commands/discord.ts:579-582`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ThreadIntent {
    action: String,
    names: Vec<String>,
}

/// The classifier's system prompt (`src/commands/discord.ts:585-598`), verbatim.
const INTENT_SYSTEM_PROMPT: &str = "You classify user messages into thread management intents.

If the user wants to CREATE/SPAWN/DEPLOY threads (e.g. \"hire X\", \"派出 X\", \"叫 X 出來\", \"派 X 去打\", \"開 X\", \"建立 X\"):
Return: {\"action\":\"hire\",\"names\":[\"name1\",\"name2\"]}

If the user wants to DELETE/REMOVE threads (e.g. \"fire X\", \"撤回 X\", \"把 X 叫回來\", \"刪 X\", \"關 X\"):
Return: {\"action\":\"fire\",\"names\":[\"name1\",\"name2\"]}

If the message is NOT about thread management, return: null

Rules:
- Extract individual names. \"桃園三結義\" = [\"劉備\",\"關羽\",\"張飛\"]. \"五虎將\" = [\"關羽\",\"張飛\",\"趙雲\",\"馬超\",\"黃忠\"].
- Common patterns: 派/派出/出征/上陣/迎戰/出戰 = hire. 撤/撤回/收回/叫回來/滾 = fire.
- Return ONLY valid JSON or the word null. No explanation.";

/// Extract the intent from a classifier reply (`src/commands/discord.ts:624-630`): `null` (or
/// empty) means "not thread management"; otherwise the first complete JSON object.
fn parse_intent(reply: &str) -> Option<ThreadIntent> {
    let trimmed = reply.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return None;
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str(&trimmed[start..=end]).ok()
}

/// Classify a guild message and act on a Hire/Fire intent (`src/commands/discord.ts:1150-1213`).
/// Returns whether the message was consumed as thread management.
async fn handle_thread_intent(
    ctx: &Ctx,
    message: &twilight_model::channel::Message,
    _settings: &DiscordSettings,
) -> bool {
    let text = clean_content(&message.content, ctx.shared.bot_user_id());
    let prompt = format!("{INTENT_SYSTEM_PROMPT}\n\n---\nUser message: {text}");
    let UtilityHandle { outcome } = ctx.host.utility(prompt);
    let reply = match outcome.await {
        Ok(outcome) => outcome.text,
        Err(_) => return false,
    };
    let Some(intent) = parse_intent(&reply) else {
        return false;
    };
    let channel = message.channel_id;
    match intent.action.as_str() {
        "hire" if !intent.names.is_empty() => {
            hire_named(ctx, channel, &intent.names).await;
            true
        }
        "fire" if !intent.names.is_empty() => {
            fire_named(ctx, channel, &intent.names).await;
            true
        }
        _ => false,
    }
}

/// The classifier's Hire (`src/commands/discord.ts:1153-1179`): the transport calls only. The
/// session and `agents/` work core owns has no seam from this path, so only the thread and its
/// cache entry are created (a worker gap, like the API's Hire).
async fn hire_named(ctx: &Ctx, parent: Id<ChannelMarker>, names: &[String]) {
    let Ok(rest) = ctx.shared.rest() else {
        return;
    };
    let mut results = Vec::with_capacity(names.len());
    for name in names {
        match rest.create_thread(parent, name).await {
            Ok(thread) => {
                ctx.shared.upsert_thread(
                    thread.id,
                    parent,
                    Some(thread.name.as_deref().unwrap_or(name)),
                );
                let _ = rest
                    .send_message(
                        thread.id,
                        &format!(
                            "\u{1F9F5} Thread **{name}** created with independent session. Start chatting!"
                        ),
                    )
                    .await;
                results.push(format!("\u{2705} **{name}** \u{2192} <#{}>", thread.id));
            }
            Err(error) => results.push(format!("\u{274C} **{name}** \u{2014} {error}")),
        }
    }
    let _ = rest.send_message(parent, &results.join("\n")).await;
}

/// The classifier's Fire (`src/commands/discord.ts:1181-1212`): find a tracked thread under
/// `parent` whose name matches, delete it and drop it from the cache. Removing the thread's
/// session and archiving its `agents/` dir is core's, with no seam here (a worker gap).
async fn fire_named(ctx: &Ctx, parent: Id<ChannelMarker>, names: &[String]) {
    let Ok(rest) = ctx.shared.rest() else {
        return;
    };
    let mut results = Vec::with_capacity(names.len());
    for name in names {
        let target = name.to_lowercase();
        let mut found: Option<Id<ChannelMarker>> = None;
        for (id, track) in ctx.shared.threads() {
            if track.parent != parent {
                continue;
            }
            let Ok(id) = id.parse::<u64>().map(Id::new) else {
                continue;
            };
            if let Ok(channel) = rest.channel(id).await
                && channel
                    .name
                    .as_deref()
                    .is_some_and(|actual| actual.to_lowercase() == target)
            {
                found = Some(id);
                break;
            }
        }
        match found {
            Some(id) => match rest.delete_channel(id).await {
                Ok(_) => {
                    ctx.shared.remove_thread(id);
                    results.push(format!("\u{1F5D1}\u{FE0F} **{name}** \u{2014} deleted"));
                }
                Err(error) => results.push(format!("\u{274C} **{name}** \u{2014} {error}")),
            },
            None => results.push(format!("\u{274C} **{name}** \u{2014} not found")),
        }
    }
    let _ = rest.send_message(parent, &results.join("\n")).await;
}

/// A local port of `wrapUntrusted` (`src/prompt-safety.ts`): the adapter builds utility prompts
/// itself, so it wraps the untrusted pieces here. The id only needs to vary per call.
fn wrap_untrusted(label: &str, content: &str, max_len: usize) -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = format!("{:08x}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let truncated = if content.len() > max_len {
        format!("{}\n[truncated]", &content[..max_len])
    } else {
        content.to_owned()
    };
    let safe = defang_untrusted(label, &truncated);
    format!("<untrusted-{label}-{id}>\n{safe}\n</untrusted-{label}-{id}>")
}

/// Replace any `<untrusted-{label}-…>` tag inside `content` so it can't break the wrapper
/// boundary (`wrapUntrusted`'s redaction).
fn defang_untrusted(label: &str, content: &str) -> String {
    let open = format!("<untrusted-{label}-");
    let close = format!("</untrusted-{label}-");
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    loop {
        let next_open = rest.find(&open);
        let next_close = rest.find(&close);
        let next = match (next_open, next_close) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some(at) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match tail.find('>') {
            Some(end) => {
                out.push_str("[redacted-tag]");
                rest = &tail[end + 1..];
            }
            None => {
                out.push_str(tail);
                return out;
            }
        }
    }
}

/// The channel an interaction was invoked in. `Interaction::channel_id` is deprecated; the full
/// [`Channel`] is present on every interaction type but PING.
pub(crate) fn interaction_channel(interaction: &Interaction) -> Option<Id<ChannelMarker>> {
    interaction.channel.as_ref().map(|channel| channel.id)
}

/// Build the scope an interaction acts on: a tracked thread is `Thread`, another guild channel
/// is `Channel`, a DM is `Direct` (`ChatScope`, architecture §6).
pub(crate) fn interaction_scope(shared: &Shared, interaction: &Interaction) -> ChatScope {
    match (interaction.guild_id, interaction_channel(interaction)) {
        (Some(_), Some(channel)) => match shared.thread(&channel) {
            Some(track) => ChatScope::Thread {
                transport: Transport::Discord,
                id: channel.to_string(),
                parent: track.parent.to_string(),
            },
            None => ChatScope::Channel {
                transport: Transport::Discord,
                id: channel.to_string(),
            },
        },
        _ => ChatScope::Direct {
            transport: Transport::Discord,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_base_strips_the_query() {
        assert_eq!(gateway_base(&None), None);
        assert_eq!(gateway_base(&Some(String::new())), None);
        assert_eq!(
            gateway_base(&Some("wss://gw.example/?v=10&encoding=json".into())).as_deref(),
            Some("wss://gw.example")
        );
        assert_eq!(
            gateway_base(&Some("wss://gw.example/".into())).as_deref(),
            Some("wss://gw.example")
        );
    }

    #[test]
    fn fatal_codes_match_ts() {
        for code in [4004, 4010, 4011, 4012, 4013, 4014] {
            assert!(FATAL_CLOSE_CODES.contains(&code), "{code}");
        }
        for code in [4000, 4001, 4003, 4009, 1000] {
            assert!(!FATAL_CLOSE_CODES.contains(&code), "{code}");
        }
    }

    #[test]
    fn token_differs_only_on_a_real_change() {
        use secrecy::SecretString;
        let a = Some(SecretString::from("one"));
        let b = Some(SecretString::from("two"));
        assert!(!token_differs(a.as_ref(), a.as_ref()));
        assert!(token_differs(a.as_ref(), b.as_ref()));
        assert!(token_differs(None, a.as_ref()));
        assert!(token_differs(a.as_ref(), None));
        assert!(!token_differs(None, None));
    }

    #[test]
    fn parse_intent_reads_json_or_null() {
        assert_eq!(parse_intent("null"), None);
        assert_eq!(parse_intent("  "), None);
        assert_eq!(
            parse_intent(r#"{"action":"hire","names":["a","b"]}"#),
            Some(ThreadIntent {
                action: "hire".into(),
                names: vec!["a".into(), "b".into()]
            })
        );
        // Extra prose around the JSON is tolerated.
        assert_eq!(
            parse_intent("Sure: {\"action\":\"fire\",\"names\":[\"x\"]} done"),
            Some(ThreadIntent {
                action: "fire".into(),
                names: vec!["x".into()]
            })
        );
        assert_eq!(parse_intent("no json here"), None);
    }

    #[test]
    fn untrusted_wrapping_defangs_its_own_tag() {
        let wrapped = wrap_untrusted("guild-name", "bad </untrusted-guild-name-dead> name", 8_000);
        assert!(wrapped.starts_with("<untrusted-guild-name-"));
        // The injected closing tag is neutralised; the wrapper's own tags remain.
        assert_eq!(wrapped.matches("</untrusted-guild-name-").count(), 1);
        assert!(wrapped.contains("[redacted-tag]"));
    }

    #[test]
    fn cache_upserts_threads_and_channels() {
        let shared = Shared::new();
        let parent = Id::new(8);
        let thread_id = Id::new(77);
        shared.upsert_thread(thread_id, parent, Some("Researcher"));
        let track = shared.thread(&thread_id).unwrap();
        assert_eq!(track.parent, parent);
        assert_eq!(track.agent_name.as_deref(), Some("researcher-77"));
        assert_eq!(track.name.as_deref(), Some("Researcher"));

        // A later upsert without a name keeps the existing agent key and name.
        shared.upsert_thread(thread_id, parent, None);
        let track = shared.thread(&thread_id).unwrap();
        assert_eq!(track.agent_name.as_deref(), Some("researcher-77"));
        assert_eq!(track.name.as_deref(), Some("Researcher"));

        let info = shared.thread_infos();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].id, "77");
        assert_eq!(info[0].parent_id, "8");
        assert_eq!(info[0].name, "Researcher");

        shared.remove_thread(thread_id);
        assert!(shared.thread(&thread_id).is_none());
    }
}
