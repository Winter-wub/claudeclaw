//! One stdout line as a typed value (architecture §4 runner `stream`).
//!
//! Every field is optional or defaulted, and a field of the wrong JSON type reads as absent, so
//! a Claude Code change to one field can't hide a whole event. Only a JSON object with no usable
//! `type` tag is [`StreamItem::Malformed`].

use std::collections::BTreeMap;
use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// One stdout line (architecture §4 runner `stream`). Lines are never dropped: what isn't an
/// event is kept for the run log.
#[derive(Clone, PartialEq)]
pub enum StreamItem {
    /// A JSON object with a `type` tag.
    Event(Event),
    /// A JSON object whose `type` tag is missing or not a string. It still counts as a JSON
    /// line for the "no JSON line" rules (#26, #42).
    Malformed(String),
    /// A line that isn't a JSON object: a wrapper's banner, a blank line (#5, #8).
    Noise(String),
}

impl fmt::Debug for StreamItem {
    // Content-free (architecture §2).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamItem::Event(e) => f.debug_tuple("Event").field(e).finish(),
            StreamItem::Malformed(l) => write!(f, "Malformed({} bytes)", l.len()),
            StreamItem::Noise(l) => write!(f, "Noise({} bytes)", l.len()),
        }
    }
}

/// A stream-json event, tagged by `type`, with a catch-all for every other type
/// (architecture §4 runner `stream`).
#[derive(Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    System(SystemEvent),
    Assistant(AssistantEvent),
    User(UserEvent),
    Result(ResultEvent),
    /// Any other `type` (`stream_event`, `tool_use`, types added later).
    #[serde(other)]
    Other,
}

impl fmt::Debug for Event {
    // Content-free (architecture §2): the kind and a few non-content facts.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::System(s) => write!(f, "System({:?})", s.subtype),
            Event::Assistant(a) => write!(
                f,
                "Assistant(blocks: {}, subagent: {})",
                a.message.as_ref().map_or(0, |m| m.content.len()),
                a.parent_tool_use_id.is_some()
            ),
            Event::User(u) => write!(
                f,
                "User(blocks: {})",
                u.message.as_ref().map_or(0, |m| m.content.len())
            ),
            Event::Result(r) => write!(
                f,
                "Result(subtype: {:?}, is_error: {}, api_error_status: {:?})",
                r.subtype, r.is_error, r.api_error_status
            ),
            Event::Other => f.write_str("Other"),
        }
    }
}

/// A `system` event. `init` is the one the runner reads (#39, #42); the init-only fields are
/// `None` on other subtypes.
#[derive(Clone, PartialEq, Deserialize)]
pub struct SystemEvent {
    #[serde(default, deserialize_with = "lenient")]
    pub subtype: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub session_id: Option<String>,
    /// Since Claude Code 2.0.30 (#42 E2).
    #[serde(default, deserialize_with = "lenient")]
    pub claude_code_version: Option<String>,
    /// `init.apiKeySource` (#39 E7).
    #[serde(default, rename = "apiKeySource", deserialize_with = "lenient")]
    pub api_key_source: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub model: Option<String>,
    /// `init.memory_paths`, `@internal` in the SDK schema (#39 E1, E2).
    #[serde(default, deserialize_with = "lenient")]
    pub memory_paths: Option<MemoryPaths>,
}

/// `init.memory_paths` (#39 E1).
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct MemoryPaths {
    /// `<config dir>/projects/<project dir>/memory/`.
    #[serde(default, deserialize_with = "lenient")]
    pub auto: Option<String>,
}

/// An `assistant` event: one API response's content, main thread or subagent.
#[derive(Clone, PartialEq, Deserialize)]
pub struct AssistantEvent {
    #[serde(default, deserialize_with = "lenient")]
    pub message: Option<AssistantMessage>,
    /// `null` on the main thread; the spawning `Agent` tool use's ID in a subagent (#39 D4).
    #[serde(default, deserialize_with = "lenient")]
    pub parent_tool_use_id: Option<String>,
}

/// `assistant.message`.
#[derive(Clone, PartialEq, Deserialize)]
pub struct AssistantMessage {
    #[serde(default, deserialize_with = "lenient")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "blocks")]
    pub content: Vec<ContentBlock>,
    #[serde(default, deserialize_with = "lenient")]
    pub usage: Option<Usage>,
}

