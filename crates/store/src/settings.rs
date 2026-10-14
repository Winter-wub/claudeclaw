//! `.claude/claudeclaw/settings.json` (architecture §4 `claudeclaw-store`, §7, #14 rules 1–2
//! and 4, #23).
//!
//! - **Reads** give a typed view, [`SettingsRead`], built from the raw JSON. TS keys are parsed
//!   the way TS's `parseSettings` does (`src/config.ts:337-486`); Rust-only keys take the §7
//!   defaults. An invalid field takes its default and is listed in [`SettingsRead::invalid`], so
//!   `core::settings` can report it once as `settings.invalid` (#33).
//! - **Writes** are JSON Merge Patches (RFC 7396) applied to a **fresh read** of the file as a
//!   `serde_json::Value` with `preserve_order`. The file is never re-serialised from a typed
//!   struct, so unknown and TS-only keys, key order and integers above 2^53 stay exact; new keys
//!   go last. Output is TS's own format: 2-space indent and a trailing newline.
//! - **Secrets** (`api`, `fallback.api`, `discord.token`) come out only as
//!   `secrecy::SecretString` in [`Secrets`], and as `{set}` in the view (#13, #23).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use claudeclaw_api::Revision;
use claudeclaw_api::settings::{
    AgenticMode, AgenticSettings, AutoCompactSettings, DiscordSettings, DiskSettings,
    ExcludeWindow, FallbackSettings, HeartbeatSettings, InboxSettings, LoggingSettings,
    RemoteSettings, RunLocaleSetting, RunPathSettings, RunPathSource, SecretState, SecurityLevel,
    SecuritySettings, SessionSettings, Settings, SettingsPatch, SettingsView,
    TimeoutBreakerSettings, TimeoutSettings,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value};

use crate::atomic::{self, DirMode, FileMode};
use crate::error::StoreError;
use crate::layout::Layout;

/// The env var that wins over `discord.token` (§7 precedence, TS `src/config.ts:377`).
pub const DISCORD_TOKEN_ENV: &str = "DISCORD_TOKEN";
/// The env var that wins over `logging.level` (#23 D1).
pub const LOG_LEVEL_ENV: &str = "CLAUDECLAW_LOG";

/// Paths of the snowflake arrays under `discord`, which the Daemon always writes as strings
/// (#14 rule 4).
const SNOWFLAKE_ARRAYS: [&str; 4] = [
    "allowedUserIds",
    "listenChannels",
    "listenGuilds",
    "allowedGuilds",
];

/// One fresh read of `settings.json`: the raw JSON object and its Revision.
#[derive(Clone)]
pub struct SettingsFile {
    path: PathBuf,
    raw: Map<String, Value>,
    revision: Revision,
    exists: bool,
}

impl fmt::Debug for SettingsFile {
    /// Only the top-level key names: the file holds secrets and the heartbeat prompt.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SettingsFile")
            .field("path", &self.path)
            .field("revision", &self.revision)
            .field("keys", &self.raw.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SettingsFile {
    /// Reads `layout.settings()`.
    pub fn load(layout: &Layout) -> Result<Self, StoreError> {
        Self::read(&layout.settings())
    }

    /// Reads a settings file. A missing file reads as `{}` (every key at its default), with the
    /// Revision of empty bytes.
    pub fn read(path: &Path) -> Result<Self, StoreError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SettingsFile {
                    path: path.to_path_buf(),
                    raw: Map::new(),
                    revision: Revision::of(b""),
                    exists: false,
                });
            }
            Err(e) => return Err(StoreError::io("read", path)(e)),
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(|source| StoreError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        let Value::Object(raw) = value else {
            return Err(StoreError::NotAnObject {
                path: path.to_path_buf(),
            });
        };
        Ok(SettingsFile {
            path: path.to_path_buf(),
            raw,
            revision: Revision::of(&bytes),
            exists: true,
        })
    }

    /// The Revision of the bytes read (management-api §1.5).
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// Whether the file existed.
    pub fn exists(&self) -> bool {
        self.exists
    }

    /// The raw JSON, unknown keys included.
    pub fn raw(&self) -> &Map<String, Value> {
        &self.raw
    }

    /// The typed view, with env vars from this process (§7 precedence).
    pub fn view(&self) -> SettingsRead {
        self.view_with_env(|name| std::env::var(name).ok())
    }

    /// The typed view, with env vars from `env`.
    pub fn view_with_env(&self, env: impl Fn(&str) -> Option<String>) -> SettingsRead {
        parse(&self.raw, &env)
    }
}

/// The typed read view of `settings.json` (§7, #23 D1).
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsRead {
    /// The keys the Rust Daemon reads, after env and defaults. Secrets show as `{set}`.
    pub settings: Settings,
    /// The secrets' values.
    pub secrets: Secrets,
    /// Dotted paths of TS-only keys in the file (`web.port`, `apiToken`, …), read for import only
    /// (§7, #14 rules 2–3). `watchdog.*` among them is `settings.legacy_key` (#32).
    pub ts_only_keys: Vec<String>,
    /// Dotted paths an env var won over, e.g. `discord.token` for `DISCORD_TOKEN`.
    pub env_overrides: Vec<String>,
    /// Dotted paths of fields that were invalid and took their default or were clamped (#33).
    pub invalid: Vec<String>,
}

impl SettingsRead {
    /// The `settings.get` response for this read (management-api §2.6).
    pub fn to_view(&self, revision: Revision) -> SettingsView {
        SettingsView {
            revision,
            settings: self.settings.clone(),
            ts_only_keys: self.ts_only_keys.clone(),
            env_overrides: self.env_overrides.clone(),
        }
    }
}

