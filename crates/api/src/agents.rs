//! Agents (management-api §2.8): `agents.list`, `agents.archived`, `agents.create`,
//! `agents.persona.get` / `put`, `agents.archive`, `agents.restore` and `agents.purge`. Archiving
//! and Fire move an agent dir to `agents/.archive/`; only an archived entry can be deleted, so
//! nothing is deleted in one step (#16, #21 D12).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::revision::Revision;
use crate::scopes::ScopeLauncher;
use crate::sessions::ScopeSession;
use crate::wire::wire_enum;

/// `agents.list` → `{agents}` (management-api §2.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentList {
    pub agents: Vec<AgentInfo>,
}

/// One agent: a dir under `agents/` (management-api §2.8). Its Scope is `agent:<name>`.
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentInfo {
    /// The dir name: `<name>` for a named agent, `<slug>-<thread id>` for a thread's.
    pub name: String,
    pub kind: AgentKind,
    /// The agent dir, relative to the Daemon dir (`agents/<name>`).
    pub dir: String,
    /// The Discord thread a `thread` agent was hired into; `null` for a `named` one.
    #[schema(required = true)]
    pub thread: Option<AgentThread>,
    /// The persona's first paragraph, cut to 200 chars; empty when there is no persona.
    pub description: String,
    /// The agent's jobs (`agents/<name>/jobs/`).
    pub jobs: u32,
    pub launcher: ScopeLauncher,
    /// The session of the agent's effective Launcher; `null` when there is none.
    #[schema(required = true)]
    pub session: Option<ScopeSession>,
}

impl fmt::Debug for AgentInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentInfo")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("dir", &self.dir)
            .field("thread", &self.thread)
            .field("description_len", &self.description.len())
            .field("jobs", &self.jobs)
            .field("launcher", &self.launcher)
            .field("session", &self.session)
            .finish()
    }
}

wire_enum! {
    /// How an agent came to be (management-api §2.8).
    pub enum AgentKind {
        /// Made by `agents.create` or by hand.
        Named = "named",
        /// Made by Hire for a Discord thread (§2.7).
        Thread = "thread",
    }
}

/// The Discord thread a `thread` agent belongs to (management-api §2.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentThread {
    pub id: String,
    pub channel_id: String,
}

/// `agents.archived` → `{archived}`, fired threads included (management-api §2.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ArchivedAgentList {
    pub archived: Vec<ArchivedAgent>,
}

/// One entry of `agents/.archive/` (management-api §2.8, #16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ArchivedAgent {
    /// The entry's dir name, `<name>-<UTC stamp>`; the `{entry}` of `agents.restore` and
    /// `agents.purge`.
    pub entry: String,
    /// The agent's name before it was archived; `agents.restore` brings it back under it.
    pub name: String,
    pub archived_at: DateTime<Utc>,
    pub reason: ArchiveReason,
}

wire_enum! {
    /// Why an agent was archived (management-api §2.8).
    pub enum ArchiveReason {
        /// Its Discord thread was fired (§2.7).
        Fired = "fired",
        /// `agents.archive`.
        Archived = "archived",
    }
}

/// `agents.create` body; `409 agent_exists` when the name is taken (management-api §2.8).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentCreate {
    pub name: String,
    /// The persona, written to `agents/<name>/CLAUDE.md`. No persona file when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
}

impl fmt::Debug for AgentCreate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentCreate")
            .field("name", &self.name)
            .field("persona_len", &self.persona.as_ref().map(String::len))
            .finish()
    }
}

/// `agents.persona.get` and `agents.persona.put` → the raw `agents/<name>/CLAUDE.md` and its
/// revision (management-api §1.5, §2.8).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentPersona {
    pub text: String,
    /// Send it back as `If-Match` with `agents.persona.put`.
    pub revision: Revision,
}

impl fmt::Debug for AgentPersona {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentPersona")
            .field("text_len", &self.text.len())
            .field("revision", &self.revision)
            .finish()
    }
}

