//! Behavioural tests for the management API server (management-api §1).
//!
//! These drive the real router through `tower`'s `oneshot`, plus the real listener over a
//! temporary Unix socket, so the layering — auth, audit, deadline — is under test, not the fakes.

use axum::body::BodyDataStream;
use axum::http::{Method, StatusCode, header};
use futures_util::StreamExt;

use claudeclaw_api::events::DaemonEvent;
use claudeclaw_api::permission::TokenPermission;
use claudeclaw_api::status::{HealthState, HousekeepingRequest, ReexecRequest};

use crate::testing::*;

/// The `error.code` of a response body, e.g. `local_only`.
fn code(body: &serde_json::Value) -> String {
    body["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The `error.details.required` of a response body.
fn required(body: &serde_json::Value) -> String {
    body["error"]["details"]["required"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// One SSE chunk as text.
async fn next_frame(body: &mut BodyDataStream) -> String {
    let chunk = body.next().await.expect("a frame").expect("no error");
    String::from_utf8_lossy(&chunk).into_owned()
}

mod access {
    use super::*;

    #[tokio::test]
    async fn a_local_identity_reaches_a_local_route() {
        let harness = Harness::new();
        let body = ReexecRequest {
            exe: "/usr/bin/claudeclaw".into(),
        };
        let response = call(
            &harness.router(),
            json_request(Method::POST, "/v1/daemon/reexec", LOCAL, &body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(harness.daemon.calls(), ["daemon.reexec"]);
    }

    #[tokio::test]
    async fn a_token_cannot_reach_a_local_route() {
        let harness = Harness::new();
        harness
            .tokens
            .add("secret", "tok-1", vec![TokenPermission::StatusRead], false);
        let response = call(
            &harness.router(),
            bearer_request(Method::POST, "/v1/daemon/reexec", "secret"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(code(&body_json(response).await), "local_only");
    }

    #[tokio::test]
    async fn a_device_cannot_reach_a_local_route() {
        let harness = Harness::new();
        let body = ReexecRequest {
            exe: "/usr/bin/claudeclaw".into(),
        };
        let response = call(
            &harness.router(),
            json_request(Method::POST, "/v1/daemon/reexec", device("d1"), &body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(code(&body_json(response).await), "local_only");
    }

    #[tokio::test]
    async fn an_unpaired_cert_is_unauthorized() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/status", unpaired()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(code(&body_json(response).await), "unauthorized");
    }

    #[tokio::test]
    async fn an_admin_route_needs_admin_not_just_a_token() {
        let harness = Harness::new();
        harness.tokens.add("secret", "tok-1", vec![], false);
        let response = call(
            &harness.router(),
            bearer_request(Method::POST, "/v1/daemon/stop", "secret"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = body_json(response).await;
        assert_eq!(code(&body), "missing_permission");
        assert_eq!(required(&body), "admin");
    }

    #[tokio::test]
    async fn a_token_needs_the_routes_permission() {
        let harness = Harness::new();
        harness.tokens.add("none", "tok-1", vec![], false);
        harness
            .tokens
            .add("read", "tok-2", vec![TokenPermission::StatusRead], false);

        let refused = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/status", "none"),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        let body = body_json(refused).await;
        assert_eq!(code(&body), "missing_permission");
        assert_eq!(required(&body), "status:read");

        let allowed = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/status", "read"),
        )
        .await;
        assert!(allowed.status().is_success());
    }

    #[tokio::test]
    async fn a_bearer_is_accepted_refused_and_revoked() {
        let harness = Harness::new();
        harness
            .tokens
            .add("good", "tok-1", vec![TokenPermission::StatusRead], false);
        harness
            .tokens
            .add("dead", "tok-2", vec![TokenPermission::StatusRead], true);

        let accepted = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/status", "good"),
        )
        .await;
        assert!(accepted.status().is_success());

        let revoked = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/status", "dead"),
        )
        .await;
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

        let unknown = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/status", "never-issued"),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    }
}

mod audit {
    use super::*;

    #[tokio::test]
    async fn a_mutation_is_audited_but_a_read_is_not() {
        let harness = Harness::new();
        let stopped = call(
            &harness.router(),
            request(Method::POST, "/v1/daemon/stop", LOCAL),
        )
        .await;
        assert_eq!(stopped.status(), StatusCode::ACCEPTED);
        assert_eq!(harness.audit.len(), 1);
        let entry = &harness.audit.entries()[0];
        assert_eq!(entry.principal, "local");
        assert_eq!(entry.action, "daemon.stop");

        let read = call(&harness.router(), request(Method::GET, "/v1/status", LOCAL)).await;
        assert!(read.status().is_success());
        assert_eq!(harness.audit.len(), 1, "the GET added no entry");
    }

    #[tokio::test]
    async fn update_check_is_never_audited() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::POST, "/v1/update/check", LOCAL),
        )
        .await;
        assert!(response.status().is_success());
        assert!(harness.audit.is_empty());
    }

    #[tokio::test]
    async fn a_housekeeping_dry_run_is_not_audited() {
        let harness = Harness::new();
        let dry = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/housekeeping",
                LOCAL,
                &HousekeepingRequest { dry_run: true },
            ),
        )
        .await;
        assert!(dry.status().is_success());
        assert!(harness.audit.is_empty(), "a dry run leaves no audit entry");

        let real = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/housekeeping",
                LOCAL,
                &HousekeepingRequest { dry_run: false },
            ),
        )
        .await;
        assert!(real.status().is_success());
        assert_eq!(harness.audit.len(), 1);
        assert_eq!(harness.audit.entries()[0].action, "housekeeping.run");
    }

    #[tokio::test]
    async fn a_failed_authentication_is_not_audited() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/status", "never-issued"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(harness.audit.is_empty());
    }
}

mod lifecycle {
    use super::*;

    #[tokio::test]
    async fn a_mutation_while_starting_is_503_with_retry_after() {
        let harness = Harness::new();
        harness.status.set_state(HealthState::Starting);

        let response = call(
            &harness.router(),
            request(Method::POST, "/v1/daemon/stop", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "5");
        assert_eq!(code(&body_json(response).await), "daemon_starting");
        assert!(
            harness.audit.is_empty(),
            "the gate runs before the audit layer"
        );

        // Reads keep working while starting.
        let health = call(&harness.router(), request(Method::GET, "/v1/health", LOCAL)).await;
        assert!(health.status().is_success());
    }

    #[tokio::test]
    async fn a_mutation_while_stopping_is_503() {
        let harness = Harness::new();
        harness.status.set_state(HealthState::Stopping);
        let response = call(
            &harness.router(),
            request(Method::POST, "/v1/daemon/restart", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code(&body_json(response).await), "daemon_stopping");
    }
}

mod deadline {
    use std::time::Duration;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_slow_backend_hits_the_server_deadline() {
        let harness = Harness::new();
        harness.daemon.set_delay(Duration::from_secs(200));
        // `update.check` is `Slow`: the server backstop is 115 s.
        let response = call(
            &harness.router(),
            request(Method::POST, "/v1/update/check", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(code(&body_json(response).await), "deadline_exceeded");
    }
}

mod events {
    use super::*;

    fn revoked(id: &str) -> DaemonEvent {
        DaemonEvent::DeviceRevoked {
            device_id: id.into(),
        }
    }

    #[tokio::test]
    async fn a_stream_starts_with_hello_then_events() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/events", device("d1")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.starts_with("text/event-stream"),
            "{content_type}"
        );

        harness.events.publish(revoked("d1"));
        let mut body = response.into_body().into_data_stream();
        let hello = next_frame(&mut body).await;
        assert!(hello.contains("event: hello"), "{hello}");
        let event = next_frame(&mut body).await;
        assert!(event.contains("event: daemon"), "{event}");
        assert!(event.contains("device.revoked"), "{event}");
    }

    #[tokio::test]
    async fn kinds_filter_skips_other_kinds() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/events?kinds=run.", device("d1")),
        )
        .await;
        harness.events.publish(revoked("d1"));
        harness.events.publish(DaemonEvent::RunStarted {
            run_id: "r1".into(),
            turn_id: "t1".into(),
            scope: claudeclaw_api::Scope::Global,
            job: None,
            launcher: "direct".into(),
        });
        let mut body = response.into_body().into_data_stream();
        let hello = next_frame(&mut body).await;
        assert!(hello.contains("event: hello"), "{hello}");
        // The `device.revoked` frame is filtered out; `run.started` arrives next.
        let event = next_frame(&mut body).await;
        assert!(event.contains("run.started"), "{event}");
    }
}

mod pairing {
    use super::*;
    use claudeclaw_api::pairing::{PairRequest, PairingCreate};

    #[tokio::test]
    async fn pairing_create_is_local_and_returns_a_live_code() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/pairing",
                LOCAL,
                &PairingCreate::default(),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["code"].as_str().unwrap().len(), 9); // XXXX-XXXX
        assert_eq!(body["fingerprint"].as_str().unwrap().len(), 43);
        let pairing_string = body["pairing_string"].as_str().unwrap();
        assert!(pairing_string.starts_with("ccpair1."), "{pairing_string}");
    }

    #[tokio::test]
    async fn a_device_cannot_create_a_pairing() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/pairing",
                device("d1"),
                &PairingCreate::default(),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(code(&body_json(response).await), "local_only");
    }

    #[tokio::test]
    async fn pair_needs_an_unpaired_cert() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            json_request(
                Method::POST,
                "/v1/pair",
                LOCAL,
                &PairRequest {
                    code: "ABCD-EFGH".into(),
                    label: "laptop".into(),
                    platform: "macos".into(),
                },
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(code(&body_json(response).await), "unauthorized");
    }

    #[tokio::test]
    async fn devices_list_serves_an_empty_list_to_admin() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/devices", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["devices"].as_array().unwrap().len(), 0);
    }
}

