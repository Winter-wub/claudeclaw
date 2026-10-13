//! Jobs (management-api §2.4): the job list and record, the job editor's create, patch and raw
//! bodies, the server-side schedule preview, and the CLI's `jobs check` / `jobs audit` results
//! (#12, #17 §5, #22, #46, #50).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::openapi::schema::{ObjectBuilder, OneOfBuilder, Schema, Type};
use utoipa::openapi::{Ref, RefOr};
use utoipa::{PartialSchema, ToSchema};

use crate::revision::Revision;
use crate::runs::{RunIndexState, RunOutcome};
use crate::wire::wire_enum;

/// `jobs.list` → every job, from the Change scan's snapshot (management-api §2.4, #46 D5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobList {
    pub jobs: Vec<JobSummary>,
}

/// One job as `jobs.list` and #22's `jobs list` show it (management-api §2.4). Content-free:
/// no prompt and no file text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobSummary {
    /// The file stem, or `<agent>/<stem>` for an agent's job; fixed at creation.
    pub name: String,
    #[schema(required = true)]
    pub agent: Option<String>,
    /// The job file, relative to the project dir.
    pub path: String,
    pub status: JobStatus,
    /// `null` for an unscheduled job (#12 §2): listed and runnable by hand.
    #[schema(required = true)]
    pub schedule: Option<String>,
    /// `null` when the file doesn't say, which is only valid without a `schedule` (#12 §3).
    #[schema(required = true)]
    pub recurring: Option<bool>,
    /// The effective value: `true` when the key is absent (#12 §2).
    pub enabled: bool,
    /// The effective value: `true` when the key is absent (#12 §2).
    pub notify: JobNotify,
    /// The job's own `launcher:` override (`direct` or a path); `null` when it inherits.
    #[schema(required = true)]
    pub launcher: Option<String>,
    #[schema(required = true)]
    pub model: Option<String>,
    /// The next scheduled fire; `null` when unscheduled, disabled or invalid.
    #[schema(required = true)]
    pub next_at: Option<DateTime<Utc>>,
    /// `null` when the job has never run.
    #[schema(required = true)]
    pub last_run: Option<JobLastRun>,
    /// `null` when read from disk with no Daemon running (#22).
    #[schema(required = true)]
    pub in_flight: Option<bool>,
    /// When a failed run is retried; `null` when no retry is pending.
    #[schema(required = true)]
    pub retry_at: Option<DateTime<Utc>>,
    /// Sent back as `If-Match` on `jobs.update`, `jobs.replace` and `jobs.delete` (#46 D5).
    pub revision: Revision,
}

wire_enum! {
    /// Whether a job loads (#12 §4, #22 `jobs list`).
    pub enum JobStatus {
        Valid = "valid",
        /// Not scheduled; `diagnostics` say why.
        Invalid = "invalid",
        /// Scheduled, with a near-miss key or a 1.x divergence (#12 §4).
        Warning = "warning",
    }
}

/// A job's latest run (#22 `jobs list`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobLastRun {
    pub at: DateTime<Utc>,
    pub outcome: RunOutcome,
}

/// `jobs.get`, and the result of every job write: [`JobSummary`] plus the job's text, parsed
/// keys and findings, read fresh from disk (management-api §2.4, #46 D5).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobDetail {
    #[serde(flatten)]
    pub summary: JobSummary,
    pub fields: JobFields,
    /// The body after the frontmatter.
    pub prompt: String,
    /// The whole file, unknown keys and comments included.
    pub raw: String,
    pub diagnostics: Vec<JobDiagnostic>,
    /// #17 §5's findings on this job, from the last Job import.
    pub import_findings: Vec<ImportFinding>,
    /// The next 5 fires; empty when unscheduled, disabled or invalid.
    pub next_fires: Vec<DateTime<Utc>>,
}

impl fmt::Debug for JobDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobDetail")
            .field("summary", &self.summary)
            .field("fields", &self.fields)
            .field("prompt_bytes", &self.prompt.len())
            .field("raw_bytes", &self.raw.len())
            .field("diagnostics", &self.diagnostics)
            .field("import_findings", &self.import_findings)
            .field("next_fires", &self.next_fires)
            .finish()
    }
}

/// The frontmatter keys the Daemon parses, each typed and optional (management-api §2.4, #12
/// §2). Absent means the key isn't in the file. Unknown keys stay in `raw` and are never dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobFields {
    /// 5 cron fields or a croner nickname such as `@daily` (#12 §1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Required whenever `schedule` is set (#12 §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<JobNotify>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Retry attempts after a failure before waiting for the next slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<u32>,
    /// Seconds between retry attempts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_delay: Option<u64>,
    /// `direct` or a Launcher path; the job's Launcher override (#8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
}

/// A job's `notify:` value (#12 §2): `true`, `false`, or `"error"` for failures only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum JobNotify {
    Bool(bool),
    Only(NotifyOnly),
}

wire_enum! {
    /// The string values of [`JobNotify`] (#12 §2).
    pub enum NotifyOnly {
        /// Notify only when the run fails.
        Error = "error",
    }
}

/// `jobs.create`'s body (management-api §2.4): the job as the form's fields and a prompt
/// (`{name, agent?, fields, prompt}`), or as the whole file (`{name, agent?, raw}`).
///
/// On the wire the two forms are told apart by which keys are present, not by a tag: a body
/// with `raw` is the raw form, and `raw` can't be combined with `fields` or `prompt`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "flat::JobCreate", into = "flat::JobCreate")]
pub struct JobCreate {
    /// The file stem; fixed at creation.
    pub name: String,
    /// Create the job under this agent's `jobs/` dir.
    pub agent: Option<String>,
    pub content: JobContent,
}