/// The secrets in `settings.json` (#23 D6). `None` when unset or empty. `Debug` is redacted by
/// `secrecy`.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    /// `api`: the token runs get as `ANTHROPIC_AUTH_TOKEN`.
    pub api: Option<SecretString>,
    /// `fallback.api`.
    pub fallback_api: Option<SecretString>,
    /// `discord.token`, or `DISCORD_TOKEN` when set.
    pub discord_token: Option<SecretString>,
}

impl PartialEq for Secrets {
    fn eq(&self, other: &Self) -> bool {
        fn same(a: &Option<SecretString>, b: &Option<SecretString>) -> bool {
            a.as_ref().map(ExposeSecret::expose_secret)
                == b.as_ref().map(ExposeSecret::expose_secret)
        }
        same(&self.api, &other.api)
            && same(&self.fallback_api, &other.fallback_api)
            && same(&self.discord_token, &other.discord_token)
    }
}

// ---- Writes -------------------------------------------------------------------------------------

/// Why a settings patch wasn't written.
#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    /// The file changed since the caller's `If-Match` Revision (management-api §1.5).
    #[error("settings.json changed on disk (now revision {current})")]
    Conflict { current: Revision },
    /// Includes an unparseable file: it is never overwritten.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A written patch.
#[derive(Debug)]
pub struct Patched {
    /// The file as written.
    pub file: SettingsFile,
    /// Dotted paths of the values that changed. Empty means nothing was written.
    pub changed_keys: Vec<String>,
}

/// Applies `patch` (RFC 7396) over a fresh read of `layout.settings()` and writes it atomically
/// at 0600 (§9.1). With `if_match`, a file whose Revision differs is left alone. Snowflakes the
/// patch carries as numbers are written as strings (#14 rule 4). A patch that changes nothing
/// writes nothing.
///
/// Which keys a caller may patch (TS-only keys, `remote.*` from a Device, …) is the caller's
/// rule, not this function's.
pub fn patch(
    layout: &Layout,
    patch: &SettingsPatch,
    if_match: Option<Revision>,
) -> Result<Patched, PatchError> {
    let current = SettingsFile::load(layout)?;
    if let Some(expected) = if_match
        && expected != current.revision
    {
        return Err(PatchError::Conflict {
            current: current.revision,
        });
    }
    let mut patch = patch.0.clone();
    snowflakes_to_strings(&mut patch);

    let old = Value::Object(current.raw.clone());
    let mut new = old.clone();
    merge_patch(&mut new, &Value::Object(patch));
    let mut changed_keys = Vec::new();
    diff("", Some(&old), Some(&new), &mut changed_keys);
    if changed_keys.is_empty() {
        return Ok(Patched {
            file: current,
            changed_keys,
        });
    }

    let bytes = to_ts_json(&new);
    let path = layout.settings();
    atomic::create_dir(layout.root(), DirMode::Default)?;
    atomic::write_atomic(&path, &bytes, FileMode::Private)?;
    let Value::Object(raw) = new else {
        unreachable!("a merge patch over an object stays an object")
    };
    Ok(Patched {
        file: SettingsFile {
            path,
            raw,
            revision: Revision::of(&bytes),
            exists: true,
        },
        changed_keys,
    })
}

/// `JSON.stringify(value, null, 2) + "\n"`, the bytes TS writes (`src/config.ts:276`).
pub(crate) fn to_ts_json(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("a Value serialises");
    bytes.push(b'\n');
    bytes
}

