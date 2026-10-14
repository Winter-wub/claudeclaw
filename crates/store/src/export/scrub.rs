//! The Pseudonymiser and the scrub passes (#28 D4, D5).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};

use claudeclaw_api::export::{FIELD_CLASSES, FieldClass, PseudonymKind, field_class};
use claudeclaw_api::logs::LogEntry;
use claudeclaw_api::scope::Scope;
use serde_json::{Map, Value};

/// The Daemon's fixed Run log names, kept as they are in `name` (#28 D3). These are the names
/// TS passes to `run`/`runUserMessage`/`execClaude` (`src/commands/start.ts:304,733,764`,
/// `src/commands/discord.ts:1696`, `src/commands/telegram.ts:969`, `src/commands/slack.ts:1110`,
/// `src/commands/send.ts:25`, `src/ui/server.ts:340`, `src/runner.ts:1821`); a job's runs are
/// named after the job instead.
pub(crate) const FIXED_RUN_LOG_NAMES: &[&str] = &[
    "bootstrap",
    "discord",
    "heartbeat",
    "inject",
    "prompt",
    "send",
    "slack",
    "telegram",
    "trigger",
];

/// System prefixes whose paths are kept (#28 D3), plus API route templates (`/v1/…`), which
/// are code-given, not files.
const KEPT_PATH_PREFIXES: &[&str] = &[
    "/usr/",
    "/opt/homebrew/",
    "/bin/",
    "/System/",
    "/v1/",
    "C:\\Program Files",
];

/// When one free-text value is known as several kinds, the first kind here wins (#28 D4).
const AMBIGUITY_ORDER: &[PseudonymKind] = &[
    PseudonymKind::Agent,
    PseudonymKind::Job,
    PseudonymKind::Launcher,
    PseudonymKind::Label,
    PseudonymKind::TokenName,
];

fn priority(kind: PseudonymKind) -> usize {
    AMBIGUITY_ORDER
        .iter()
        .position(|k| *k == kind)
        .unwrap_or(AMBIGUITY_ORDER.len())
}

/// Whether a kind's values are high-entropy enough for the verify pass's text scan (#28 D5.4).
/// Job, agent, Launcher and MCP names are short and collide with code-given words, so field
/// classes and the free-text pass cover them, and verify checks them only in data keys.
pub(crate) fn verified(kind: PseudonymKind) -> bool {
    !short_name(kind)
}

fn short_name(kind: PseudonymKind) -> bool {
    matches!(
        kind,
        PseudonymKind::Job | PseudonymKind::Agent | PseudonymKind::Launcher | PseudonymKind::Mcp
    )
}

/// What may sit around a known value for it to count, in the scrub and the verify pass.
///
/// Random-looking identifiers match anywhere (`resume_<session>` still hides the session);
/// only short, word-like names need word boundaries, so `claude-opus-4` doesn't match a job
/// called `opus` (#28 D5.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Boundary {
    /// Not next to `[A-Za-z0-9_-]`: names (jobs, agents, Launchers, MCP tools, labels, token
    /// names, the OS user and the hostname).
    Word,
    /// Not next to another digit, so a known snowflake or IP doesn't match inside a longer
    /// number.
    Digit,
    /// Anywhere: Session, Device and token IDs, paths, secrets and the project and home dirs.
    Anywhere,
}

impl Boundary {
    pub(crate) fn of(kind: PseudonymKind) -> Self {
        match kind {
            PseudonymKind::Session
            | PseudonymKind::Device
            | PseudonymKind::Token
            | PseudonymKind::Path
            | PseudonymKind::Daemon => Boundary::Anywhere,
            PseudonymKind::Discord | PseudonymKind::Ip => Boundary::Digit,
            _ => Boundary::Word,
        }
    }

    pub(crate) fn holds(self, before: Option<char>, after: Option<char>) -> bool {
        match self {
            Boundary::Word => !is_word(before) && !is_word(after),
            Boundary::Digit => !is_digit(before) && !is_digit(after),
            Boundary::Anywhere => true,
        }
    }
}

