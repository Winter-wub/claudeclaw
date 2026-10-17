//! Building a [`chat::Turn`] from an inbound Discord message (architecture §4
//! `claudeclaw-discord`, §6). A port of the prompt-building half of the TS message handler
//! (`src/commands/discord.ts:1046-1300`).
//!
//! The adapter hands `core` the raw user text (it does the `<untrusted-…>` wrapping) plus the
//! context lines TS adds: the channel tag, the `[Discord from …]` line, attachment notes, and
//! the reply-to or forwarded-message line. Attachments are downloaded through an
//! [`AttachmentFetcher`](crate::seams::AttachmentFetcher); one that can't be fetched becomes a
//! context line and the Turn runs without the file.

use claudeclaw_chat::{Author, ChatScope, LocalFile, MessageRef, Transport, Turn};
use twilight_model::channel::Attachment;
use twilight_model::channel::Message;
use twilight_model::channel::message::MessageReferenceType;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, UserMarker};

use crate::seams::{AttachmentFetcher, FetchError};
use crate::triggers::DiscordSettings;

/// The 25 MB cap the fetcher enforces (TS `MAX_FILE_ATTACHMENT_BYTES`,
/// `src/commands/discord.ts:656`), used only to phrase the oversized note.
pub const MAX_FILE_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

/// What the adapter's cache knows about a thread the message is in (TS `knownThreads`,
/// `src/commands/discord.ts:191`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadTrack {
    /// The thread's parent channel.
    pub parent: Id<ChannelMarker>,
    /// The agent directory key (`agentDirKey`, `src/runner.ts:747`), when the name was
    /// sanitizable. The Turn's `agent_dir` is `agents/<key>`.
    pub agent_name: Option<String>,
    /// The thread's display name, when the transport reported one.
    pub name: Option<String>,
}

/// A guaranteed-unique, filesystem-safe directory key for an agent thread
/// (`agentDirKey`, `src/runner.ts:747-757`): the display name slugified, cut so the
/// `-<thread id>` suffix is never truncated away. `None` when nothing survived slugifying,
/// which TS throws on and the adapter treats as "no agent scoping".
pub fn agent_dir_key(raw_name: &str, thread_id: &str) -> Option<String> {
    let suffix = format!("-{thread_id}");
    let max_slug_len = 64usize.saturating_sub(suffix.len()).max(1);
    let slug = slugify(raw_name, max_slug_len);
    (!slug.is_empty()).then(|| format!("{slug}{suffix}"))
}

/// TS `raw.toLowerCase().replace(/[^a-z0-9_]+/g, "-").replace(/^-+|-+$/g, "").slice(0, max)`.
/// The replacements leave only ASCII, so the slice counts bytes.
fn slugify(raw: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut dashed = false;
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
            dashed = false;
        } else if !dashed {
            out.push('-');
            dashed = true;
        }
    }
    out.trim_matches('-').chars().take(max_len).collect()
}

/// The Scope a message came from (§6): a DM is `Direct` (mapped to the Global Scope, TS parity),
/// a tracked thread is `Thread`, and any other guild channel is `Channel`.
pub fn scope_for(message: &Message, thread: Option<&ThreadTrack>) -> ChatScope {
    match (message.guild_id, thread) {
        (None, _) => ChatScope::Direct {
            transport: Transport::Discord,
        },
        (Some(_), Some(track)) => ChatScope::Thread {
            transport: Transport::Discord,
            id: message.channel_id.to_string(),
            parent: track.parent.to_string(),
        },
        (Some(_), None) => ChatScope::Channel {
            transport: Transport::Discord,
            id: message.channel_id.to_string(),
        },
    }
}

/// The thread's `agents/<key>` dir, if the adapter tracks the thread and its name sanitized.
fn agent_dir_for(thread: Option<&ThreadTrack>) -> Option<String> {
    thread
        .and_then(|t| t.agent_name.as_deref())
        .map(|name| format!("agents/{name}"))
}

/// TS's `content.replace(/\0/g, "").replace(/<@!?bot>/g, "").trim()`
/// (`src/commands/discord.ts:965,1047-1050`): the raw user text, minus NUL bytes and the bot's
/// own mention.
pub fn clean_content(content: &str, bot_user_id: Option<Id<UserMarker>>) -> String {
    let mut text = content.replace('\0', "");
    if let Some(bot) = bot_user_id {
        text = text.replace(&format!("<@{bot}>"), "");
        text = text.replace(&format!("<@!{bot}>"), "");
    }
    text.trim().to_owned()
}

