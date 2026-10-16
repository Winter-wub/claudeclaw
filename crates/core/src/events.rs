//! The event bus (architecture §4 `claudeclaw-core`, management-api §1.6, §2.2).
//!
//! [`EventBus`] is a thin wrapper over a `tokio::sync::broadcast` of [`DaemonEvent`] that feeds
//! the `events.stream` SSE route. Components publish; each connected client holds a
//! [`broadcast::Receiver`]. Publishing is content-free and infallible: an event with no subscriber
//! is simply dropped, never an error, because a Daemon with no Desktop app attached is normal.
//!
//! **A slow subscriber.** The bus keeps [`CAPACITY`] events for each receiver. A receiver that
//! falls further behind than that loses the oldest events, and its next `recv` returns
//! [`RecvError::Lagged`] with the count missed. This is not a failure: events are hints to
//! re-fetch, so a subscriber answers a lag by re-reading what it shows and sending the client a
//! `gap` frame ([`GapReason::Lagged`](claudeclaw_api::GapReason::Lagged)) rather than replaying
//! the lost ones (management-api §2.2, #23 D5). [`RecvError::Closed`] means every [`EventBus`]
//! was dropped.

use std::fmt;

use claudeclaw_api::DaemonEvent;
use tokio::sync::broadcast;

/// How many events the bus holds for each receiver before it starts dropping the oldest.
pub const CAPACITY: usize = 256;

/// The Daemon's event bus: every `api::DaemonEvent`, fanned out to SSE subscribers.
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<DaemonEvent>,
}

impl EventBus {
    /// A bus holding [`CAPACITY`] events for each receiver.
    pub fn new() -> Self {
        EventBus {
            sender: broadcast::channel(CAPACITY).0,
        }
    }

    /// Publishes `event` to every current subscriber. A publish with no subscriber is not an
    /// error; the event is dropped.
    pub fn publish(&self, event: DaemonEvent) {
        // The only error is "no receivers"; that is normal and not worth a log line.
        let _ = self.sender.send(event);
    }

    /// A new receiver, which sees only events published after it subscribed (a bus has no replay;
    /// a client that wants history re-fetches through the API).
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.sender.subscribe()
    }

    /// How many receivers are attached right now.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for EventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventBus")
            .field("capacity", &CAPACITY)
            .field("subscribers", &self.sender.receiver_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::DaemonEvent;
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};

    use super::*;

    fn event(id: &str) -> DaemonEvent {
        DaemonEvent::DeviceRevoked {
            device_id: id.into(),
        }
    }

    #[test]
    fn publishing_with_no_subscriber_is_not_an_error() {
        let bus = EventBus::new();
        assert_eq!(bus.subscriber_count(), 0);
        for _ in 0..10 {
            bus.publish(event("1"));
        }
    }

    #[tokio::test]
    async fn one_subscriber_receives_a_published_event() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        bus.publish(event("123"));
        assert_eq!(rx.recv().await.unwrap(), event("123"));
    }

    #[tokio::test]
    async fn two_subscribers_each_receive_a_published_event() {
        let bus = EventBus::new();
        let mut first = bus.subscribe();
        let mut second = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);
        bus.publish(event("123"));
        assert_eq!(first.recv().await.unwrap(), event("123"));
        assert_eq!(second.recv().await.unwrap(), event("123"));
    }

    #[tokio::test]
    async fn all_receivers_gone_is_not_an_error() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        drop(rx);
        bus.publish(event("1"));
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn a_lagged_subscriber_is_told_how_many_it_missed() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        // Overflow the buffer without receiving.
        for n in 0..CAPACITY + 3 {
            bus.publish(event(&format!("{n}")));
        }
        match rx.recv().await {
            Err(RecvError::Lagged(missed)) => assert_eq!(missed, 3),
            other => panic!("expected Lagged(3), got {other:?}"),
        }
        // The receiver resyncs at the oldest event still held, then gets the rest in order.
        assert_eq!(rx.recv().await.unwrap(), event("3"));
        for n in 4..CAPACITY + 3 {
            assert_eq!(rx.recv().await.unwrap(), event(&format!("{n}")));
        }
        // Nothing is left, and the sender is still alive.
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }
}