/// A value the verify pass looks for: lowercased, what it is, and how it must sit.
pub(crate) type Target = (String, &'static str, Boundary);

/// What the export knows before it scrubs anything (#28 D5.2): every identifier it collected,
/// the secrets, and the fixed replacements.
#[derive(Clone, Default)]
pub(crate) struct Known {
    /// Normalised value → kind, the ambiguity order applied.
    values: BTreeMap<String, PseudonymKind>,
    /// (secret, label), for `<redacted:label>`.
    secrets: Vec<(String, &'static str)>,
    project_dir: Option<String>,
    home_dir: Option<String>,
}

impl Known {
    /// Records `value` as an identifier of `kind`, ignoring kept literals.
    pub(crate) fn add(&mut self, kind: PseudonymKind, value: &str) {
        let value = normalise(kind, value);
        if value.is_empty() || kept_literal(kind, &value) {
            return;
        }
        let slot = self.values.entry(value).or_insert(kind);
        if priority(kind) < priority(*slot) {
            *slot = kind;
        }
    }

    pub(crate) fn add_secret(&mut self, secret: &str, label: &'static str) {
        if !secret.is_empty() {
            self.secrets.push((secret.to_owned(), label));
        }
    }

    pub(crate) fn set_project_dir(&mut self, dir: &str) {
        self.project_dir =
            Some(dir.trim_end_matches(['/', '\\']).to_owned()).filter(|d| !d.is_empty());
    }

    pub(crate) fn set_home_dir(&mut self, dir: &str) {
        self.home_dir =
            Some(dir.trim_end_matches(['/', '\\']).to_owned()).filter(|d| !d.is_empty());
    }

    pub(crate) fn kind_of(&self, value: &str) -> Option<PseudonymKind> {
        self.values.get(value).copied()
    }

    /// Every value the verify pass scans the text for (#28 D5.4): high-entropy identifiers,
    /// the project and home dirs, and the secrets.
    pub(crate) fn verify_targets(&self) -> Vec<Target> {
        let mut out: Vec<Target> = self
            .values
            .iter()
            .filter(|(_, kind)| verified(**kind))
            .map(|(v, kind)| (v.to_ascii_lowercase(), kind.as_str(), Boundary::of(*kind)))
            .collect();
        let anywhere = |v: &String, what| (v.to_ascii_lowercase(), what, Boundary::Anywhere);
        out.extend(self.project_dir.iter().map(|d| anywhere(d, "project-dir")));
        out.extend(self.home_dir.iter().map(|d| anywhere(d, "home-dir")));
        out.extend(self.secrets.iter().map(|(s, _)| anywhere(s, "secret")));
        out.retain(|(v, _, _)| !v.is_empty());
        out
    }

    /// The short names the verify pass checks data keys for: a key holds no field class, so a
    /// name there is a leak, not a word from code.
    pub(crate) fn key_targets(&self) -> Vec<Target> {
        self.values
            .iter()
            .filter(|(_, kind)| short_name(**kind))
            .map(|(v, kind)| (v.to_ascii_lowercase(), kind.as_str(), Boundary::Word))
            .collect()
    }

    /// The known values by lowercased first byte, longest first, for the free-text pass.
    fn by_first_byte(&self) -> HashMap<u8, Vec<(String, PseudonymKind)>> {
        let mut out: HashMap<u8, Vec<(String, PseudonymKind)>> = HashMap::new();
        for (value, kind) in &self.values {
            out.entry(value.as_bytes()[0].to_ascii_lowercase())
                .or_default()
                .push((value.clone(), *kind));
        }
        for list in out.values_mut() {
            list.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        }
        out
    }
}

/// `(kind, value)` before lookup (#28 D4): Session IDs lowercased, Launchers by file name,
/// paths without a trailing separator.
fn normalise(kind: PseudonymKind, value: &str) -> String {
    match kind {
        PseudonymKind::Session => value.to_ascii_lowercase(),
        PseudonymKind::Launcher => value.rsplit(['/', '\\']).next().unwrap_or(value).to_owned(),
        PseudonymKind::Path => value.trim_end_matches(['/', '\\']).to_owned(),
        _ => value.to_owned(),
    }
}

/// Built-in values a field of `kind` keeps (#28 D3).
fn kept_literal(kind: PseudonymKind, value: &str) -> bool {
    match kind {
        PseudonymKind::Launcher => value == "direct",
        PseudonymKind::Mcp => !value.starts_with("mcp__"),
        PseudonymKind::Ip => value
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified()),
        _ => false,
    }
}

/// Whether `name` is a field name with a Field class, so a code-given key (#28 D5.1). Any
/// other object key may be data, and goes through the free-text pass.
pub(crate) fn classified(name: &str) -> bool {
    FIELD_CLASSES
        .binary_search_by(|(field, _)| (*field).cmp(name))
        .is_ok()
}

/// One export's map from real values to `<kind>:#n` (#28 D4): numbered on first appearance,
/// per kind from 1, with no hash and no salt, so the same input gives the same file and two
/// exports of different input number differently.
#[derive(Clone)]
pub(crate) struct Scrubber {
    known: Known,
    by_first_byte: HashMap<u8, Vec<(String, PseudonymKind)>>,
    ids: HashMap<(PseudonymKind, String), u32>,
    counts: BTreeMap<&'static str, u32>,
    reverse: BTreeMap<String, String>,
    /// `<dir>/<dir>` fragments of paths under the home dir seen in free text, for the verify
    /// pass: if one survives, part of a path escaped the path scanner.
    home_fragments: BTreeSet<String>,
}

impl Scrubber {
    pub(crate) fn new(mut known: Known) -> Self {
        known.secrets.sort_by_key(|s| std::cmp::Reverse(s.0.len()));
        let by_first_byte = known.by_first_byte();
        Scrubber {
            known,
            by_first_byte,
            ids: HashMap::new(),
            counts: BTreeMap::new(),
            reverse: BTreeMap::new(),
            home_fragments: BTreeSet::new(),
        }
    }

    /// Pseudonyms assigned, by kind.
    pub(crate) fn counts(&self) -> BTreeMap<String, u32> {
        self.counts
            .iter()
            .map(|(k, n)| ((*k).to_owned(), *n))
            .collect()
    }

    /// The reverse map `{pseudonym: real value}` (#28 D4 "The key").
    pub(crate) fn reverse(&self) -> &BTreeMap<String, String> {
        &self.reverse
    }

    /// Records the `<dir>/<dir>` fragments of every path under the home dir (or `~/`) in
    /// `text`, read on past spaces (see [`home_path_end`]).
    fn note_home_paths(&mut self, text: &str) {
        let mut starts: Vec<usize> = Vec::new();
        if let Some(home) = &self.known.home_dir {
            starts.extend(
                text.match_indices(home.as_str())
                    .map(|(i, _)| i + home.len()),
            );
        }
        starts.extend(
            text.match_indices("~/")
                .filter(|(i, _)| !char_before(text, *i, None).is_some_and(is_path_char))
                .map(|(i, _)| i + 1),
        );
        // A path inside the project dir stays project-relative in the export
        // (`<project-dir>/agents/x`), so its dirs are not fragments to hunt for.
        let in_project = |start: usize| {
            let (Some(home), Some(project)) = (&self.known.home_dir, &self.known.project_dir)
            else {
                return false;
            };
            project.strip_prefix(home.as_str()).is_some_and(|rel| {
                let rest = &text[start..];
                rest.strip_prefix(rel)
                    .is_some_and(|after| after.is_empty() || after.starts_with(['/', '\\']))
            })
        };
        starts.retain(|start| !in_project(*start));
        for start in starts {
            let quote = char_before(text, start.saturating_sub(1), None)
                .filter(|c| matches!(c, '"' | '\'' | '`'));
            let rest = &text[start..];
            let tail = &rest[..home_path_end(rest, quote)];
            // Directory ranges between separators; the first is empty (the separator after
            // the home dir) and the last may run into prose, so only inner pairs count.
            let mut comps = Vec::new();
            let mut from = 0;
            for (i, c) in tail.char_indices() {
                if c == '/' || c == '\\' {
                    comps.push(from..i);
                    from = i + 1;
                }
            }
            comps.push(from..tail.len());
            for pair in comps.windows(2).take(comps.len().saturating_sub(2)) {
                let (pred, dir) = (&tail[pair[0].clone()], &tail[pair[1].clone()]);
                let word = pred.rsplit(char::is_whitespace).next().unwrap_or(pred);
                if word.is_empty() || dir.trim().is_empty() {
                    continue;
                }
                let fragment = &tail[pair[0].end - word.len()..pair[1].end];
                self.home_fragments.insert(fragment.to_ascii_lowercase());
            }
        }
    }

    /// Verify targets for every path under the home dir this scrubber saw: each
    /// `<last word of a dir>/<next dir>` pair inside it, lowercased, word-bounded. A part of
    /// such a path left in the text (say after a space the scanner stopped at) holds one.
    pub(crate) fn home_targets(&self) -> Vec<Target> {
        self.home_fragments
            .iter()
            .map(|f| (f.clone(), "path", Boundary::Word))
            .collect()
    }

    /// The pseudonym of `value` as `kind`, assigning the next number on first sight.
    pub(crate) fn pseudonym(&mut self, kind: PseudonymKind, value: &str) -> String {
        let value = normalise(kind, value);
        let key = (kind, value);
        let n = match self.ids.get(&key) {
            Some(n) => *n,
            None => {
                let count = self.counts.entry(kind.as_str()).or_insert(0);
                *count += 1;
                let n = *count;
                self.reverse
                    .insert(format!("{}:#{n}", kind.as_str()), key.1.clone());
                self.ids.insert(key, n);
                n
            }
        };
        format!("{}:#{n}", kind.as_str())
    }

    /// A `settings.json` key: a bare number is a snowflake (a channel-keyed map); anything
    /// else goes through the free-text pass, since a key can be data (#28 D1.4).
    pub(crate) fn key(&mut self, key: &str) -> String {
        if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) {
            self.pseudonym(PseudonymKind::Discord, key)
        } else {
            self.free_text(key)
        }
    }

    // ---- Field classes (#28 D5.1) ---------------------------------------------------------

    /// Scrubs `value` as the field `name` by its Field class.
    pub(crate) fn field(&mut self, name: &str, value: &Value) -> Value {
        match field_class(name) {
            FieldClass::Structural => self.structural(value),
            FieldClass::Identifier(kind) => self.identifier(kind, name, value),
            FieldClass::Scope => self.map_strings(value, Self::scope),
            FieldClass::Principal => self.map_strings(value, Self::principal),
            FieldClass::FreeText => self.free_value(value),
        }
    }

    /// Every field of an object, in order. A key with a Field class is code-given and kept;
    /// any other key goes through the free-text pass, so `{"per_job": {"<name>": 2}}` hides
    /// the name.
    pub(crate) fn object(&mut self, map: &Map<String, Value>) -> Map<String, Value> {
        map.iter()
            .map(|(k, v)| {
                let key = if classified(k) {
                    k.clone()
                } else {
                    self.free_text(k)
                };
                (key, self.field(k, v))
            })
            .collect()
    }

    /// A Daemon log line minus its cursor (#28 D1.8); a raw line's text is free text.
    pub(crate) fn log_entry(&mut self, entry: &LogEntry) -> Value {
        match entry {
            LogEntry::Line(line) => {
                let mut out = Map::new();
                out.insert("seq".into(), line.seq.into());
                out.insert("timestamp".into(), to_value(&line.timestamp));
                out.insert("level".into(), to_value(&line.level));
                out.insert("target".into(), line.target.clone().into());
                if let Some(event) = &line.event {
                    out.insert("event".into(), event.clone().into());
                }
                out.insert("message".into(), self.free_text(&line.message).into());
                out.insert("span".into(), Value::Object(self.object(&line.span)));
                out.insert("fields".into(), Value::Object(self.object(&line.fields)));
                Value::Object(out)
            }
            LogEntry::Raw(raw) => serde_json::json!({ "raw": self.free_text(&raw.raw) }),
        }
    }

    fn structural(&mut self, value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(self.object(map)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.structural(v)).collect()),
            other => other.clone(),
        }
    }

    pub(crate) fn free_value(&mut self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.free_text(s)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.free_value(v)).collect()),
            Value::Object(map) => Value::Object(self.object(map)),
            // A snowflake stored as a JSON number is still one; other numbers, floats included
            // (#28 Evidence: `0.30000000000000004`), are kept.
            Value::Number(n) => match snowflake_number(n) {
                Some(id) => Value::String(self.pseudonym(PseudonymKind::Discord, &id)),
                None => value.clone(),
            },
            other => other.clone(),
        }
    }

    fn map_strings(&mut self, value: &Value, f: fn(&mut Self, &str) -> String) -> Value {
        match value {
            Value::String(s) => Value::String(f(self, s)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|v| self.map_strings(v, f)).collect())
            }
            other => self.free_value(other),
        }
    }

    fn identifier(&mut self, kind: PseudonymKind, name: &str, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.identifier_str(kind, name, s)),
            // A bare-number snowflake from TS settings (#3 trap 9) is still a snowflake.
            Value::Number(n) if kind == PseudonymKind::Discord => {
                Value::String(self.pseudonym(kind, &n.to_string()))
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|v| self.identifier(kind, name, v))
                    .collect(),
            ),
            other => self.free_value(other),
        }
    }

    /// One identifier string; `name` decides between a job and an agent (#28 D1.7).
    pub(crate) fn identifier_str(
        &mut self,
        kind: PseudonymKind,
        name: &str,
        value: &str,
    ) -> String {
        if value.is_empty() {
            return String::new();
        }
        let kind = if name == "name" {
            if FIXED_RUN_LOG_NAMES.contains(&value) {
                return value.to_owned();
            }
            match self.known.kind_of(value) {
                Some(PseudonymKind::Agent) => PseudonymKind::Agent,
                _ => kind,
            }
        } else {
            kind
        };
        if kind == PseudonymKind::Ip {
            return self.peer(value);
        }
        if kept_literal(kind, &normalise(kind, value)) {
            return value.to_owned();
        }
        self.pseudonym(kind, value)
    }

    /// A Scope key: split, then its name pseudonymised; `global` is kept (#28 D5.1).
    pub(crate) fn scope(&mut self, value: &str) -> String {
        match value.parse::<Scope>() {
            Ok(Scope::Global) => value.to_owned(),
            Ok(Scope::Discord(id)) => self.pseudonym(PseudonymKind::Discord, &id),
            Ok(Scope::Job(name)) => self.pseudonym(PseudonymKind::Job, &name),
            Ok(Scope::Agent(name)) => self.pseudonym(PseudonymKind::Agent, &name),
            Err(_) => self.free_text(value),
        }
    }

    /// `local`, `pairing` and the other fixed principals are kept; Device and token IDs become
    /// `device:#n` and `token:#n` (#28 D5.1).
    fn principal(&mut self, value: &str) -> String {
        if let Some(id) = value.strip_prefix("device:") {
            return self.pseudonym(PseudonymKind::Device, id);
        }
        if let Some(id) = value.strip_prefix("token:") {
            return self.pseudonym(PseudonymKind::Token, id);
        }
        match value {
            "local" | "local-offline" | "pairing" => value.to_owned(),
            _ if value.starts_with("system:") => value.to_owned(),
            _ => self.free_text(value),
        }
    }

    /// A peer address, port kept.
    fn peer(&mut self, value: &str) -> String {
        if let Ok(ip) = value.parse::<IpAddr>() {
            return self.ip(ip);
        }
        if let Ok(addr) = value.parse::<SocketAddr>() {
            let ip = self.ip(addr.ip());
            return format!("{ip}:{}", addr.port());
        }
        self.free_text(value)
    }

    fn ip(&mut self, ip: IpAddr) -> String {
        if ip.is_loopback() || ip.is_unspecified() {
            ip.to_string()
        } else {
            self.pseudonym(PseudonymKind::Ip, &ip.to_string())
        }
    }

    // ---- The free-text pass (#28 D5.3) ----------------------------------------------------

    /// Secrets, then the project dir, paths, known values, snowflakes and IP literals, each on
    /// what the earlier steps left. Replaced text is never scanned again.
    pub(crate) fn free_text(&mut self, text: &str) -> String {
        self.segments(text)
            .into_iter()
            .map(Seg::into_string)
            .collect()
    }

    /// [`Scrubber::free_text`] cut to at most about `max_bytes` bytes, then ` […]`.
    ///
    /// The cut is chosen on the scrubbed text, so it never falls inside an identifier (a raw
    /// prefix would escape every scanner) or inside a Pseudonym. Only the kept part of the
    /// original is then scrubbed for real, so a value in the cut-off tail takes no number.
    pub(crate) fn free_text_capped(&mut self, text: &str, max_bytes: usize) -> String {
        let trial = self.clone().segments(text);
        let mut size = 0;
        let mut kept = 0;
        let mut cut = false;
        for seg in &trial {
            match seg {
                Seg::Text(t) => {
                    if size + t.len() <= max_bytes {
                        size += t.len();
                        kept += t.len();
                        continue;
                    }
                    let mut room = max_bytes - size;
                    while !t.is_char_boundary(room) {
                        room -= 1;
                    }
                    kept += room;
                    cut = true;
                    break;
                }
                Seg::Done(replacement, original) => {
                    if size + replacement.len() > max_bytes {
                        cut = true;
                        break;
                    }
                    size += replacement.len();
                    kept += original;
                }
            }
        }
        if !cut {
            return self.free_text(text);
        }
        format!("{} […]", self.free_text(&text[..kept]).trim_end())
    }

    fn segments(&mut self, text: &str) -> Vec<Seg> {
        if text.is_empty() {
            return Vec::new();
        }
        self.note_home_paths(text);
        let mut segs = vec![Seg::Text(text.to_owned())];
        for (secret, label) in &self.known.secrets {
            segs = replace_exact(segs, secret, &format!("<redacted:{label}>"));
        }
        if let Some(dir) = self.known.project_dir.clone() {
            segs = replace_exact(segs, &dir, PROJECT_DIR);
        }
        segs = self.pass(segs, Self::scan_path);
        segs = self.pass(segs, Self::scan_known);
        segs = self.pass(segs, Self::scan_snowflake);
        self.pass(segs, Self::scan_ip)
    }

    /// Runs one scanner over every text segment. A scanner looks at `text` (with the context
    /// around it, for boundaries) and returns the first match at or after a position as
    /// `(start, end, replacement)`.
    fn pass(&mut self, segs: Vec<Seg>, scan: Scanner) -> Vec<Seg> {
        let mut out = Vec::with_capacity(segs.len());
        let mut before: Option<char> = None;
        let mut after_project_dir = false;
        let count = segs.len();
        let mut iter = segs.into_iter().peekable();
        for _ in 0..count {
            let seg = iter.next().expect("counted");
            let after = iter.peek().and_then(Seg::first_char);
            match seg {
                Seg::Done(s, original) => {
                    before = s.chars().last().or(before);
                    after_project_dir = s == PROJECT_DIR;
                    out.push(Seg::Done(s, original));
                }
                Seg::Text(text) => {
                    let ctx = Ctx {
                        before,
                        after,
                        after_project_dir,
                    };
                    let mut from = 0;
                    let mut pos = 0;
                    while let Some((start, end, replacement)) = scan(self, &text, pos, ctx) {
                        if start > from {
                            out.push(Seg::Text(text[from..start].to_owned()));
                        }
                        out.push(Seg::Done(replacement, end - start));
                        from = end;
                        pos = end;
                    }
                    if from < text.len() {
                        out.push(Seg::Text(text[from..].to_owned()));
                    }
                    before = text.chars().last().or(before);
                    after_project_dir = false;
                }
            }
        }
        out
    }

    /// Known values, longest first, each with its kind's [`Boundary`] (#28 D5.3.2), ASCII
    /// case-insensitive: Session IDs are normalised to lowercase, and over-scrubbing a
    /// differently cased name is safe.
    fn scan_known(&mut self, text: &str, pos: usize, ctx: Ctx) -> Option<(usize, usize, String)> {
        let bytes = text.as_bytes();
        for (start, _) in text[pos..].char_indices().map(|(i, c)| (i + pos, c)) {
            let Some(candidates) = self.by_first_byte.get(&bytes[start].to_ascii_lowercase())
            else {
                continue;
            };
            let found = candidates.iter().find(|(value, kind)| {
                let end = start + value.len();
                text.get(start..end)
                    .is_some_and(|t| t.eq_ignore_ascii_case(value))
                    && Boundary::of(*kind).holds(
                        char_before(text, start, ctx.before),
                        char_after(text, end, ctx.after),
                    )
            });
            if let Some((value, kind)) = found.cloned() {
                let p = self.pseudonym(kind, &value);
                return Some((start, start + value.len(), p));
            }
        }
        None
    }

    /// Runs of 17–20 digits inside a string, except the parts of a decimal number (#28
    /// D5.3.3).
    fn scan_snowflake(
        &mut self,
        text: &str,
        pos: usize,
        ctx: Ctx,
    ) -> Option<(usize, usize, String)> {
        let (start, end) = digit_runs(text, pos, ctx.before, ctx.after).next()?;
        let p = self.pseudonym(PseudonymKind::Discord, &text[start..end]);
        Some((start, end, p))
    }

    /// Absolute, `~/…` and drive paths outside the project dir and the system prefixes, the
    /// whole path as one `path:#n` (#28 D3); the home dir alone becomes `~`.
    fn scan_path(&mut self, text: &str, pos: usize, ctx: Ctx) -> Option<(usize, usize, String)> {
        let mut search = pos;
        while let Some((start, end)) = next_path(text, search, ctx) {
            let path = &text[start..end];
            search = end;
            if KEPT_PATH_PREFIXES.iter().any(|p| path.starts_with(p)) {
                continue;
            }
            let trimmed = path.trim_end_matches(['/', '\\']);
            if self.known.home_dir.as_deref() == Some(trimmed) || trimmed == "~" {
                return Some((start, end, "~".into()));
            }
            let p = self.pseudonym(PseudonymKind::Path, path);
            return Some((start, end, p));
        }
        None
    }

    /// Tokens that parse as an IP address (with an optional port), except loopback and
    /// unspecified (#28 D5.3.5).
    fn scan_ip(&mut self, text: &str, pos: usize, ctx: Ctx) -> Option<(usize, usize, String)> {
        let bytes = text.as_bytes();
        let mut i = pos;
        while i < bytes.len() {
            if !is_ip_char(bytes[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && is_ip_char(bytes[i]) {
                i += 1;
            }
            let mut end = i;
            while end > start && matches!(bytes[end - 1], b'.' | b':') {
                end -= 1;
            }
            let token = &text[start..end];
            let bounded = !char_before(text, start, ctx.before)
                .is_some_and(|c| c.is_alphanumeric())
                && !char_after(text, i, ctx.after).is_some_and(|c| c.is_alphanumeric());
            if !bounded || !(token.contains('.') || token.contains(':')) {
                continue;
            }
            if let Ok(ip) = token.parse::<IpAddr>() {
                if ip.is_loopback() || ip.is_unspecified() {
                    continue;
                }
                let p = self.ip(ip);
                return Some((start, end, p));
            }
            if let Some((host, port)) = token.rsplit_once(':')
                && port.bytes().all(|b| b.is_ascii_digit())
                && let Ok(ip) = host.parse::<IpAddr>()
                && !(ip.is_loopback() || ip.is_unspecified())
            {
                let p = self.ip(ip);
                return Some((start, start + host.len(), p));
            }
        }
        None
    }
}

const PROJECT_DIR: &str = "<project-dir>";

/// What a scanner sees around a text segment.
#[derive(Clone, Copy)]
struct Ctx {
    /// The character before the segment, if any.
    before: Option<char>,
    /// The character after it.
    after: Option<char>,
    /// The segment follows `<project-dir>`, so a leading `/…` is project-relative.
    after_project_dir: bool,
}

type Scanner = fn(&mut Scrubber, &str, usize, Ctx) -> Option<(usize, usize, String)>;

/// A piece of a free-text value: still to scan, or already replaced (with the length of the
/// original text it replaced).
enum Seg {
    Text(String),
    Done(String, usize),
}

impl Seg {
    fn first_char(&self) -> Option<char> {
        match self {
            Seg::Text(s) | Seg::Done(s, _) => s.chars().next(),
        }
    }

    fn into_string(self) -> String {
        match self {
            Seg::Text(s) | Seg::Done(s, _) => s,
        }
    }
}

fn replace_exact(segs: Vec<Seg>, needle: &str, replacement: &str) -> Vec<Seg> {
    let mut out = Vec::with_capacity(segs.len());
    for seg in segs {
        match seg {
            Seg::Text(text) if text.contains(needle) => {
                let mut rest = text.as_str();
                while let Some(i) = rest.find(needle) {
                    if i > 0 {
                        out.push(Seg::Text(rest[..i].to_owned()));
                    }
                    out.push(Seg::Done(replacement.to_owned(), needle.len()));
                    rest = &rest[i + needle.len()..];
                }
                if !rest.is_empty() {
                    out.push(Seg::Text(rest.to_owned()));
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// `[A-Za-z0-9_-]`: the characters a word-bounded value must not touch (#28 D5.3.2).
pub(crate) fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_digit(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_digit())
}

fn is_ip_char(b: u8) -> bool {
    b.is_ascii_hexdigit() || b == b'.' || b == b':'
}

pub(crate) fn char_before(text: &str, i: usize, outside: Option<char>) -> Option<char> {
    if i == 0 {
        outside
    } else {
        text[..i].chars().next_back()
    }
}

pub(crate) fn char_after(text: &str, i: usize, outside: Option<char>) -> Option<char> {
    if i >= text.len() {
        outside
    } else {
        text[i..].chars().next()
    }
}

/// Maximal runs of 17–20 ASCII digits in `text` from `pos`. A run is skipped only when it is
/// part of a decimal number (`0.300…04`, `…04.5`): a full stop after an ID is not a decimal
/// point, so `user <id>.` still counts (#28 D5.3.3, D5.4).
pub(crate) fn digit_runs<'a>(
    text: &'a str,
    pos: usize,
    before: Option<char>,
    after: Option<char>,
) -> impl Iterator<Item = (usize, usize)> + 'a {
    let bytes = text.as_bytes();
    let mut i = pos;
    // A run that started before `pos` is not a new run.
    while i > 0 && i < bytes.len() && bytes[i].is_ascii_digit() && bytes[i - 1].is_ascii_digit() {
        i += 1;
    }
    std::iter::from_fn(move || {
        while i < bytes.len() {
            if !bytes[i].is_ascii_digit() {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let len = i - start;
            // Digit, dot, then this run: a fraction.
            let fraction =
                start >= 2 && bytes[start - 1] == b'.' && bytes[start - 2].is_ascii_digit();
            // This run, dot, then a digit: an integer part.
            let integer =
                bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
            let digit_edge =
                (start == 0 && is_digit(before)) || (i == bytes.len() && is_digit(after));
            if (17..=20).contains(&len) && !fraction && !integer && !digit_edge {
                return Some((start, i));
            }
        }
        None
    })
}

/// Characters that continue a path or a relative path's name, so a `/` after one doesn't
/// start an absolute path (`a/b`, `./x`, `~/x` is handled on its own).
fn is_path_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '.' | '_' | '-' | '~' | '/' | '\\' | '@' | '+' | '%' | '$'
        )
}

/// The next absolute path in `text` from `pos`: `/…`, `~/…`, `X:\…` or `X:/…`, starting
/// after any character that can't be part of a path (whitespace, quotes, `:`, `=`, `(`, …),
/// and ending at whitespace, a quote or closing punctuation, with a trailing `.` or `:` left
/// out. In a `file:///…` URL the path starts at the last slash; `//host` (a URL authority) is
/// not a path.
fn next_path(text: &str, pos: usize, ctx: Ctx) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut i = pos;
    while i < bytes.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let continues = i == 0 && ctx.after_project_dir;
        let opens = !(continues || char_before(text, i, ctx.before).is_some_and(is_path_char));
        if !opens {
            i += 1;
            continue;
        }
        let start = match bytes[i] {
            b'/' => {
                let run = bytes[i..].iter().take_while(|b| **b == b'/').count();
                let last = i + run - 1;
                match run {
                    2 => None,
                    _ if run > 2 && is_drive(bytes, last + 1) => Some(last + 1),
                    _ => bytes
                        .get(last + 1)
                        .is_some_and(|b| !is_path_end(*b))
                        .then_some(last),
                }
            }
            b'~' => (bytes.get(i + 1) == Some(&b'/')).then_some(i),
            _ if is_drive(bytes, i) => Some(i),
            _ => None,
        };
        let Some(start) = start else {
            i += 1;
            continue;
        };
        // A quoted path runs to its closing quote, spaces and all.
        let quote = char_before(text, start, ctx.before).filter(|c| matches!(c, '"' | '\'' | '`'));
        let quoted = quote.and_then(|q| {
            let close = start + text[start..].find(q)?;
            (!text[start..close].contains('\n')).then_some(close)
        });
        let mut end = quoted.unwrap_or_else(|| path_extent(bytes, start));
        while end > start + 1 && matches!(bytes[end - 1], b'.' | b':') {
            end -= 1;
        }
        return Some((start, end));
    }
    None
}

