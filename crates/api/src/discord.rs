//! Discord channels and threads (management-api §2.7): `discord.get`, `discord.available`,
//! `discord.channels.update`, `discord.channels.remove`, `discord.hire` and `discord.fire`. These
//! routes go through the adapter's `chat::AdapterControl` (architecture §6, #21). Every object
//! carries its snowflake `id` as a string; the app shows names and copies the `id` (#16).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::adapter::AdapterState;
use crate::scopes::ScopeLauncher;
use crate::sessions::ScopeSession;
use crate::wire::wire_enum;

/// `discord.get` → the Discord screen (management-api §2.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DiscordOverview {
    /// The Discord adapter's connection.
    pub state: AdapterState,
    /// When the adapter entered `state`.
    pub since: DateTime<Utc>,
    /// The bot's own user; `null` until the gateway's first `READY`.
    #[schema(required = true)]
    pub bot: Option<DiscordBot>,
    pub guilds: Vec<DiscordGuild>,
    /// The users allowed to talk to the bot in DMs and in channels without their own list.
    pub dm_allowed_users: Vec<DiscordUser>,
    /// Every channel the bot listens in.
    pub channels: Vec<DiscordChannel>,
}

/// The bot's own Discord user (management-api §2.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscordBot {
    pub id: String,
    pub name: String,
}

/// A guild the bot is in (management-api §2.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscordGuild {
    pub id: String,
    pub name: String,
}

/// A Discord user (management-api §2.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscordUser {
    pub id: String,
    /// `null` when the gateway cache doesn't know the user.
    #[schema(required = true)]
    pub name: Option<String>,
}

/// A channel the bot listens in (management-api §2.7). Its Scope is `discord:<id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DiscordChannel {
    pub id: String,
    pub name: String,
    pub guild_id: String,
    pub trigger: ChannelTrigger,
    pub allowed_users: AllowedUsers,
    pub launcher: ScopeLauncher,
    pub threads: Vec<DiscordThread>,
}

wire_enum! {
    /// Which messages in a channel start a Turn (management-api §2.7).
    pub enum ChannelTrigger {
        /// Only messages that mention the bot.
        Mention = "mention",
        EveryMessage = "every_message",
    }
}

/// Who may talk to the bot in a channel (management-api §2.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AllowedUsers {
    /// `global` when the channel has no list of its own and uses `dm_allowed_users`.
    pub source: AllowedUsersSource,
    pub users: Vec<DiscordUser>,
}

wire_enum! {
    /// Where a channel's allowed users come from (management-api §2.7).
    pub enum AllowedUsersSource {
        /// `discord.allowedUserIds`.
        Global = "global",
        /// The channel's `discord.channelAllowedUserIds` entry.
        Channel = "channel",
    }
}

/// A thread under a listened channel (management-api §2.7). Its Scope is `discord:<id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DiscordThread {
    pub id: String,
    pub name: String,
    /// The channel the thread is under.
    pub parent_id: String,
    /// The thread's working dir, `agents/<slug>-<thread id>`; `null` for a thread that wasn't
    /// hired into and has none.
    #[schema(required = true)]
    pub agent_dir: Option<String>,
    pub last_active_at: DateTime<Utc>,
    /// The session of the thread's effective Launcher; `null` when there is none.
    #[schema(required = true)]
    pub session: Option<ScopeSession>,
}

/// `discord.available` → the channels the bot could listen in, from the gateway cache
/// (management-api §2.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AvailableChannelList {
    pub channels: Vec<AvailableChannel>,
}

/// One channel in the "Listen in a channel…" picker (management-api §2.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AvailableChannel {
    pub id: String,
    pub name: String,
    pub guild_id: String,
    pub kind: ChannelKind,
}

wire_enum! {
    /// The kind of a guild channel the bot can listen in (management-api §2.7).
    pub enum ChannelKind {
        Text = "text",
        Announcement = "announcement",
        /// A forum; its posts are threads under it.
        Forum = "forum",
        /// A voice channel's text chat.
        Voice = "voice",
    }
}

/// `discord.channels.update` body: listen in a channel, or change how (management-api §2.7).
/// Writes `listenChannels` and `channelAllowedUserIds`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscordChannelPatch {
    pub trigger: ChannelTrigger,
    /// The channel's own allow-list; `null` uses the global one.
    #[schema(required = true)]
    pub allowed_user_ids: Option<Vec<String>>,
}

