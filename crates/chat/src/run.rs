//! What a Chat adapter gets back for a Turn: a stream of events, then one outcome
//! (architecture §6).

use std::fmt;

use tokio::sync::{mpsc, oneshot};

/// Progress while Claude works on a Turn (architecture §6). The adapter renders it: the
/// streaming preview, tool-category reactions, agent markers. `Debug` prints only sizes.
#[derive(Clone, PartialEq, Eq)]
pub enum RunEvent {
    /// A piece of the reply text.
    TextDelta(String),
    /// Claude called a tool; `summary` is a short description of the call.
    ToolUse { name: String, summary: String },
    /// A tool call finished.
    ToolResult { name: String, ok: bool },
    /// A subagent started.
    AgentSpawn { id: String, description: String },
    /// A subagent finished.
    AgentDone { id: String },
}

impl fmt::Debug for RunEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunEvent::TextDelta(text) => f
                .debug_struct("TextDelta")
                .field("text_bytes", &text.len())
                .finish(),
            RunEvent::ToolUse { name, summary } => f
                .debug_struct("ToolUse")
                .field("name", name)
                .field("summary_bytes", &summary.len())
                .finish(),
            RunEvent::ToolResult { name, ok } => f
                .debug_struct("ToolResult")
                .field("name", name)
                .field("ok", ok)
                .finish(),
            RunEvent::AgentSpawn { id, description } => f
                .debug_struct("AgentSpawn")
                .field("id", id)
                .field("description_bytes", &description.len())
                .finish(),
            RunEvent::AgentDone { id } => f.debug_struct("AgentDone").field("id", id).finish(),
        }
    }
}

/// How a Turn or a sessionless run ended (the values of management-api §2.3's `TurnOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutcomeKind {
    Ok,
    Failed,
    /// A Refused run: the Launcher could not be run, with no fallback (#8).
    Refused,
    /// The run was stopped by a shutdown or a crash (#9, #29).
    Interrupted,
}

/// The one result of a Turn, a compact or a sessionless run (architecture §6). `Debug` prints
/// only sizes.
#[derive(Clone, PartialEq, Eq)]
pub struct ChatOutcome {
    pub kind: OutcomeKind,
    /// What to send back: the reply on success, or the user-facing message `core` wrote for a
    /// failure (which already says what Auto-compact did, #33). It may hold `[react:x]`
    /// directives and is not yet split.
    pub text: String,
}

impl fmt::Debug for ChatOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatOutcome")
            .field("kind", &self.kind)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

/// The adapter's end of a queued Turn (architecture §6): [`RunEvent`]s while Claude works,
/// then one [`ChatOutcome`]. The events channel closes before the outcome is sent.
#[derive(Debug)]
pub struct RunHandle {
    pub events: mpsc::Receiver<RunEvent>,
    pub outcome: oneshot::Receiver<ChatOutcome>,
}

impl RunHandle {
    /// A new handle and the [`RunSender`] `core` keeps to feed it, with room for
    /// `events_capacity` unread events.
    ///
    /// # Panics
    ///
    /// If `events_capacity` is 0, as [`mpsc::channel`] does.
    pub fn channel(events_capacity: usize) -> (RunSender, RunHandle) {
        let (events_tx, events) = mpsc::channel(events_capacity);
        let (outcome_tx, outcome) = oneshot::channel();
        let sender = RunSender {
            events: events_tx,
            outcome: outcome_tx,
        };
        (sender, RunHandle { events, outcome })
    }
}

/// `core`'s end of a [`RunHandle`]. Drop `events` before sending `outcome`.
#[derive(Debug)]
pub struct RunSender {
    pub events: mpsc::Sender<RunEvent>,
    pub outcome: oneshot::Sender<ChatOutcome>,
}

/// The adapter's end of a sessionless utility run, such as the thread-intent classifier
/// (architecture §6, §4 `claudeclaw-discord`). It has no events, only the outcome.
#[derive(Debug)]
pub struct UtilityHandle {
    pub outcome: oneshot::Receiver<ChatOutcome>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_delivers_events_then_outcome() {
        let (sender, mut handle) = RunHandle::channel(4);
        sender
            .events
            .try_send(RunEvent::TextDelta("hi".into()))
            .unwrap();
        sender
            .events
            .try_send(RunEvent::ToolResult {
                name: "Bash".into(),
                ok: true,
            })
            .unwrap();
        let RunSender { events, outcome } = sender;
        drop(events);
        outcome
            .send(ChatOutcome {
                kind: OutcomeKind::Ok,
                text: "done".into(),
            })
            .unwrap();

        assert_eq!(
            handle.events.try_recv().unwrap(),
            RunEvent::TextDelta("hi".into())
        );
        assert!(matches!(
            handle.events.try_recv().unwrap(),
            RunEvent::ToolResult { ok: true, .. }
        ));
        assert_eq!(
            handle.events.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        );
        assert_eq!(handle.outcome.try_recv().unwrap().kind, OutcomeKind::Ok);
    }

    #[test]
    fn dropped_sender_closes_the_outcome() {
        let (sender, mut handle) = RunHandle::channel(1);
        drop(sender);
        assert!(handle.outcome.try_recv().is_err());
    }

    #[test]
    fn debug_is_content_free() {
        let events = [
            RunEvent::TextDelta("secret text".into()),
            RunEvent::ToolUse {
                name: "Bash".into(),
                summary: "secret command".into(),
            },
            RunEvent::AgentSpawn {
                id: "a1".into(),
                description: "secret task".into(),
            },
        ];
        for event in events {
            let got = format!("{event:?}");
            assert!(!got.contains("secret"), "{got}");
        }
        let outcome = ChatOutcome {
            kind: OutcomeKind::Failed,
            text: "secret reply".into(),
        };
        let got = format!("{outcome:?}");
        assert!(!got.contains("secret"), "{got}");
        assert!(got.contains("text_bytes: 12"), "{got}");
    }
}
