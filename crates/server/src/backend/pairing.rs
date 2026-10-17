//! The Pairing, Devices and API tokens backend (management-api §2.12; architecture §4
//! `claudeclaw-server`; #13, #41, #21).
//!
//! [`PairingBackend`] is the seam the routes call. It is a trait so the binary's composition root
//! can implement or wrap it, and so the routes can be tested without `core`. [`PairingService`] is
//! the ready-made implementation over three ports:
//!
//! - [`SecretsStore`] reads and writes the typed `devices.json` / `api-tokens.json` / cert values.
//!   `claudeclaw-server` itself never touches the filesystem (§3.1); the composition root supplies
//!   a file-backed store. [`MemorySecrets`] is the in-memory one for tests.
//! - [`PairingHooks`] is what only `core` knows: the Daemon's name and version, the remote
//!   listener's binding, the live connections, and the owner notices (#41 D4).
//! - [`crate::pairing::PairingRegistry`] owns the in-memory Pairing codes.
//!
//! Secrets are write-only: no type here carries a secret in its `Debug`, and no log line or error
//! message echoes one.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};

use claudeclaw_api::PairingCode;
use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::log_event::daemon;
use claudeclaw_api::pairing::{
    DeviceInfo, DeviceList, DeviceRevoked, DeviceUpdate, PairRequest, PairResult, PairedDaemon,
    PairingCancelled, PairingCreate, PairingStart, PairingState, PairingStatus, PairingString,
    TokenCreate, TokenCreated, TokenList, TokenRevoked, is_valid_host,
};
use claudeclaw_api::permission::TokenPermission;
use tracing::{info, warn};

use crate::auth::hash_token;
use crate::backend::MissingBackend;
use crate::devices::{DevicesFile, SharedDevices};
use crate::pairing::{Clock, PairingRegistry, PairingView, Submit};
use crate::tls::cert::{CertPem, DaemonCert};
use crate::tls::identity::{ClientCertSpki, DeviceId, PeerAddr};
use crate::tokens::{SharedTokens, TokensFile, hex, new_id, new_secret};

/// A future the backend returns: boxed and `Send`, so [`PairingBackend`] stays object-safe and its
/// methods can be called from an axum handler.
pub type PairFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PairingError>> + Send + 'a>>;

/// A future the [`SecretsStore`] returns.
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// The §2.12 error envelope. A newtype over [`ApiError`] so the routes can return it directly and
/// the composition root can map it onto its own error type at merge.
///
/// The [`ApiError`] is boxed: it carries a couple of `String`s and a details map, and a bare
/// `Result<_, PairingError>` would otherwise trip `clippy::result_large_err`.
#[derive(Debug, Clone, PartialEq)]
pub struct PairingError(Box<ApiError>);

impl PairingError {
    /// An error with `code` and a message that never echoes a secret or request content.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        PairingError(Box::new(ApiError::new(code, message)))
    }

    /// Adds the `hint` the envelope carries.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        *self.0 = self.0.with_hint(hint);
        self
    }

    /// Adds a code-specific `details` field.
    pub fn with_detail(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        *self.0 = self.0.with_detail(key, value);
        self
    }

    /// The HTTP status this error is sent with.
    pub fn status(&self) -> u16 {
        self.0.status()
    }

    /// The stable error code.
    pub fn code(&self) -> ErrorCode {
        self.0.error.code
    }

    /// The wire body.
    pub fn body(&self) -> &ApiError {
        &self.0
    }

    /// `404 not_found` for a resource named by a path segment.
    pub fn not_found(what: &str) -> Self {
        PairingError::new(ErrorCode::NotFound, format!("no such {what}"))
    }

    /// `409 remote_disabled`: `pairing.create` while the remote listener is off.
    pub fn remote_disabled() -> Self {
        PairingError::new(ErrorCode::RemoteDisabled, "remote access is off")
            .with_hint("turn on settings.remote.enabled first")
    }

    /// `422 invalid_host`: a `pairing.create` host that is neither a hostname nor an IP literal.
    pub fn invalid_host() -> Self {
        PairingError::new(
            ErrorCode::InvalidHost,
            "the host must be a hostname or an IP address",
        )
    }

    /// `401 pairing_code_invalid`: an unknown, used, cancelled or burned code.
    pub fn pairing_code_invalid() -> Self {
        PairingError::new(ErrorCode::PairingCodeInvalid, "invalid pairing code")
    }

    /// `410 pairing_expired`: a code whose ten minutes ran out.
    pub fn pairing_expired() -> Self {
        PairingError::new(ErrorCode::PairingExpired, "the pairing code expired")
            .with_hint("run `pair` again on the Daemon's machine")
    }

    /// `422 invalid_permission_set`: `runs:content` without `logs:read`, or an unknown permission.
    pub fn invalid_permission_set() -> Self {
        PairingError::new(
            ErrorCode::InvalidPermissionSet,
            "the requested permissions are not a valid set",
        )
        .with_hint("`runs:content` needs `logs:read`")
    }

    /// `401 unauthorized`: `POST /v1/pair` reached without a client cert to pin.
    pub fn unauthorized() -> Self {
        PairingError::new(
            ErrorCode::Unauthorized,
            "pairing needs a client certificate",
        )
    }

    /// `500 internal`: a store or key-generation failure. `context` is a fixed, content-free
    /// label, never an error's own text.
    pub fn internal(context: &'static str) -> Self {
        PairingError::new(ErrorCode::Internal, context)
    }
}

impl From<ApiError> for PairingError {
    fn from(error: ApiError) -> Self {
        PairingError(Box::new(error))
    }
}

impl From<PairingError> for crate::error::ApiFailure {
    fn from(error: PairingError) -> Self {
        crate::error::ApiFailure(error.0)
    }
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.0.error.code.status(),
            self.0.error.message
        )
    }
}

impl std::error::Error for PairingError {}

/// Why a [`SecretsStore`] read or write failed. A fixed, content-free label; a store error's own
/// text may name a path, so it is never carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreError {
    context: &'static str,
}

