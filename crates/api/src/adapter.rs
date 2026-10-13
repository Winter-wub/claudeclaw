//! Chat adapter identity and connection state, shared by `DaemonStatus.adapters[]`,
//! `DaemonEvent`'s `adapter.changed` and `DiscordOverview` (management-api §2.1, §2.2, §2.7).
//! No ticket fixes the state values; the coordinator chose them for the api build.

use crate::wire::wire_enum;

wire_enum! {
    /// A chat transport (architecture §6 `chat::Transport`).
    pub enum Transport {
        Discord = "discord",
        Telegram = "telegram",
        Slack = "slack",
    }
}

wire_enum! {
    /// A chat adapter's connection (management-api §2.1, §2.2, §2.7).
    pub enum AdapterState {
        /// Starting, or logging in for the first time.
        Connecting = "connecting",
        Connected = "connected",
        /// The connection dropped; the adapter is retrying by itself.
        Reconnecting = "reconnecting",
        /// Stopped after an error it won't retry, such as a rejected token.
        Disconnected = "disconnected",
        /// Not configured, or turned off in settings.
        Disabled = "disabled",
    }
}

/// The schemas of this module, merged by [`crate::openapi`].
#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(Transport, AdapterState)))]
pub(crate) struct Doc;
