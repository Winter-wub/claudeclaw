//! Line-level edits to a job file (#12 §2, architecture §4).
//!
//! Writes never re-serialise: every Daemon write (the one-shot finish, the Job import, the
//! Management API's edits) changes only the lines it names, so key order, quoting, comments and
//! unknown keys survive (#10, #12 §2). The result is text; `store` writes it atomically.

use std::fmt;

use crate::fields::{JobFields, Notify};
use crate::file::{Envelope, Line, line_key, lines};

/// One edit to a job file's text. Line numbers are 1-based file lines, as in
/// [`JobDiagnostic`](crate::JobDiagnostic), and always refer to the text the edits are applied
/// to, not to the text after earlier edits.
#[derive(Clone, PartialEq, Eq)]
pub enum LineEdit {
    /// Sets `key` to `value`, written with [`render_value`]. Replaces the key's line (and the
    /// indented lines that continue its value), or adds a line at the end of the frontmatter
    /// when the key is absent.
    Set { key: String, value: String },
    /// Removes the key's line and the indented lines that continue its value. Nothing happens
    /// when the key is absent.
    Remove { key: String },
    /// Inserts `text` as a new line after line `line`. Line 1 is the opening `---`, so `1`
    /// inserts at the top of the frontmatter.
    InsertAfter { line: u32, text: String },
    /// Replaces a frontmatter line's content, keeping its line break.
    ReplaceLine { line: u32, text: String },
    /// Turns a frontmatter line into a comment by prefixing `# `. TS ignores it too (#12 §2).
    CommentOut { line: u32 },
    /// Replaces everything after the closing `---` line with `body`, verbatim.
    ReplaceBody { body: String },
}

/// Content-free: values, lines and the body can be job content (architecture §2).
impl fmt::Debug for LineEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LineEdit::Set { key, value } => f
                .debug_struct("Set")
                .field("key", key)
                .field("value_bytes", &value.len())
                .finish(),
            LineEdit::Remove { key } => f.debug_struct("Remove").field("key", key).finish(),
            LineEdit::InsertAfter { line, text } => f
                .debug_struct("InsertAfter")
                .field("line", line)
                .field("text_bytes", &text.len())
                .finish(),
            LineEdit::ReplaceLine { line, text } => f
                .debug_struct("ReplaceLine")
                .field("line", line)
                .field("text_bytes", &text.len())
                .finish(),
            LineEdit::CommentOut { line } => {
                f.debug_struct("CommentOut").field("line", line).finish()
            }
            LineEdit::ReplaceBody { body } => f
                .debug_struct("ReplaceBody")
                .field("body_bytes", &body.len())
                .finish(),
        }
    }
}

/// Why a set of [`LineEdit`]s can't be applied. Nothing is changed when this is returned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The text has no `---` envelope, so there is no frontmatter to edit (#12 §2).
    #[error("the job file has no `---` frontmatter envelope")]
    NoEnvelope,
    /// A line number outside the frontmatter (or, for `InsertAfter`, outside the opening
    /// `---` and the frontmatter).
    #[error("line {0} is not a frontmatter line")]
    LineOutOfRange(u32),
    /// A key that isn't a plain `[A-Za-z0-9_-]+` name.
    #[error("`{0}` is not a plain frontmatter key")]
    InvalidKey(String),
    /// Line text with a line break in it.
    #[error("a line edit's text can't contain a line break")]
    MultiLine,
    /// Two edits change the same line.
    #[error("two edits change line {0}")]
    Overlap(u32),
}

/// A resolved edit: replace `from..to` with `insert`. Insertions have `from == to`.
struct Splice {
    from: usize,
    to: usize,
    insert: String,
    /// The first 1-based line it touches, for [`EditError::Overlap`].
    line: u32,
}

struct Doc<'a> {
    text: &'a str,
    lines: Vec<Line>,
    envelope: Envelope,
    eol: &'static str,
}