impl StoreError {
    pub fn new(context: &'static str) -> Self {
        StoreError { context }
    }

    pub fn context(&self) -> &'static str {
        self.context
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "secrets store: {}", self.context)
    }
}

impl std::error::Error for StoreError {}

// ---------------------------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------------------------

/// Persistence for the §2.12 secrets. The composition root implements this over `store`; the
/// server never touches the filesystem (§3.1).
pub trait SecretsStore: Send + Sync + 'static {
    fn load_devices(&self) -> StoreFuture<'_, DevicesFile>;
    fn save_devices(&self, devices: DevicesFile) -> StoreFuture<'_, ()>;
    fn load_tokens(&self) -> StoreFuture<'_, TokensFile>;
    fn save_tokens(&self, tokens: TokensFile) -> StoreFuture<'_, ()>;
    /// `None` when no cert has been generated yet.
    fn load_cert(&self) -> StoreFuture<'_, Option<CertPem>>;
    fn save_cert(&self, cert: CertPem) -> StoreFuture<'_, ()>;
}

/// An in-memory [`SecretsStore`]: the server's own tests, and a stand-in for the composition root.
#[derive(Debug)]
pub struct MemorySecrets {
    devices: Mutex<Option<DevicesFile>>,
    tokens: Mutex<Option<TokensFile>>,
    cert: Mutex<Option<CertPem>>,
    fail: bool,
}

impl MemorySecrets {
    /// An empty store: a default `devices.json` and `api-tokens.json`, no cert.
    pub fn new() -> Self {
        MemorySecrets {
            devices: Mutex::new(Some(DevicesFile::default())),
            tokens: Mutex::new(Some(TokensFile::default())),
            cert: Mutex::new(None),
            fail: false,
        }
    }

    /// A store seeded with existing values.
    pub fn with(devices: DevicesFile, tokens: TokensFile) -> Self {
        MemorySecrets {
            devices: Mutex::new(Some(devices)),
            tokens: Mutex::new(Some(tokens)),
            cert: Mutex::new(None),
            fail: false,
        }
    }

    /// A store whose every read and write fails, for the error path.
    pub fn failing() -> Self {
        MemorySecrets {
            devices: Mutex::new(None),
            tokens: Mutex::new(None),
            cert: Mutex::new(None),
            fail: true,
        }
    }

    /// The stored `devices.json`.
    pub fn devices(&self) -> DevicesFile {
        lock(&self.devices).clone().unwrap_or_default()
    }

    /// The stored `api-tokens.json`.
    pub fn tokens(&self) -> TokensFile {
        lock(&self.tokens).clone().unwrap_or_default()
    }

    /// The stored cert PEM pair, if any.
    pub fn cert(&self) -> Option<CertPem> {
        lock(&self.cert).clone()
    }
}

impl Default for MemorySecrets {
    fn default() -> Self {
        MemorySecrets::new()
    }
}

impl SecretsStore for MemorySecrets {
    fn load_devices(&self) -> StoreFuture<'_, DevicesFile> {
        let value = if self.fail {
            None
        } else {
            lock(&self.devices).clone()
        };
        Box::pin(async move { value.ok_or_else(|| StoreError::new("reading devices")) })
    }

    fn save_devices(&self, devices: DevicesFile) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            if self.fail {
                return Err(StoreError::new("writing devices"));
            }
            *lock(&self.devices) = Some(devices);
            Ok(())
        })
    }

    fn load_tokens(&self) -> StoreFuture<'_, TokensFile> {
        let value = if self.fail {
            None
        } else {
            lock(&self.tokens).clone()
        };
        Box::pin(async move { value.ok_or_else(|| StoreError::new("reading tokens")) })
    }

    fn save_tokens(&self, tokens: TokensFile) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            if self.fail {
                return Err(StoreError::new("writing tokens"));
            }
            *lock(&self.tokens) = Some(tokens);
            Ok(())
        })
    }

    fn load_cert(&self) -> StoreFuture<'_, Option<CertPem>> {
        let value = if self.fail {
            None
        } else {
            lock(&self.cert).clone()
        };
        // `fail` only affects reads through the `Option`: a failing store has no cert.
        Box::pin(async move {
            if self.fail {
                return Err(StoreError::new("reading the certificate"));
            }
            Ok(value)
        })
    }

    fn save_cert(&self, cert: CertPem) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            if self.fail {
                return Err(StoreError::new("writing the certificate"));
            }
            *lock(&self.cert) = Some(cert);
            Ok(())
        })
    }
}

/// The Daemon's own name and version, for `PairResult.daemon`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonIdentity {
    pub name: String,
    pub version: String,
}

/// The remote listener's binding, from `settings.remote` (architecture §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBinding {
    /// `settings.remote.enabled`: `pairing.create` is `409 remote_disabled` when false.
    pub enabled: bool,
    /// `settings.remote.listen`, e.g. `0.0.0.0` or a Tailscale name; the Pairing string's default
    /// host unless it is a wildcard.
    pub host: String,
    /// `settings.remote.port`.
    pub port: u16,
    /// The addresses the listener actually bound, `host:port`; `PairingStart.listen`.
    pub listen: Vec<String>,
}

/// Everything §2.12 needs that only `core` knows (#41 D4). The composition root implements this.
pub trait PairingHooks: Send + Sync + 'static {
    /// The Daemon's name and version.
    fn daemon(&self) -> DaemonIdentity;

    /// The remote listener's binding.
    fn remote(&self) -> RemoteBinding;

    /// The Device ids with a live connection now, for `DeviceInfo.connected`.
    fn connected(&self) -> BTreeSet<DeviceId>;

    /// Records that a Device connected, so `last_seen_at` and the connected set move.
    fn device_connected(&self, device_id: &DeviceId);

    /// Drops a Device's live connections. `true` when one was dropped, for
    /// `DeviceRevoked.disconnected`.
    fn disconnect(&self, device_id: &str) -> bool;

    /// Raises the `device.paired` [`claudeclaw_api::DaemonEvent`] and the owner notice (#41).
    fn announce_paired(&self, device: &DeviceInfo, peer: Option<&PeerAddr>);

    /// Raises the `device.revoked` event and the owner notice (#41).
    fn announce_revoked(&self, device_id: &DeviceId);
}

