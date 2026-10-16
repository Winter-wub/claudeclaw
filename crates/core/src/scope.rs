//! Scopes and the Launcher resolution chain (architecture §4 `claudeclaw-core`, §6 "Scope keys",
//! §7; #8, #17).
//!
//! A [`Scope`] is where work happens: the Global scope, a chat channel, a thread under a channel,
//! a Job, or an Agent. Chat Scopes keep their [`Transport`], so a Telegram thread and a Discord
//! thread never collide (architecture §6).
//!
//! Three keys come off a Scope, for three different readers:
//!
//! - [`Scope::to_api`] / [`Scope::from_api`]: the management API's wire form
//!   (management-api §1.7). Only Discord is representable today, and a thread collapses onto its
//!   own snowflake.
//! - [`Scope::ts_key`]: the TS session file a rolled-back TS daemon reads (§6, #14 rule 7). It is
//!   the mapping `store::rollback::session_mirror` uses.
//! - [`Scope::launcher_keys`]: the keys the Launcher chain looks up in `runtime.launchers`, from
//!   the Scope itself to its parent channel (§7). It matches TS's `getRuntimeLauncher(scopeChain)`:
//!   a thread passes `[threadId, parentChannelId]`.
//!
//! [`LauncherSettings`] is a pure view of where launchers are set: the Rust `launcher` key (§7:
//! `{ "default": "direct" | "<abs path>", "scopes": { "<ts scope key>": … } }`) and, kept as a
//! fallback until the First-start import rewrites them (#17), the 1.x `runtime.launcher` /
//! `runtime.launchers` keys. The Rust key wins where both name the same Scope. A `ccs <profile>`
//! entry is not a [`Launcher`] — it has no path — so it is skipped; the import turns it into a
//! generated script (#17 §2).
//!
//! [`LauncherSettings::resolve`] is the chain (#8): a job's frontmatter `launcher:` first, then
//! the Scope's own key, then a thread's parent channel, then the default, then `direct`.

use std::collections::{BTreeMap, BTreeSet};

use claudeclaw_api::scope::Scope as ApiScope;
use claudeclaw_chat::{ChatScope, LauncherSource, Transport};
use claudeclaw_runner::launcher::{AbsPath, Launcher};
use claudeclaw_store::{Layout, SettingsFile, StoreError, TsKey};
use serde_json::{Map, Value};

/// Where work happens (architecture §4). `Debug`, ordering and hashing are over the whole value,
/// which holds IDs and names only — never a message body.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// The Global scope: direct messages and anything with no channel of its own.
    Global,
    /// A chat channel.
    Channel {
        transport: Transport,
        /// The transport's own channel id.
        id: String,
    },
    /// A thread under a channel. The parent channel is kept so the Launcher chain can inherit it
    /// (§4, §7).
    Thread {
        transport: Transport,
        /// The transport's own thread id.
        id: String,
        /// The transport's own id of the channel the thread is under.
        parent: String,
    },
    /// A job: the file stem, or `<agent>/<stem>` for one of an agent's jobs.
    Job(String),
    /// An agent.
    Agent(String),
}

impl Scope {
    /// The transport a chat Scope came through; `None` for Global, Job and Agent.
    pub fn transport(&self) -> Option<Transport> {
        match self {
            Scope::Channel { transport, .. } | Scope::Thread { transport, .. } => Some(*transport),
            Scope::Global | Scope::Job(_) | Scope::Agent(_) => None,
        }
    }

    /// The TS session key this Scope mirrors to (§6 "Scope keys", #14 rule 7).
    pub fn ts_key(&self) -> TsKey {
        match self {
            Scope::Global => TsKey::Global,
            Scope::Channel { transport, id } | Scope::Thread { transport, id, .. } => {
                TsKey::Thread(chat_key(*transport, id))
            }
            Scope::Job(name) => TsKey::Thread(job_key(name)),
            Scope::Agent(name) => TsKey::Agent(name.clone()),
        }
    }

    /// The keys the Launcher chain looks up, most specific first (§7). Empty for Global, whose
    /// Launcher is the default. A thread names itself and then its parent channel.
    pub fn launcher_keys(&self) -> Vec<String> {
        match self {
            Scope::Global => Vec::new(),
            Scope::Channel { transport, id } => vec![chat_key(*transport, id)],
            Scope::Thread {
                transport,
                id,
                parent,
            } => vec![chat_key(*transport, id), chat_key(*transport, parent)],
            Scope::Job(name) => vec![job_key(name)],
            Scope::Agent(name) => vec![format!("agent:{name}")],
        }
    }

