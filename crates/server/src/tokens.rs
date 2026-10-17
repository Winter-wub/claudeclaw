//! API tokens: `daemon/secrets/api-tokens.json` (architecture §9.1; management-api §1.3, §2.12;
//! #13, #17, #21).
//!
//! Only a SHA-256 hash of a token's secret is stored (§1.3); the plaintext is shown once, by
//! `tokens.create`, and never again. The legacy `settings.apiToken` is imported here with
//! `source: "settings.apiToken"` and `send` (#17 §3). Revoking the legacy token keeps its hash as
//! a tombstone, so the same value is not re-imported on the next settings load — only a changed
//! value is (#21 D14).
//!
//! The file is a typed value here; persistence is [`crate::backend::pairing::SecretsStore`].

use std::sync::{Arc, PoisonError, RwLock};

use chrono::{DateTime, Utc};
use claudeclaw_api::{ApiTokenInfo, TokenPermission};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::auth::{TokenRecord, TokenStore};
use crate::tls::cert::base64_encode;

/// The `schema` this build writes into `api-tokens.json`.
pub const TOKENS_SCHEMA: u32 = 1;

/// The `source` of the legacy token imported from `settings.apiToken` (#17 §3).
pub const LEGACY_SOURCE: &str = "settings.apiToken";

/// `daemon/secrets/api-tokens.json` (architecture §9.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokensFile {
    #[serde(default = "tokens_schema")]
    pub schema: u32,
    #[serde(default)]
    pub tokens: Vec<Token>,
    /// Top-level keys a later build added, kept on a rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn tokens_schema() -> u32 {
    TOKENS_SCHEMA
}

impl Default for TokensFile {
    fn default() -> Self {
        TokensFile {
            schema: TOKENS_SCHEMA,
            tokens: Vec::new(),
            extra: Map::new(),
        }
    }
}

/// One API token, or its tombstone once revoked (#21 D14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub id: String,
    pub name: String,
    pub allow: Vec<TokenPermission>,
    /// The SHA-256 of the bearer value, as 64 lowercase hex characters. The only copy kept.
    pub hash: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
    /// `settings.apiToken` for the imported legacy token; `None` for one made here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Set once the token is revoked; the record stays as a tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Token {
    /// Whether the record is a live token rather than a revoked tombstone.
    pub fn is_live(&self) -> bool {
        self.revoked_at.is_none()
    }

    /// The `legacy` flag the wire type carries: true only for the imported `settings.apiToken`.
    pub fn legacy(&self) -> bool {
        self.source.as_deref() == Some(LEGACY_SOURCE)
    }

    /// This token as the wire [`ApiTokenInfo`].
    pub fn info(&self) -> ApiTokenInfo {
        ApiTokenInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            allow: self.allow.clone(),
            created_at: self.created_at,
            last_used_at: self.last_used_at,
            legacy: self.legacy(),
            source: self.source.clone(),
        }
    }
}

/// What a bearer token's SHA-256 means to the auth layer (management-api §1.2–1.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenLookup {
    /// A live token with these permissions.
    Granted {
        id: String,
        allow: Vec<TokenPermission>,
    },
    /// A revoked token: `401 unauthorized` (#21 D14).
    Revoked { id: String },
    /// No token with that hash.
    Unknown,
}

impl TokenLookup {
    /// The auth layer's [`TokenRecord`]: `None` for an unknown hash, a `revoked` record for a
    /// tombstone, so the auth layer answers `401 unauthorized` either way (§1.3).
    fn into_record(self, hash: [u8; 32]) -> Option<TokenRecord> {
        match self {
            TokenLookup::Granted { id, allow } => Some(TokenRecord {
                id,
                permissions: allow,
                revoked: false,
                hash,
            }),
            TokenLookup::Revoked { id } => Some(TokenRecord {
                id,
                permissions: Vec::new(),
                revoked: true,
                hash,
            }),
            TokenLookup::Unknown => None,
        }
    }
}

/// The auth layer's view of `api-tokens.json` (§1.3): a lookup by the SHA-256 of the bearer
/// value, constant-time inside [`TokensFile::lookup`].
impl TokenStore for RwLock<TokensFile> {
    fn lookup(&self, hash: &[u8; 32]) -> Option<TokenRecord> {
        read(self).lookup(hash).into_record(*hash)
    }
}