/// The `pair` caller's connection facts: the cert to pin, and where it came from.
#[derive(Debug, Clone, Default)]
pub struct PairingPeer {
    /// The client cert's SPKI-SHA-256, inserted by the remote listener. `None` means no cert, and
    /// `pair` is `401 unauthorized`: there is nothing to pin.
    pub spki: Option<ClientCertSpki>,
    /// The peer address, for the owner notice.
    pub peer: Option<PeerAddr>,
}

// ---------------------------------------------------------------------------------------------
// The backend seam
// ---------------------------------------------------------------------------------------------

/// The §2.12 operations, as the routes see them.
pub trait PairingBackend: Send + Sync + 'static {
    /// `pairing.create` (local): a new Pairing code, `409 remote_disabled` when remote is off.
    fn pairing_create(&self, request: PairingCreate) -> PairFuture<'_, PairingStart>;

    /// `pairing.get` (local): a long-poll for the pairing's state, waiting up to `wait`.
    fn pairing_get(&self, pairing_id: String, wait: Duration) -> PairFuture<'_, PairingState>;

    /// `pairing.cancel` (local).
    fn pairing_cancel(&self, pairing_id: String) -> PairFuture<'_, PairingCancelled>;

    /// `pair` (remote listener only): redeem a code, pin the cert, and record the Device.
    fn pair(&self, request: PairRequest, peer: PairingPeer) -> PairFuture<'_, PairResult>;

    /// `devices.list`.
    fn devices_list(&self, current: Option<DeviceId>) -> PairFuture<'_, DeviceList>;

    /// `devices.update`.
    fn devices_update(
        &self,
        device_id: String,
        update: DeviceUpdate,
        current: Option<DeviceId>,
    ) -> PairFuture<'_, DeviceInfo>;

    /// `devices.revoke`.
    fn devices_revoke(&self, device_id: String) -> PairFuture<'_, DeviceRevoked>;

    /// `tokens.list`.
    fn tokens_list(&self) -> PairFuture<'_, TokenList>;

    /// `tokens.create`: the only time a token's secret is visible.
    fn tokens_create(&self, request: TokenCreate) -> PairFuture<'_, TokenCreated>;

    /// `tokens.revoke`.
    fn tokens_revoke(&self, token_id: String) -> PairFuture<'_, TokenRevoked>;
}

// ---------------------------------------------------------------------------------------------
// The ready-made implementation
// ---------------------------------------------------------------------------------------------

/// The §2.12 logic over a [`SecretsStore`], the in-memory [`PairingRegistry`] and [`PairingHooks`].
pub struct PairingService<H> {
    hooks: H,
    store: Arc<dyn SecretsStore>,
    registry: PairingRegistry,
    clock: Arc<dyn Clock>,
    devices: SharedDevices,
    tokens: SharedTokens,
    cert: Mutex<Option<DaemonCert>>,
}