/// `discord.channels.remove` → `{channel_id, sessions_kept}` (management-api §2.7). The channel
/// leaves `listenChannels` and `channelAllowedUserIds`; its sessions are untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscordChannelRemoved {
    pub channel_id: String,
    /// The channel's and its threads' sessions, all kept.
    pub sessions_kept: u32,
}

/// `discord.hire` body: one public thread per name under the channel (management-api §2.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HireRequest {
    pub names: Vec<String>,
}

/// `discord.hire` → the new threads, each with its agent dir (management-api §2.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HireResult {
    pub threads: Vec<DiscordThread>,
}

/// `discord.fire` → what was removed and where the agent dir went (management-api §2.7, #16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FireResult {
    pub thread_id: String,
    /// Whether the Discord thread was deleted; `false` when Discord had already lost it.
    pub discord_deleted: bool,
    pub sessions_removed: u32,
    /// The archived dir, `agents/.archive/<slug>-<id>-<UTC stamp>`; `null` when the thread had
    /// no agent dir. Listed and restorable in §2.8.
    #[schema(required = true)]
    pub archived_to: Option<String>,
}

/// The schemas of this module, merged by [`crate::openapi`].
#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    DiscordOverview,
    DiscordBot,
    DiscordGuild,
    DiscordUser,
    DiscordChannel,
    ChannelTrigger,
    AllowedUsers,
    AllowedUsersSource,
    DiscordThread,
    AvailableChannelList,
    AvailableChannel,
    ChannelKind,
    DiscordChannelPatch,
    DiscordChannelRemoved,
    HireRequest,
    HireResult,
    FireResult,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::scope::Scope;
    use crate::scopes::{InheritedFrom, LauncherSource};
    use crate::sessions::SessionStatus;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, hour, 0, 0).unwrap()
    }

    fn thread() -> DiscordThread {
        DiscordThread {
            id: "5678".into(),
            name: "paper-digest".into(),
            parent_id: "1234".into(),
            agent_dir: Some("agents/paper-digest-5678".into()),
            last_active_at: at(8),
            session: Some(ScopeSession {
                launcher: "direct".into(),
                session_id: "s-1".into(),
                status: SessionStatus::InUse,
            }),
        }
    }

    fn thread_wire() -> serde_json::Value {
        json!({
            "id": "5678",
            "name": "paper-digest",
            "parent_id": "1234",
            "agent_dir": "agents/paper-digest-5678",
            "last_active_at": "2026-10-14T08:00:00Z",
            "session": {"launcher": "direct", "session_id": "s-1", "status": "in_use"},
        })
    }

    #[test]
    fn overview_shape() {
        let overview = DiscordOverview {
            state: AdapterState::Connected,
            since: at(6),
            bot: Some(DiscordBot {
                id: "42".into(),
                name: "helper".into(),
            }),
            guilds: vec![DiscordGuild {
                id: "7".into(),
                name: "workshop".into(),
            }],
            dm_allowed_users: vec![
                DiscordUser {
                    id: "11".into(),
                    name: Some("owner".into()),
                },
                DiscordUser {
                    id: "12".into(),
                    name: None,
                },
            ],
            channels: vec![DiscordChannel {
                id: "1234".into(),
                name: "research".into(),
                guild_id: "7".into(),
                trigger: ChannelTrigger::EveryMessage,
                allowed_users: AllowedUsers {
                    source: AllowedUsersSource::Channel,
                    users: vec![DiscordUser {
                        id: "11".into(),
                        name: Some("owner".into()),
                    }],
                },
                launcher: ScopeLauncher {
                    scope: Scope::Discord("1234".into()),
                    set_here: Some("/opt/launchers/work".into()),
                    effective: "/opt/launchers/work".into(),
                    inherited_from: InheritedFrom::Scope(Scope::Discord("1234".into())),
                    source: LauncherSource::Settings,
                },
                threads: vec![thread()],
            }],
        };
        let wire = json!({
            "state": "connected",
            "since": "2026-10-14T06:00:00Z",
            "bot": {"id": "42", "name": "helper"},
            "guilds": [{"id": "7", "name": "workshop"}],
            "dm_allowed_users": [{"id": "11", "name": "owner"}, {"id": "12", "name": null}],
            "channels": [{
                "id": "1234",
                "name": "research",
                "guild_id": "7",
                "trigger": "every_message",
                "allowed_users": {"source": "channel", "users": [{"id": "11", "name": "owner"}]},
                "launcher": {
                    "scope": "discord:1234",
                    "set_here": "/opt/launchers/work",
                    "effective": "/opt/launchers/work",
                    "inherited_from": "discord:1234",
                    "source": "settings",
                },
                "threads": [thread_wire()],
            }],
        });
        assert_eq!(serde_json::to_value(&overview).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<DiscordOverview>(wire).unwrap(),
            overview
        );
    }

    #[test]
    fn a_disabled_adapter_sends_its_nulls() {
        let overview = DiscordOverview {
            state: AdapterState::Disabled,
            since: at(6),
            bot: None,
            guilds: vec![],
            dm_allowed_users: vec![],
            channels: vec![],
        };
        let wire = json!({
            "state": "disabled",
            "since": "2026-10-14T06:00:00Z",
            "bot": null,
            "guilds": [],
            "dm_allowed_users": [],
            "channels": [],
        });
        assert_eq!(serde_json::to_value(&overview).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<DiscordOverview>(wire).unwrap(),
            overview
        );

        let unhired = DiscordThread {
            agent_dir: None,
            session: None,
            ..thread()
        };
        let wire = serde_json::to_value(&unhired).unwrap();
        assert_eq!(wire["agent_dir"], json!(null));
        assert_eq!(wire["session"], json!(null));
    }

    #[test]
    fn available_channels_shape() {
        let list = AvailableChannelList {
            channels: vec![AvailableChannel {
                id: "99".into(),
                name: "announcements".into(),
                guild_id: "7".into(),
                kind: ChannelKind::Announcement,
            }],
        };
        let wire = json!({"channels": [
            {"id": "99", "name": "announcements", "guild_id": "7", "kind": "announcement"},
        ]});
        assert_eq!(serde_json::to_value(&list).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<AvailableChannelList>(wire).unwrap(),
            list
        );
    }

    #[test]
    fn channel_patch_and_remove_shapes() {
        let patch = DiscordChannelPatch {
            trigger: ChannelTrigger::Mention,
            allowed_user_ids: Some(vec!["11".into()]),
        };
        let wire = json!({"trigger": "mention", "allowed_user_ids": ["11"]});
        assert_eq!(serde_json::to_value(&patch).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<DiscordChannelPatch>(wire).unwrap(),
            patch
        );

        let global = DiscordChannelPatch {
            trigger: ChannelTrigger::EveryMessage,
            allowed_user_ids: None,
        };
        let wire = json!({"trigger": "every_message", "allowed_user_ids": null});
        assert_eq!(serde_json::to_value(&global).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<DiscordChannelPatch>(wire).unwrap(),
            global
        );

        let removed = DiscordChannelRemoved {
            channel_id: "1234".into(),
            sessions_kept: 3,
        };
        let wire = json!({"channel_id": "1234", "sessions_kept": 3});
        assert_eq!(serde_json::to_value(&removed).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<DiscordChannelRemoved>(wire).unwrap(),
            removed
        );
    }

    #[test]
    fn hire_and_fire_shapes() {
        let request = HireRequest {
            names: vec!["paper-digest".into(), "rust-notes".into()],
        };
        let wire = json!({"names": ["paper-digest", "rust-notes"]});
        assert_eq!(serde_json::to_value(&request).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<HireRequest>(wire).unwrap(),
            request
        );

        let result = HireResult {
            threads: vec![thread()],
        };
        let wire = json!({"threads": [thread_wire()]});
        assert_eq!(serde_json::to_value(&result).unwrap(), wire);
        assert_eq!(serde_json::from_value::<HireResult>(wire).unwrap(), result);

        let fired = FireResult {
            thread_id: "5678".into(),
            discord_deleted: true,
            sessions_removed: 1,
            archived_to: Some("agents/.archive/paper-digest-5678-20261014T090000Z".into()),
        };
        let wire = json!({
            "thread_id": "5678",
            "discord_deleted": true,
            "sessions_removed": 1,
            "archived_to": "agents/.archive/paper-digest-5678-20261014T090000Z",
        });
        assert_eq!(serde_json::to_value(&fired).unwrap(), wire);
        assert_eq!(serde_json::from_value::<FireResult>(wire).unwrap(), fired);

        let no_dir = FireResult {
            archived_to: None,
            ..fired
        };
        assert_eq!(
            serde_json::to_value(&no_dir).unwrap()["archived_to"],
            json!(null)
        );
    }

    #[test]
    fn unknown_values_parse() {
        assert_eq!(
            serde_json::from_value::<ChannelTrigger>(json!("reaction")).unwrap(),
            ChannelTrigger::Unknown
        );
        assert_eq!(
            serde_json::from_value::<AllowedUsersSource>(json!("role")).unwrap(),
            AllowedUsersSource::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ChannelKind>(json!("stage")).unwrap(),
            ChannelKind::Unknown
        );
    }
}
