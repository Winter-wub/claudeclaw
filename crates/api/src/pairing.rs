//! Pairing, Devices and API tokens (management-api §2.12, #13, #41): `pairing.create` / `get` /
//! `cancel`, `pair`, `devices.*` and `tokens.*`, plus the shared [`PairingString`] parser the CLI
//! and the app both use.
//!
//! A Pairing code and a token's secret are secrets: every type that carries one has a
//! hand-written `Debug` that leaves it out.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::permission::TokenPermission;
use crate::wire::wire_enum;

/// `pairing.create`'s body (management-api §2.12, #41 D2): `{host?}`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct PairingCreate {
    /// The host put into the Pairing string (`pair --host <name>`). A hostname or an IP
    /// literal ([`is_valid_host`]); anything else is `422 invalid_host`. When absent, the string
    /// carries `remote.listen` if that is a specific address, and no host if it is a wildcard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// `pairing.create` → a live Pairing code, valid for 10 minutes (management-api §2.12, #41).
/// `claudeclaw pair --no-wait --json` prints the same type.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PairingStart {
    pub pairing_id: String,
    /// The Pairing code as shown: `XXXX-XXXX` ([`PairingCode`]'s `Display`).
    pub code: String,
    pub expires_at: DateTime<Utc>,
    /// The Daemon's key fingerprint, as `pair` prints it.
    pub fingerprint: String,
    /// The same key in curl's `--pinnedpubkey` form (`sha256//<base64>`).
    pub fingerprint_curl: String,
    /// The remote listener's addresses, `host:port`.
    pub listen: Vec<String>,
    /// `ccpair1.<code>.<fingerprint>.<port>[.<host>]`; parse it with [`PairingString`].
    pub pairing_string: String,
}

impl fmt::Debug for PairingStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingStart")
            .field("pairing_id", &self.pairing_id)
            .field("code_len", &self.code.len())
            .field("expires_at", &self.expires_at)
            .field("fingerprint", &self.fingerprint)
            .field("fingerprint_curl", &self.fingerprint_curl)
            .field("listen", &self.listen)
            .field("pairing_string_len", &self.pairing_string.len())
            .finish()
    }
}

/// `pairing.get` → where a pairing stands, long-polled with `?wait=` (management-api §2.12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PairingState {
    pub state: PairingStatus,
    /// The new Device once `state` is `completed`; `null` otherwise.
    #[schema(required = true)]
    pub device: Option<DeviceInfo>,
}

wire_enum! {
    /// [`PairingState::state`] (management-api §2.12).
    pub enum PairingStatus {
        /// The code is live and unused.
        Pending = "pending",
        /// A Device paired with the code.
        Completed = "completed",
        /// The code's 10 minutes ran out.
        Expired = "expired",
        /// `pairing.cancel`, or `pair` stopped with Ctrl-C.
        Cancelled = "cancelled",
        /// Burned after 5 wrong attempts in total (#21 D15).
        Burned = "burned",
    }
}

/// `pairing.cancel` → `{cancelled}` (management-api §2.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PairingCancelled {
    /// `false` when the pairing had already ended.
    pub cancelled: bool,
}

/// `pair`'s body, sent over TLS with the new Device's cert (management-api §2.12, #13).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PairRequest {
    /// The Pairing code as entered; the server reads it with [`PairingCode`]'s `FromStr`, and a
    /// code that doesn't parse or match is `401 pairing_code_invalid`.
    pub code: String,
    /// The Device's name, e.g. the machine name.
    pub label: String,
    /// The Device's OS, shown as `<label> (<platform>)` (#41 D4).
    pub platform: String,
}

impl fmt::Debug for PairRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairRequest")
            .field("code_len", &self.code.len())
            .field("label", &self.label)
            .field("platform", &self.platform)
            .finish()
    }
}

/// `pair` → the new Device and the Daemon it paired with (management-api §2.12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PairResult {
    pub device: DeviceInfo,
    pub daemon: PairedDaemon,
}

