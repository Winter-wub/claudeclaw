//! Behavioural tests for the Agents routes (management-api §2.8), driven through the real
//! router like the rest of the server's tests.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};

use claudeclaw_api::error::ErrorCode;
use claudeclaw_api::logs::AuditOutcome;
use claudeclaw_api::permission::TokenPermission;
use claudeclaw_api::revision::Revision;

use crate::testing::*;
use crate::testing_agents::{FIRED_ENTRY, PERSONA};

/// The `error.code` of a response body.
fn code(body: &Value) -> String {
    body["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// A write with an optional `If-Match`, as the persona editor sends it (§1.5).
fn write(
    method: Method,
    uri: &str,
    revision: Option<Revision>,
    body: &impl serde::Serialize,
) -> Request<Body> {
    let mut request = json_request(method, uri, LOCAL, body);
    if let Some(revision) = revision {
        request
            .headers_mut()
            .insert(header::IF_MATCH, revision.to_string().parse().unwrap());
    }
    request
}

/// A refused name is still a failed mutation, so it is audited, but never against the raw
/// value: the entry has no `target`.
fn assert_refusals_audited_without_a_target(harness: &Harness, count: usize) {
    let entries = harness.audit.entries();
    assert_eq!(entries.len(), count);
    for entry in entries {
        assert_eq!(entry.outcome, AuditOutcome::Error, "{}", entry.action);
        assert_eq!(entry.code, Some(ErrorCode::BadRequest));
        assert_eq!(entry.target, None, "{}", entry.action);
    }
}

mod reads {
    use super::*;

    #[tokio::test]
    async fn the_list_and_the_archive_are_admin_reads_that_are_never_audited() {
        let harness = Harness::new();

        for identity in [LOCAL, device("dev-1")] {
            let list = call(
                &harness.router(),
                request(Method::GET, "/v1/agents", identity.clone()),
            )
            .await;
            assert_eq!(list.status(), StatusCode::OK);
            let body = body_json(list).await;
            assert_eq!(body["agents"][0]["name"], "researcher");
            assert_eq!(body["agents"][0]["kind"], "named");
            assert_eq!(body["agents"][0]["dir"], "agents/researcher");
            assert_eq!(body["agents"][0]["jobs"], 2);
            assert_eq!(
                body["agents"][0]["description"],
                "Finds and summarises papers."
            );

            let archived = call(
                &harness.router(),
                request(Method::GET, "/v1/archived-agents", identity),
            )
            .await;
            assert_eq!(archived.status(), StatusCode::OK);
            let body = body_json(archived).await;
            assert_eq!(body["archived"][0]["entry"], FIRED_ENTRY);
            assert_eq!(body["archived"][0]["name"], "paper-digest-5678");
            assert_eq!(body["archived"][0]["reason"], "fired");
        }

        let persona = call(
            &harness.router(),
            request(Method::GET, "/v1/agents/researcher/persona", LOCAL),
        )
        .await;
        assert_eq!(persona.status(), StatusCode::OK);
        let body = body_json(persona).await;
        assert_eq!(body["text"], PERSONA);
        assert_eq!(
            body["revision"],
            harness.agents.persona_revision("researcher").to_string()
        );

        assert!(harness.audit.is_empty(), "reads leave no audit entry");
    }

    #[tokio::test]
    async fn a_missing_agent_has_no_persona() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/agents/nobody/persona", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(code(&body_json(response).await), "not_found");
    }

    #[tokio::test]
    async fn every_route_is_admin_only() {
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
        let calls: [(Method, &str); 8] = [
            (Method::GET, "/v1/agents"),
            (Method::GET, "/v1/archived-agents"),
            (Method::POST, "/v1/agents"),
            (Method::GET, "/v1/agents/researcher/persona"),
            (Method::PUT, "/v1/agents/researcher/persona"),
            (Method::POST, "/v1/agents/researcher/archive"),
            (Method::POST, "/v1/archived-agents/x/restore"),
            (Method::DELETE, "/v1/archived-agents/x"),
        ];
        for (method, uri) in calls {
            let response = call(&harness.router(), bearer_request(method, uri, "secret")).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
            assert_eq!(code(&body_json(response).await), "missing_permission");
        }
        assert!(harness.agents.calls().is_empty());
    }
}

mod create {
    use super::*;

    #[tokio::test]
    async fn a_new_agent_is_201_with_its_persona_and_audited_by_name() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/agents",
                LOCAL,
                &json!({"name": "scribe", "persona": "# Scribe\n\nKeeps notes.\n"}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        assert_eq!(body["name"], "scribe");
        assert_eq!(body["dir"], "agents/scribe");
        assert_eq!(body["description"], "Keeps notes.");
        assert_eq!(
            harness.agents.persona("scribe").as_deref(),
            Some("# Scribe\n\nKeeps notes.\n")
        );

        let entries = harness.audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "agents.create");
        assert_eq!(entries[0].target.as_deref(), Some("scribe"));
        assert_eq!(entries[0].outcome, AuditOutcome::Ok);
    }

    #[tokio::test]
    async fn the_persona_is_optional() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/agents",
                LOCAL,
                &json!({"name": "scribe"}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(harness.agents.persona("scribe"), None);
        assert!(harness.agents.has_agent("scribe"));
    }

    #[tokio::test]
    async fn a_taken_name_is_409_agent_exists_and_the_attempt_is_audited() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/agents",
                LOCAL,
                &json!({"name": "researcher", "persona": "overwrite"}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(code(&body_json(response).await), "agent_exists");
        assert_eq!(
            harness.agents.persona("researcher").as_deref(),
            Some(PERSONA)
        );

        let entries = harness.audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].outcome, AuditOutcome::Error);
        assert_eq!(entries[0].target.as_deref(), Some("researcher"));
    }

    #[tokio::test]
    async fn a_name_that_could_leave_agents_is_refused_before_the_backend() {
        let harness = Harness::new();
        let long = "a".repeat(201);
        let names = [
            "",
            ".",
            "..",
            "../escape",
            "a/b",
            "a\\b",
            ".hidden",
            ".archive",
            "a..b",
            "trailing.",
            " padded",
            "padded ",
            "ctrl\u{7}char",
            "nul\0byte",
            "new\nline",
            "c:evil",
            "wild*card",
            "pipe|d",
            long.as_str(),
        ];
        for name in names {
            let response = call(
                &harness.router(),
                json_request(Method::POST, "/v1/agents", LOCAL, &json!({"name": name})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name:?}");
            let body = body_json(response).await;
            assert_eq!(code(&body), "bad_request", "{name:?}");
            assert!(
                !body.to_string().contains("escape"),
                "the refusal never echoes the name"
            );
        }
        assert!(
            harness.agents.calls().is_empty(),
            "nothing reached the backend"
        );
        assert_refusals_audited_without_a_target(&harness, names.len());
    }

    #[tokio::test]
    async fn names_with_dots_spaces_and_unicode_inside_are_fine() {
        let harness = Harness::new();
        for name in [
            "paper-digest-5678",
            "v1.2",
            "my agent",
            "研究者",
            "under_score",
        ] {
            let response = call(
                &harness.router(),
                json_request(Method::POST, "/v1/agents", LOCAL, &json!({"name": name})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::CREATED, "{name:?}");
        }
    }

    #[tokio::test]
    async fn a_body_that_is_not_an_agent_create_never_reaches_the_backend() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(Method::POST, "/v1/agents", LOCAL, &json!({"persona": "x"})),
        )
        .await;
        assert!(response.status().is_client_error());
        assert!(harness.agents.calls().is_empty());
    }
}