/// `agents.persona.put` body: the whole new persona. `If-Match` is required (management-api
/// §1.5, §2.8).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentPersonaPut {
    pub text: String,
}

impl fmt::Debug for AgentPersonaPut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentPersonaPut")
            .field("text_len", &self.text.len())
            .finish()
    }
}

/// `agents.archive` → where the agent went (management-api §2.8). Its `jobs/` moves with it, so
/// those jobs stop being scheduled; its sessions are kept and resume on restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentArchived {
    /// The new archive entry, as `ArchivedAgent.entry`.
    pub entry: String,
    /// The archived dir, relative to the Daemon dir (`agents/.archive/<entry>`).
    pub archived_to: String,
    /// The agent's jobs that are no longer scheduled.
    pub jobs_unscheduled: u32,
}

/// `agents.purge` → `{deleted}` (management-api §2.8). Only archived entries can be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentPurged {
    pub deleted: bool,
}

/// The schemas of this module, merged by [`crate::openapi`].
#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    AgentList,
    AgentInfo,
    AgentKind,
    AgentThread,
    ArchivedAgentList,
    ArchivedAgent,
    ArchiveReason,
    AgentCreate,
    AgentPersona,
    AgentPersonaPut,
    AgentArchived,
    AgentPurged,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::scope::Scope;
    use crate::scopes::{InheritedFrom, LauncherSource};
    use crate::sessions::SessionStatus;

    fn info() -> AgentInfo {
        AgentInfo {
            name: "paper-digest-5678".into(),
            kind: AgentKind::Thread,
            dir: "agents/paper-digest-5678".into(),
            thread: Some(AgentThread {
                id: "5678".into(),
                channel_id: "1234".into(),
            }),
            description: "Finds and summarises papers.".into(),
            jobs: 2,
            launcher: ScopeLauncher {
                scope: Scope::Agent("paper-digest-5678".into()),
                set_here: None,
                effective: "direct".into(),
                inherited_from: InheritedFrom::Default,
                source: LauncherSource::Default,
            },
            session: Some(ScopeSession {
                launcher: "direct".into(),
                session_id: "s-1".into(),
                status: SessionStatus::InUse,
            }),
        }
    }

    fn info_wire() -> serde_json::Value {
        json!({
            "name": "paper-digest-5678",
            "kind": "thread",
            "dir": "agents/paper-digest-5678",
            "thread": {"id": "5678", "channel_id": "1234"},
            "description": "Finds and summarises papers.",
            "jobs": 2,
            "launcher": {
                "scope": "agent:paper-digest-5678",
                "set_here": null,
                "effective": "direct",
                "inherited_from": "default",
                "source": "default",
            },
            "session": {"launcher": "direct", "session_id": "s-1", "status": "in_use"},
        })
    }

    #[test]
    fn agent_list_shape() {
        let list = AgentList {
            agents: vec![info()],
        };
        let wire = json!({"agents": [info_wire()]});
        assert_eq!(serde_json::to_value(&list).unwrap(), wire);
        assert_eq!(serde_json::from_value::<AgentList>(wire).unwrap(), list);
    }

    #[test]
    fn a_named_agent_sends_its_nulls() {
        let named = AgentInfo {
            name: "researcher".into(),
            kind: AgentKind::Named,
            dir: "agents/researcher".into(),
            thread: None,
            session: None,
            ..info()
        };
        let wire = serde_json::to_value(&named).unwrap();
        assert_eq!(wire["kind"], json!("named"));
        assert_eq!(wire["thread"], json!(null));
        assert_eq!(wire["session"], json!(null));
        assert_eq!(serde_json::from_value::<AgentInfo>(wire).unwrap(), named);
    }

    #[test]
    fn archived_list_shape() {
        let list = ArchivedAgentList {
            archived: vec![ArchivedAgent {
                entry: "paper-digest-5678-20261014T090000Z".into(),
                name: "paper-digest-5678".into(),
                archived_at: Utc.with_ymd_and_hms(2026, 10, 14, 9, 0, 0).unwrap(),
                reason: ArchiveReason::Fired,
            }],
        };
        let wire = json!({"archived": [{
            "entry": "paper-digest-5678-20261014T090000Z",
            "name": "paper-digest-5678",
            "archived_at": "2026-10-14T09:00:00Z",
            "reason": "fired",
        }]});
        assert_eq!(serde_json::to_value(&list).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ArchivedAgentList>(wire).unwrap(),
            list
        );
    }

    #[test]
    fn create_shapes() {
        let bare: AgentCreate = serde_json::from_value(json!({"name": "scribe"})).unwrap();
        assert_eq!(bare.persona, None);
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({"name": "scribe"})
        );

        let create = AgentCreate {
            name: "scribe".into(),
            persona: Some("Keeps notes.".into()),
        };
        let wire = json!({"name": "scribe", "persona": "Keeps notes."});
        assert_eq!(serde_json::to_value(&create).unwrap(), wire);
        assert_eq!(serde_json::from_value::<AgentCreate>(wire).unwrap(), create);
    }

    #[test]
    fn persona_shapes() {
        let text = "# Scribe\n\nKeeps notes.\n";
        let persona = AgentPersona {
            text: text.into(),
            revision: Revision::of(text.as_bytes()),
        };
        let wire = json!({"text": text, "revision": Revision::of(text.as_bytes()).to_string()});
        assert_eq!(serde_json::to_value(&persona).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<AgentPersona>(wire).unwrap(),
            persona
        );
        assert!(
            serde_json::from_value::<AgentPersona>(json!({"text": "", "revision": "nope"}))
                .is_err()
        );

        let put = AgentPersonaPut { text: text.into() };
        let wire = json!({"text": text});
        assert_eq!(serde_json::to_value(&put).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<AgentPersonaPut>(wire).unwrap(),
            put
        );
    }

    #[test]
    fn archive_and_purge_shapes() {
        let archived = AgentArchived {
            entry: "researcher-20261014T090000Z".into(),
            archived_to: "agents/.archive/researcher-20261014T090000Z".into(),
            jobs_unscheduled: 1,
        };
        let wire = json!({
            "entry": "researcher-20261014T090000Z",
            "archived_to": "agents/.archive/researcher-20261014T090000Z",
            "jobs_unscheduled": 1,
        });
        assert_eq!(serde_json::to_value(&archived).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<AgentArchived>(wire).unwrap(),
            archived
        );

        let purged = AgentPurged { deleted: true };
        let wire = json!({"deleted": true});
        assert_eq!(serde_json::to_value(purged).unwrap(), wire);
        assert_eq!(serde_json::from_value::<AgentPurged>(wire).unwrap(), purged);
    }

    #[test]
    fn content_stays_out_of_debug() {
        let persona = AgentPersona {
            text: "a private persona".into(),
            revision: Revision::of(b"a private persona"),
        };
        let debug = format!("{persona:?}");
        assert!(!debug.contains("private"), "{debug}");
        assert!(debug.contains("text_len: 17"), "{debug}");

        let debug = format!(
            "{:?}",
            AgentPersonaPut {
                text: "a private persona".into()
            }
        );
        assert!(!debug.contains("private"), "{debug}");

        let debug = format!(
            "{:?}",
            AgentCreate {
                name: "scribe".into(),
                persona: Some("a private persona".into()),
            }
        );
        assert!(!debug.contains("private"), "{debug}");
        assert!(debug.contains("persona_len: Some(17)"), "{debug}");

        let debug = format!("{:?}", info());
        assert!(!debug.contains("summarises"), "{debug}");
    }

    #[test]
    fn unknown_values_parse() {
        assert_eq!(
            serde_json::from_value::<AgentKind>(json!("imported")).unwrap(),
            AgentKind::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ArchiveReason>(json!("expired")).unwrap(),
            ArchiveReason::Unknown
        );
    }
}
