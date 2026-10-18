//! Behavioural tests for the Scopes, Launchers and sessions routes (management-api §2.5): the
//! status and error codes the table fixes, the Scope key in the path (§1.7), the Test launcher's
//! join and Deadline (§1.9, #40), and `sessions.compact` as a Turn.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};

use claudeclaw_api::logs::AuditOutcome;
use claudeclaw_api::permission::TokenPermission;
use claudeclaw_api::scope::Scope;
use claudeclaw_api::scopes::{LauncherSet, LauncherTestRequest};
use claudeclaw_api::sessions::SessionResetRequest;
use claudeclaw_api::turns::{TurnOutcome, TurnPurpose, TurnState};

use super::{code, required};
use crate::testing::*;

/// A request whose body is sent exactly as written, for the malformed ones.
fn raw(method: Method, uri: &str, body: &str) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap();
    request.extensions_mut().insert(LOCAL);
    request
}

async fn status_and_code(harness: &Harness, request: Request<Body>) -> (StatusCode, String) {
    let response = call(&harness.router(), request).await;
    let status = response.status();
    (status, code(&body_json(response).await))
}

mod scopes {
    use super::*;

    #[tokio::test]
    async fn the_list_is_whole_and_filters_by_kind() {
        let harness = Harness::new();
        let all = call(&harness.router(), request(Method::GET, "/v1/scopes", LOCAL)).await;
        assert_eq!(all.status(), StatusCode::OK);
        let body = body_json(all).await;
        assert_eq!(body["scopes"].as_array().unwrap().len(), 3);
        assert_eq!(body["scopes"][0]["scope"], "global");

        let threads = call(
            &harness.router(),
            request(Method::GET, "/v1/scopes?kind=thread", LOCAL),
        )
        .await;
        let body = body_json(threads).await;
        assert_eq!(body["scopes"].as_array().unwrap().len(), 1);
        assert_eq!(body["scopes"][0]["scope"], "discord:5678");
        assert_eq!(harness.scopes.list_calls().len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_kind_is_a_400() {
        let harness = Harness::new();
        for uri in ["/v1/scopes?kind=cloud", "/v1/scopes?kind=unknown"] {
            let (status, error) = status_and_code(&harness, request(Method::GET, uri, LOCAL)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(error, "bad_request", "{uri}");
        }
        assert!(harness.scopes.list_calls().is_empty());
    }

    #[tokio::test]
    async fn every_route_here_is_admin_only() {
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
        for (method, uri) in [
            (Method::GET, "/v1/scopes"),
            (Method::GET, "/v1/launchers"),
            (Method::PUT, "/v1/scopes/global/launcher"),
            (Method::DELETE, "/v1/scopes/discord:1/launcher"),
            (Method::POST, "/v1/launchers/test"),
            (Method::GET, "/v1/sessions"),
            (Method::GET, "/v1/sessions/s-1"),
            (Method::GET, "/v1/sessions/s-1/messages"),
            (Method::POST, "/v1/scopes/global/sessions/reset"),
            (Method::POST, "/v1/scopes/global/sessions/compact"),
            (Method::DELETE, "/v1/scopes/global/sessions"),
        ] {
            let response = call(&harness.router(), bearer_request(method, uri, "secret")).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
            let body = body_json(response).await;
            assert_eq!(code(&body), "missing_permission", "{uri}");
            assert_eq!(required(&body), "admin", "{uri}");
        }
    }
}

mod launchers {
    use super::*;

    fn set(launcher: &str) -> LauncherSet {
        LauncherSet {
            launcher: launcher.into(),
        }
    }

    #[tokio::test]
    async fn the_overview_is_served() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/launchers", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["default"], "direct");
        assert_eq!(body["picker"][1], WORK_LAUNCHER);
    }

    #[tokio::test]
    async fn set_answers_200_and_is_audited_against_the_scope() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::PUT,
                "/v1/scopes/discord:1234/launcher",
                LOCAL,
                &set(WORK_LAUNCHER),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["scope"], "discord:1234");
        assert_eq!(body["effective"], WORK_LAUNCHER);
        assert_eq!(body["session"], "new");
        assert_eq!(
            harness.scopes.set_calls(),
            [(Scope::Discord("1234".into()), WORK_LAUNCHER.to_owned())]
        );

        let entry = &harness.audit.entries()[0];
        assert_eq!(entry.action, "launchers.set");
        assert_eq!(entry.target.as_deref(), Some("discord:1234"));
        assert_eq!(entry.outcome, AuditOutcome::Ok);
    }

