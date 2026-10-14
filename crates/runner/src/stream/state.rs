//! Folding parsed lines into typed events and the facts `judge()` needs.

use std::collections::HashMap;
use std::fmt;

use serde_json::Value;

use super::hint::TranscriptHint;
use super::item::{
    AssistantEvent, ContentBlock, Event, ResultEvent, StreamItem, SystemEvent, Usage, UserEvent,
};
use crate::version::ClaudeCodeVersion;

/// What a stream says while it runs, for `RunEvents` (architecture §4 runner "Output").
///
/// The integration PR wraps these, plus the final [`super::RunOutcome`], into `RunEvents`.
#[derive(Clone, PartialEq)]
pub enum StreamEvent {
    /// The session ID, first seen or changed (`system` or `result` events, TS
    /// `src/runner.ts:513`).
    SessionId(String),
    /// `init.claude_code_version` (#42 D2).
    ClaudeCodeVersion(ClaudeCodeVersion),
    /// From `init.memory_paths.auto`, after the shape check (#39 D1).
    TranscriptHint(TranscriptHint),
    /// New reply text. A new assistant message after text sends a lone `"\n"` first, as TS does
    /// (`src/runner.ts:523`).
    TextDelta { text: String, subagent: bool },
    /// A tool call, with TS's short summary such as `Read(src/main.rs)` (`src/runner.ts:588`).
    ToolUse {
        id: String,
        name: String,
        summary: String,
        subagent: bool,
    },
    /// A tool's result: its first line, at most 80 chars, `Error: `-prefixed on error, or
    /// `done` (`src/runner.ts:542`). `name` is `None` when the call wasn't seen.
    ToolResult {
        tool_use_id: String,
        name: Option<String>,
        summary: String,
        is_error: bool,
    },
    /// An `Agent` tool call (TS `src/runner.ts:1590`).
    AgentSpawn { id: String, description: String },
    /// That call's result (TS `src/runner.ts:1612`).
    AgentDone {
        id: String,
        description: String,
        result: String,
    },
}

impl fmt::Debug for StreamEvent {
    // Content-free (architecture §2): kinds, IDs, tool names and sizes only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamEvent::SessionId(id) => write!(f, "SessionId({id})"),
            StreamEvent::ClaudeCodeVersion(v) => write!(f, "ClaudeCodeVersion({})", v.raw),
            StreamEvent::TranscriptHint(h) => write!(f, "{h:?}"),
            StreamEvent::TextDelta { text, subagent } => {
                write!(f, "TextDelta({} bytes, subagent: {subagent})", text.len())
            }
            StreamEvent::ToolUse {
                id, name, subagent, ..
            } => write!(f, "ToolUse({id}, {name}, subagent: {subagent})"),
            StreamEvent::ToolResult {
                tool_use_id,
                name,
                is_error,
                ..
            } => write!(
                f,
                "ToolResult({tool_use_id}, {name:?}, is_error: {is_error})"
            ),
            StreamEvent::AgentSpawn { id, .. } => write!(f, "AgentSpawn({id})"),
            StreamEvent::AgentDone { id, result, .. } => {
                write!(f, "AgentDone({id}, {} bytes)", result.len())
            }
        }
    }
}

/// What the stream said once `init` arrived (#39, #42).
#[derive(Clone, PartialEq, Default)]
pub(super) struct Init {
    pub(super) version: Option<ClaudeCodeVersion>,
    pub(super) api_key_source: Option<String>,
    pub(super) transcript_hint: Option<TranscriptHint>,
    /// `init.model`: the model ID Claude Code resolved for the session.
    pub(super) model: Option<String>,
}

/// One content block of the current message, as far as text deltas need it.
#[derive(Clone, PartialEq)]
enum SeenBlock {
    Text(String),
    ToolUse(String),
    Other,
}

impl SeenBlock {
    fn of(block: &ContentBlock) -> Option<Self> {
        match block {
            ContentBlock::Text { text } => Some(SeenBlock::Text(text.clone())),
            ContentBlock::ToolUse { id, .. } => Some(SeenBlock::ToolUse(id.clone())),
            ContentBlock::ToolResult { .. } => None,
            ContentBlock::Other => Some(SeenBlock::Other),
        }
    }
}

/// Text-delta bookkeeping for one thread: the main thread or one subagent (TS
/// `src/runner.ts:494-541`, kept per thread).
#[derive(Clone, Default)]
struct ThreadText {
    message_id: Option<String>,
    /// The current message's blocks so far, by index.
    blocks: Vec<SeenBlock>,
    /// Text was delivered for the current message.
    delivered: bool,
}