impl<H: PairingHooks> PairingService<H> {
    /// Loads the persisted secrets and prepares the service. The Daemon certificate is loaded if
    /// one exists; [`PairingService::ensure_cert`] generates it on first use.
    pub async fn load(
        hooks: H,
        store: Arc<dyn SecretsStore>,
        registry: PairingRegistry,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, PairingError> {
        let devices = store
            .load_devices()
            .await
            .map_err(|_| PairingError::internal("could not read the secrets store"))?;
        let tokens = store
            .load_tokens()
            .await
            .map_err(|_| PairingError::internal("could not read the secrets store"))?;
        let cert = store
            .load_cert()
            .await
            .map_err(|_| PairingError::internal("could not read the secrets store"))?;
        let cert = cert
            .map(|pem| DaemonCert::from_pem(&pem))
            .transpose()
            .map_err(|_| PairingError::internal("the stored certificate is unreadable"))?;
        Ok(PairingService {
            hooks,
            store,
            registry,
            clock,
            devices: Arc::new(RwLock::new(devices)),
            tokens: Arc::new(RwLock::new(tokens)),
            cert: Mutex::new(cert),
        })
    }

    /// A service over default (empty) secrets, for tests that do not need persisted seeds.
    #[cfg(test)]
    pub(crate) fn for_test(hooks: H, registry: PairingRegistry, clock: Arc<dyn Clock>) -> Self {
        PairingService {
            hooks,
            store: Arc::new(MemorySecrets::new()),
            registry,
            clock,
            devices: Arc::new(RwLock::new(DevicesFile::default())),
            tokens: Arc::new(RwLock::new(TokensFile::default())),
            cert: Mutex::new(None),
        }
    }

    /// The live `devices.json` the remote listener reads through [`crate::devices::DeviceLookup`].
    pub fn devices_handle(&self) -> SharedDevices {
        Arc::clone(&self.devices)
    }

    /// The live `api-tokens.json` the auth layer reads through [`crate::auth::TokenStore`].
    pub fn tokens_handle(&self) -> SharedTokens {
        Arc::clone(&self.tokens)
    }

    /// The hooks, for the composition root.
    pub fn hooks(&self) -> &H {
        &self.hooks
    }

    /// The in-memory Pairing codes.
    pub fn registry(&self) -> &PairingRegistry {
        &self.registry
    }

    /// The Daemon certificate, if it has been generated or loaded.
    pub fn cert(&self) -> Option<DaemonCert> {
        lock(&self.cert).clone()
    }

    /// Returns the Daemon certificate, generating and persisting one on first use.
    pub async fn ensure_cert(&self) -> Result<DaemonCert, PairingError> {
        if let Some(cert) = self.cert() {
            return Ok(cert);
        }
        let cert = DaemonCert::generate()
            .map_err(|_| PairingError::internal("could not generate the Daemon certificate"))?;
        self.store
            .save_cert(cert.to_pem())
            .await
            .map_err(|_| PairingError::internal("could not write the secrets store"))?;
        *lock(&self.cert) = Some(cert.clone());
        Ok(cert)
    }

    /// The daemon block of a `PairResult`.
    fn daemon_wire(&self, cert: &DaemonCert) -> PairedDaemon {
        let daemon = self.hooks.daemon();
        PairedDaemon {
            name: daemon.name,
            version: daemon.version,
            fingerprint: cert.fingerprint(),
        }
    }

    /// The wire [`DeviceInfo`] for a record, with `current` supplied by the caller.
    fn device_info(&self, id: &str, current: bool) -> Option<DeviceInfo> {
        let connected = self.hooks.connected();
        let devices = read(&self.devices);
        let device = devices.get(id)?;
        Some(DeviceInfo {
            id: device.id.clone(),
            label: device.label.clone(),
            fingerprint: device.spki.clone(),
            created_at: device.created_at,
            last_seen_at: device.last_seen_at,
            connected: connected.contains(&device.id),
            current,
        })
    }

    /// The wire [`PairingState`] for a registry view.
    fn pairing_state(&self, view: PairingView) -> PairingState {
        let device = match view.status {
            PairingStatus::Completed => view
                .device_id
                .as_deref()
                .and_then(|id| self.device_info(id, false)),
            _ => None,
        };
        PairingState {
            state: view.status,
            device,
        }
    }

    async fn pairing_create_impl(
        &self,
        request: PairingCreate,
    ) -> Result<PairingStart, PairingError> {
        let remote = self.hooks.remote();
        if !remote.enabled {
            return Err(PairingError::remote_disabled());
        }
        if let Some(host) = &request.host
            && !is_valid_host(host)
        {
            return Err(PairingError::invalid_host());
        }
        let cert = self.ensure_cert().await?;
        let created = self
            .registry
            .create(request.host.clone())
            .map_err(|_| PairingError::internal("could not generate a pairing code"))?;
        let host = request.host.clone().or_else(|| default_host(&remote.host));
        let pairing_string = PairingString {
            code: created.code,
            fingerprint: cert.fingerprint(),
            port: remote.port,
            host,
        };
        info!(event = daemon::PAIRING_STARTED);
        Ok(PairingStart {
            pairing_id: created.id,
            code: created.code.to_string(),
            expires_at: created.expires_at,
            fingerprint: cert.fingerprint(),
            fingerprint_curl: cert.fingerprint_curl(),
            listen: remote.listen.clone(),
            pairing_string: pairing_string.to_string(),
        })
    }

    async fn pairing_get_impl(
        &self,
        pairing_id: String,
        wait: Duration,
    ) -> Result<PairingState, PairingError> {
        let deadline = Instant::now() + wait;
        loop {
            let Some(view) = self.registry.get(&pairing_id) else {
                return Err(PairingError::not_found("pairing"));
            };
            if view.status != PairingStatus::Pending || Instant::now() >= deadline {
                return Ok(self.pairing_state(view));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::sleep(remaining.min(Duration::from_millis(200))).await;
        }
    }

    async fn pairing_cancel_impl(
        &self,
        pairing_id: String,
    ) -> Result<PairingCancelled, PairingError> {
        if self.registry.get(&pairing_id).is_none() {
            return Err(PairingError::not_found("pairing"));
        }
        Ok(PairingCancelled {
            cancelled: self.registry.cancel(&pairing_id),
        })
    }

    async fn pair_impl(
        &self,
        request: PairRequest,
        peer: PairingPeer,
    ) -> Result<PairResult, PairingError> {
        let Some(spki) = peer.spki else {
            warn!(
                event = daemon::API_AUTH_FAILED,
                listener = "remote",
                reason = "no_client_cert",
                "pairing without a client certificate"
            );
            return Err(PairingError::unauthorized());
        };
        let Ok(code) = request.code.parse::<PairingCode>() else {
            self.registry.record_wrong_attempt();
            warn!(event = daemon::PAIRING_FAILED, "malformed pairing code");
            return Err(PairingError::pairing_code_invalid());
        };
        let now = self.clock.now();
        match self.registry.claim(code) {
            Submit::Matched { id, .. } => {
                let proposed = new_device_id()
                    .map_err(|_| PairingError::internal("could not generate a device id"))?;
                // The record is keyed by the cert, so re-pairing the same key reuses its id.
                let device_id = write(&self.devices).pair(
                    proposed,
                    request.label.clone(),
                    Some(request.platform.clone()),
                    spki.0.clone(),
                    now,
                );
                if !self.registry.complete(&id, device_id.clone()) {
                    // Another caller claimed the code between our claim and complete.
                    warn!(event = daemon::PAIRING_FAILED, "pairing code already used");
                    return Err(PairingError::pairing_code_invalid());
                }
                let snapshot = read(&self.devices).clone();
                self.store
                    .save_devices(snapshot)
                    .await
                    .map_err(|_| PairingError::internal("could not write the secrets store"))?;
                self.hooks.device_connected(&device_id);
                let device = self
                    .device_info(&device_id, false)
                    .ok_or_else(|| PairingError::internal("the paired device vanished"))?;
                self.hooks.announce_paired(&device, peer.peer.as_ref());
                info!(event = daemon::PAIRING_COMPLETED);
                let daemon = self.daemon_wire(&self.ensure_cert().await?);
                Ok(PairResult { device, daemon })
            }
            Submit::Expired { .. } => {
                warn!(event = daemon::PAIRING_FAILED, "pairing code expired");
                Err(PairingError::pairing_expired())
            }
            Submit::NoMatch => {
                self.registry.record_wrong_attempt();
                warn!(event = daemon::PAIRING_FAILED, "invalid pairing code");
                Err(PairingError::pairing_code_invalid())
            }
        }
    }

    async fn devices_list_impl(
        &self,
        current: Option<DeviceId>,
    ) -> Result<DeviceList, PairingError> {
        let connected = self.hooks.connected();
        let devices = read(&self.devices);
        Ok(DeviceList {
            devices: devices.infos(&connected, current.as_deref()),
        })
    }

    async fn devices_update_impl(
        &self,
        device_id: String,
        update: DeviceUpdate,
        current: Option<DeviceId>,
    ) -> Result<DeviceInfo, PairingError> {
        let renamed = write(&self.devices).rename(&device_id, update.label);
        if renamed.is_none() {
            return Err(PairingError::not_found("device"));
        }
        let snapshot = read(&self.devices).clone();
        self.store
            .save_devices(snapshot)
            .await
            .map_err(|_| PairingError::internal("could not write the secrets store"))?;
        self.device_info(&device_id, current.as_deref() == Some(device_id.as_str()))
            .ok_or_else(|| PairingError::not_found("device"))
    }

    async fn devices_revoke_impl(&self, device_id: String) -> Result<DeviceRevoked, PairingError> {
        if read(&self.devices).get(&device_id).is_none() {
            return Err(PairingError::not_found("device"));
        }
        let revoked = write(&self.devices).revoke(&device_id, self.clock.now());
        if !revoked {
            // Already a tombstone.
            return Ok(DeviceRevoked {
                revoked: false,
                disconnected: false,
            });
        }
        let snapshot = read(&self.devices).clone();
        self.store
            .save_devices(snapshot)
            .await
            .map_err(|_| PairingError::internal("could not write the secrets store"))?;
        let disconnected = self.hooks.disconnect(&device_id);
        self.hooks.announce_revoked(&device_id);
        info!(event = daemon::DEVICE_REVOKED);
        Ok(DeviceRevoked {
            revoked: true,
            disconnected,
        })
    }

    async fn tokens_list_impl(&self) -> Result<TokenList, PairingError> {
        Ok(TokenList {
            tokens: read(&self.tokens).infos(),
        })
    }

    async fn tokens_create_impl(&self, request: TokenCreate) -> Result<TokenCreated, PairingError> {
        if !TokenPermission::valid_set(&request.allow) {
            return Err(PairingError::invalid_permission_set());
        }
        let secret = new_secret()
            .map_err(|_| PairingError::internal("could not generate a token secret"))?;
        let id = new_id().map_err(|_| PairingError::internal("could not generate a token id"))?;
        let hash = hash_token(&secret);
        let now = self.clock.now();
        let token = {
            let mut tokens = write(&self.tokens);
            tokens.create(
                id.clone(),
                request.name.clone(),
                request.allow.clone(),
                hash,
                now,
            );
            tokens
                .tokens
                .iter()
                .find(|token| token.id == id)
                .map(crate::tokens::Token::info)
                .ok_or_else(|| PairingError::internal("the new token vanished"))?
        };
        let snapshot = read(&self.tokens).clone();
        self.store
            .save_tokens(snapshot)
            .await
            .map_err(|_| PairingError::internal("could not write the secrets store"))?;
        Ok(TokenCreated { token, secret })
    }

    async fn tokens_revoke_impl(&self, token_id: String) -> Result<TokenRevoked, PairingError> {
        if !read(&self.tokens).tokens.iter().any(|t| t.id == token_id) {
            return Err(PairingError::not_found("token"));
        }
        let revoked = write(&self.tokens).revoke(&token_id, self.clock.now());
        if !revoked {
            return Ok(TokenRevoked { revoked: false });
        }
        let snapshot = read(&self.tokens).clone();
        self.store
            .save_tokens(snapshot)
            .await
            .map_err(|_| PairingError::internal("could not write the secrets store"))?;
        Ok(TokenRevoked { revoked: true })
    }
}

impl<H: PairingHooks> PairingBackend for PairingService<H> {
    fn pairing_create(&self, request: PairingCreate) -> PairFuture<'_, PairingStart> {
        Box::pin(self.pairing_create_impl(request))
    }

    fn pairing_get(&self, pairing_id: String, wait: Duration) -> PairFuture<'_, PairingState> {
        Box::pin(self.pairing_get_impl(pairing_id, wait))
    }

