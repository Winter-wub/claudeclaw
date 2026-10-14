//! Reading a job file (#12 §2–§4): envelope split, serde-saphyr, the strict value grammar, the
//! one-shot rules, and the diagnostics.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{FixedOffset, TimeZone};
use serde_saphyr::MessageFormatter;

use crate::cron::Schedule;
use crate::diagnostic::{JobDiagnostic, Severity, kind};
use crate::fields::{JobFields, near_miss, parse_bool, parse_notify, parse_uint};
use crate::lint::Lint;

/// One line of text: `start..end` is its content, `start..next` includes the line break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Line {
    pub start: usize,
    pub end: usize,
    pub next: usize,
}

/// Splits `text` into lines. A `\r` before `\n` belongs to the break, not the content. The
/// last line has `end == next` when the text doesn't end with a break.
pub(crate) fn lines(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let (end, next) = match text[start..].find('\n') {
            Some(i) => {
                let nl = start + i;
                let end = if text[start..nl].ends_with('\r') {
                    nl - 1
                } else {
                    nl
                };
                (end, nl + 1)
            }
            None => (text.len(), text.len()),
        };
        out.push(Line { start, end, next });
        start = next;
    }
    out
}

const BOM: &str = "\u{feff}";

fn is_marker(content: &str) -> bool {
    content.trim_end_matches([' ', '\t']) == "---"
}

/// Where the frontmatter and body sit in the text (#12 §2). Line indexes are 0-based into
/// [`lines`]; the opening `---` is always line 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Envelope {
    /// The closing `---` line.
    pub close: usize,
    /// Byte range of the frontmatter: after the opening line, up to the closing line.
    pub frontmatter: (usize, usize),
    /// Byte offset where the body starts (the end of the text when there is none).
    pub body: usize,
}

impl Envelope {
    /// The first line is `---` (after an optional BOM), and the frontmatter ends at the next
    /// `---` line, which may be the last line. Trailing spaces on a marker are tolerated, as TS
    /// did (`src/jobs.ts:36`, `^---\s*\n`).
    pub fn split(text: &str, lines: &[Line]) -> Result<Envelope, JobDiagnostic> {
        let first = lines.first().map(|l| &text[l.start..l.end]).unwrap_or("");
        if !is_marker(first.strip_prefix(BOM).unwrap_or(first)) {
            return Err(JobDiagnostic::error(
                kind::ENVELOPE_MISSING,
                "a job file starts with a `---` line, then the frontmatter, then another `---` line",
            )
            .at(1, None));
        }
        let close = (1..lines.len())
            .find(|&i| is_marker(&text[lines[i].start..lines[i].end]))
            .ok_or_else(|| {
                JobDiagnostic::error(
                    kind::ENVELOPE_UNCLOSED,
                    "no `---` line closes the frontmatter that starts on line 1",
                )
                .at(1, None)
            })?;
        Ok(Envelope {
            close,
            frontmatter: (lines[0].next, lines[close].start),
            body: lines[close].next,
        })
    }
}

/// The key a top-level block-mapping line starts, and the byte offset of its value within the
/// line: `key:` or `key :` followed by whitespace or the end of the line, at column 1. Comments,
/// indented lines and sequence items have none.
pub(crate) fn line_key(content: &str) -> Option<(&str, usize)> {
    let first = content.chars().next()?;
    if first.is_whitespace() || matches!(first, '#' | '-' | '"' | '\'' | '{' | '[' | '?') {
        return None;
    }
    let mut from = 0;
    while let Some(i) = content[from..].find(':') {
        let colon = from + i;
        let after = &content[colon + 1..];
        if after.is_empty() || after.starts_with([' ', '\t']) {
            let key = content[..colon].trim_end_matches([' ', '\t']);
            let value = colon + 1 + (after.len() - after.trim_start_matches([' ', '\t']).len());
            return Some((key, value));
        }
        from = colon + 1;
    }
    None
}

/// A parsed job file (#12 §2–§4): the text, its frontmatter keys as serde-saphyr reads them, the
/// typed [`JobFields`], and every [`JobDiagnostic`].
///
/// Parsing never fails: a file that can't be read is an invalid job, with errors in
/// [`diagnostics`](Self::diagnostics), so it is listed and reported while the other jobs load
/// (#12 §4).
#[derive(Clone)]
pub struct JobFile {
    text: String,
    lines: Vec<Line>,
    envelope: Option<Envelope>,
    keys: BTreeMap<String, Option<String>>,
    fields: JobFields,
    diagnostics: Vec<JobDiagnostic>,
}

