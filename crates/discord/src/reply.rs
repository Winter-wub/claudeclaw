//! The outbound side of one Turn: the streaming preview, tool-category reactions, the final
//! reply split at 2000 characters, images from the outbox, and the bot's own progress reactions
//! (architecture §4 `claudeclaw-discord`, §6; TS `handleMessageCreate`,
//! `src/commands/discord.ts:1316-1375`, plus its helpers).
//!
//! Order of a Turn's end, as in TS: the tool reactions are cleared and the streaming placeholder
//! is deleted first, then the reply reacts to the origin (`[react:x]`), then the reply itself is
//! sent — split, with outbox images attached to the last chunk — and the 👀 "thinking" reaction
//! is removed last of all (`src/commands/discord.ts:1339-1374`).
//!
//! The 👀 reaction is added as soon as the Turn starts, before the first event: TS adds it at the
//! top of the run and drops it in its `finally`, however the run ended
//! (`src/commands/discord.ts:1083-1085`, `:1371-1374`). It is one of the bot's progress reactions,
//! so [`crate::notifier`] clears it too when a Turn is interrupted or reaped (#29, #79).
//!
//! One TS piece is deliberately not ported: an outbound image size cap, because TS has none. A
//! file over 25 MB is skipped with a warning rather than read into memory, where TS would let
//! Discord reject the upload.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use claudeclaw_chat::{RunEvent, RunHandle, extract_react_directive, reply_chunks};
use futures_util::future::BoxFuture;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, MessageMarker};

use crate::rest::{MAX_MESSAGE_LEN, Rest, UploadFile, debug_send_failed, warn_send_failed};
use crate::seams::{ReplySink, ReplyTarget};

/// The outbox the reply reads images from when settings name no roots (`DEFAULT_IMAGE_OUTPUT_ROOT`,
/// `src/config.ts:19`; TS `extractImagePaths`, `src/commands/discord.ts:404`).
pub const DEFAULT_IMAGE_OUTPUT_ROOT: &str = "outbox/discord";

/// How far before the request an image's modification time may be and still be sent; older files
/// are pre-existing, not this run's output (TS `PATH_SKEW_MS`, `src/commands/discord.ts:397`).
const PATH_SKEW: Duration = Duration::from_millis(30_000);

/// The largest image the sink will read into memory to upload. TS has no cap and lets Discord
/// answer `413`; skipping early keeps a multi-gigabyte file out of memory.
const MAX_UPLOAD_BYTES: u64 = 25 * 1024 * 1024;

/// The gap between two reaction calls (TS `TOOL_REACTION_SPACING_MS`,
/// `src/commands/discord.ts:339`). 1 ms in tests so they do not idle.
pub(crate) const TOOL_REACTION_SPACING: Duration =
    Duration::from_millis(if cfg!(test) { 1 } else { 300 });

/// The streaming preview's edit throttle (TS `STREAM_EDIT_INTERVAL_MS`,
/// `src/commands/discord.ts:829`). Zero in tests so every delta edits at once.
const STREAM_EDIT_INTERVAL: Duration = Duration::from_millis(if cfg!(test) { 0 } else { 1500 });

/// The placeholder posted on the first delta or tool event (TS, `src/commands/discord.ts:869`).
const STREAM_PLACEHOLDER: &str = "⏳";

/// The most text the preview shows, leaving room for the italic markers (TS
/// `STREAM_CONTENT_MAX`, `src/commands/discord.ts:830`).
const STREAM_CONTENT_MAX: usize = MAX_MESSAGE_LEN - 10;

/// The eight tool-category reactions, in the switch order of `toolReactionEmoji`
/// (`src/commands/discord.ts:39-65`).
pub(crate) const TOOL_REACTION_EMOJIS: [&str; 8] = [
    "\u{1F50C}",         // mcp__*    🔌
    "\u{1F4D6}",         // Read, Grep, Glob, LS    📖
    "\u{270F}\u{FE0F}",  // Edit, MultiEdit, Write, NotebookEdit    ✏️
    "\u{1F4BB}",         // Bash    💻
    "\u{1F310}",         // WebFetch, WebSearch    🌐
    "\u{1F916}",         // Agent, Task    🤖
    "\u{1F9E9}",         // Skill    🧩
    "\u{1F6E0}\u{FE0F}", // default    🛠️
];