impl TokensFile {
    /// The live tokens, as the wire [`ApiTokenInfo`]. Revoked tombstones are not listed.
    pub fn infos(&self) -> Vec<ApiTokenInfo> {
        self.tokens
            .iter()
            .filter(|token| token.is_live())
            .map(Token::info)
            .collect()
    }

    /// What this hash means (§1.3, #21 D14).
    pub fn lookup(&self, hash: &[u8; 32]) -> TokenLookup {
        let hex = hex(hash);
        match self
            .tokens
            .iter()
            .find(|token| constant_time_eq(token.hash.as_bytes(), hex.as_bytes()))
        {
            Some(token) if token.is_live() => TokenLookup::Granted {
                id: token.id.clone(),
                allow: token.allow.clone(),
            },
            Some(token) => TokenLookup::Revoked {
                id: token.id.clone(),
            },
            None => TokenLookup::Unknown,
        }
    }

    /// Inserts a new token, returning its id. The caller generates and hashes the secret.
    pub fn create(
        &mut self,
        id: String,
        name: String,
        allow: Vec<TokenPermission>,
        hash: [u8; 32],
        now: DateTime<Utc>,
    ) -> String {
        self.tokens.push(Token {
            id: id.clone(),
            name,
            allow,
            hash: hex(&hash),
            created_at: now,
            last_used_at: None,
            source: None,
            revoked_at: None,
        });
        id
    }

    /// Revokes a live token, keeping its tombstone. `false` when already revoked or absent.
    pub fn revoke(&mut self, id: &str, now: DateTime<Utc>) -> bool {
        match self
            .tokens
            .iter_mut()
            .find(|token| token.id == id && token.is_live())
        {
            Some(token) => {
                token.revoked_at = Some(now);
                true
            }
            None => false,
        }
    }

    /// Records a use of a live token (#13). `false` for an unknown or revoked id.
    pub fn record_use(&mut self, id: &str, now: DateTime<Utc>) -> bool {
        match self
            .tokens
            .iter_mut()
            .find(|token| token.id == id && token.is_live())
        {
            Some(token) => {
                token.last_used_at = Some(now);
                true
            }
            None => false,
        }
    }

    /// Keeps the legacy token in step with `settings.apiToken` (#17 §3, #21 D14).
    ///
    /// The stored hash is compared with `hash`: the same value changes nothing (a revoked legacy
    /// token stays revoked, so it is not re-imported); a changed value updates the record and
    /// clears the tombstone. A first-ever import adds the legacy token.
    pub fn import_legacy(&mut self, hash: [u8; 32], now: DateTime<Utc>) -> bool {
        let hex = hex(&hash);
        if self.tokens.iter().any(|token| token.hash == hex) {
            return false;
        }
        match self.tokens.iter_mut().find(|token| token.legacy()) {
            Some(token) => {
                token.hash = hex;
                token.revoked_at = None;
                token.last_used_at = None;
                true
            }
            None => {
                self.tokens.push(Token {
                    id: new_id(),
                    name: "legacy-apiToken".to_owned(),
                    allow: vec![TokenPermission::Send],
                    hash: hex,
                    created_at: now,
                    last_used_at: None,
                    source: Some(LEGACY_SOURCE.to_owned()),
                    revoked_at: None,
                });
                true
            }
        }
    }
}

/// A shared, mutable `api-tokens.json` for the auth layer and the service.
pub type SharedTokens = Arc<RwLock<TokensFile>>;

/// A fresh bearer value: 32 random bytes in unpadded base64url, shown once by `tokens.create`.
pub fn new_secret() -> Result<String, TokenError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| TokenError::Random)?;
    Ok(base64_encode(&bytes, true, false))
}

/// A fresh token id, `tok_<16 hex>`.
pub fn new_id() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    format!("tok_{}", hex(&bytes))
}

/// Why a secret could not be generated.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("the system random source failed")]
    Random,
}

