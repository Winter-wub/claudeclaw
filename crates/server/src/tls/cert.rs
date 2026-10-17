//! The Daemon's self-generated TLS certificate (architecture §4, §9.1; #13, #41).
//!
//! One ECDSA P-256 key and a self-signed certificate, generated with `rcgen` the first time
//! remote access is turned on and then reused. Both peers pin the other's key by the SHA-256 of
//! its SubjectPublicKeyInfo: [`DaemonCert::fingerprint`] is that hash in unpadded base64url (the
//! `pair` / `PairingString` form), and [`DaemonCert::fingerprint_curl`] is curl's
//! `sha256//<base64>` form.
//!
//! The PEM pair ([`CertPem`]) is what the store keeps at `daemon/secrets/tls/daemon.key|daemon.crt`;
//! this module never touches the filesystem (§3.1), it just produces and parses the bytes.

use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The certificate and its private key, as the store keeps them (PEM, both 0600).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertPem {
    /// The PKCS#8 private key, PEM-armoured (`-----BEGIN PRIVATE KEY-----`).
    pub key_pem: String,
    /// The self-signed certificate, PEM-armoured (`-----BEGIN CERTIFICATE-----`).
    pub cert_pem: String,
}

/// Why generating or loading the Daemon certificate failed.
#[derive(Debug, thiserror::Error)]
pub enum CertError {
    #[error("generating the Daemon certificate: {0}")]
    Rcgen(#[from] rcgen::Error),
    #[error("the stored certificate is not valid PEM")]
    Pem,
    #[error("the certificate carries no subject public key info")]
    Spki,
}

/// The Daemon's TLS key and certificate, with the pinned SPKI-SHA-256 already computed.
#[derive(Debug, Clone)]
pub struct DaemonCert {
    key_pkcs8: Vec<u8>,
    cert_der: Vec<u8>,
    key_pem: String,
    cert_pem: String,
    spki_sha256: [u8; 32],
}

impl DaemonCert {
    /// Generates a fresh ECDSA P-256 key and a self-signed certificate.
    pub fn generate() -> Result<Self, CertError> {
        let CertifiedKey { cert, signing_key } = generate_simple_self_signed(Vec::<String>::new())?;
        let cert_der = cert.der().to_vec();
        let spki_sha256 = spki_sha256(&cert_der)?;
        Ok(DaemonCert {
            key_pkcs8: signing_key.serialize_der(),
            key_pem: signing_key.serialize_pem(),
            cert_pem: cert.pem(),
            cert_der,
            spki_sha256,
        })
    }

    /// Reads back a certificate the store kept.
    pub fn from_pem(pem: &CertPem) -> Result<Self, CertError> {
        let cert_der = pem_decode(&pem.cert_pem, "CERTIFICATE").ok_or(CertError::Pem)?;
        let key_pkcs8 = pem_decode(&pem.key_pem, "PRIVATE KEY").ok_or(CertError::Pem)?;
        let spki_sha256 = spki_sha256(&cert_der)?;
        Ok(DaemonCert {
            key_pkcs8,
            cert_der,
            key_pem: pem.key_pem.clone(),
            cert_pem: pem.cert_pem.clone(),
            spki_sha256,
        })
    }

    /// The PEM pair to persist.
    pub fn to_pem(&self) -> CertPem {
        CertPem {
            key_pem: self.key_pem.clone(),
            cert_pem: self.cert_pem.clone(),
        }
    }

    /// The certificate chain for a rustls server config.
    pub fn cert_chain(&self) -> Vec<CertificateDer<'static>> {
        vec![CertificateDer::from(self.cert_der.clone())]
    }

    /// The private key for a rustls server config.
    pub fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_pkcs8.clone()))
    }

    /// The SPKI-SHA-256, unpadded base64url (43 characters): the `pair` and `PairingString` form.
    pub fn fingerprint(&self) -> String {
        base64_encode(&self.spki_sha256, true, false)
    }

    /// The same hash in curl's `--pinnedpubkey` form, `sha256//<base64>`.
    pub fn fingerprint_curl(&self) -> String {
        format!("sha256//{}", base64_encode(&self.spki_sha256, false, true))
    }
}

/// Computes the SPKI-SHA-256 of a certificate's DER bytes, if they parse.
pub fn spki_sha256(cert_der: &[u8]) -> Result<[u8; 32], CertError> {
    let spki = spki_der(cert_der).ok_or(CertError::Spki)?;
    Ok(Sha256::digest(spki).into())
}

/// Extracts the DER of a certificate's `subjectPublicKeyInfo` (RFC 5280 §4.1).
///
/// A minimal walk of `Certificate ::= SEQUENCE { tbsCertificate, … }` and
/// `TBSCertificate`, skipping the optional version, serial, signature, issuer, validity and
/// subject. No ASN.1 crate is on the workspace dependency list, so the few tag/length bytes are
/// read here.
pub fn spki_der(cert_der: &[u8]) -> Option<&[u8]> {
    let (tag, cert, _) = take(cert_der)?;
    if tag != 0x30 {
        return None;
    }
    let (tag, tbs, _) = take(cert)?;
    if tag != 0x30 {
        return None;
    }
    let mut rest = tbs;
    // version [0] EXPLICIT is optional (absent in a v1 certificate). Advance past it: the
    // first attempt left `rest` pointing at the version TLV while expecting an INTEGER.
    if rest.first() == Some(&0xa0) {
        take_advance(&mut rest)?;
    }
    let (tag, serial, _) = take_advance(&mut rest)?;
    if tag != 0x02 {
        return None;
    }
    let _ = serial;
    for _ in 0..4 {
        // signature, issuer, validity, subject
        take_advance(&mut rest)?;
    }
    let (tag, spki, raw) = take_advance(&mut rest)?;
    if tag != 0x30 {
        return None;
    }
    // `raw` is the whole SubjectPublicKeyInfo TLV, which is what gets hashed. `spki` is only
    // borrowed to prove the walk located a SEQUENCE.
    debug_assert!(!spki.is_empty());
    Some(raw)
}