    fn pairing_cancel(&self, pairing_id: String) -> PairFuture<'_, PairingCancelled> {
        Box::pin(self.pairing_cancel_impl(pairing_id))
    }

    fn pair(&self, request: PairRequest, peer: PairingPeer) -> PairFuture<'_, PairResult> {
        Box::pin(self.pair_impl(request, peer))
    }

    fn devices_list(&self, current: Option<DeviceId>) -> PairFuture<'_, DeviceList> {
        Box::pin(self.devices_list_impl(current))
    }

    fn devices_update(
        &self,
        device_id: String,
        update: DeviceUpdate,
        current: Option<DeviceId>,
    ) -> PairFuture<'_, DeviceInfo> {
        Box::pin(self.devices_update_impl(device_id, update, current))
    }

    fn devices_revoke(&self, device_id: String) -> PairFuture<'_, DeviceRevoked> {
        Box::pin(self.devices_revoke_impl(device_id))
    }

    fn tokens_list(&self) -> PairFuture<'_, TokenList> {
        Box::pin(self.tokens_list_impl())
    }

    fn tokens_create(&self, request: TokenCreate) -> PairFuture<'_, TokenCreated> {
        Box::pin(self.tokens_create_impl(request))
    }

    fn tokens_revoke(&self, token_id: String) -> PairFuture<'_, TokenRevoked> {
        Box::pin(self.tokens_revoke_impl(token_id))
    }
}

/// The builder's default [`PairingBackend`]: every §2.12 call fails loudly with `500 internal`,
/// so a half-wired server never serves a half-working Pairing API.
impl PairingBackend for MissingBackend {
    fn pairing_create(&self, _request: PairingCreate) -> PairFuture<'_, PairingStart> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }

    fn pairing_get(&self, _pairing_id: String, _wait: Duration) -> PairFuture<'_, PairingState> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }

    fn pairing_cancel(&self, _pairing_id: String) -> PairFuture<'_, PairingCancelled> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }

    fn pair(&self, _request: PairRequest, _peer: PairingPeer) -> PairFuture<'_, PairResult> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }

    fn devices_list(&self, _current: Option<DeviceId>) -> PairFuture<'_, DeviceList> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }

    fn devices_update(
        &self,
        _device_id: String,
        _update: DeviceUpdate,
        _current: Option<DeviceId>,
    ) -> PairFuture<'_, DeviceInfo> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }

