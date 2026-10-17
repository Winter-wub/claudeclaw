//! The Discord [`Notifier`]: owner DMs and the interrupted reply (architecture §4
//! `claudeclaw-discord`, §6; #9, #29, #79).
//!
//! Owner DMs go to `discord.allowedUserIds` as `sendMessageToUser` does in TS
//! (`src/config.ts` `DiscordConfig.allowedUserIds`, `src/commands/start.ts:656-666`). The
//! interrupted reply is the Rust rewrite's addition: at shutdown, and after a crash for every run
//! a Reap proved dead, `core` calls [`Notifier::interrupted`] with the Turn's origin, which
//! replies to the origin message and clears every progress reaction the bot added while working —
//! the 👀 thinking reaction and the tool-category emoji (#9, #29, #79). TS had no equivalent; the
//! wording matches `core`'s `ChatOutcome` for an interrupted run.

use std::fmt;
use std::sync::Arc;

use claudeclaw_chat::{MessageRef, Notifier, NotifyError, OwnerNotice};
use futures_util::future::BoxFuture;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, MessageMarker, UserMarker};

use crate::reply::{THINKING_REACTION, TOOL_REACTION_EMOJIS, TOOL_REACTION_SPACING};
use crate::rest::{Rest, RestError, parse_owner_ids, send_owner_text, warn_send_failed};

/// What `Notifier::interrupted` sends (#9, #79). The same sentence `core`'s `chat_outcome` uses
/// for an interrupted run, so a shutdown reply and a resumed run's reply read alike.
pub const INTERRUPTED_TEXT: &str = "The run was interrupted; please resend your message.";

/// The Discord notifier, registered with `core`'s `notify`. Holds the shared [`Rest`] and the
/// parsed owner IDs.
pub struct DiscordNotifier {
    rest: Arc<Rest>,
    owners: Vec<Id<UserMarker>>,
}

impl DiscordNotifier {
    /// A notifier for `allowed_user_ids`, the entries of `discord.allowedUserIds`. Entries that
    /// are not snowflakes are skipped with a warning, as elsewhere.
    pub fn new(rest: Arc<Rest>, allowed_user_ids: &[String]) -> Self {
        DiscordNotifier {
            rest,
            owners: parse_owner_ids(allowed_user_ids),
        }
    }

    /// How many owners this notifier can reach.
    pub fn owner_count(&self) -> usize {
        self.owners.len()
    }

    /// Replies to the origin and clears the progress reactions (#9, #29, #79). A missing origin
    /// message is not an error.
    async fn interrupt(&self, origin: MessageRef) -> Result<(), NotifyError> {
        let channel: Id<ChannelMarker> = origin
            .channel
            .parse()
            .map_err(|_| transport(RestError::Id))?;
        let message: Id<MessageMarker> = origin
            .message
            .parse()
            .map_err(|_| transport(RestError::Id))?;

        match self
            .rest
            .send(channel, INTERRUPTED_TEXT, Some(message))
            .await
        {
            Ok(_) => {}
            // The message (or its channel) is gone: nothing left to reply to.
            Err(err) if err.is_not_found() => return Ok(()),
            Err(err) => {
                warn_send_failed(&err);
                return Err(transport(err));
            }
        }

        // Remove the progress reactions the run may have left behind — the tool categories and
        // the 👀 thinking reaction — spaced as reactions are (they are cosmetic, so a failure
        // only logs). 👀 goes last, as it does at the end of a normal Turn.
        for emoji in TOOL_REACTION_EMOJIS
            .into_iter()
            .chain(std::iter::once(THINKING_REACTION))
        {
            if let Err(err) = self.rest.remove_reaction(channel, message, emoji).await {
                tracing::debug!(
                    event = "discord.send_failed",
                    status = err.status(),
                    "a progress reaction could not be cleared: {err}"
                );
            }
            tokio::time::sleep(TOOL_REACTION_SPACING).await;
        }
        Ok(())
    }
}

impl fmt::Debug for DiscordNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordNotifier")
            .field("owners", &self.owners.len())
            .finish()
    }
}

impl Notifier for DiscordNotifier {
    fn notify_owners(&self, msg: OwnerNotice) -> BoxFuture<'_, Result<(), NotifyError>> {
        Box::pin(async move {
            if self.owners.is_empty() {
                return Err(NotifyError::NoOwners);
            }
            send_owner_text(&self.rest, &self.owners, &msg.text).await
        })
    }

    fn interrupted(&self, origin: MessageRef) -> BoxFuture<'_, Result<(), NotifyError>> {
        Box::pin(self.interrupt(origin))
    }
}