    /// The management API wire form (management-api §1.7). `Err` when the Scope's transport has no
    /// form: today's wire names only Discord.
    pub fn to_api(&self) -> Result<ApiScope, NoWireForm> {
        match self {
            Scope::Global => Ok(ApiScope::Global),
            Scope::Channel { transport, id } | Scope::Thread { transport, id, .. } => {
                match transport {
                    Transport::Discord => Ok(ApiScope::Discord(id.clone())),
                    other => Err(NoWireForm(*other)),
                }
            }
            Scope::Job(name) => Ok(ApiScope::Job(name.clone())),
            Scope::Agent(name) => Ok(ApiScope::Agent(name.clone())),
        }
    }

    /// A Scope from its API wire form. The wire cannot tell a channel from a thread, so
    /// `discord:<id>` is read as a channel (management-api §1.7).
    pub fn from_api(scope: &ApiScope) -> Scope {
        match scope {
            ApiScope::Global => Scope::Global,
            ApiScope::Discord(id) => Scope::Channel {
                transport: Transport::Discord,
                id: id.clone(),
            },
            ApiScope::Job(name) => Scope::Job(name.clone()),
            ApiScope::Agent(name) => Scope::Agent(name.clone()),
        }
    }

    /// A Scope from where a chat message came (architecture §6). A direct message is the Global
    /// Scope, TS parity.
    pub fn from_chat(scope: &ChatScope) -> Scope {
        match scope {
            ChatScope::Direct { .. } => Scope::Global,
            ChatScope::Channel { transport, id } => Scope::Channel {
                transport: *transport,
                id: id.clone(),
            },
            ChatScope::Thread {
                transport,
                id,
                parent,
            } => Scope::Thread {
                transport: *transport,
                id: id.clone(),
                parent: parent.clone(),
            },
        }
    }
}

impl From<&ChatScope> for Scope {
    fn from(scope: &ChatScope) -> Self {
        Scope::from_chat(scope)
    }
}

impl From<ChatScope> for Scope {
    fn from(scope: ChatScope) -> Self {
        Scope::from_chat(&scope)
    }
}

impl From<&ApiScope> for Scope {
    fn from(scope: &ApiScope) -> Self {
        Scope::from_api(scope)
    }
}

impl TryFrom<&Scope> for ApiScope {
    type Error = NoWireForm;

    fn try_from(scope: &Scope) -> Result<Self, Self::Error> {
        scope.to_api()
    }
}

/// A Scope whose transport has no management-API wire form (management-api §1.7): the wire names
/// only `global`, `discord:<id>`, `job:<name>` and `agent:<name>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a {0} Scope has no API wire form; only Discord is representable")]
pub struct NoWireForm(pub Transport);

/// The `runtime.launchers` / session key for a chat channel or thread (§6): a Discord snowflake is
/// bare, Telegram and Slack keep their TS prefix.
fn chat_key(transport: Transport, id: &str) -> String {
    match transport {
        Transport::Discord => id.to_owned(),
        Transport::Telegram => format!("tg:{id}"),
        Transport::Slack => format!("slk:{id}"),
    }
}

/// The key for a job: its bare name, or `agent:<agent>` for one of an agent's jobs (TS runs every
/// job of one agent under that one key, §6).
fn job_key(name: &str) -> String {
    match name.split_once('/') {
        Some((agent, _stem)) => format!("agent:{agent}"),
        None => name.to_owned(),
    }
}

/// The launchers `settings.json` names, as a pure view (§7). Built from a raw settings object with
/// [`LauncherSettings::from_raw`], or read from disk with [`LauncherSettings::load`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LauncherSettings {
    default: Option<Launcher>,
    scopes: BTreeMap<String, Launcher>,
}

