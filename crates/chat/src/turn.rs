//! One inbound chat message, as a Chat adapter hands it to `core` (architecture §6).

use std::fmt;
use std::path::PathBuf;

use crate::ChatScope;

/// One inbound message, already authorised by the Chat adapter (architecture §6). `core` turns
/// it into a Turn on the Scope's serial queue.
#[derive(Clone, PartialEq, Eq)]
pub struct Turn {
    pub scope: ChatScope,
    /// The message this Turn answers. `core` keeps it in the Run registry's `origin`, so the
    /// next Worker can send the interrupted reply after a crash (#29, #79).
    pub origin: MessageRef,
    pub author: Author,
    /// The raw user text; `core` does the `<untrusted-…>` wrapping.
    pub text: String,
    /// Lines the adapter adds for context: `[In reply to …]`, forwarded text, a channel tag.
    pub context_lines: Vec<String>,
    /// Attachments already downloaded into `inbox/<transport>/`.
    pub attachments: Vec<LocalFile>,
    /// The thread's working dir under `agents/` (TS parity); `None` outside a hired thread.
    pub agent_dir: Option<String>,
}

impl fmt::Debug for Turn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Turn")
            .field("scope", &self.scope)
            .field("origin", &self.origin)
            .field("author", &self.author)
            .field("text_bytes", &self.text.len())
            .field("context_lines", &self.context_lines.len())
            .field("attachments", &self.attachments)
            .field("agent_dir", &self.agent_dir.is_some())
            .finish()
    }
}

/// A reference to one inbound message (#79): opaque transport-defined IDs that `core` stores
/// and hands back without reading them. The same pair as `api::supervisor::ChatOrigin`, which
/// adds the transport. IDs are identifiers, not content, so `Debug` prints them, like
/// [`ChatScope`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageRef {
    /// The channel or thread the message is in.
    pub channel: String,
    /// The message itself.
    pub message: String,
}

/// Who wrote a [`Turn`] (architecture §6): the display name only. User IDs stay in the
/// adapter, and `Debug` prints only the name's size.
#[derive(Clone, PartialEq, Eq)]
pub struct Author {
    pub display_name: String,
}

impl fmt::Debug for Author {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Author")
            .field("display_name_bytes", &self.display_name.len())
            .finish()
    }
}

/// A file on local disk: an attachment the adapter downloaded into `inbox/<transport>/`
/// (architecture §6, #1). `Debug` prints only sizes.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub path: PathBuf,
    /// The file name the user sent it under.
    pub name: String,
    /// The MIME type the transport reported; `None` when it reported none.
    pub mime: Option<String>,
    pub size_bytes: u64,
}

impl fmt::Debug for LocalFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalFile")
            .field("size_bytes", &self.size_bytes)
            .field("name_bytes", &self.name.len())
            .field("has_mime", &self.mime.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Transport;

    #[test]
    fn debug_is_content_free() {
        let turn = Turn {
            scope: ChatScope::Channel {
                transport: Transport::Discord,
                id: "1234".into(),
            },
            origin: MessageRef {
                channel: "1234".into(),
                message: "5678".into(),
            },
            author: Author {
                display_name: "Somebody".into(),
            },
            text: "the secret prompt".into(),
            context_lines: vec!["[In reply to a hidden line]".into()],
            attachments: vec![LocalFile {
                path: PathBuf::from("inbox/discord/hidden-file.png"),
                name: "hidden-file.png".into(),
                mime: Some("image/png".into()),
                size_bytes: 42,
            }],
            agent_dir: Some("agents/hidden-agent".into()),
        };
        let got = format!("{turn:?}");
        for content in ["Somebody", "secret", "hidden", "image/png"] {
            assert!(!got.contains(content), "{content} leaked: {got}");
        }
        assert!(got.contains("text_bytes: 17"), "{got}");
        assert!(got.contains("size_bytes: 42"), "{got}");
    }
}
