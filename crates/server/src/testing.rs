//! In-process fakes and helpers for the server's tests.
//!
//! Every backend seam has a fake whose state is shared through `Arc`, so a test can build the
//! router once and then flip the lifecycle, count calls, or replay events from the handle.

#![cfg(test)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use tokio::sync::broadcast;

use claudeclaw_api::DaemonEvent;
use claudeclaw_api::logs::AuditEntry;
use claudeclaw_api::permission::TokenPermission;
use claudeclaw_api::status::{
    DaemonHealth, DaemonStatus, Health, HealthState, HeartbeatStatus, HousekeepingReport,
    HousekeepingRequest, JobsStatus, LaunchersStatus, ReexecAccepted, ReexecRequest, RemoteStatus,
    RestartAccepted, StatusSource, StatusVersion, StopAccepted, UpdateInfo, UpdateStatus,
};
use claudeclaw_api::supervisor::DaemonReachability;

use crate::auth::{Principal, TokenRecord, TokenStore, hash_token};
use crate::backend::pairing::{PairingService, TestClock, TestHooks};
use crate::backend::{
    AuditBackend, AuditFuture, BackendFuture, DaemonBackend, EventsBackend, StatusBackend,
};
use crate::pairing::PairingRegistry;
use crate::state::AppState;

/// The sample `Health` the read routes return.
pub fn sample_health(state: HealthState) -> Health {
    Health {
        state,
        boot_id: "boot-1".into(),
        version: "2.0.0".into(),
        api_version: "1".into(),
        import: None,
    }
}

/// A complete `DaemonStatus`, so `status.get` has something real to serve.
pub fn sample_status() -> DaemonStatus {
    DaemonStatus {
        dir: "/tmp/demo".into(),
        state: DaemonReachability::Running,
        health: DaemonHealth::Ok,
        problems: vec![],
        lock_holder: None,
        watchdog_pid: None,
        worker_pid: None,
        version: StatusVersion {
            daemon: "2.0.0".into(),
            cli: None,
        },
        started_at: None,
        boot_id: Some("boot-1".into()),
        socket: "/tmp/demo/.claude/claudeclaw/api.sock".into(),
        remote: RemoteStatus {
            enabled: false,
            listen: vec![],
            fingerprint: None,
            fingerprint_curl: None,
        },
        adapters: vec![],
        heartbeat: HeartbeatStatus {
            enabled: false,
            next_at: None,
        },
        jobs: JobsStatus {
            total: 0,
            invalid: 0,
            warnings: 0,
            next: vec![],
        },
        active_runs: 0,
        launchers: LaunchersStatus {
            default: "direct".into(),
            refused_scopes: vec![],
            direct_resolved: None,
        },
        update: UpdateStatus {
            available: false,
            latest: None,
        },
        run_path: None,
        run_locale: None,
        run_index: None,
        beacons: vec![],
        config_dir_mismatch: false,
        supervisor: None,
        source: StatusSource::Api,
    }
}

pub fn sample_update(available: bool) -> UpdateInfo {
    UpdateInfo {
        current: "2.0.0".into(),
        latest: Some(if available { "2.0.1" } else { "2.0.0" }.into()),
        available,
        checked_at: None,
        release_url: None,
    }
}

/// The `health.get` / `status.get` / `update.get` backend.
#[derive(Clone)]
pub struct FakeStatus {
    state: Arc<Mutex<HealthState>>,
}

impl FakeStatus {
    pub fn ready() -> Self {
        FakeStatus {
            state: Arc::new(Mutex::new(HealthState::Ready)),
        }
    }

    /// Flips the lifecycle the auth layer's gate reads.
    pub fn set_state(&self, state: HealthState) {
        *self.state.lock().unwrap() = state;
    }
}

impl StatusBackend for FakeStatus {
    fn health_state(&self) -> HealthState {
        *self.state.lock().unwrap()
    }