impl LauncherSettings {
    /// Reads the Rust `launcher` key and, wherever it is silent, the 1.x `runtime.launcher` /
    /// `runtime.launchers` keys (§7, #17). An unparseable value is ignored, never a panic.
    pub fn from_raw(raw: &Map<String, Value>) -> Self {
        let mut settings = LauncherSettings::default();
        if let Some(Value::Object(launcher)) = raw.get("launcher") {
            if let Some(value) = launcher.get("default").and_then(Value::as_str) {
                settings.default = Launcher::parse(value).ok();
            }
            if let Some(Value::Object(scopes)) = launcher.get("scopes") {
                for (key, value) in scopes {
                    if let Some(launcher) = value.as_str().and_then(|v| Launcher::parse(v).ok()) {
                        settings.scopes.insert(key.clone(), launcher);
                    }
                }
            }
        }
        if let Some(Value::Object(runtime)) = raw.get("runtime") {
            if settings.default.is_none() {
                settings.default = runtime.get("launcher").and_then(ts_launcher);
            }
            if let Some(Value::Object(scopes)) = runtime.get("launchers") {
                for (key, value) in scopes {
                    if settings.scopes.contains_key(key) {
                        continue;
                    }
                    if let Some(launcher) = ts_launcher(value) {
                        settings.scopes.insert(key.clone(), launcher);
                    }
                }
            }
        }
        settings
    }

    /// Reads `settings.json` and builds the view. The synchronous store read runs on
    /// `spawn_blocking` (architecture §2).
    pub async fn load(layout: &Layout) -> Result<Self, LauncherSettingsError> {
        let path = layout.settings();
        let raw = tokio::task::spawn_blocking(move || {
            SettingsFile::read(&path).map(|file| file.raw().clone())
        })
        .await??;
        Ok(Self::from_raw(&raw))
    }

    /// The default Launcher: the `launcher.default` value, or `direct`.
    pub fn default_launcher(&self) -> Launcher {
        self.default.clone().unwrap_or(Launcher::Direct)
    }

    /// The Launcher a Scope key is set to, if any; the key is a [`Scope::launcher_keys`] value.
    pub fn get(&self, key: &str) -> Option<&Launcher> {
        self.scopes.get(key)
    }

    /// The effective Launcher for `scope`: a job's `job_launcher` frontmatter first, then the
    /// Scope's own key, then a thread's parent channel, then the default (#8).
    pub fn resolve(&self, scope: &Scope, job_launcher: Option<&str>) -> Launcher {
        self.resolve_with_source(scope, job_launcher).0
    }

    /// Like [`resolve`](Self::resolve), and where the value came from, for a
    /// [`LauncherView`](claudeclaw_chat::LauncherView).
    pub fn resolve_with_source(
        &self,
        scope: &Scope,
        job_launcher: Option<&str>,
    ) -> (Launcher, LauncherSource) {
        if let Some(value) = job_launcher
            && let Ok(launcher) = Launcher::parse(value)
        {
            return (launcher, LauncherSource::Scope);
        }
        for (index, key) in scope.launcher_keys().iter().enumerate() {
            if let Some(launcher) = self.scopes.get(key) {
                let source = if index == 0 {
                    LauncherSource::Scope
                } else {
                    LauncherSource::ParentChannel
                };
                return (launcher.clone(), source);
            }
        }
        (self.default_launcher(), LauncherSource::Global)
    }

    /// The Launcher picker (#8): `direct` first, then every distinct path in use, in path order.
    pub fn picker(&self) -> Vec<Launcher> {
        let mut paths: BTreeSet<&AbsPath> = BTreeSet::new();
        for launcher in self.default.iter().chain(self.scopes.values()) {
            if let Launcher::Path(path) = launcher {
                paths.insert(path);
            }
        }
        let mut picker = vec![Launcher::Direct];
        picker.extend(paths.into_iter().cloned().map(Launcher::Path));
        picker
    }
}

/// The 1.x `runtime.launcher` / `runtime.launchers` entry as a [`Launcher`]: `direct` when `kind`
/// is `direct` or absent. A `ccs <profile>` entry has no path, so it is `None` until the import
/// generates its script (#17 §2).
fn ts_launcher(value: &Value) -> Option<Launcher> {
    match value.as_object()?.get("kind").and_then(Value::as_str) {
        None | Some("direct") => Some(Launcher::Direct),
        Some(_) => None,
    }
}