/// What [`JobCreate`] creates the job from.
#[derive(Clone, PartialEq)]
pub enum JobContent {
    /// The job editor's form. The Daemon writes the file, always with `recurring:` (#12 §3).
    Form { fields: JobFields, prompt: String },
    /// The raw editor's text, validated as it is.
    Raw { raw: String },
}

impl fmt::Debug for JobCreate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobCreate")
            .field("name", &self.name)
            .field("agent", &self.agent)
            .field("content", &self.content)
            .finish()
    }
}

impl fmt::Debug for JobContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobContent::Form { fields, prompt } => f
                .debug_struct("Form")
                .field("fields", fields)
                .field("prompt_bytes", &prompt.len())
                .finish(),
            JobContent::Raw { raw } => f
                .debug_struct("Raw")
                .field("raw_bytes", &raw.len())
                .finish(),
        }
    }
}

impl TryFrom<flat::JobCreate> for JobCreate {
    type Error = &'static str;

    fn try_from(body: flat::JobCreate) -> Result<Self, Self::Error> {
        let content = match (body.raw, body.fields, body.prompt) {
            (Some(raw), None, None) => JobContent::Raw { raw },
            (None, Some(fields), Some(prompt)) => JobContent::Form { fields, prompt },
            (Some(_), _, _) => return Err("`raw` can't be combined with `fields` or `prompt`"),
            (None, _, _) => return Err("give `fields` and `prompt`, or `raw`"),
        };
        Ok(JobCreate {
            name: body.name,
            agent: body.agent,
            content,
        })
    }
}

impl From<JobCreate> for flat::JobCreate {
    fn from(create: JobCreate) -> Self {
        let (fields, prompt, raw) = match create.content {
            JobContent::Form { fields, prompt } => (Some(fields), Some(prompt), None),
            JobContent::Raw { raw } => (None, None, Some(raw)),
        };
        flat::JobCreate {
            name: create.name,
            agent: create.agent,
            fields,
            prompt,
            raw,
        }
    }
}

/// `jobs.update`'s body: line edits to the frontmatter, validated before anything is written
/// (management-api §2.4, #12 §2). Each `set` key is a frontmatter key; a value replaces the
/// key's line or inserts one, and `null` removes it. `prompt` replaces the body.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobPatch {
    #[schema(schema_with = patch_set_schema)]
    pub set: BTreeMap<String, Option<JobValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

impl fmt::Debug for JobPatch {
    /// Only the key names and the prompt's size.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobPatch")
            .field("set", &self.set.keys().collect::<Vec<_>>())
            .field("prompt_bytes", &self.prompt.as_ref().map(String::len))
            .finish()
    }
}

/// `JobPatch.set`: an object of [`JobValue`] or `null`. utoipa can't derive a map with nullable
/// values.
fn patch_set_schema() -> Schema {
    let value = OneOfBuilder::new()
        .item(Ref::from_schema_name("JobValue"))
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .build();
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .additional_properties(Some(Schema::OneOf(value)))
        .into()
}

/// One frontmatter value in a [`JobPatch`] (#12 §2's value grammar decides which key takes
/// which).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum JobValue {
    Bool(bool),
    Integer(u64),
    String(String),
}

/// `jobs.replace`'s body: the whole file, sent with a required `If-Match` (management-api
/// §2.4).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobRaw {
    pub raw: String,
}

impl fmt::Debug for JobRaw {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobRaw")
            .field("raw_bytes", &self.raw.len())
            .finish()
    }
}

/// `jobs.delete` → what went with the file (management-api §2.4, #12 §6). Deleting through the
/// API removes the job's sessions and per-job state at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobDeleted {
    pub deleted: bool,
    pub sessions_removed: u32,
    pub state_removed: bool,
}

/// One validation finding on a job file (management-api §2.4, architecture §4
/// `claudeclaw-jobs`, #12 §4).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobDiagnostic {
    pub severity: DiagnosticSeverity,
    pub kind: JobDiagnosticKind,
    /// 1-based; `null` for a finding on the whole file.
    #[schema(required = true)]
    pub line: Option<u32>,
    /// 1-based; `null` for a finding on the whole file or line.
    #[schema(required = true)]
    pub column: Option<u32>,
    /// Can quote a line of the job file, so the Bug-report export drops it (#28).
    pub message: String,
}

impl fmt::Debug for JobDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobDiagnostic")
            .field("severity", &self.severity)
            .field("kind", &self.kind)
            .field("line", &self.line)
            .field("column", &self.column)
            .field("message_bytes", &self.message.len())
            .finish()
    }
}

wire_enum! {
    /// How bad a [`JobDiagnostic`] is (#12 §4).
    pub enum DiagnosticSeverity {
        /// The job is invalid and not scheduled.
        Error = "error",
        /// The job runs; shown on the Jobs screen.
        Warning = "warning",
    }
}

/// A [`JobDiagnostic`]'s stable kind, such as `schedule.invalid` or `key.unknown` (#28).
///
/// A validated string, not a closed enum: the kinds belong to `claudeclaw-jobs` and its `Lint`
/// hooks (architecture §4), and the spec names only examples. The grammar is dotted
/// `snake_case` segments, at most 64 bytes, so a kind can't carry job content and the
/// Bug-report export can keep it while dropping the message (#28).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobDiagnosticKind(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid diagnostic kind; expected dotted snake_case segments, at most 64 bytes")]
pub struct InvalidDiagnosticKind;