/// A run's stream so far: feed it every stdout line with [`StreamState::observe`], then pass it
/// to [`super::judge`]. Pure: no IO, no clock, no tokio (architecture §4 runner `stream`).
#[derive(Clone, Default)]
pub struct StreamState {
    pub(super) json_lines: u64,
    pub(super) noise_lines: u64,
    pub(super) session_id: Option<String>,
    pub(super) init: Option<Init>,
    pub(super) result: Option<ResultEvent>,
    pub(super) last_main_usage: Option<Usage>,
    /// The last main-thread assistant `message.model` (#39 D4).
    pub(super) last_main_model: Option<String>,
    /// Keyed by `parent_tool_use_id`; the main thread is `None`.
    threads: HashMap<Option<String>, ThreadText>,
    /// tool_use_id → tool name.
    pending_tools: HashMap<String, String>,
    /// tool_use_id → Agent description.
    pending_agents: HashMap<String, String>,
}

impl fmt::Debug for StreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamState")
            .field("json_lines", &self.json_lines)
            .field("noise_lines", &self.noise_lines)
            .field("init", &self.init.is_some())
            .field("result", &self.result.is_some())
            .finish_non_exhaustive()
    }
}

impl StreamState {
    /// An empty stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses `line` with [`super::parse_line`] and observes it.
    pub fn push_line(&mut self, line: &str) -> Vec<StreamEvent> {
        self.observe(&super::parse_line(line))
    }

    /// Folds one line in and returns the events it produced, in order.
    pub fn observe(&mut self, item: &StreamItem) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        match item {
            StreamItem::Noise(_) => self.noise_lines += 1,
            StreamItem::Malformed(_) => {
                self.json_lines += 1;
                tracing::debug!("stream-json line without a usable type tag");
            }
            StreamItem::Event(event) => {
                self.json_lines += 1;
                match event {
                    Event::System(s) => self.system(s, &mut out),
                    Event::Assistant(a) => self.assistant(a, &mut out),
                    Event::User(u) => self.user(u, &mut out),
                    Event::Result(r) => {
                        self.session(r.session_id.as_deref(), &mut out);
                        self.result = Some(r.clone());
                    }
                    Event::Other => {}
                }
            }
        }
        out
    }

    /// Whether a `result` event has been read; a `result` read before a timeout wins (#47 D3).
    pub fn result_seen(&self) -> bool {
        self.result.is_some()
    }

    /// Whether any line was a JSON object (#26 D3, #42 D3).
    pub fn json_seen(&self) -> bool {
        self.json_lines > 0
    }

    /// The session ID, from the newest `system` or `result` event.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn session(&mut self, id: Option<&str>, out: &mut Vec<StreamEvent>) {
        if let Some(id) = id
            && self.session_id.as_deref() != Some(id)
        {
            self.session_id = Some(id.to_owned());
            out.push(StreamEvent::SessionId(id.to_owned()));
        }
    }

    fn system(&mut self, s: &SystemEvent, out: &mut Vec<StreamEvent>) {
        self.session(s.session_id.as_deref(), out);
        if s.subtype.as_deref() != Some("init") {
            return;
        }
        let version = s.claude_code_version.as_deref().map(ClaudeCodeVersion::new);
        if let Some(v) = &version {
            out.push(StreamEvent::ClaudeCodeVersion(v.clone()));
        }
        let auto = s.memory_paths.as_ref().and_then(|m| m.auto.as_deref());
        let transcript_hint = auto.and_then(|p| match TranscriptHint::from_memory_path(p) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::debug!(error = %e, "init memory path gives no transcript hint");
                None
            }
        });
        if let Some(h) = &transcript_hint {
            out.push(StreamEvent::TranscriptHint(h.clone()));
        }
        self.init = Some(Init {
            version,
            api_key_source: s.api_key_source.clone(),
            transcript_hint,
            model: s.model.clone(),
        });
    }

    fn assistant(&mut self, a: &AssistantEvent, out: &mut Vec<StreamEvent>) {
        let Some(msg) = &a.message else { return };
        let subagent = a.parent_tool_use_id.is_some();
        if !subagent {
            if let Some(usage) = msg.usage {
                self.last_main_usage = Some(usage);
            }
            // Claude Code's own error messages say `<synthetic>`, which is no model.
            if let Some(model) = msg.model.as_deref().filter(|m| *m != "<synthetic>") {
                self.last_main_model = Some(model.to_owned());
            }
        }

        let thread = self
            .threads
            .entry(a.parent_tool_use_id.clone())
            .or_default();
        // TS `src/runner.ts:522-526`: a new message after text gets a "\n" separator, here
        // within the same thread only.
        if msg.id != thread.message_id {
            if thread.delivered {
                out.push(StreamEvent::TextDelta {
                    text: "\n".into(),
                    subagent,
                });
            }
            thread.message_id = msg.id.clone();
            thread.blocks.clear();
            thread.delivered = false;
        }

        // An event holds either a snapshot of the message so far (the blocks already seen, at
        // the same indexes, the last text possibly grown) or its next blocks. Only a snapshot's
        // already-seen blocks are de-duplicated.
        let incoming: Vec<(&ContentBlock, SeenBlock)> = msg
            .content
            .iter()
            .filter_map(|b| SeenBlock::of(b).map(|s| (b, s)))
            .collect();
        let seen = std::mem::take(&mut thread.blocks);
        let snapshot = is_snapshot(&seen, incoming.iter().map(|(_, s)| s));
        let first_new = if snapshot { seen.len() } else { 0 };

        let mut text = String::new();
        for (i, (block, _)) in incoming.iter().enumerate() {
            match block {
                ContentBlock::Text { text: t } => match seen.get(i) {
                    Some(SeenBlock::Text(old)) if snapshot => text.push_str(&t[old.len()..]),
                    _ => text.push_str(t),
                },
                ContentBlock::ToolUse { id, name, input } if i >= first_new => {
                    self.pending_tools.insert(id.clone(), name.clone());
                    out.push(StreamEvent::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        summary: tool_call_summary(name, input),
                        subagent,
                    });
                    if name == "Agent" {
                        let description = field_text(input, "description")
                            .or_else(|| field_text(input, "prompt"))
                            .unwrap_or_else(|| "Running background task...".into());
                        self.pending_agents.insert(id.clone(), description.clone());
                        out.push(StreamEvent::AgentSpawn {
                            id: id.clone(),
                            description,
                        });
                    }
                }
                _ => {}
            }
        }
        let incoming = incoming.into_iter().map(|(_, s)| s);
        thread.blocks = if snapshot {
            incoming.collect()
        } else {
            seen.into_iter().chain(incoming).collect()
        };
        if !text.is_empty() {
            thread.delivered = true;
            out.push(StreamEvent::TextDelta { text, subagent });
        }
    }

    fn user(&mut self, u: &UserEvent, out: &mut Vec<StreamEvent>) {
        let Some(msg) = &u.message else { return };
        for block in &msg.content {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            else {
                continue;
            };
            let name = self.pending_tools.remove(tool_use_id);
            let text = tool_result_text(content);
            let first: String = text
                .split('\n')
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect();
            let summary = if *is_error {
                format!("Error: {first}")
            } else if first.is_empty() {
                "done".into()
            } else {
                first
            };
            out.push(StreamEvent::ToolResult {
                tool_use_id: tool_use_id.clone(),
                name,
                summary,
                is_error: *is_error,
            });
            if let Some(description) = self.pending_agents.remove(tool_use_id) {
                // TS `src/runner.ts:1615`: a string as-is, anything else as JSON.
                let result = match content {
                    Value::String(s) => s.clone(),
                    Value::Null => "\"\"".into(),
                    other => other.to_string(),
                };
                out.push(StreamEvent::AgentDone {
                    id: tool_use_id.clone(),
                    description,
                    result,
                });
            }
        }
    }
}

