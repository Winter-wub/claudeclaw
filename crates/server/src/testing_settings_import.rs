//! In-process fakes for the Settings and First-start import seams (management-api §2.6, §2.11).
//!
//! [`FakeSettings`] keeps a small `settings.json` in memory, patches it with RFC 7396 and derives
//! the view from it the way the store does: a secret in the file reads back as `{set}` and never
//! as its value. The key catalogue it enforces is a test fixture standing in for the real one,
//! which lives with the store.

#![cfg(test)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Map, Value, json};

use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::import::{
    ImportApplyRequest, ImportJobs, ImportKind, ImportPlan, ImportRecord, ImportResult, ImportStep,
    ImportStepRecord,
};
use claudeclaw_api::revision::Revision;
use claudeclaw_api::settings::{
    AgenticSettings, AutoCompactSettings, DiscordSettings, DiskSettings, FallbackSettings,
    HeartbeatSettings, InboxSettings, LoggingSettings, RemoteSettings, Restarted, RunLocaleSetting,
    RunPathSettings, RunPathSource, SecretState, SecurityLevel, SecuritySettings, SessionSettings,
    Settings, SettingsApplied, SettingsPatch, SettingsView, TimeoutBreakerSettings,
    TimeoutSettings,
};

use crate::backend::{BackendFuture, ImportBackend, ImportPlanQuery, SettingsBackend};

/// The top-level keys the fake's catalogue knows.
const KNOWN: [&str; 12] = [
    "timezone",
    "model",
    "api",
    "fallback",
    "heartbeat",
    "discord",
    "security",
    "session",
    "logging",
    "remote",
    "inbox",
    "disk",
];

/// A secret already in the fake's file, which no response may ever carry.
pub const SEEDED_TOKEN: &str = "seeded-discord-token-do-not-echo";

/// The `settings.json` the fake starts from: a Discord token, a remote listener and a TS-only
/// `web` table.
fn initial_file() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "model": "opus",
        "discord": {"token": SEEDED_TOKEN, "allowedUserIds": ["1111"]},
        "remote": {"enabled": false, "listen": "0.0.0.0", "port": 4633},
        "web": {"port": 4632},
    }) else {
        unreachable!("an object literal")
    };
    map
}

/// What one `settings.update` was given.
pub type SettingsWrite = (SettingsPatch, Option<Revision>);

/// The settings seam: it holds a file, refuses what the catalogue refuses, and records writes.
#[derive(Clone)]
pub struct FakeSettings {
    file: Arc<Mutex<Map<String, Value>>>,
    get_calls: Arc<Mutex<u32>>,
    update_calls: Arc<Mutex<Vec<SettingsWrite>>>,
}

impl Default for FakeSettings {
    fn default() -> Self {
        FakeSettings {
            file: Arc::new(Mutex::new(initial_file())),
            get_calls: Arc::default(),
            update_calls: Arc::default(),
        }
    }
}

impl FakeSettings {
    /// How many times `settings.get` was served.
    pub fn get_calls(&self) -> u32 {
        *self.get_calls.lock().unwrap()
    }

    /// The patches `settings.update` was given, with the revision their `If-Match` carried.
    pub fn update_calls(&self) -> Vec<SettingsWrite> {
        self.update_calls.lock().unwrap().clone()
    }

    /// The file as it is now, secrets and all: for asserting what was written.
    pub fn file(&self) -> Value {
        Value::Object(self.file.lock().unwrap().clone())
    }

    /// The Revision of the file as it is now.
    pub fn revision(&self) -> Revision {
        revision_of(&self.file.lock().unwrap())
    }

    /// The file changed on disk behind the API's back.
    pub fn change_on_disk(&self) {
        self.file
            .lock()
            .unwrap()
            .insert("model".into(), json!("sonnet"));
    }
}

fn revision_of(file: &Map<String, Value>) -> Revision {
    Revision::of(&serde_json::to_vec(file).unwrap())
}