/// RFC 7396 merge. Removing a key keeps the order of the others; a new key goes last.
fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Value::Object(target) = target else {
        unreachable!()
    };
    for (key, value) in patch {
        if value.is_null() {
            target.shift_remove(key);
        } else {
            merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// Dotted paths of the leaves that differ between `old` and `new`. Arrays are leaves; an object
/// that appears or disappears contributes its leaves, and a value that changes between an object
/// and a non-object also contributes its own path.
fn diff(prefix: &str, old: Option<&Value>, new: Option<&Value>, out: &mut Vec<String>) {
    let (a, b) = (
        old.and_then(Value::as_object),
        new.and_then(Value::as_object),
    );
    if a.is_none() && b.is_none() {
        if old != new {
            out.push(prefix.to_owned());
        }
        return;
    }
    if (a.is_none() && old.is_some()) || (b.is_none() && new.is_some()) {
        out.push(prefix.to_owned());
    }
    let empty = Map::new();
    let (a, b) = (a.unwrap_or(&empty), b.unwrap_or(&empty));
    for key in a.keys().chain(b.keys().filter(|k| !a.contains_key(*k))) {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        diff(&path, a.get(key), b.get(key), out);
    }
}

/// Turns number snowflakes in a patch's `discord` arrays and `channelAllowedUserIds` values into
/// strings (#14 rule 4).
fn snowflakes_to_strings(patch: &mut Map<String, Value>) {
    let Some(Value::Object(discord)) = patch.get_mut("discord") else {
        return;
    };
    let fix = |value: &mut Value| {
        if let Value::Array(items) = value {
            for item in items {
                if let Value::Number(n) = item {
                    *item = Value::String(n.to_string());
                }
            }
        }
    };
    for key in SNOWFLAKE_ARRAYS {
        if let Some(value) = discord.get_mut(key) {
            fix(value);
        }
    }
    if let Some(Value::Object(channels)) = discord.get_mut("channelAllowedUserIds") {
        channels.values_mut().for_each(fix);
    }
}

// ---- The read view ------------------------------------------------------------------------------

/// Reads typed fields out of the raw JSON, recording every invalid one.
struct Reader<'a> {
    root: &'a Map<String, Value>,
    invalid: Vec<String>,
}

impl<'a> Reader<'a> {
    /// The value at a dotted path; `null` counts as missing, as TS's `??` does.
    fn get(&self, path: &str) -> Option<&'a Value> {
        let mut parts = path.split('.');
        let mut value = self.root.get(parts.next()?)?;
        for part in parts {
            value = value.as_object()?.get(part)?;
        }
        (!value.is_null()).then_some(value)
    }

    fn invalid(&mut self, path: &str) {
        self.invalid.push(path.to_owned());
    }

    /// TS: `typeof x === "string" ? x.trim() : ""`.
    fn string(&mut self, path: &str) -> String {
        match self.get(path) {
            Some(Value::String(s)) => s.trim().to_owned(),
            Some(_) => {
                self.invalid(path);
                String::new()
            }
            None => String::new(),
        }
    }

    fn string_or(&mut self, path: &str, default: &str) -> String {
        match self.get(path) {
            Some(Value::String(s)) => s.trim().to_owned(),
            Some(_) => {
                self.invalid(path);
                default.to_owned()
            }
            None => default.to_owned(),
        }
    }

    /// TS: `x ?? default` for a boolean.
    fn bool_or(&mut self, path: &str, default: bool) -> bool {
        match self.get(path) {
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                self.invalid(path);
                default
            }
            None => default,
        }
    }

    /// An integer within `range`; anything else takes `default`.
    fn int_or<T: TryFrom<u64> + Copy>(
        &mut self,
        path: &str,
        default: T,
        range: std::ops::RangeInclusive<u64>,
    ) -> T {
        match self.get(path) {
            None => default,
            Some(v) => match v.as_u64().filter(|n| range.contains(n)).map(T::try_from) {
                Some(Ok(n)) => n,
                _ => {
                    self.invalid(path);
                    default
                }
            },
        }
    }

    /// An integer clamped up to `min`, or `null`; missing takes `default` (#23 D1, #44).
    fn nullable_min(&mut self, path: &str, default: Option<u32>, min: u32) -> Option<u32> {
        match self.root_get_raw(path) {
            None => default,
            Some(Value::Null) => None,
            Some(v) => match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
                Some(n) if n < min => {
                    self.invalid(path);
                    Some(min)
                }
                Some(n) => Some(n),
                None => {
                    self.invalid(path);
                    default
                }
            },
        }
    }

    /// Like [`Reader::get`], but `null` is returned as a value.
    fn root_get_raw(&self, path: &str) -> Option<&'a Value> {
        let mut parts = path.split('.');
        let mut value = self.root.get(parts.next()?)?;
        for part in parts {
            value = value.as_object()?.get(part)?;
        }
        Some(value)
    }

    /// An array of strings. TS: `Array.isArray(x) ? x.map(String) : []`; numbers become their
    /// decimal text (exact here, as TS's raw-text snowflake extraction makes them,
    /// `src/config.ts:528-540`), other items are dropped and reported.
    fn strings(&mut self, path: &str) -> Vec<String> {
        match self.get(path) {
            Some(Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Value::String(s) => out.push(s.clone()),
                        Value::Number(n) => out.push(n.to_string()),
                        _ => self.invalid(path),
                    }
                }
                out
            }
            Some(_) => {
                self.invalid(path);
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    fn object(&mut self, path: &str) -> Option<&'a Map<String, Value>> {
        match self.get(path) {
            Some(Value::Object(map)) => Some(map),
            Some(_) => {
                self.invalid(path);
                None
            }
            None => None,
        }
    }

    fn wire_enum<T: serde::de::DeserializeOwned + PartialEq>(
        &mut self,
        path: &str,
        default: T,
        unknown: T,
    ) -> T {
        match self.get(path) {
            None => default,
            Some(v) => match serde_json::from_value::<T>(v.clone()) {
                Ok(t) if t != unknown => t,
                _ => {
                    self.invalid(path);
                    default
                }
            },
        }
    }
}