impl JobFile {
    /// Reads a job file from the bytes `store` hands over.
    pub fn parse(bytes: &[u8]) -> JobFile {
        JobFile::parse_with(bytes, &[])
    }

    /// Reads a job file, then runs each [`Lint`] and appends its findings (#12 §4).
    pub fn parse_with(bytes: &[u8], lints: &[&dyn Lint]) -> JobFile {
        let mut job = match std::str::from_utf8(bytes) {
            Ok(text) => JobFile::read(text.to_owned()),
            Err(e) => {
                let text = String::from_utf8_lossy(bytes).into_owned();
                let line = text[..e.valid_up_to()].matches('\n').count() + 1;
                let mut job = JobFile::empty(text);
                job.diagnostics.push(
                    JobDiagnostic::error(kind::ENCODING_INVALID, "the file is not valid UTF-8")
                        .at(to_u32(line), None),
                );
                job
            }
        };
        for lint in lints {
            let found = lint.check(&job);
            job.diagnostics.extend(found);
        }
        job.diagnostics.sort_by_key(|d| (d.line.is_some(), d.line));
        job
    }

    fn empty(text: String) -> JobFile {
        JobFile {
            lines: lines(&text),
            text,
            envelope: None,
            keys: BTreeMap::new(),
            fields: JobFields::default(),
            diagnostics: Vec::new(),
        }
    }

    fn read(text: String) -> JobFile {
        let mut job = JobFile::empty(text);
        let envelope = match Envelope::split(&job.text, &job.lines) {
            Ok(envelope) => envelope,
            Err(d) => {
                job.diagnostics.push(d);
                return job;
            }
        };
        job.envelope = Some(envelope);
        let (from, to) = envelope.frontmatter;
        match serde_saphyr::from_str::<Option<BTreeMap<String, Option<String>>>>(
            &job.text[from..to],
        ) {
            Ok(keys) => job.keys = keys.unwrap_or_default(),
            Err(e) => {
                job.diagnostics.push(yaml_diagnostic(&e));
                return job;
            }
        }
        job.read_fields();
        job
    }