fn secret_set(file: &Map<String, Value>, path: &[&str]) -> SecretState {
    let mut at = file.get(path[0]);
    for key in &path[1..] {
        at = at.and_then(|value| value.get(*key));
    }
    SecretState {
        set: at.and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
    }
}

/// The view of the file, with every secret as `{set}` (#13).
fn view_of(file: &Map<String, Value>) -> SettingsView {
    let mut settings = sample_settings();
    if let Some(model) = file.get("model").and_then(Value::as_str) {
        settings.model = model.to_owned();
    }
    settings.api = secret_set(file, &["api"]);
    settings.discord.token = secret_set(file, &["discord", "token"]);
    if let Some(port) = file
        .get("remote")
        .and_then(|remote| remote.get("port"))
        .and_then(Value::as_u64)
    {
        settings.remote.port = port as u16;
    }
    let ts_only_keys = match file.get("web") {
        Some(Value::Object(web)) => web.keys().map(|key| format!("web.{key}")).collect(),
        _ => Vec::new(),
    };
    SettingsView {
        revision: revision_of(file),
        settings,
        ts_only_keys,
        env_overrides: vec!["discord.token".into()],
    }
}

/// §2.6's refusals, as the real catalogue makes them. Nothing here echoes a value.
fn classify(patch: &SettingsPatch) -> Result<(), Box<ApiError>> {
    for (key, value) in &patch.0 {
        let ts_only = match key.as_str() {
            "apiToken" | "web" | "watchdog" => Some(key.clone()),
            "runtime" => value
                .as_object()
                .and_then(|runtime| {
                    ["launcher", "launchers"]
                        .into_iter()
                        .find(|name| runtime.contains_key(*name))
                })
                .map(|name| format!("runtime.{name}")),
            _ => None,
        };
        if let Some(path) = ts_only {
            return Err(Box::new(
                ApiError::new(
                    ErrorCode::TsOnlyKey,
                    "this key is read by the TypeScript daemon only",
                )
                .with_detail("key", path),
            ));
        }
        if key == "launcher" {
            return Err(Box::new(
                ApiError::new(ErrorCode::InvalidValue, "a Launcher is not a settings key")
                    .with_hint("set a Launcher with launchers.set (management-api §2.5)")
                    .with_detail("key", "launcher"),
            ));
        }
        if !KNOWN.contains(&key.as_str()) {
            return Err(Box::new(
                ApiError::new(ErrorCode::UnknownKey, "no such settings key")
                    .with_detail("key", key.clone()),
            ));
        }
    }
    Ok(())
}