mod logs {
    use claudeclaw_api::error::{ApiError, ErrorCode};
    use claudeclaw_api::logs::LogLevel;

    use crate::backend::{LogCursor, LogSource, SequencedLog};

    use super::*;

    /// A `logs.stream` response, optionally carrying the `Last-Event-ID` a client resumes with.
    async fn stream_from(
        harness: &Harness,
        uri: &str,
        last_event_id: Option<&str>,
    ) -> axum::response::Response {
        let mut request = request(Method::GET, uri, LOCAL);
        if let Some(id) = last_event_id {
            request.headers_mut().insert(
                header::HeaderName::from_static("last-event-id"),
                id.parse().expect("a header value"),
            );
        }
        call(&harness.router(), request).await
    }

    /// The replay a stream asks for: one line at `seq`, ready to publish.
    fn replayed(seq: u64) -> SequencedLog {
        SequencedLog {
            seq,
            entry: sample_log_line(seq),
        }
    }

    #[tokio::test]
    async fn the_merged_daemon_source_hands_out_a_cursor_it_takes_back() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(
                Method::GET,
                "/v1/logs/daemon?limit=50&min_level=INFO&event=run.&scope=job:example&q=start",
                LOCAL,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["lines"].as_array().unwrap().len(), 1);

        let calls = harness.logs.tail_calls();
        assert_eq!(calls.len(), 1);
        let (source, query) = &calls[0];
        assert_eq!(*source, LogSource::Daemon, "the merge is server-side");
        assert_eq!(query.limit, 50);
        assert_eq!(query.min_level, Some(LogLevel::Info));
        assert_eq!(query.event.as_deref(), Some("run."));
        assert_eq!(
            query.scope.as_ref().map(ToString::to_string).as_deref(),
            Some("job:example")
        );
        assert_eq!(query.q.as_deref(), Some("start"));

