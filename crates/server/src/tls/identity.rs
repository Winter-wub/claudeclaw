//! The per-connection facts the remote listener stamps beside the auth layer's
//! [`PeerIdentity`] (architecture §4 `claudeclaw-server`; management-api §1.2).
//!
//! The identity enum itself lives in `crate::auth`; the listener inserts
//! [`PeerIdentity::Device`] / [`PeerIdentity::UnknownCert`] per connection. This module holds
//! the two facts that only the listener knows: the client cert's SPKI-SHA-256 and the peer's
//! address.

/// A paired Device's id (`DeviceInfo.id`).
pub type DeviceId = String;

/// The client cert a connection presented, if any: its SPKI-SHA-256 in unpadded base64url.
///
/// The remote listener inserts it beside [`PeerIdentity`] so `POST /v1/pair` can record the new
/// Device's public key while the peer is still [`PeerIdentity::UnknownCert`] (#41).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCertSpki(pub String);

/// The peer's address as text, e.g. `127.0.0.1:53124`; used in the pairing owner notice (#41 D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAddr(pub String);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::PeerIdentity;

    #[test]
    fn the_facts_are_content_free() {
        assert_eq!(ClientCertSpki("fp".into()).0, "fp");
        assert_eq!(PeerAddr("127.0.0.1:53124".into()).0, "127.0.0.1:53124");
        assert_eq!(PeerIdentity::UnknownCert.peer(), "unknown");
    }
}