/// RFC 7396.
fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Value::Object(target) = target else {
        unreachable!("just made an object")
    };
    for (key, value) in patch {
        if value.is_null() {
            target.remove(key);
        } else {
            merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// Dotted paths of the leaves that differ between `old` and `new`.
fn diff(prefix: &str, old: Option<&Value>, new: Option<&Value>, out: &mut Vec<String>) {
    if old == new {
        return;
    }
    if let (Some(Value::Object(old)), Some(Value::Object(new))) = (old, new) {
        let keys: BTreeMap<_, _> = old.keys().chain(new.keys()).map(|k| (k, ())).collect();
        for key in keys.keys() {
            diff(
                &format!("{prefix}{key}."),
                old.get(*key),
                new.get(*key),
                out,
            );
        }
        return;
    }
    out.push(prefix.trim_end_matches('.').to_owned());
}

/// One `settings.update` over the fake's file: the revision check, then the catalogue, then the
/// merge.
fn apply(
    file: &mut Map<String, Value>,
    patch: SettingsPatch,
    expected: Option<Revision>,
) -> Result<SettingsApplied, Box<ApiError>> {
    let current = revision_of(file);
    if let Some(expected) = expected
        && expected != current
    {
        return Err(Box::new(
            ApiError::new(
                ErrorCode::RevisionMismatch,
                "settings.json changed on disk since it was read",
            )
            .with_detail("current", current.to_string()),
        ));
    }
    classify(&patch)?;
    let old = Value::Object(file.clone());
    let mut new = old.clone();
    merge_patch(&mut new, &Value::Object(patch.0.clone()));
    let mut changed_keys = Vec::new();
    diff("", Some(&old), Some(&new), &mut changed_keys);
    let Value::Object(next) = new else {
        unreachable!("a merge patch over an object stays an object")
    };
    *file = next;
    let mut restarted = Vec::new();
    if changed_keys.iter().any(|key| key == "discord.token") {
        restarted.push(Restarted::Discord);
    }
    if changed_keys.iter().any(|key| key.starts_with("remote")) {
        restarted.push(Restarted::RemoteListener);
    }
    Ok(SettingsApplied {
        view: view_of(file),
        changed_keys,
        restarted,
    })
}

impl SettingsBackend for FakeSettings {
    fn get(&self) -> BackendFuture<'_, SettingsView> {
        *self.get_calls.lock().unwrap() += 1;
        let view = view_of(&self.file.lock().unwrap());
        Box::pin(async move { Ok(view) })
    }

    fn update(
        &self,
        patch: SettingsPatch,
        expected: Option<Revision>,
    ) -> BackendFuture<'_, SettingsApplied> {
        self.update_calls
            .lock()
            .unwrap()
            .push((patch.clone(), expected));
        let result = apply(&mut self.file.lock().unwrap(), patch, expected).map_err(|e| *e);
        Box::pin(async move { result })
    }
}

/// The typed view the fake starts from, before the file's own values are laid over it.
pub fn sample_settings() -> Settings {
    Settings {
        timezone: "UTC".into(),
        model: "opus".into(),
        api: SecretState { set: false },
        fallback: FallbackSettings {
            model: "sonnet".into(),
            api: SecretState { set: false },
        },
        agentic: AgenticSettings {
            enabled: false,
            default_mode: "implementation".into(),
            modes: vec![],
        },
        heartbeat: HeartbeatSettings {
            enabled: true,
            interval: 15,
            prompt: "check the inbox".into(),
            exclude_windows: vec![],
        },
        discord: DiscordSettings {
            token: SecretState { set: false },
            allowed_user_ids: vec!["1111".into()],
            listen_channels: vec![],
            listen_guilds: vec![],
            allowed_guilds: vec![],
            channel_allowed_user_ids: BTreeMap::new(),
            channel_names: BTreeMap::new(),
            image_output_roots: vec![],
            streaming: false,
        },
        security: SecuritySettings {
            level: SecurityLevel::Moderate,
            allowed_tools: vec![],
            disallowed_tools: vec![],
        },
        session: SessionSettings {
            auto_rotate: false,
            max_messages: 50,
            max_age_hours: 24,
            summary_path: String::new(),
        },
        session_timeout_ms: 1_800_000,
        timeouts: TimeoutSettings {
            heartbeat: 15,
            job: 30,
            default: 5,
        },
        logging: LoggingSettings {
            level: "info".into(),
            daemon_keep_files: 14,
            daemon_max_daily_mib: 256,
            daemon_max_total_mib: 1024,
            run_log_keep_days: None,
            audit_keep_months: Some(12),
        },
        remote: RemoteSettings {
            enabled: false,
            listen: "0.0.0.0".into(),
            port: 4633,
        },
        run_path: RunPathSettings {
            source: RunPathSource::Auto,
            prepend: vec![],
        },
        run_locale: RunLocaleSetting::Auto,
        timeout_breaker: TimeoutBreakerSettings {
            max_consecutive_timeouts: None,
            max_failing_seconds: None,
        },
        auto_compact: AutoCompactSettings {
            enabled: true,
            timeout_secs: 300,
        },
        transcript_roots: vec![],
        inbox: InboxSettings {
            keep_days: Some(30),
            max_mib: None,
        },
        disk: DiskSettings {
            warn_free_mib: 2048,
            min_free_mib: 256,
        },
    }
}