        // The merged cursor comes back to the source that handed it out, and to no other.
        let merged = "2026-10-13:4096%7C2026-10-12:512";
        let accepted = call(
            &harness.router(),
            request(
                Method::GET,
                &format!("/v1/logs/daemon?before={merged}"),
                LOCAL,
            ),
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::OK);
        let cursor = LogCursor::parse(LogSource::Daemon, "2026-10-13:4096|2026-10-12:512").unwrap();
        assert_eq!(harness.logs.tail_calls()[1].1.before, Some(cursor));

        let refused = call(
            &harness.router(),
            request(
                Method::GET,
                &format!("/v1/logs/worker?before={merged}"),
                LOCAL,
            ),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert_eq!(code(&body_json(refused).await), "invalid_cursor");
    }

    #[tokio::test]
    async fn a_logs_read_token_reads_logs_runs_and_audit_and_a_plain_one_does_not() {
        let harness = Harness::new();
        harness
            .tokens
            .add("plain", "tok-1", vec![TokenPermission::StatusRead], false);
        harness
            .tokens
            .add("reader", "tok-2", vec![TokenPermission::LogsRead], false);

        for uri in ["/v1/logs/worker", "/v1/runs", "/v1/audit"] {
            let refused = call(&harness.router(), bearer_request(Method::GET, uri, "plain")).await;
            assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{uri}");
            let body = body_json(refused).await;
            assert_eq!(code(&body), "missing_permission", "{uri}");
            assert_eq!(required(&body), "logs:read", "{uri}");

            let allowed = call(
                &harness.router(),
                bearer_request(Method::GET, uri, "reader"),
            )
            .await;
            assert!(allowed.status().is_success(), "{uri}: {}", allowed.status());
        }
    }