impl JobDiagnosticKind {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for JobDiagnosticKind {
    type Err = InvalidDiagnosticKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let segment_ok = |seg: &str| {
            seg.starts_with(|c: char| c.is_ascii_lowercase())
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        if s.len() > 64 || !s.split('.').all(segment_ok) {
            return Err(InvalidDiagnosticKind);
        }
        Ok(JobDiagnosticKind(s.to_owned()))
    }
}

impl fmt::Display for JobDiagnosticKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for JobDiagnosticKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for JobDiagnosticKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl PartialSchema for JobDiagnosticKind {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .pattern(Some(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$"))
            .max_length(Some(64))
            .description(Some(
                "A stable diagnostic kind: dotted snake_case segments. New kinds can appear \
                 within v1.",
            ))
            .examples(["schedule.invalid", "key.unknown"])
            .into()
    }
}

impl ToSchema for JobDiagnosticKind {}

/// One Job import finding on a job file (#17 §5): the line diff, the 1.x and 2.x readings, the
/// next 3 fires under each daemon for a cron change, and the reason. The same findings appear
/// in the dry run, the import report, `JobDetail.import_findings` and `jobs check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportFinding {
    pub level: ImportLevel,
    pub kind: ImportFindingKind,
    /// The lines the import rewrites, inserts or comments out; empty when the file is kept as is.
    pub edits: Vec<ImportLineEdit>,
    /// How the TS daemon (1.x) reads the affected key; `null` when no key's meaning is involved.
    #[schema(required = true)]
    pub ts_reading: Option<String>,
    /// How the Rust Daemon (2.x) reads it.
    #[schema(required = true)]
    pub rust_reading: Option<String>,
    /// For a cron finding, the next 3 fires under 1.x; empty otherwise.
    pub ts_next_fires: Vec<DateTime<Utc>>,
    /// For a cron finding, the next 3 fires under 2.x; empty otherwise.
    pub rust_next_fires: Vec<DateTime<Utc>>,
    /// Keyed to #12's tables.
    pub reason: String,
}

wire_enum! {
    /// How much an [`ImportFinding`] matters (#17 §5).
    pub enum ImportLevel {
        /// The job is invalid and won't be scheduled, e.g. fewer than 5 fields.
        Error = "error",
        /// The meaning or behaviour changes, or a line is rewritten.
        Warning = "warning",
        /// Kept as is.
        Info = "info",
    }
}

wire_enum! {
    /// What an [`ImportFinding`] is about: #17 §5's list, keyed to #12's tables.
    pub enum ImportFindingKind {
        /// A day-of-month or month `*/n` rewritten to `n-31/n` or `n-12/n` (#12 §1).
        StepRewritten = "step_rewritten",
        /// `+` added to day-of-week so both daemons AND the day fields (#12 §1).
        AndAdded = "and_added",
        /// A step on a single value (`5/15`) reduced to what 1.x ran (#12 §1).
        SingleValueStep = "single_value_step",
        /// Disabled because the schedule never fired under 1.x (#12 §1).
        NeverFiredDisabled = "never_fired_disabled",
        /// Cron fields past the fifth truncated (#12 §1).
        FieldsTruncated = "fields_truncated",
        /// A frontmatter value re-quoted so both parsers read it the same (#12 §2).
        LineRequoted = "line_requoted",
        /// A line 1.x ignored commented out with `# ` (#12 §2).
        LineCommentedOut = "line_commented_out",
        /// A lenient value (`90s`, `maybe`) canonicalised to what 1.x read (#12 §2).
        ValueCanonicalised = "value_canonicalised",
        /// `recurring: false` inserted for an implicit one-shot (#12 §3).
        RecurringInserted = "recurring_inserted",
        /// `daily:` converted to `recurring:` (#12 §3).
        DailyConverted = "daily_converted",
        /// A key within edit distance 2 of a known one (#12 §2).
        NearMissKey = "near_miss_key",
        /// An agent job that will no longer be replayed after restarts (#17 §5).
        AgentReplayDropped = "agent_replay_dropped",
        /// A job-name session key whose 30-day orphan clock starts (#12 §6).
        OrphanClockStarted = "orphan_clock_started",
        /// The file is invalid in both daemons and is left alone (#12 §5).
        Invalid = "invalid",
    }
}

/// One line of an [`ImportFinding`]'s diff (#17 §5, #22 `jobs check`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportLineEdit {
    /// 1-based, in the file before the import.
    pub line: u32,
    /// `null` for an inserted line.
    #[schema(required = true)]
    pub before: Option<String>,
    /// `null` for a removed line.
    #[schema(required = true)]
    pub after: Option<String>,
}

impl fmt::Debug for ImportLineEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportLineEdit")
            .field("line", &self.line)
            .field("before_bytes", &self.before.as_ref().map(String::len))
            .field("after_bytes", &self.after.as_ref().map(String::len))
            .finish()
    }
}

/// `jobs.preview`'s body (management-api §2.4): a job as raw text (`{raw}`), as form fields
/// and a prompt (`{fields, prompt}`), or a bare schedule (`{schedule}`), plus how many fires to
/// list. Writes nothing.
///
/// Like [`JobCreate`], the forms are told apart by which keys are present: exactly one of
/// `raw`, `fields` + `prompt`, or `schedule`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "flat::JobPreviewRequest", into = "flat::JobPreviewRequest")]
pub struct JobPreviewRequest {
    pub source: PreviewSource,
    /// How many next fires to list; 5 when absent.
    pub count: u32,
}

/// What a [`JobPreviewRequest`] previews.
#[derive(Clone, PartialEq)]
pub enum PreviewSource {
    /// The raw editor's text; the preview parses it into `fields` and `prompt`.
    Raw { raw: String },
    /// The form; the preview renders it as `raw`.
    Form { fields: JobFields, prompt: String },
    /// The cron builder's expression alone.
    Schedule { schedule: String },
}

impl fmt::Debug for JobPreviewRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobPreviewRequest")
            .field("source", &self.source)
            .field("count", &self.count)
            .finish()
    }
}