mod persona {
    use super::*;

    #[tokio::test]
    async fn a_whole_file_write_needs_the_matching_revision() {
        let harness = Harness::new();
        let put = json!({"text": "# Researcher\n\nNow with citations.\n"});
        let uri = "/v1/agents/researcher/persona";

        let unguarded = call(&harness.router(), write(Method::PUT, uri, None, &put)).await;
        assert_eq!(unguarded.status(), StatusCode::PRECONDITION_REQUIRED);
        assert_eq!(code(&body_json(unguarded).await), "revision_required");
        assert_eq!(
            harness.agents.persona("researcher").as_deref(),
            Some(PERSONA),
            "refused in the route, before the backend"
        );
        assert!(harness.agents.calls().is_empty());

        let current = harness.agents.persona_revision("researcher");
        let replaced = call(
            &harness.router(),
            write(Method::PUT, uri, Some(current), &put),
        )
        .await;
        assert_eq!(replaced.status(), StatusCode::OK);
        let body = body_json(replaced).await;
        assert_eq!(body["text"], put["text"]);
        assert_eq!(
            body["revision"],
            Revision::of(put["text"].as_str().unwrap().as_bytes()).to_string()
        );
        assert_eq!(
            harness.agents.persona("researcher").as_deref(),
            put["text"].as_str()
        );
        assert_eq!(
            harness.agents.calls(),
            [format!("persona.put researcher {current}")]
        );
    }

