//! Read-only collection (#28 D5.2): what the export shows, and every identifier it must hide.

use std::fs;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use claudeclaw_api::export::{FieldClass, PseudonymKind, field_class};
use claudeclaw_api::runs::RunRecord;
use claudeclaw_api::scope::Scope;
use claudeclaw_api::supervisor::SupervisorStatus;
use secrecy::ExposeSecret;
use serde_json::{Map, Value};

use super::scrub::{FIXED_RUN_LOG_NAMES, Known};
use super::{ExportError, ExportInput};
use crate::layout::Layout;
use claudeclaw_api::logs::LogEntry;

use crate::logs::Positioned;
use crate::settings::SettingsFile;

/// `settings.json` key paths holding secrets, shown as `{"set": bool}` (#28 D1.4) and redacted
/// wherever their values appear (#28 D3). TS-only ones included (`apiToken`, `web.token`,
/// `telegram.token`, `slack.*Token`).
pub(crate) const SECRET_PATHS: &[(&str, &str)] = &[
    ("api", "api-key"),
    ("fallback.api", "api-key"),
    ("discord.token", "discord-token"),
    ("apiToken", "legacy-api-token"),
    ("web.token", "web-token"),
    ("telegram.token", "telegram-token"),
    ("slack.botToken", "slack-token"),
    ("slack.appToken", "slack-token"),
];

/// What `settings.json` held.
pub(crate) enum SettingsState {
    Missing,
    /// It exists but isn't a JSON object; the section says so.
    Unreadable,
    Read(Map<String, Value>),
}

/// What `supervisor.json` held.
pub(crate) enum CrashState {
    Missing,
    Unreadable,
    Read(Box<SupervisorStatus>),
}

pub(crate) fn settings(
    layout: &Layout,
) -> Result<(SettingsState, Option<SettingsFile>), ExportError> {
    match SettingsFile::load(layout) {
        Ok(file) if !file.exists() => Ok((SettingsState::Missing, None)),
        Ok(file) => Ok((SettingsState::Read(file.raw().clone()), Some(file))),
        Err(crate::StoreError::Json { .. } | crate::StoreError::NotAnObject { .. }) => {
            Ok((SettingsState::Unreadable, None))
        }
        Err(e) => Err(ExportError::Store(e)),
    }
}

/// `daemon/state/supervisor.json`, or the live status's copy when the file is missing.
pub(crate) fn crash(layout: &Layout, input: &ExportInput) -> Result<CrashState, ExportError> {
    let path = layout.supervisor();
    match read_optional(&path)? {
        Some(bytes) => Ok(match serde_json::from_slice(&bytes) {
            Ok(status) => CrashState::Read(Box::new(status)),
            Err(_) => CrashState::Unreadable,
        }),
        None => Ok(
            match input.status.as_ref().and_then(|s| s.supervisor.clone()) {
                Some(status) => CrashState::Read(Box::new(status)),
                None => CrashState::Missing,
            },
        ),
    }
}

/// The Run index records that started in `[since, until)`, oldest first, and how many lines
/// didn't parse. A minimal private reader of `daemon/runs/<YYYY-MM>.jsonl` (#23 D4).
pub(crate) fn runs(
    layout: &Layout,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<(Vec<RunRecord>, u32), ExportError> {
    let mut records = Vec::new();
    let mut unparsed = 0;
    let first = NaiveDate::from_ymd_opt(since.year(), since.month(), 1).expect("valid month");
    let mut month = first;
    while month <= until.date_naive() {
        let path = layout.run_index(&month.format("%Y-%m").to_string());
        if let Some(bytes) = read_optional(&path)? {
            for line in String::from_utf8_lossy(&bytes).lines() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<RunRecord>(line) {
                    Ok(r) if r.started_at >= since && r.started_at < until => records.push(r),
                    Ok(_) => {}
                    Err(_) => unparsed += 1,
                }
            }
        }
        month = month + Months::new(1);
    }
    records.sort_by_key(|r| r.started_at);
    Ok((records, unparsed))
}

