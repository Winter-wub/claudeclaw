//! Revisions (management-api §1.5): the first 16 hex of SHA-256 over a file's bytes, sent back
//! as `If-Match` so an edit never silently overwrites a change made on disk.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{PartialSchema, ToSchema};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Revision([u8; 8]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid revision; expected 16 lowercase hex digits")]
pub struct InvalidRevision;

impl Revision {
    /// The revision of a whole file's bytes (a job file, `settings.json`, a persona).
    pub fn of(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut prefix = [0; 8];
        prefix.copy_from_slice(&digest[..8]);
        Revision(prefix)
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl FromStr for Revision {
    type Err = InvalidRevision;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !valid {
            return Err(InvalidRevision);
        }
        let mut bytes = [0; 8];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| InvalidRevision)?;
        }
        Ok(Revision(bytes))
    }
}

impl Serialize for Revision {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Revision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl PartialSchema for Revision {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .pattern(Some("^[0-9a-f]{16}$"))
            .description(Some(
                "The first 16 hex digits of SHA-256 over the file's bytes. Send it back as \
                 `If-Match`.",
            ))
            .into()
    }
}

impl ToSchema for Revision {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_the_sha256_prefix() {
        // SHA-256("") = e3b0c44298fc1c14 9afbf4c8…
        assert_eq!(Revision::of(b"").to_string(), "e3b0c44298fc1c14");
        assert_ne!(Revision::of(b"a"), Revision::of(b"b"));
    }

    #[test]
    fn parses_only_its_own_form() {
        let revision = Revision::of(b"schedule: 0 9 * * *\n");
        assert_eq!(revision.to_string().parse(), Ok(revision));
        for s in [
            "",
            "e3b0c44298fc1c1",
            "E3B0C44298FC1C14",
            "e3b0c44298fc1c14a",
            "g3b0c44298fc1c14",
        ] {
            assert_eq!(s.parse::<Revision>(), Err(InvalidRevision), "{s:?}");
        }
    }
}
