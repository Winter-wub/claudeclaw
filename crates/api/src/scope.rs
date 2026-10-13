//! The Scope wire form (management-api §1.7): `global`, `discord:<id>` (a channel or a
//! thread), `job:<name>` and `agent:<name>`. Percent-encoded when it is a path segment.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{PartialSchema, ToSchema};

/// A Scope as clients name it. Parsing checks only the shape; whether the Scope exists is the
/// server's question.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Global,
    /// A Discord channel or thread, by snowflake.
    Discord(String),
    /// A job: the file stem, or `<agent>/<stem>` for an agent's job.
    Job(String),
    Agent(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid scope; expected global, discord:<id>, job:<name> or agent:<name>")]
pub struct InvalidScope;

impl FromStr for Scope {
    type Err = InvalidScope;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "global" {
            return Ok(Scope::Global);
        }
        let (kind, name) = s.split_once(':').ok_or(InvalidScope)?;
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(InvalidScope);
        }
        match kind {
            "discord" if name.bytes().all(|b| b.is_ascii_digit()) => {
                Ok(Scope::Discord(name.to_owned()))
            }
            "job" if valid_job_name(name) => Ok(Scope::Job(name.to_owned())),
            "agent" if !name.contains('/') => Ok(Scope::Agent(name.to_owned())),
            _ => Err(InvalidScope),
        }
    }
}

/// `<stem>` or `<agent>/<stem>`, with no empty segment.
fn valid_job_name(name: &str) -> bool {
    match name.split_once('/') {
        None => true,
        Some((agent, stem)) => !agent.is_empty() && !stem.is_empty() && !stem.contains('/'),
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Global => f.write_str("global"),
            Scope::Discord(id) => write!(f, "discord:{id}"),
            Scope::Job(name) => write!(f, "job:{name}"),
            Scope::Agent(name) => write!(f, "agent:{name}"),
        }
    }
}

impl Serialize for Scope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Scope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl PartialSchema for Scope {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .pattern(Some(
                r"^(global|discord:[0-9]+|job:[^/]+(/[^/]+)?|agent:[^/]+)$",
            ))
            .description(Some(
                "A Scope: `global`, `discord:<id>` (channel or thread), `job:<name>` \
                 (`<agent>/<stem>` for an agent's job) or `agent:<name>`.",
            ))
            .examples(["global", "discord:1234", "job:researcher/paper-digest"])
            .into()
    }
}

impl ToSchema for Scope {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_form_round_trips() {
        for s in [
            "global",
            "discord:1234",
            "job:daily-report",
            "job:researcher/paper-digest",
            "agent:researcher",
        ] {
            let scope: Scope = s.parse().unwrap();
            assert_eq!(scope.to_string(), s);
            assert_eq!(serde_json::to_value(&scope).unwrap(), s);
        }
    }

    #[test]
    fn malformed_scopes_are_refused() {
        for s in [
            "",
            "Global",
            "global:x",
            "discord:",
            "discord:12ab",
            "job:",
            "job:/stem",
            "job:agent/",
            "job:a/b/c",
            "agent:a/b",
            "agent:bad\nname",
            "telegram:1",
        ] {
            assert_eq!(s.parse::<Scope>(), Err(InvalidScope), "{s:?}");
        }
        assert!(serde_json::from_str::<Scope>(r#""job:""#).is_err());
    }
}
