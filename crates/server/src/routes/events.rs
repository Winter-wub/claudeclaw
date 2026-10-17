//! The `events.stream` handler and its frame stream (management-api §2.2, §1.6).

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast;

use claudeclaw_api::events::{DaemonEvent, GapReason, StreamGap, StreamHello};

use crate::error::ApiFailure;
use crate::state::AppState;

/// `events.stream`'s query: `?kinds=<prefix>,…` (§2.2).
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub kinds: Option<String>,
}

/// One thing the stream sends. Modelling the frames apart from axum's [`Event`] keeps the filter
/// and lag behaviour testable: `Event` gives no way to read its bytes back.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// Always first: `event: hello` with the `boot_id` (§1.6).
    Hello(StreamHello),
    /// `event: daemon` with one [`DaemonEvent`].
    Daemon(Box<DaemonEvent>),
    /// `event: gap`, sent instead of skipping events silently (§1.6).
    Gap(StreamGap),
}

impl Frame {
    /// The SSE event name.
    pub fn name(&self) -> &'static str {
        match self {
            Frame::Hello(_) => "hello",
            Frame::Daemon(event) => event.kind(),
            Frame::Gap(_) => "gap",
        }
    }

    /// Renders the frame as an axum SSE [`Event`].
    pub fn into_event(self) -> Event {
        match self {
            Frame::Hello(hello) => json_event("hello", hello),
            Frame::Daemon(event) => json_event("daemon", event),
            Frame::Gap(gap) => json_event("gap", gap),
        }
    }
}

fn json_event<T: serde::Serialize>(name: &'static str, data: T) -> Event {
    Event::default()
        .event(name)
        .json_data(data)
        .unwrap_or_else(|_| Event::default().event(name))
}

/// The frame stream: a `hello`, then every matching [`DaemonEvent`], and a `gap` frame whenever a
/// slow client falls behind the bus (which has no replay, §2.2).
pub fn frames(
    boot_id: String,
    receiver: broadcast::Receiver<DaemonEvent>,
    kinds: Vec<String>,
) -> impl Stream<Item = Frame> {
    let hello = Some(Frame::Hello(StreamHello { boot_id }));
    futures_util::stream::unfold(
        (receiver, hello, kinds),
        |(mut receiver, mut hello, kinds)| async move {
            if let Some(frame) = hello.take() {
                return Some((frame, (receiver, hello, kinds)));
            }
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        if kinds_match(&kinds, event.kind()) {
                            let frame = Frame::Daemon(Box::new(event));
                            return Some((frame, (receiver, hello, kinds)));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        let frame = Frame::Gap(StreamGap {
                            reason: GapReason::Lagged,
                            missed: Some(missed.min(u64::from(u32::MAX)) as u32),
                        });
                        return Some((frame, (receiver, hello, kinds)));
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    )
}

/// `?kinds=` filters by prefix of [`DaemonEvent::kind`]; an empty list passes everything.
pub fn kinds_match(kinds: &[String], kind: &str) -> bool {
    kinds.is_empty() || kinds.iter().any(|prefix| kind.starts_with(prefix.as_str()))
}

/// `GET /v1/events`: the SSE handler.
pub async fn stream(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<Response, ApiFailure> {
    let boot_id = state.events.boot_id();
    let receiver = state.events.subscribe();
    let kinds = parse_kinds(query.kinds.as_deref());
    let stream =
        frames(boot_id, receiver, kinds).map(|frame| Ok::<Event, Infallible>(frame.into_event()));
    let sse = Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    );
    Ok(sse.into_response())
}

fn parse_kinds(raw: Option<&str>) -> Vec<String> {
    raw.map(|kinds| {
        kinds
            .split(',')
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::events::DaemonEvent;

    use super::*;

    fn receiver() -> (
        broadcast::Sender<DaemonEvent>,
        broadcast::Receiver<DaemonEvent>,
    ) {
        let (sender, receiver) = broadcast::channel(4);
        (sender, receiver)
    }

    fn revoked(id: &str) -> DaemonEvent {
        DaemonEvent::DeviceRevoked {
            device_id: id.into(),
        }
    }

    #[tokio::test]
    async fn a_stream_starts_with_hello_then_events() {
        let (sender, receiver) = receiver();
        let mut frames = Box::pin(frames("boot-1".into(), receiver, vec![]));
        assert_eq!(
            frames.next().await,
            Some(Frame::Hello(StreamHello {
                boot_id: "boot-1".into()
            }))
        );
        sender.send(revoked("a")).unwrap();
        assert_eq!(
            frames.next().await,
            Some(Frame::Daemon(Box::new(revoked("a"))))
        );
    }

    #[tokio::test]
    async fn kinds_filter_by_prefix() {
        let (sender, receiver) = receiver();
        let kinds = parse_kinds(Some("run.,job.fired"));
        let mut frames = Box::pin(frames("boot-1".into(), receiver, kinds));
        assert!(matches!(frames.next().await, Some(Frame::Hello(_))));
        sender
            .send(DaemonEvent::DeviceRevoked {
                device_id: "a".into(),
            })
            .unwrap();
        sender
            .send(DaemonEvent::RunStarted {
                run_id: "r1".into(),
                turn_id: "t1".into(),
                scope: claudeclaw_api::Scope::Global,
                job: None,
                launcher: "direct".into(),
            })
            .unwrap();
        // The filtered-out event is skipped; the matching one arrives.
        let frame = frames.next().await.unwrap();
        assert_eq!(frame.name(), "run.started");
    }

    #[tokio::test]
    async fn a_lagging_receiver_gets_a_gap_frame() {
        let (sender, receiver) = receiver();
        let mut frames = Box::pin(frames("boot-1".into(), receiver, vec![]));
        assert!(matches!(frames.next().await, Some(Frame::Hello(_))));
        for n in 0..6 {
            sender.send(revoked(&format!("{n}"))).unwrap();
        }
        let frame = frames.next().await.unwrap();
        assert_eq!(
            frame,
            Frame::Gap(StreamGap {
                reason: GapReason::Lagged,
                missed: Some(2)
            })
        );
    }

    #[test]
    fn an_empty_kinds_list_passes_everything() {
        assert!(kinds_match(&[], "status.changed"));
        assert!(kinds_match(&["status".into()], "status.changed"));
        assert!(!kinds_match(&["log".into()], "status.changed"));
    }
}