/// Every identifier the export must hide (#28 D5.2), read-only.
pub(crate) fn known(
    layout: &Layout,
    input: &ExportInput,
    settings: Option<&SettingsFile>,
    runs: &[RunRecord],
    lines: &[Positioned],
    crash: &CrashState,
) -> Result<Known, ExportError> {
    let mut known = Known::default();
    known.set_project_dir(&layout.project_dir().to_string_lossy());

    // Host facts.
    if let Some(home) = &input.host.home_dir {
        known.set_home_dir(&home.to_string_lossy());
    }
    if let Some(host) = &input.host.hostname {
        known.add(PseudonymKind::Host, host);
    }
    if let Some(user) = &input.host.os_user {
        known.add(PseudonymKind::OsUser, user);
    }

    // Secrets.
    if let Some(file) = settings {
        let secrets = file.view().secrets;
        for (secret, label) in [
            (&secrets.api, "api-key"),
            (&secrets.fallback_api, "api-key"),
            (&secrets.discord_token, "discord-token"),
        ] {
            if let Some(secret) = secret {
                known.add_secret(secret.expose_secret(), label);
            }
        }
        for (path, label) in SECRET_PATHS {
            if let Some(Value::String(s)) = lookup(file.raw(), path) {
                known.add_secret(s, label);
            }
        }
        settings_identifiers(&mut known, file.raw(), "");
    }
    if let Some(bytes) = read_optional(&layout.ts_web_token())? {
        known.add_secret(String::from_utf8_lossy(&bytes).trim(), "web-token");
    }
    for (secret, label) in &input.secrets {
        known.add_secret(secret.expose_secret(), label.as_str());
    }

    // Job files and agents.
    for stem in md_stems(&layout.jobs_dir())? {
        known.add(PseudonymKind::Job, &stem);
    }
    for agent in subdirs(&layout.agents_dir())? {
        known.add(PseudonymKind::Agent, &agent);
        for stem in md_stems(&layout.agent_jobs_dir(&agent))? {
            known.add(PseudonymKind::Job, &format!("{agent}/{stem}"));
            known.add(PseudonymKind::Job, &stem);
        }
        session_ids(&mut known, &layout.agent_session(&agent))?;
    }

    // Sessions: the Rust store and the TS mirror, walked for IDs only.
    for path in [
        layout.session_state(),
        layout.session(),
        layout.sessions(),
        layout.session_fallback(),
    ] {
        session_ids(&mut known, &path)?;
    }

    // Devices and API tokens: IDs, labels and names, never keys or hashes.
    walk_keys(
        &mut known,
        &read_json(&layout.devices())?,
        &[
            ("id", PseudonymKind::Device),
            ("device_id", PseudonymKind::Device),
            ("label", PseudonymKind::Label),
        ],
    );
    walk_keys(
        &mut known,
        &read_json(&layout.api_tokens())?,
        &[
            ("id", PseudonymKind::Token),
            ("token_id", PseudonymKind::Token),
            ("name", PseudonymKind::TokenName),
        ],
    );

    // What the caller supplied.
    for job in &input.jobs {
        known.add(PseudonymKind::Job, &job.summary.name);
        if let Some(agent) = &job.summary.agent {
            known.add(PseudonymKind::Agent, agent);
        }
        if let Some(launcher) = &job.summary.launcher {
            add_launcher(&mut known, launcher);
        }
    }
    for row in &input.claude_code {
        add_launcher(&mut known, &row.launcher);
    }
    if let Some(status) = &input.status {
        add_launcher(&mut known, &status.launchers.default);
        for problem in &status.problems {
            if let Some(scope) = &problem.scope {
                add_scope(&mut known, scope);
            }
            if let Some(job) = &problem.job {
                known.add(PseudonymKind::Job, job);
            }
            if let Some(launcher) = &problem.launcher {
                add_launcher(&mut known, launcher);
            }
        }
        for scope in &status.launchers.refused_scopes {
            add_scope(&mut known, scope);
        }
    }

    // Identifier fields in the Run index, the Daemon log and the Crash record's tail: a first
    // pass, so a name first seen late is still scrubbed in an earlier message.
    for run in runs {
        if let Ok(Value::Object(map)) = serde_json::to_value(run) {
            identifiers(&mut known, &map);
        }
    }
    for line in lines {
        if let LogEntry::Line(line) = &line.entry {
            identifiers(&mut known, &line.span);
            identifiers(&mut known, &line.fields);
        }
    }
    if let CrashState::Read(status) = crash {
        for crash in &status.crashes {
            for tail in crash.stderr_tail.iter().flatten() {
                if let LogEntry::Line(line) = crate::logs::parse_line(tail, None) {
                    identifiers(&mut known, &line.span);
                    identifiers(&mut known, &line.fields);
                }
            }
        }
    }
    Ok(known)
}

/// Every `Identifier`, `Scope` and `Principal` value in `map`, by Field class (#28 D5.1).
pub(crate) fn identifiers(known: &mut Known, map: &Map<String, Value>) {
    for (name, value) in map {
        identifier_value(known, name, value);
    }
}

fn identifier_value(known: &mut Known, name: &str, value: &Value) {
    match (field_class(name), value) {
        (_, Value::Array(items)) => items.iter().for_each(|v| identifier_value(known, name, v)),
        (_, Value::Object(map)) => identifiers(known, map),
        (FieldClass::Identifier(kind), Value::String(s)) => {
            if name == "name" && FIXED_RUN_LOG_NAMES.contains(&s.as_str()) {
                return;
            }
            match kind {
                PseudonymKind::Ip => {
                    if let Ok(ip) = s.parse::<IpAddr>() {
                        known.add(kind, &ip.to_string());
                    } else if let Ok(addr) = s.parse::<SocketAddr>() {
                        known.add(kind, &addr.ip().to_string());
                    }
                }
                PseudonymKind::Launcher => add_launcher(known, s),
                _ => known.add(kind, s),
            }
        }
        (FieldClass::Identifier(PseudonymKind::Discord), Value::Number(n)) => {
            known.add(PseudonymKind::Discord, &n.to_string());
        }
        (FieldClass::Scope, Value::String(s)) => {
            if let Ok(scope) = s.parse() {
                add_scope(known, &scope);
            }
        }
        (FieldClass::Principal, Value::String(s)) => {
            if let Some(id) = s.strip_prefix("device:") {
                known.add(PseudonymKind::Device, id);
            } else if let Some(id) = s.strip_prefix("token:") {
                known.add(PseudonymKind::Token, id);
            }
        }
        _ => {}
    }
}

