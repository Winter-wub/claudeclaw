//! In-process fake for the Agents seam (management-api §2.8).
//!
//! [`FakeAgents`] keeps `agents/` and `agents/.archive/` in memory with the semantics the real
//! backend promises: a taken name is `409 agent_exists`, a missing agent or entry is
//! `404 not_found`, only an archived entry can be purged, and a persona write against a stale
//! revision is `409 revision_mismatch` with `details.current`.

#![cfg(test)]

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};

use claudeclaw_api::agents::{
    AgentArchived, AgentCreate, AgentInfo, AgentKind, AgentList, AgentPersona, AgentPurged,
    ArchiveReason, ArchivedAgent, ArchivedAgentList,
};
use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::revision::Revision;
use claudeclaw_api::scope::Scope;
use claudeclaw_api::scopes::{InheritedFrom, LauncherSource, ScopeLauncher};

use crate::backend::{AgentsBackend, BackendFuture};

/// The stamp the fake gives every archive entry.
const STAMP: &str = "20261014T090000Z";

/// One live agent dir.
#[derive(Clone)]
struct Agent {
    /// `agents/<name>/CLAUDE.md`; `None` when the agent has no persona file.
    persona: Option<String>,
    jobs: u32,
}

/// One entry of `agents/.archive/`.
#[derive(Clone)]
struct Archived {
    name: String,
    agent: Agent,
    reason: ArchiveReason,
}

#[derive(Default)]
struct Dirs {
    agents: BTreeMap<String, Agent>,
    archive: BTreeMap<String, Archived>,
}

/// What a call to the seam was given, in order: `<op> <argument>`.
type Calls = Arc<Mutex<Vec<String>>>;

/// The agents seam over an in-memory `agents/` tree.
#[derive(Clone, Default)]
pub struct FakeAgents {
    dirs: Arc<Mutex<Dirs>>,
    calls: Calls,
}

impl FakeAgents {
    /// A tree with one named agent (`researcher`, a persona and two jobs) and one fired thread
    /// in the archive.
    pub fn seeded() -> Self {
        let fake = FakeAgents::default();
        {
            let mut dirs = fake.dirs.lock().unwrap();
            dirs.agents.insert(
                "researcher".into(),
                Agent {
                    persona: Some(PERSONA.into()),
                    jobs: 2,
                },
            );
            dirs.archive.insert(
                FIRED_ENTRY.into(),
                Archived {
                    name: "paper-digest-5678".into(),
                    agent: Agent {
                        persona: None,
                        jobs: 1,
                    },
                    reason: ArchiveReason::Fired,
                },
            );
        }
        fake
    }

    /// Every call the seam served, as `<op> <argument>`.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// The persona text now on disk, `None` for no agent or no persona file.
    pub fn persona(&self, name: &str) -> Option<String> {
        let dirs = self.dirs.lock().unwrap();
        dirs.agents.get(name)?.persona.clone()
    }

    /// The Revision of the persona now on disk (an absent file reads as empty).
    pub fn persona_revision(&self, name: &str) -> Revision {
        revision_of(self.persona(name).as_deref())
    }

    /// Whether `agents/<name>/` exists.
    pub fn has_agent(&self, name: &str) -> bool {
        self.dirs.lock().unwrap().agents.contains_key(name)
    }

    /// Whether `agents/.archive/<entry>/` exists.
    pub fn has_entry(&self, entry: &str) -> bool {
        self.dirs.lock().unwrap().archive.contains_key(entry)
    }

    /// The persona changes on disk behind the API's back.
    pub fn change_persona_on_disk(&self, name: &str) {
        let mut dirs = self.dirs.lock().unwrap();
        if let Some(agent) = dirs.agents.get_mut(name) {
            agent.persona = Some("# Changed elsewhere\n".into());
        }
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

/// The seeded agent's persona.
pub const PERSONA: &str = "# Researcher\n\nFinds and summarises papers.\n";
/// The seeded fired thread's archive entry.
pub const FIRED_ENTRY: &str = "paper-digest-5678-20261014T090000Z";

fn revision_of(text: Option<&str>) -> Revision {
    Revision::of(text.unwrap_or_default().as_bytes())
}

fn not_found(what: &str) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("no such {what}"))
}

fn agent_exists() -> ApiError {
    ApiError::new(ErrorCode::AgentExists, "an agent with that name exists")
}

fn info_of(name: &str, agent: &Agent) -> AgentInfo {
    AgentInfo {
        name: name.to_owned(),
        kind: AgentKind::Named,
        dir: format!("agents/{name}"),
        thread: None,
        description: agent
            .persona
            .as_deref()
            .and_then(|text| {
                text.lines()
                    .find(|line| !line.is_empty() && !line.starts_with('#'))
            })
            .unwrap_or_default()
            .to_owned(),
        jobs: agent.jobs,
        launcher: ScopeLauncher {
            scope: Scope::Agent(name.to_owned()),
            set_here: None,
            effective: "direct".into(),
            inherited_from: InheritedFrom::Default,
            source: LauncherSource::Default,
        },
        session: None,
    }
}