/// The Daemon in a [`PairResult`] (management-api §2.12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PairedDaemon {
    pub name: String,
    pub version: String,
    pub fingerprint: String,
}

/// `devices.list` → `{devices}` (management-api §2.12). Revoked Devices are tombstones in
/// `devices.json` and are not listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeviceList {
    pub devices: Vec<DeviceInfo>,
}

/// One paired Device (management-api §2.12, #13). Every Device is full admin in v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeviceInfo {
    pub id: String,
    pub label: String,
    /// The fingerprint of the Device cert's key.
    pub fingerprint: String,
    pub created_at: DateTime<Utc>,
    /// `null` when the Device has not connected since it paired.
    #[schema(required = true)]
    pub last_seen_at: Option<DateTime<Utc>>,
    /// Whether the Device has a live connection now.
    pub connected: bool,
    /// Whether this is the calling Device.
    pub current: bool,
}

/// `devices.update`'s body: `{label}` (management-api §2.12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeviceUpdate {
    pub label: String,
}

/// `devices.revoke` → `{revoked, disconnected}` (management-api §2.12). The Device is kept as a
/// tombstone, so its cert then gets `401 device_revoked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeviceRevoked {
    pub revoked: bool,
    /// Whether a live connection of the Device was dropped.
    pub disconnected: bool,
}

/// `tokens.list` → `{tokens}` (management-api §2.12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TokenList {
    pub tokens: Vec<ApiTokenInfo>,
}

/// One API token (management-api §2.12, #13). Only its hash is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ApiTokenInfo {
    pub id: String,
    pub name: String,
    pub allow: Vec<TokenPermission>,
    pub created_at: DateTime<Utc>,
    /// `null` when the token has never been used.
    #[schema(required = true)]
    pub last_used_at: Option<DateTime<Utc>>,
    /// The token imported from the legacy `settings.apiToken` (#17 §3).
    pub legacy: bool,
    /// Where the token came from: `"settings.apiToken"` for the legacy token, `null` for one
    /// made with `tokens.create`.
    #[schema(required = true)]
    pub source: Option<String>,
}

/// `tokens.create`'s body (management-api §2.12). `allow` must pass
/// [`TokenPermission::valid_set`], or the call is `422 invalid_permission_set`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TokenCreate {
    pub name: String,
    pub allow: Vec<TokenPermission>,
}

/// `tokens.create` → `201` with the new token and its secret (management-api §2.12). The secret
/// appears only here; afterwards only its hash is kept.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TokenCreated {
    pub token: ApiTokenInfo,
    /// The bearer value. Never in `Debug`.
    pub secret: String,
}

impl fmt::Debug for TokenCreated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenCreated")
            .field("token", &self.token)
            .field("secret_len", &self.secret.len())
            .finish()
    }
}

/// `tokens.revoke` → `{revoked}` (management-api §2.12). Revoking the legacy token is kept by
/// hash, so the same `settings.apiToken` value is not imported again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TokenRevoked {
    pub revoked: bool,
}

/// Crockford base32's symbols, in value order.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The value of a Crockford base32 symbol: case-insensitive, with `I`/`L` read as `1` and `O`
/// as `0`. `U` and anything else is `None`.
fn crockford_value(b: u8) -> Option<u8> {
    let b = match b.to_ascii_uppercase() {
        b'I' | b'L' => b'1',
        b'O' => b'0',
        b => b,
    };
    CROCKFORD.iter().position(|&c| c == b).map(|v| v as u8)
}

/// A Pairing code (#41 D3): 8 Crockford base32 characters (40 bits), shown as `XXXX-XXXX`.
///
/// `FromStr` reads a code as a person enters it: case-insensitive, `-` and spaces ignored, `I`/`L`
/// read as `1` and `O` as `0`. `Display` gives `XXXX-XXXX`; [`PairingCode::compact`] gives the
/// 8 characters with no dash, as in a [`PairingString`]. `Debug` never shows the code.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PairingCode([u8; 8]);