fn parse(raw: &Map<String, Value>, env: &dyn Fn(&str) -> Option<String>) -> SettingsRead {
    let mut r = Reader {
        root: raw,
        invalid: Vec::new(),
    };
    let mut env_overrides = Vec::new();
    let env_value = |name: &str| {
        env(name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };

    let api = r.string("api");
    let fallback_api = r.string("fallback.api");
    let file_token = r.string("discord.token");
    let discord_token = match env_value(DISCORD_TOKEN_ENV) {
        Some(token) => {
            env_overrides.push("discord.token".to_owned());
            token
        }
        None => file_token,
    };
    let log_level = match env_value(LOG_LEVEL_ENV) {
        Some(level) => {
            env_overrides.push("logging.level".to_owned());
            r.string_or("logging.level", "info");
            level
        }
        None => r.string_or("logging.level", "info"),
    };

    let settings = Settings {
        timezone: timezone(&mut r),
        model: r.string("model"),
        api: state(&api),
        fallback: FallbackSettings {
            model: r.string("fallback.model"),
            api: state(&fallback_api),
        },
        agentic: agentic(&mut r),
        heartbeat: HeartbeatSettings {
            enabled: r.bool_or("heartbeat.enabled", false),
            interval: r.int_or("heartbeat.interval", 15, 1..=u64::from(u32::MAX)),
            prompt: match r.get("heartbeat.prompt") {
                Some(Value::String(s)) => s.clone(),
                Some(_) => {
                    r.invalid("heartbeat.prompt");
                    String::new()
                }
                None => String::new(),
            },
            exclude_windows: exclude_windows(&mut r),
        },
        discord: discord(&mut r, &discord_token),
        security: SecuritySettings {
            level: r.wire_enum(
                "security.level",
                SecurityLevel::Moderate,
                SecurityLevel::Unknown,
            ),
            allowed_tools: r.strings("security.allowedTools"),
            disallowed_tools: r.strings("security.disallowedTools"),
        },
        session: SessionSettings {
            auto_rotate: r.bool_or("session.autoRotate", false),
            max_messages: r.int_or("session.maxMessages", 50, 0..=u64::from(u32::MAX)),
            max_age_hours: r.int_or("session.maxAgeHours", 24, 0..=u64::from(u32::MAX)),
            summary_path: r.string("session.summaryPath"),
        },
        session_timeout_ms: r.int_or("sessionTimeoutMs", 30 * 60 * 1000, 1..=u64::MAX),
        timeouts: TimeoutSettings {
            heartbeat: r.int_or("timeouts.heartbeat", 15, 1..=u64::from(u32::MAX)),
            job: r.int_or("timeouts.job", 30, 1..=u64::from(u32::MAX)),
            default: r.int_or("timeouts.default", 5, 1..=u64::from(u32::MAX)),
        },
        logging: LoggingSettings {
            level: log_level,
            daemon_keep_files: clamp(&mut r, "logging.daemonKeepFiles", 14, 1, 365),
            daemon_max_daily_mib: r.int_or(
                "logging.daemonMaxDailyMiB",
                256,
                1..=u64::from(u32::MAX),
            ),
            daemon_max_total_mib: r.int_or(
                "logging.daemonMaxTotalMiB",
                1024,
                1..=u64::from(u32::MAX),
            ),
            run_log_keep_days: r.nullable_min("logging.runLogKeepDays", None, 7),
            audit_keep_months: r.nullable_min("logging.auditKeepMonths", None, 3),
        },
        remote: RemoteSettings {
            enabled: r.bool_or("remote.enabled", false),
            listen: r.string_or("remote.listen", "0.0.0.0"),
            port: r.int_or("remote.port", 4633, 1..=u64::from(u16::MAX)),
        },
        run_path: RunPathSettings {
            source: r.wire_enum(
                "run_path.source",
                RunPathSource::Auto,
                RunPathSource::Unknown,
            ),
            prepend: r.strings("run_path.prepend"),
        },
        run_locale: r.wire_enum(
            "run_locale",
            RunLocaleSetting::Auto,
            RunLocaleSetting::Unknown,
        ),
        timeout_breaker: TimeoutBreakerSettings {
            max_consecutive_timeouts: r.nullable_min(
                "timeout_breaker.max_consecutive_timeouts",
                None,
                1,
            ),
            max_failing_seconds: r
                .nullable_min("timeout_breaker.max_failing_seconds", None, 1)
                .map(u64::from),
        },
        auto_compact: AutoCompactSettings {
            enabled: r.bool_or("auto_compact.enabled", true),
            timeout_secs: r.int_or("auto_compact.timeout_secs", 300, 1..=u64::from(u32::MAX)),
        },
        transcript_roots: r.strings("transcript_roots"),
        inbox: InboxSettings {
            keep_days: r.nullable_min("inbox.keepDays", Some(30), 1),
            max_mib: r.nullable_min("inbox.maxMiB", Some(1024), 1),
        },
        disk: DiskSettings {
            warn_free_mib: r.int_or("disk.warnFreeMiB", 2048, 0..=u64::from(u32::MAX)),
            min_free_mib: r.int_or("disk.minFreeMiB", 256, 0..=u64::from(u32::MAX)),
        },
    };

    SettingsRead {
        settings,
        secrets: Secrets {
            api: secret(api),
            fallback_api: secret(fallback_api),
            discord_token: secret(discord_token),
        },
        ts_only_keys: ts_only_keys(raw),
        env_overrides,
        invalid: r.invalid,
    }
}

fn state(secret: &str) -> SecretState {
    SecretState {
        set: !secret.is_empty(),
    }
}

fn secret(value: String) -> Option<SecretString> {
    (!value.is_empty()).then(|| SecretString::from(value))
}

/// An integer clamped into `min..=max`; a clamped value is reported (#23 D1).
fn clamp(r: &mut Reader<'_>, path: &str, default: u32, min: u32, max: u32) -> u32 {
    match r.get(path).map(Value::as_u64) {
        None => default,
        Some(Some(n)) if (u64::from(min)..=u64::from(max)).contains(&n) => n as u32,
        Some(Some(n)) => {
            r.invalid(path);
            if n < u64::from(min) { min } else { max }
        }
        Some(None) => {
            r.invalid(path);
            default
        }
    }
}

/// TS `normalizeTimezoneName` (`src/timezone.ts:27-41`): a `UTC±H[:MM]` / `GMT±H[:MM]` offset
/// is upper-cased with spaces removed; anything else is kept trimmed. TS also drops an unknown
/// IANA name; store has no zone database, so the scheduler does that check.
fn timezone(r: &mut Reader<'_>) -> String {
    let name = r.string("timezone");
    let compact: String = name
        .to_ascii_uppercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if is_utc_offset(&compact) {
        compact
    } else {
        name
    }
}

/// TS `parseUtcOffsetMinutes` (`src/timezone.ts:13-25`) on an upper-cased, space-free string.
fn is_utc_offset(s: &str) -> bool {
    if s == "UTC" || s == "GMT" {
        return true;
    }
    let Some(rest) = s.strip_prefix("UTC").or_else(|| s.strip_prefix("GMT")) else {
        return false;
    };
    let (sign, rest) = match rest.as_bytes().first() {
        Some(b'+') => (1, &rest[1..]),
        Some(b'-') => (-1, &rest[1..]),
        _ => return false,
    };
    let (hours, minutes) = match rest.split_once(':') {
        Some((h, m)) => (h, Some(m)),
        None if rest.len() > 2 => (&rest[..rest.len() - 2], Some(&rest[rest.len() - 2..])),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(hours) || hours.len() > 2 {
        return false;
    }
    let minutes = match minutes {
        None => 0,
        Some(m) if m.len() == 2 && digits(m) && m.as_bytes()[0] <= b'5' => {
            m.parse::<i32>().unwrap()
        }
        Some(_) => return false,
    };
    let hours: i32 = hours.parse().unwrap();
    let total = sign * (hours * 60 + minutes);
    hours <= 14 && (-12 * 60..=14 * 60).contains(&total)
}

/// TS `DEFAULT_SETTINGS.agentic.modes` (`src/config.ts:43-69`).
fn default_agentic_modes() -> Vec<AgenticMode> {
    let words = |list: &[&str]| list.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    vec![
        AgenticMode {
            name: "planning".into(),
            model: "opus".into(),
            keywords: words(&[
                "plan",
                "design",
                "architect",
                "strategy",
                "approach",
                "research",
                "investigate",
                "analyze",
                "explore",
                "understand",
                "think",
                "consider",
                "evaluate",
                "assess",
                "review",
                "system design",
                "trade-off",
                "decision",
                "choose",
                "compare",
                "brainstorm",
                "ideate",
                "concept",
                "proposal",
            ]),
            phrases: Some(words(&[
                "how to implement",
                "how should i",
                "what's the best way to",
                "should i",
                "which approach",
                "help me decide",
                "help me understand",
            ])),
        },
        AgenticMode {
            name: "implementation".into(),
            model: "sonnet".into(),
            keywords: words(&[
                "implement",
                "code",
                "write",
                "create",
                "build",
                "add",
                "fix",
                "debug",
                "refactor",
                "update",
                "modify",
                "change",
                "deploy",
                "run",
                "execute",
                "install",
                "configure",
                "test",
                "commit",
                "push",
                "merge",
                "release",
                "generate",
                "scaffold",
                "setup",
                "initialize",
            ]),
            phrases: None,
        },
    ]
}

/// TS `parseAgenticConfig` (`src/config.ts:301-335`), including the old
/// `planningModel` / `implementationModel` form.
fn agentic(r: &mut Reader<'_>) -> AgenticSettings {
    let mut modes = default_agentic_modes();
    let Some(raw) = r.object("agentic") else {
        return AgenticSettings {
            enabled: false,
            default_mode: "implementation".into(),
            modes,
        };
    };
    let enabled = r.bool_or("agentic.enabled", false);
    let has_modes = matches!(raw.get("modes"), Some(Value::Array(_)));
    if !has_modes && (raw.contains_key("planningModel") || raw.contains_key("implementationModel"))
    {
        let model = |key: &str, default: &str| match raw.get(key) {
            Some(Value::String(s)) => s.trim().to_owned(),
            _ => default.to_owned(),
        };
        modes[0].model = model("planningModel", "opus");
        modes[1].model = model("implementationModel", "sonnet");
        return AgenticSettings {
            enabled,
            default_mode: "implementation".into(),
            modes,
        };
    }
    let parsed: Vec<AgenticMode> = match raw.get("modes") {
        Some(Value::Array(items)) => items.iter().filter_map(agentic_mode).collect(),
        _ => Vec::new(),
    };
    AgenticSettings {
        enabled,
        default_mode: r.string_or("agentic.defaultMode", "implementation"),
        modes: if parsed.is_empty() { modes } else { parsed },
    }
}

/// TS `parseAgenticMode` (`src/config.ts:287-299`).
fn agentic_mode(raw: &Value) -> Option<AgenticMode> {
    let raw = raw.as_object()?;
    let text = |key: &str| raw.get(key).and_then(Value::as_str).map(str::trim);
    let name = text("name").filter(|s| !s.is_empty())?;
    let model = text("model").filter(|s| !s.is_empty())?;
    let list = |key: &str| {
        raw.get(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|s| s.to_lowercase().trim().to_owned())
                .collect::<Vec<_>>()
        })
    };
    Some(AgenticMode {
        name: name.to_owned(),
        model: model.to_owned(),
        keywords: list("keywords").unwrap_or_default(),
        phrases: list("phrases").filter(|p| !p.is_empty()),
    })
}