/// The 👀 reaction put on the triggering message while a Turn runs (`THINKING_REACTION`,
/// `src/commands/discord.ts:36`). Added when the Turn starts, removed last, after the reply
/// (`:1083-1085`, `:1371-1374`).
pub const THINKING_REACTION: &str = "\u{1F440}";

/// The reaction emoji for a tool, straight from TS `toolReactionEmoji`
/// (`src/commands/discord.ts:39-65`).
pub(crate) fn tool_reaction_emoji(tool_name: &str) -> &'static str {
    if tool_name.starts_with("mcp__") {
        return "\u{1F50C}";
    }
    match tool_name {
        "Read" | "Grep" | "Glob" | "LS" => "\u{1F4D6}",
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => "\u{270F}\u{FE0F}",
        "Bash" => "\u{1F4BB}",
        "WebFetch" | "WebSearch" => "\u{1F310}",
        "Agent" | "Task" => "\u{1F916}",
        "Skill" => "\u{1F9E9}",
        _ => "\u{1F6E0}\u{FE0F}",
    }
}

/// The Discord [`ReplySink`], registered with the adapter's gateway side. `streaming` mirrors
/// `discord.streaming`; `image_roots` mirrors `discord.imageOutputRoots` (empty means
/// [`DEFAULT_IMAGE_OUTPUT_ROOT`]).
pub struct DiscordReplySink {
    rest: Arc<Rest>,
    streaming: bool,
    image_roots: Vec<PathBuf>,
}

impl DiscordReplySink {
    /// A sink over the shared REST client.
    pub fn new(rest: Arc<Rest>, streaming: bool, image_roots: Vec<PathBuf>) -> Self {
        DiscordReplySink {
            rest,
            streaming,
            image_roots,
        }
    }

    /// Sends the final reply: plain chunks, or the last chunk with all outbox images attached
    /// (TS `sendMessageWithImages`, `src/commands/discord.ts:433-446`).
    async fn send_reply(&self, channel: Id<ChannelMarker>, text: &str, paths: &[PathBuf]) {
        if paths.is_empty() {
            for chunk in reply_chunks(text, MAX_MESSAGE_LEN) {
                if let Err(err) = self.rest.send(channel, &chunk, None).await {
                    warn_send_failed(&err);
                }
            }
            return;
        }

        // TS `discordMessageChunks(text || "\u{200b}")`, then the last chunk carries the files.
        let mut chunks = reply_chunks(text, MAX_MESSAGE_LEN);
        if chunks.is_empty() {
            chunks.push("\u{200b}".to_owned());
        }
        let upload = chunks.pop().unwrap_or_else(|| "\u{200b}".to_owned());
        for chunk in &chunks {
            if let Err(err) = self.rest.send(channel, chunk, None).await {
                warn_send_failed(&err);
            }
        }

        let files = self.read_uploads(paths).await;
        if files.is_empty() {
            // Every image was skipped: send the text so it is not lost.
            if let Err(err) = self.rest.send(channel, &upload, None).await {
                warn_send_failed(&err);
            }
            return;
        }
        if let Err(err) = self.rest.send_files(channel, &upload, &files).await {
            warn_send_failed(&err);
        }
    }

    /// Reads the outbox files, skipping (with a warning) any over [`MAX_UPLOAD_BYTES`] or that
    /// cannot be read.
    async fn read_uploads(&self, paths: &[PathBuf]) -> Vec<UploadFile> {
        let mut files = Vec::new();
        for path in paths {
            let over_cap = match tokio::fs::metadata(path).await {
                Ok(meta) if meta.len() > MAX_UPLOAD_BYTES => Some(meta.len()),
                Ok(_) => None,
                Err(err) => {
                    tracing::warn!("an outbox image could not be read: {err}");
                    continue;
                }
            };
            if let Some(size) = over_cap {
                tracing::warn!("an outbox image over 25 MB was not uploaded ({size} bytes)");
                continue;
            }
            match tokio::fs::read(path).await {
                Ok(bytes) => files.push(UploadFile {
                    name: path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("image")
                        .to_owned(),
                    bytes,
                }),
                Err(err) => tracing::warn!("an outbox image could not be read: {err}"),
            }
        }
        files
    }
}

impl std::fmt::Debug for DiscordReplySink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordReplySink")
            .field("streaming", &self.streaming)
            .field("image_root_count", &self.image_roots.len())
            .finish()
    }
}

