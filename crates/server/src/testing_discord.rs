//! An in-process fake for the Discord seam (management-api §2.7).
//!
//! [`FakeDiscord`] stands in for the adapter's `chat::AdapterControl` plus core's session and
//! `agents/` work: a gateway cache of two channels, one of them listened in with one hired
//! thread, and an adapter state the tests move. Every id is a synthetic snowflake.

#![cfg(test)]

use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};

use claudeclaw_api::Scope;
use claudeclaw_api::adapter::AdapterState;
use claudeclaw_api::discord::{
    AllowedUsers, AllowedUsersSource, AvailableChannel, AvailableChannelList, ChannelKind,
    ChannelTrigger, DiscordBot, DiscordChannel, DiscordChannelPatch, DiscordChannelRemoved,
    DiscordGuild, DiscordOverview, DiscordThread, DiscordUser, FireResult, HireResult,
};
use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::scopes::{InheritedFrom, LauncherSource, ScopeLauncher};

use crate::backend::discord::adapter_unavailable;
use crate::backend::{BackendFuture, DiscordBackend};

/// The guild the fake's bot is in.
pub const GUILD: &str = "1001";
/// A channel the bot listens in, with [`THREAD`] under it.
pub const LISTENED: &str = "2001";
/// A channel the bot could listen in but doesn't.
pub const UNLISTENED: &str = "2002";
/// A hired thread under [`LISTENED`].
pub const THREAD: &str = "3001";
/// The first thread id a hire creates; the next ones count up from it.
pub const FIRST_HIRED: u64 = 3100;
/// The owner in the fake's global allow-list.
pub const OWNER: &str = "4001";
/// The stamp the fake archives into, as core's clock would write it.
pub const ARCHIVE_STAMP: &str = "20261018T090000Z";

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 18, 9, 0, 0).unwrap()
}

/// One call the routes made on the seam.
#[derive(Debug, Clone, PartialEq)]
pub enum DiscordCall {
    Get,
    Available,
    Update(String, DiscordChannelPatch),
    Remove(String),
    Hire(String, Vec<String>),
    Fire(String),
}

struct Inner {
    adapter: AdapterState,
    discord_lost: bool,
    next_thread: u64,
    channels: Vec<DiscordChannel>,
    calls: Vec<DiscordCall>,
}

/// The Discord seam: it serves a small gateway cache, applies writes to it, and records calls.
#[derive(Clone)]
pub struct FakeDiscord {
    inner: Arc<Mutex<Inner>>,
}

fn launcher(id: &str) -> ScopeLauncher {
    ScopeLauncher {
        scope: Scope::Discord(id.to_owned()),
        set_here: None,
        effective: "direct".into(),
        inherited_from: InheritedFrom::Default,
        source: LauncherSource::Default,
    }
}

fn owner() -> DiscordUser {
    DiscordUser {
        id: OWNER.into(),
        name: Some("owner".into()),
    }
}

fn thread(id: &str, parent: &str, name: &str) -> DiscordThread {
    DiscordThread {
        id: id.to_owned(),
        name: name.to_owned(),
        parent_id: parent.to_owned(),
        agent_dir: Some(format!("agents/{name}-{id}")),
        last_active_at: at(),
        session: None,
    }
}

fn listened_channel() -> DiscordChannel {
    DiscordChannel {
        id: LISTENED.into(),
        name: "research".into(),
        guild_id: GUILD.into(),
        trigger: ChannelTrigger::Mention,
        allowed_users: AllowedUsers {
            source: AllowedUsersSource::Global,
            users: vec![owner()],
        },
        launcher: launcher(LISTENED),
        threads: vec![thread(THREAD, LISTENED, "paper-digest")],
    }
}

impl Default for FakeDiscord {
    fn default() -> Self {
        FakeDiscord {
            inner: Arc::new(Mutex::new(Inner {
                adapter: AdapterState::Connected,
                discord_lost: false,
                next_thread: FIRST_HIRED,
                channels: vec![listened_channel()],
                calls: Vec::new(),
            })),
        }
    }
}

impl FakeDiscord {
    /// The calls the routes made, in order.
    pub fn calls(&self) -> Vec<DiscordCall> {
        self.inner.lock().unwrap().calls.clone()
    }

    /// Moves the adapter to `state`.
    pub fn set_adapter(&self, state: AdapterState) {
        self.inner.lock().unwrap().adapter = state;
    }

    /// Discord has already lost the next fired thread (`discord_deleted: false`).
    pub fn lose_threads_on_discord(&self) {
        self.inner.lock().unwrap().discord_lost = true;
    }

    /// The channels listened in right now.
    pub fn listened(&self) -> Vec<DiscordChannel> {
        self.inner.lock().unwrap().channels.clone()
    }
}

fn not_found(what: &str) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("no such {what}"))
}

fn available_channels() -> Vec<AvailableChannel> {
    vec![
        AvailableChannel {
            id: LISTENED.into(),
            name: "research".into(),
            guild_id: GUILD.into(),
            kind: ChannelKind::Text,
        },
        AvailableChannel {
            id: UNLISTENED.into(),
            name: "announcements".into(),
            guild_id: GUILD.into(),
            kind: ChannelKind::Announcement,
        },
    ]
}

impl Inner {
    /// What a control call is refused with while the adapter isn't connected.
    fn refusal(&self) -> Option<ApiError> {
        match self.adapter {
            AdapterState::Connected => None,
            other => Some(adapter_unavailable(other)),
        }
    }
}