fn at(hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 14, hour, 0, 0).unwrap()
}

/// The `plan_hash` of the fake's current data.
pub const SAMPLE_PLAN_HASH: &str = "0123456789abcdef";

/// A plan with a `jobs` section only, which is enough to see what the routes pass through.
pub fn sample_plan(plan_hash: &str) -> ImportPlan {
    ImportPlan {
        plan_hash: plan_hash.to_owned(),
        errors: 0,
        warnings: 0,
        launchers: None,
        token: None,
        settings: None,
        state: None,
        jobs: Some(ImportJobs { files: vec![] }),
        sessions: None,
        claude_code: None,
        notes: vec![],
    }
}

/// An imported 1.x data dir, one step done.
pub fn sample_record() -> ImportRecord {
    ImportRecord {
        kind: ImportKind::Imported,
        version: "2.0.0".into(),
        steps: vec![ImportStepRecord {
            step: ImportStep::Backup,
            done_at: Some(at(9)),
            result: Some("2 files".into()),
        }],
        report: Some("import-report-20261014T090000Z.md".into()),
        backup_dir: Some("import-backup/20261014T090000Z".into()),
        plan: None,
    }
}

/// The import seam: it serves the samples, records what the routes asked for, and refuses a
/// `plan_hash` that is not the current one the way the planner does (§2.11).
#[derive(Clone)]
pub struct FakeImport {
    hash: Arc<Mutex<String>>,
    get_calls: Arc<Mutex<u32>>,
    plan_calls: Arc<Mutex<Vec<ImportPlanQuery>>>,
    apply_calls: Arc<Mutex<Vec<ImportApplyRequest>>>,
}

impl Default for FakeImport {
    fn default() -> Self {
        FakeImport {
            hash: Arc::new(Mutex::new(SAMPLE_PLAN_HASH.to_owned())),
            get_calls: Arc::default(),
            plan_calls: Arc::default(),
            apply_calls: Arc::default(),
        }
    }
}

impl FakeImport {
    pub fn get_calls(&self) -> u32 {
        *self.get_calls.lock().unwrap()
    }

    /// The queries `import.plan` served.
    pub fn plan_calls(&self) -> Vec<ImportPlanQuery> {
        self.plan_calls.lock().unwrap().clone()
    }

    /// The requests `import.apply` was given, including the ones it refused.
    pub fn apply_calls(&self) -> Vec<ImportApplyRequest> {
        self.apply_calls.lock().unwrap().clone()
    }

    /// The data changed after a preview: the plan now hashes to something else.
    pub fn change_data(&self) {
        *self.hash.lock().unwrap() = "fedcba9876543210".to_owned();
    }
}

impl ImportBackend for FakeImport {
    fn get(&self) -> BackendFuture<'_, ImportRecord> {
        *self.get_calls.lock().unwrap() += 1;
        Box::pin(async { Ok(sample_record()) })
    }

    fn plan(&self, query: ImportPlanQuery) -> BackendFuture<'_, ImportPlan> {
        self.plan_calls.lock().unwrap().push(query);
        let plan = sample_plan(&self.hash.lock().unwrap());
        Box::pin(async move { Ok(plan) })
    }

    fn apply(&self, request: ImportApplyRequest) -> BackendFuture<'_, ImportResult> {
        self.apply_calls.lock().unwrap().push(request.clone());
        let current = self.hash.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(sent) = &request.plan_hash
                && *sent != current
            {
                return Err(ApiError::new(
                    ErrorCode::PlanChanged,
                    "the data changed since the plan was made",
                )
                .with_hint("preview the import again with import.plan"));
            }
            Ok(ImportResult {
                record: sample_record(),
                report: "import-report-20261014T090000Z.md".into(),
                backup_dir: None,
            })
        })
    }
}