    #[tokio::test]
    async fn a_query_the_logs_routes_refuse_is_a_400() {
        let harness = Harness::new();
        let refusals = [
            ("/v1/logs/gpu?limit=10", "bad_request"),
            ("/v1/logs/worker?limit=0", "bad_request"),
            ("/v1/logs/worker?limit=1001", "bad_request"),
            ("/v1/logs/worker?min_level=notice", "bad_request"),
            ("/v1/logs/worker?scope=nope", "invalid_scope"),
            ("/v1/logs/worker?before=junk", "invalid_cursor"),
            ("/v1/logs/daemon?before=2026-10-13:4096", "invalid_cursor"),
            ("/v1/logs/worker/stream?backfill=many", "bad_request"),
            ("/v1/logs/export?map=yes", "bad_request"),
            ("/v1/logs/export?since=yesterday", "bad_request"),
            ("/v1/logs/export?max_bytes=lots", "bad_request"),
        ];
        for (uri, expected) in refusals {
            let response = call(&harness.router(), request(Method::GET, uri, LOCAL)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(code(&body_json(response).await), expected, "{uri}");
        }
    }

    #[tokio::test]
    async fn the_log_summary_serves_the_badges() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/logs/summary", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["level"], "info");
        assert_eq!(body["since_boot"]["warn"], 1);
        assert_eq!(body["disk"]["daemon_bytes"], 1024);
    }

    #[tokio::test]
    async fn the_bug_report_export_is_served_and_never_audited() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(
                Method::GET,
                "/v1/logs/export?map=true&max_bytes=1024&app_version=1.2.3",
                LOCAL,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["manifest"]["format"], "claudeclaw-export/1");

