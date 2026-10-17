//! One serial queue of Turns per Scope (architecture §4 `conversations`, §6).
//!
//! Each Scope gets its own queue and its own draining task. The task runs one Turn at a time, so
//! two Turns in the same Scope never overlap, and — because the pipeline only returns once the
//! run is settled (#37) — the next Turn starts only after the previous run's process group is
//! empty. The reply goes out earlier, at the run's leader exit and after the events channel has
//! closed (#37 D2, the `chat` contract), so an answer is never delayed by the settle. A task that
//! finds its queue empty removes the Scope from the map and exits, so an idle Worker holds no
//! per-Scope task.
//!
//! The map lock is the only synchronisation between a producer ([`Queues::converse`]) and the
//! drain task. `converse` sends into the unbounded channel **while holding the lock**, and the
//! task removes an empty Scope **under the same lock**, so a Turn cannot be sent into a queue
//! that is being removed: either the send lands first and the task sees it, or the removal lands
//! first and `converse` creates a fresh queue.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use claudeclaw_api::log_event::daemon;
use claudeclaw_chat::{RunHandle, RunSender, Turn};
use tokio::sync::mpsc;

use crate::conversations::pipeline::{Pipeline, scope_label};
use crate::scope::Scope;

/// The per-Scope Turn queues ([`Queues::converse`] is the `ChatHost::converse` body).
pub struct Queues {
    pipeline: Arc<Pipeline>,
    /// Room for unread [`claudeclaw_chat::RunEvent`]s on each Turn's channel. At least 1.
    capacity: usize,
    scopes: Arc<Mutex<BTreeMap<Scope, ScopeQueue>>>,
}

/// One active Scope: the sender the producer uses and how many Turns are queued but not started.
struct ScopeQueue {
    tx: mpsc::UnboundedSender<QueuedTurn>,
    /// Queued Turns, incremented by `converse` and decremented as the task starts one.
    queued: Arc<AtomicUsize>,
}

/// A Turn plus the channels it answers on.
struct QueuedTurn {
    turn: Turn,
    sender: RunSender,
}