/// Where an unquoted path from `start` ends: at whitespace, a quote or closing punctuation,
/// unless the text after the spaces continues it, i.e. its next token holds a `/` or `\`
/// (`~/Mobile Documents/x`, `~/My Drive/x`).
fn path_extent(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    loop {
        while i < bytes.len() && !is_path_end(bytes[i]) {
            i += 1;
        }
        let mut next = i;
        while next < bytes.len() && matches!(bytes[next], b' ' | b'\t') {
            next += 1;
        }
        if next == i {
            return i;
        }
        let mut token = next;
        while token < bytes.len() && !is_path_end(bytes[token]) {
            token += 1;
        }
        if !bytes[next..token].iter().any(|b| matches!(b, b'/' | b'\\')) {
            return i;
        }
        i = token;
    }
}

/// Where a path under the home dir, read for verify fragments, ends in `rest`: at the closing
/// quote of a quoted path, a newline, `;`, `|`, `<` or `>`, or at the first space of a dir name
/// that holds a second one. Dir names like `Mobile Documents` and `My Drive` keep their one
/// space; a run of prose words is not a dir name.
fn home_path_end(rest: &str, quote: Option<char>) -> usize {
    let mut first_space = None;
    let mut spaces = 0;
    for (i, c) in rest.char_indices() {
        if Some(c) == quote || matches!(c, '\n' | ';' | '|' | '<' | '>') {
            return i;
        }
        if c == '/' || c == '\\' {
            first_space = None;
            spaces = 0;
        } else if c.is_whitespace() {
            spaces += 1;
            first_space.get_or_insert(i);
            if spaces > 1 {
                return first_space.unwrap_or(i);
            }
        }
    }
    rest.len()
}