/// Whether `incoming` repeats `seen` at the same indexes: same kinds, same tool-use IDs, equal
/// texts except the last seen one, which may have grown.
fn is_snapshot<'a>(seen: &[SeenBlock], incoming: impl Iterator<Item = &'a SeenBlock>) -> bool {
    let incoming: Vec<&SeenBlock> = incoming.collect();
    if seen.is_empty() || incoming.len() < seen.len() {
        return false;
    }
    seen.iter()
        .zip(&incoming)
        .enumerate()
        .all(|(i, (old, new))| match (old, new) {
            (SeenBlock::Text(o), SeenBlock::Text(n)) if i + 1 == seen.len() => {
                n.starts_with(o.as_str())
            }
            (SeenBlock::Text(o), SeenBlock::Text(n)) => o == n,
            (SeenBlock::ToolUse(o), SeenBlock::ToolUse(n)) => o == n,
            (SeenBlock::Other, SeenBlock::Other) => true,
            _ => false,
        })
}

/// JS `String(v ?? "")` for the values tool inputs carry.
fn value_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// `input[key]` when present and not null.
fn field_text(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .filter(|v| !v.is_null())
        .map(|v| value_text(Some(v)))
}

/// TS `formatToolCallSummary` (`src/runner.ts:588-601`).
pub(super) fn tool_call_summary(name: &str, input: &Value) -> String {
    let s = |key: &str, max: usize| -> String {
        value_text(input.get(key)).chars().take(max).collect()
    };
    match name {
        "Write" | "Edit" | "Read" => format!("{name}({})", s("file_path", 50)),
        "Bash" => format!("Bash({})", s("command", 60)),
        "Grep" => {
            let path = field_text(input, "path").unwrap_or_else(|| ".".into());
            let path: String = path.chars().take(50).collect();
            format!("Grep({} in {path})", s("pattern", 50))
        }
        "Glob" => format!("Glob({})", s("pattern", 50)),
        "WebSearch" => format!("WebSearch({})", s("query", 50)),
        "WebFetch" => format!("WebFetch({})", s("url", 60)),
        _ => format!("{name}(...)"),
    }
}

/// TS `extractToolResultText` (`src/runner.ts:609-618`).
fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