impl AgentsBackend for FakeAgents {
    fn list(&self) -> BackendFuture<'_, AgentList> {
        self.record("list".into());
        let agents = {
            let dirs = self.dirs.lock().unwrap();
            dirs.agents
                .iter()
                .map(|(name, agent)| info_of(name, agent))
                .collect()
        };
        Box::pin(async move { Ok(AgentList { agents }) })
    }

    fn archived(&self) -> BackendFuture<'_, ArchivedAgentList> {
        self.record("archived".into());
        let archived = {
            let dirs = self.dirs.lock().unwrap();
            dirs.archive
                .iter()
                .map(|(entry, archived)| ArchivedAgent {
                    entry: entry.clone(),
                    name: archived.name.clone(),
                    archived_at: Utc.with_ymd_and_hms(2026, 10, 14, 9, 0, 0).unwrap(),
                    reason: archived.reason,
                })
                .collect()
        };
        Box::pin(async move { Ok(ArchivedAgentList { archived }) })
    }

    fn create(&self, create: AgentCreate) -> BackendFuture<'_, AgentInfo> {
        self.record(format!("create {}", create.name));
        let result = {
            let mut dirs = self.dirs.lock().unwrap();
            match dirs.agents.entry(create.name.clone()) {
                Entry::Occupied(_) => Err(agent_exists()),
                Entry::Vacant(slot) => {
                    let agent = Agent {
                        persona: create.persona,
                        jobs: 0,
                    };
                    let info = info_of(&create.name, &agent);
                    slot.insert(agent);
                    Ok(info)
                }
            }
        };
        Box::pin(async move { result })
    }

    fn persona_get(&self, name: &str) -> BackendFuture<'_, AgentPersona> {
        self.record(format!("persona.get {name}"));
        let result = match self.dirs.lock().unwrap().agents.get(name) {
            None => Err(not_found("agent")),
            Some(agent) => {
                let text = agent.persona.clone().unwrap_or_default();
                Ok(AgentPersona {
                    revision: Revision::of(text.as_bytes()),
                    text,
                })
            }
        };
        Box::pin(async move { result })
    }

    fn persona_put(
        &self,
        name: &str,
        text: String,
        expected: Revision,
    ) -> BackendFuture<'_, AgentPersona> {
        self.record(format!("persona.put {name} {expected}"));
        let result = match self.dirs.lock().unwrap().agents.get_mut(name) {
            None => Err(not_found("agent")),
            Some(agent) => {
                let current = revision_of(agent.persona.as_deref());
                if current != expected {
                    Err(ApiError::new(
                        ErrorCode::RevisionMismatch,
                        "the persona changed on disk since it was read",
                    )
                    .with_detail("current", current.to_string()))
                } else {
                    let revision = Revision::of(text.as_bytes());
                    agent.persona = Some(text.clone());
                    Ok(AgentPersona { text, revision })
                }
            }
        };
        Box::pin(async move { result })
    }

    fn archive(&self, name: &str) -> BackendFuture<'_, AgentArchived> {
        self.record(format!("archive {name}"));
        let result = {
            let mut dirs = self.dirs.lock().unwrap();
            match dirs.agents.remove(name) {
                None => Err(not_found("agent")),
                Some(agent) => {
                    let entry = format!("{name}-{STAMP}");
                    let jobs_unscheduled = agent.jobs;
                    dirs.archive.insert(
                        entry.clone(),
                        Archived {
                            name: name.to_owned(),
                            agent,
                            reason: ArchiveReason::Archived,
                        },
                    );
                    Ok(AgentArchived {
                        archived_to: format!("agents/.archive/{entry}"),
                        entry,
                        jobs_unscheduled,
                    })
                }
            }
        };
        Box::pin(async move { result })
    }

    fn restore(&self, entry: &str) -> BackendFuture<'_, AgentInfo> {
        self.record(format!("restore {entry}"));
        let result = {
            let mut dirs = self.dirs.lock().unwrap();
            match dirs
                .archive
                .get(entry)
                .map(|archived| archived.name.clone())
            {
                None => Err(not_found("archive entry")),
                Some(name) if dirs.agents.contains_key(&name) => Err(agent_exists()),
                Some(name) => {
                    let archived = dirs.archive.remove(entry).expect("looked up above");
                    let info = info_of(&name, &archived.agent);
                    dirs.agents.insert(name, archived.agent);
                    Ok(info)
                }
            }
        };
        Box::pin(async move { result })
    }

    fn purge(&self, entry: &str) -> BackendFuture<'_, AgentPurged> {
        self.record(format!("purge {entry}"));
        let result = match self.dirs.lock().unwrap().archive.remove(entry) {
            None => Err(not_found("archive entry")),
            Some(_) => Ok(AgentPurged { deleted: true }),
        };
        Box::pin(async move { result })
    }
}