/// A 17–20 digit integer, as a decimal string: a snowflake stored as a JSON number.
pub(crate) fn snowflake_number(n: &serde_json::Number) -> Option<String> {
    let digits = n
        .as_u64()
        .map(|v| v.to_string())
        .or_else(|| n.as_i64().map(|v| v.unsigned_abs().to_string()))?;
    (17..=20).contains(&digits.len()).then_some(digits)
}

/// `X:\` or `X:/` at `i`.
fn is_drive(bytes: &[u8], i: usize) -> bool {
    bytes.get(i).is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(i + 1) == Some(&b':')
        && matches!(bytes.get(i + 2), Some(b'\\' | b'/'))
}

fn is_path_end(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            b'"' | b'\'' | b'`' | b'<' | b'>' | b'|' | b',' | b';' | b')' | b']' | b'}'
        )
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("wire types serialise")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A fake snowflake of `n` repeated digits, built at runtime so no long digit run sits in
    /// the source (gitleaks).
    fn flake(d: char, len: usize) -> String {
        std::iter::repeat_n(d, len).collect()
    }

    fn scrubber(f: impl FnOnce(&mut Known)) -> Scrubber {
        let mut known = Known::default();
        f(&mut known);
        Scrubber::new(known)
    }

    #[test]
    fn numbers_follow_first_appearance_per_kind() {
        let mut s = scrubber(|_| ());
        assert_eq!(s.pseudonym(PseudonymKind::Job, "b"), "job:#1");
        assert_eq!(s.pseudonym(PseudonymKind::Job, "a"), "job:#2");
        assert_eq!(s.pseudonym(PseudonymKind::Agent, "a"), "agent:#1");
        assert_eq!(s.pseudonym(PseudonymKind::Job, "b"), "job:#1");
        assert_eq!(s.pseudonym(PseudonymKind::Session, "ABC"), "session:#1");
        assert_eq!(s.pseudonym(PseudonymKind::Session, "abc"), "session:#1");
        assert_eq!(
            s.pseudonym(PseudonymKind::Launcher, "/x/y/run.sh"),
            "launcher:#1"
        );
        assert_eq!(
            s.pseudonym(PseudonymKind::Launcher, "run.sh"),
            "launcher:#1"
        );
        assert_eq!(s.counts()["job"], 2);
        assert_eq!(s.reverse()["job:#2"], "a");
    }

    #[test]
    fn a_scope_and_a_bare_field_share_a_pseudonym() {
        let id = flake('7', 18);
        let mut s = scrubber(|_| ());
        assert_eq!(
            s.field("scope", &json!(format!("discord:{id}"))),
            "discord:#1"
        );
        assert_eq!(s.field("channel_id", &json!(id)), "discord:#1");
        assert_eq!(s.field("scope", &json!("job:daily")), "job:#1");
        assert_eq!(s.field("job", &json!("daily")), "job:#1");
        assert_eq!(s.field("scope", &json!("global")), "global");
        assert_eq!(s.field("principal", &json!("device:dev1")), "device:#1");
        assert_eq!(s.field("principal", &json!("token:tok1")), "token:#1");
        assert_eq!(s.field("principal", &json!("local")), "local");
        assert_eq!(s.field("launcher", &json!("direct")), "direct");
        assert_eq!(s.field("tool", &json!("Bash")), "Bash");
        assert_eq!(s.field("tool", &json!("mcp__corp__search")), "mcp:#1");
        assert_eq!(s.field("peer", &json!("127.0.0.1")), "127.0.0.1");
        assert_eq!(s.field("peer", &json!("192.0.2.7:4443")), "ip:#1:4443");
        assert_eq!(s.field("name", &json!("heartbeat")), "heartbeat");
        assert_eq!(s.field("name", &json!("daily")), "job:#1");
        assert_eq!(s.field("attempt", &json!(2)), 2);
    }

    #[test]
    fn a_bare_number_snowflake_is_pseudonymised_but_numbers_are_never_scanned() {
        let mut s = scrubber(|_| ());
        let n: Value = serde_json::from_str(&flake('5', 18)).unwrap();
        assert_eq!(s.field("user_id", &n), "discord:#1");
        // #28 Evidence: a float whose digits look like a snowflake.
        let cost = json!(0.30000000000000004);
        assert_eq!(s.field("cost_usd", &cost), cost);
        assert_eq!(s.field("message", &cost), cost);
        assert_eq!(
            s.free_text("cost 0.30000000000000004"),
            "cost 0.30000000000000004"
        );
    }

    #[test]
    fn known_values_match_only_at_word_boundaries() {
        let mut s = scrubber(|k| {
            k.add(PseudonymKind::Job, "opus");
            k.add(PseudonymKind::Agent, "researcher");
        });
        assert_eq!(
            s.free_text("agents/researcher/jobs/opus.md and opus."),
            "agents/agent:#1/jobs/job:#1.md and job:#1."
        );
        assert_eq!(
            s.free_text("claude-opus-4 opus_x xopus"),
            "claude-opus-4 opus_x xopus"
        );
    }

    #[test]
    fn ambiguity_prefers_agent_then_job() {
        let mut s = scrubber(|k| {
            k.add(PseudonymKind::Job, "zed");
            k.add(PseudonymKind::Agent, "zed");
        });
        assert_eq!(s.free_text("zed failed"), "agent:#1 failed");
    }

    #[test]
    fn replaced_text_is_not_scanned_again() {
        let mut s = scrubber(|k| {
            k.add(PseudonymKind::Job, "job");
            k.add(PseudonymKind::Job, "run");
        });
        assert_eq!(s.free_text("run started"), "job:#1 started");
        assert_eq!(s.free_text("job run"), "job:#2 job:#1");
    }

    #[test]
    fn snowflakes_in_strings_but_not_next_to_a_dot() {
        let id = flake('4', 19);
        let mut s = scrubber(|_| ());
        assert_eq!(
            s.free_text(&format!("POST channels/{id}/messages: 403")),
            "POST channels/discord:#1/messages: 403"
        );
        let short = flake('4', 16);
        let long = flake('4', 21);
        let text = format!("{short} {long} 0.{id} {id}.5");
        assert_eq!(s.free_text(&text), text);
    }

    #[test]
    fn secrets_are_redacted_before_anything_else() {
        let mut s = scrubber(|k| {
            k.add_secret("CANARY-TOKEN-1", "discord-token");
            k.add(PseudonymKind::Job, "CANARY");
        });
        assert_eq!(
            s.free_text("token=CANARY-TOKEN-1 CANARY"),
            "token=<redacted:discord-token> job:#1"
        );
    }

    #[test]
    fn paths_project_dir_and_home() {
        let mut s = scrubber(|k| {
            k.set_project_dir("/home/claw/proj");
            k.set_home_dir("/home/claw");
            k.add(PseudonymKind::OsUser, "claw");
        });
        assert_eq!(
            s.free_text("open /home/claw/proj/jobs/a.md: denied"),
            "open <project-dir>/jobs/a.md: denied"
        );
        assert_eq!(s.free_text("in /home/claw/notes/x.txt."), "in path:#1.");
        assert_eq!(s.free_text("in ~/notes/x.txt"), "in path:#2");
        assert_eq!(s.free_text("home=/home/claw"), "home=~");
        assert_eq!(
            s.free_text("ran /usr/bin/env and /v1/status"),
            "ran /usr/bin/env and /v1/status"
        );
        assert_eq!(s.free_text("user claw"), "user os-user:#1");
        assert_eq!(s.free_text("a/b and 1/2"), "a/b and 1/2");
        assert_eq!(s.free_text(r"at C:\Users\owner\x"), "at path:#3");
    }

    #[test]
    fn ip_literals_except_loopback_and_unspecified() {
        let mut s = scrubber(|_| ());
        assert_eq!(
            s.free_text("from 192.0.2.7:443 and 2001:db8::1, not 127.0.0.1 or ::"),
            "from ip:#1:443 and ip:#2, not 127.0.0.1 or ::"
        );
        assert_eq!(s.free_text("at 08:30:27 v1.2"), "at 08:30:27 v1.2");
    }

    #[test]
    fn log_entries_drop_the_cursor_and_scrub_by_class() {
        let mut s = scrubber(|k| k.add(PseudonymKind::Job, "daily"));
        let entry = crate::logs::parse_line(
            r#"{"seq":1,"timestamp":"2026-10-13T08:30:27Z","level":"INFO","message":"daily done","event":"run.finish","target":"t","span":{"scope":"job:daily","run_id":"r1"},"session_id":"S1","unknown_field":"daily"}"#,
            Some("2026-10-13:0".into()),
        );
        assert_eq!(
            s.log_entry(&entry),
            json!({
                "seq": 1, "timestamp": "2026-10-13T08:30:27Z", "level": "INFO", "target": "t",
                "event": "run.finish", "message": "job:#1 done",
                "span": {"scope": "job:#1", "run_id": "r1"},
                "fields": {"session_id": "session:#1", "unknown_field": "job:#1"},
            })
        );
        let raw = crate::logs::parse_line("panic in daily", Some("c".into()));
        assert_eq!(s.log_entry(&raw), json!({"raw": "panic in job:#1"}));
    }

    #[test]
    fn a_snowflake_before_a_full_stop_is_caught_but_decimals_are_not() {
        let id = flake('3', 18);
        let mut s = scrubber(|_| ());
        assert_eq!(
            s.free_text(&format!("Unknown user {id}.")),
            "Unknown user discord:#1."
        );
        assert_eq!(s.free_text(&format!("x.{id} end")), "x.discord:#1 end");
        let decimals = format!("0.{id} and {id}.5");
        assert_eq!(s.free_text(&decimals), decimals);
    }

    #[test]
    fn random_looking_ids_match_anywhere_and_names_need_word_boundaries() {
        let id = flake('6', 18);
        let mut s = scrubber(|k| {
            k.add(PseudonymKind::Session, "5E55-AB12");
            k.add(PseudonymKind::Device, "dev7f3a");
            k.add(PseudonymKind::Discord, &id);
            k.add(PseudonymKind::Job, "opus");
        });
        assert_eq!(
            s.free_text("resume_5e55-ab12 failed on xdev7f3ay"),
            "resume_session:#1 failed on xdevice:#1y"
        );
        assert_eq!(s.free_text(&format!("chan_{id}_x")), "chan_discord:#1_x");
        // 21 digits: neither a snowflake run nor the known one.
        let longer = format!("111{id}");
        assert_eq!(s.free_text(&longer), longer, "not inside a longer number");
        assert_eq!(s.free_text("claude-opus-4 opus"), "claude-opus-4 job:#1");
    }

    #[test]
    fn unclassified_object_keys_are_free_text() {
        let mut s = scrubber(|k| {
            k.add(PseudonymKind::Job, "payroll-sync");
            k.add(PseudonymKind::Job, "job");
        });
        let value = json!({"per_job": {"payroll-sync": 2}, "job": "job"});
        assert_eq!(
            Value::Object(s.object(value.as_object().unwrap())),
            json!({"per_job": {"job:#1": 2}, "job": "job:#2"}),
            "a key with a Field class (`job`) is code-given and kept"
        );
        assert_eq!(s.key("payroll-sync"), "job:#1");
        assert_eq!(s.key("model"), "model");
    }

    #[test]
    fn a_capped_text_is_cut_after_scrubbing_and_never_inside_a_pseudonym() {
        let id = flake('8', 18);
        let mut s = scrubber(|k| k.add(PseudonymKind::Job, "tail-job"));
        // A raw cut at 20 chars would leave half the snowflake.
        let text = format!("lost channel {id} then tail-job");
        let capped = s.free_text_capped(&text, 20);
        assert_eq!(capped, "lost channel […]");
        assert!(s.reverse().is_empty(), "nothing cut off takes a number");
        let capped = s.free_text_capped(&text, 30);
        assert_eq!(capped, "lost channel discord:#1 then […]");
        assert_eq!(s.counts().get("job"), None);
        assert_eq!(s.free_text_capped("short", 30), "short");
    }

    #[test]
    fn paths_after_any_non_path_character_and_in_urls() {
        let mut s = scrubber(|k| k.add(PseudonymKind::OsUser, "claw"));
        assert_eq!(
            s.free_text("see file:///home/claw/clients/acme-corp/plan.md now"),
            "see file://path:#1 now"
        );
        assert_eq!(
            s.free_text("path:/home/claw/clients/acme-corp/plan.md"),
            "path:path:#1"
        );
        assert_eq!(
            s.free_text(r"file:///C:/home/claw/plan.md and drive=C:\Users\claw\x"),
            "file:///path:#2 and drive=path:#3"
        );
        assert_eq!(
            s.free_text("https://discord.com/api/x and a/b"),
            "https://discord.com/api/x and a/b"
        );
    }

    #[test]
    fn a_path_with_spaces_or_in_quotes_is_one_path() {
        let mut s = scrubber(|k| k.set_home_dir("/home/claw"));
        assert_eq!(
            s.free_text("open /home/claw/Mobile Documents/AcmeCorp/plan.md was denied"),
            "open path:#1 was denied"
        );
        assert_eq!(
            s.free_text("in ~/My Drive/Clients/x.md, then stop"),
            "in path:#2, then stop"
        );
        assert_eq!(
            s.free_text("read '/home/claw/Shared Stuff/notes': no"),
            "read 'path:#3': no"
        );
        assert_eq!(
            s.free_text("moved /home/claw/a to the bin"),
            "moved path:#4 to the bin",
            "a following word with no separator is prose"
        );
    }

    #[test]
    fn home_path_fragments_become_verify_targets() {
        let mut s = scrubber(|k| k.set_home_dir("/home/claw"));
        s.free_text("open /home/claw/Mobile Documents/AcmeCorp/plan.md was denied");
        s.free_text("\"~/.local/bin/claude\" ran");
        // Prose after a path is not part of it, so `agents/zed` here is no target.
        s.free_text("~/a/b failed for agents/zed/jobs/y.md");
        let targets: Vec<String> = s.home_targets().into_iter().map(|t| t.0).collect();
        assert_eq!(targets, [".local/bin", "documents/acmecorp"]);
    }

    #[test]
    fn snowflakes_stored_as_numbers_are_pseudonymised_outside_structural_fields() {
        let mut s = scrubber(|_| ());
        let n: Value = serde_json::from_str(&flake('7', 18)).unwrap();
        let value = json!({"thread_id": n, "extra": [n], "duration_ms": n, "count": 12});
        assert_eq!(
            Value::Object(s.object(value.as_object().unwrap())),
            json!({
                "thread_id": "discord:#1",
                "extra": ["discord:#1"],
                "duration_ms": n,
                "count": 12,
            })
        );
        let float = json!(0.30000000000000004);
        assert_eq!(s.field("message", &float), float);
    }
}
