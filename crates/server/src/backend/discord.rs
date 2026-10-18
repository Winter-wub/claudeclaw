//! The Discord seam (management-api §2.7: `discord.get`, `discord.available`,
//! `discord.channels.update`, `discord.channels.remove`, `discord.hire` and `discord.fire`;
//! #16, #21, architecture §6).
//!
//! The server depends on neither the `discord` nor the `chat` crate (architecture §3.1), so the
//! composition root wires this to the adapter's `chat::AdapterControl` (`overview`,
//! `available_channels`, `hire`, `fire`) plus core's session and `agents/` work: the adapter
//! makes the transport calls, core moves the agent dir into `agents/.archive/` on a fire.
//!
//! Every method but [`DiscordBackend::get`] answers `409 adapter_unavailable` while the adapter is
//! disabled or not connected; [`adapter_unavailable`] builds that error. `get` still answers then,
//! with `state` and empty lists. Ids in the arguments are Discord snowflakes the route has
//! already checked for shape; whether they name anything is the backend's `404 not_found`.

use claudeclaw_api::adapter::{AdapterState, Transport};
use claudeclaw_api::discord::{
    AvailableChannelList, DiscordChannel, DiscordChannelPatch, DiscordChannelRemoved,
    DiscordOverview, FireResult, HireResult,
};
use claudeclaw_api::error::{ApiError, ErrorCode};

use super::BackendFuture;

/// The Discord routes (management-api §2.7). The composition root wires this to the Discord
/// adapter's `chat::AdapterControl`, to `settings.json`'s `listenChannels` and
/// `channelAllowedUserIds`, and to core's sessions and `agents/`.
pub trait DiscordBackend: Send + Sync + 'static {
    /// The Discord screen from the adapter's gateway cache (§2.7). Answers with the adapter's
    /// `state` and empty lists while it is disabled, so the app can say why.
    fn get(&self) -> BackendFuture<'_, DiscordOverview>;

    /// The channels the bot could listen in, from the gateway cache (§2.7).
    fn available(&self) -> BackendFuture<'_, AvailableChannelList>;

    /// Listens in a channel, or changes how (§2.7): writes `listenChannels` and
    /// `channelAllowedUserIds`. `404 not_found` when the gateway doesn't know the channel.
    fn update(
        &self,
        channel_id: &str,
        patch: DiscordChannelPatch,
    ) -> BackendFuture<'_, DiscordChannel>;

    /// Stops listening in a channel (§2.7): it leaves `listenChannels` and
    /// `channelAllowedUserIds`, and its sessions and its threads' sessions are untouched.
    /// `404 not_found` when the channel isn't listened in.
    fn remove(&self, channel_id: &str) -> BackendFuture<'_, DiscordChannelRemoved>;

    /// One public thread per name under a listened channel (§2.7): auto-archive 3 days, a
    /// confirmation post, and an agent dir `agents/<slug>-<thread id>/`. `404 not_found` when
    /// the channel isn't listened in. The names are trimmed and 1..=100 characters.
    fn hire(&self, channel_id: &str, names: Vec<String>) -> BackendFuture<'_, HireResult>;

    /// Deletes the Discord thread, removes its session, then moves the agent dir to
    /// `agents/.archive/<slug>-<id>-<UTC stamp>/` (§2.7, #16). `archived_to` is relative to the
    /// data dir. `404 not_found` when the thread isn't one the bot knows.
    fn fire(&self, thread_id: &str) -> BackendFuture<'_, FireResult>;
}

/// `409 adapter_unavailable` (§2.7): the adapter is `state`, which is `disabled` when it isn't
/// configured or is turned off, and anything else but `connected` while it can't take the call.
pub fn adapter_unavailable(state: AdapterState) -> ApiError {
    let name = serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let transport = serde_json::to_value(Transport::Discord)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    ApiError::new(
        ErrorCode::AdapterUnavailable,
        format!("the Discord adapter is {name}"),
    )
    .with_hint("turn the adapter on in settings or wait for it to connect")
    .with_detail("transport", transport)
    .with_detail("state", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_names_the_transport_and_the_adapters_state() {
        let error = adapter_unavailable(AdapterState::Disabled);
        assert_eq!(error.error.code, ErrorCode::AdapterUnavailable);
        assert_eq!(error.status(), 409);
        assert_eq!(error.error.details["transport"], "discord");
        assert_eq!(error.error.details["state"], "disabled");
        assert!(error.error.hint.is_some());
    }
}
