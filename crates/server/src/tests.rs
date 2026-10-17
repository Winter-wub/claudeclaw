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