impl ReplySink for DiscordReplySink {
    fn deliver(&self, target: ReplyTarget, handle: RunHandle) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let started_at = SystemTime::now();
            // TS reacts with 👀 as soon as the run starts — before any work, not on the first
            // event — and removes it in its `finally`, after the reply (`src/commands/discord.ts:1083-1085`,
            // `:1371-1374`). A failed add is not removed later.
            let thinking = match self
                .rest
                .add_reaction(target.channel, target.origin, THINKING_REACTION)
                .await
            {
                Ok(()) => true,
                Err(err) => {
                    warn_send_failed(&err);
                    false
                }
            };
            let mut stream = self
                .streaming
                .then(|| StreamPreview::start(self.rest.clone(), target.channel));
            let mut reactions =
                ToolReactions::start(self.rest.clone(), target.channel, target.origin);

            let RunHandle {
                mut events,
                outcome,
            } = handle;
            while let Some(event) = events.recv().await {
                match event {
                    RunEvent::TextDelta(text) => {
                        if let Some(stream) = stream.as_mut() {
                            stream.append(&text);
                        }
                    }
                    RunEvent::ToolUse { name, summary } => {
                        reactions.on_tool(&name);
                        if let Some(stream) = stream.as_mut() {
                            stream.line(&format!("● {summary}"));
                        }
                    }
                    RunEvent::ToolResult { name, ok } => {
                        if let Some(stream) = stream.as_mut() {
                            let status = if ok { "done" } else { "Error" };
                            stream.line(&format!("  ⎿  [{name}] {status}"));
                        }
                    }
                    // TS's Discord stream renders no agent markers.
                    RunEvent::AgentSpawn { .. } | RunEvent::AgentDone { .. } => {}
                }
            }

            let outcome = outcome.await;
            reactions.clear().await;
            if let Some(stream) = stream.take() {
                stream.finalize().await;
            }
            if let Ok(outcome) = outcome {
                let directive = extract_react_directive(&outcome.text);
                if let Some(emoji) = directive.emoji.as_deref()
                    && let Err(err) = self
                        .rest
                        .add_reaction(target.channel, target.origin, emoji)
                        .await
                {
                    warn_send_failed(&err);
                }

                let (paths, text) =
                    extract_image_paths(&directive.text, &self.image_roots, started_at);
                let text = if text.is_empty() {
                    "(empty response)".to_owned()
                } else {
                    text
                };
                self.send_reply(target.channel, &text, &paths).await;
            }
            // The sender was dropped without an outcome (nothing to reply to), or the reply is
            // out: either way the thinking reaction goes last, as in TS's `finally`.

            if thinking
                && let Err(err) = self
                    .rest
                    .remove_reaction(target.channel, target.origin, THINKING_REACTION)
                    .await
            {
                warn_send_failed(&err);
            }
        })
    }
}

/// The tool-category reactions, applied one at a time through a queue so a burst of tool calls
/// does not trip Discord's per-channel reaction limit (TS `makeToolReactionTracker`,
/// `src/commands/discord.ts:341-376`). The queue lives on its own task, so the Turn's event loop
/// is never blocked by a reaction call.
struct ToolReactions {
    tx: mpsc::UnboundedSender<ReactionMsg>,
    added: Vec<&'static str>,
    task: JoinHandle<()>,
}