        let calls = harness.logs.export_calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].map, "the caller asked for the map");
        assert_eq!(calls[0].max_bytes, Some(1024));
        assert_eq!(calls[0].app_version.as_deref(), Some("1.2.3"));
        assert!(
            harness.audit.is_empty(),
            "logs.export is a GET and is never audited"
        );
    }

    #[tokio::test]
    async fn a_second_export_is_409_and_a_leak_fails_closed_500() {
        let harness = Harness::new();
        harness.logs.fail_export_with(ApiError::new(
            ErrorCode::ExportBusy,
            "an export is already running",
        ));
        let busy = call(
            &harness.router(),
            request(Method::GET, "/v1/logs/export", LOCAL),
        )
        .await;
        assert_eq!(busy.status(), StatusCode::CONFLICT);
        assert_eq!(code(&body_json(busy).await), "export_busy");

        harness.logs.fail_export_with(ApiError::new(
            ErrorCode::ExportLeak,
            "the scrubber left a secret behind",
        ));
        let leak = call(
            &harness.router(),
            request(Method::GET, "/v1/logs/export", LOCAL),
        )
        .await;
        assert_eq!(leak.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(code(&body_json(leak).await), "export_leak");
    }

    #[tokio::test]
    async fn a_log_stream_opens_with_hello_then_carries_lines_with_their_ids() {
        let harness = Harness::new();
        harness.logs.set_backfill(vec![replayed(6)]);
        let response = stream_from(&harness, "/v1/logs/worker/stream?backfill=2", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.starts_with("text/event-stream"),
            "{content_type}"
        );

        let mut body = response.into_body().into_data_stream();
        let hello = next_frame(&mut body).await;
        assert!(hello.contains("event: hello"), "{hello}");
        assert!(hello.contains("\"last_seq\""), "{hello}");

        let replay = next_frame(&mut body).await;
        assert!(replay.contains("event: log"), "{replay}");
        assert!(replay.contains("id: boot-1:6"), "{replay}");

        harness.logs.publish(7);
        let live = next_frame(&mut body).await;
        assert!(live.contains("event: log"), "{live}");
        assert!(live.contains("id: boot-1:7"), "{live}");

        let calls = harness.logs.stream_calls();
        assert_eq!(calls[0].0, LogSource::Worker);
        assert_eq!(calls[0].1.backfill, 2, "?backfill= lines to replay");
    }

    #[tokio::test]
    async fn a_last_event_id_resumes_in_its_own_boot_and_reports_one_it_cannot() {
        // Same boot: the replay continues after the last id the client saw.
        let harness = Harness::new();
        harness.logs.set_backfill(vec![replayed(6)]);
        let response = stream_from(&harness, "/v1/logs/worker/stream", Some("boot-1:5")).await;
        let mut body = response.into_body().into_data_stream();
        let hello = next_frame(&mut body).await;
        assert!(hello.contains("event: hello"), "{hello}");
        let replay = next_frame(&mut body).await;
        assert!(replay.contains("event: log"), "{replay}");
        assert!(replay.contains("id: boot-1:6"), "{replay}");
        assert_eq!(harness.logs.stream_calls()[0].1.resume, Some(5));

        // Another boot: a `restart` gap says the client starts over (§1.6).
        let harness = Harness::new();
        let response = stream_from(&harness, "/v1/logs/worker/stream", Some("boot-0:5")).await;
        let mut body = response.into_body().into_data_stream();
        let hello = next_frame(&mut body).await;
        assert!(hello.contains("event: hello"), "{hello}");
        let gap = next_frame(&mut body).await;
        assert!(gap.contains("event: gap"), "{gap}");
        assert!(gap.contains("\"restart\""), "{gap}");

        // A header that is not `<boot_id>:<seq>` still says so, rather than skipping silently.
        let harness = Harness::new();
        let response = stream_from(&harness, "/v1/logs/worker/stream", Some("junk")).await;
        let mut body = response.into_body().into_data_stream();
        let hello = next_frame(&mut body).await;
        assert!(hello.contains("event: hello"), "{hello}");
        let gap = next_frame(&mut body).await;
        assert!(gap.contains("event: gap"), "{gap}");
        assert!(gap.contains("\"evicted\""), "{gap}");
        assert_eq!(
            harness.logs.stream_calls()[0].1.resume,
            None,
            "there was no resume point to pass on"
        );
    }

    #[tokio::test]
    async fn a_client_that_falls_behind_the_ring_gets_a_gap_frame() {
        let harness = Harness::new();
        let response = stream_from(&harness, "/v1/logs/worker/stream", None).await;
        let mut body = response.into_body().into_data_stream();
        let hello = next_frame(&mut body).await;
        assert!(hello.contains("event: hello"), "{hello}");

        for seq in 1..=20 {
            harness.logs.publish(seq);
        }
        let gap = next_frame(&mut body).await;
        assert!(gap.contains("event: gap"), "{gap}");
        assert!(gap.contains("\"lagged\""), "{gap}");
        assert!(gap.contains("\"missed\""), "{gap}");

        let line = next_frame(&mut body).await;
        assert!(line.contains("event: log"), "{line}");
    }
}

mod runs {
    use claudeclaw_api::runs::{RunOrigin, RunOutcome};

    use super::*;

    #[tokio::test]
    async fn a_run_page_carries_the_index_progress() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(
                Method::GET,
                "/v1/runs?limit=10&origin=scheduler&job=daily-report",
                LOCAL,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["runs"][0]["run_id"], "run-1");
        assert_eq!(
            body["index"]["state"], "ready",
            "the page labels partial history"
        );