/// Reads the next TLV, returning `(tag, content)`.
fn take(input: &[u8]) -> Option<(u8, &[u8], usize)> {
    let tag = *input.first()?;
    let first = *input.get(1)?;
    let (len, header) = if first & 0x80 == 0 {
        (first as usize, 2)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..count {
            len = (len << 8) | *input.get(2 + i)? as usize;
        }
        (len, 2 + count)
    };
    let end = header.checked_add(len)?;
    if input.len() < end {
        return None;
    }
    Some((tag, &input[header..end], end))
}

/// Reads the next TLV and advances `input` past it, returning `(tag, content, raw)`.
fn take_advance<'a>(input: &mut &'a [u8]) -> Option<(u8, &'a [u8], &'a [u8])> {
    let (tag, content, total) = take(input)?;
    let raw = &input[..total];
    *input = &input[total..];
    Some((tag, content, raw))
}

/// Standard and URL-safe base64 (RFC 4648), optionally padded.
pub(crate) fn base64_encode(input: &[u8], url_safe: bool, pad: bool) -> String {
    const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let alphabet = if url_safe { URL } else { STD };
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        out.push(alphabet[((n >> 18) & 63) as usize] as char);
        out.push(alphabet[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(alphabet[((n >> 6) & 63) as usize] as char);
        } else if pad {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(alphabet[(n & 63) as usize] as char);
        } else if pad {
            out.push('=');
        }
    }
    out
}

/// Decodes standard or URL-safe base64, ignoring `=` padding and ASCII whitespace.
pub(crate) fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for byte in input.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// Extracts the base64 body of a PEM block with `label`, e.g. `CERTIFICATE`.
pub(crate) fn pem_decode(pem: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = pem.find(&begin)? + begin.len();
    let stop = pem[start..].find(&end)? + start;
    base64_decode(&pem[start..stop])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{KeyPair, PublicKeyData};

    #[test]
    fn generates_once_and_round_trips_through_pem() {
        let cert = DaemonCert::generate().unwrap();
        let pem = cert.to_pem();
        assert!(pem.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(pem.key_pem.contains("BEGIN PRIVATE KEY"));
        let again = DaemonCert::from_pem(&pem).unwrap();
        assert_eq!(again.fingerprint(), cert.fingerprint());
        assert_eq!(again.fingerprint_curl(), cert.fingerprint_curl());
        assert_eq!(
            again.cert_chain()[0].as_ref(),
            cert.cert_chain()[0].as_ref()
        );
    }

    #[test]
    fn the_fingerprint_is_the_key_spki_hash() {
        let key = KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        let cert = params.self_signed(&key).unwrap();
        let expected: [u8; 32] = Sha256::digest(key.subject_public_key_info()).into();
        assert_eq!(spki_sha256(cert.der()).unwrap(), expected);
        assert_eq!(spki_der(cert.der()).unwrap(), key.subject_public_key_info());
    }

    #[test]
    fn the_fingerprint_is_43_base64url_chars() {
        let cert = DaemonCert::generate().unwrap();
        let fingerprint = cert.fingerprint();
        assert_eq!(fingerprint.len(), 43);
        assert!(!fingerprint.contains('='));
        assert!(!fingerprint.contains('+'));
        assert!(!fingerprint.contains('/'));
        assert!(cert.fingerprint_curl().starts_with("sha256//"));
        assert!(cert.fingerprint_curl().ends_with('='));
    }

    #[test]
    fn two_certificates_differ() {
        let a = DaemonCert::generate().unwrap();
        let b = DaemonCert::generate().unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b"", false, true), "");
        assert_eq!(base64_encode(b"f", false, true), "Zg==");
        assert_eq!(base64_encode(b"fo", false, true), "Zm8=");
        assert_eq!(base64_encode(b"foo", false, true), "Zm9v");
        assert_eq!(base64_encode(b"foob", false, true), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba", false, true), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar", false, true), "Zm9vYmFy");
        // URL-safe, unpadded.
        assert_eq!(base64_encode(&[0xfb, 0xff], true, false), "-_8");
        assert_eq!(base64_decode("-_8").unwrap(), [0xfb, 0xff]);
        for text in [
            "", "Zg==", "Zm8=", "Zm9v", "Zm9vYg==", "Zm9vYmE=", "Zm9vYmFy",
        ] {
            let bytes = base64_decode(text).unwrap();
            assert_eq!(base64_encode(&bytes, false, true), text);
        }
    }

    #[test]
    fn spki_parsing_rejects_garbage() {
        assert!(spki_sha256(b"not a certificate").is_err());
        assert!(spki_sha256(&[]).is_err());
    }
}