impl fmt::Debug for PreviewSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PreviewSource::Raw { raw } => f
                .debug_struct("Raw")
                .field("raw_bytes", &raw.len())
                .finish(),
            PreviewSource::Form { fields, prompt } => f
                .debug_struct("Form")
                .field("fields", fields)
                .field("prompt_bytes", &prompt.len())
                .finish(),
            PreviewSource::Schedule { schedule } => f
                .debug_struct("Schedule")
                .field("schedule", schedule)
                .finish(),
        }
    }
}

const fn five() -> u32 {
    5
}

impl TryFrom<flat::JobPreviewRequest> for JobPreviewRequest {
    type Error = &'static str;

    fn try_from(body: flat::JobPreviewRequest) -> Result<Self, Self::Error> {
        let source = match (body.raw, body.fields, body.prompt, body.schedule) {
            (Some(raw), None, None, None) => PreviewSource::Raw { raw },
            (None, Some(fields), Some(prompt), None) => PreviewSource::Form { fields, prompt },
            (None, None, None, Some(schedule)) => PreviewSource::Schedule { schedule },
            _ => return Err("give exactly one of `raw`, `fields` and `prompt`, or `schedule`"),
        };
        Ok(JobPreviewRequest {
            source,
            count: body.count,
        })
    }
}

impl From<JobPreviewRequest> for flat::JobPreviewRequest {
    fn from(request: JobPreviewRequest) -> Self {
        let mut body = flat::JobPreviewRequest {
            raw: None,
            fields: None,
            prompt: None,
            schedule: None,
            count: request.count,
        };
        match request.source {
            PreviewSource::Raw { raw } => body.raw = Some(raw),
            PreviewSource::Form { fields, prompt } => {
                body.fields = Some(fields);
                body.prompt = Some(prompt);
            }
            PreviewSource::Schedule { schedule } => body.schedule = Some(schedule),
        }
        body
    }
}

/// `jobs.preview` → the job both ways, its diagnostics and next fires, computed by the Daemon
/// with croner's OR day semantics in its timezone (management-api §2.4, #12, #16, #50).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobPreview {
    pub fields: JobFields,
    pub prompt: String,
    pub raw: String,
    pub diagnostics: Vec<JobDiagnostic>,
    /// The next `count` future slots; a new or rescheduled job never catches up a past slot
    /// (#50).
    pub next_fires: Vec<DateTime<Utc>>,
    /// The Daemon's IANA timezone, which the schedule is read in.
    pub timezone: String,
    /// Explanations for the cron builder, e.g. that day-of-month and day-of-week match with OR.
    pub notes: Vec<String>,
}

impl fmt::Debug for JobPreview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobPreview")
            .field("fields", &self.fields)
            .field("prompt_bytes", &self.prompt.len())
            .field("raw_bytes", &self.raw.len())
            .field("diagnostics", &self.diagnostics)
            .field("next_fires", &self.next_fires)
            .field("timezone", &self.timezone)
            .field("notes", &self.notes)
            .finish()
    }
}

/// `jobs.check` and `claudeclaw jobs check --json`: every job read from disk, never written
/// (management-api §2.4; #12 §4, #22).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobsCheck {
    pub daemon_running: bool,
    pub jobs: Vec<JobCheck>,
}

/// One job in [`JobsCheck`] (#22 `jobs check`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobCheck {
    pub name: String,
    pub path: String,
    pub status: JobStatus,
    pub diagnostics: Vec<JobDiagnostic>,
    /// The next `next` fires (default 5).
    pub next_fires: Vec<DateTime<Utc>>,
    /// `null` when the job has never run.
    #[schema(required = true)]
    pub last_run: Option<JobCheckLastRun>,
    /// Computed "as if started now"; `null` while a Daemon runs.
    #[schema(required = true)]
    pub catch_up: Option<JobCatchUp>,
    /// Pending Job import edits (#17 §5).
    pub import: Vec<ImportFinding>,
}

/// When a job last ran and where that was read from (#22 `jobs check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobCheckLastRun {
    pub at: DateTime<Utc>,
    pub source: LastRunSource,
}

wire_enum! {
    /// Where [`JobCheckLastRun`] came from (#22).
    pub enum LastRunSource {
        /// The Rust job state.
        Rust = "rust",
        /// A TS-pattern Run log name.
        TsLog = "ts-log",
    }
}

/// Whether a start now would catch the job up (#22 `jobs check`, #9's 6 h window).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobCatchUp {
    pub would_fire: bool,
    /// The slot it would catch up; `null` when it wouldn't fire.
    #[schema(required = true)]
    pub slot: Option<DateTime<Utc>>,
}

/// `jobs.audit` and `claudeclaw jobs audit --json`: each schedule's expected fires against the
/// Run index (management-api §2.4; #22, #44).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobsAudit {
    pub window: AuditWindow,
    pub jobs: Vec<JobAudit>,
    /// No job has a problem in the window.
    pub ok: bool,
    /// Whether the Run index was complete when the audit read it (#44 D2).
    pub index: RunIndexState,
}

/// The time range [`JobsAudit`] covers (#22: `--since`, default 24 h; `--until`, default now).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// One job in [`JobsAudit`] (#22 `jobs audit`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobAudit {
    pub name: String,
    /// The slots the schedule says should have fired in the window.
    pub expected: Vec<DateTime<Utc>>,
    pub runs: Vec<AuditRun>,
    pub problems: Vec<AuditProblem>,
}

/// A run found for the job in the window (#22 `jobs audit`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditRun {
    pub at: DateTime<Utc>,
    /// `null` when read from TS-pattern log names because the Run index doesn't exist yet.
    #[schema(required = true)]
    pub run_id: Option<String>,
}

/// One mismatch between expected fires and runs (#22 `jobs audit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditProblem {
    pub kind: AuditProblemKind,
    /// The expected slot, or the run's start for `unexpected`.
    pub at: DateTime<Utc>,
    /// How far the run started after its slot; `null` for `missed` and `unexpected`.
    #[schema(required = true)]
    pub delay_ms: Option<u64>,
}

