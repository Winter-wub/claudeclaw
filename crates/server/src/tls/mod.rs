//! The remote listener: `tokio-rustls` over TCP, with a custom [`ClientCertVerifier`] and the
//! per-connection [`PeerIdentity`] (architecture §4 `claudeclaw-server`; #13, #41).
//!
//! Client certificates are **optional**: any self-signed cert proves key possession because TLS
//! makes the client sign the handshake with the key it just presented. Authorisation is the HTTP
//! layer's job: the listener only resolves the cert's SPKI-SHA-256 against `devices.json` and
//! inserts [`PeerIdentity::Device`] (a live Device), [`PeerIdentity::UnknownCert`] (unknown or
//! revoked, or no cert) beside [`ClientCertSpki`] and [`PeerAddr`]. The auth layer turns that
//! into `401 unknown_device` / `401 device_revoked` / `Pairing`.
//!
//! The Daemon's own certificate is [`cert::DaemonCert`]; this module never touches the filesystem.

pub mod cert;
pub mod identity;

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Extension;
use axum::serve::IncomingStream;
use axum::{Router, serve};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::server::ServerConfig;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, Error, SignatureScheme};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use tracing::{debug, warn};

use crate::auth::PeerIdentity;
use crate::devices::{DeviceLookup, DeviceMatch};
use crate::tls::cert::{DaemonCert, base64_encode, spki_sha256};
use crate::tls::identity::{ClientCertSpki, PeerAddr};

/// The client-cert verifier: offer client auth, require nothing, and accept any certificate.
///
/// Accepting the cert is safe here because TLS still verifies the handshake signature it produced
/// with the cert's key (`verify_tls12_signature` / `verify_tls13_signature`), so an unauthenticated
/// client cannot impersonate a key it does not hold. The HTTP layer authorises separately.
#[derive(Debug)]
struct AllowAnyClientCert {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ClientCertVerifier for AllowAnyClientCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// A rustls server config for the remote listener: the Daemon cert, client certs optional and
/// accepted, verified with the `ring` provider (architecture §2, §4).
pub fn tls_config(cert: &DaemonCert) -> Result<Arc<ServerConfig>, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(AllowAnyClientCert {
        algorithms: provider.signature_verification_algorithms,
    });
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert.cert_chain(), cert.private_key())?;
    Ok(Arc::new(config))
}

/// A connection the remote listener accepted, with the identity resolved from its client cert.
struct RemoteIo {
    inner: TlsStream<TcpStream>,
    identity: PeerIdentity,
    spki: Option<ClientCertSpki>,
    peer: String,
}

impl RemoteIo {
    fn identity(&self) -> &PeerIdentity {
        &self.identity
    }

    fn spki(&self) -> Option<&ClientCertSpki> {
        self.spki.as_ref()
    }

    fn peer(&self) -> &str {
        &self.peer
    }
}

impl AsyncRead for RemoteIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for RemoteIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The `axum::serve` listener that accepts TCP and completes the TLS handshake per connection.
struct RemoteListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
    devices: Arc<dyn DeviceLookup>,
}