fn add_scope(known: &mut Known, scope: &Scope) {
    match scope {
        Scope::Global => {}
        Scope::Discord(id) => known.add(PseudonymKind::Discord, id),
        Scope::Job(name) => known.add(PseudonymKind::Job, name),
        Scope::Agent(name) => known.add(PseudonymKind::Agent, name),
    }
}

/// A Launcher by file name; a path is also known as a path.
fn add_launcher(known: &mut Known, launcher: &str) {
    known.add(PseudonymKind::Launcher, launcher);
    if launcher.contains(['/', '\\']) {
        known.add(PseudonymKind::Path, launcher);
    }
}

/// Snowflakes anywhere in `settings.json` (ID lists, bare-number IDs, snowflake-keyed maps)
/// and Launcher paths under any `launcher` key.
fn settings_identifiers(known: &mut Known, map: &Map<String, Value>, prefix: &str) {
    for (key, value) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if is_snowflake(key) {
            known.add(PseudonymKind::Discord, key);
        }
        settings_value(known, &path, value);
    }
}

fn settings_value(known: &mut Known, path: &str, value: &Value) {
    let discord = path == "discord" || path.starts_with("discord.");
    match value {
        Value::Object(map) => settings_identifiers(known, map, path),
        Value::Array(items) => items.iter().for_each(|v| settings_value(known, path, v)),
        Value::String(s) if is_snowflake(s) || (discord && is_id(s)) => {
            known.add(PseudonymKind::Discord, s);
        }
        Value::Number(n) if is_snowflake(&n.to_string()) || (discord && n.is_u64()) => {
            known.add(PseudonymKind::Discord, &n.to_string());
        }
        Value::String(s) if path.to_ascii_lowercase().contains("launcher") && s != "direct" => {
            add_launcher(known, s);
        }
        _ => {}
    }
}

fn is_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_snowflake(s: &str) -> bool {
    (17..=20).contains(&s.len()) && is_id(s)
}

/// Session IDs (`session_id`, `sessionId`) and snowflakes in a session file of any shape.
fn session_ids(known: &mut Known, path: &Path) -> Result<(), ExportError> {
    let value = read_json(path)?;
    walk_keys(
        known,
        &value,
        &[
            ("session_id", PseudonymKind::Session),
            ("sessionId", PseudonymKind::Session),
        ],
    );
    walk_snowflake_keys(known, &value);
    Ok(())
}

fn walk_snowflake_keys(known: &mut Known, value: &Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if is_snowflake(k) {
                    known.add(PseudonymKind::Discord, k);
                }
                walk_snowflake_keys(known, v);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| walk_snowflake_keys(known, v)),
        Value::String(s) if is_snowflake(s) => known.add(PseudonymKind::Discord, s),
        _ => {}
    }
}

/// String values under the named keys, at any depth.
fn walk_keys(known: &mut Known, value: &Value, keys: &[(&str, PseudonymKind)]) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if let (Some((_, kind)), Value::String(s)) = (keys.iter().find(|(n, _)| n == k), v)
                {
                    known.add(*kind, s);
                }
                walk_keys(known, v, keys);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| walk_keys(known, v, keys)),
        _ => {}
    }
}

/// The value at a dotted path.
pub(crate) fn lookup<'a>(map: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut value = map.get(parts.next()?)?;
    for part in parts {
        value = value.as_object()?.get(part)?;
    }
    Some(value)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, ExportError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ExportError::Io {
            op: "read",
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// A JSON file, `null` when missing or not JSON: these files only feed the known values.
fn read_json(path: &Path) -> Result<Value, ExportError> {
    Ok(read_optional(path)?
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null))
}

fn list(dir: &Path) -> Result<Vec<(String, bool)>, ExportError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ExportError::Io {
                op: "list",
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        out.push((name, is_dir));
    }
    out.sort();
    Ok(out)
}

/// Job file stems: `*.md`, dot-prefixed ones excluded (§4 `scan`).
fn md_stems(dir: &Path) -> Result<Vec<String>, ExportError> {
    Ok(list(dir)?
        .into_iter()
        .filter(|(_, is_dir)| !is_dir)
        .filter_map(|(name, _)| name.strip_suffix(".md").map(str::to_owned))
        .collect())
}

fn subdirs(dir: &Path) -> Result<Vec<String>, ExportError> {
    Ok(list(dir)?
        .into_iter()
        .filter(|(_, is_dir)| *is_dir)
        .map(|(name, _)| name)
        .collect())
}