enum ReactionMsg {
    Add(&'static str),
    Remove(&'static str),
    Barrier(oneshot::Sender<()>),
}

impl ToolReactions {
    fn start(
        rest: Arc<Rest>,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
    ) -> ToolReactions {
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(reaction_loop(rest, channel, message, rx));
        ToolReactions {
            tx,
            added: Vec::new(),
            task,
        }
    }

    /// Adds the category reaction for `tool_name`, at most once per emoji.
    fn on_tool(&mut self, tool_name: &str) {
        let emoji = tool_reaction_emoji(tool_name);
        if self.added.contains(&emoji) {
            return;
        }
        self.added.push(emoji);
        let _ = self.tx.send(ReactionMsg::Add(emoji));
    }

    /// Removes every reaction added this run, once the queue has caught up (TS `clear`,
    /// `src/commands/discord.ts:370-374`).
    async fn clear(&mut self) {
        for emoji in std::mem::take(&mut self.added) {
            let _ = self.tx.send(ReactionMsg::Remove(emoji));
        }
        let (done, wait) = oneshot::channel();
        let _ = self.tx.send(ReactionMsg::Barrier(done));
        let _ = wait.await;
    }
}

impl Drop for ToolReactions {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn reaction_loop(
    rest: Arc<Rest>,
    channel: Id<ChannelMarker>,
    message: Id<MessageMarker>,
    mut rx: mpsc::UnboundedReceiver<ReactionMsg>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            ReactionMsg::Add(emoji) => {
                if let Err(err) = rest.add_reaction(channel, message, emoji).await {
                    debug_send_failed(&err);
                }
            }
            ReactionMsg::Remove(emoji) => {
                if let Err(err) = rest.remove_reaction(channel, message, emoji).await {
                    debug_send_failed(&err);
                }
            }
            ReactionMsg::Barrier(done) => {
                let _ = done.send(());
            }
        }
        tokio::time::sleep(TOOL_REACTION_SPACING).await;
    }
}

/// The streaming preview: a placeholder message edited at most every [`STREAM_EDIT_INTERVAL`] to
/// hold the tail of the reply, deleted when the Turn ends (TS `makeDiscordStreamCallback`,
/// `src/commands/discord.ts:843-943`). Its edits run on their own task, like the reactions.
struct StreamPreview {
    tx: mpsc::UnboundedSender<StreamMsg>,
    task: JoinHandle<()>,
}

enum StreamMsg {
    Append(String),
    Line(String),
    Finalize(oneshot::Sender<()>),
}

impl StreamPreview {
    fn start(rest: Arc<Rest>, channel: Id<ChannelMarker>) -> StreamPreview {
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(stream_loop(rest, channel, rx));
        StreamPreview { tx, task }
    }

    fn append(&mut self, text: &str) {
        let _ = self.tx.send(StreamMsg::Append(text.to_owned()));
    }

    fn line(&mut self, line: &str) {
        let _ = self.tx.send(StreamMsg::Line(line.to_owned()));
    }