impl Queues {
    /// Builds the queues over `pipeline`, giving each Turn's event channel room for
    /// `events_capacity` unread events (clamped to at least 1, as [`RunHandle::channel`] panics on
    /// 0).
    pub fn new(pipeline: Arc<Pipeline>, events_capacity: usize) -> Self {
        Queues {
            pipeline,
            capacity: events_capacity.max(1),
            scopes: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Accepts `turn` and returns the adapter's [`RunHandle`]. The Turn is queued behind any other
    /// Turn in the same Scope. The handle's events channel closes and its outcome arrives at the
    /// run's leader exit, **before** the settle (#37 D2); the next Turn is queued only after this
    /// run has settled (#37 D3).
    pub fn converse(&self, turn: Turn) -> RunHandle {
        let (sender, handle) = RunHandle::channel(self.capacity);
        let scope = Scope::from_chat(&turn.scope);
        let label = scope_label(&scope);

        // The send happens while the map lock is held, so it can never race the drain task's
        // removal of an idle Scope.
        let depth = {
            let mut scopes = lock(&self.scopes);
            let queue = scopes.entry(scope.clone()).or_insert_with(|| {
                let (tx, rx) = mpsc::unbounded_channel();
                let queued = Arc::new(AtomicUsize::new(0));
                let pipeline = Arc::clone(&self.pipeline);
                let scopes = Arc::clone(&self.scopes);
                tokio::spawn(drain(
                    scope.clone(),
                    rx,
                    Arc::clone(&queued),
                    pipeline,
                    scopes,
                ));
                ScopeQueue { tx, queued }
            });
            let depth = queue.queued.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = queue.tx.send(QueuedTurn { turn, sender });
            depth
        };

        tracing::info_span!("turn", scope = %label)
            .in_scope(|| tracing::info!(event = daemon::TURN_ACCEPTED, queue_depth = depth));
        handle
    }

    /// How many Scopes have a live queue task. 0 once every queue has drained and exited; this is
    /// what the idle-exit test reads.
    pub fn active_scopes(&self) -> usize {
        lock(&self.scopes).len()
    }
}

impl fmt::Debug for Queues {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Queues")
            .field("capacity", &self.capacity)
            .field("active_scopes", &self.active_scopes())
            .finish_non_exhaustive()
    }
}

/// The per-Scope task: run queued Turns one at a time, then remove the Scope and exit when there
/// is nothing left.
async fn drain(
    scope: Scope,
    mut rx: mpsc::UnboundedReceiver<QueuedTurn>,
    queued: Arc<AtomicUsize>,
    pipeline: Arc<Pipeline>,
    scopes: Arc<Mutex<BTreeMap<Scope, ScopeQueue>>>,
) {
    loop {
        match rx.try_recv() {
            Ok(QueuedTurn { turn, sender }) => {
                queued.fetch_sub(1, Ordering::SeqCst);
                let RunSender { events, outcome } = sender;
                // The pipeline sends the adapter's outcome at the run's leader exit (#37 D2) and
                // this only returns once the run is settled, so the next Turn waits for the settle.
                pipeline.run_turn(turn, Some(events), outcome).await;
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                // Removing an idle Scope must be atomic with a producer's send. Both take this
                // lock, and `converse` sends while holding it, so if the queue is still empty here
                // no send is in flight and it is safe to drop the entry.
                let mut map = lock(&scopes);
                if rx.is_empty() {
                    map.remove(&scope);
                    return;
                }
            }
            Err(mpsc::error::TryRecvError::Disconnected) => return,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::Queues;
    use crate::conversations::pipeline::test_support::{
        HAPPY, harness, harness_with_straggler_grace, read_calls, turn,
    };
    use claudeclaw_chat::{OutcomeKind, RunEvent};

    /// A fake `claude` that reports success, records its start, and leaves a straggler that traps
    /// `TERM` and records `strap-<n>`. The straggler is what makes the settle observable.
    fn straggler_script(counter: &str, log: &str) -> String {
        format!(
            "n=$(cat '{counter}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{counter}'\n\
             echo \"start-$n\" >> '{log}'\n\
             printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"id\":\"m0\",\"content\":[{{\"type\":\"text\",\"text\":\"x\"}}]}},\"parent_tool_use_id\":null}}'\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"session_id\":\"s-1\"}}'\n\
             ( trap \"echo strap-$n >> '{log}'; exit 0\" TERM; while :; do sleep 0.05; done ) &\n\
             exit 0"
        )
    }

    #[tokio::test]
    async fn turns_in_one_scope_run_in_order_and_wait_for_the_settle() {
        let h = harness(Duration::from_secs(30)).await;
        let counter = h.dir.path().join("count");
        h.write_claude(&straggler_script(
            &counter.display().to_string(),
            &h.calls.display().to_string(),
        ));
        let queues = Queues::new(Arc::clone(&h.pipeline), 8);

        let first = queues.converse(turn("one", "123"));
        let second = queues.converse(turn("two", "123"));
        let _ = first.outcome.await.unwrap();
        let _ = second.outcome.await.unwrap();

        let calls = read_calls(&h.calls);
        let start_two = calls
            .iter()
            .position(|line| line == "start-2")
            .unwrap_or_else(|| panic!("the second Turn never ran: {calls:?}"));
        let strap_one = calls
            .iter()
            .position(|line| line == "strap-1")
            .unwrap_or_else(|| panic!("the first Turn never settled: {calls:?}"));
        assert!(
            strap_one < start_two,
            "the second Turn must start only after the first run settled: {calls:?}"
        );
    }

    #[tokio::test]
    async fn the_reply_goes_out_before_the_settle_then_the_next_turn_waits() {
        // A long Straggler grace: the gap between the reply (leader exit) and the settle is
        // observable with margin.
        let h = harness_with_straggler_grace(Duration::from_secs(30), Duration::from_secs(2)).await;
        let counter = h.dir.path().join("count");
        h.write_claude(&straggler_script(
            &counter.display().to_string(),
            &h.calls.display().to_string(),
        ));
        let queues = Queues::new(Arc::clone(&h.pipeline), 8);

        let mut first = queues.converse(turn("one", "123"));
        let second = queues.converse(turn("two", "123"));

        // The events channel closes first (the `chat` contract), then the reply arrives at the first
        // run's leader exit, before its Straggler is dealt with.
        let mut deltas = 0;
        while let Some(event) = first.events.recv().await {
            if matches!(event, RunEvent::TextDelta(_)) {
                deltas += 1;
            }
        }
        assert_eq!(deltas, 1, "the run's event was delivered before the reply");

        let outcome = first.outcome.await.unwrap();
        assert_eq!(outcome.kind, OutcomeKind::Ok);

        let calls = read_calls(&h.calls);
        assert!(
            calls.iter().any(|line| line == "start-1"),
            "the first run should have started: {calls:?}"
        );
        assert!(
            !calls.iter().any(|line| line == "strap-1"),
            "the reply must arrive before the settle: {calls:?}"
        );
        assert!(
            !calls.iter().any(|line| line == "start-2"),
            "the next Turn must wait for the first run to settle: {calls:?}"
        );

        // Once the first run settles, the second Turn runs.
        let _ = second.outcome.await.unwrap();
        let calls = read_calls(&h.calls);
        let strap_one = calls
            .iter()
            .position(|line| line == "strap-1")
            .unwrap_or_else(|| panic!("the first Turn never settled: {calls:?}"));
        let start_two = calls
            .iter()
            .position(|line| line == "start-2")
            .unwrap_or_else(|| panic!("the second Turn never ran: {calls:?}"));
        assert!(
            strap_one < start_two,
            "the second Turn must start only after the first run settled: {calls:?}"
        );
    }

    #[tokio::test]
    async fn turns_in_two_scopes_run_concurrently() {
        let h = harness(Duration::from_secs(30)).await;
        h.write_claude(&format!(
            "echo \"start-$$\" >> '{log}'\n\
             sleep 0.3\n\
             echo \"end-$$\" >> '{log}'\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"session_id\":\"s-1\"}}'",
            log = h.calls.display()
        ));
        let queues = Queues::new(Arc::clone(&h.pipeline), 8);

        let first = queues.converse(turn("one", "111"));
        let second = queues.converse(turn("two", "222"));
        let _ = first.outcome.await.unwrap();
        let _ = second.outcome.await.unwrap();

        let calls = read_calls(&h.calls);
        assert!(calls.len() >= 2, "expected two runs: {calls:?}");
        assert!(
            calls[0].starts_with("start-") && calls[1].starts_with("start-"),
            "both Scopes must start before either ends: {calls:?}"
        );
    }

    #[tokio::test]
    async fn an_idle_queue_task_exits() {
        let h = harness(Duration::from_secs(30)).await;
        h.write_claude(HAPPY);
        let queues = Queues::new(Arc::clone(&h.pipeline), 8);

        let handle = queues.converse(turn("one", "123"));
        let _ = handle.outcome.await.unwrap();

        for _ in 0..200 {
            if queues.active_scopes() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            queues.active_scopes(),
            0,
            "the per-Scope task should exit once its queue is idle"
        );
    }
}