impl<'a> Doc<'a> {
    fn new(text: &'a str) -> Result<Doc<'a>, EditError> {
        let lines = lines(text);
        let envelope = Envelope::split(text, &lines).map_err(|_| EditError::NoEnvelope)?;
        let first = lines[0];
        let eol = if text[first.end..first.next].starts_with('\r') {
            "\r\n"
        } else {
            "\n"
        };
        Ok(Doc {
            text,
            lines,
            envelope,
            eol,
        })
    }

    fn content(&self, i: usize) -> &'a str {
        let l = self.lines[i];
        &self.text[l.start..l.end]
    }

    /// The 0-based index of a frontmatter line, from a 1-based line number.
    fn frontmatter_line(&self, line: u32) -> Result<usize, EditError> {
        let i = (line as usize).wrapping_sub(1);
        if (1..self.envelope.close).contains(&i) {
            Ok(i)
        } else {
            Err(EditError::LineOutOfRange(line))
        }
    }

    /// The key's line and the last line of its value: following lines that are indented,
    /// skipping blank lines between them (#12 §2 allows block scalars).
    fn key_span(&self, key: &str) -> Option<(usize, usize)> {
        let first = (1..self.envelope.close)
            .find(|&i| line_key(self.content(i)).is_some_and(|(k, _)| k == key))?;
        let mut last = first;
        for i in first + 1..self.envelope.close {
            let c = self.content(i);
            if c.trim().is_empty() {
                continue;
            }
            if c.starts_with([' ', '\t']) {
                last = i;
            } else {
                break;
            }
        }
        Some((first, last))
    }

    fn splice(&self, edit: &LineEdit) -> Result<Option<Splice>, EditError> {
        let single = |text: &str| {
            if text.contains(['\n', '\r']) {
                Err(EditError::MultiLine)
            } else {
                Ok(())
            }
        };
        let line_no = |i: usize| u32::try_from(i + 1).unwrap_or(u32::MAX);
        Ok(Some(match edit {
            LineEdit::Set { key, value } => {
                check_key(key)?;
                let line = format!("{key}: {}", render_value(value));
                match self.key_span(key) {
                    Some((first, last)) => Splice {
                        from: self.lines[first].start,
                        to: self.lines[last].end,
                        insert: line,
                        line: line_no(first),
                    },
                    None => {
                        let at = self.lines[self.envelope.close].start;
                        Splice {
                            from: at,
                            to: at,
                            insert: line + self.eol,
                            line: line_no(self.envelope.close),
                        }
                    }
                }
            }
            LineEdit::Remove { key } => {
                check_key(key)?;
                let Some((first, last)) = self.key_span(key) else {
                    return Ok(None);
                };
                Splice {
                    from: self.lines[first].start,
                    to: self.lines[last].next,
                    insert: String::new(),
                    line: line_no(first),
                }
            }
            LineEdit::InsertAfter { line, text } => {
                single(text)?;
                let i = if *line == 1 {
                    0
                } else {
                    self.frontmatter_line(*line)?
                };
                let at = self.lines[i].next;
                Splice {
                    from: at,
                    to: at,
                    insert: format!("{text}{}", self.eol),
                    line: *line + 1,
                }
            }
            LineEdit::ReplaceLine { line, text } => {
                single(text)?;
                let l = self.lines[self.frontmatter_line(*line)?];
                Splice {
                    from: l.start,
                    to: l.end,
                    insert: text.clone(),
                    line: *line,
                }
            }
            LineEdit::CommentOut { line } => {
                let l = self.lines[self.frontmatter_line(*line)?];
                Splice {
                    from: l.start,
                    to: l.end,
                    insert: format!("# {}", &self.text[l.start..l.end]),
                    line: *line,
                }
            }
            LineEdit::ReplaceBody { body } => {
                let close = self.lines[self.envelope.close];
                // A closing `---` on the last line has no break to put the body after.
                let insert = if close.end == close.next && !body.is_empty() {
                    format!("{}{body}", self.eol)
                } else {
                    body.clone()
                };
                Splice {
                    from: self.envelope.body,
                    to: self.text.len(),
                    insert,
                    line: line_no(self.envelope.close + 1),
                }
            }
        }))
    }
}

fn check_key(key: &str) -> Result<(), EditError> {
    let plain = !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !key.starts_with('-');
    if plain {
        Ok(())
    } else {
        Err(EditError::InvalidKey(key.to_owned()))
    }
}