    #[tokio::test]
    async fn a_stale_revision_is_409_with_the_current_one_and_nothing_is_written() {
        let harness = Harness::new();
        let uri = "/v1/agents/researcher/persona";
        let opened = harness.agents.persona_revision("researcher");
        harness.agents.change_persona_on_disk("researcher");

        let stale = call(
            &harness.router(),
            write(Method::PUT, uri, Some(opened), &json!({"text": "mine"})),
        )
        .await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        let body = body_json(stale).await;
        assert_eq!(code(&body), "revision_mismatch");
        assert_eq!(
            body["error"]["details"]["current"],
            harness.agents.persona_revision("researcher").to_string()
        );
        assert_eq!(
            harness.agents.persona("researcher").as_deref(),
            Some("# Changed elsewhere\n")
        );

        let entries = harness.audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "agents.persona.put");
        assert_eq!(entries[0].target.as_deref(), Some("researcher"));
        assert_eq!(entries[0].outcome, AuditOutcome::Error);
    }

    #[tokio::test]
    async fn a_malformed_if_match_is_400_not_ignored() {
        let harness = Harness::new();
        let mut request = json_request(
            Method::PUT,
            "/v1/agents/researcher/persona",
            LOCAL,
            &json!({"text": "x"}),
        );
        request
            .headers_mut()
            .insert(header::IF_MATCH, "not-a-revision".parse().unwrap());
        let response = call(&harness.router(), request).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(code(&body_json(response).await), "bad_request");
        assert!(harness.agents.calls().is_empty());
    }

    #[tokio::test]
    async fn a_missing_agent_is_404_even_with_a_revision() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            write(
                Method::PUT,
                "/v1/agents/nobody/persona",
                Some(Revision::of(b"")),
                &json!({"text": "x"}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!harness.agents.has_agent("nobody"));
    }

    #[tokio::test]
    async fn a_name_in_the_path_cannot_leave_agents() {
        let harness = Harness::new();
        for name in [
            "..",
            ".archive",
            ".hidden",
            "%2E%2E",
            "..%2Fescape",
            "a%2Fb",
            "a%5Cb",
            "ctrl%07char",
            "nul%00byte",
        ] {
            let uri = format!("/v1/agents/{name}/persona");
            let read = call(&harness.router(), request(Method::GET, &uri, LOCAL)).await;
            assert_eq!(read.status(), StatusCode::BAD_REQUEST, "GET {name}");

            let put = call(
                &harness.router(),
                write(
                    Method::PUT,
                    &uri,
                    Some(Revision::of(b"")),
                    &json!({"text": "x"}),
                ),
            )
            .await;
            assert_eq!(put.status(), StatusCode::BAD_REQUEST, "PUT {name}");

            let archive = call(
                &harness.router(),
                request(Method::POST, &format!("/v1/agents/{name}/archive"), LOCAL),
            )
            .await;
            assert_eq!(archive.status(), StatusCode::BAD_REQUEST, "archive {name}");
        }
        assert!(harness.agents.calls().is_empty());
        // 9 names, each tried as a PUT and an archive (the GETs are not audited).
        assert_refusals_audited_without_a_target(&harness, 18);
    }
}

mod archive {
    use super::*;