/// TS's per-type notes for a fetched attachment (`src/commands/discord.ts:1242-1270`).
fn attachment_notes(file: &LocalFile) -> Vec<String> {
    let mut notes = Vec::new();
    let mime = file.mime.as_deref().unwrap_or("");
    if mime.starts_with("image/") {
        notes.push(
            "The user attached an image. Inspect this image file directly before answering."
                .to_owned(),
        );
    } else if mime.starts_with("audio/") {
        notes.push("The user attached voice audio. Use its content as their message.".to_owned());
    }
    notes
}

/// The note for an attachment the fetcher refused (`src/commands/discord.ts:1272-1282`).
fn fetch_error_note(attachment: &Attachment, error: &FetchError) -> String {
    match error {
        FetchError::TooLarge => {
            let size_mb = format!("{:.1}", attachment.size as f64 / (1024.0 * 1024.0));
            format!(
                "The user attached {} ({} MB), which exceeds the {} MB download limit and was not \
                 downloaded. Tell them.",
                attachment.filename,
                size_mb,
                MAX_FILE_ATTACHMENT_BYTES / (1024 * 1024),
            )
        }
        FetchError::DiskLow => format!(
            "The user attached {}, which could not be downloaded because the daemon is low on \
             disk. Tell them.",
            attachment.filename,
        ),
        FetchError::Download | FetchError::Write => format!(
            "The user attached {} , which could not be downloaded. Tell them and ask them to \
             resend.",
            attachment.filename,
        ),
    }
}

/// The forwarded-message context line (TS `src/commands/discord.ts:1285-1292`). Discord never
/// puts the snapshot's author in the payload, so TS (and this port) always renders `unknown`.
fn forwarded_line(message: &Message) -> Option<String> {
    let snapshot = message.message_snapshots.first()?.message.clone();
    let attachments = if snapshot.attachments.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = snapshot
            .attachments
            .iter()
            .map(|a| a.filename.as_str())
            .collect();
        format!(" [attachments: {}]", names.join(", "))
    };
    Some(format!(
        "[Forwarded message from unknown]: {}{attachments}",
        snapshot.content,
    ))
}

/// The reply-to context line (TS `src/commands/discord.ts:1293-1299`).
fn reply_line(message: &Message) -> Option<String> {
    let referenced = message.referenced_message.as_ref()?;
    let attachments = if referenced.attachments.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = referenced
            .attachments
            .iter()
            .map(|a| a.filename.as_str())
            .collect();
        format!(" [attachments: {}]", names.join(", "))
    };
    Some(format!(
        "[In reply to {}]: {}{attachments}",
        referenced.author.name, referenced.content,
    ))
}