    /// Deletes the placeholder and waits for the queue to drain (TS `finalize`,
    /// `src/commands/discord.ts:928-940`).
    async fn finalize(self) {
        let (done, wait) = oneshot::channel();
        let _ = self.tx.send(StreamMsg::Finalize(done));
        let _ = wait.await;
        self.task.abort();
    }
}

impl Drop for StreamPreview {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn stream_loop(
    rest: Arc<Rest>,
    channel: Id<ChannelMarker>,
    mut rx: mpsc::UnboundedReceiver<StreamMsg>,
) {
    let mut accumulated = String::new();
    let mut message: Option<Id<MessageMarker>> = None;
    let mut deadline: Option<Instant> = None;
    loop {
        // A far deadline when nothing is pending; `select` still wakes immediately on a message.
        let sleep_until = deadline
            .map(tokio::time::Instant::from_std)
            .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            biased;
            received = rx.recv() => match received {
                None => break,
                Some(StreamMsg::Append(text)) => {
                    accumulated.push_str(&text);
                    post_placeholder(&rest, channel, &mut message).await;
                    deadline.get_or_insert_with(|| Instant::now() + STREAM_EDIT_INTERVAL);
                }
                Some(StreamMsg::Line(line)) => {
                    if !accumulated.is_empty() {
                        accumulated.push('\n');
                    }
                    accumulated.push_str(&line);
                    post_placeholder(&rest, channel, &mut message).await;
                    deadline.get_or_insert_with(|| Instant::now() + STREAM_EDIT_INTERVAL);
                }
                Some(StreamMsg::Finalize(done)) => {
                    if let Some(id) = message
                        && let Err(err) = rest.delete(channel, id).await
                    {
                        debug_send_failed(&err);
                    }
                    let _ = done.send(());
                    break;
                }
            },
            () = tokio::time::sleep_until(sleep_until) => {
                if let Some(id) = message {
                    let content = preview_content(&accumulated);
                    if let Err(err) = rest.edit(channel, id, &content).await {
                        debug_send_failed(&err);
                    }
                }
                deadline = None;
            }
        }
    }
}

/// Posts the placeholder once, remembering its ID; a failure leaves it unposted and is retried
/// never, matching TS (`placeholderPosted` is set before the POST,
/// `src/commands/discord.ts:861-877`).
async fn post_placeholder(
    rest: &Rest,
    channel: Id<ChannelMarker>,
    message: &mut Option<Id<MessageMarker>>,
) {
    if message.is_some() {
        return;
    }
    match rest.send(channel, STREAM_PLACEHOLDER, None).await {
        Ok(id) => *message = Some(id),
        Err(err) => debug_send_failed(&err),
    }
}

/// The preview's content: the tail of the reply, italicised with `_`s escaped (TS `scheduleEdit`,
/// `src/commands/discord.ts:884-886`).
fn preview_content(accumulated: &str) -> String {
    format!(
        "_{}_",
        escape_italic(last_units(accumulated, STREAM_CONTENT_MAX))
    )
}

/// TS `escapeItalic` (`src/commands/discord.ts:832-834`).
fn escape_italic(text: &str) -> String {
    text.replace('_', "\\_")
}

/// The longest suffix of `text` of at most `max` UTF-16 code units, always starting at a
/// character boundary, so an emoji is never split (the same divergence as
/// [`chat::split_message`](claudeclaw_chat::split_message)).
fn last_units(text: &str, max: usize) -> &str {
    let mut units = 0;
    for (index, c) in text.char_indices().rev() {
        let width = c.len_utf16();
        if units + width > max {
            return &text[index + c.len_utf8()..];
        }
        units += width;
    }
    text
}

/// Pulls absolute image paths out of `text`, returning the paths and the text with those paths
/// removed (TS `extractImagePaths`, `src/commands/discord.ts:399-431`).
///
/// A path is kept only when it resolves inside one of the roots and was modified at or after the
/// request started (minus [`PATH_SKEW`]); anything else stays in the text as written.
fn extract_image_paths(
    text: &str,
    allowed_roots: &[PathBuf],
    started_at: SystemTime,
) -> (Vec<PathBuf>, String) {
    let roots: Vec<PathBuf> = if allowed_roots.is_empty() {
        vec![PathBuf::from(DEFAULT_IMAGE_OUTPUT_ROOT)]
    } else {
        allowed_roots.to_vec()
    };
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .map(|root| std::fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
        .collect();
    let floor = started_at
        .checked_sub(PATH_SKEW)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut paths = Vec::new();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let token_start = index;
        while index < text.len() && !is_js_space(next_char(text, index)) {
            index += next_char(text, index).len_utf8();
        }
        let token = &text[token_start..index];
        if token.starts_with('/') && is_image_token(token) {
            if let Some(resolved) = confined_recent(token, &canonical_roots, floor) {
                paths.push(resolved);
                // The token is dropped; the whitespace after it is copied below.
            } else {
                out.push_str(token);
            }
        } else {
            out.push_str(token);
        }
        let space_start = index;
        while index < text.len() && is_js_space(next_char(text, index)) {
            index += next_char(text, index).len_utf8();
        }
        out.push_str(&text[space_start..index]);
    }

    let cleaned = fold_blank_lines(&out);
    (paths, js_trim(&cleaned).to_owned())
}

/// The character at a byte index (which is always a boundary here).
fn next_char(text: &str, index: usize) -> char {
    text[index..].chars().next().unwrap_or('\0')
}

/// Whether `token` is an absolute image path with a known extension (the regex
/// `IMAGE_PATH_RE`, `src/commands/discord.ts:396`).
fn is_image_token(token: &str) -> bool {
    if !token.starts_with('/') {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp"]
        .iter()
        .any(|ext| lower.ends_with(ext) && token.len() > ext.len() + 1)
}

/// Canonicalizes `token`, returning it when it is inside one of `roots` and recent enough
/// (TS `extractImagePaths`'s per-match body, `src/commands/discord.ts:411-427`).
fn confined_recent(token: &str, roots: &[PathBuf], floor: SystemTime) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(token).ok()?;
    let confined = roots
        .iter()
        .any(|root| resolved == *root || resolved.starts_with(root));
    if !confined {
        return None;
    }
    let modified = std::fs::metadata(&resolved).ok()?.modified().ok()?;
    (modified >= floor).then_some(resolved)
}

/// TS `.replace(/\n{3,}/g, "\n\n")`.
fn fold_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    for c in text.chars() {
        if c == '\n' {
            run += 1;
            if run > 2 {
                continue;
            }
        } else {
            run = 0;
        }
        out.push(c);
    }
    out
}