    #[tokio::test]
    async fn direct_is_set_on_global_and_an_agent_scope_is_percent_decoded() {
        let harness = Harness::new();
        for uri in [
            "/v1/scopes/global/launcher",
            "/v1/scopes/agent%3Aresearcher/launcher",
        ] {
            let response = call(
                &harness.router(),
                json_request(Method::PUT, uri, LOCAL, &set("direct")),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
        }
        assert_eq!(
            harness.scopes.set_calls(),
            [
                (Scope::Global, "direct".to_owned()),
                (Scope::Agent("researcher".into()), "direct".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn a_launcher_that_cannot_run_is_422_and_the_failure_is_audited() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::PUT,
                "/v1/scopes/global/launcher",
                LOCAL,
                &set(BROKEN_LAUNCHER),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(code(&body_json(response).await), "launcher_not_runnable");
        let entry = &harness.audit.entries()[0];
        assert_eq!(entry.outcome, AuditOutcome::Error);
        assert_eq!(entry.target.as_deref(), Some("global"));
    }

    #[tokio::test]
    async fn a_job_scope_is_409_use_job_frontmatter_for_both_writes() {
        let harness = Harness::new();
        for scope in ["job%3Adaily-report", "job%3Aresearcher%2Fpaper-digest"] {
            let (status, error) = status_and_code(
                &harness,
                json_request(
                    Method::PUT,
                    &format!("/v1/scopes/{scope}/launcher"),
                    LOCAL,
                    &set("direct"),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{scope}");
            assert_eq!(error, "use_job_frontmatter", "{scope}");

            let (status, error) = status_and_code(
                &harness,
                request(
                    Method::DELETE,
                    &format!("/v1/scopes/{scope}/launcher"),
                    LOCAL,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{scope}");
            assert_eq!(error, "use_job_frontmatter", "{scope}");
        }
        assert!(harness.scopes.set_calls().is_empty());
        assert!(harness.scopes.unset_calls().is_empty());
    }

    #[tokio::test]
    async fn a_malformed_scope_is_400_invalid_scope() {
        let harness = Harness::new();
        for uri in [
            "/v1/scopes/nope/launcher",
            "/v1/scopes/discord%3Aabc/launcher",
            "/v1/scopes/job%3A/launcher",
            "/v1/scopes/agent%3Aa%2Fb/launcher",
        ] {
            let (status, error) = status_and_code(
                &harness,
                json_request(Method::PUT, uri, LOCAL, &set("direct")),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(error, "invalid_scope", "{uri}");
            let (status, error) =
                status_and_code(&harness, request(Method::DELETE, uri, LOCAL)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(error, "invalid_scope", "{uri}");
        }
        assert!(harness.scopes.set_calls().is_empty());
    }

    #[tokio::test]
    async fn a_launcher_value_is_direct_or_an_absolute_path() {
        let harness = Harness::new();
        for bad in ["", "Direct", "relative/launcher", "./launcher", "/opt/x\ny"] {
            let (status, error) = status_and_code(
                &harness,
                json_request(Method::PUT, "/v1/scopes/global/launcher", LOCAL, &set(bad)),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}");
            assert_eq!(error, "invalid_value", "{bad:?}");
        }
        for body in ["{}", r#"["direct"]"#, "direct"] {
            let (status, error) = status_and_code(
                &harness,
                raw(Method::PUT, "/v1/scopes/global/launcher", body),
            )
            .await;
            assert_eq!(
                (status, error.as_str()),
                (StatusCode::BAD_REQUEST, "bad_request"),
                "{body}"
            );
        }
        assert!(harness.scopes.set_calls().is_empty());
    }

    #[tokio::test]
    async fn unset_inherits_again_and_refuses_global() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::DELETE, "/v1/scopes/discord:1234/launcher", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["effective"], "direct");
        assert_eq!(body["inherited_from"], "default");
        assert_eq!(harness.audit.entries()[0].action, "launchers.unset");

        let (status, error) = status_and_code(
            &harness,
            request(Method::DELETE, "/v1/scopes/global/launcher", LOCAL),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error, "invalid_scope", "set direct instead (§2.5)");
        assert_eq!(
            harness.scopes.unset_calls(),
            [Scope::Discord("1234".into())],
            "global never reached the backend"
        );
        // The refusal is a failed mutation in the audit log, against the Scope.
        let entry = &harness.audit.entries()[1];
        assert_eq!(entry.outcome, AuditOutcome::Error);
        assert_eq!(entry.target.as_deref(), Some("global"));
    }

    #[tokio::test]
    async fn the_test_runs_with_the_defaults_and_is_audited() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/launchers/test",
                LOCAL,
                &LauncherTestRequest::default(),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["launcher"], "direct");
        assert_eq!(body["scope"], "global");
        assert_eq!(body["checks"][0]["id"], "runnable");
        assert_eq!(
            harness.scopes.test_calls(),
            [LauncherTestRequest::default()]
        );

        let entry = &harness.audit.entries()[0];
        assert_eq!(entry.action, "launchers.test");
        assert_eq!(entry.target.as_deref(), Some("global"));

        // No body at all is the same defaults.
        let bare = call(
            &harness.router(),
            request(Method::POST, "/v1/launchers/test", LOCAL),
        )
        .await;
        assert_eq!(bare.status(), StatusCode::OK);
        assert_eq!(
            harness.scopes.test_calls()[1],
            LauncherTestRequest::default()
        );
    }

    #[tokio::test]
    async fn timeout_s_is_10_to_600_and_422_outside() {
        let harness = Harness::new();
        for (timeout, ok) in [
            (9, false),
            (10, true),
            (120, true),
            (600, true),
            (601, false),
            (0, false),
            (-1, false),
            (5_000_000_000_i64, false),
        ] {
            let body = format!(r#"{{"timeout_s": {timeout}}}"#);
            let (status, error) =
                status_and_code(&harness, raw(Method::POST, "/v1/launchers/test", &body)).await;
            if ok {
                assert_eq!(status, StatusCode::OK, "{timeout}");
            } else {
                assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{timeout}");
                assert_eq!(error, "invalid_value", "{timeout}");
            }
        }
        assert_eq!(
            harness.scopes.test_calls().len(),
            3,
            "a refused timeout never starts a test"
        );
    }

    #[tokio::test]
    async fn a_malformed_test_request_is_refused_before_a_test_starts() {
        let harness = Harness::new();
        for (body, expected) in [
            ("not json", "bad_request"),
            ("[]", "bad_request"),
            (r#"{"timeout_s": "soon"}"#, "bad_request"),
            (r#"{"scope": "nope"}"#, "invalid_scope"),
            (r#"{"scope": "discord:abc"}"#, "invalid_scope"),
            (r#"{"launcher": "relative"}"#, "invalid_value"),
        ] {
            let (_, error) =
                status_and_code(&harness, raw(Method::POST, "/v1/launchers/test", body)).await;
            assert_eq!(error, expected, "{body}");
        }
        assert!(harness.scopes.test_calls().is_empty());
    }

    #[tokio::test]
    async fn a_test_names_the_launcher_and_scope_it_was_asked_for() {
        let harness = Harness::new();
        let request = LauncherTestRequest {
            launcher: Some(WORK_LAUNCHER.into()),
            scope: Scope::Job("daily-report".into()),
            timeout_s: 30,
        };
        let response = call(
            &harness.router(),
            json_request(Method::POST, "/v1/launchers/test", LOCAL, &request),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["launcher"], WORK_LAUNCHER);
        assert_eq!(body["scope"], "job:daily-report");
        assert_eq!(harness.scopes.test_calls(), [request]);
        assert_eq!(
            harness.audit.entries()[0].target.as_deref(),
            Some("job:daily-report")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_request_for_a_launcher_under_test_joins_it() {
        let harness = Harness::new();
        harness.scopes.set_test_delay(Duration::from_secs(5));
        let router = harness.router();
        let body = LauncherTestRequest::default();
        let (first, second) = tokio::join!(
            call(
                &router,
                json_request(Method::POST, "/v1/launchers/test", LOCAL, &body)
            ),
            call(
                &router,
                json_request(Method::POST, "/v1/launchers/test", LOCAL, &body)
            ),
        );
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(
            body_json(first).await["checks"],
            body_json(second).await["checks"]
        );
        // Each request reached the seam once; the seam started one test for both (#40).
        assert_eq!(harness.scopes.test_calls().len(), 2);
        assert_eq!(harness.scopes.test_runs(), 1);

        // Once it has finished, the next request starts a fresh test.
        let again = call(
            &router,
            json_request(Method::POST, "/v1/launchers/test", LOCAL, &body),
        )
        .await;
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(harness.scopes.test_runs(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_backstop_is_timeout_s_plus_40_seconds_not_the_default() {
        // The default `timeout_s` is 120, so the backstop is 160 s: a 200 s test is cut off.
        let harness = Harness::new();
        harness.scopes.set_test_delay(Duration::from_secs(200));
        let (status, error) =
            status_and_code(&harness, raw(Method::POST, "/v1/launchers/test", "{}")).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(error, "deadline_exceeded");

        // A 300 s `timeout_s` in the body moves the backstop to 340 s, so the same test is let
        // finish. The deadline layer cannot see the body; the handler has to.
        let harness = Harness::new();
        harness.scopes.set_test_delay(Duration::from_secs(200));
        let response = call(
            &harness.router(),
            raw(Method::POST, "/v1/launchers/test", r#"{"timeout_s": 300}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}

mod sessions {
    use super::*;

    #[tokio::test]
    async fn the_list_filters_by_scope_and_status() {
        let harness = Harness::new();
        let all = call(
            &harness.router(),
            request(Method::GET, "/v1/sessions", LOCAL),
        )
        .await;
        assert_eq!(all.status(), StatusCode::OK);
        assert_eq!(
            body_json(all).await["sessions"].as_array().unwrap().len(),
            2
        );

        let parked = call(
            &harness.router(),
            request(
                Method::GET,
                "/v1/sessions?scope=global&status=parked",
                LOCAL,
            ),
        )
        .await;
        let body = body_json(parked).await;
        assert_eq!(body["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(body["sessions"][0]["session_id"], UNREADABLE);
        let query = &harness.sessions.list_calls()[1];
        assert_eq!(query.scope, Some(Scope::Global));
        assert_eq!(
            query.status,
            Some(claudeclaw_api::sessions::SessionStatus::Parked)
        );
    }

    #[tokio::test]
    async fn a_filter_the_list_refuses_is_a_400() {
        let harness = Harness::new();
        for (uri, expected) in [
            ("/v1/sessions?status=gone", "bad_request"),
            ("/v1/sessions?status=unknown", "bad_request"),
            ("/v1/sessions?scope=nope", "invalid_scope"),
        ] {
            let (status, error) = status_and_code(&harness, request(Method::GET, uri, LOCAL)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(error, expected, "{uri}");
        }
        assert!(harness.sessions.list_calls().is_empty());
    }

    #[tokio::test]
    async fn one_session_carries_its_context_meter() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/sessions/s-1", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["session_id"], "s-1");
        assert_eq!(body["context"]["used_tokens"], 42_000);

        let parked = call(
            &harness.router(),
            request(Method::GET, &format!("/v1/sessions/{UNREADABLE}"), LOCAL),
        )
        .await;
        assert_eq!(body_json(parked).await["context"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn an_unknown_or_malformed_session_is_404_not_found() {
        let harness = Harness::new();
        let (status, error) =
            status_and_code(&harness, request(Method::GET, "/v1/sessions/nope", LOCAL)).await;
        assert_eq!(
            (status, error.as_str()),
            (StatusCode::NOT_FOUND, "not_found")
        );
        assert_eq!(harness.sessions.get_calls(), ["nope"]);

        // An id that is not a plain token never reaches the backend's Transcript lookup.
        for uri in [
            "/v1/sessions/..%2Fsecrets",
            "/v1/sessions/a%2Fb/messages",
            "/v1/sessions/a.jsonl",
        ] {
            let (status, error) = status_and_code(&harness, request(Method::GET, uri, LOCAL)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(error, "not_found", "{uri}");
        }
        assert_eq!(harness.sessions.get_calls().len(), 1);
        assert!(harness.sessions.message_calls().is_empty());
    }

    #[tokio::test]
    async fn messages_page_backwards_to_the_start() {
        let harness = Harness::new();
        let first = call(
            &harness.router(),
            request(Method::GET, "/v1/sessions/s-1/messages?limit=2", LOCAL),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let page = body_json(first).await;
        let uuids: Vec<&str> = page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["uuid"].as_str().unwrap())
            .collect();
        assert_eq!(uuids, ["m-4", "m-3"], "newest first");
        assert_eq!(page["next"], "c3");

        let second = call(
            &harness.router(),
            request(
                Method::GET,
                "/v1/sessions/s-1/messages?limit=10&before=c3",
                LOCAL,
            ),
        )
        .await;
        let page = body_json(second).await;
        assert_eq!(page["messages"].as_array().unwrap().len(), 3);
        assert_eq!(page["next"], serde_json::Value::Null);

        let calls = harness.sessions.message_calls();
        assert_eq!(calls[0].1.limit, 2);
        assert_eq!(calls[0].1.before, None);
        assert_eq!(calls[1].1.before.as_deref(), Some("c3"));
    }

    #[tokio::test]
    async fn an_unreadable_transcript_is_404_with_a_reason_never_an_empty_page() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(
                Method::GET,
                &format!("/v1/sessions/{UNREADABLE}/messages"),
                LOCAL,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_json(response).await;
        assert_eq!(code(&body), "transcript_not_found");
        assert_eq!(body["error"]["details"]["reason"], "not_located");
        assert!(body["error"]["hint"].is_string());
        assert!(body.get("messages").is_none());
    }

    #[tokio::test]
    async fn a_query_the_messages_route_refuses_is_a_400() {
        let harness = Harness::new();
        for (uri, expected) in [
            ("/v1/sessions/s-1/messages?limit=0", "bad_request"),
            ("/v1/sessions/s-1/messages?limit=1001", "bad_request"),
            ("/v1/sessions/s-1/messages?limit=x", "bad_request"),
            ("/v1/sessions/s-1/messages?before=zzz", "invalid_cursor"),
        ] {
            let (status, error) = status_and_code(&harness, request(Method::GET, uri, LOCAL)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(error, expected, "{uri}");
        }
    }

    #[tokio::test]
    async fn reset_clears_the_current_launcher_or_the_named_one() {
        let harness = Harness::new();
        let bare = call(
            &harness.router(),
            request(
                Method::POST,
                "/v1/scopes/discord:1234/sessions/reset",
                LOCAL,
            ),
        )
        .await;
        assert_eq!(bare.status(), StatusCode::OK);
        let body = body_json(bare).await;
        assert_eq!(body["scope"], "discord:1234");
        assert_eq!(body["launcher"], "direct");
        assert!(body["backup"].is_string());

        let named = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/scopes/global/sessions/reset",
                LOCAL,
                &SessionResetRequest {
                    launcher: Some(WORK_LAUNCHER.into()),
                },
            ),
        )
        .await;
        assert_eq!(body_json(named).await["launcher"], WORK_LAUNCHER);

        let calls = harness.sessions.reset_calls();
        assert_eq!(calls[0].1, SessionResetRequest::default());
        assert_eq!(calls[1].1.launcher.as_deref(), Some(WORK_LAUNCHER));
        let entry = &harness.audit.entries()[0];
        assert_eq!(entry.action, "sessions.reset");
        assert_eq!(entry.target.as_deref(), Some("discord:1234"));
    }

    #[tokio::test]
    async fn reset_refuses_a_bad_scope_launcher_or_body() {
        let harness = Harness::new();
        for (uri, body, expected) in [
            ("/v1/scopes/nope/sessions/reset", "{}", "invalid_scope"),
            (
                "/v1/scopes/global/sessions/reset",
                r#"{"launcher": "relative"}"#,
                "invalid_value",
            ),
            ("/v1/scopes/global/sessions/reset", "[1]", "bad_request"),
        ] {
            let (_, error) = status_and_code(&harness, raw(Method::POST, uri, body)).await;
            assert_eq!(error, expected, "{uri} {body}");
        }
        assert!(harness.sessions.reset_calls().is_empty());
    }

    #[tokio::test]
    async fn compact_is_a_turn_accepted_at_once_and_owned_by_the_caller() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/scopes/discord:1234/sessions/compact",
                LOCAL,
                &serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let accepted = body_json(response).await;
        assert_eq!(accepted["turn_id"], "turn-1");
        assert_eq!(accepted["scope"], "discord:1234");
        assert_eq!(
            harness.sessions.compact_calls(),
            [(Scope::Discord("1234".into()), "local".to_owned())]
        );

        // Progress is the existing turns.events; the record says it is a compact.
        let record = harness.turns.record("turn-1").unwrap();
        assert_eq!(record.purpose, TurnPurpose::Compact);
        assert!(record.compact.is_some());
        let read = call(
            &harness.router(),
            request(Method::GET, "/v1/turns/turn-1", LOCAL),
        )
        .await;
        assert_eq!(body_json(read).await["purpose"], "compact");
        let stream = call(
            &harness.router(),
            request(Method::GET, "/v1/turns/turn-1/events", LOCAL),
        )
        .await;
        assert_eq!(stream.status(), StatusCode::OK);

        let entry = &harness.audit.entries()[0];
        assert_eq!(entry.action, "sessions.compact");
        assert_eq!(entry.target.as_deref(), Some("discord:1234"));
    }

    #[tokio::test(start_paused = true)]
    async fn compact_wait_is_the_same_202_or_200_exchange_as_turns_create() {
        let harness = Harness::new();
        harness.turns.set_finish_after(Duration::from_secs(1));
        let waited = call(
            &harness.router(),
            request(
                Method::POST,
                "/v1/scopes/global/sessions/compact?wait=30",
                LOCAL,
            ),
        )
        .await;
        assert_eq!(waited.status(), StatusCode::OK, "a finished compact is 200");
        let record = body_json(waited).await;
        assert_eq!(record["state"], "done");
        assert_eq!(record["purpose"], "compact");
        assert_eq!(record["compact"]["scope"], "global");

        // `wait=0` is "do not wait".
        let immediate = call(
            &harness.router(),
            request(
                Method::POST,
                "/v1/scopes/global/sessions/compact?wait=0",
                LOCAL,
            ),
        )
        .await;
        assert_eq!(immediate.status(), StatusCode::ACCEPTED);

        // A compact nobody finishes is still accepted, and the wait never cancels it (§1.9).
        let stuck = Harness::new();
        let response = call(
            &stuck.router(),
            request(
                Method::POST,
                "/v1/scopes/global/sessions/compact?wait=5",
                LOCAL,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            stuck.turns.record("turn-1").unwrap().state,
            TurnState::Queued
        );
        stuck.turns.finish("turn-1", TurnOutcome::Ok, "compacted");
        assert!(stuck.turns.finished("turn-1"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_compact_survives_the_client_going_away() {
        let harness = Harness::new();
        harness.turns.set_finish_after(Duration::from_secs(2));
        let response = call(
            &harness.router(),
            request(
                Method::POST,
                "/v1/scopes/global/sessions/compact?wait=1",
                LOCAL,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(harness.turns.finished("turn-1"));
    }

    #[tokio::test]
    async fn compact_refuses_a_bad_scope_or_body_before_queuing_anything() {
        let harness = Harness::new();
        let (status, error) = status_and_code(
            &harness,
            request(Method::POST, "/v1/scopes/nope/sessions/compact", LOCAL),
        )
        .await;
        assert_eq!(
            (status, error.as_str()),
            (StatusCode::BAD_REQUEST, "invalid_scope")
        );
        let (status, error) = status_and_code(
            &harness,
            raw(Method::POST, "/v1/scopes/global/sessions/compact", "[]"),
        )
        .await;
        assert_eq!(
            (status, error.as_str()),
            (StatusCode::BAD_REQUEST, "bad_request")
        );
        assert!(harness.sessions.compact_calls().is_empty());
    }

    #[tokio::test]
    async fn a_session_in_use_is_409_and_a_parked_one_is_deleted() {
        let harness = Harness::new();
        let (status, error) = status_and_code(
            &harness,
            request(Method::DELETE, "/v1/scopes/global/sessions", LOCAL),
        )
        .await;
        assert_eq!(
            (status, error.as_str()),
            (StatusCode::CONFLICT, "session_in_use")
        );

        let uri = format!("/v1/scopes/global/sessions?launcher={WORK_LAUNCHER}");
        let response = call(&harness.router(), request(Method::DELETE, &uri, LOCAL)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["deleted"], true);
        assert_eq!(
            harness.sessions.delete_calls()[1],
            (Scope::Global, Some(WORK_LAUNCHER.to_owned()))
        );

        let entries = harness.audit.entries();
        assert_eq!(entries[0].action, "sessions.delete");
        assert_eq!(entries[0].outcome, AuditOutcome::Error);
        assert_eq!(entries[1].outcome, AuditOutcome::Ok);
        assert_eq!(entries[1].target.as_deref(), Some("global"));
    }

    #[tokio::test]
    async fn delete_refuses_a_bad_scope_or_launcher() {
        let harness = Harness::new();
        for (uri, expected) in [
            ("/v1/scopes/nope/sessions", "invalid_scope"),
            (
                "/v1/scopes/global/sessions?launcher=relative",
                "bad_request",
            ),
        ] {
            let (status, error) =
                status_and_code(&harness, request(Method::DELETE, uri, LOCAL)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(error, expected, "{uri}");
        }
        assert!(harness.sessions.delete_calls().is_empty());
    }

    #[tokio::test]
    async fn a_device_is_an_admin_for_the_writes() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/scopes/global/sessions/compact",
                device("d1"),
                &serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(harness.sessions.compact_calls()[0].1, "device:d1");
    }
}