/// Build the [`Turn`] for an authorised message: its Scope, origin, author name, cleaned text,
/// context lines and downloaded attachments.
///
/// The caller has already decided to handle the message ([`crate::triggers::decide`]); this does
/// not re-check triggers.
pub async fn build(
    message: &Message,
    bot_user_id: Option<Id<UserMarker>>,
    settings: &DiscordSettings,
    thread: Option<&ThreadTrack>,
    fetcher: &dyn AttachmentFetcher,
) -> Turn {
    let is_guild = message.guild_id.is_some();
    let scope = scope_for(message, thread);

    let mut context_lines = vec![
        settings.channel_tag(is_guild, &message.channel_id),
        format!("[Discord from {}]", message.author.name),
    ];

    let mut attachments = Vec::new();
    let mut failed = Vec::new();
    for attachment in &message.attachments {
        match fetcher.fetch(attachment).await {
            Ok(file) => {
                context_lines.extend(attachment_notes(&file));
                attachments.push(file);
            }
            Err(error) => failed.push(fetch_error_note(attachment, &error)),
        }
    }
    if attachments.len() == 1 {
        context_lines
            .push("The user attached a document. Read and process this file directly.".to_owned());
    } else if attachments.len() > 1 {
        context_lines.push(format!(
            "The user attached {} documents. Read and process these files directly.",
            attachments.len(),
        ));
    }
    context_lines.extend(failed);

    let is_forward =
        message.reference.as_ref().map(|r| r.kind) == Some(MessageReferenceType::Forward);
    if is_forward {
        if let Some(line) = forwarded_line(message) {
            context_lines.push(line);
        }
    } else if let Some(line) = reply_line(message) {
        context_lines.push(line);
    }

    Turn {
        scope,
        origin: MessageRef {
            channel: message.channel_id.to_string(),
            message: message.id.to_string(),
        },
        author: Author {
            display_name: message.author.name.clone(),
        },
        text: clean_content(&message.content, bot_user_id),
        context_lines,
        attachments,
        agent_dir: agent_dir_for(thread),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use claudeclaw_chat::ChatScope;
    use futures_util::future::BoxFuture;
    use serde_json::{Value, json};
    use twilight_model::channel::Message;
    use twilight_model::id::Id;

    use super::*;
    use crate::seams::FetchError;

    const BOT: u64 = 100;
    const USER: u64 = 200;
    const CHANNEL: u64 = 7;
    const PARENT: u64 = 8;
    const GUILD: u64 = 9;

    fn message(value: Value) -> Message {
        serde_json::from_value(value).unwrap()
    }

    fn settings() -> DiscordSettings {
        serde_json::from_value(json!({
            "allowedUserIds": [USER.to_string()],
            "channelNames": {"7": "general"},
        }))
        .unwrap()
    }

    fn base(content: &str, guild: bool) -> Value {
        let mut value = json!({
            "id": "500",
            "channel_id": CHANNEL.to_string(),
            "author": {"id": USER.to_string(), "username": "hopper", "discriminator": "0"},
            "content": content,
            "timestamp": "2026-10-17T00:00:00+00:00",
            "edited_timestamp": null,
            "tts": false,
            "mention_everyone": false,
            "mentions": [], "mention_roles": [], "attachments": [], "embeds": [],
            "pinned": false, "type": 0, "flags": 0,
        });
        if guild {
            value["guild_id"] = json!(GUILD.to_string());
        }
        value
    }

    fn attachment_json() -> Value {
        json!({
            "id": "600",
            "filename": "notes.txt",
            "content_type": "text/plain",
            "size": 12,
            "url": "https://cdn.example/notes.txt",
            "proxy_url": "https://media.example/notes.txt",
        })
    }

    /// A fetcher that succeeds for the `text/plain` fixture and refuses otherwise.
    struct FakeFetcher {
        fail_with: Option<FetchError>,
    }

    impl AttachmentFetcher for FakeFetcher {
        fn fetch<'a>(
            &'a self,
            attachment: &'a Attachment,
        ) -> BoxFuture<'a, Result<LocalFile, FetchError>> {
            Box::pin(async move {
                if let Some(error) = &self.fail_with {
                    return Err(error.clone());
                }
                Ok(LocalFile {
                    path: PathBuf::from("/tmp/inbox/discord/600-1.txt"),
                    name: attachment.filename.clone(),
                    mime: attachment.content_type.clone(),
                    size_bytes: attachment.size,
                })
            })
        }
    }

    fn thread() -> ThreadTrack {
        ThreadTrack {
            parent: Id::new(PARENT),
            agent_name: Some("researcher-77".into()),
            name: Some("Researcher".into()),
        }
    }

    #[tokio::test]
    async fn dm_builds_the_global_scope() {
        let m = message(base("hello there", false));
        let fetcher = FakeFetcher { fail_with: None };
        let turn = build(&m, Some(Id::new(BOT)), &settings(), None, &fetcher).await;
        assert_eq!(
            turn.scope,
            ChatScope::Direct {
                transport: Transport::Discord
            }
        );
        assert_eq!(turn.text, "hello there");
        assert_eq!(turn.author.display_name, "hopper");
        assert_eq!(turn.origin.channel, "7");
        assert_eq!(turn.origin.message, "500");
        assert!(turn.context_lines.contains(&"[Discord DM]".to_owned()));
        assert!(
            turn.context_lines
                .contains(&"[Discord from hopper]".to_owned())
        );
        assert!(turn.agent_dir.is_none());
    }

    #[tokio::test]
    async fn channel_mention_strips_the_mention_and_tags_the_channel() {
        let m = message(base("<@100> ping <@!100>", true));
        let fetcher = FakeFetcher { fail_with: None };
        let turn = build(&m, Some(Id::new(BOT)), &settings(), None, &fetcher).await;
        assert_eq!(
            turn.scope,
            ChatScope::Channel {
                transport: Transport::Discord,
                id: "7".into()
            }
        );
        assert_eq!(turn.text, "ping");
        assert!(
            turn.context_lines
                .contains(&"[Discord Channel: general]".to_owned())
        );
    }

    #[tokio::test]
    async fn thread_carries_the_parent_and_the_agent_dir() {
        let mut value = base("hi", true);
        value["channel_id"] = json!("77");
        let m = message(value);
        let fetcher = FakeFetcher { fail_with: None };
        let turn = build(
            &m,
            Some(Id::new(BOT)),
            &settings(),
            Some(&thread()),
            &fetcher,
        )
        .await;
        assert_eq!(
            turn.scope,
            ChatScope::Thread {
                transport: Transport::Discord,
                id: "77".into(),
                parent: "8".into(),
            }
        );
        assert_eq!(turn.agent_dir.as_deref(), Some("agents/researcher-77"));
    }

    #[tokio::test]
    async fn reply_to_builds_the_in_reply_to_line() {
        let mut value = base("yes please", true);
        value["type"] = json!(19);
        value["referenced_message"] = json!({
            "id": "400", "channel_id": CHANNEL.to_string(),
            "author": {"id": "300", "username": "sarah", "discriminator": "0"},
            "content": "shall we ship?", "timestamp": "2026-10-17T00:00:00+00:00",
            "edited_timestamp": null, "tts": false, "mention_everyone": false,
            "mentions": [], "mention_roles": [], "attachments": [], "embeds": [],
            "pinned": false, "type": 0,
        });
        let m = message(value);
        let fetcher = FakeFetcher { fail_with: None };
        let turn = build(&m, Some(Id::new(BOT)), &settings(), None, &fetcher).await;
        assert!(
            turn.context_lines
                .contains(&"[In reply to sarah]: shall we ship?".to_owned())
        );
    }

    #[tokio::test]
    async fn forwarded_builds_the_forwarded_line() {
        let mut value = base("fyi", true);
        value["message_reference"] = json!({"type": 1});
        value["message_snapshots"] = json!([{
            "message": {
                "content": "the original text",
                "attachments": [{"id": "601", "filename": "chart.png",
                    "content_type": "image/png", "size": 5,
                    "url": "https://cdn.example/chart.png",
                    "proxy_url": "https://media.example/chart.png"}],
                "timestamp": "2026-10-17T00:00:00+00:00",
                "edited_timestamp": null, "mentions": [], "mention_roles": [],
                "embeds": [], "type": 0,
            }
        }]);
        let m = message(value);
        let fetcher = FakeFetcher { fail_with: None };
        let turn = build(&m, Some(Id::new(BOT)), &settings(), None, &fetcher).await;
        assert!(
            turn.context_lines.contains(
                &"[Forwarded message from unknown]: the original text [attachments: chart.png]"
                    .to_owned()
            )
        );
    }

    #[tokio::test]
    async fn a_fetched_attachment_lands_in_the_turn() {
        let mut value = base("see file", true);
        value["attachments"] = json!([attachment_json()]);
        let m = message(value);
        let fetcher = FakeFetcher { fail_with: None };
        let turn = build(&m, Some(Id::new(BOT)), &settings(), None, &fetcher).await;
        assert_eq!(turn.attachments.len(), 1);
        assert_eq!(turn.attachments[0].name, "notes.txt");
        assert_eq!(turn.attachments[0].size_bytes, 12);
        assert!(turn.context_lines.iter().any(|l| l.contains("a document")));
        assert!(!turn.context_lines.iter().any(|l| l.contains("could not")));
    }

    #[tokio::test]
    async fn a_refused_attachment_becomes_a_note() {
        let mut value = base("see file", true);
        value["attachments"] = json!([attachment_json()]);
        let m = message(value);
        let fetcher = FakeFetcher {
            fail_with: Some(FetchError::Download),
        };
        let turn = build(&m, Some(Id::new(BOT)), &settings(), None, &fetcher).await;
        assert!(turn.attachments.is_empty());
        assert!(
            turn.context_lines
                .iter()
                .any(|l| l.contains("could not be downloaded"))
        );

        let fetcher = FakeFetcher {
            fail_with: Some(FetchError::TooLarge),
        };
        let turn = build(&m, Some(Id::new(BOT)), &settings(), None, &fetcher).await;
        assert!(
            turn.context_lines
                .iter()
                .any(|l| l.contains("exceeds the 25 MB download limit"))
        );
    }

    #[test]
    fn scope_for_a_dm_with_a_stale_thread_track_is_still_direct() {
        // A DM channel id can never be a tracked guild thread; Direct wins regardless.
        let m = message(base("hi", false));
        assert!(matches!(
            scope_for(&m, Some(&thread())),
            ChatScope::Direct { .. }
        ));
    }

    /// Values come from running TS `agentDirKey` (`src/runner.ts:747`) on each input.
    #[test]
    fn agent_dir_key_matches_ts() {
        assert_eq!(
            agent_dir_key("Researcher", "77").as_deref(),
            Some("researcher-77")
        );
        // Runs of disallowed characters collapse to one dash and the ends are trimmed.
        assert_eq!(
            agent_dir_key("  Hello, World!  ", "77").as_deref(),
            Some("hello-world-77")
        );
        // Underscores and digits survive.
        assert_eq!(
            agent_dir_key("agent_01", "77").as_deref(),
            Some("agent_01-77")
        );
        // Unsanitizable names become `None` instead of throwing.
        assert_eq!(agent_dir_key("!!!", "77"), None);
        assert_eq!(agent_dir_key("", "77"), None);
        // The `-<id>` suffix is never truncated away; the slug keeps 64-3 = 61 bytes.
        let long = "a".repeat(100);
        let key = agent_dir_key(&long, "77").unwrap();
        assert_eq!(key, format!("{}-77", "a".repeat(61)));
        assert_eq!(key.len(), 64);
    }
}