impl PairingCode {
    /// The code for 40 random bits, most significant first.
    pub fn from_bytes(bytes: [u8; 5]) -> Self {
        let bits = bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
        let mut code = [0u8; 8];
        for (i, c) in code.iter_mut().enumerate() {
            *c = CROCKFORD[((bits >> (35 - 5 * i)) & 0x1f) as usize];
        }
        PairingCode(code)
    }

    /// The 8 canonical characters with no dash.
    pub fn compact(&self) -> &str {
        std::str::from_utf8(&self.0).expect("Crockford symbols are ASCII")
    }

    /// Exactly 8 symbols, no separators.
    fn from_symbols(s: &str) -> Result<Self, InvalidPairingCode> {
        let bytes = s.as_bytes();
        if bytes.len() != 8 {
            return Err(InvalidPairingCode);
        }
        let mut code = [0u8; 8];
        for (c, &b) in code.iter_mut().zip(bytes) {
            *c = CROCKFORD[crockford_value(b).ok_or(InvalidPairingCode)? as usize];
        }
        Ok(PairingCode(code))
    }
}

/// A Pairing code that isn't 8 Crockford base32 characters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid pairing code; expected 8 characters like XXXX-XXXX")]
pub struct InvalidPairingCode;

impl FromStr for PairingCode {
    type Err = InvalidPairingCode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let symbols: String = s.chars().filter(|&c| c != '-' && c != ' ').collect();
        PairingCode::from_symbols(&symbols)
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.compact();
        write!(f, "{}-{}", &code[..4], &code[4..])
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode(..)")
    }
}

/// The Pairing string's version tag (#41 D2).
const PAIRING_PREFIX: &str = "ccpair1";

/// A Pairing string, `ccpair1.<code>.<fingerprint>.<port>[.<host>]` (management-api §2.12,
/// #41 D2): the shared parser the CLI and the app use, so the app can pin the Daemon's key
/// before it sends the code.
///
/// - `<code>` is the Pairing code with no dash.
/// - `<fingerprint>` is the Daemon's SPKI SHA-256 in unpadded base64url (43 characters).
/// - `<port>` is `remote.port`.
/// - `<host>` is optional and last, because a hostname has dots and an IPv6 literal has colons.
///
/// `FromStr` trims surrounding whitespace (a paste) and rejects anything else that is
/// malformed. `Display` writes the canonical string. `Debug` never shows the code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PairingString {
    pub code: PairingCode,
    pub fingerprint: String,
    pub port: u16,
    pub host: Option<String>,
}

/// Why a [`PairingString`] didn't parse (#41 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidPairingString {
    /// Not `ccpair1.…`: from a newer app or Daemon, or not a Pairing string at all.
    #[error("unsupported pairing string; update the app")]
    Prefix,
    /// Fewer than the four required segments.
    #[error("invalid pairing string; expected ccpair1.<code>.<fingerprint>.<port>[.<host>]")]
    Segments,
    #[error("invalid pairing string; the code is not 8 Crockford base32 characters")]
    Code,
    #[error("invalid pairing string; the fingerprint is not 43 base64url characters")]
    Fingerprint,
    #[error("invalid pairing string; the port is not a number from 1 to 65535")]
    Port,
    #[error("invalid pairing string; the host is not a hostname or IP address")]
    Host,
}

impl FromStr for PairingString {
    type Err = InvalidPairingString;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut segments = s.trim().splitn(5, '.');
        if segments.next() != Some(PAIRING_PREFIX) {
            return Err(InvalidPairingString::Prefix);
        }
        let (Some(code), Some(fingerprint), Some(port)) =
            (segments.next(), segments.next(), segments.next())
        else {
            return Err(InvalidPairingString::Segments);
        };
        let host = segments.next();