    /// The typed fields, the value grammar and the one-shot rule (#12 §2–§3).
    fn read_fields(&mut self) {
        let mut diags = Vec::new();
        let mut fields = JobFields::default();
        for (key, value) in &self.keys {
            let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
                continue;
            };
            let (line, column) = self.locate(key);
            let bad = |kind: &'static str, expected: &str| {
                let mut d = JobDiagnostic::error(
                    kind,
                    format!("`{key}: {value}` is not valid; expected {expected}"),
                );
                d.line = line;
                d.column = column;
                d
            };
            match key.as_str() {
                "schedule" => match Schedule::parse(value) {
                    Ok(s) if never_fires(&s) => diags.push(bad(
                        kind::SCHEDULE_NEVER_FIRES,
                        "a schedule with at least one fire time",
                    )),
                    Ok(s) => fields.schedule = Some(s),
                    Err(e) => {
                        let mut d = bad(kind::SCHEDULE_INVALID, "5 cron fields or a nickname");
                        d.message = format!("{}: {e}", d.message);
                        diags.push(d);
                    }
                },
                "recurring" => match parse_bool(value) {
                    Some(b) => fields.recurring = Some(b),
                    None => {
                        diags.push(bad(kind::RECURRING_INVALID, "true, false, yes, no, 1 or 0"))
                    }
                },
                "enabled" => match parse_bool(value) {
                    Some(b) => fields.enabled = Some(b),
                    None => diags.push(bad(kind::ENABLED_INVALID, "true, false, yes, no, 1 or 0")),
                },
                "notify" => match parse_notify(value) {
                    Some(n) => fields.notify = Some(n),
                    None => diags.push(bad(kind::NOTIFY_INVALID, "true, false, yes, no or error")),
                },
                "timeout" => match parse_uint(value) {
                    Some(n) => fields.timeout = Some(n),
                    None => diags.push(bad(kind::TIMEOUT_INVALID, "whole seconds, such as 90")),
                },
                "retry" => match parse_uint(value) {
                    Some(n) => fields.retry = Some(n),
                    None => diags.push(bad(kind::RETRY_INVALID, "a whole number, such as 2")),
                },
                "retry_delay" => match parse_uint(value) {
                    Some(n) => fields.retry_delay = Some(n),
                    None => {
                        diags.push(bad(kind::RETRY_DELAY_INVALID, "whole seconds, such as 300"))
                    }
                },
                "model" => fields.model = Some(value.to_owned()),
                "launcher" => fields.launcher = Some(value.to_owned()),
                "label" => fields.label = Some(value.to_owned()),
                "agent" => fields.agent = Some(value.to_owned()),
                _ => {}
            }
        }
        if self.has_value("schedule") && !self.has_value("recurring") {
            let (line, _) = self.locate("schedule");
            let mut d = JobDiagnostic::error(
                kind::RECURRING_MISSING,
                "a scheduled job needs `recurring:`; add `recurring: true` (repeat) or \
                 `recurring: false` (run once)",
            );
            d.line = line;
            diags.push(d);
        }
        for key in self.keys.keys() {
            if let Some(known) = near_miss(key).filter(|k| *k != key) {
                let (line, _) = self.locate(key);
                let mut d = JobDiagnostic::warning(
                    kind::KEY_UNKNOWN,
                    format!("unknown key `{key}`; did you mean `{known}`?"),
                );
                d.line = line;
                d.column = line.map(|_| 1);
                diags.push(d);
            }
        }
        self.fields = fields;
        self.diagnostics.extend(diags);
    }

    fn has_value(&self, key: &str) -> bool {
        self.value(key).is_some()
    }

    /// The 1-based file line of `key`'s line in the frontmatter, and the column its value
    /// starts at.
    fn locate(&self, key: &str) -> (Option<u32>, Option<u32>) {
        match self.key_line(key) {
            Some(i) => {
                let l = self.lines[i];
                let content = &self.text[l.start..l.end];
                let column =
                    line_key(content).map(|(_, v)| to_u32(content[..v].chars().count() + 1));
                (Some(to_u32(i + 1)), column)
            }
            None => (None, None),
        }
    }

    /// The 0-based index into [`lines`] of the top-level line that starts `key`, if any.
    pub(crate) fn key_line(&self, key: &str) -> Option<usize> {
        let envelope = self.envelope?;
        (1..envelope.close).find(|&i| {
            let l = self.lines[i];
            line_key(&self.text[l.start..l.end]).is_some_and(|(k, _)| k == key)
        })
    }

    /// The whole file as text (lossily decoded if it wasn't UTF-8).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The frontmatter text between the two `---` lines; `None` without an envelope.
    pub fn frontmatter(&self) -> Option<&str> {
        self.envelope
            .map(|e| &self.text[e.frontmatter.0..e.frontmatter.1])
    }

    /// Everything after the closing `---` line, byte for byte; `None` without an envelope.
    pub fn body(&self) -> Option<&str> {
        self.envelope.map(|e| &self.text[e.body..])
    }

    /// The prompt: the body with surrounding whitespace trimmed, as TS reads it
    /// (`src/jobs.ts:43`). Empty without an envelope.
    pub fn prompt(&self) -> &str {
        self.body().unwrap_or("").trim()
    }

    /// Every frontmatter key as serde-saphyr reads it, unknown keys included (#12 §2). Each
    /// scalar keeps its literal text; `None` is YAML null. Empty if the frontmatter didn't parse.
    pub fn keys(&self) -> &BTreeMap<String, Option<String>> {
        &self.keys
    }

    /// A key's value, trimmed; `None` when absent, null or empty.
    pub fn value(&self, key: &str) -> Option<&str> {
        self.keys
            .get(key)?
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }

    /// The known keys, typed.
    pub fn fields(&self) -> &JobFields {
        &self.fields
    }

    /// Every error and warning, ordered by line. Findings on the whole file come first, and
    /// findings on one line keep the order they were found in.
    pub fn diagnostics(&self) -> &[JobDiagnostic] {
        &self.diagnostics
    }

    /// No [`Severity::Error`]: the job can be scheduled (#12 §4).
    pub fn is_valid(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

/// Content-free: the text and prompt are job content (architecture §2).
impl fmt::Debug for JobFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobFile")
            .field("text_bytes", &self.text.len())
            .field("keys", &self.keys.keys().collect::<Vec<_>>())
            .field("fields", &self.fields)
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

/// A schedule that parses but has no fire time from 2000-01-01 on, such as `0 0 30 2 *`.
/// croner gives up at year 5000, so any real schedule finds one.
fn never_fires(s: &Schedule) -> bool {
    let utc = FixedOffset::east_opt(0).expect("zero offset");
    let from = utc
        .with_ymd_and_hms(2000, 1, 1, 0, 0, 0)
        .single()
        .expect("valid date");
    s.next_fire(from).is_none()
}

/// serde-saphyr's error as a diagnostic. Its location is relative to the frontmatter, which
/// starts on file line 2.
fn yaml_diagnostic(e: &serde_saphyr::Error) -> JobDiagnostic {
    let e = e.without_snippet();
    let message = serde_saphyr::DefaultMessageFormatter.format_message(e);
    let message: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut d = JobDiagnostic::error(
        kind::YAML_INVALID,
        format!("the frontmatter is not valid YAML: {message}"),
    );
    if let Some(loc) = e.location().filter(|l| l.line() > 0) {
        d.line = Some(to_u32(loc.line().saturating_add(1)));
        d.column = Some(to_u32(loc.column())).filter(|c| *c > 0);
    }
    d
}

fn to_u32<T: TryInto<u32>>(n: T) -> u32 {
    n.try_into().unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields::Notify;

    fn job(text: &str) -> JobFile {
        JobFile::parse(text.as_bytes())
    }

    fn kinds(j: &JobFile) -> Vec<&'static str> {
        j.diagnostics().iter().map(|d| d.kind).collect()
    }

    fn only(j: &JobFile) -> &JobDiagnostic {
        assert_eq!(j.diagnostics().len(), 1, "{:?}", j.diagnostics());
        &j.diagnostics()[0]
    }

    const LIVE_SHAPE: &str = "---\nschedule: \"0 9 * * *\"\nrecurring: true\nnotify: error\n\
                              timeout: 600\nmodel: haiku\nenabled: true\n---\nDo the thing.\n";

    #[test]
    fn reads_a_live_shaped_job() {
        let j = job(LIVE_SHAPE);
        assert!(j.diagnostics().is_empty(), "{:?}", j.diagnostics());
        let f = j.fields();
        assert_eq!(f.schedule.as_ref().unwrap().as_str(), "0 9 * * *");
        assert_eq!(f.recurring, Some(true));
        assert_eq!(f.notify, Some(Notify::OnError));
        assert_eq!(f.timeout, Some(600));
        assert_eq!(f.model.as_deref(), Some("haiku"));
        assert_eq!(f.enabled, Some(true));
        assert_eq!(j.prompt(), "Do the thing.");
        assert_eq!(j.body(), Some("Do the thing.\n"));
        assert!(j.is_valid());
    }

    #[test]
    fn envelope_tolerates_bom_crlf_and_a_last_line_marker() {
        let j = job("\u{feff}---\r\nschedule: \"@daily\"\r\nrecurring: false\r\n---");
        assert!(j.is_valid(), "{:?}", j.diagnostics());
        assert_eq!(j.prompt(), "");
        assert!(j.fields().is_one_shot());
        assert_eq!(
            j.frontmatter(),
            Some("schedule: \"@daily\"\r\nrecurring: false\r\n")
        );
        let j = job("---  \nmodel: x\n---\t\nbody");
        assert!(j.is_valid());
        assert_eq!(j.body(), Some("body"));
    }

    #[test]
    fn envelope_errors() {
        let j = job("schedule: x\n---\n");
        assert_eq!(kinds(&j), [kind::ENVELOPE_MISSING]);
        assert!(!j.is_valid());
        assert_eq!(job("").diagnostics()[0].kind, kind::ENVELOPE_MISSING);
        assert_eq!(kinds(&job("---\nmodel: x\n")), [kind::ENVELOPE_UNCLOSED]);
        assert_eq!(
            kinds(&job("--- x\nmodel: x\n---\n")),
            [kind::ENVELOPE_MISSING]
        );
    }

    #[test]
    fn non_utf8_is_invalid() {
        let j = JobFile::parse(b"---\nmodel: \xff\n---\n");
        assert_eq!(kinds(&j), [kind::ENCODING_INVALID]);
        assert_eq!(j.diagnostics()[0].line, Some(2));
    }

    #[test]
    fn empty_frontmatter_is_an_unscheduled_job() {
        for text in [
            "---\n---\nhi\n",
            "---\n# just a comment\n---\nhi\n",
            "---\n\n---\n",
        ] {
            let j = job(text);
            assert!(j.is_valid(), "{text:?}: {:?}", j.diagnostics());
            assert!(j.keys().is_empty());
            assert_eq!(j.fields(), &JobFields::default());
        }
    }

    #[test]
    fn absent_schedule_is_unscheduled_not_an_error() {
        let j = job("---\nnotify: false\n---\nhi\n");
        assert!(j.is_valid());
        assert_eq!(j.fields().schedule, None);
        assert_eq!(j.fields().notify, Some(Notify::Never));
    }

    // #12 §2 "Frontmatter readings", serde-saphyr column.

    #[test]
    fn strict_unquoted_star_schedule_is_rejected() {
        let j = job("---\nschedule: */5 * * * *\nrecurring: true\n---\n");
        let d = only(&j);
        assert_eq!(d.kind, kind::YAML_INVALID);
        assert_eq!(d.line, Some(2));
        assert!(!j.is_valid());
        assert!(j.keys().is_empty());
    }

    #[test]
    fn strict_trailing_comment_is_a_comment() {
        let j = job("---\nschedule: \"0 9 * * *\" # note\nrecurring: true\n---\n");
        assert!(j.is_valid(), "{:?}", j.diagnostics());
        assert_eq!(j.fields().schedule.as_ref().unwrap().as_str(), "0 9 * * *");
    }

    #[test]
    fn strict_repeated_key_is_rejected() {
        let j = job("---\nmodel: a\nrecurring: true\nmodel: b\n---\n");
        let d = only(&j);
        assert_eq!(d.kind, kind::YAML_INVALID);
        assert_eq!(d.line, Some(4), "{}", d.message);
    }

    #[test]
    fn strict_mismatched_quotes_are_rejected() {
        let j = job("---\nschedule: '0 9 * * *\"\nrecurring: true\n---\n");
        assert_eq!(only(&j).kind, kind::YAML_INVALID);
    }

    #[test]
    fn strict_prose_line_is_rejected() {
        let j = job("---\nrecurring: true\nThis job does the thing\n---\n");
        assert_eq!(only(&j).kind, kind::YAML_INVALID);
    }

    #[test]
    fn strict_tab_indented_key_is_rejected() {
        let j = job("---\nrecurring: true\n\tmodel: haiku\n---\n");
        assert_eq!(only(&j).kind, kind::YAML_INVALID);
    }

    #[test]
    fn strict_scalars_keep_their_text() {
        let j = job("---\nschedule: \"0 9 * * *\"\nrecurring: yes\n---\n");
        assert!(j.is_valid());
        assert_eq!(j.keys()["recurring"].as_deref(), Some("yes"));
        assert_eq!(j.fields().recurring, Some(true));

        let j = job("---\ntimeout: 90s\n---\n");
        let d = only(&j);
        assert_eq!(d.kind, kind::TIMEOUT_INVALID);
        assert_eq!((d.line, d.column), (Some(2), Some(10)));
        assert_eq!(j.fields().timeout, None);
    }

    #[test]
    fn strict_nested_value_is_rejected() {
        assert_eq!(
            only(&job("---\nmodel:\n  name: x\n---\n")).kind,
            kind::YAML_INVALID
        );
        assert_eq!(only(&job("---\n- a\n- b\n---\n")).kind, kind::YAML_INVALID);
        assert_eq!(only(&job("---\njust text\n---\n")).kind, kind::YAML_INVALID);
    }

    #[test]
    fn value_grammar_errors() {
        let j = job(
            "---\nschedule: \"0 9 * * *\"\nrecurring: maybe\nenabled: on\nnotify: 1\n\
             retry: -1\nretry_delay: 5m\n---\n",
        );
        assert_eq!(
            kinds(&j),
            [
                kind::RECURRING_INVALID,
                kind::ENABLED_INVALID,
                kind::NOTIFY_INVALID,
                kind::RETRY_INVALID,
                kind::RETRY_DELAY_INVALID,
            ]
        );
        assert_eq!(j.diagnostics()[0].line, Some(3));
    }

    #[test]
    fn schedule_errors() {
        let j = job("---\nschedule: \"5/15 * * * *\"\nrecurring: true\n---\n");
        let d = only(&j);
        assert_eq!(d.kind, kind::SCHEDULE_INVALID);
        assert_eq!((d.line, d.column), (Some(2), Some(11)));

        let six = job("---\nschedule: \"0 0 9 * * *\"\nrecurring: true\n---\n");
        assert_eq!(only(&six).kind, kind::SCHEDULE_INVALID);

        let feb30 = job("---\nschedule: \"0 0 30 2 *\"\nrecurring: true\n---\n");
        assert_eq!(only(&feb30).kind, kind::SCHEDULE_NEVER_FIRES);
        assert_eq!(feb30.fields().schedule, None);
    }

    #[test]
    fn recurring_is_required_with_a_schedule() {
        let j = job("---\n\nschedule: \"@daily\"\n---\n");
        let d = only(&j);
        assert_eq!(d.kind, kind::RECURRING_MISSING);
        assert_eq!(d.line, Some(3));
        assert!(d.message.contains("recurring: false"));
        // `daily:` is an unknown key in the Rust Daemon (#12 §3).
        let j = job("---\nschedule: \"@daily\"\ndaily: true\n---\n");
        assert_eq!(kinds(&j), [kind::RECURRING_MISSING]);
        // An invalid schedule still reports the missing `recurring`.
        let j = job("---\nschedule: \"nope\"\n---\n");
        assert_eq!(kinds(&j), [kind::SCHEDULE_INVALID, kind::RECURRING_MISSING]);
        // Without a schedule, `recurring` is optional.
        assert!(job("---\nmodel: x\n---\n").is_valid());
    }

    #[test]
    fn null_and_empty_are_absent() {
        let j = job("---\nschedule:\nmodel: ~\nlauncher: \"\"\nlabel: null\nenabled:\n---\n");
        assert!(j.is_valid(), "{:?}", j.diagnostics());
        assert_eq!(j.fields(), &JobFields::default());
        assert!(j.keys().contains_key("model"));
    }

    #[test]
    fn strings_and_unknown_keys() {
        let j = job(
            "---\nlauncher: /opt/launchers/ccs-work.sh\nagent: helper\nlabel: daily\n\
             description: anything\nrecuring: true\n---\n",
        );
        assert_eq!(
            j.fields().launcher.as_deref(),
            Some("/opt/launchers/ccs-work.sh")
        );
        assert_eq!(j.fields().agent.as_deref(), Some("helper"));
        assert_eq!(j.fields().label.as_deref(), Some("daily"));
        assert_eq!(j.keys()["description"].as_deref(), Some("anything"));
        let d = only(&j);
        assert_eq!(d.kind, kind::KEY_UNKNOWN);
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!((d.line, d.column), (Some(6), Some(1)));
        assert!(j.is_valid(), "a warning doesn't stop the job");
    }

    #[test]
    fn comments_and_block_scalars_are_allowed() {
        let j = job(
            "---\n# when it runs\nschedule: >-\n  0 9\n  * * *\nrecurring: true # always\n\
             model: |\n  haiku\n---\n",
        );
        assert!(j.is_valid(), "{:?}", j.diagnostics());
        assert_eq!(j.fields().schedule.as_ref().unwrap().as_str(), "0 9 * * *");
        assert_eq!(j.fields().model.as_deref(), Some("haiku"));
    }

    #[test]
    fn debug_is_content_free() {
        let j = job("---\nmodel: x\n---\nsecret prompt\n");
        let shown = format!("{j:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }

    #[test]
    fn line_keys() {
        assert_eq!(line_key("schedule: x"), Some(("schedule", 10)));
        assert_eq!(line_key("schedule :x"), None);
        assert_eq!(line_key("schedule :  x"), Some(("schedule", 12)));
        assert_eq!(line_key("model:"), Some(("model", 6)));
        assert_eq!(line_key("url: http://x"), Some(("url", 5)));
        assert_eq!(line_key("a:b: c"), Some(("a:b", 5)));
        assert_eq!(line_key("  model: x"), None);
        assert_eq!(line_key("# model: x"), None);
        assert_eq!(line_key("- model: x"), None);
        assert_eq!(line_key("prose"), None);
    }
}