    fn devices_revoke(&self, _device_id: String) -> PairFuture<'_, DeviceRevoked> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }

    fn tokens_list(&self) -> PairFuture<'_, TokenList> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }

    fn tokens_create(&self, _request: TokenCreate) -> PairFuture<'_, TokenCreated> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }

    fn tokens_revoke(&self, _token_id: String) -> PairFuture<'_, TokenRevoked> {
        Box::pin(async { Err(PairingError::internal("no pairing backend is configured")) })
    }
}

/// A fresh Device id, `dev_<16 hex>`.
fn new_device_id() -> Result<DeviceId, crate::random::Unavailable> {
    let mut bytes = [0u8; 8];
    crate::random::fill(&mut bytes)?;
    Ok(format!("dev_{}", hex(&bytes)))
}

/// The host a Pairing string carries when `pairing.create` was given none: `remote.listen` if it
/// names a specific address, and none if it is a wildcard (`0.0.0.0`, `::`) or empty.
fn default_host(listen: &str) -> Option<String> {
    let listen = listen.trim();
    if listen.is_empty() || listen == "*" {
        return None;
    }
    if let Ok(ip) = listen.parse::<std::net::IpAddr>()
        && ip.is_unspecified()
    {
        return None;
    }
    Some(listen.to_owned())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
pub(crate) use self::tests::{TestClock, TestHooks};

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
    use claudeclaw_api::TokenPermission;
    use claudeclaw_api::pairing::PairingCode;

    use super::*;

    /// A clock a test moves by hand.
    pub(crate) struct TestClock(Mutex<DateTime<Utc>>);

    impl TestClock {
        pub(crate) fn at(hour: u32) -> Self {
            TestClock(Mutex::new(
                Utc.with_ymd_and_hms(2026, 10, 16, hour, 0, 0).unwrap(),
            ))
        }

        fn set(&self, hour: u32) {
            *lock(&self.0) = Utc.with_ymd_and_hms(2026, 10, 16, hour, 0, 0).unwrap();
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *lock(&self.0)
        }
    }

    /// Hooks a test drives.
    pub(crate) struct TestHooks {
        enabled: bool,
        host: String,
        port: u16,
        listen: Vec<String>,
        connected: Mutex<BTreeSet<DeviceId>>,
        paired: Mutex<Vec<String>>,
        revoked: Mutex<Vec<String>>,
    }

    impl TestHooks {
        pub(crate) fn enabled() -> Self {
            TestHooks {
                enabled: true,
                host: "0.0.0.0".into(),
                port: 4633,
                listen: vec!["100.64.0.1:4633".into()],
                connected: Mutex::new(BTreeSet::new()),
                paired: Mutex::new(Vec::new()),
                revoked: Mutex::new(Vec::new()),
            }
        }
    }

    impl PairingHooks for TestHooks {
        fn daemon(&self) -> DaemonIdentity {
            DaemonIdentity {
                name: "home".into(),
                version: "2.0.0".into(),
            }
        }

        fn remote(&self) -> RemoteBinding {
            RemoteBinding {
                enabled: self.enabled,
                host: self.host.clone(),
                port: self.port,
                listen: self.listen.clone(),
            }
        }

        fn connected(&self) -> BTreeSet<DeviceId> {
            lock(&self.connected).clone()
        }

        fn device_connected(&self, device_id: &DeviceId) {
            lock(&self.connected).insert(device_id.clone());
        }

        fn disconnect(&self, device_id: &str) -> bool {
            lock(&self.connected).remove(device_id)
        }

        fn announce_paired(&self, device: &DeviceInfo, _peer: Option<&PeerAddr>) {
            lock(&self.paired).push(device.id.clone());
        }

        fn announce_revoked(&self, device_id: &DeviceId) {
            lock(&self.revoked).push(device_id.clone());
        }
    }

    async fn build(
        hooks: TestHooks,
        store: &Arc<MemorySecrets>,
        clock: &Arc<TestClock>,
    ) -> PairingService<TestHooks> {
        let registry = PairingRegistry::with_clock(clock.clone());
        PairingService::load(hooks, store.clone(), registry, clock.clone())
            .await
            .unwrap()
    }

    fn peer(spki: &str) -> PairingPeer {
        PairingPeer {
            spki: Some(ClientCertSpki(spki.into())),
            peer: Some(PeerAddr("127.0.0.1:53124".into())),
        }
    }

    async fn create_code(service: &PairingService<TestHooks>, host: Option<&str>) -> String {
        service
            .pairing_create(PairingCreate {
                host: host.map(str::to_owned),
            })
            .await
            .unwrap()
            .code
    }

    async fn pair_with(
        service: &PairingService<TestHooks>,
        code: String,
        label: &str,
        spki: &str,
    ) -> Result<PairResult, PairingError> {
        service
            .pair(
                PairRequest {
                    code,
                    label: label.into(),
                    platform: "macos".into(),
                },
                peer(spki),
            )
            .await
    }

    #[tokio::test]
    async fn pairing_create_needs_remote_enabled() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let mut hooks = TestHooks::enabled();
        hooks.enabled = false;
        let service = build(hooks, &store, &clock).await;
        let error = service
            .pairing_create(PairingCreate::default())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::RemoteDisabled);
        // No cert is generated while remote is off.
        assert!(store.cert().is_none());
    }

    #[tokio::test]
    async fn pairing_create_rejects_a_bad_host() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let error = service
            .pairing_create(PairingCreate {
                host: Some("not a host".into()),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidHost);
    }

    #[tokio::test]
    async fn pairing_create_returns_a_parseable_pairing_string() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let start = service
            .pairing_create(PairingCreate::default())
            .await
            .unwrap();
        assert_eq!(start.code.len(), 9); // XXXX-XXXX
        assert_eq!(start.expires_at, clock.now() + ChronoDuration::minutes(10));
        assert_eq!(start.listen, vec!["100.64.0.1:4633"]);
        assert_eq!(start.fingerprint.len(), 43);
        assert!(start.fingerprint_curl.starts_with("sha256//"));
        let parsed = PairingString::from_str(&start.pairing_string).unwrap();
        assert_eq!(parsed.fingerprint, start.fingerprint);
        assert_eq!(parsed.port, 4633);
        // A wildcard `remote.listen` leaves the host out of the string.
        assert_eq!(parsed.host, None);
        assert!(store.cert().is_some());
    }

    #[tokio::test]
    async fn a_failing_random_source_creates_no_pairing_code() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let error = {
            let _fail = crate::random::failing::enable();
            service
                .pairing_create(PairingCreate::default())
                .await
                .unwrap_err()
        };
        assert_eq!(error.code(), ErrorCode::Internal);
        assert_eq!(service.registry.pending_count(), 0);
    }

    #[tokio::test]
    async fn a_failing_random_source_creates_no_token() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let error = {
            let _fail = crate::random::failing::enable();
            service
                .tokens_create(TokenCreate {
                    name: "ci".into(),
                    allow: vec![TokenPermission::Send],
                })
                .await
                .unwrap_err()
        };
        assert_eq!(error.code(), ErrorCode::Internal);
        assert!(service.tokens_list().await.unwrap().tokens.is_empty());
    }

    #[tokio::test]
    async fn pairing_create_takes_a_specific_listen_address_as_the_host() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let mut hooks = TestHooks::enabled();
        hooks.host = "100.64.0.1".into();
        let start = build(hooks, &store, &clock)
            .await
            .pairing_create(PairingCreate::default())
            .await
            .unwrap();
        let parsed = PairingString::from_str(&start.pairing_string).unwrap();
        assert_eq!(parsed.host.as_deref(), Some("100.64.0.1"));
    }

    #[tokio::test]
    async fn a_generated_cert_is_reused_after_a_reload() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let first = build(TestHooks::enabled(), &store, &clock)
            .await
            .ensure_cert()
            .await
            .unwrap();
        let reloaded = build(TestHooks::enabled(), &store, &clock).await;
        assert_eq!(reloaded.cert().unwrap().fingerprint(), first.fingerprint());
    }

    #[tokio::test]
    async fn pairing_get_unknown_is_not_found() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let error = service
            .pairing_get("pair_nope".into(), Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn pairing_get_is_immediately_pending_with_no_wait() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let start = service
            .pairing_create(PairingCreate::default())
            .await
            .unwrap();
        let state = service
            .pairing_get(start.pairing_id.clone(), Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(state.state, PairingStatus::Pending);
        assert!(state.device.is_none());
    }

    #[tokio::test]
    async fn pair_without_a_cert_is_unauthorized() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let code = create_code(&service, None).await;
        let error = service
            .pair(
                PairRequest {
                    code,
                    label: "laptop".into(),
                    platform: "macos".into(),
                },
                PairingPeer::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Unauthorized);
    }

    #[tokio::test]
    async fn pair_records_the_device_and_completes_the_pairing() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let start = service
            .pairing_create(PairingCreate::default())
            .await
            .unwrap();
        let result = pair_with(&service, start.code.clone(), "laptop", "FP-LAPTOP")
            .await
            .unwrap();
        assert_eq!(result.device.label, "laptop");
        assert_eq!(result.device.fingerprint, "FP-LAPTOP");
        assert!(result.device.connected);
        assert!(!result.device.current);
        assert_eq!(result.daemon.name, "home");
        assert_eq!(result.daemon.version, "2.0.0");
        assert_eq!(result.daemon.fingerprint, start.fingerprint);

        // The owner was notified, the store holds the record, and the code is spent.
        assert_eq!(
            lock(&service.hooks.paired).clone(),
            vec![result.device.id.clone()]
        );
        let persisted = store.devices();
        assert_eq!(persisted.get(&result.device.id).unwrap().spki, "FP-LAPTOP");
        let again = pair_with(&service, start.code.clone(), "other", "FP-OTHER")
            .await
            .unwrap_err();
        assert_eq!(again.code(), ErrorCode::PairingCodeInvalid);

        // A completed pairing shows its Device.
        let state = service
            .pairing_get(start.pairing_id.clone(), Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(state.state, PairingStatus::Completed);
        assert_eq!(state.device.unwrap().id, result.device.id);
    }

    #[tokio::test]
    async fn re_pairing_the_same_key_keeps_the_device_id() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let first = pair_with(
            &service,
            create_code(&service, None).await,
            "laptop",
            "FP-SAME",
        )
        .await
        .unwrap();
        let second = pair_with(
            &service,
            create_code(&service, None).await,
            "old laptop",
            "FP-SAME",
        )
        .await
        .unwrap();
        assert_eq!(second.device.id, first.device.id);
        assert_eq!(second.device.label, "old laptop");
        assert_eq!(store.devices().devices.len(), 1);
    }

    #[tokio::test]
    async fn five_wrong_codes_burn_a_pairing() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let start = service
            .pairing_create(PairingCreate::default())
            .await
            .unwrap();
        for _ in 0..5 {
            let error = pair_with(&service, "ZZZZ-ZZZZ".into(), "x", "FP")
                .await
                .unwrap_err();
            assert_eq!(error.code(), ErrorCode::PairingCodeInvalid);
        }
        let state = service
            .pairing_get(start.pairing_id.clone(), Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(state.state, PairingStatus::Burned);
    }

    #[tokio::test]
    async fn an_expired_code_is_gone() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let code = create_code(&service, None).await;
        clock.set(10); // exactly ten minutes later
        let error = pair_with(&service, code, "laptop", "FP").await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::PairingExpired);
    }

    #[tokio::test]
    async fn pairing_cancel_ends_a_pairing() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let start = service
            .pairing_create(PairingCreate::default())
            .await
            .unwrap();
        assert!(
            service
                .pairing_cancel(start.pairing_id.clone())
                .await
                .unwrap()
                .cancelled
        );
        assert!(
            !service
                .pairing_cancel(start.pairing_id.clone())
                .await
                .unwrap()
                .cancelled
        );
        let error = service
            .pairing_cancel("pair_nope".into())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn devices_list_marks_the_current_and_connected_devices() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let paired = pair_with(&service, create_code(&service, None).await, "laptop", "FP")
            .await
            .unwrap();
        let list = service
            .devices_list(Some(paired.device.id.clone()))
            .await
            .unwrap();
        assert_eq!(list.devices.len(), 1);
        assert!(list.devices[0].current);
        assert!(list.devices[0].connected);
    }

    #[tokio::test]
    async fn devices_update_renames_and_persists() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let paired = pair_with(&service, create_code(&service, None).await, "laptop", "FP")
            .await
            .unwrap();
        let updated = service
            .devices_update(
                paired.device.id.clone(),
                DeviceUpdate {
                    label: "new name".into(),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(updated.label, "new name");
        assert_eq!(
            store.devices().get(&paired.device.id).unwrap().label,
            "new name"
        );
        let error = service
            .devices_update("dev_nope".into(), DeviceUpdate { label: "x".into() }, None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn devices_revoke_tombstones_and_disconnects() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let paired = pair_with(&service, create_code(&service, None).await, "laptop", "FP")
            .await
            .unwrap();
        let revoked = service
            .devices_revoke(paired.device.id.clone())
            .await
            .unwrap();
        assert!(revoked.revoked);
        assert!(revoked.disconnected); // it was connected from the pair
        assert_eq!(
            lock(&service.hooks.revoked).clone(),
            vec![paired.device.id.clone()]
        );
        // Listed no more, but the tombstone and its hash stay.
        assert!(service.devices_list(None).await.unwrap().devices.is_empty());
        assert_eq!(store.devices().get(&paired.device.id).unwrap().spki, "FP");
        // Revoking again is a no-op, and an unknown id is not found.
        assert!(
            !service
                .devices_revoke(paired.device.id.clone())
                .await
                .unwrap()
                .revoked
        );
        assert_eq!(
            service
                .devices_revoke("dev_nope".into())
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn tokens_create_shows_the_secret_once_and_keeps_only_its_hash() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let created = service
            .tokens_create(TokenCreate {
                name: "ci".into(),
                allow: vec![TokenPermission::LogsRead, TokenPermission::RunsContent],
            })
            .await
            .unwrap();
        assert_eq!(created.secret.len(), 43);
        assert_eq!(created.token.name, "ci");
        assert!(!created.token.legacy);
        // The secret is nowhere in the persisted file.
        let text = serde_json::to_string(&store.tokens()).unwrap();
        assert!(!text.contains(&created.secret));
        assert!(text.contains(&hex(&hash_token(&created.secret))));

        let list = service.tokens_list().await.unwrap();
        assert_eq!(list.tokens.len(), 1);
        assert_eq!(list.tokens[0].id, created.token.id);

        assert!(
            service
                .tokens_revoke(created.token.id.clone())
                .await
                .unwrap()
                .revoked
        );
        assert!(service.tokens_list().await.unwrap().tokens.is_empty());
        assert!(
            !service
                .tokens_revoke(created.token.id)
                .await
                .unwrap()
                .revoked
        );
        assert_eq!(
            service
                .tokens_revoke("tok_nope".into())
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn tokens_create_rejects_an_invalid_permission_set() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let error = service
            .tokens_create(TokenCreate {
                name: "bad".into(),
                allow: vec![TokenPermission::RunsContent],
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidPermissionSet);
    }

    #[tokio::test]
    async fn a_failing_store_surfaces_as_internal() {
        let store = Arc::new(MemorySecrets::failing());
        let clock = Arc::new(TestClock::at(9));
        let registry = PairingRegistry::with_clock(clock.clone());
        let error = match PairingService::load(TestHooks::enabled(), store, registry, clock).await {
            Ok(_) => panic!("load should fail when the store errors"),
            Err(error) => error,
        };
        assert_eq!(error.code(), ErrorCode::Internal);
    }

    #[tokio::test]
    async fn the_device_cert_round_trips_through_the_store() {
        let store = Arc::new(MemorySecrets::new());
        let clock = Arc::new(TestClock::at(9));
        let service = build(TestHooks::enabled(), &store, &clock).await;
        let cert = service.ensure_cert().await.unwrap();
        let pem = store.cert().unwrap();
        assert_eq!(
            DaemonCert::from_pem(&pem).unwrap().fingerprint(),
            cert.fingerprint()
        );
        // A second call returns the same cert rather than generating a new one.
        assert_eq!(
            service.ensure_cert().await.unwrap().fingerprint(),
            cert.fingerprint()
        );
    }

    #[tokio::test]
    async fn reloading_devices_and_tokens_from_the_store_works() {
        let clock = Arc::new(TestClock::at(9));
        let store = Arc::new(MemorySecrets::new());
        {
            let service = build(TestHooks::enabled(), &store, &clock).await;
            pair_with(&service, create_code(&service, None).await, "laptop", "FP")
                .await
                .unwrap();
            service
                .tokens_create(TokenCreate {
                    name: "ci".into(),
                    allow: vec![TokenPermission::Send],
                })
                .await
                .unwrap();
        }
        let reloaded = build(TestHooks::enabled(), &store, &clock).await;
        assert_eq!(reloaded.devices_list(None).await.unwrap().devices.len(), 1);
        assert_eq!(reloaded.tokens_list().await.unwrap().tokens.len(), 1);
    }

    #[test]
    fn errors_carry_the_envelope_and_never_echo_content() {
        let error = PairingError::internal("could not read the secrets store");
        assert_eq!(error.code(), ErrorCode::Internal);
        assert_eq!(error.status(), 500);
        let body = serde_json::to_value(error.body()).unwrap();
        assert_eq!(body["error"]["code"], "internal");
        assert_eq!(body["error"]["message"], "could not read the secrets store");
        assert!(body["error"].get("details").is_some());
        // The wire type still parses codes the round-trip doesn't know.
        assert_eq!(
            PairingCode::from_str("ZZZZ-ZZZZ").unwrap().compact(),
            "ZZZZZZZZ"
        );
    }
}