/// Why [`LauncherSettings::load`] failed.
#[derive(Debug, thiserror::Error)]
pub enum LauncherSettingsError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the settings read did not finish")]
    Join(#[from] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::scope::Scope as ApiScope;
    use claudeclaw_chat::{ChatScope, Transport};
    use claudeclaw_store::TsKey;
    use serde_json::{Value, json};

    use super::*;

    fn raw(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected an object, got {other}"),
        }
    }

    fn view(value: Value) -> LauncherSettings {
        LauncherSettings::from_raw(&raw(value))
    }

    fn launcher(value: &str) -> Launcher {
        Launcher::parse(value).expect("a valid launcher")
    }

    fn channel(id: &str) -> Scope {
        Scope::Channel {
            transport: Transport::Discord,
            id: id.into(),
        }
    }

    fn thread(id: &str, parent: &str) -> Scope {
        Scope::Thread {
            transport: Transport::Discord,
            id: id.into(),
            parent: parent.into(),
        }
    }

    #[test]
    fn ts_session_keys_follow_the_mirror_table() {
        assert_eq!(Scope::Global.ts_key(), TsKey::Global);
        assert_eq!(channel("123").ts_key(), TsKey::Thread("123".into()));
        assert_eq!(thread("9", "7").ts_key(), TsKey::Thread("9".into()));
        assert_eq!(
            Scope::Channel {
                transport: Transport::Telegram,
                id: "-100:7".into(),
            }
            .ts_key(),
            TsKey::Thread("tg:-100:7".into())
        );
        assert_eq!(
            Scope::Channel {
                transport: Transport::Slack,
                id: "C1".into(),
            }
            .ts_key(),
            TsKey::Thread("slk:C1".into())
        );
        assert_eq!(
            Scope::Job("daily-report".into()).ts_key(),
            TsKey::Thread("daily-report".into())
        );
        assert_eq!(
            Scope::Job("ops/digest".into()).ts_key(),
            TsKey::Thread("agent:ops".into())
        );
        assert_eq!(
            Scope::Agent("ops".into()).ts_key(),
            TsKey::Agent("ops".into())
        );
    }

    #[test]
    fn chat_scopes_map_in_with_direct_as_global() {
        assert_eq!(
            Scope::from_chat(&ChatScope::Direct {
                transport: Transport::Discord,
            }),
            Scope::Global
        );
        assert_eq!(
            Scope::from_chat(&ChatScope::Channel {
                transport: Transport::Telegram,
                id: "12".into(),
            }),
            Scope::Channel {
                transport: Transport::Telegram,
                id: "12".into(),
            }
        );
        assert_eq!(
            Scope::from_chat(&ChatScope::Thread {
                transport: Transport::Slack,
                id: "34".into(),
                parent: "12".into(),
            }),
            Scope::Thread {
                transport: Transport::Slack,
                id: "34".into(),
                parent: "12".into(),
            }
        );
        // The blanket conversions agree with the named constructors.
        let chat = ChatScope::Direct {
            transport: Transport::Discord,
        };
        assert_eq!(Scope::from(&chat), Scope::Global);
        assert_eq!(Scope::from(chat), Scope::Global);
    }

    #[test]
    fn api_scopes_round_trip_through_core() {
        for wire in [
            "global",
            "discord:123",
            "job:daily-report",
            "job:researcher/paper-digest",
            "agent:researcher",
        ] {
            let api: ApiScope = wire.parse().expect("a valid wire scope");
            let scope = Scope::from_api(&api);
            assert_eq!(scope.to_api().expect("representable"), api, "{wire}");
            assert_eq!(scope, Scope::from(&api));
            assert_eq!(ApiScope::try_from(&scope).unwrap(), api);
        }
    }

    #[test]
    fn non_discord_scopes_have_no_wire_form() {
        let scope = Scope::Channel {
            transport: Transport::Telegram,
            id: "1".into(),
        };
        assert_eq!(scope.to_api(), Err(NoWireForm(Transport::Telegram)));
        assert_eq!(
            ApiScope::try_from(&scope),
            Err(NoWireForm(Transport::Telegram))
        );
    }

    #[test]
    fn launcher_keys_run_from_the_scope_to_its_parent() {
        assert!(Scope::Global.launcher_keys().is_empty());
        assert_eq!(channel("123").launcher_keys(), ["123"]);
        assert_eq!(thread("9", "7").launcher_keys(), ["9", "7"]);
        assert_eq!(
            Scope::Thread {
                transport: Transport::Telegram,
                id: "9".into(),
                parent: "7".into(),
            }
            .launcher_keys(),
            ["tg:9", "tg:7"]
        );
        assert_eq!(
            Scope::Job("ops/digest".into()).launcher_keys(),
            ["agent:ops"]
        );
        assert_eq!(Scope::Agent("ops".into()).launcher_keys(), ["agent:ops"]);
    }

    #[test]
    fn the_chain_resolves_every_branch() {
        let settings = view(json!({
            "launcher": {
                "default": "/opt/tools/claude",
                "scopes": {
                    "123": "/opt/tools/ccs",
                    "agent:ops": "/opt/tools/agent",
                },
            },
        }));

        // The frontmatter override wins, even over the Scope's own key.
        assert_eq!(
            settings.resolve(&channel("123"), Some("direct")),
            Launcher::Direct
        );
        // The Scope's own key.
        assert_eq!(
            settings.resolve_with_source(&channel("123"), None),
            (launcher("/opt/tools/ccs"), LauncherSource::Scope)
        );
        assert_eq!(
            settings.resolve_with_source(&thread("9", "123"), None),
            (launcher("/opt/tools/ccs"), LauncherSource::ParentChannel)
        );
        assert_eq!(
            settings.resolve_with_source(&thread("9", "7"), None),
            (launcher("/opt/tools/claude"), LauncherSource::Global)
        );
        // A thread's own key beats its parent's.
        assert_eq!(
            settings.resolve_with_source(&thread("123", "7"), None),
            (launcher("/opt/tools/ccs"), LauncherSource::Scope)
        );
        // An agent's job inherits the agent's entry.
        assert_eq!(
            settings.resolve(&Scope::Job("ops/digest".into()), None),
            launcher("/opt/tools/agent")
        );
        assert_eq!(
            settings.resolve(&Scope::Agent("ops".into()), None),
            launcher("/opt/tools/agent")
        );
        // Global is the default; an unset Scope too.
        assert_eq!(
            settings.resolve(&Scope::Global, None),
            launcher("/opt/tools/claude")
        );
        assert_eq!(
            settings.resolve(&channel("456"), None),
            launcher("/opt/tools/claude")
        );
        // A job frontmatter that isn't a Launcher is ignored, falling through the chain.
        assert_eq!(
            settings.resolve(&channel("123"), Some("relative/path")),
            launcher("/opt/tools/ccs")
        );
    }

    #[test]
    fn no_settings_means_direct() {
        let settings = LauncherSettings::default();
        assert_eq!(settings.default_launcher(), Launcher::Direct);
        assert_eq!(settings.resolve(&channel("1"), None), Launcher::Direct);
        assert_eq!(
            settings.resolve(&Scope::Job("j".into()), Some("not/absolute")),
            Launcher::Direct
        );
    }

    #[test]
    fn the_picker_is_direct_then_distinct_paths_in_order() {
        let settings = view(json!({
            "launcher": {
                "default": "/opt/b",
                "scopes": {
                    "1": "/opt/a",
                    "2": "/opt/b",
                    "3": "/opt/a",
                    "4": "relative",
                },
            },
        }));
        assert_eq!(
            settings.picker(),
            [Launcher::Direct, launcher("/opt/a"), launcher("/opt/b")]
        );
    }

    #[test]
    fn ts_runtime_launchers_are_a_fallback() {
        // `kind: direct` and a missing `kind` read as `direct`; a `ccs` entry has no path yet.
        let settings = view(json!({
            "runtime": {
                "launcher": {"kind": "direct"},
                "launchers": {
                    "123": {"kind": "direct"},
                    "7": {"kind": "ccs", "profile": "work"},
                    "8": {},
                },
            },
        }));
        assert_eq!(settings.default_launcher(), Launcher::Direct);
        assert_eq!(settings.get("123"), Some(&Launcher::Direct));
        assert_eq!(settings.get("7"), None);
        assert_eq!(settings.get("8"), Some(&Launcher::Direct));

        // The Rust key wins where both name a value.
        let settings = view(json!({
            "launcher": {"default": "/opt/rust", "scopes": {"123": "/opt/rust-scope"}},
            "runtime": {
                "launcher": {"kind": "direct"},
                "launchers": {"123": {"kind": "direct"}, "scopes": {"1": {}}},
            },
        }));
        assert_eq!(settings.default_launcher(), launcher("/opt/rust"));
        assert_eq!(settings.get("123"), Some(&launcher("/opt/rust-scope")));
    }

    #[tokio::test]
    async fn load_reads_settings_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        tokio::fs::create_dir_all(layout.root()).await.unwrap();
        tokio::fs::write(
            layout.settings(),
            r#"{"launcher":{"default":"/opt/tools/claude","scopes":{"123":"/opt/tools/ccs"}}}"#,
        )
        .await
        .unwrap();

        let settings = LauncherSettings::load(&layout).await.unwrap();
        assert_eq!(settings.default_launcher(), launcher("/opt/tools/claude"));
        assert_eq!(
            settings.resolve(&channel("123"), None),
            launcher("/opt/tools/ccs")
        );
    }
}