    fn health(&self) -> BackendFuture<'_, Health> {
        let health = sample_health(self.health_state());
        Box::pin(async move { Ok(health) })
    }

    fn status(&self) -> BackendFuture<'_, DaemonStatus> {
        Box::pin(async { Ok(sample_status()) })
    }

    fn update(&self) -> BackendFuture<'_, UpdateInfo> {
        Box::pin(async { Ok(sample_update(false)) })
    }
}

/// The daemon control backend. It counts the calls it saw and can stall to exercise the deadline.
#[derive(Clone, Default)]
pub struct FakeDaemon {
    calls: Arc<Mutex<Vec<&'static str>>>,
    delay: Arc<Mutex<Option<Duration>>>,
}

impl FakeDaemon {
    /// Makes every method sleep first, for the deadline test.
    pub fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = Some(delay);
    }

    /// The ops this backend has served, in order.
    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, op: &'static str) {
        self.calls.lock().unwrap().push(op);
    }

    async fn wait(&self) {
        let delay = *self.delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
    }
}

impl DaemonBackend for FakeDaemon {
    fn stop(&self) -> BackendFuture<'_, StopAccepted> {
        self.record("daemon.stop");
        Box::pin(async move {
            self.wait().await;
            Ok(StopAccepted {
                boot_id: "boot-1".into(),
                in_flight_runs: 0,
                deadline_at: crate::audit::now(),
            })
        })
    }

    fn restart(&self) -> BackendFuture<'_, RestartAccepted> {
        self.record("daemon.restart");
        Box::pin(async move {
            self.wait().await;
            Ok(RestartAccepted {
                old_boot_id: "boot-1".into(),
            })
        })
    }

    fn reexec(&self, _request: ReexecRequest) -> BackendFuture<'_, ReexecAccepted> {
        self.record("daemon.reexec");
        Box::pin(async move {
            self.wait().await;
            Ok(ReexecAccepted {
                old_boot_id: "boot-1".into(),
                target_version: "2.0.1".into(),
            })
        })
    }

    fn check_update(&self) -> BackendFuture<'_, UpdateInfo> {
        self.record("update.check");
        Box::pin(async move {
            self.wait().await;
            Ok(sample_update(true))
        })
    }

    fn housekeeping(&self, request: HousekeepingRequest) -> BackendFuture<'_, HousekeepingReport> {
        self.record("housekeeping.run");
        Box::pin(async move {
            self.wait().await;
            Ok(HousekeepingReport {
                would_delete: request.dry_run.then(std::collections::BTreeMap::new),
                deleted: (!request.dry_run).then(std::collections::BTreeMap::new),
                freed_bytes: 0,
            })
        })
    }
}

/// The `events.stream` bus.
#[derive(Clone)]
pub struct FakeEvents {
    boot_id: String,
    sender: broadcast::Sender<DaemonEvent>,
}

impl FakeEvents {
    pub fn new(boot_id: &str) -> Self {
        FakeEvents {
            boot_id: boot_id.to_owned(),
            sender: broadcast::channel(256).0,
        }
    }

    pub fn publish(&self, event: DaemonEvent) {
        let _ = self.sender.send(event);
    }
}

impl EventsBackend for FakeEvents {
    fn boot_id(&self) -> String {
        self.boot_id.clone()
    }

    fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.sender.subscribe()
    }
}

/// An audit backend that keeps every entry it is handed.
#[derive(Clone, Default)]
pub struct RecordingAudit {
    entries: Arc<Mutex<Vec<AuditEntry>>>,
}