wire_enum! {
    /// What went wrong with a slot (#22 `jobs audit`).
    pub enum AuditProblemKind {
        /// No run for an expected slot.
        Missed = "missed",
        /// More than one run for one slot.
        Duplicate = "duplicate",
        /// A run later than `--late` (default 2 m) after its slot.
        Late = "late",
        /// A scheduled run with no expected slot.
        Unexpected = "unexpected",
    }
}

/// The flat bodies [`JobCreate`] and [`JobPreviewRequest`] are parsed from and written as.
mod flat {
    use serde::{Deserialize, Serialize};

    use super::JobFields;

    #[derive(Serialize, Deserialize)]
    pub(super) struct JobCreate {
        pub(super) name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) fields: Option<JobFields>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) raw: Option<String>,
    }

    #[derive(Serialize, Deserialize)]
    pub(super) struct JobPreviewRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) raw: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) fields: Option<JobFields>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) schedule: Option<String>,
        #[serde(default = "super::five")]
        pub(super) count: u32,
    }
}

/// The schemas of [`JobCreate`] and [`JobPreviewRequest`]: one object per form. utoipa can't
/// derive them from the hand-parsed types, so they come from these copies, and tests check that
/// each form parses as the real type.
mod schema {
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    use super::JobFields;

    /// `jobs.create`'s body (management-api §2.4): a new job as the form's fields and a prompt,
    /// or as the whole file. `raw` can't be combined with `fields` or `prompt`.
    #[derive(Serialize, Deserialize, ToSchema)]
    #[serde(untagged)]
    pub(super) enum JobCreate {
        Form {
            name: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            agent: Option<String>,
            fields: JobFields,
            prompt: String,
        },
        Raw {
            name: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            agent: Option<String>,
            raw: String,
        },
    }

    /// `jobs.preview`'s body (management-api §2.4): a job as raw text, as form fields and a
    /// prompt, or as a bare schedule; exactly one form. `count` defaults to 5.
    #[derive(Serialize, Deserialize, ToSchema)]
    #[serde(untagged)]
    pub(super) enum JobPreviewRequest {
        Raw {
            raw: String,
            #[serde(default = "super::five")]
            #[schema(default = 5)]
            count: u32,
        },
        Form {
            fields: JobFields,
            prompt: String,
            #[serde(default = "super::five")]
            #[schema(default = 5)]
            count: u32,
        },
        Schedule {
            schedule: String,
            #[serde(default = "super::five")]
            #[schema(default = 5)]
            count: u32,
        },
    }
}

impl PartialSchema for JobCreate {
    fn schema() -> RefOr<Schema> {
        schema::JobCreate::schema()
    }
}

impl ToSchema for JobCreate {
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schema::JobCreate::schemas(schemas);
    }
}

impl PartialSchema for JobPreviewRequest {
    fn schema() -> RefOr<Schema> {
        schema::JobPreviewRequest::schema()
    }
}