impl serve::Listener for RemoteListener {
    type Io = RemoteIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (tcp, addr) = match self.tcp.accept().await {
                Ok(pair) => pair,
                Err(err) => {
                    warn!(event = "api.remote_accept_failed", %err, "TCP accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            match self.acceptor.accept(tcp).await {
                Ok(stream) => {
                    let peer = addr.to_string();
                    let (identity, spki) = resolve(&stream, self.devices.as_ref());
                    debug!(event = "api.remote_tls_established", %peer, ?identity);
                    return (
                        RemoteIo {
                            inner: stream,
                            identity,
                            spki,
                            peer,
                        },
                        addr,
                    );
                }
                Err(err) => {
                    debug!(event = "api.remote_tls_failed", %addr, %err, "TLS handshake failed");
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

/// Resolves a completed handshake's client cert against `devices.json`.
fn resolve(
    stream: &TlsStream<TcpStream>,
    devices: &dyn DeviceLookup,
) -> (PeerIdentity, Option<ClientCertSpki>) {
    let Some(cert) = stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
    else {
        // No client cert: not Local, and not a Device either.
        return (PeerIdentity::UnknownCert, None);
    };
    let Ok(hash) = spki_sha256(cert.as_ref()) else {
        return (PeerIdentity::UnknownCert, None);
    };
    let fingerprint = base64_encode(&hash, true, false);
    let identity = match devices.lookup_spki(&fingerprint) {
        DeviceMatch::Known(id) => PeerIdentity::Device { id },
        // Revoked and unknown certs both reach only `POST /v1/pair`; the auth layer reads
        // `devices.json` (or the coordinator's richer `PeerIdentity`) to tell them apart.
        DeviceMatch::Revoked | DeviceMatch::Unknown => PeerIdentity::UnknownCert,
    };
    (identity, Some(ClientCertSpki(fingerprint)))
}

/// Serves `router` on the remote listener until the listener errors.
///
/// `devices` is consulted per connection, so a revocation takes effect on the next handshake.
pub async fn serve_remote(
    listener: TcpListener,
    tls: Arc<ServerConfig>,
    devices: Arc<dyn DeviceLookup>,
    router: Router,
) -> std::io::Result<()> {
    let listener = RemoteListener {
        tcp: listener,
        acceptor: TlsAcceptor::from(tls),
        devices,
    };
    serve(listener, MakeRouter { router }).await
}

/// The make-service `axum::serve` uses for [`RemoteListener`]: it clones `router` per connection
/// and layers that connection's [`PeerIdentity`], [`ClientCertSpki`] and [`PeerAddr`] onto it.
///
/// A named `Service` (not a `service_fn`) because the returned `Future` would otherwise capture
/// the higher-ranked `IncomingStream` lifetime (E0657).
struct MakeRouter {
    router: Router,
}

impl<'a> tower::Service<IncomingStream<'a, RemoteListener>> for MakeRouter {
    type Response = Router;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<Router, std::convert::Infallible>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, stream: IncomingStream<'a, RemoteListener>) -> Self::Future {
        let io = stream.io();
        let identity = io.identity().clone();
        let spki = io.spki().cloned();
        let peer = PeerAddr(io.peer().to_owned());
        let mut svc = self
            .router
            .clone()
            .layer(Extension(identity))
            .layer(Extension(peer));
        if let Some(spki) = spki {
            svc = svc.layer(Extension(spki));
        }
        std::future::ready(Ok(svc))
    }
}

/// An HTTP GET over any byte stream (a rustls client, or a test's), for callers that speak the
/// protocol directly. Returns the whole response; the status line and headers are not split off.
pub async fn get_body<S>(stream: &mut S, path: &str, host: &str) -> std::io::Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// A body-carrying router response for the listener tests.
#[cfg(test)]
pub(crate) fn identity_router() -> Router {
    use axum::routing::get;
    async fn handler(
        identity: Option<Extension<PeerIdentity>>,
        spki: Option<Extension<ClientCertSpki>>,
    ) -> String {
        let identity = identity
            .map(|Extension(identity)| match identity {
                PeerIdentity::Local => "local".to_owned(),
                PeerIdentity::Device { id } => format!("device:{id}"),
                PeerIdentity::UnknownCert => "unknown-device".to_owned(),
            })
            .unwrap_or_else(|| "none".to_owned());
        let spki = spki
            .map(|Extension(ClientCertSpki(spki))| spki)
            .unwrap_or_else(|| "none".to_owned());
        format!("identity={identity}\nspki={spki}\n")
    }
    Router::new().route("/whoami", get(handler))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};

    use chrono::{DateTime, TimeZone, Utc};
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::WebPkiSupportedAlgorithms;
    use rustls::pki_types::ServerName;

    use super::*;
    use crate::devices::{DeviceLookup, DevicesFile, SharedDevices};
    use crate::tls::cert::DaemonCert;

    /// Accepts the Daemon's self-signed certificate: the test pins by SPKI-SHA-256, not by a CA,
    /// which is exactly what the real client does.
    #[derive(Debug)]
    struct AcceptAnyServerCert {
        algorithms: WebPkiSupportedAlgorithms,
    }

    impl ServerCertVerifier for AcceptAnyServerCert {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.algorithms.supported_schemes()
        }
    }

    fn client_config(cert: Option<&DaemonCert>) -> rustls::ClientConfig {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert {
                algorithms: provider.signature_verification_algorithms,
            }));
        match cert {
            Some(cert) => builder
                .with_client_auth_cert(cert.cert_chain(), cert.private_key())
                .unwrap(),
            None => builder.with_no_client_auth(),
        }
    }

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 16, hour, 0, 0).unwrap()
    }

    /// One `GET /whoami` over a fresh pinned-TLS connection, with an optional client cert.
    async fn whoami(addr: SocketAddr, cert: Option<&DaemonCert>) -> String {
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config(cert)));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        let mut tls = connector.connect(name, tcp).await.unwrap();
        let raw = get_body(&mut tls, "/whoami", "localhost").await.unwrap();
        raw.split_once("\r\n\r\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or(raw)
    }

    #[tokio::test]
    async fn the_listener_resolves_a_client_cert_to_a_peer_identity() {
        // `rcgen` and `rustls` are built with `ring`; installing it also lets any implicit default
        // lookup succeed.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let server = DaemonCert::generate().unwrap();
        let known = DaemonCert::generate().unwrap();
        let revoked = DaemonCert::generate().unwrap();
        let unknown = DaemonCert::generate().unwrap();

        let mut file = DevicesFile::default();
        file.pair(
            "dev-known".into(),
            "laptop".into(),
            Some("macos".into()),
            known.fingerprint(),
            at(9),
        );
        let revoked_id = file.pair(
            "dev-revoked".into(),
            "old".into(),
            None,
            revoked.fingerprint(),
            at(9),
        );
        assert!(file.revoke(&revoked_id, at(10)));
        let devices: SharedDevices = Arc::new(RwLock::new(file));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let lookup: Arc<dyn DeviceLookup> = devices.clone();
        tokio::spawn(serve_remote(
            listener,
            tls_config(&server).unwrap(),
            lookup,
            identity_router(),
        ));

        // No client cert: no Device, and no SPKI to report.
        let body = whoami(addr, None).await;
        assert!(body.contains("identity=unknown-device"), "{body}");
        assert!(body.contains("spki=none"), "{body}");

        // A live Device's cert: its id, and the SPKI the auth layer can match to `devices.json`.
        let body = whoami(addr, Some(&known)).await;
        assert!(body.contains("identity=device:dev-known"), "{body}");
        assert!(
            body.contains(&format!("spki={}", known.fingerprint())),
            "{body}"
        );

        // A revoked Device's cert is not a Device any more, but its SPKI is still visible: the auth
        // layer turns this into `401 device_revoked` rather than `401 unknown_device` (#21 D14).
        let body = whoami(addr, Some(&revoked)).await;
        assert!(body.contains("identity=unknown-device"), "{body}");
        assert!(
            body.contains(&format!("spki={}", revoked.fingerprint())),
            "{body}"
        );

        // A cert no record mentions: unknown Device, SPKI still recorded for `POST /v1/pair`.
        let body = whoami(addr, Some(&unknown)).await;
        assert!(body.contains("identity=unknown-device"), "{body}");
        assert!(
            body.contains(&format!("spki={}", unknown.fingerprint())),
            "{body}"
        );
    }
}