/// TS `parseExcludeWindows` (`src/config.ts:495-517`): `HH:MM` bounds, days 0–6 sorted and
/// de-duplicated, every day when none are valid.
fn exclude_windows(r: &mut Reader<'_>) -> Vec<ExcludeWindow> {
    let Some(Value::Array(items)) = r.get("heartbeat.excludeWindows") else {
        return Vec::new();
    };
    let is_time = |s: &str| {
        let b = s.as_bytes();
        b.len() == 5
            && b[2] == b':'
            && matches!(
                (b[0], b[1]),
                (b'0'..=b'1', b'0'..=b'9') | (b'2', b'0'..=b'3')
            )
            && matches!((b[3], b[4]), (b'0'..=b'5', b'0'..=b'9'))
    };
    let mut out = Vec::new();
    for entry in items {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::trim);
        let (Some(start), Some(end)) = (text("start"), text("end")) else {
            continue;
        };
        if !is_time(start) || !is_time(end) {
            continue;
        }
        let mut days: Vec<u8> = entry
            .get("days")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| match d {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            })
            .filter(|d| d.fract() == 0.0 && (0.0..=6.0).contains(d))
            .map(|d| d as u8)
            .collect();
        days.sort_unstable();
        days.dedup();
        if days.is_empty() {
            days = (0..=6).collect();
        }
        out.push(ExcludeWindow {
            days: Some(days),
            start: start.to_owned(),
            end: end.to_owned(),
        });
    }
    out
}