        let code = PairingCode::from_symbols(code).map_err(|_| InvalidPairingString::Code)?;
        if !is_fingerprint(fingerprint) {
            return Err(InvalidPairingString::Fingerprint);
        }
        let port = parse_port(port).ok_or(InvalidPairingString::Port)?;
        if host.is_some_and(|h| !is_valid_host(h)) {
            return Err(InvalidPairingString::Host);
        }
        Ok(PairingString {
            code,
            fingerprint: fingerprint.to_owned(),
            port,
            host: host.map(str::to_owned),
        })
    }
}

impl fmt::Display for PairingString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{PAIRING_PREFIX}.{}.{}.{}",
            self.code.compact(),
            self.fingerprint,
            self.port
        )?;
        if let Some(host) = &self.host {
            write!(f, ".{host}")?;
        }
        Ok(())
    }
}

/// A SHA-256 in unpadded base64url: 43 characters, the last carrying 4 bits and 2 zero bits.
fn is_fingerprint(s: &str) -> bool {
    let value = |b: u8| match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    };
    let bytes = s.as_bytes();
    bytes.len() == 43
        && bytes.iter().all(|&b| value(b).is_some())
        && value(bytes[42]).is_some_and(|v| v % 4 == 0)
}

/// A decimal port from 1 to 65535, with no sign or leading zero.
fn parse_port(s: &str) -> Option<u16> {
    if s.is_empty() || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Whether `host` is an IP literal (IPv6 without brackets) or a hostname: dot-separated labels of
/// 1–63 letters, digits and inner hyphens, at most 253 bytes, whose last label is not all digits.
/// `pairing.create` answers anything else with `422 invalid_host` (#41).
pub fn is_valid_host(host: &str) -> bool {
    if host.parse::<Ipv4Addr>().is_ok() || host.parse::<Ipv6Addr>().is_ok() {
        return true;
    }
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    host.len() <= 253
        && host.split('.').all(label_ok)
        && !host
            .rsplit('.')
            .next()
            .is_some_and(|last| last.bytes().all(|b| b.is_ascii_digit()))
}

/// The schemas of this module, merged by [`crate::openapi`].
#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    PairingCreate,
    PairingStart,
    PairingState,
    PairingStatus,
    PairingCancelled,
    PairRequest,
    PairResult,
    PairedDaemon,
    DeviceList,
    DeviceInfo,
    DeviceUpdate,
    DeviceRevoked,
    TokenList,
    ApiTokenInfo,
    TokenCreate,
    TokenCreated,
    TokenRevoked,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    const FP: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, h, 0, 0).unwrap()
    }

    fn device() -> DeviceInfo {
        DeviceInfo {
            id: "dev-1".into(),
            label: "laptop".into(),
            fingerprint: "fp-device".into(),
            created_at: at(9),
            last_seen_at: Some(at(10)),
            connected: true,
            current: false,
        }
    }

    fn device_wire() -> serde_json::Value {
        json!({
            "id": "dev-1",
            "label": "laptop",
            "fingerprint": "fp-device",
            "created_at": "2026-10-14T09:00:00Z",
            "last_seen_at": "2026-10-14T10:00:00Z",
            "connected": true,
            "current": false,
        })
    }

    fn token() -> ApiTokenInfo {
        ApiTokenInfo {
            id: "tok-1".into(),
            name: "legacy-apiToken".into(),
            allow: vec![TokenPermission::Send],
            created_at: at(9),
            last_used_at: None,
            legacy: true,
            source: Some("settings.apiToken".into()),
        }
    }

    fn token_wire() -> serde_json::Value {
        json!({
            "id": "tok-1",
            "name": "legacy-apiToken",
            "allow": ["send"],
            "created_at": "2026-10-14T09:00:00Z",
            "last_used_at": null,
            "legacy": true,
            "source": "settings.apiToken",
        })
    }

    fn round_trip<T>(value: &T, wire: serde_json::Value)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + fmt::Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), wire);
        assert_eq!(&serde_json::from_value::<T>(wire).unwrap(), value);
    }

    #[test]
    fn pairing_create_shapes() {
        round_trip(&PairingCreate::default(), json!({}));
        round_trip(
            &PairingCreate {
                host: Some("build-box.example".into()),
            },
            json!({"host": "build-box.example"}),
        );
    }

    #[test]
    fn pairing_start_shape() {
        let start = PairingStart {
            pairing_id: "p-1".into(),
            code: "ABCD-EFGH".into(),
            expires_at: at(10),
            fingerprint: "fp-daemon".into(),
            fingerprint_curl: "sha256//fp".into(),
            listen: vec!["0.0.0.0:8443".into()],
            pairing_string: format!("ccpair1.ABCDEFGH.{FP}.8443"),
        };
        let wire = json!({
            "pairing_id": "p-1",
            "code": "ABCD-EFGH",
            "expires_at": "2026-10-14T10:00:00Z",
            "fingerprint": "fp-daemon",
            "fingerprint_curl": "sha256//fp",
            "listen": ["0.0.0.0:8443"],
            "pairing_string": format!("ccpair1.ABCDEFGH.{FP}.8443"),
        });
        round_trip(&start, wire);
    }

    #[test]
    fn pairing_state_shapes() {
        round_trip(
            &PairingState {
                state: PairingStatus::Pending,
                device: None,
            },
            json!({"state": "pending", "device": null}),
        );
        round_trip(
            &PairingState {
                state: PairingStatus::Completed,
                device: Some(device()),
            },
            json!({"state": "completed", "device": device_wire()}),
        );
        round_trip(
            &PairingCancelled { cancelled: true },
            json!({"cancelled": true}),
        );
    }

    #[test]
    fn pair_shapes() {
        round_trip(
            &PairRequest {
                code: "abcd-efgh".into(),
                label: "laptop".into(),
                platform: "macos".into(),
            },
            json!({"code": "abcd-efgh", "label": "laptop", "platform": "macos"}),
        );
        round_trip(
            &PairResult {
                device: device(),
                daemon: PairedDaemon {
                    name: "home".into(),
                    version: "2.0.0".into(),
                    fingerprint: "fp-daemon".into(),
                },
            },
            json!({
                "device": device_wire(),
                "daemon": {"name": "home", "version": "2.0.0", "fingerprint": "fp-daemon"},
            }),
        );
    }

    #[test]
    fn device_shapes() {
        round_trip(
            &DeviceList {
                devices: vec![device()],
            },
            json!({"devices": [device_wire()]}),
        );
        let never_seen = DeviceInfo {
            last_seen_at: None,
            connected: false,
            current: true,
            ..device()
        };
        let wire = serde_json::to_value(&never_seen).unwrap();
        assert_eq!(wire["last_seen_at"], json!(null));
        assert_eq!(
            serde_json::from_value::<DeviceInfo>(wire).unwrap(),
            never_seen
        );

        round_trip(
            &DeviceUpdate {
                label: "old laptop".into(),
            },
            json!({"label": "old laptop"}),
        );
        round_trip(
            &DeviceRevoked {
                revoked: true,
                disconnected: false,
            },
            json!({"revoked": true, "disconnected": false}),
        );
    }

    #[test]
    fn token_shapes() {
        round_trip(
            &TokenList {
                tokens: vec![token()],
            },
            json!({"tokens": [token_wire()]}),
        );
        round_trip(
            &TokenCreate {
                name: "ci".into(),
                allow: vec![TokenPermission::LogsRead, TokenPermission::RunsContent],
            },
            json!({"name": "ci", "allow": ["logs:read", "runs:content"]}),
        );
        round_trip(
            &TokenCreated {
                token: token(),
                secret: "s3cret-value".into(),
            },
            json!({"token": token_wire(), "secret": "s3cret-value"}),
        );
        round_trip(&TokenRevoked { revoked: true }, json!({"revoked": true}));
    }

    #[test]
    fn secrets_stay_out_of_debug() {
        let debug = format!(
            "{:?}",
            TokenCreated {
                token: token(),
                secret: "s3cret-value".into(),
            }
        );
        assert!(!debug.contains("s3cret"), "{debug}");
        assert!(debug.contains("secret_len: 12"), "{debug}");

        let debug = format!(
            "{:?}",
            PairRequest {
                code: "ABCD-EFGH".into(),
                label: "laptop".into(),
                platform: "macos".into(),
            }
        );
        assert!(!debug.contains("ABCD"), "{debug}");

        let debug = format!(
            "{:?}",
            PairingStart {
                pairing_id: "p-1".into(),
                code: "ABCD-EFGH".into(),
                expires_at: at(10),
                fingerprint: "fp-daemon".into(),
                fingerprint_curl: "sha256//fp".into(),
                listen: vec![],
                pairing_string: format!("ccpair1.ABCDEFGH.{FP}.8443"),
            }
        );
        assert!(!debug.contains("ABCD"), "{debug}");

        let parsed: PairingString = format!("ccpair1.ABCDEFGH.{FP}.8443").parse().unwrap();
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("ABCD"), "{debug}");
        assert!(debug.contains("8443"), "{debug}");
    }

    #[test]
    fn unknown_states_parse() {
        assert_eq!(
            serde_json::from_value::<PairingStatus>(json!("approved")).unwrap(),
            PairingStatus::Unknown
        );
    }

    #[test]
    fn pairing_code_reads_what_a_person_types() {
        let code: PairingCode = "ABCD-EFGH".parse().unwrap();
        assert_eq!(code.to_string(), "ABCD-EFGH");
        assert_eq!(code.compact(), "ABCDEFGH");
        assert_eq!("abcd efgh".parse::<PairingCode>().unwrap(), code);
        // I and L read as 1, O as 0.
        assert_eq!(
            "iLoO-0000".parse::<PairingCode>().unwrap().compact(),
            "11000000"
        );
        assert!("ABCD-EFG".parse::<PairingCode>().is_err());
        assert!("ABCD-EFGHJ".parse::<PairingCode>().is_err());
        assert!("ABCD-EFGU".parse::<PairingCode>().is_err());
        assert!("ABCD-EF*H".parse::<PairingCode>().is_err());
    }

    #[test]
    fn pairing_code_from_bytes() {
        assert_eq!(PairingCode::from_bytes([0; 5]).compact(), "00000000");
        assert_eq!(PairingCode::from_bytes([0xff; 5]).compact(), "ZZZZZZZZ");
        // 0x0842108421 is 1 in every 5-bit group.
        assert_eq!(
            PairingCode::from_bytes([0x08, 0x42, 0x10, 0x84, 0x21]).compact(),
            "11111111"
        );
    }

    #[test]
    fn pairing_string_round_trips_without_a_host() {
        let text = format!("ccpair1.ABCDEFGH.{FP}.8443");
        let parsed: PairingString = text.parse().unwrap();
        assert_eq!(
            parsed,
            PairingString {
                code: "ABCD-EFGH".parse().unwrap(),
                fingerprint: FP.into(),
                port: 8443,
                host: None,
            }
        );
        assert_eq!(parsed.to_string(), text);
        // A paste with a trailing newline.
        assert_eq!(format!("{text}\n").parse::<PairingString>(), Ok(parsed));
    }

    #[test]
    fn pairing_string_round_trips_with_a_host() {
        for host in [
            "build-box.tail1234.ts.net",
            "100.64.0.1",
            "fd7a:115c::1",
            "nas",
        ] {
            let text = format!("ccpair1.ABCDEFGH.{FP}.8443.{host}");
            let parsed: PairingString = text.parse().unwrap();
            assert_eq!(parsed.host.as_deref(), Some(host));
            assert_eq!(parsed.port, 8443);
            assert_eq!(parsed.to_string(), text);
            assert_eq!(parsed.to_string().parse::<PairingString>(), Ok(parsed));
        }
    }

    #[test]
    fn pairing_string_canonicalises_the_code() {
        let parsed: PairingString = format!("ccpair1.abcdefgh.{FP}.8443").parse().unwrap();
        assert_eq!(parsed.to_string(), format!("ccpair1.ABCDEFGH.{FP}.8443"));
    }

    #[test]
    fn pairing_string_rejects_malformed_input() {
        use InvalidPairingString::*;
        let cases = [
            // Bad prefix.
            (format!("ccpair2.ABCDEFGH.{FP}.8443"), Prefix),
            (format!("CCPAIR1.ABCDEFGH.{FP}.8443"), Prefix),
            (format!("ABCDEFGH.{FP}.8443"), Prefix),
            (String::new(), Prefix),
            // Missing segments.
            (format!("ccpair1.ABCDEFGH.{FP}"), Segments),
            ("ccpair1".to_owned(), Segments),
            // Wrong code length.
            (format!("ccpair1.ABCDEFG.{FP}.8443"), Code),
            (format!("ccpair1.ABCDEFGHJ.{FP}.8443"), Code),
            (format!("ccpair1.ABCD-EFGH.{FP}.8443"), Code),
            // Non-Crockford characters.
            (format!("ccpair1.ABCDEFGU.{FP}.8443"), Code),
            (format!("ccpair1.ABCDEF_H.{FP}.8443"), Code),
            (format!("ccpair1.ABCDEFGÉ.{FP}.8443"), Code),
            // Bad fingerprint.
            ("ccpair1.ABCDEFGH.short.8443".to_owned(), Fingerprint),
            (format!("ccpair1.ABCDEFGH.{}B.8443", &FP[..42]), Fingerprint),
            (format!("ccpair1.ABCDEFGH.{}+.8443", &FP[..42]), Fingerprint),
            // Bad port.
            (format!("ccpair1.ABCDEFGH.{FP}.0"), Port),
            (format!("ccpair1.ABCDEFGH.{FP}.65536"), Port),
            (format!("ccpair1.ABCDEFGH.{FP}.08443"), Port),
            (format!("ccpair1.ABCDEFGH.{FP}.+8443"), Port),
            (format!("ccpair1.ABCDEFGH.{FP}.port"), Port),
            (format!("ccpair1.ABCDEFGH.{FP}."), Port),
            // Extra segments.
            (format!("ccpair1.ABCDEFGH.ABCDEFGH.{FP}.8443"), Fingerprint),
            (format!("ccpair1.ABCDEFGH.{FP}.8443.9443"), Host),
            (format!("ccpair1.ABCDEFGH.{FP}.8443.host."), Host),
            (format!("ccpair1.ABCDEFGH.{FP}.8443..host"), Host),
            (format!("ccpair1.ABCDEFGH.{FP}.8443."), Host),
            // Bad host.
            (format!("ccpair1.ABCDEFGH.{FP}.8443.-box"), Host),
            (format!("ccpair1.ABCDEFGH.{FP}.8443.[fd7a::1]"), Host),
            (format!("ccpair1.ABCDEFGH.{FP}.8443.box:22"), Host),
        ];
        for (text, err) in cases {
            assert_eq!(text.parse::<PairingString>(), Err(err), "{text:?}");
        }
    }

    #[test]
    fn valid_hosts() {
        for host in [
            "localhost",
            "a.b-c.example",
            "100.64.0.1",
            "::1",
            "xn--bcher-kva.example",
        ] {
            assert!(is_valid_host(host), "{host}");
        }
        let long_label = "a".repeat(64);
        let long_host = ["a"; 128].join(".");
        for host in [
            "",
            "a..b",
            "a b",
            "-a",
            "a-",
            "1.2.3",
            "300.1.1.1",
            "h_st",
            &long_label,
            &long_host,
        ] {
            assert!(!is_valid_host(host), "{host}");
        }
    }
}
