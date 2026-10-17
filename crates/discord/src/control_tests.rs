//! Tests for the Management API's Discord routes ([`super`]) against the outbound side's fake
//! Discord REST server (`crate::rest::test_server`), so the crate has one fake server.

use std::sync::Arc;

use claudeclaw_chat::AdapterState;
use twilight_model::channel::ChannelType;
use twilight_model::id::Id;

use super::*;
use crate::gateway::Shared;
use crate::rest::test_server::Server;

/// A [`Shared`] whose cached REST client points at `server`.
fn shared(server: &Server) -> Arc<Shared> {
    let shared = Arc::new(Shared::new());
    shared.set_rest(Some(Arc::new(server.connect())));
    shared
}

/// How many requests `server` answered matching `method` and ending with `suffix`.
fn count(server: &Server, method: &str, suffix: &str) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| request.method == method && request.path.ends_with(suffix))
        .count()
}

#[test]
fn channel_kind_maps_only_the_listenable_types() {
    assert_eq!(
        channel_kind(ChannelType::GuildText),
        Some(ChannelKind::Text)
    );
    assert_eq!(
        channel_kind(ChannelType::GuildAnnouncement),
        Some(ChannelKind::Announcement)
    );
    assert_eq!(
        channel_kind(ChannelType::GuildForum),
        Some(ChannelKind::Forum)
    );
    assert_eq!(
        channel_kind(ChannelType::GuildVoice),
        Some(ChannelKind::Voice)
    );
    assert_eq!(channel_kind(ChannelType::Private), None);
    assert_eq!(channel_kind(ChannelType::Group), None);
}

#[tokio::test]
async fn hire_creates_a_thread_tracks_it_and_posts_a_confirmation() {
    let server = Server::start().await;
    server.canned(
        "POST",
        "/channels/7/threads",
        201,
        r#"{"id":"77","type":11,"name":"Researcher","parent_id":"7"}"#,
    );
    let shared = shared(&server);
    let control = DiscordControl::new(Arc::clone(&shared));

    let created = control
        .hire("7".to_owned(), vec!["Researcher".to_owned()])
        .await
        .expect("hire");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].id, "77");
    assert_eq!(created[0].name, "Researcher");
    assert_eq!(created[0].parent_id, "7");

    // The thread was created once and got its confirmation post.
    assert_eq!(count(&server, "POST", "/channels/7/threads"), 1);
    assert_eq!(count(&server, "POST", "/channels/77/messages"), 1);
    let confirmation = server
        .requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.path.ends_with("/channels/77/messages"))
        .expect("the confirmation was posted");
    assert!(confirmation.text().contains("Researcher"));
    // And the cache learned it, with the agent dir key.
    let track = shared.thread(&Id::new(77)).expect("tracked");
    assert_eq!(track.parent, Id::new(7));
    assert_eq!(track.agent_name.as_deref(), Some("researcher-77"));
}

#[tokio::test]
async fn hire_rejects_a_non_snowflake_channel_without_a_request() {
    let server = Server::start().await;
    let shared = shared(&server);
    let control = DiscordControl::new(shared);
    let error = control
        .hire("not-a-channel".to_owned(), vec!["x".to_owned()])
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AdapterError::NotFound {
            what: "channel",
            ..
        }
    ));
    assert_eq!(count(&server, "POST", "/threads"), 0);
}

#[tokio::test]
async fn fire_deletes_the_thread_and_reports_already_gone() {
    let server = Server::start().await;
    server.canned("DELETE", "/channels/77", 204, "");
    let shared = shared(&server);
    let control = DiscordControl::new(Arc::clone(&shared));

    let done = control.fire("77".to_owned()).await.expect("fire");
    assert_eq!(done.thread_id, "77");
    assert!(done.deleted);
    assert!(shared.thread(&Id::new(77)).is_none());

    // A thread Discord no longer has is a success with `deleted: false`.
    let done = control.fire("78".to_owned()).await.expect("fire");
    assert_eq!(done.thread_id, "78");
    assert!(!done.deleted);
}

#[tokio::test]
async fn overview_reports_the_connection_state() {
    let server = Server::start().await;
    let shared = shared(&server);
    shared.set_state(AdapterState::Connected);
    let control = DiscordControl::new(shared);
    let overview = control.overview().await.expect("overview");
    assert_eq!(overview.state, AdapterState::Connected);
    assert!(overview.bot.is_none());
    assert!(overview.guilds.is_empty());
    assert!(
        control
            .available_channels()
            .await
            .expect("channels")
            .is_empty()
    );
}