/// Applies `edits` to `text`. Every byte outside the edited lines is kept: line breaks
/// (CRLF too), a BOM, comments, quoting, key order and unknown keys (#12 §2). New lines use
/// the file's line break. Edits are all resolved against `text` first, so their line numbers
/// don't shift; two edits that change the same line are an [`EditError::Overlap`], and
/// insertions at the same place keep their order.
pub fn apply(text: &str, edits: &[LineEdit]) -> Result<String, EditError> {
    let doc = Doc::new(text)?;
    let mut splices = Vec::with_capacity(edits.len());
    for edit in edits {
        if let Some(s) = doc.splice(edit)? {
            splices.push(s);
        }
    }
    // Stable: insertions at one offset keep the order they were given in.
    splices.sort_by_key(|s| (s.from, s.to));
    for pair in splices.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let a_replaces = a.from < a.to;
        let b_replaces = b.from < b.to;
        if b.from < a.to || (a_replaces && b_replaces && a.from == b.from) {
            return Err(EditError::Overlap(b.line.min(a.line).max(1)));
        }
    }
    let mut out = String::with_capacity(text.len() + 64);
    let mut at = 0;
    for s in &splices {
        out.push_str(&text[at..s.from]);
        out.push_str(&s.insert);
        at = s.to;
    }
    out.push_str(&text[at..]);
    Ok(out)
}

/// The one-shot finish (#12 §3): `enabled: false`, keeping `schedule:`. It replaces an existing
/// `enabled:` line, or inserts one right after `schedule:` (and its continuation lines). TS
/// reads the result as disabled too, so a rollback doesn't re-run the job (#14 rule 8).
pub fn finish_one_shot(text: &str) -> Result<String, EditError> {
    let doc = Doc::new(text)?;
    let set = LineEdit::Set {
        key: "enabled".to_owned(),
        value: "false".to_owned(),
    };
    let edit = match (doc.key_span("enabled"), doc.key_span("schedule")) {
        (None, Some((_, last))) => LineEdit::InsertAfter {
            line: u32::try_from(last + 1).unwrap_or(u32::MAX),
            text: "enabled: false".to_owned(),
        },
        _ => set,
    };
    apply(text, &[edit])
}