/// Keeps the bytes on one side of the comparison and avoids an early exit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Lowercase hex, 64 characters for a SHA-256.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Locks for read, recovering a poisoned lock.
pub(crate) fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use claudeclaw_api::TokenPermission::*;

    use super::*;
    use crate::auth::hash_token;

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 16, h, 0, 0).unwrap()
    }

    fn tokens() -> TokensFile {
        let mut file = TokensFile::default();
        file.create(
            "tok-1".into(),
            "ci".into(),
            vec![LogsRead],
            hash_token("secret-one"),
            at(9),
        );
        file
    }

    #[test]
    fn create_stores_only_the_hash_and_lists_once() {
        let mut file = tokens();
        let hash = hash_token("secret-one");
        assert_eq!(
            file.lookup(&hash),
            TokenLookup::Granted {
                id: "tok-1".into(),
                allow: vec![LogsRead],
            }
        );
        // The plaintext is nowhere in the file.
        let text = serde_json::to_string(&file).unwrap();
        assert!(!text.contains("secret-one"));
        assert!(text.contains(&hex(&hash)));
        assert_eq!(file.infos().len(), 1);
        let info = &file.infos()[0];
        assert!(!info.legacy);
        assert_eq!(info.source, None);

        file.create(
            "tok-2".into(),
            "other".into(),
            vec![Send],
            hash_token("secret-two"),
            at(10),
        );
        assert_eq!(file.infos().len(), 2);
    }

    #[test]
    fn revoke_keeps_a_tombstone() {
        let mut file = tokens();
        let hash = hash_token("secret-one");
        assert!(file.revoke("tok-1", at(11)));
        assert!(!file.revoke("tok-1", at(12)));
        assert!(!file.revoke("missing", at(12)));
        assert_eq!(
            file.lookup(&hash),
            TokenLookup::Revoked { id: "tok-1".into() }
        );
        assert!(file.infos().is_empty());
        // The tombstone (and therefore its hash) stays.
        assert_eq!(file.tokens.len(), 1);
        assert_eq!(file.tokens[0].revoked_at, Some(at(11)));
    }

    #[test]
    fn record_use_touches_live_tokens_only() {
        let mut file = tokens();
        assert!(file.record_use("tok-1", at(11)));
        assert_eq!(file.infos()[0].last_used_at, Some(at(11)));
        assert!(!file.record_use("missing", at(11)));
        file.revoke("tok-1", at(12));
        assert!(!file.record_use("tok-1", at(13)));
    }

    #[test]
    fn unknown_hashes_are_unknown() {
        assert_eq!(tokens().lookup(&[7; 32]), TokenLookup::Unknown);
    }

    #[test]
    fn legacy_import_is_kept_in_step() {
        let mut file = TokensFile::default();
        let first = hash_token("legacy-value");
        assert!(file.import_legacy(first, at(9)));
        let legacy = file.infos().pop().unwrap();
        assert!(legacy.legacy);
        assert_eq!(legacy.source.as_deref(), Some(LEGACY_SOURCE));
        assert_eq!(legacy.allow, vec![Send]);
        // The same value is not re-imported.
        assert!(!file.import_legacy(first, at(10)));
        // Revoked, then the same value again: still no re-import.
        assert!(file.revoke(&legacy.id, at(11)));
        assert!(!file.import_legacy(first, at(12)));
        assert_eq!(
            file.lookup(&first),
            TokenLookup::Revoked {
                id: legacy.id.clone()
            }
        );
        // A changed value revives the record with the new hash.
        let second = hash_token("changed-value");
        assert!(file.import_legacy(second, at(13)));
        assert_eq!(
            file.lookup(&second),
            TokenLookup::Granted {
                id: legacy.id.clone(),
                allow: vec![Send],
            }
        );
        assert_eq!(file.lookup(&first), TokenLookup::Unknown);
        assert_eq!(file.infos().len(), 1);
        assert!(file.infos()[0].legacy);
    }

    #[test]
    fn secrets_and_ids_are_unique_and_not_plaintext() {
        let a = new_secret().unwrap();
        let b = new_secret().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(new_id().starts_with("tok_"));
        assert_ne!(new_id(), new_id());
        assert_eq!(hash_token("x").len(), 32);
    }

    #[test]
    fn json_round_trips_and_keeps_unknown_keys() {
        let file = tokens();
        let text = serde_json::to_string(&file).unwrap();
        let back: TokensFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back, file);
        let mut value: Value = serde_json::from_str(&text).unwrap();
        value["future"] = serde_json::json!(1);
        let back: TokensFile = serde_json::from_value(value).unwrap();
        assert_eq!(back.extra["future"], serde_json::json!(1));
    }
}