        let calls = harness.runs.list_calls();
        assert_eq!(calls[0].limit, 10);
        assert_eq!(calls[0].origin, Some(RunOrigin::Scheduler));
        assert_eq!(calls[0].job.as_deref(), Some("daily-report"));
        assert!(calls[0].before.is_none());
    }

    #[tokio::test]
    async fn a_run_record_and_its_log_as_markdown() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/runs/run-1", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["run_id"], "run-1");
        assert_eq!(harness.runs.get_calls(), ["run-1"]);

        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/runs/run-1/log?tail_lines=50", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.starts_with("text/markdown"), "{content_type}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("The Run log"), "{text}");
        assert_eq!(harness.runs.log_calls(), [Some(50)]);

        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/runs/run-1/log", LOCAL),
        )
        .await;
        assert!(response.status().is_success());
        assert_eq!(
            harness.runs.log_calls(),
            [Some(50), None],
            "tail_lines is optional"
        );
    }

    #[tokio::test]
    async fn the_run_log_needs_runs_content_as_well_as_logs_read() {
        let harness = Harness::new();
        harness
            .tokens
            .add("logs", "tok-1", vec![TokenPermission::LogsRead], false);

        let refused = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/runs/run-1/log", "logs"),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        let body = body_json(refused).await;
        assert_eq!(code(&body), "missing_permission");
        assert_eq!(required(&body), "runs:content");

        let listed = call(
            &harness.router(),
            bearer_request(Method::GET, "/v1/runs", "logs"),
        )
        .await;
        assert!(listed.status().is_success(), "runs.list is logs:read alone");
    }

    #[tokio::test]
    async fn a_query_the_runs_routes_refuse_is_a_400() {
        let harness = Harness::new();
        let refusals = [
            ("/v1/runs?limit=0", "bad_request"),
            ("/v1/runs?origin=cloud", "bad_request"),
            ("/v1/runs?outcome=maybe", "bad_request"),
            ("/v1/runs?scope=nope", "invalid_scope"),
            ("/v1/runs?before=2026-10:42", "invalid_cursor"),
            ("/v1/runs/run-1/log?tail_lines=0", "bad_request"),
        ];
        for (uri, expected) in refusals {
            let response = call(&harness.router(), request(Method::GET, uri, LOCAL)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(code(&body_json(response).await), expected, "{uri}");
        }

        // `unknown` is a real outcome the set spells out; a word it does not, is not.
        let accepted = call(
            &harness.router(),
            request(Method::GET, "/v1/runs?outcome=unknown", LOCAL),
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(
            harness.runs.list_calls().last().unwrap().outcome,
            Some(RunOutcome::Unknown)
        );
    }
}

mod audit_list {
    use super::*;

    #[tokio::test]
    async fn the_audit_page_lists_what_the_layers_recorded_newest_first() {
        let harness = Harness::new();
        for action in ["stop", "restart"] {
            let response = call(
                &harness.router(),
                request(Method::POST, &format!("/v1/daemon/{action}"), LOCAL),
            )
            .await;
            assert_eq!(response.status(), StatusCode::ACCEPTED, "{action}");
        }

        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/audit?limit=10", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let entries = body["entries"].as_array().unwrap();
        assert_eq!(entries[0]["action"], "daemon.restart", "newest first");
        assert_eq!(entries[1]["action"], "daemon.stop");
        assert_eq!(entries[0]["principal"], "local");
        assert_eq!(harness.audit.list_calls()[0].limit, 10);
        assert_eq!(harness.audit.len(), 2, "the GET added no entry");
    }

    #[tokio::test]
    async fn an_audit_page_refuses_a_cursor_it_did_not_hand_out() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/audit?before=2026-10:42", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(code(&body_json(response).await), "invalid_cursor");

        let response = call(
            &harness.router(),
            request(Method::GET, "/v1/audit?limit=0", LOCAL),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(code(&body_json(response).await), "bad_request");
    }
}

mod usage {
    use claudeclaw_api::usage::UsageGroup;

    use super::*;

    #[tokio::test]
    async fn usage_get_serves_the_window_and_the_group_it_was_asked_for() {
        let harness = Harness::new();
        let response = call(
            &harness.router(),
            request(
                Method::GET,
                "/v1/usage?from=2026-10-01&to=2026-10-02&tz=Asia/Bangkok&group_by=job",
                LOCAL,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["totals"]["runs"], 1);

        let calls = harness.usage.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tz, "Asia/Bangkok");
        assert_eq!(calls[0].group_by, UsageGroup::Job);
        assert_eq!(calls[0].from.to_string(), "2026-10-01");
        assert_eq!(calls[0].to.to_string(), "2026-10-02");

        // A caller that leaves `group_by` out gets the first group §2.10 lists.
        let response = call(
            &harness.router(),
            request(
                Method::GET,
                "/v1/usage?from=2026-10-01&to=2026-10-02&tz=UTC",
                LOCAL,
            ),
        )
        .await;
        assert!(response.status().is_success());
        assert_eq!(harness.usage.calls()[1].group_by, UsageGroup::Scope);
    }

    #[tokio::test]
    async fn usage_get_is_admin_only() {
        let harness = Harness::new();
        harness
            .tokens
            .add("reader", "tok-1", vec![TokenPermission::LogsRead], false);
        let response = call(
            &harness.router(),
            bearer_request(
                Method::GET,
                "/v1/usage?from=2026-10-01&to=2026-10-02&tz=UTC",
                "reader",
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = body_json(response).await;
        assert_eq!(code(&body), "missing_permission");
        assert_eq!(required(&body), "admin");
        assert!(
            harness.usage.calls().is_empty(),
            "the gate runs before the handler"
        );
    }

    #[tokio::test]
    async fn usage_get_wants_a_whole_window_and_a_zone_name() {
        let harness = Harness::new();
        let refusals = [
            ("/v1/usage?to=2026-10-02&tz=UTC", "from"),
            ("/v1/usage?from=2026-10-01&tz=UTC", "to"),
            ("/v1/usage?from=2026-10-01&to=2026-10-02", "tz"),
            ("/v1/usage?from=yesterday&to=2026-10-02&tz=UTC", "from"),
            ("/v1/usage?from=2026-10-02&to=2026-10-01&tz=UTC", "from"),
            (
                "/v1/usage?from=2026-10-01&to=2026-10-02&tz=Europe/London/../etc",
                "tz",
            ),
            (
                "/v1/usage?from=2026-10-01&to=2026-10-02&tz=UTC&group_by=nonsense",
                "group_by",
            ),
        ];
        for (uri, mentioned) in refusals {
            let response = call(&harness.router(), request(Method::GET, uri, LOCAL)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body = body_json(response).await;
            assert_eq!(code(&body), "bad_request", "{uri}");
            let message = body["error"]["message"].as_str().unwrap_or_default();
            assert!(message.contains(mentioned), "{uri}: {message}");
        }
        assert!(
            harness.usage.calls().is_empty(),
            "nothing reached the backend"
        );
    }
}

#[cfg(unix)]
mod listener {
    use std::path::{Path, PathBuf};

    use super::*;

    /// `reqwest`'s `rustls-no-provider` needs a provider before any client is built.
    fn provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    fn client(path: &Path) -> reqwest::Client {
        provider();
        reqwest::Client::builder()
            .unix_socket(path)
            .build()
            .unwrap()
    }

    async fn serve(harness: &Harness, path: &Path) -> tokio::task::JoinHandle<()> {
        let listener = crate::listener::bind_local(path).await.unwrap();
        let app = harness.local_router();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_socket_serves_reads_and_local_writes() {
        let harness = Harness::new();
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("api.sock");
        let server = serve(&harness, &path).await;
        let client = client(&path);

        let health = client
            .get("http://localhost/v1/health")
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let body: serde_json::Value = health.json().await.unwrap();
        assert_eq!(body["state"], "ready");

        let reexec = client
            .post("http://localhost/v1/daemon/reexec")
            .json(&ReexecRequest {
                exe: "/usr/bin/claudeclaw".into(),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(reexec.status(), StatusCode::ACCEPTED);
        assert_eq!(harness.daemon.calls(), ["daemon.reexec"]);

        server.abort();
    }
}