/// Writes a frontmatter value so serde-saphyr reads back exactly `value` and, where possible,
/// TS's line parser does too (#12 §2: a changed line keeps the meaning TS reads, #14).
///
/// Plain when that is safe, else double-quoted, else single-quoted. A value with both quote
/// kinds, a backslash or a control character gets an escaped double-quoted form, which TS
/// can't read the same way.
pub fn render_value(value: &str) -> String {
    let control = value.chars().any(char::is_control);
    if is_plain_safe(value) {
        value.to_owned()
    } else if !control && !value.contains(['"', '\\']) {
        format!("\"{value}\"")
    } else if !control && !value.contains('\'') {
        format!("'{value}'")
    } else {
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for c in value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

/// A conservative test for a YAML plain scalar that reads back as itself, as a string, and
/// that TS's quote stripping (`src/jobs.ts:31`) leaves alone.
fn is_plain_safe(v: &str) -> bool {
    let Some(first) = v.chars().next() else {
        return false;
    };
    !first.is_whitespace()
        && !v.ends_with(char::is_whitespace)
        && !"-?:,[]{}#&*!|>'\"%@`".contains(first)
        && !v.contains(": ")
        && !v.contains(" #")
        && !v.ends_with(':')
        && !v.contains(['"', '\'', '\t'])
        && !v.chars().any(char::is_control)
        && !matches!(v, "~" | "null" | "Null" | "NULL")
}

/// Writes a new job file from typed fields and a prompt: the `---` envelope, one line per set
/// field in [`KNOWN_KEYS`](crate::KNOWN_KEYS) order, then the prompt. For `jobs.create`'s form
/// and the preview's form → raw rendering (management-api §2.4). Existing files are never
/// re-rendered; they get [`LineEdit`]s.
pub fn render_job(fields: &JobFields, prompt: &str) -> String {
    let mut out = String::from("---\n");
    let mut line = |key: &str, value: Option<String>| {
        if let Some(v) = value {
            out.push_str(&format!("{key}: {}\n", render_value(&v)));
        }
    };
    let notify = fields.notify.map(|n| match n {
        Notify::Always => "true".to_owned(),
        Notify::Never => "false".to_owned(),
        Notify::OnError => "error".to_owned(),
    });
    line(
        "schedule",
        fields.schedule.as_ref().map(|s| s.as_str().to_owned()),
    );
    line("recurring", fields.recurring.map(|b| b.to_string()));
    line("enabled", fields.enabled.map(|b| b.to_string()));
    line("notify", notify);
    line("model", fields.model.clone());
    line("timeout", fields.timeout.map(|n| n.to_string()));
    line("retry", fields.retry.map(|n| n.to_string()));
    line("retry_delay", fields.retry_delay.map(|n| n.to_string()));
    line("launcher", fields.launcher.clone());
    line("label", fields.label.clone());
    line("agent", fields.agent.clone());
    out.push_str("---\n");
    out.push_str(prompt);
    if !prompt.is_empty() && !prompt.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::Schedule;
    use crate::file::JobFile;

    const FILE: &str = "---\n# how often\nschedule: \"0 9 */3 * *\"   # every third day\n\
                        recurring: true\ncustom: kept\nmodel: >-\n  claude-haiku\n\n  -latest\n\
                        timeout: 600\n---\nPrompt line one.\n\nPrompt line two.\n";

    fn set(key: &str, value: &str) -> LineEdit {
        LineEdit::Set {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Asserts that `after` is `before` with exactly `old` swapped for `new` at one place, and
    /// every other byte unchanged.
    fn only_changed(before: &str, after: &str, old: &str, new: &str) {
        let at = before
            .find(old)
            .unwrap_or_else(|| panic!("{old:?} not in file"));
        assert_eq!(&after[..at], &before[..at], "prefix changed");
        assert_eq!(&after[at..at + new.len()], new);
        assert_eq!(
            &after[at + new.len()..],
            &before[at + old.len()..],
            "suffix changed"
        );
    }

    #[test]
    fn set_replaces_only_the_key_line() {
        let out = apply(FILE, &[set("timeout", "900")]).unwrap();
        only_changed(FILE, &out, "timeout: 600", "timeout: 900");
        assert_eq!(JobFile::parse(out.as_bytes()).fields().timeout, Some(900));
    }

    #[test]
    fn set_replaces_a_value_with_its_continuation_lines() {
        let out = apply(FILE, &[set("model", "haiku")]).unwrap();
        only_changed(
            FILE,
            &out,
            "model: >-\n  claude-haiku\n\n  -latest",
            "model: haiku",
        );
    }

    #[test]
    fn set_replaces_a_trailing_comment_too() {
        let out = apply(FILE, &[set("schedule", "3-31/3 9 * * *")]).unwrap();
        only_changed(
            FILE,
            &out,
            "schedule: \"0 9 */3 * *\"   # every third day",
            "schedule: 3-31/3 9 * * *",
        );
    }

    #[test]
    fn set_appends_an_absent_key_at_the_end_of_the_frontmatter() {
        let out = apply(FILE, &[set("notify", "error"), set("retry", "2")]).unwrap();
        only_changed(
            FILE,
            &out,
            "---\nPrompt",
            "notify: error\nretry: 2\n---\nPrompt",
        );
        let job = JobFile::parse(out.as_bytes());
        assert!(job.is_valid(), "{:?}", job.diagnostics());
        assert_eq!(job.keys()["custom"].as_deref(), Some("kept"));
    }

    #[test]
    fn remove_drops_the_key_lines() {
        let out = apply(
            FILE,
            &[LineEdit::Remove {
                key: "model".into(),
            }],
        )
        .unwrap();
        only_changed(FILE, &out, "model: >-\n  claude-haiku\n\n  -latest\n", "");
        assert_eq!(
            apply(FILE, &[LineEdit::Remove { key: "nope".into() }]).unwrap(),
            FILE
        );
    }

    #[test]
    fn insert_after_comment_out_and_replace_line() {
        let out = apply(
            FILE,
            &[LineEdit::InsertAfter {
                line: 1,
                text: "enabled: true".into(),
            }],
        )
        .unwrap();
        only_changed(FILE, &out, "---\n# how", "---\nenabled: true\n# how");

        let out = apply(FILE, &[LineEdit::CommentOut { line: 5 }]).unwrap();
        only_changed(FILE, &out, "custom: kept", "# custom: kept");
        assert!(!JobFile::parse(out.as_bytes()).keys().contains_key("custom"));

        let out = apply(
            FILE,
            &[LineEdit::ReplaceLine {
                line: 4,
                text: "recurring: false".into(),
            }],
        )
        .unwrap();
        only_changed(FILE, &out, "recurring: true", "recurring: false");
    }

    #[test]
    fn several_edits_use_the_original_line_numbers() {
        let out = apply(
            FILE,
            &[
                LineEdit::InsertAfter {
                    line: 2,
                    text: "a: 1".into(),
                },
                LineEdit::CommentOut { line: 5 },
                set("timeout", "1"),
            ],
        )
        .unwrap();
        let want = FILE
            .replace("# how often\n", "# how often\na: 1\n")
            .replace("custom: kept", "# custom: kept")
            .replace("timeout: 600", "timeout: 1");
        assert_eq!(out, want);
    }

    #[test]
    fn replace_body_keeps_the_frontmatter() {
        let out = apply(
            FILE,
            &[LineEdit::ReplaceBody {
                body: "New.\n".into(),
            }],
        )
        .unwrap();
        let fm_end = FILE.find("---\nPrompt").unwrap() + 4;
        assert_eq!(&out[..fm_end], &FILE[..fm_end]);
        assert_eq!(&out[fm_end..], "New.\n");
        let bare = apply(
            "---\nmodel: x\n---",
            &[LineEdit::ReplaceBody { body: "B".into() }],
        );
        assert_eq!(bare.unwrap(), "---\nmodel: x\n---\nB");
    }

    #[test]
    fn crlf_and_bom_survive() {
        let text = "\u{feff}---\r\nschedule: \"@daily\"\r\nrecurring: true\r\n---\r\nhi\r\n";
        let out = apply(text, &[set("recurring", "false"), set("notify", "false")]).unwrap();
        assert_eq!(
            out,
            "\u{feff}---\r\nschedule: \"@daily\"\r\nrecurring: false\r\nnotify: false\r\n---\r\nhi\r\n"
        );
        assert!(JobFile::parse(out.as_bytes()).fields().is_one_shot());
    }

    #[test]
    fn edit_errors() {
        assert_eq!(
            apply("no envelope", &[set("a", "b")]),
            Err(EditError::NoEnvelope)
        );
        let line = |line| LineEdit::CommentOut { line };
        assert_eq!(apply(FILE, &[line(1)]), Err(EditError::LineOutOfRange(1)));
        assert_eq!(apply(FILE, &[line(11)]), Err(EditError::LineOutOfRange(11)));
        assert_eq!(apply(FILE, &[line(0)]), Err(EditError::LineOutOfRange(0)));
        assert!(apply(FILE, &[line(10)]).is_ok());
        assert_eq!(
            apply(FILE, &[set("bad key", "x")]),
            Err(EditError::InvalidKey("bad key".into()))
        );
        let multi = LineEdit::InsertAfter {
            line: 2,
            text: "a\nb".into(),
        };
        assert_eq!(apply(FILE, &[multi]), Err(EditError::MultiLine));
        assert_eq!(
            apply(FILE, &[line(4), set("recurring", "false")]),
            Err(EditError::Overlap(4))
        );
        // Inserting next to a replaced line is fine.
        let next_to = LineEdit::InsertAfter {
            line: 4,
            text: "x: 1".into(),
        };
        assert!(apply(FILE, &[line(4), next_to]).is_ok());
    }

    #[test]
    fn one_shot_finish_inserts_after_schedule() {
        let text = "---\nschedule: \"30 8 * * *\"\nrecurring: false\nnotify: true\n---\nGo.\n";
        let out = finish_one_shot(text).unwrap();
        only_changed(text, &out, "\nrecurring", "\nenabled: false\nrecurring");
        let job = JobFile::parse(out.as_bytes());
        assert_eq!(job.fields().enabled, Some(false));
        assert!(job.fields().schedule.is_some(), "schedule is kept");
        assert!(job.is_valid());
    }

    #[test]
    fn one_shot_finish_replaces_an_enabled_line() {
        let text = "---\nenabled: yes # on\nschedule: \"30 8 * * *\"\nrecurring: false\n---\nGo.\n";
        let out = finish_one_shot(text).unwrap();
        only_changed(text, &out, "enabled: yes # on", "enabled: false");
        // Finishing twice changes nothing more.
        assert_eq!(finish_one_shot(&out).unwrap(), out);
    }

    #[test]
    fn one_shot_finish_after_a_multi_line_schedule() {
        let text = "---\nschedule: >-\n  30 8\n  * * *\nrecurring: false\n---\n";
        let out = finish_one_shot(text).unwrap();
        only_changed(text, &out, "* * *\n", "* * *\nenabled: false\n");
        assert_eq!(JobFile::parse(out.as_bytes()).fields().enabled, Some(false));
    }

    /// What TS's line parser reads from `key: <rendered>` (`src/jobs.ts:30-32`).
    fn ts_reads(rendered: &str) -> String {
        let v = rendered.trim();
        let v = v.strip_prefix(['"', '\'']).unwrap_or(v);
        v.strip_suffix(['"', '\'']).unwrap_or(v).to_owned()
    }

    #[test]
    fn rendered_values_read_back() {
        let values = [
            "0 9 * * *",
            "*/5 * * * *",
            "@daily",
            "true",
            "1",
            "haiku",
            "/opt/launchers/ccs-work.sh",
            "C:\\launchers\\ccs.cmd",
            "null",
            "~",
            "",
            " padded ",
            "a: b",
            "a #b",
            "say \"hi\"",
            "it's",
            "both ' and \"",
            "tab\there",
            "line\nbreak",
            "- dash",
            "x:",
            "% pct",
        ];
        for v in values {
            let r = render_value(v);
            let text = format!("---\nk: {r}\n---\n");
            let job = JobFile::parse(text.as_bytes());
            assert!(
                job.diagnostics().is_empty(),
                "{v:?} → {r}: {:?}",
                job.diagnostics()
            );
            assert_eq!(job.keys()["k"].as_deref(), Some(v), "{v:?} → {r}");
            let both_quotes = v.contains('"') && v.contains('\'');
            let ts_safe = !v.contains(['\\', '\n', '\t']) && !both_quotes;
            if ts_safe && v.trim() == v && !v.is_empty() {
                assert_eq!(ts_reads(&r), v, "TS reading of {r}");
            }
        }
        assert_eq!(render_value("0 9 * * *"), "0 9 * * *");
        assert_eq!(render_value("*/5 * * * *"), "\"*/5 * * * *\"");
    }

    #[test]
    fn render_job_writes_a_valid_file() {
        let fields = JobFields {
            schedule: Some(Schedule::parse("*/15 9-17 * * MON-FRI").unwrap()),
            recurring: Some(true),
            notify: Some(Notify::OnError),
            timeout: Some(300),
            launcher: Some("direct".into()),
            ..JobFields::default()
        };
        let text = render_job(&fields, "Check the inbox.");
        assert_eq!(
            text,
            "---\nschedule: \"*/15 9-17 * * MON-FRI\"\nrecurring: true\nnotify: error\n\
             timeout: 300\nlauncher: direct\n---\nCheck the inbox.\n"
        );
        let job = JobFile::parse(text.as_bytes());
        assert!(job.diagnostics().is_empty(), "{:?}", job.diagnostics());
        assert_eq!(job.fields(), &fields);
        assert_eq!(job.prompt(), "Check the inbox.");
    }

    #[test]
    fn debug_is_content_free() {
        let shown = format!(
            "{:?}",
            LineEdit::ReplaceBody {
                body: "secret".into()
            }
        );
        assert!(!shown.contains("secret"));
        let shown = format!("{:?}", set("model", "secret"));
        assert!(!shown.contains("secret") && shown.contains("model"));
    }
}