/// JavaScript's `trim` (ECMAScript *WhiteSpace* plus *LineTerminator*). Duplicated from
/// `chat`'s `js` module, which is private to that crate.
fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// JS `\s`: the same set `chat::js` trims on. Duplicated here for the same reason.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use claudeclaw_chat::{ChatOutcome, OutcomeKind, RunEvent, RunSender};
    use tempfile::TempDir;

    use super::*;
    use crate::rest::test_server::{Request, Server};

    fn target() -> ReplyTarget {
        ReplyTarget {
            channel: Id::new(123),
            origin: Id::new(456),
        }
    }

    /// A sink over `server` with streaming off and no image roots.
    fn sink(server: &Server, streaming: bool) -> DiscordReplySink {
        DiscordReplySink::new(Arc::new(server.connect()), streaming, Vec::new())
    }

    /// Runs one Turn: `events`, then `text` as the outcome.
    async fn run(sink: &DiscordReplySink, server: &Server, events: Vec<RunEvent>, text: &str) {
        let (sender, handle) = RunHandle::channel(16);
        let RunSender {
            events: events_tx,
            outcome,
        } = sender;
        for event in events {
            events_tx.send(event).await.unwrap();
        }
        drop(events_tx);
        outcome
            .send(ChatOutcome {
                kind: OutcomeKind::Ok,
                text: text.to_owned(),
            })
            .unwrap();
        sink.deliver(target(), handle).await;
        let _ = server;
    }

    fn posts_to(requests: &[Request], path: &str) -> Vec<Request> {
        requests
            .iter()
            .filter(|request| request.method == "POST" && request.path == path)
            .cloned()
            .collect()
    }

    #[test]
    fn tool_reaction_emoji_matches_ts() {
        let cases: &[(&str, &str)] = &[
            ("mcp__server__tool", "\u{1F50C}"),
            ("Read", "\u{1F4D6}"),
            ("Grep", "\u{1F4D6}"),
            ("Glob", "\u{1F4D6}"),
            ("LS", "\u{1F4D6}"),
            ("Edit", "\u{270F}\u{FE0F}"),
            ("MultiEdit", "\u{270F}\u{FE0F}"),
            ("Write", "\u{270F}\u{FE0F}"),
            ("NotebookEdit", "\u{270F}\u{FE0F}"),
            ("Bash", "\u{1F4BB}"),
            ("WebFetch", "\u{1F310}"),
            ("WebSearch", "\u{1F310}"),
            ("Agent", "\u{1F916}"),
            ("Task", "\u{1F916}"),
            ("Skill", "\u{1F9E9}"),
            ("SomethingElse", "\u{1F6E0}\u{FE0F}"),
        ];
        for &(tool, emoji) in cases {
            assert_eq!(tool_reaction_emoji(tool), emoji, "{tool}");
        }
        // The eight distinct categories are exactly the fixed set.
        let mut distinct: Vec<&str> = cases.iter().map(|(t, _)| tool_reaction_emoji(t)).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), TOOL_REACTION_EMOJIS.len());
    }

    #[test]
    fn preview_helpers_match_ts() {
        assert_eq!(escape_italic("a_b__c"), "a\\_b\\_\\_c");
        assert_eq!(last_units("abcdef", 3), "def");
        assert_eq!(last_units("abcdef", 100), "abcdef");
        // Two-unit emoji: a suffix never splits one.
        assert_eq!(last_units("a🔥b", 2), "b");
        assert_eq!(last_units("a🔥b", 3), "🔥b");
        assert_eq!(preview_content("hi"), "_hi_");
    }

    #[tokio::test]
    async fn the_final_reply_reacts_then_sends() {
        let server = Server::start().await;
        let sink = sink(&server, false);
        run(&sink, &server, vec![], "nice work [react:🔥]").await;

        let requests = server.requests();
        // The 👀 thinking reaction goes on first, then the origin's 🔥, then the reply, and 👀
        // comes off last (TS `src/commands/discord.ts:1083-1085`, `:1347`, `:1371-1374`).
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].method, "PUT");
        assert!(requests[0].path.contains("/reactions/%F0%9F%91%80/@me"));
        assert_eq!(requests[1].method, "PUT");
        assert!(requests[1].path.contains("/reactions/%F0%9F%94%A5/@me"));
        assert_eq!(requests[2].method, "POST");
        assert_eq!(requests[2].text(), r#"{"content":"nice work"}"#);
        assert_eq!(requests[3].method, "DELETE");
        assert!(requests[3].path.contains("/reactions/%F0%9F%91%80/@me"));
    }

    #[tokio::test]
    async fn an_empty_reply_is_the_empty_response_line() {
        let server = Server::start().await;
        let sink = sink(&server, false);
        run(&sink, &server, vec![], "   ").await;
        let reply = server
            .requests()
            .into_iter()
            .find(|request| request.method == "POST" && request.path.ends_with("/messages"))
            .expect("the reply was posted");
        assert_eq!(reply.text(), r#"{"content":"(empty response)"}"#);
    }

    #[tokio::test]
    async fn thinking_is_removed_when_no_outcome_arrives() {
        let server = Server::start().await;
        let sink = sink(&server, false);
        // The sender is dropped without an outcome: there is nothing to reply to, but the 👀
        // reaction must still come off (TS's `finally`, `src/commands/discord.ts:1371-1374`).
        let (sender, handle) = RunHandle::channel(16);
        drop(sender);
        sink.deliver(target(), handle).await;

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "PUT");
        assert!(requests[0].path.contains("/reactions/%F0%9F%91%80/@me"));
        assert_eq!(requests[1].method, "DELETE");
        assert!(requests[1].path.contains("/reactions/%F0%9F%91%80/@me"));
    }

    #[tokio::test]
    async fn a_long_reply_is_split_at_2000() {
        let server = Server::start().await;
        let sink = sink(&server, false);
        run(&sink, &server, vec![], &"a".repeat(2001)).await;
        let posts = posts_to(&server.requests(), "/api/v10/channels/123/messages");
        assert_eq!(posts.len(), 2);
        let first: serde_json::Value = serde_json::from_slice(&posts[0].body).unwrap();
        let second: serde_json::Value = serde_json::from_slice(&posts[1].body).unwrap();
        assert_eq!(first["content"].as_str().unwrap().len(), 2000);
        assert_eq!(second["content"], "a");
    }

    #[tokio::test]
    async fn tool_reactions_are_added_once_per_category_and_cleared() {
        let server = Server::start().await;
        let sink = sink(&server, false);
        run(
            &sink,
            &server,
            vec![
                RunEvent::ToolUse {
                    name: "Read".into(),
                    summary: "read a file".into(),
                },
                RunEvent::ToolUse {
                    name: "Grep".into(),
                    summary: "search".into(),
                },
                RunEvent::ToolUse {
                    name: "Bash".into(),
                    summary: "run".into(),
                },
            ],
            "done",
        )
        .await;

        let requests = server.requests();
        // The 👀 thinking reaction opens the run and closes it; the tool categories go between.
        assert_eq!(requests[0].method, "PUT");
        assert!(requests[0].path.contains("/reactions/%F0%9F%91%80/@me"));
        let last = requests.last().expect("requests were served");
        assert_eq!(last.method, "DELETE");
        assert!(last.path.contains("/reactions/%F0%9F%91%80/@me"));

        let added: Vec<&Request> = requests
            .iter()
            .filter(|r| {
                r.method == "PUT"
                    && r.path.contains("/reactions/")
                    && !r.path.contains("%F0%9F%91%80")
            })
            .collect();
        // 📖 once (Read and Grep share it) and 💻 once.
        assert_eq!(added.len(), 2);
        let cleared: Vec<&Request> = requests
            .iter()
            .filter(|r| {
                r.method == "DELETE"
                    && r.path.contains("/reactions/")
                    && !r.path.contains("%F0%9F%91%80")
            })
            .collect();
        assert_eq!(cleared.len(), 2);
    }

    #[tokio::test]
    async fn streaming_posts_edits_and_deletes_the_placeholder() {
        let server = Server::start().await;
        let sink = sink(&server, true);
        run(
            &sink,
            &server,
            vec![
                RunEvent::TextDelta("hello".into()),
                RunEvent::ToolUse {
                    name: "Bash".into(),
                    summary: "ls".into(),
                },
            ],
            "final answer",
        )
        .await;

        let requests = server.requests();
        let placeholder = requests
            .iter()
            .find(|r| r.method == "POST" && r.text().contains("⏳"))
            .expect("placeholder posted");
        assert!(placeholder.path.ends_with("/channels/123/messages"));
        assert!(
            requests
                .iter()
                .any(|r| r.method == "PATCH" && r.text().contains("● ls")),
            "an edit carried the tool summary"
        );
        assert!(
            requests
                .iter()
                .any(|r| r.method == "DELETE" && r.path.ends_with("/messages/456")),
            "the placeholder was deleted at finalize"
        );
        assert!(
            server
                .requests()
                .iter()
                .any(|r| r.method == "POST" && r.text().contains(r#""content":"final answer""#))
        );
    }

    #[tokio::test]
    async fn streaming_off_posts_no_placeholder() {
        let server = Server::start().await;
        let sink = sink(&server, false);
        run(
            &sink,
            &server,
            vec![RunEvent::TextDelta("hello".into())],
            "final answer",
        )
        .await;
        assert!(
            !server.requests().iter().any(|r| r.method == "PATCH"),
            "no preview edits when streaming is off"
        );
    }

    #[tokio::test]
    async fn outbox_images_are_attached_to_the_last_chunk() {
        let dir = TempDir::new().unwrap();
        let image = dir.path().join("plot.png");
        std::fs::write(&image, b"not really a png").unwrap();
        let server = Server::start().await;
        let rest = Arc::new(server.connect());
        let sink = DiscordReplySink::new(rest, false, vec![dir.path().to_path_buf()]);

        let text = format!("here is the chart {} done", image.display());
        run(&sink, &server, vec![], &text).await;

        let requests = server.requests();
        let upload = requests
            .iter()
            .find(|request| request.method == "POST" && request.path.ends_with("/messages"))
            .expect("the image reply was posted");
        let body = upload.text();
        assert!(body.contains("here is the chart"), "{body}");
        assert!(body.contains("plot.png"), "{body}");
        assert!(body.contains("payload_json"), "{body}");
        // The raw path is gone from the message text.
        assert!(!body.contains(&image.display().to_string()), "{body}");
    }

    #[test]
    fn image_paths_outside_a_root_or_too_old_are_kept() {
        let dir = TempDir::new().unwrap();
        let inside = dir.path().join("fresh.png");
        std::fs::write(&inside, b"x").unwrap();
        let outside = TempDir::new().unwrap();
        let stranger = outside.path().join("stranger.png");
        std::fs::write(&stranger, b"x").unwrap();

        let text = format!(
            "kept {} and {} and {}",
            inside.display(),
            stranger.display(),
            inside.display()
        );
        let roots = vec![dir.path().to_path_buf()];

        // A request that starts later than the files' modification time sees them as
        // pre-existing (older than the 30 s skew) and keeps both raw paths.
        let later = SystemTime::now() + Duration::from_secs(3600);
        let (paths, cleaned) = extract_image_paths(&text, &roots, later);
        assert!(paths.is_empty(), "{paths:?}");
        assert!(cleaned.contains("fresh.png"), "{cleaned}");

        // A request starting now picks up the in-root file, and only the in-root one. TS
        // `String.replace` runs the callback once per regex match, so the repeated path is
        // reported twice (`src/commands/discord.ts:414`): no de-duplication.
        let now = SystemTime::now();
        let (paths, cleaned) = extract_image_paths(&text, &roots, now);
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(paths.iter().all(|path| path.ends_with("fresh.png")));
        assert!(cleaned.contains("stranger.png"), "{cleaned}");
        // The attached path is gone and the trailing whitespace is trimmed (JS `trim`).
        assert!(!cleaned.contains("fresh.png"), "{cleaned}");
        assert!(cleaned.ends_with("and"), "{cleaned}");
    }

    #[test]
    fn the_default_root_is_used_when_none_is_configured() {
        let (paths, cleaned) = extract_image_paths("nothing here", &[], SystemTime::now());
        assert!(paths.is_empty());
        assert_eq!(cleaned, "nothing here");
    }

    #[test]
    fn image_token_matching_matches_the_ts_regex() {
        assert!(is_image_token("/a/b.png"));
        assert!(is_image_token("/a.JPEG"));
        assert!(is_image_token("/x.webp"));
        assert!(!is_image_token("a/b.png"), "not preceded by a boundary");
        assert!(!is_image_token("/.png"), "no name before the extension");
        assert!(!is_image_token("/a.pngx"));
        assert!(!is_image_token("/a.txt"));
    }

    #[tokio::test]
    async fn debug_is_content_free() {
        let sink = DiscordReplySink::new(
            Arc::new(Rest::for_test(
                &secrecy::SecretString::from("top-secret"),
                &crate::DiscordEndpoints::default(),
            )),
            true,
            vec![PathBuf::from("hidden-root")],
        );
        let got = format!("{sink:?}");
        assert!(!got.contains("secret"), "{got}");
        assert!(!got.contains("hidden"), "{got}");
        assert!(got.contains("streaming: true"), "{got}");
    }
}