impl ToSchema for JobPreviewRequest {
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schema::JobPreviewRequest::schemas(schemas);
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    JobList,
    JobSummary,
    JobStatus,
    JobLastRun,
    JobDetail,
    JobFields,
    JobNotify,
    NotifyOnly,
    JobCreate,
    JobPatch,
    JobValue,
    JobRaw,
    JobDeleted,
    JobDiagnostic,
    DiagnosticSeverity,
    JobDiagnosticKind,
    ImportFinding,
    ImportLevel,
    ImportFindingKind,
    ImportLineEdit,
    JobPreviewRequest,
    JobPreview,
    JobsCheck,
    JobCheck,
    JobCheckLastRun,
    LastRunSource,
    JobCatchUp,
    JobsAudit,
    AuditWindow,
    JobAudit,
    AuditRun,
    AuditProblem,
    AuditProblemKind,
)))]
pub(crate) struct Doc;

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn summary() -> (JobSummary, Value) {
        let summary = JobSummary {
            name: "daily-report".into(),
            agent: None,
            path: ".claude/claudeclaw/jobs/daily-report.md".into(),
            status: JobStatus::Valid,
            schedule: Some("0 9 * * *".into()),
            recurring: Some(true),
            enabled: true,
            notify: JobNotify::Only(NotifyOnly::Error),
            launcher: None,
            model: Some("claude-sonnet-5-5".into()),
            next_at: Some(at("2026-10-15T09:00:00Z")),
            last_run: Some(JobLastRun {
                at: at("2026-10-14T09:00:02Z"),
                outcome: RunOutcome::Ok,
            }),
            in_flight: Some(false),
            retry_at: None,
            revision: "0123456789abcdef".parse().unwrap(),
        };
        let wire = json!({
            "name": "daily-report",
            "agent": null,
            "path": ".claude/claudeclaw/jobs/daily-report.md",
            "status": "valid",
            "schedule": "0 9 * * *",
            "recurring": true,
            "enabled": true,
            "notify": "error",
            "launcher": null,
            "model": "claude-sonnet-5-5",
            "next_at": "2026-10-15T09:00:00Z",
            "last_run": {"at": "2026-10-14T09:00:02Z", "outcome": "ok"},
            "in_flight": false,
            "retry_at": null,
            "revision": "0123456789abcdef",
        });
        (summary, wire)
    }

    fn finding() -> (ImportFinding, Value) {
        let finding = ImportFinding {
            level: ImportLevel::Warning,
            kind: ImportFindingKind::StepRewritten,
            edits: vec![ImportLineEdit {
                line: 2,
                before: Some("schedule: \"0 9 */3 * *\"".into()),
                after: Some("schedule: \"0 9 3-31/3 * *\"".into()),
            }],
            ts_reading: Some("days 3, 6, 9, …".into()),
            rust_reading: Some("days 3, 6, 9, …".into()),
            ts_next_fires: vec![at("2026-10-15T09:00:00Z")],
            rust_next_fires: vec![at("2026-10-15T09:00:00Z")],
            reason: "day-of-month */n is counted from 1 in 2.x".into(),
        };
        let wire = json!({
            "level": "warning",
            "kind": "step_rewritten",
            "edits": [{
                "line": 2,
                "before": "schedule: \"0 9 */3 * *\"",
                "after": "schedule: \"0 9 3-31/3 * *\"",
            }],
            "ts_reading": "days 3, 6, 9, …",
            "rust_reading": "days 3, 6, 9, …",
            "ts_next_fires": ["2026-10-15T09:00:00Z"],
            "rust_next_fires": ["2026-10-15T09:00:00Z"],
            "reason": "day-of-month */n is counted from 1 in 2.x",
        });
        (finding, wire)
    }

    fn diagnostic() -> (JobDiagnostic, Value) {
        let diagnostic = JobDiagnostic {
            severity: DiagnosticSeverity::Warning,
            kind: "key.near_miss".parse().unwrap(),
            line: Some(4),
            column: Some(1),
            message: "unknown key `recuring`; did you mean `recurring`?".into(),
        };
        let wire = json!({
            "severity": "warning",
            "kind": "key.near_miss",
            "line": 4,
            "column": 1,
            "message": "unknown key `recuring`; did you mean `recurring`?",
        });
        (diagnostic, wire)
    }

    fn round_trip<T>(value: &T, wire: Value)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + fmt::Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), wire);
        assert_eq!(&serde_json::from_value::<T>(wire).unwrap(), value);
    }

    #[test]
    fn job_list_shape() {
        let (summary, wire) = summary();
        round_trip(
            &JobList {
                jobs: vec![summary],
            },
            json!({"jobs": [wire]}),
        );
    }

    #[test]
    fn job_detail_is_the_summary_plus_its_text() {
        let (summary, mut wire) = summary();
        let (diagnostic, diagnostic_wire) = diagnostic();
        let (finding, finding_wire) = finding();
        let detail = JobDetail {
            summary,
            fields: JobFields {
                schedule: Some("0 9 * * *".into()),
                recurring: Some(true),
                notify: Some(JobNotify::Only(NotifyOnly::Error)),
                model: Some("claude-sonnet-5-5".into()),
                ..JobFields::default()
            },
            prompt: "Summarise yesterday.\n".into(),
            raw: "---\nschedule: \"0 9 * * *\"\n---\nSummarise yesterday.\n".into(),
            diagnostics: vec![diagnostic],
            import_findings: vec![finding],
            next_fires: vec![at("2026-10-15T09:00:00Z"), at("2026-10-16T09:00:00Z")],
        };
        let extra = json!({
            "fields": {
                "schedule": "0 9 * * *",
                "recurring": true,
                "notify": "error",
                "model": "claude-sonnet-5-5",
            },
            "prompt": "Summarise yesterday.\n",
            "raw": "---\nschedule: \"0 9 * * *\"\n---\nSummarise yesterday.\n",
            "diagnostics": [diagnostic_wire],
            "import_findings": [finding_wire],
            "next_fires": ["2026-10-15T09:00:00Z", "2026-10-16T09:00:00Z"],
        });
        wire.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        round_trip(&detail, wire);
    }

    #[test]
    fn job_fields_send_only_the_keys_present() {
        round_trip(&JobFields::default(), json!({}));
        let fields = JobFields {
            schedule: Some("@daily".into()),
            recurring: Some(false),
            enabled: Some(false),
            notify: Some(JobNotify::Bool(true)),
            model: Some("claude-haiku-5-5".into()),
            timeout: Some(900),
            retry: Some(2),
            retry_delay: Some(300),
            launcher: Some("direct".into()),
        };
        round_trip(
            &fields,
            json!({
                "schedule": "@daily",
                "recurring": false,
                "enabled": false,
                "notify": true,
                "model": "claude-haiku-5-5",
                "timeout": 900,
                "retry": 2,
                "retry_delay": 300,
                "launcher": "direct",
            }),
        );
    }

    #[test]
    fn notify_is_a_bool_or_error() {
        round_trip(&JobNotify::Bool(false), json!(false));
        round_trip(&JobNotify::Only(NotifyOnly::Error), json!("error"));
        assert_eq!(
            serde_json::from_value::<JobNotify>(json!("added_later")).unwrap(),
            JobNotify::Only(NotifyOnly::Unknown)
        );
    }

    #[test]
    fn job_create_form() {
        let create = JobCreate {
            name: "paper-digest".into(),
            agent: Some("researcher".into()),
            content: JobContent::Form {
                fields: JobFields {
                    schedule: Some("0 8 * * 1".into()),
                    recurring: Some(true),
                    ..JobFields::default()
                },
                prompt: "Digest new papers.".into(),
            },
        };
        round_trip(
            &create,
            json!({
                "name": "paper-digest",
                "agent": "researcher",
                "fields": {"schedule": "0 8 * * 1", "recurring": true},
                "prompt": "Digest new papers.",
            }),
        );
    }

    #[test]
    fn job_create_raw() {
        let create = JobCreate {
            name: "daily-report".into(),
            agent: None,
            content: JobContent::Raw {
                raw: "---\nrecurring: true\n---\nHi.\n".into(),
            },
        };
        round_trip(
            &create,
            json!({"name": "daily-report", "raw": "---\nrecurring: true\n---\nHi.\n"}),
        );
    }

    #[test]
    fn job_create_needs_exactly_one_form() {
        for body in [
            json!({"name": "a"}),
            json!({"name": "a", "fields": {}}),
            json!({"name": "a", "prompt": "p"}),
            json!({"name": "a", "raw": "r", "prompt": "p"}),
            json!({"name": "a", "raw": "r", "fields": {}, "prompt": "p"}),
        ] {
            assert!(
                serde_json::from_value::<JobCreate>(body.clone()).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn job_preview_request_raw() {
        let request = JobPreviewRequest {
            source: PreviewSource::Raw {
                raw: "---\nschedule: \"@daily\"\n---\n".into(),
            },
            count: 5,
        };
        assert_eq!(
            serde_json::from_value::<JobPreviewRequest>(
                json!({"raw": "---\nschedule: \"@daily\"\n---\n"})
            )
            .unwrap(),
            request
        );
        round_trip(
            &request,
            json!({"raw": "---\nschedule: \"@daily\"\n---\n", "count": 5}),
        );
    }

    #[test]
    fn job_preview_request_form() {
        let request = JobPreviewRequest {
            source: PreviewSource::Form {
                fields: JobFields {
                    schedule: Some("0 9 13 * 1".into()),
                    ..JobFields::default()
                },
                prompt: "Hi.".into(),
            },
            count: 10,
        };
        round_trip(
            &request,
            json!({"fields": {"schedule": "0 9 13 * 1"}, "prompt": "Hi.", "count": 10}),
        );
    }

    #[test]
    fn job_preview_request_schedule() {
        let request = JobPreviewRequest {
            source: PreviewSource::Schedule {
                schedule: "*/15 * * * *".into(),
            },
            count: 5,
        };
        assert_eq!(
            serde_json::from_value::<JobPreviewRequest>(json!({"schedule": "*/15 * * * *"}))
                .unwrap(),
            request
        );
        round_trip(&request, json!({"schedule": "*/15 * * * *", "count": 5}));
    }

    #[test]
    fn job_preview_request_needs_exactly_one_form() {
        for body in [
            json!({}),
            json!({"count": 3}),
            json!({"fields": {}}),
            json!({"raw": "r", "schedule": "@daily"}),
            json!({"fields": {}, "prompt": "p", "schedule": "@daily"}),
        ] {
            assert!(
                serde_json::from_value::<JobPreviewRequest>(body.clone()).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn every_schema_form_parses_as_the_real_type() {
        for body in [
            json!({"name": "a", "fields": {}, "prompt": "p"}),
            json!({"name": "a", "agent": "b", "raw": "r"}),
        ] {
            serde_json::from_value::<schema::JobCreate>(body.clone()).unwrap();
            serde_json::from_value::<JobCreate>(body).unwrap();
        }
        for body in [
            json!({"raw": "r"}),
            json!({"fields": {}, "prompt": "p", "count": 3}),
            json!({"schedule": "@daily"}),
        ] {
            serde_json::from_value::<schema::JobPreviewRequest>(body.clone()).unwrap();
            serde_json::from_value::<JobPreviewRequest>(body).unwrap();
        }
    }

    #[test]
    fn job_patch_sets_and_removes_keys() {
        let patch = JobPatch {
            set: BTreeMap::from([
                ("enabled".into(), Some(JobValue::Bool(false))),
                ("model".into(), None),
                ("notify".into(), Some(JobValue::String("error".into()))),
                ("timeout".into(), Some(JobValue::Integer(600))),
            ]),
            prompt: Some("New body.".into()),
        };
        round_trip(
            &patch,
            json!({
                "set": {"enabled": false, "model": null, "notify": "error", "timeout": 600},
                "prompt": "New body.",
            }),
        );
        round_trip(&JobPatch::default(), json!({"set": {}}));
    }

    #[test]
    fn job_raw_and_deleted_shapes() {
        round_trip(
            &JobRaw {
                raw: "---\n---\n".into(),
            },
            json!({"raw": "---\n---\n"}),
        );
        round_trip(
            &JobDeleted {
                deleted: true,
                sessions_removed: 2,
                state_removed: true,
            },
            json!({"deleted": true, "sessions_removed": 2, "state_removed": true}),
        );
    }

    #[test]
    fn diagnostic_kinds_parse() {
        for ok in [
            "schedule.invalid",
            "key.unknown",
            "recurring",
            "lint.ts_1x.divergence",
        ] {
            assert_eq!(ok.parse::<JobDiagnosticKind>().unwrap().as_str(), ok);
        }
        let too_long = format!("a.{}", "b".repeat(63));
        for bad in [
            "",
            ".",
            "key.",
            ".key",
            "key..unknown",
            "Key.unknown",
            "key.1x",
            "key unknown",
            "key:unknown",
            too_long.as_str(),
        ] {
            assert!(bad.parse::<JobDiagnosticKind>().is_err(), "{bad:?}");
        }
        assert!(serde_json::from_value::<JobDiagnosticKind>(json!("schedule says hi")).is_err());
        let (diagnostic, wire) = diagnostic();
        round_trip(&diagnostic, wire);
    }

    #[test]
    fn import_finding_shape_and_catch_alls() {
        let (finding, wire) = finding();
        round_trip(&finding, wire);
        round_trip(
            &ImportLineEdit {
                line: 3,
                before: None,
                after: Some("recurring: false".into()),
            },
            json!({"line": 3, "before": null, "after": "recurring: false"}),
        );
        assert_eq!(
            serde_json::from_value::<ImportFindingKind>(json!("added_later")).unwrap(),
            ImportFindingKind::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ImportLevel>(json!("added_later")).unwrap(),
            ImportLevel::Unknown
        );
    }

    #[test]
    fn job_preview_shape() {
        let preview = JobPreview {
            fields: JobFields {
                schedule: Some("0 9 13 * 1".into()),
                ..JobFields::default()
            },
            prompt: String::new(),
            raw: "---\nschedule: \"0 9 13 * 1\"\n---\n".into(),
            diagnostics: vec![],
            next_fires: vec![at("2026-10-19T09:00:00Z")],
            timezone: "UTC".into(),
            notes: vec!["day-of-month and day-of-week match with OR".into()],
        };
        round_trip(
            &preview,
            json!({
                "fields": {"schedule": "0 9 13 * 1"},
                "prompt": "",
                "raw": "---\nschedule: \"0 9 13 * 1\"\n---\n",
                "diagnostics": [],
                "next_fires": ["2026-10-19T09:00:00Z"],
                "timezone": "UTC",
                "notes": ["day-of-month and day-of-week match with OR"],
            }),
        );
    }

    #[test]
    fn jobs_check_shape() {
        let (diagnostic, diagnostic_wire) = diagnostic();
        let (finding, finding_wire) = finding();
        let check = JobsCheck {
            daemon_running: false,
            jobs: vec![JobCheck {
                name: "daily-report".into(),
                path: ".claude/claudeclaw/jobs/daily-report.md".into(),
                status: JobStatus::Warning,
                diagnostics: vec![diagnostic],
                next_fires: vec![at("2026-10-15T09:00:00Z")],
                last_run: Some(JobCheckLastRun {
                    at: at("2026-10-14T09:00:02Z"),
                    source: LastRunSource::TsLog,
                }),
                catch_up: Some(JobCatchUp {
                    would_fire: false,
                    slot: None,
                }),
                import: vec![finding],
            }],
        };
        round_trip(
            &check,
            json!({
                "daemon_running": false,
                "jobs": [{
                    "name": "daily-report",
                    "path": ".claude/claudeclaw/jobs/daily-report.md",
                    "status": "warning",
                    "diagnostics": [diagnostic_wire],
                    "next_fires": ["2026-10-15T09:00:00Z"],
                    "last_run": {"at": "2026-10-14T09:00:02Z", "source": "ts-log"},
                    "catch_up": {"would_fire": false, "slot": null},
                    "import": [finding_wire],
                }],
            }),
        );
    }

    #[test]
    fn jobs_audit_shape() {
        let audit = JobsAudit {
            window: AuditWindow {
                from: at("2026-10-13T10:00:00Z"),
                to: at("2026-10-14T10:00:00Z"),
            },
            jobs: vec![JobAudit {
                name: "daily-report".into(),
                expected: vec![at("2026-10-14T09:00:00Z")],
                runs: vec![AuditRun {
                    at: at("2026-10-14T09:03:30Z"),
                    run_id: Some("0199e0c2-0000-7000-8000-000000000001".into()),
                }],
                problems: vec![AuditProblem {
                    kind: AuditProblemKind::Late,
                    at: at("2026-10-14T09:00:00Z"),
                    delay_ms: Some(210_000),
                }],
            }],
            ok: false,
            index: RunIndexState {
                state: crate::runs::RunIndexPhase::Ready,
                done: 0,
                total: 0,
                since: at("2026-10-13T08:00:00Z"),
            },
        };
        round_trip(
            &audit,
            json!({
                "window": {"from": "2026-10-13T10:00:00Z", "to": "2026-10-14T10:00:00Z"},
                "jobs": [{
                    "name": "daily-report",
                    "expected": ["2026-10-14T09:00:00Z"],
                    "runs": [{
                        "at": "2026-10-14T09:03:30Z",
                        "run_id": "0199e0c2-0000-7000-8000-000000000001",
                    }],
                    "problems": [{"kind": "late", "at": "2026-10-14T09:00:00Z", "delay_ms": 210000}],
                }],
                "ok": false,
                "index": {"state": "ready", "done": 0, "total": 0, "since": "2026-10-13T08:00:00Z"},
            }),
        );
        assert_eq!(
            serde_json::from_value::<AuditProblemKind>(json!("added_later")).unwrap(),
            AuditProblemKind::Unknown
        );
    }

    #[test]
    fn debug_prints_no_content() {
        let secret = "SECRET-PROMPT-TEXT";
        let (summary, _) = summary();
        let detail = JobDetail {
            summary,
            fields: JobFields::default(),
            prompt: secret.into(),
            raw: format!("---\n---\n{secret}"),
            diagnostics: vec![JobDiagnostic {
                message: format!("line says {secret}"),
                ..diagnostic().0
            }],
            import_findings: vec![ImportFinding {
                edits: vec![ImportLineEdit {
                    line: 1,
                    before: Some(secret.into()),
                    after: Some(format!("# {secret}")),
                }],
                ..finding().0
            }],
            next_fires: vec![],
        };
        let create = JobCreate {
            name: "a".into(),
            agent: None,
            content: JobContent::Form {
                fields: JobFields::default(),
                prompt: secret.into(),
            },
        };
        let raw_create = JobCreate {
            content: JobContent::Raw { raw: secret.into() },
            ..create.clone()
        };
        let patch = JobPatch {
            set: BTreeMap::from([("model".into(), Some(JobValue::String(secret.into())))]),
            prompt: Some(secret.into()),
        };
        let preview_request = JobPreviewRequest {
            source: PreviewSource::Form {
                fields: JobFields::default(),
                prompt: secret.into(),
            },
            count: 5,
        };
        let preview = JobPreview {
            fields: JobFields::default(),
            prompt: secret.into(),
            raw: secret.into(),
            diagnostics: vec![],
            next_fires: vec![],
            timezone: "UTC".into(),
            notes: vec![],
        };
        for debug in [
            format!("{detail:?}"),
            format!("{create:?}"),
            format!("{raw_create:?}"),
            format!("{patch:?}"),
            format!("{:?}", JobRaw { raw: secret.into() }),
            format!("{preview_request:?}"),
            format!("{preview:?}"),
        ] {
            assert!(!debug.contains(secret), "{debug}");
        }
    }
}