impl RecordingAudit {
    pub fn entries(&self) -> Vec<AuditEntry> {
        self.entries.lock().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl AuditBackend for RecordingAudit {
    fn record(&self, entry: AuditEntry) -> AuditFuture<'_> {
        self.entries.lock().unwrap().push(entry);
        Box::pin(async { Ok(()) })
    }
}

/// An in-memory token store, keyed by the SHA-256 of each token's secret.
#[derive(Clone, Default)]
pub struct FakeTokens {
    records: Arc<Mutex<Vec<TokenRecord>>>,
}

impl FakeTokens {
    /// Adds a token. `revoked` exercises the revoked-token path.
    pub fn add(&self, secret: &str, id: &str, permissions: Vec<TokenPermission>, revoked: bool) {
        self.records.lock().unwrap().push(TokenRecord {
            id: id.to_owned(),
            permissions,
            revoked,
            hash: hash_token(secret),
        });
    }
}

impl TokenStore for FakeTokens {
    fn lookup(&self, hash: &[u8; 32]) -> Option<TokenRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|record| &record.hash == hash)
            .cloned()
    }
}

/// Everything a test needs: the state (for the real listener), the router, and each fake handle.
pub struct Harness {
    pub state: AppState,
    pub audit: RecordingAudit,
    pub daemon: FakeDaemon,
    pub status: FakeStatus,
    pub events: FakeEvents,
    pub tokens: FakeTokens,
}

impl Harness {
    pub fn new() -> Self {
        let audit = RecordingAudit::default();
        let daemon = FakeDaemon::default();
        let status = FakeStatus::ready();
        let events = FakeEvents::new("boot-1");
        let tokens = FakeTokens::default();
        // A real §2.12 service over an in-memory store, so the mounted routes are exercised
        // end to end.
        let clock = Arc::new(TestClock::at(9));
        let registry = PairingRegistry::with_clock(clock.clone());
        let state = AppState::builder()
            .status(status.clone())
            .daemon(daemon.clone())
            .events(events.clone())
            .audit(audit.clone())
            .tokens(tokens.clone())
            .with_pairing(PairingService::for_test(
                TestHooks::enabled(),
                registry,
                clock,
            ))
            .build();
        Harness {
            state,
            audit,
            daemon,
            status,
            events,
            tokens,
        }
    }

    pub fn router(&self) -> Router {
        crate::router::router(self.state.clone())
    }

    pub fn local_router(&self) -> Router {
        crate::router::local_app(self.state.clone())
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// A request stamped with a [`PeerIdentity`], as a listener would.
pub fn request(method: Method, uri: &str, identity: crate::auth::PeerIdentity) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(identity);
    request
}

/// A request from an unpaired remote cert carrying a bearer token.
pub fn bearer_request(method: Method, uri: &str, secret: &str) -> Request<Body> {
    let mut request = request(method, uri, crate::auth::PeerIdentity::UnknownCert);
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {secret}").parse().unwrap(),
    );
    request
}

/// A request with a JSON body.
pub fn json_request<T: serde::Serialize>(
    method: Method,
    uri: &str,
    identity: crate::auth::PeerIdentity,
    body: &T,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    request.extensions_mut().insert(identity);
    request
}

/// Drives one request through the router.
pub async fn call(router: &Router, request: Request<Body>) -> Response {
    use tower::ServiceExt;
    router
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible")
}

/// Reads a response's JSON body, or `null` for an empty one.
pub async fn body_json(response: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }
}

/// Asserts a `2xx` and returns the JSON body.
pub async fn ok_json(response: Response) -> serde_json::Value {
    assert!(
        response.status().is_success(),
        "expected success, got {}",
        response.status()
    );
    body_json(response).await
}

pub const LOCAL: crate::auth::PeerIdentity = crate::auth::PeerIdentity::Local;

pub fn device(id: &str) -> crate::auth::PeerIdentity {
    crate::auth::PeerIdentity::Device { id: id.to_owned() }
}

pub fn unpaired() -> crate::auth::PeerIdentity {
    crate::auth::PeerIdentity::UnknownCert
}

/// A principal's audit name, for assertions.
pub fn audit_name(principal: &Principal) -> String {
    principal.audit_name()
}

#[allow(dead_code)]
pub fn status_code() -> StatusCode {
    StatusCode::OK
}