/// TS's `discord` parse (`src/config.ts:376-408`). Snowflakes are strings.
fn discord(r: &mut Reader<'_>, token: &str) -> DiscordSettings {
    let string_map = |r: &mut Reader<'_>, path: &str| -> BTreeMap<String, String> {
        r.object(path)
            .into_iter()
            .flatten()
            .map(|(k, v)| {
                let v = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), v)
            })
            .collect()
    };
    let channel_names = string_map(r, "discord.channelNames");
    let channel_allowed_user_ids = r
        .object("discord.channelAllowedUserIds")
        .into_iter()
        .flatten()
        .filter(|(_, v)| v.is_array())
        .map(|(k, _)| {
            let ids = r.strings(&format!("discord.channelAllowedUserIds.{k}"));
            (k.clone(), ids)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    DiscordSettings {
        token: state(token),
        allowed_user_ids: r.strings("discord.allowedUserIds"),
        listen_channels: r.strings("discord.listenChannels"),
        listen_guilds: r.strings("discord.listenGuilds"),
        allowed_guilds: r.strings("discord.allowedGuilds"),
        channel_allowed_user_ids,
        channel_names,
        image_output_roots: r
            .strings("discord.imageOutputRoots")
            .into_iter()
            // Node's `path.isAbsolute` (src/config.ts:398): on Windows a rooted path such as
            // `/abs` counts too, which `Path::is_absolute` rejects without a drive letter.
            .filter(|p| Path::new(p).is_absolute() || Path::new(p).has_root())
            .collect(),
        streaming: matches!(r.get("discord.streaming"), Some(Value::Bool(true))),
    }
}

/// Dotted paths of the TS-only keys present (§7, management-api §2.6): `web.*`,
/// `runtime.launcher`, `runtime.launchers`, `apiToken` and `watchdog.*`.
fn ts_only_keys(raw: &Map<String, Value>) -> Vec<String> {
    let mut keys = Vec::new();
    let mut children = |parent: &str| match raw.get(parent) {
        Some(Value::Object(map)) => keys.extend(map.keys().map(|k| format!("{parent}.{k}"))),
        Some(_) => keys.push(parent.to_owned()),
        None => {}
    };
    children("web");
    children("watchdog");
    if let Some(Value::Object(runtime)) = raw.get("runtime") {
        for key in ["launcher", "launchers"] {
            if runtime.contains_key(key) {
                keys.push(format!("runtime.{key}"));
            }
        }
    }
    if raw.contains_key("apiToken") {
        keys.push("apiToken".to_owned());
    }
    keys
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.root()).unwrap();
        (dir, layout)
    }

    fn patch_of(value: Value) -> SettingsPatch {
        serde_json::from_value(value).unwrap()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// A 19-digit integer, built at run time so no long digit run sits in the source.
    fn big() -> u64 {
        10u64.pow(18) + 7
    }

    #[test]
    fn a_patch_keeps_unknown_keys_order_and_a_19_digit_integer() {
        let (_dir, layout) = layout();
        let big = big();
        assert_eq!(big.to_string().len(), 19);
        assert!(big > 1 << 53);
        // TS's format: 2-space indent, trailing newline; keys deliberately not sorted.
        let before = format!(
            "{{\n  \"zeta\": {{\n    \"keep\": true\n  }},\n  \"model\": \"opus\",\n  \
             \"discord\": {{\n    \"allowedUserIds\": [\n      {big}\n    ]\n  }},\n  \
             \"alpha\": 1\n}}\n"
        );
        std::fs::write(layout.settings(), &before).unwrap();

        let done = patch(
            &layout,
            &patch_of(json!({"auto_compact": {"enabled": false}})),
            None,
        )
        .unwrap();
        assert_eq!(done.changed_keys, ["auto_compact.enabled"]);

        let after = std::fs::read_to_string(layout.settings()).unwrap();
        let expected = format!(
            "{},\n  \"auto_compact\": {{\n    \"enabled\": false\n  }}\n}}\n",
            before.trim_end().trim_end_matches('}').trim_end()
        );
        assert_eq!(after, expected);
        assert!(after.contains(&big.to_string()));
        let keys: Vec<_> = done.file.raw().keys().collect();
        assert_eq!(keys, ["zeta", "model", "discord", "alpha", "auto_compact"]);
        assert_eq!(done.file.revision(), Revision::of(after.as_bytes()));

        // The untouched number snowflake reads as the exact string.
        let view = done.file.view_with_env(no_env);
        assert_eq!(view.settings.discord.allowed_user_ids, [big.to_string()]);
    }

    #[test]
    fn removing_a_key_keeps_the_order_of_the_rest() {
        let (_dir, layout) = layout();
        std::fs::write(layout.settings(), r#"{"c":1,"b":{"x":1,"y":2},"a":3}"#).unwrap();
        let done = patch(
            &layout,
            &patch_of(json!({"b": {"x": null}, "c": null})),
            None,
        )
        .unwrap();
        assert_eq!(done.changed_keys, ["c", "b.x"]);
        let after = std::fs::read_to_string(layout.settings()).unwrap();
        assert_eq!(after, "{\n  \"b\": {\n    \"y\": 2\n  },\n  \"a\": 3\n}\n");
    }

    #[test]
    fn snowflakes_the_daemon_writes_are_strings() {
        let (_dir, layout) = layout();
        let done = patch(
            &layout,
            &patch_of(json!({"discord": {
                "listenChannels": [1234, "5678"],
                "channelAllowedUserIds": {"1234": [42]},
            }})),
            None,
        )
        .unwrap();
        assert_eq!(
            done.file.raw()["discord"],
            json!({"listenChannels": ["1234", "5678"], "channelAllowedUserIds": {"1234": ["42"]}})
        );
    }

    #[test]
    fn if_match_refuses_a_changed_file_and_a_no_op_writes_nothing() {
        let (_dir, layout) = layout();
        std::fs::write(layout.settings(), "{\"model\":\"opus\"}").unwrap();
        let stale = Revision::of(b"{}");
        let err = patch(&layout, &patch_of(json!({"model": "x"})), Some(stale)).unwrap_err();
        assert!(matches!(err, PatchError::Conflict { .. }), "{err:?}");

        let current = SettingsFile::load(&layout).unwrap().revision();
        let done = patch(&layout, &patch_of(json!({"model": "opus"})), Some(current)).unwrap();
        assert!(done.changed_keys.is_empty());
        // Still the original, compact bytes: nothing was rewritten.
        assert_eq!(
            std::fs::read_to_string(layout.settings()).unwrap(),
            "{\"model\":\"opus\"}"
        );
    }

    #[test]
    fn an_unparseable_file_is_never_overwritten() {
        let (_dir, layout) = layout();
        std::fs::write(layout.settings(), "{ not json").unwrap();
        let err = patch(&layout, &patch_of(json!({"model": "x"})), None).unwrap_err();
        assert!(
            matches!(err, PatchError::Store(StoreError::Json { .. })),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(layout.settings()).unwrap(),
            "{ not json"
        );

        std::fs::write(layout.settings(), "[]").unwrap();
        assert!(matches!(
            SettingsFile::load(&layout),
            Err(StoreError::NotAnObject { .. })
        ));
    }

    #[test]
    fn a_missing_file_reads_as_defaults_and_a_patch_creates_it() {
        let (_dir, layout) = layout();
        let file = SettingsFile::load(&layout).unwrap();
        assert!(!file.exists());
        assert_eq!(file.revision(), Revision::of(b""));
        patch(&layout, &patch_of(json!({"model": "opus"})), None).unwrap();
        assert_eq!(
            std::fs::read_to_string(layout.settings()).unwrap(),
            "{\n  \"model\": \"opus\"\n}\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_patched_settings_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, layout) = layout();
        std::fs::write(layout.settings(), "{}").unwrap();
        crate::atomic::set_mode(&layout.settings(), 0o644).unwrap();
        patch(&layout, &patch_of(json!({"model": "opus"})), None).unwrap();
        let mode = std::fs::metadata(layout.settings())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn defaults_match_ts_and_section_7() {
        let view = parse(&Map::new(), &no_env);
        let s = &view.settings;
        // TS `parseSettings` on `{}` (`src/config.ts:349-454`).
        assert_eq!(s.timezone, "");
        assert_eq!(s.model, "");
        assert!(!s.api.set);
        assert!(!s.agentic.enabled);
        assert_eq!(s.agentic.default_mode, "implementation");
        assert_eq!(s.agentic.modes.len(), 2);
        assert_eq!(s.agentic.modes[0].keywords.len(), 24);
        assert_eq!(s.agentic.modes[1].keywords.len(), 26);
        assert!(!s.heartbeat.enabled);
        assert_eq!(s.heartbeat.interval, 15);
        assert_eq!(s.security.level, SecurityLevel::Moderate);
        assert_eq!(s.session.max_messages, 50);
        assert_eq!(s.session.max_age_hours, 24);
        assert_eq!(s.session_timeout_ms, 1_800_000);
        assert_eq!(
            (s.timeouts.heartbeat, s.timeouts.job, s.timeouts.default),
            (15, 30, 5)
        );
        // Rust-only keys (§7, #23 D1, #33, #44).
        assert_eq!(s.logging.level, "info");
        assert_eq!(s.logging.daemon_keep_files, 14);
        assert_eq!(s.logging.daemon_max_daily_mib, 256);
        assert_eq!(s.logging.daemon_max_total_mib, 1024);
        assert_eq!(s.logging.run_log_keep_days, None);
        assert_eq!(s.logging.audit_keep_months, None);
        assert!(!s.remote.enabled);
        assert_eq!(s.run_path.source, RunPathSource::Auto);
        assert_eq!(s.run_locale, RunLocaleSetting::Auto);
        assert_eq!(s.timeout_breaker.max_consecutive_timeouts, None);
        assert_eq!(
            s.auto_compact,
            AutoCompactSettings {
                enabled: true,
                timeout_secs: 300
            }
        );
        assert_eq!(s.inbox.keep_days, Some(30));
        assert_eq!(s.inbox.max_mib, Some(1024));
        assert_eq!((s.disk.warn_free_mib, s.disk.min_free_mib), (2048, 256));
        assert!(view.invalid.is_empty(), "{:?}", view.invalid);
        assert!(view.ts_only_keys.is_empty());
    }

    #[test]
    fn a_ts_shaped_file_reads_like_ts() {
        let raw = json!({
            "timezone": " utc + 7 ",
            "model": " opus ",
            "api": "key-a",
            "heartbeat": {
                "enabled": true,
                "interval": 30,
                "prompt": "check",
                "excludeWindows": [
                    {"start": "23:00", "end": "07:00", "days": [6, "0", 6, 9]},
                    {"start": "24:00", "end": "07:00"},
                    {"start": "01:00", "end": "02:00"},
                ],
            },
            "discord": {
                "token": "file-token",
                "allowedUserIds": [1234, "5678"],
                "channelNames": {"1234": "general"},
                "channelAllowedUserIds": {"1234": [42], "9": "not-an-array"},
                "imageOutputRoots": ["/abs", "relative"],
                "streaming": "yes",
            },
            "security": {"level": "paranoid"},
            "agentic": {"enabled": true, "planningModel": " big "},
            "timeouts": {"job": 0},
            "logging": {"daemonKeepFiles": 999, "runLogKeepDays": 2, "auditKeepMonths": null},
            "inbox": {"keepDays": null},
            "web": {"port": 4632, "enabled": false},
            "runtime": {"launcher": {"kind": "direct"}},
            "apiToken": "legacy",
            "watchdog": {"maxRuntimeSeconds": 60},
            "somethingNew": {"kept": true},
        });
        let Value::Object(raw) = raw else {
            unreachable!()
        };
        let env = |name: &str| (name == DISCORD_TOKEN_ENV).then(|| " env-token ".to_owned());
        let view = parse(&raw, &env);
        let s = &view.settings;

        assert_eq!(s.timezone, "UTC+7");
        assert_eq!(s.model, "opus");
        assert!(s.api.set);
        assert_eq!(
            s.heartbeat.exclude_windows,
            [
                ExcludeWindow {
                    days: Some(vec![0, 6]),
                    start: "23:00".into(),
                    end: "07:00".into()
                },
                ExcludeWindow {
                    days: Some((0..=6).collect()),
                    start: "01:00".into(),
                    end: "02:00".into(),
                },
            ]
        );
        assert_eq!(s.discord.allowed_user_ids, ["1234", "5678"]);
        assert_eq!(s.discord.channel_names["1234"], "general");
        assert_eq!(
            s.discord.channel_allowed_user_ids,
            BTreeMap::from([("1234".to_owned(), vec!["42".to_owned()])])
        );
        assert_eq!(s.discord.image_output_roots, ["/abs"]);
        assert!(!s.discord.streaming);
        assert!(s.discord.token.set);
        assert_eq!(s.security.level, SecurityLevel::Moderate);
        assert_eq!(s.agentic.modes[0].model, "big");
        assert_eq!(s.agentic.modes[1].model, "sonnet");
        assert_eq!(s.timeouts.job, 30);
        assert_eq!(s.logging.daemon_keep_files, 365);
        assert_eq!(s.logging.run_log_keep_days, Some(7));
        assert_eq!(s.logging.audit_keep_months, None);
        assert_eq!(s.inbox.keep_days, None);

        assert_eq!(
            view.secrets
                .discord_token
                .as_ref()
                .map(|t| t.expose_secret()),
            Some("env-token")
        );
        assert_eq!(
            view.secrets.api.as_ref().map(|t| t.expose_secret()),
            Some("key-a")
        );
        assert!(view.secrets.fallback_api.is_none());
        assert_eq!(view.env_overrides, ["discord.token"]);
        assert_eq!(
            view.ts_only_keys,
            [
                "web.port",
                "web.enabled",
                "watchdog.maxRuntimeSeconds",
                "runtime.launcher",
                "apiToken"
            ]
        );
        assert_eq!(
            view.invalid,
            [
                "security.level",
                "timeouts.job",
                "logging.daemonKeepFiles",
                "logging.runLogKeepDays",
            ]
        );
    }

    #[test]
    fn utc_offsets_follow_ts() {
        // Expected values from `parseUtcOffsetMinutes`, `src/timezone.ts:13-25`.
        for (input, offset) in [
            ("UTC", true),
            ("GMT", true),
            ("UTC+7", true),
            ("UTC-03:30", true),
            ("UTC+0530", true),
            ("UTC+14", true),
            ("UTC+15", false),
            ("UTC-13", false),
            ("UTC+7:60", false),
            ("UTC7", false),
            ("ASIA/BANGKOK", false),
        ] {
            assert_eq!(is_utc_offset(input), offset, "{input}");
        }
    }

    #[test]
    fn secrets_stay_out_of_debug() {
        let raw =
            json!({"api": "key-a", "discord": {"token": "tok-b"}, "heartbeat": {"prompt": "p-c"}});
        let Value::Object(raw) = raw else {
            unreachable!()
        };
        let view = parse(&raw, &no_env);
        let debug = format!("{view:?}");
        for leaked in ["key-a", "tok-b", "p-c"] {
            assert!(!debug.contains(leaked), "{debug}");
        }
        let file = SettingsFile {
            path: "settings.json".into(),
            raw,
            revision: Revision::of(b""),
            exists: true,
        };
        let debug = format!("{file:?}");
        assert!(!debug.contains("key-a"), "{debug}");
    }
}
