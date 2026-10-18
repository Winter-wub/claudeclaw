//! Behavioural tests for the Discord routes (management-api §2.7), driven through the real
//! router like the rest of the server's tests.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};

use claudeclaw_api::adapter::AdapterState;
use claudeclaw_api::logs::AuditOutcome;
use claudeclaw_api::permission::TokenPermission;

use crate::state::AppState;
use crate::testing::*;
use crate::testing_discord::{
    ARCHIVE_STAMP, DiscordCall, FIRST_HIRED, GUILD, LISTENED, THREAD, UNLISTENED,
};

/// The `error.code` of a response body.
fn code(body: &Value) -> String {
    body["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// A request with a raw body, for the bodies a client must not send.
fn raw_request(method: Method, uri: &str, body: &'static str) -> Request<Body> {
    let mut request = request(method, uri, LOCAL);
    request
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    *request.body_mut() = Body::from(body);
    request
}

fn put_channel(id: &str, body: &Value) -> Request<Body> {
    json_request(
        Method::PUT,
        &format!("/v1/discord/channels/{id}"),
        LOCAL,
        body,
    )
}

fn hire(id: &str, body: &Value) -> Request<Body> {
    json_request(
        Method::POST,
        &format!("/v1/discord/channels/{id}/hire"),
        LOCAL,
        body,
    )
}

fn fire(id: &str) -> Request<Body> {
    json_request(
        Method::POST,
        &format!("/v1/discord/threads/{id}/fire"),
        LOCAL,
        &json!({}),
    )
}

fn delete_channel(id: &str) -> Request<Body> {
    request(Method::DELETE, &format!("/v1/discord/channels/{id}"), LOCAL)
}

fn mention_patch() -> Value {
    json!({"trigger": "every_message", "allowed_user_ids": null})
}

mod reads {
    use super::*;

    #[tokio::test]
    async fn the_overview_is_admin_only_and_never_audited() {
        let harness = Harness::new();
        harness
            .tokens
            .add("secret", "tok-1", vec![TokenPermission::StatusRead], false);

        for identity in [LOCAL, device("dev-1")] {
            let response = call(
                &harness.router(),
                request(Method::GET, "/v1/discord", identity),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            assert_eq!(body["state"], "connected");
            assert_eq!(body["bot"]["name"], "helper");
            assert_eq!(body["guilds"][0]["id"], GUILD);
            assert_eq!(body["dm_allowed_users"][0]["name"], "owner");
            let channel = &body["channels"][0];
            assert_eq!(channel["id"], LISTENED);
            assert_eq!(channel["trigger"], "mention");
            assert_eq!(channel["allowed_users"]["source"], "global");
            assert_eq!(channel["launcher"]["scope"], format!("discord:{LISTENED}"));
            assert_eq!(channel["threads"][0]["id"], THREAD);
            assert_eq!(channel["threads"][0]["parent_id"], LISTENED);
        }
        assert_eq!(
            harness.discord.calls(),
            [DiscordCall::Get, DiscordCall::Get]
        );

        // §1.3: Discord is in no token permission.
        let denied = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/discord", "secret"),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(code(&body_json(denied).await), "missing_permission");
        assert_eq!(harness.discord.calls().len(), 2);
        assert!(harness.audit.is_empty());
    }

    #[tokio::test]
    async fn a_disabled_adapter_still_answers_the_overview() {
        let harness = Harness::new();
        harness.discord.set_adapter(AdapterState::Disabled);
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/discord", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["state"], "disabled");
        assert_eq!(body["bot"], Value::Null);
        assert_eq!(body["channels"], json!([]));
        assert_eq!(body["guilds"], json!([]));
    }

    #[tokio::test]
    async fn the_channel_picker_lists_the_gateway_cache_for_admins_only() {
        let harness = Harness::new();
        harness
            .tokens
            .add("secret", "tok-1", vec![TokenPermission::StatusRead], false);

        let response = call(
            &harness.router(),
            request(
                Method::GET,
                "/v1/discord/available-channels",
                device("dev-1"),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["channels"][0]["id"], LISTENED);
        assert_eq!(body["channels"][0]["kind"], "text");
        assert_eq!(body["channels"][1]["id"], UNLISTENED);
        assert_eq!(body["channels"][1]["kind"], "announcement");
        assert_eq!(body["channels"][1]["guild_id"], GUILD);

        let denied = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/discord/available-channels", "secret"),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(harness.discord.calls(), [DiscordCall::Available]);
        assert!(harness.audit.is_empty());
    }
}

mod channels {
    use super::*;

    #[tokio::test]
    async fn listening_in_a_new_channel_returns_it_and_audits_its_id() {
        let harness = Harness::new();
        let body = json!({"trigger": "every_message", "allowed_user_ids": ["4002"]});
        let response = call(&harness.router(), put_channel(UNLISTENED, &body)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let channel = body_json(response).await;
        assert_eq!(channel["id"], UNLISTENED);
        assert_eq!(channel["name"], "announcements");
        assert_eq!(channel["trigger"], "every_message");
        assert_eq!(channel["allowed_users"]["source"], "channel");
        assert_eq!(
            channel["allowed_users"]["users"],
            json!([{"id": "4002", "name": null}])
        );
        assert_eq!(channel["threads"], json!([]));

        let calls = harness.discord.calls();
        assert!(matches!(
            &calls[..],
            [DiscordCall::Update(id, patch)] if id == UNLISTENED
                && patch.allowed_user_ids.as_deref() == Some(&["4002".to_owned()][..])
        ));

        let audit = harness.audit.entries();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "discord.channels.update");
        assert_eq!(audit[0].target.as_deref(), Some(UNLISTENED));
        assert_eq!(audit[0].outcome, AuditOutcome::Ok);
        assert_eq!(audit[0].principal, "local");
    }

    #[tokio::test]
    async fn editing_a_listened_channel_can_go_back_to_the_global_list() {
        let harness = Harness::new();
        let first = json!({"trigger": "every_message", "allowed_user_ids": ["4002"]});
        call(&harness.router(), put_channel(LISTENED, &first)).await;
        let response = call(&harness.router(), put_channel(LISTENED, &mention_patch())).await;
        let channel = body_json(response).await;
        assert_eq!(channel["allowed_users"]["source"], "global");
        assert_eq!(
            harness.discord.listened().len(),
            1,
            "edited, not duplicated"
        );
        assert_eq!(
            harness.discord.listened()[0].threads.len(),
            1,
            "its threads stay"
        );
    }

    #[tokio::test]
    async fn a_bad_id_or_body_is_a_400_that_never_reaches_the_backend_or_the_audit_target() {
        let harness = Harness::new();
        let router = harness.router();

        let too_long = "1".repeat(21);
        let bad_ids = ["abc", "12ab", too_long.as_str(), "-5", "1.5"];
        for id in bad_ids {
            for request in [
                put_channel(id, &mention_patch()),
                delete_channel(id),
                hire(id, &json!({"names": ["x"]})),
                fire(id),
            ] {
                let response = call(&router, request).await;
                assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{id}");
                let body = body_json(response).await;
                assert_eq!(code(&body), "bad_request", "{id}");
                assert!(!body.to_string().contains(id), "the id is not echoed");
            }
        }

        let bad_bodies = [
            json!({"trigger": "reaction", "allowed_user_ids": null}),
            json!({"trigger": "mention", "allowed_user_ids": ["not-a-snowflake"]}),
            json!({"trigger": "mention"}),
            json!({"allowed_user_ids": null}),
            json!([]),
        ];
        for body in bad_bodies {
            let response = call(&router, put_channel(LISTENED, &body)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(code(&body_json(response).await), "bad_request", "{body}");
        }
        let response = call(
            &router,
            raw_request(
                Method::PUT,
                &format!("/v1/discord/channels/{LISTENED}"),
                "not json",
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(code(&body_json(response).await), "bad_request");

        assert!(harness.discord.calls().is_empty());
        // A refused write is still a write attempt: it is audited, with no target to name.
        let audit = harness.audit.entries();
        assert!(!audit.is_empty());
        assert!(audit.iter().all(|entry| entry.target.is_none()));
        assert!(
            audit
                .iter()
                .all(|entry| entry.outcome == AuditOutcome::Error)
        );
    }

    #[tokio::test]
    async fn an_unknown_channel_is_a_404_audited_against_its_id() {
        let harness = Harness::new();
        let unknown = "2999";
        let response = call(&harness.router(), put_channel(unknown, &mention_patch())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(code(&body_json(response).await), "not_found");

        let response = call(&harness.router(), delete_channel(UNLISTENED)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "not listened in");

        let audit = harness.audit.entries();
        assert_eq!(audit[0].target.as_deref(), Some(unknown));
        assert_eq!(audit[0].outcome, AuditOutcome::Error);
        assert_eq!(audit[1].target.as_deref(), Some(UNLISTENED));
        assert_eq!(audit[1].action, "discord.channels.remove");
    }

    #[tokio::test]
    async fn stopping_listening_keeps_every_session() {
        let harness = Harness::new();
        let response = call(&harness.router(), delete_channel(LISTENED)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await,
            // The channel's session and the one thread's, both kept.
            json!({"channel_id": LISTENED, "sessions_kept": 2})
        );
        assert!(harness.discord.listened().is_empty());

        let overview = call(
            &harness.router(),
            request(Method::GET, "/v1/discord", LOCAL),
        )
        .await;
        assert_eq!(body_json(overview).await["channels"], json!([]));

        let audit = harness.audit.entries();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "discord.channels.remove");
        assert_eq!(audit[0].target.as_deref(), Some(LISTENED));

        // Gone now: a second remove is a 404, not a second success.
        let again = call(&harness.router(), delete_channel(LISTENED)).await;
        assert_eq!(again.status(), StatusCode::NOT_FOUND);
    }
}

mod threads {
    use super::*;

    #[tokio::test]
    async fn hiring_makes_one_thread_per_trimmed_name_with_its_agent_dir() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            hire(
                LISTENED,
                &json!({"names": ["  rust-notes ", "inbox-triage"]}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let threads = body["threads"].as_array().unwrap();
        assert_eq!(threads.len(), 2);
        let first = FIRST_HIRED.to_string();
        let second = (FIRST_HIRED + 1).to_string();
        assert_eq!(threads[0]["id"], first);
        assert_eq!(threads[0]["name"], "rust-notes");
        assert_eq!(threads[0]["parent_id"], LISTENED);
        assert_eq!(
            threads[0]["agent_dir"],
            format!("agents/rust-notes-{first}")
        );
        assert_eq!(threads[0]["session"], Value::Null);
        assert_eq!(threads[1]["id"], second);

        assert_eq!(
            harness.discord.calls(),
            [DiscordCall::Hire(
                LISTENED.into(),
                vec!["rust-notes".into(), "inbox-triage".into()]
            )],
            "the backend gets the trimmed names"
        );
        let audit = harness.audit.entries();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "discord.hire");
        assert_eq!(audit[0].target.as_deref(), Some(LISTENED));
        assert_eq!(audit[0].outcome, AuditOutcome::Ok);
    }

    #[tokio::test]
    async fn bad_hire_names_are_a_400_and_an_unlistened_channel_a_404() {
        let harness = Harness::new();
        let router = harness.router();
        let long = "a".repeat(101);
        let many = vec!["x"; 26];
        for body in [
            json!({"names": []}),
            json!({"names": ["   "]}),
            json!({"names": ["tab\there"]}),
            json!({"names": [long]}),
            json!({"names": many}),
            json!({"names": "one"}),
            json!({}),
        ] {
            let response = call(&router, hire(LISTENED, &body)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(code(&body_json(response).await), "bad_request", "{body}");
        }
        assert!(harness.discord.calls().is_empty());

        let response = call(&router, hire(UNLISTENED, &json!({"names": ["x"]}))).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(code(&body_json(response).await), "not_found");
        let audit = harness.audit.entries();
        assert_eq!(audit.last().unwrap().target.as_deref(), Some(UNLISTENED));
    }

    #[tokio::test]
    async fn firing_archives_the_agent_dir_under_a_relative_path() {
        let harness = Harness::new();
        let response = call(&harness.router(), fire(THREAD)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["thread_id"], THREAD);
        assert_eq!(body["discord_deleted"], true);
        assert_eq!(body["sessions_removed"], 1);
        let archived = body["archived_to"].as_str().unwrap();
        assert_eq!(
            archived,
            format!("agents/.archive/paper-digest-{THREAD}-{ARCHIVE_STAMP}")
        );
        assert!(!archived.starts_with('/') && !archived.contains(".."));

        // The thread is gone from the overview.
        let overview = call(
            &harness.router(),
            request(Method::GET, "/v1/discord", LOCAL),
        )
        .await;
        assert_eq!(
            body_json(overview).await["channels"][0]["threads"],
            json!([])
        );

        let audit = harness.audit.entries();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "discord.fire");
        assert_eq!(audit[0].target.as_deref(), Some(THREAD));
        assert_eq!(audit[0].outcome, AuditOutcome::Ok);

        // Fired once: the second call finds nothing.
        let again = call(&harness.router(), fire(THREAD)).await;
        assert_eq!(again.status(), StatusCode::NOT_FOUND);
        assert_eq!(code(&body_json(again).await), "not_found");
    }

    #[tokio::test]
    async fn a_thread_discord_already_lost_is_still_fired_locally() {
        let harness = Harness::new();
        harness.discord.lose_threads_on_discord();
        let response = call(&harness.router(), fire(THREAD)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["discord_deleted"], false);
        assert!(body["archived_to"].is_string());
    }

    #[tokio::test]
    async fn the_fire_body_is_empty_or_an_object() {
        let harness = Harness::new();
        let uri = format!("/v1/discord/threads/{THREAD}/fire");
        let bare = call(&harness.router(), request(Method::POST, &uri, LOCAL)).await;
        assert_eq!(bare.status(), StatusCode::OK, "a bare POST works");

        for body in ["[]", "\"now\"", "nope"] {
            let response = call(&harness.router(), raw_request(Method::POST, &uri, body)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(code(&body_json(response).await), "bad_request");
        }
        assert_eq!(harness.discord.calls().len(), 1, "only the bare POST fired");
    }
}

mod gating {
    use super::*;

    /// Every control route, as a fresh request: the five that need the adapter.
    fn control_requests() -> Vec<(&'static str, Request<Body>, &'static str)> {
        vec![
            (
                "discord.available",
                request(Method::GET, "/v1/discord/available-channels", LOCAL),
                "",
            ),
            (
                "discord.channels.update",
                put_channel(LISTENED, &mention_patch()),
                LISTENED,
            ),
            (
                "discord.channels.remove",
                delete_channel(LISTENED),
                LISTENED,
            ),
            (
                "discord.hire",
                hire(LISTENED, &json!({"names": ["x"]})),
                LISTENED,
            ),
            ("discord.fire", fire(THREAD), THREAD),
        ]
    }

    #[tokio::test]
    async fn control_routes_are_409_adapter_unavailable_unless_connected() {
        for state in [
            AdapterState::Disabled,
            AdapterState::Disconnected,
            AdapterState::Connecting,
            AdapterState::Reconnecting,
        ] {
            let harness = Harness::new();
            harness.discord.set_adapter(state);
            let wire = serde_json::to_value(state).unwrap();
            for (op, request, _) in control_requests() {
                let response = call(&harness.router(), request).await;
                assert_eq!(response.status(), StatusCode::CONFLICT, "{op} {wire}");
                // Not a transient outage: nothing promises a retry (that is §1.8's 503).
                assert!(response.headers().get(header::RETRY_AFTER).is_none());
                let body = body_json(response).await;
                assert_eq!(code(&body), "adapter_unavailable", "{op} {wire}");
                assert_eq!(body["error"]["details"]["transport"], "discord");
                assert_eq!(body["error"]["details"]["state"], wire);
                assert!(body["error"]["hint"].is_string());
            }
            // The four writes are audited as failures against the id they named.
            let audit = harness.audit.entries();
            assert_eq!(audit.len(), 4, "{wire}");
            for (entry, (op, _, target)) in audit.iter().zip(control_requests().iter().skip(1)) {
                assert_eq!(entry.action, *op);
                assert_eq!(entry.target.as_deref(), Some(*target));
                assert_eq!(entry.outcome, AuditOutcome::Error);
                assert_eq!(
                    entry.code,
                    Some(claudeclaw_api::error::ErrorCode::AdapterUnavailable)
                );
            }
        }
    }

    #[tokio::test]
    async fn a_token_cannot_reach_any_discord_route() {
        let harness = Harness::new();
        harness.tokens.add(
            "secret",
            "tok-1",
            vec![
                TokenPermission::Send,
                TokenPermission::JobsRun,
                TokenPermission::StatusRead,
                TokenPermission::LogsRead,
            ],
            false,
        );
        let routes = [
            (Method::GET, "/v1/discord".to_owned()),
            (Method::GET, "/v1/discord/available-channels".to_owned()),
            (Method::PUT, format!("/v1/discord/channels/{LISTENED}")),
            (Method::DELETE, format!("/v1/discord/channels/{LISTENED}")),
            (
                Method::POST,
                format!("/v1/discord/channels/{LISTENED}/hire"),
            ),
            (Method::POST, format!("/v1/discord/threads/{THREAD}/fire")),
        ];
        for (method, uri) in routes {
            let response = call(
                &harness.router(),
                bearer_json_request(method.clone(), &uri, "secret", &json!({})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
            assert_eq!(code(&body_json(response).await), "missing_permission");
        }
        assert!(harness.discord.calls().is_empty());
    }

    #[tokio::test]
    async fn an_unwired_discord_seam_fails_loudly_on_every_route() {
        let state = AppState::builder().build();
        let router = crate::router::local_app(state);
        let routes = [
            (Method::GET, "/v1/discord".to_owned()),
            (Method::GET, "/v1/discord/available-channels".to_owned()),
            (Method::PUT, format!("/v1/discord/channels/{LISTENED}")),
            (Method::DELETE, format!("/v1/discord/channels/{LISTENED}")),
            (
                Method::POST,
                format!("/v1/discord/channels/{LISTENED}/hire"),
            ),
            (Method::POST, format!("/v1/discord/threads/{THREAD}/fire")),
        ];
        let body = json!({"trigger": "mention", "allowed_user_ids": null, "names": ["x"]});
        for (method, uri) in routes {
            let response = call(&router, json_request(method.clone(), &uri, LOCAL, &body)).await;
            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{method} {uri}"
            );
            assert_eq!(code(&body_json(response).await), "internal");
        }
    }
}
