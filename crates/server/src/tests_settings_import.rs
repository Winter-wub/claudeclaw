//! Behavioural tests for the Settings and First-start import routes (management-api §2.6,
//! §2.11), driven through the real router like the rest of the server's tests.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};

use claudeclaw_api::import::{ImportApplyRequest, ImportOnly};
use claudeclaw_api::logs::AuditOutcome;
use claudeclaw_api::permission::TokenPermission;
use claudeclaw_api::revision::Revision;
use claudeclaw_api::status::HealthState;

use crate::testing::*;
use crate::testing_settings_import::{SAMPLE_PLAN_HASH, SEEDED_TOKEN};

/// The `error.code` of a response body.
fn code(body: &Value) -> String {
    body["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// A settings patch from `identity`, with an optional `If-Match`.
fn patch_as(
    identity: crate::auth::PeerIdentity,
    revision: Option<Revision>,
    body: &Value,
) -> Request<Body> {
    let mut request = json_request(Method::PATCH, "/v1/settings", identity, body);
    if let Some(revision) = revision {
        request
            .headers_mut()
            .insert(header::IF_MATCH, revision.to_string().parse().unwrap());
    }
    request
}

mod settings {
    use super::*;

    #[tokio::test]
    async fn the_view_is_admin_only_and_never_audited() {
        let harness = Harness::new();
        harness
            .tokens
            .add("secret", "tok-1", vec![TokenPermission::StatusRead], false);

        for identity in [LOCAL, device("dev-1")] {
            let response = call(
                &harness.router(),
                request(Method::GET, "/v1/settings", identity),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            assert_eq!(body["revision"], harness.settings.revision().to_string());
            assert_eq!(body["settings"]["model"], "opus");
            assert_eq!(body["ts_only_keys"], json!(["web.port"]));
            assert_eq!(body["env_overrides"], json!(["discord.token"]));
        }
        assert_eq!(harness.settings.get_calls(), 2);

        // §1.3: settings are not in any token permission.
        let denied = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/settings", "secret"),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(code(&body_json(denied).await), "missing_permission");
        assert_eq!(harness.settings.get_calls(), 2);
        assert!(harness.audit.is_empty());
    }

    #[tokio::test]
    async fn a_set_secret_reads_as_set_and_its_value_never_appears() {
        let harness = Harness::new();
        assert!(
            harness.settings.file().to_string().contains(SEEDED_TOKEN),
            "the fixture's file holds the secret"
        );

        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/settings", LOCAL),
        )
        .await;
        let text = body_json(response).await.to_string();
        assert!(!text.contains(SEEDED_TOKEN), "{text}");
        let view: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(view["settings"]["discord"]["token"], json!({"set": true}));
        assert_eq!(view["settings"]["api"], json!({"set": false}));
    }

    #[tokio::test]
    async fn a_patch_sets_and_clears_a_secret_without_echoing_it() {
        let harness = Harness::new();
        let new_token = "replacement-discord-token-do-not-echo";

        let set = call(
            &harness.router(),
            patch_as(LOCAL, None, &json!({"discord": {"token": new_token}})),
        )
        .await;
        assert_eq!(set.status(), StatusCode::OK);
        let text = body_json(set).await.to_string();
        assert!(
            !text.contains(new_token) && !text.contains(SEEDED_TOKEN),
            "{text}"
        );
        let applied: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(applied["changed_keys"], json!(["discord.token"]));
        assert_eq!(applied["restarted"], json!(["discord"]));
        assert_eq!(
            applied["view"]["settings"]["discord"]["token"],
            json!({"set": true})
        );
        assert_eq!(harness.settings.file()["discord"]["token"], new_token);

        // Neither the audit entry nor a later read carries it.
        let audit = serde_json::to_string(&harness.audit.entries()).unwrap();
        assert!(!audit.contains(new_token), "{audit}");
        let read = call(
            &harness.router(),
            request(Method::GET, "/v1/settings", LOCAL),
        )
        .await;
        assert!(!body_json(read).await.to_string().contains(new_token));

        let cleared = call(
            &harness.router(),
            patch_as(LOCAL, None, &json!({"discord": {"token": null}})),
        )
        .await;
        let applied = ok_json(cleared).await;
        assert_eq!(
            applied["view"]["settings"]["discord"]["token"],
            json!({"set": false})
        );
        assert_eq!(applied["changed_keys"], json!(["discord.token"]));
    }

    #[tokio::test]
    async fn a_refusal_never_quotes_the_secret_it_was_sent() {
        let harness = Harness::new();
        let sent = "never-quote-this-secret";
        for body in [
            json!({"apiToken": sent}),
            json!({"launcher": sent}),
            json!({"nope": sent}),
        ] {
            let response = call(&harness.router(), patch_as(LOCAL, None, &body)).await;
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert!(!body_json(response).await.to_string().contains(sent));
        }
        let malformed = call(
            &harness.router(),
            Request::builder()
                .method(Method::PATCH)
                .uri("/v1/settings")
                .extension(LOCAL)
                .body(Body::from(format!("[\"{sent}\""))) // truncated JSON
                .unwrap(),
        )
        .await;
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        assert!(!body_json(malformed).await.to_string().contains(sent));
    }

    #[tokio::test]
    async fn an_update_is_audited_under_its_op() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            patch_as(device("dev-1"), None, &json!({"model": "sonnet"})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let applied = body_json(response).await;
        assert_eq!(applied["changed_keys"], json!(["model"]));
        assert_eq!(applied["restarted"], json!([]));
        assert_eq!(applied["view"]["settings"]["model"], "sonnet");

        let entries = harness.audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "settings.update");
        assert_eq!(entries[0].principal, "device:dev-1");
        assert_eq!(entries[0].outcome, AuditOutcome::Ok);
    }

    #[tokio::test]
    async fn an_if_match_is_optional_and_a_stale_one_is_a_409_with_the_current() {
        let harness = Harness::new();
        let current = harness.settings.revision();

        // No If-Match: a patch over a fresh read.
        let bare = call(
            &harness.router(),
            patch_as(LOCAL, None, &json!({"timezone": "Asia/Bangkok"})),
        )
        .await;
        assert_eq!(bare.status(), StatusCode::OK);
        assert_eq!(harness.settings.update_calls()[0].1, None);

        // The revision the first write left is accepted.
        let fresh = harness.settings.revision();
        assert_ne!(fresh, current);
        let matched = call(
            &harness.router(),
            patch_as(LOCAL, Some(fresh), &json!({"model": "haiku"})),
        )
        .await;
        assert_eq!(matched.status(), StatusCode::OK);
        assert_eq!(harness.settings.update_calls()[1].1, Some(fresh));

        // The old one is stale now.
        let stale = call(
            &harness.router(),
            patch_as(LOCAL, Some(current), &json!({"model": "opus"})),
        )
        .await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        let body = body_json(stale).await;
        assert_eq!(code(&body), "revision_mismatch");
        assert_eq!(
            body["error"]["details"]["current"],
            harness.settings.revision().to_string()
        );
        assert_eq!(harness.settings.file()["model"], "haiku", "nothing written");

        let last = harness.audit.entries().pop().unwrap();
        assert_eq!(last.outcome, AuditOutcome::Error);
        assert_eq!(
            last.code,
            Some(claudeclaw_api::error::ErrorCode::RevisionMismatch)
        );
    }

    #[tokio::test]
    async fn a_malformed_if_match_or_body_is_a_400_and_reaches_nothing() {
        let harness = Harness::new();
        let mut bad_header = patch_as(LOCAL, None, &json!({"model": "x"}));
        bad_header
            .headers_mut()
            .insert(header::IF_MATCH, "not-a-revision".parse().unwrap());
        let response = call(&harness.router(), bad_header).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        for body in [json!([1]), json!("model"), json!(7), Value::Null] {
            let response = call(&harness.router(), patch_as(LOCAL, None, &body)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(code(&body_json(response).await), "bad_request");
        }
        assert!(harness.settings.update_calls().is_empty());
    }

    #[tokio::test]
    async fn ts_only_launcher_and_unknown_keys_are_422_and_write_nothing() {
        let harness = Harness::new();
        let before = harness.settings.file();
        for (body, expected) in [
            (json!({"web": {"port": 1}}), "ts_only_key"),
            (json!({"apiToken": "x"}), "ts_only_key"),
            (json!({"watchdog": {"enabled": true}}), "ts_only_key"),
            (json!({"runtime": {"launcher": "ccs"}}), "ts_only_key"),
            (json!({"runtime": {"launchers": {}}}), "ts_only_key"),
            (json!({"launcher": "direct"}), "invalid_value"),
            (json!({"nope": 1}), "unknown_key"),
            // One refused key refuses the whole patch.
            (json!({"model": "sonnet", "nope": 1}), "unknown_key"),
        ] {
            let response = call(&harness.router(), patch_as(LOCAL, None, &body)).await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{body}"
            );
            let error = body_json(response).await;
            assert_eq!(code(&error), expected, "{body}");
            if expected == "invalid_value" {
                assert!(
                    error["error"]["hint"].as_str().unwrap().contains("§2.5"),
                    "the hint points at the Launcher routes"
                );
            }
        }
        assert_eq!(harness.settings.file(), before);
    }

    #[tokio::test]
    async fn remote_keys_are_local_only_and_the_rest_of_a_devices_patch_is_allowed() {
        let harness = Harness::new();
        let before = harness.settings.file();

        for body in [
            json!({"remote": {"port": 4700}}),
            json!({"remote": {"enabled": true}}),
            json!({"remote": null}),
            // A mixed patch is refused whole: nothing of it is written.
            json!({"model": "sonnet", "remote": {"listen": "127.0.0.1"}}),
        ] {
            let response = call(&harness.router(), patch_as(device("dev-1"), None, &body)).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{body}");
            assert_eq!(code(&body_json(response).await), "local_only", "{body}");
        }
        assert!(harness.settings.update_calls().is_empty());
        assert_eq!(harness.settings.file(), before);

        // The same Device may change anything else...
        let other = call(
            &harness.router(),
            patch_as(device("dev-1"), None, &json!({"model": "sonnet"})),
        )
        .await;
        assert_eq!(other.status(), StatusCode::OK);
        // ...and an empty `remote` object changes no remote key.
        let empty = call(
            &harness.router(),
            patch_as(device("dev-1"), None, &json!({"remote": {}})),
        )
        .await;
        assert_eq!(empty.status(), StatusCode::OK);

        // Local may move the listener, and the rebind is reported.
        let local = call(
            &harness.router(),
            patch_as(LOCAL, None, &json!({"remote": {"port": 4700}})),
        )
        .await;
        let applied = ok_json(local).await;
        assert_eq!(applied["changed_keys"], json!(["remote.port"]));
        assert_eq!(applied["restarted"], json!(["remote_listener"]));
        assert_eq!(applied["view"]["settings"]["remote"]["port"], 4700);

        // The refusals were audited as errors, in order, with the code.
        let refused: Vec<_> = harness
            .audit
            .entries()
            .into_iter()
            .filter(|entry| entry.outcome == AuditOutcome::Error)
            .collect();
        assert_eq!(refused.len(), 4);
        assert!(
            refused
                .iter()
                .all(|entry| entry.code == Some(claudeclaw_api::error::ErrorCode::LocalOnly))
        );
    }

    #[tokio::test]
    async fn a_token_cannot_patch_settings_at_all() {
        let harness = Harness::new();
        harness.tokens.add(
            "secret",
            "tok-1",
            vec![TokenPermission::Send, TokenPermission::JobsRun],
            false,
        );
        for body in [json!({"model": "x"}), json!({"remote": {"port": 1}})] {
            let response = call(
                &harness.router(),
                bearer_json_request(Method::PATCH, "/v1/settings", "secret", &body),
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(code(&body_json(response).await), "missing_permission");
        }
        assert!(harness.settings.update_calls().is_empty());
    }

    #[tokio::test]
    async fn a_patch_while_starting_is_503_and_a_read_is_served() {
        let harness = Harness::new();
        harness.status.set_state(HealthState::Starting);
        let response = call(
            &harness.router(),
            patch_as(LOCAL, None, &json!({"model": "x"})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code(&body_json(response).await), "daemon_starting");
        assert!(harness.settings.update_calls().is_empty());

        let read = call(
            &harness.router(),
            request(Method::GET, "/v1/settings", LOCAL),
        )
        .await;
        assert_eq!(read.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_file_changed_on_disk_shows_in_the_next_revision() {
        let harness = Harness::new();
        let read = |router| async move {
            body_json(call(&router, request(Method::GET, "/v1/settings", LOCAL)).await).await
                ["revision"]
                .clone()
        };
        let first = read(harness.router()).await;
        harness.settings.change_on_disk();
        assert_ne!(read(harness.router()).await, first);
    }
}

mod import {
    use super::*;

    #[tokio::test]
    async fn the_record_is_admin_only_and_never_audited() {
        let harness = Harness::new();
        harness
            .tokens
            .add("secret", "tok-1", vec![TokenPermission::StatusRead], false);

        let response = call(&harness.router(), request(Method::GET, "/v1/import", LOCAL)).await;
        let body = ok_json(response).await;
        assert_eq!(body["kind"], "imported");
        assert_eq!(body["steps"][0]["step"], "backup");
        assert_eq!(body["plan"], Value::Null);

        let device = call(
            &harness.router(),
            request(Method::GET, "/v1/import", device("dev-1")),
        )
        .await;
        assert_eq!(device.status(), StatusCode::OK);

        let denied = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/import", "secret"),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(code(&body_json(denied).await), "missing_permission");
        assert_eq!(harness.import.get_calls(), 2);
        assert!(harness.audit.is_empty());
    }

    #[tokio::test]
    async fn a_plan_writes_nothing_so_it_is_not_audited_and_reads_its_filters() {
        let harness = Harness::new();

        let bare = call(
            &harness.router(),
            request(Method::GET, "/v1/import/plan", LOCAL),
        )
        .await;
        let plan = ok_json(bare).await;
        assert_eq!(plan["plan_hash"], SAMPLE_PLAN_HASH);

        let filtered = call(
            &harness.router(),
            request(
                Method::GET,
                "/v1/import/plan?only=jobs&only=state&only=jobs&job=daily-report&job=researcher%2Fpaper-digest",
                device("dev-1"),
            ),
        )
        .await;
        assert_eq!(filtered.status(), StatusCode::OK);

        let calls = harness.import.plan_calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].only.is_empty() && calls[0].jobs.is_empty());
        assert_eq!(calls[1].only, [ImportOnly::Jobs, ImportOnly::State]);
        assert_eq!(calls[1].jobs, ["daily-report", "researcher/paper-digest"]);
        assert!(
            harness.audit.is_empty(),
            "import.plan writes nothing, so it writes no audit line (§2.11)"
        );
    }

    #[tokio::test]
    async fn a_step_the_plan_does_not_list_is_a_400() {
        let harness = Harness::new();
        for uri in [
            "/v1/import/plan?only=inbox",
            "/v1/import/plan?only=jobs&only=unknown",
            "/v1/import/plan?only=JOBS",
        ] {
            let response = call(&harness.router(), request(Method::GET, uri, LOCAL)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(code(&body_json(response).await), "bad_request");
        }
        // An empty `only=` is "not sent", like every other filter.
        let empty = call(
            &harness.router(),
            request(Method::GET, "/v1/import/plan?only=", LOCAL),
        )
        .await;
        assert_eq!(empty.status(), StatusCode::OK);
        assert_eq!(harness.import.plan_calls().len(), 1);
    }

    #[tokio::test]
    async fn every_step_name_is_accepted_by_only() {
        let harness = Harness::new();
        for step in ["launchers", "token", "state", "settings", "jobs"] {
            let response = call(
                &harness.router(),
                request(Method::GET, &format!("/v1/import/plan?only={step}"), LOCAL),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "{step}");
        }
        assert_eq!(harness.import.plan_calls().len(), 5);
    }

    #[tokio::test]
    async fn an_apply_is_audited_and_passes_its_selection_through() {
        let harness = Harness::new();
        let body = ImportApplyRequest {
            only: Some(vec![ImportOnly::Jobs]),
            jobs: Some(vec!["researcher/paper-digest".into()]),
            plan_hash: Some(SAMPLE_PLAN_HASH.into()),
        };
        let response = call(
            &harness.router(),
            json_request(Method::POST, "/v1/import/apply", device("dev-1"), &body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let result = body_json(response).await;
        assert_eq!(result["report"], "import-report-20261014T090000Z.md");
        assert_eq!(result["backup_dir"], Value::Null);
        assert_eq!(result["record"]["kind"], "imported");
        assert_eq!(harness.import.apply_calls(), [body]);

        let entries = harness.audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "import.apply");
        assert_eq!(entries[0].principal, "device:dev-1");
        assert_eq!(entries[0].outcome, AuditOutcome::Ok);
    }

    #[tokio::test]
    async fn an_empty_body_applies_everything() {
        let harness = Harness::new();
        let bare = call(
            &harness.router(),
            request(Method::POST, "/v1/import/apply", LOCAL),
        )
        .await;
        assert_eq!(bare.status(), StatusCode::OK);
        let empty_object = call(
            &harness.router(),
            json_request(Method::POST, "/v1/import/apply", LOCAL, &json!({})),
        )
        .await;
        assert_eq!(empty_object.status(), StatusCode::OK);
        assert_eq!(
            harness.import.apply_calls(),
            [ImportApplyRequest::default(), ImportApplyRequest::default()]
        );
    }

    #[tokio::test]
    async fn a_stale_plan_hash_is_409_plan_changed_and_audited_as_an_error() {
        let harness = Harness::new();
        harness.import.change_data();
        let body = ImportApplyRequest {
            plan_hash: Some(SAMPLE_PLAN_HASH.into()),
            ..ImportApplyRequest::default()
        };
        let response = call(
            &harness.router(),
            json_request(Method::POST, "/v1/import/apply", LOCAL, &body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(code(&body_json(response).await), "plan_changed");

        let entry = &harness.audit.entries()[0];
        assert_eq!(entry.action, "import.apply");
        assert_eq!(entry.outcome, AuditOutcome::Error);
        assert_eq!(
            entry.code,
            Some(claudeclaw_api::error::ErrorCode::PlanChanged)
        );

        // Re-previewing gives the hash that applies.
        let plan = ok_json(
            call(
                &harness.router(),
                request(Method::GET, "/v1/import/plan", LOCAL),
            )
            .await,
        )
        .await;
        let again = ImportApplyRequest {
            plan_hash: Some(plan["plan_hash"].as_str().unwrap().into()),
            ..ImportApplyRequest::default()
        };
        let response = call(
            &harness.router(),
            json_request(Method::POST, "/v1/import/apply", LOCAL, &again),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_invalid_apply_body_is_a_400_that_reaches_nothing() {
        let harness = Harness::new();
        for body in [
            json!({"only": ["inbox"]}),
            json!({"only": []}),
            json!({"jobs": []}),
            json!({"jobs": [""]}),
            json!({"only": "jobs"}),
            json!([1]),
        ] {
            let response = call(
                &harness.router(),
                json_request(Method::POST, "/v1/import/apply", LOCAL, &body),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(code(&body_json(response).await), "bad_request", "{body}");
        }
        assert!(harness.import.apply_calls().is_empty());
    }

    #[tokio::test]
    async fn an_apply_is_admin_only_and_refused_while_starting() {
        let harness = Harness::new();
        harness.tokens.add(
            "secret",
            "tok-1",
            vec![TokenPermission::Send, TokenPermission::JobsRun],
            false,
        );
        let denied = call(
            &harness.router(),
            bearer_json_request(Method::POST, "/v1/import/apply", "secret", &json!({})),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        harness.status.set_state(HealthState::Starting);
        let starting = call(
            &harness.router(),
            request(Method::POST, "/v1/import/apply", LOCAL),
        )
        .await;
        assert_eq!(starting.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code(&body_json(starting).await), "daemon_starting");
        assert!(harness.import.apply_calls().is_empty());

        // The preview is a read: it is served while the Worker finishes its import (§1.8).
        let plan = call(
            &harness.router(),
            request(Method::GET, "/v1/import/plan", LOCAL),
        )
        .await;
        assert_eq!(plan.status(), StatusCode::OK);
    }
}