    #[tokio::test]
    async fn archiving_moves_the_agent_and_reports_its_unscheduled_jobs() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::POST, "/v1/agents/researcher/archive", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["entry"], "researcher-20261014T090000Z");
        assert_eq!(
            body["archived_to"],
            "agents/.archive/researcher-20261014T090000Z"
        );
        assert_eq!(body["jobs_unscheduled"], 2);
        assert!(!harness.agents.has_agent("researcher"));
        assert!(harness.agents.has_entry("researcher-20261014T090000Z"));

        let entries = harness.audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "agents.archive");
        assert_eq!(entries[0].target.as_deref(), Some("researcher"));

        let again = call(
            &harness.router(),
            request(Method::POST, "/v1/agents/researcher/archive", LOCAL),
        )
        .await;
        assert_eq!(again.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn restoring_brings_it_back_and_a_taken_name_is_409() {
        let harness = Harness::new();
        let archived = call(
            &harness.router(),
            request(Method::POST, "/v1/agents/researcher/archive", LOCAL),
        )
        .await;
        let entry = body_json(archived).await["entry"]
            .as_str()
            .unwrap()
            .to_owned();
        let uri = format!("/v1/archived-agents/{entry}/restore");

        // The name is taken again before the restore.
        let created = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/agents",
                LOCAL,
                &json!({"name": "researcher"}),
            ),
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
        let conflict = call(&harness.router(), request(Method::POST, &uri, LOCAL)).await;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_eq!(code(&body_json(conflict).await), "agent_exists");
        assert!(harness.agents.has_entry(&entry), "the entry is untouched");

        // A fired thread restores under its own name.
        let restored = call(
            &harness.router(),
            request(
                Method::POST,
                &format!("/v1/archived-agents/{FIRED_ENTRY}/restore"),
                LOCAL,
            ),
        )
        .await;
        assert_eq!(restored.status(), StatusCode::OK);
        let body = body_json(restored).await;
        assert_eq!(body["name"], "paper-digest-5678");
        assert_eq!(body["dir"], "agents/paper-digest-5678");
        assert!(harness.agents.has_agent("paper-digest-5678"));
        assert!(!harness.agents.has_entry(FIRED_ENTRY));

        let entries = harness.audit.entries();
        let restores: Vec<_> = entries
            .iter()
            .filter(|entry| entry.action == "agents.restore")
            .collect();
        assert_eq!(restores.len(), 2);
        assert_eq!(restores[0].target.as_deref(), Some(entry.as_str()));
        assert_eq!(restores[0].outcome, AuditOutcome::Error);
        assert_eq!(restores[1].target.as_deref(), Some(FIRED_ENTRY));
        assert_eq!(restores[1].outcome, AuditOutcome::Ok);
    }

    #[tokio::test]
    async fn purge_deletes_an_archived_entry_and_only_that() {
        let harness = Harness::new();
        let uri = format!("/v1/archived-agents/{FIRED_ENTRY}");
        let purged = call(&harness.router(), request(Method::DELETE, &uri, LOCAL)).await;
        assert_eq!(purged.status(), StatusCode::OK);
        assert_eq!(body_json(purged).await, json!({"deleted": true}));
        assert!(!harness.agents.has_entry(FIRED_ENTRY));

        let entries = harness.audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "agents.purge");
        assert_eq!(entries[0].target.as_deref(), Some(FIRED_ENTRY));

        // The same entry is gone, and a live agent is not an archived entry.
        let again = call(&harness.router(), request(Method::DELETE, &uri, LOCAL)).await;
        assert_eq!(again.status(), StatusCode::NOT_FOUND);
        let live = call(
            &harness.router(),
            request(Method::DELETE, "/v1/archived-agents/researcher", LOCAL),
        )
        .await;
        assert_eq!(live.status(), StatusCode::NOT_FOUND);
        assert!(harness.agents.has_agent("researcher"));
    }

    #[tokio::test]
    async fn an_entry_that_could_leave_the_archive_is_refused_before_the_backend() {
        let harness = Harness::new();
        for entry in [
            "..",
            ".",
            "%2E%2E",
            "..%2Fagents",
            "..%2F..%2Fetc",
            "a%2Fb",
            "a%5Cb",
            ".hidden",
            "ctrl%07char",
            "nul%00byte",
        ] {
            let purge = call(
                &harness.router(),
                request(
                    Method::DELETE,
                    &format!("/v1/archived-agents/{entry}"),
                    LOCAL,
                ),
            )
            .await;
            assert_eq!(purge.status(), StatusCode::BAD_REQUEST, "purge {entry}");
            assert_eq!(code(&body_json(purge).await), "bad_request");

            let restore = call(
                &harness.router(),
                request(
                    Method::POST,
                    &format!("/v1/archived-agents/{entry}/restore"),
                    LOCAL,
                ),
            )
            .await;
            assert_eq!(restore.status(), StatusCode::BAD_REQUEST, "restore {entry}");
        }
        assert!(harness.agents.calls().is_empty());
        // 10 entries, each tried as a purge and a restore.
        assert_refusals_audited_without_a_target(&harness, 20);
        assert!(harness.agents.has_entry(FIRED_ENTRY));
    }

    #[tokio::test]
    async fn an_agent_named_archive_does_not_shadow_a_route() {
        // §1.7: archived agents live under `/v1/archived-agents/…`, so these names are plain
        // agents.
        let harness = Harness::new();
        for name in ["archive", "persona", "archived-agents"] {
            let response = call(
                &harness.router(),
                json_request(Method::POST, "/v1/agents", LOCAL, &json!({"name": name})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::CREATED, "{name}");
            let persona = call(
                &harness.router(),
                request(Method::GET, &format!("/v1/agents/{name}/persona"), LOCAL),
            )
            .await;
            assert_eq!(persona.status(), StatusCode::OK, "{name}");
        }
    }
}

mod wiring {
    use super::*;
    use crate::state::AppState;

    #[tokio::test]
    async fn an_unwired_server_fails_loudly_with_500_internal() {
        let router = crate::router::router(AppState::builder().build());
        for (method, uri) in [
            (Method::GET, "/v1/agents"),
            (Method::GET, "/v1/archived-agents"),
            (Method::GET, "/v1/agents/researcher/persona"),
            (Method::POST, "/v1/agents/researcher/archive"),
            (Method::DELETE, "/v1/archived-agents/x"),
        ] {
            let response = call(&router, request(method, uri, LOCAL)).await;
            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{uri}"
            );
            assert_eq!(code(&body_json(response).await), "internal");
        }
    }
}