/// A `user` event: tool results coming back.
#[derive(Clone, PartialEq, Deserialize)]
pub struct UserEvent {
    #[serde(default, deserialize_with = "lenient")]
    pub message: Option<UserMessage>,
    #[serde(default, deserialize_with = "lenient")]
    pub parent_tool_use_id: Option<String>,
}

/// `user.message`.
#[derive(Clone, PartialEq, Deserialize)]
pub struct UserMessage {
    #[serde(default, deserialize_with = "blocks")]
    pub content: Vec<ContentBlock>,
}

/// One content block, with a catch-all (`thinking`, `image`, later types).
#[derive(Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        #[serde(default)]
        tool_use_id: String,
        #[serde(default)]
        content: Value,
        #[serde(default, deserialize_with = "lenient_or_default")]
        is_error: bool,
    },
    #[serde(other)]
    Other,
}

/// The `result` event: the run's end as Claude Code reports it (#5, #8, #39).
#[derive(Clone, PartialEq, Deserialize)]
pub struct ResultEvent {
    /// `success`, `error_during_execution`, `error_max_turns`, … A 429 or 401 still says
    /// `success`; [`ResultEvent::is_error`] is what counts (#5).
    #[serde(default, deserialize_with = "lenient")]
    pub subtype: Option<String>,
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub is_error: bool,
    /// The reply text.
    #[serde(default, deserialize_with = "lenient")]
    pub result: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub session_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub num_turns: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    pub duration_ms: Option<u64>,
    /// Session-cumulative on `--resume` (#39 E4).
    #[serde(default, deserialize_with = "lenient")]
    pub total_cost_usd: Option<f64>,
    /// This run only, summed over its requests (#39 E4).
    #[serde(default, deserialize_with = "lenient")]
    pub usage: Option<Usage>,
    /// Per model, session-cumulative on `--resume` (#39 E4, E5, E10).
    #[serde(
        default,
        rename = "modelUsage",
        deserialize_with = "lenient_or_default"
    )]
    pub model_usage: BTreeMap<String, ModelUsage>,
    /// The provider's HTTP status, since Claude Code 2.1.110 (#42 E8).
    #[serde(default, deserialize_with = "lenient")]
    pub api_error_status: Option<u16>,
    /// E.g. `No conversation found with session ID: <id>` (#39 E3).
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub errors: Vec<String>,
}

/// Token counts as Claude Code reports them in `message.usage` and `result.usage`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Usage {
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub input_tokens: u64,
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub output_tokens: u64,
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub cache_read_input_tokens: u64,
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    /// The context in use: input + cache read + cache creation (#39 D4).
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
    }
}

/// One model's entry in `result.modelUsage` (#39 E5, E10).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub input_tokens: u64,
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub output_tokens: u64,
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub cache_read_input_tokens: u64,
    #[serde(default, deserialize_with = "lenient_or_default")]
    pub cache_creation_input_tokens: u64,
    #[serde(default, rename = "costUSD", deserialize_with = "lenient")]
    pub cost_usd: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub context_window: Option<u64>,
    #[serde(default, deserialize_with = "cost_basis")]
    pub cost_basis: CostBasis,
    /// `firstParty`, … (#39 E5).
    #[serde(default, deserialize_with = "lenient")]
    pub provider: Option<String>,
}

/// `modelUsage[m].costBasis` (#39 E5, D9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CostBasis {
    /// Also what a missing field means, as Claude Code documents (#39 D9).
    #[default]
    List,
    Managed,
    /// Claude Code had no price for the model; any other value reads as this too.
    Unknown,
}

/// A field of the wrong type reads as absent.
fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

/// A field of the wrong type, or `null`, reads as the default.
fn lenient_or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    Ok(lenient(d)?.unwrap_or_default())
}

fn cost_basis<'de, D: Deserializer<'de>>(d: D) -> Result<CostBasis, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) if s == "list" => CostBasis::List,
        Value::String(s) if s == "managed" => CostBasis::Managed,
        // `null` is as good as missing.
        Value::Null => CostBasis::List,
        _ => CostBasis::Unknown,
    })
}

/// `message.content`: an array of blocks, or a bare string (one text block). A block that
/// doesn't fit its type reads as [`ContentBlock::Other`].
fn blocks<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ContentBlock>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(text) => vec![ContentBlock::Text { text }],
        Value::Array(items) => items
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap_or(ContentBlock::Other))
            .collect(),
        _ => Vec::new(),
    })
}