impl DiscordBackend for FakeDiscord {
    fn get(&self) -> BackendFuture<'_, DiscordOverview> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let mut inner = inner.lock().unwrap();
            inner.calls.push(DiscordCall::Get);
            if inner.adapter == AdapterState::Disabled {
                return Ok(DiscordOverview {
                    state: AdapterState::Disabled,
                    since: at(),
                    bot: None,
                    guilds: vec![],
                    dm_allowed_users: vec![],
                    channels: vec![],
                });
            }
            Ok(DiscordOverview {
                state: inner.adapter,
                since: at(),
                bot: Some(DiscordBot {
                    id: "5001".into(),
                    name: "helper".into(),
                }),
                guilds: vec![DiscordGuild {
                    id: GUILD.into(),
                    name: "workshop".into(),
                }],
                dm_allowed_users: vec![owner()],
                channels: inner.channels.clone(),
            })
        })
    }

    fn available(&self) -> BackendFuture<'_, AvailableChannelList> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let mut inner = inner.lock().unwrap();
            inner.calls.push(DiscordCall::Available);
            if let Some(refusal) = inner.refusal() {
                return Err(refusal);
            }
            Ok(AvailableChannelList {
                channels: available_channels(),
            })
        })
    }

    fn update(
        &self,
        channel_id: &str,
        patch: DiscordChannelPatch,
    ) -> BackendFuture<'_, DiscordChannel> {
        let inner = self.inner.clone();
        let channel_id = channel_id.to_owned();
        Box::pin(async move {
            let mut inner = inner.lock().unwrap();
            inner
                .calls
                .push(DiscordCall::Update(channel_id.clone(), patch.clone()));
            if let Some(refusal) = inner.refusal() {
                return Err(refusal);
            }
            let known = available_channels()
                .into_iter()
                .find(|channel| channel.id == channel_id)
                .ok_or_else(|| not_found("channel"))?;
            let allowed_users = match &patch.allowed_user_ids {
                None => AllowedUsers {
                    source: AllowedUsersSource::Global,
                    users: vec![owner()],
                },
                Some(ids) => AllowedUsers {
                    source: AllowedUsersSource::Channel,
                    users: ids
                        .iter()
                        .map(|id| DiscordUser {
                            id: id.clone(),
                            name: None,
                        })
                        .collect(),
                },
            };
            if let Some(channel) = inner.channels.iter_mut().find(|c| c.id == channel_id) {
                channel.trigger = patch.trigger;
                channel.allowed_users = allowed_users;
                return Ok(channel.clone());
            }
            let channel = DiscordChannel {
                id: known.id.clone(),
                name: known.name,
                guild_id: known.guild_id,
                trigger: patch.trigger,
                allowed_users,
                launcher: launcher(&known.id),
                threads: vec![],
            };
            inner.channels.push(channel.clone());
            Ok(channel)
        })
    }

    fn remove(&self, channel_id: &str) -> BackendFuture<'_, DiscordChannelRemoved> {
        let inner = self.inner.clone();
        let channel_id = channel_id.to_owned();
        Box::pin(async move {
            let mut inner = inner.lock().unwrap();
            inner.calls.push(DiscordCall::Remove(channel_id.clone()));
            if let Some(refusal) = inner.refusal() {
                return Err(refusal);
            }
            let at = inner
                .channels
                .iter()
                .position(|channel| channel.id == channel_id)
                .ok_or_else(|| not_found("listened channel"))?;
            let channel = inner.channels.remove(at);
            Ok(DiscordChannelRemoved {
                channel_id: channel.id,
                // The channel's own session and one per thread, all kept.
                sessions_kept: 1 + channel.threads.len() as u32,
            })
        })
    }

    fn hire(&self, channel_id: &str, names: Vec<String>) -> BackendFuture<'_, HireResult> {
        let inner = self.inner.clone();
        let channel_id = channel_id.to_owned();
        Box::pin(async move {
            let mut inner = inner.lock().unwrap();
            inner
                .calls
                .push(DiscordCall::Hire(channel_id.clone(), names.clone()));
            if let Some(refusal) = inner.refusal() {
                return Err(refusal);
            }
            let index = inner
                .channels
                .iter()
                .position(|channel| channel.id == channel_id)
                .ok_or_else(|| not_found("listened channel"))?;
            let mut hired = Vec::new();
            for name in &names {
                let id = inner.next_thread.to_string();
                inner.next_thread += 1;
                hired.push(thread(&id, &channel_id, name));
            }
            inner.channels[index].threads.extend(hired.clone());
            Ok(HireResult { threads: hired })
        })
    }

    fn fire(&self, thread_id: &str) -> BackendFuture<'_, FireResult> {
        let inner = self.inner.clone();
        let thread_id = thread_id.to_owned();
        Box::pin(async move {
            let mut inner = inner.lock().unwrap();
            inner.calls.push(DiscordCall::Fire(thread_id.clone()));
            if let Some(refusal) = inner.refusal() {
                return Err(refusal);
            }
            let discord_deleted = !inner.discord_lost;
            let fired = inner.channels.iter_mut().find_map(|channel| {
                let at = channel.threads.iter().position(|t| t.id == thread_id)?;
                Some(channel.threads.remove(at))
            });
            let fired = fired.ok_or_else(|| not_found("thread"))?;
            Ok(FireResult {
                thread_id: fired.id.clone(),
                discord_deleted,
                sessions_removed: 1,
                archived_to: fired.agent_dir.map(|dir| {
                    let entry = dir.trim_start_matches("agents/");
                    format!("agents/.archive/{entry}-{ARCHIVE_STAMP}")
                }),
            })
        })
    }
}