/// Wraps a [`RestError`] as a [`NotifyError::Transport`].
fn transport(err: RestError) -> NotifyError {
    NotifyError::Transport(Box::new(err))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::rest::test_server::Server;

    fn message_ref(channel: &str, message: &str) -> MessageRef {
        MessageRef {
            channel: channel.to_owned(),
            message: message.to_owned(),
        }
    }

    #[tokio::test]
    async fn owner_dms_go_to_every_owner() {
        let server = Server::start().await;
        let rest = Arc::new(server.connect());
        let notifier =
            DiscordNotifier::new(rest, &["1".to_owned(), "2".to_owned(), "bad".to_owned()]);
        assert_eq!(notifier.owner_count(), 2);

        notifier
            .notify_owners(OwnerNotice {
                kind: "job_failed".into(),
                text: "the job failed".into(),
            })
            .await
            .unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[0].text().contains(r#""recipient_id":"1""#));
        assert!(requests[2].text().contains(r#""recipient_id":"2""#));
    }

    #[tokio::test]
    async fn no_owners_is_no_owners() {
        let server = Server::start().await;
        let rest = Arc::new(server.connect());
        let notifier = DiscordNotifier::new(rest, &[]);
        let err = notifier
            .notify_owners(OwnerNotice {
                kind: "job_failed".into(),
                text: "x".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, NotifyError::NoOwners));
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn a_failing_owner_does_not_stop_the_others() {
        let server = Server::start().await;
        // Every DM creation fails, so nobody is reached and the error is Transport.
        server.canned("POST", "/users/@me/channels", 500, r#"{"message":"boom"}"#);
        let rest = Arc::new(server.connect());
        let notifier = DiscordNotifier::new(rest, &["1".to_owned()]);
        let err = notifier
            .notify_owners(OwnerNotice {
                kind: "job_failed".into(),
                text: "x".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, NotifyError::Transport(_)));
    }

    #[tokio::test]
    async fn interrupted_replies_then_clears_reactions() {
        let server = Server::start().await;
        let rest = Arc::new(server.connect());
        let notifier = DiscordNotifier::new(rest, &[]);
        notifier
            .interrupted(message_ref("123", "456"))
            .await
            .unwrap();

        let requests = server.requests();
        // One reply, then one delete per tool-category reaction.
        let reply = &requests[0];
        assert_eq!(reply.method, "POST");
        assert_eq!(reply.path, "/api/v10/channels/123/messages");
        let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(body["content"], INTERRUPTED_TEXT);
        assert_eq!(body["message_reference"]["message_id"], "456");

        let removes: Vec<&str> = requests[1..].iter().map(|r| r.method.as_str()).collect();
        assert_eq!(removes, ["DELETE"; TOOL_REACTION_EMOJIS.len() + 1]);
        assert_eq!(requests.len(), TOOL_REACTION_EMOJIS.len() + 2);
        // The 👀 thinking reaction a crashed Turn left behind is cleared too, and last of all.
        assert!(
            requests[1..]
                .iter()
                .any(|r| r.path.contains("/reactions/%F0%9F%91%80/@me"))
        );
        assert!(requests.last().unwrap().path.contains("%F0%9F%91%80"));
    }

    #[tokio::test]
    async fn interrupted_treats_a_gone_message_as_success() {
        let server = Server::start().await;
        server.canned(
            "POST",
            "/messages",
            404,
            r#"{"message":"gone","code":10008}"#,
        );
        let rest = Arc::new(server.connect());
        let notifier = DiscordNotifier::new(rest, &[]);
        notifier
            .interrupted(message_ref("123", "456"))
            .await
            .unwrap();
        // Only the reply attempt; no reactions are cleared after a 404.
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn interrupted_rejects_a_garbage_origin() {
        let server = Server::start().await;
        let rest = Arc::new(server.connect());
        let notifier = DiscordNotifier::new(rest, &[]);
        let err = notifier
            .interrupted(message_ref("not-a-number", "456"))
            .await
            .unwrap_err();
        match err {
            NotifyError::Transport(source) => {
                assert!(source.downcast_ref::<RestError>().is_some());
            }
            other => panic!("expected Transport, got {other:?}"),
        }
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn debug_is_content_free() {
        let rest = Arc::new(Rest::for_test(
            &secrecy::SecretString::from("top-secret"),
            &crate::DiscordEndpoints::default(),
        ));
        let notifier = DiscordNotifier::new(rest, &["1".to_owned()]);
        let got = format!("{notifier:?}");
        assert!(!got.contains("secret"), "{got}");
        assert!(got.contains("owners: 1"), "{got}");
    }
}
