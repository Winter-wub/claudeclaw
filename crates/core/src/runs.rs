//! The Run registry service (architecture §4 `core::runs`; #29 D2, #37 D3, #74).
//!
//! [`RunRegistryFile`] is the `daemon/state/runs.json` store; this module is who drives it. One
//! writer task owns a [`RunRegistryFile`] and serialises every change through it, saving with
//! `spawn_blocking` because the file IO is synchronous (architecture §2). [`RunRegistry`] is that
//! task's cloneable handle: the Worker registers a `spawning` entry before a spawn
//! ([`RunRegistry::register_spawning`]) and the [`SpawnHook`] marks it `running` and `settling`
//! and removes it as the run's life goes (#29 D2, #37 D3).
//!
//! **Ordering.** [`RunRegistry::register_spawning`] awaits the writer, so the `spawning` entry is
//! in memory before the caller spawns. Should a hook fire before its Register is processed, the
//! writer buffers that run's hook events and replays them in order once the Register lands: the
//! `spawning → running → settling → removed` order holds either way. A hook never blocks.
//!
//! **A failed write.** The in-memory registry is authoritative. A save that fails logs
//! [`daemon::RUN_REGISTRY_WRITE_FAILED`] and is otherwise dropped, so a full disk or a bad path
//! never stops a run or a hook; this is the write-side twin of the Reap's unreadable read (#29
//! D4, #74).
//!
//! [`chat_origin`] builds the `origin` entry a chat Turn leaves behind, so the next Worker can
//! send the interrupted reply after a crash (#29 D2, #79).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use claudeclaw_api::adapter::Transport as ApiTransport;
use claudeclaw_api::log_event::daemon;
use claudeclaw_api::supervisor::{
    ChatOrigin, LinuxStamp, MacosStamp, ProcessStamp as ApiProcessStamp, ReapedRun, RegistryEntry,
    WindowsStamp,
};
use claudeclaw_chat::{Transport as ChatTransport, Turn};
use claudeclaw_runner::reap::ProcessStamp as RunnerProcessStamp;
use claudeclaw_runner::spawn::SpawnHook;
use claudeclaw_store::{Layout, RunRegistryError, RunRegistryFile};
use tokio::sync::{mpsc, oneshot};

/// Why a `core::runs` operation failed. Messages name run ids and states, never contents.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The registry itself refused the operation, such as a duplicate `run_id`.
    #[error(transparent)]
    Registry(#[from] RunRegistryError),
    /// The writer task has stopped; a fresh [`RunRegistry::open`] is needed.
    #[error("the Run registry writer has stopped")]
    WriterGone,
    /// A `spawn_blocking` registry task did not finish.
    #[error("a Run registry task did not finish")]
    Join(#[from] tokio::task::JoinError),
}

/// The Run registry's cloneable handle. Every clone shares the one writer task, so any holder's
/// hook and query land in the same order.
#[derive(Clone)]
pub struct RunRegistry {
    tx: mpsc::UnboundedSender<Command>,
}

impl RunRegistry {
    /// Opens `runs.json` and starts the writer task. A missing file starts an empty registry; a
    /// file that cannot be read is an error for the caller to judge (a Reap reports the same read
    /// as `reap.registry_unreadable`, #29 D4). The read runs on `spawn_blocking` (§2).
    pub async fn open(layout: &Layout) -> Result<Self, RunError> {
        let read_layout = layout.clone();
        let loaded =
            tokio::task::spawn_blocking(move || RunRegistryFile::load(&read_layout)).await??;
        let (tx, rx) = mpsc::unbounded_channel();
        let writer = Writer {
            file: loaded.unwrap_or_default(),
            layout: layout.clone(),
            registered: BTreeSet::new(),
            pending: BTreeMap::new(),
        };
        tokio::spawn(writer.run(rx));
        Ok(RunRegistry { tx })
    }

    /// Registers `entry` as `spawning` and waits for the writer, so the entry is in memory before
    /// the caller spawns (#29 D2 step 1). A write that fails is logged, not returned: the
    /// in-memory registry is authoritative.
    pub async fn register_spawning(&self, entry: RegistryEntry) -> Result<(), RunError> {
        let (ack, result) = oneshot::channel();
        if self
            .tx
            .send(Command::Register {
                entry: Box::new(entry),
                ack,
            })
            .is_err()
        {
            return Err(RunError::WriterGone);
        }
        match result.await {
            Ok(result) => result,
            Err(_) => Err(RunError::WriterGone),
        }
    }

    /// Takes the last Reap's results, clearing them on disk, so the next Worker consumes them
    /// exactly once (#29 D6). Empty when the writer has stopped.
    pub async fn take_reaped(&self) -> Vec<ReapedRun> {
        let (ack, result) = oneshot::channel();
        if self.tx.send(Command::TakeReaped { ack }).is_err() {
            return Vec::new();
        }
        result.await.unwrap_or_default()
    }

    /// Hands `command` to the writer without blocking. A writer that has stopped drops it: a hook
    /// is best effort and must never wait.
    fn enqueue(&self, command: Command) {
        let _ = self.tx.send(command);
    }
}

impl fmt::Debug for RunRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunRegistry").finish_non_exhaustive()
    }
}

impl SpawnHook for RunRegistry {
    fn spawned(&self, run_id: &str, pid: u32, stamp: Option<&RunnerProcessStamp>) {
        self.enqueue(Command::Spawned {
            run_id: run_id.to_owned(),
            pid,
            stamp: stamp.map(to_api_stamp),
        });
    }

    fn leader_exited(&self, run_id: &str) {
        self.enqueue(Command::LeaderExited {
            run_id: run_id.to_owned(),
        });
    }

    fn settled(&self, run_id: &str) {
        self.enqueue(Command::Settled {
            run_id: run_id.to_owned(),
        });
    }
}

/// The Run registry `origin` for a chat Turn (#79): its transport and the message it answers.
/// These are identifiers, never a message body.
pub fn chat_origin(turn: &Turn) -> ChatOrigin {
    ChatOrigin {
        adapter: to_api_transport(turn.scope.transport()),
        channel: turn.origin.channel.clone(),
        message: turn.origin.message.clone(),
    }
}

/// One command the writer task serves, in arrival order.
enum Command {
    Register {
        entry: Box<RegistryEntry>,
        ack: oneshot::Sender<Result<(), RunError>>,
    },
    Spawned {
        run_id: String,
        pid: u32,
        stamp: Option<ApiProcessStamp>,
    },
    LeaderExited {
        run_id: String,
    },
    Settled {
        run_id: String,
    },
    TakeReaped {
        ack: oneshot::Sender<Vec<ReapedRun>>,
    },
}

/// One hook event, buffered until its run is registered.
enum HookEvent {
    Spawned {
        pid: u32,
        stamp: Option<ApiProcessStamp>,
    },
    LeaderExited,
    Settled,
}

/// The single writer task: it owns the [`RunRegistryFile`] and applies every command in order.
struct Writer {
    file: RunRegistryFile,
    layout: Layout,
    /// Run ids whose Register has been processed and whose run hasn't settled. Their hook events
    /// apply straight away; the id leaves with the entry at the settle, so the set stays bounded.
    registered: BTreeSet<String>,
    /// Hook events that arrived before their Register, replayed in order once it lands.
    pending: BTreeMap<String, Vec<HookEvent>>,
}

impl Writer {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Command>) {
        while let Some(command) = rx.recv().await {
            self.handle(command).await;
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Register { entry, ack } => {
                let run_id = entry.run_id.clone();
                let result = self.file.add(*entry);
                if result.is_ok() {
                    self.registered.insert(run_id.clone());
                    if let Some(pending) = self.pending.remove(&run_id) {
                        for event in pending {
                            self.apply(&run_id, event);
                        }
                    }
                    self.save().await;
                }
                let _ = ack.send(result.map_err(RunError::from));
            }
            Command::Spawned { run_id, pid, stamp } => {
                self.hook(run_id, HookEvent::Spawned { pid, stamp }).await;
            }
            Command::LeaderExited { run_id } => {
                self.hook(run_id, HookEvent::LeaderExited).await;
            }
            Command::Settled { run_id } => {
                self.hook(run_id, HookEvent::Settled).await;
            }
            Command::TakeReaped { ack } => {
                let reaped = self.file.take_reaped();
                if !reaped.is_empty() {
                    self.save().await;
                }
                let _ = ack.send(reaped);
            }
        }
    }

    /// Applies a hook event, or buffers it when its run isn't registered yet.
    async fn hook(&mut self, run_id: String, event: HookEvent) {
        if !self.registered.contains(&run_id) {
            self.pending.entry(run_id).or_default().push(event);
            return;
        }
        self.apply(&run_id, event);
        self.save().await;
    }

    /// Applies one hook event to the in-memory registry. A transition the store refuses means a
    /// duplicate or stray hook; it is logged and dropped, never a panic.
    fn apply(&mut self, run_id: &str, event: HookEvent) {
        match event {
            HookEvent::Spawned { pid, stamp } => {
                if let Err(err) = self.file.mark_running(run_id, pid, stamp) {
                    warn_refused(run_id, &err);
                }
            }
            HookEvent::LeaderExited => {
                if let Err(err) = self.file.mark_settling(run_id) {
                    warn_refused(run_id, &err);
                }
            }
            HookEvent::Settled => {
                self.file.remove(run_id);
                self.registered.remove(run_id);
            }
        }
    }

    /// Saves the registry on `spawn_blocking`, logging a failure without ever failing the caller.
    async fn save(&self) {
        let file = self.file.clone();
        let layout = self.layout.clone();
        let result = tokio::task::spawn_blocking(move || file.save(&layout)).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => log_write_failed(&err),
            Err(err) => log_write_failed(&err),
        }
    }
}

/// The Runner's stamp enum onto the wire struct the registry stores (#29 D2).
fn to_api_stamp(stamp: &RunnerProcessStamp) -> ApiProcessStamp {
    match stamp {
        RunnerProcessStamp::Macos { sec, usec } => ApiProcessStamp {
            macos: Some(MacosStamp {
                sec: *sec,
                usec: *usec,
            }),
            ..ApiProcessStamp::default()
        },
        RunnerProcessStamp::Linux {
            boot_id,
            start_ticks,
        } => ApiProcessStamp {
            linux: Some(LinuxStamp {
                boot_id: boot_id.clone(),
                start_ticks: *start_ticks,
            }),
            ..ApiProcessStamp::default()
        },
        RunnerProcessStamp::Windows { creation_time } => ApiProcessStamp {
            windows: Some(WindowsStamp {
                creation_time: *creation_time,
            }),
            ..ApiProcessStamp::default()
        },
    }
}

/// The chat transport onto the management-API wire transport (§6, management-api §1.7).
fn to_api_transport(transport: ChatTransport) -> ApiTransport {
    match transport {
        ChatTransport::Discord => ApiTransport::Discord,
        ChatTransport::Telegram => ApiTransport::Telegram,
        ChatTransport::Slack => ApiTransport::Slack,
    }
}

fn warn_refused(run_id: &str, err: &RunRegistryError) {
    tracing::warn!(
        run_id = %run_id,
        err = %err,
        "a Run registry transition was refused"
    );
}

fn log_write_failed(cause: &dyn fmt::Display) {
    tracing::error!(
        event = daemon::RUN_REGISTRY_WRITE_FAILED,
        cause = %cause,
        "the Run registry could not be written; the in-memory registry stands"
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, Utc};
    use claudeclaw_api::scope::Scope as ApiScope;
    use claudeclaw_api::supervisor::{KillMethod, ReapOutcome, RegistryState, SpawnPurpose};
    use claudeclaw_chat::{Author, ChatScope, MessageRef, Transport};

    use super::*;

    fn at() -> DateTime<Utc> {
        "2026-10-14T08:00:00Z".parse().unwrap()
    }

    fn spawning(run_id: &str) -> RegistryEntry {
        RegistryEntry {
            run_id: run_id.into(),
            state: RegistryState::Spawning,
            pid: None,
            stamp: None,
            kill: KillMethod::ProcessGroup,
            purpose: SpawnPurpose::Turn,
            scope: ApiScope::Discord("123".into()),
            origin: None,
            started_at: at(),
        }
    }

    fn runner_stamp() -> RunnerProcessStamp {
        RunnerProcessStamp::Linux {
            boot_id: "b007".into(),
            start_ticks: 88_000,
        }
    }

    fn reaped() -> ReapedRun {
        ReapedRun {
            run_id: "run-9".into(),
            scope: ApiScope::Global,
            origin: None,
            purpose: SpawnPurpose::Summary,
            state: RegistryState::Settling,
            outcome: ReapOutcome::Killed,
        }
    }

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        (dir, layout)
    }

    /// Reads `runs.json` off disk, so a test can see what the writer actually persisted.
    async fn on_disk(layout: &Layout) -> RunRegistryFile {
        let layout = layout.clone();
        tokio::task::spawn_blocking(move || RunRegistryFile::load(&layout))
            .await
            .expect("the load task ran")
            .expect("the registry is readable")
            .expect("the registry file exists")
    }

    #[tokio::test]
    async fn a_run_moves_through_the_hook_and_leaves_an_empty_registry() {
        let (_dir, layout) = layout();
        let registry = RunRegistry::open(&layout).await.unwrap();
        registry.register_spawning(spawning("run-1")).await.unwrap();

        // Register awaited the writer, so the `spawning` entry is already on disk.
        let file = on_disk(&layout).await;
        assert_eq!(
            file.get("run-1").unwrap().state,
            RegistryState::Spawning,
            "written before the spawn (#29 D2 step 1)"
        );

        // The hooks are fire-and-forget; `take_reaped` is a round trip that drains them first.
        registry.spawned("run-1", 701, Some(&runner_stamp()));
        registry.take_reaped().await;
        let file = on_disk(&layout).await;
        let running = file.get("run-1").unwrap();
        assert_eq!(running.state, RegistryState::Running);
        assert_eq!(running.pid, Some(701));
        assert_eq!(running.stamp, Some(to_api_stamp(&runner_stamp())));

        registry.leader_exited("run-1");
        registry.take_reaped().await;
        assert_eq!(
            on_disk(&layout).await.get("run-1").unwrap().state,
            RegistryState::Settling
        );

        registry.settled("run-1");
        registry.take_reaped().await;
        assert!(on_disk(&layout).await.registry().runs.is_empty());
    }

    #[tokio::test]
    async fn a_hook_that_fires_before_register_is_replayed_in_order() {
        let (_dir, layout) = layout();
        let registry = RunRegistry::open(&layout).await.unwrap();

        // `spawned` reaches the writer for a run whose Register hasn't been processed yet.
        registry.spawned("run-1", 701, Some(&runner_stamp()));
        registry.register_spawning(spawning("run-1")).await.unwrap();

        // Register replays the buffered `spawned`, so the entry is `running`, with its pid; had
        // the event been dropped, it would still be `spawning`.
        let file = on_disk(&layout).await;
        let entry = file.get("run-1").unwrap();
        assert_eq!(entry.state, RegistryState::Running);
        assert_eq!(entry.pid, Some(701));

        // The rest of the life replays too, in order.
        registry.leader_exited("run-1");
        registry.settled("run-1");
        registry.take_reaped().await;
        assert!(on_disk(&layout).await.registry().runs.is_empty());
    }

    #[tokio::test]
    async fn a_failed_write_keeps_the_in_memory_entry_and_logs() {
        let (_dir, layout) = layout();
        let registry = RunRegistry::open(&layout).await.unwrap();

        // Make every save fail: `daemon/state` is a regular file, so `create_dir_all` fails.
        std::fs::create_dir_all(layout.daemon_dir()).unwrap();
        std::fs::write(layout.state_dir(), b"not a directory").unwrap();

        let events = Events::default();
        let _guard = tracing::subscriber::set_default(events.clone());

        // The write fails, but the registration stands in memory and is acknowledged.
        registry.register_spawning(spawning("run-1")).await.unwrap();
        // Registering the same run again is refused, which proves the first stayed in memory.
        assert!(matches!(
            registry.register_spawning(spawning("run-1")).await,
            Err(RunError::Registry(RunRegistryError::Duplicate { .. }))
        ));

        let failed = events.named(daemon::RUN_REGISTRY_WRITE_FAILED);
        assert_eq!(failed.len(), 1, "one failed save: {failed:?}");
        assert!(
            failed[0].iter().any(|(name, _)| name == "cause"),
            "the failure carries its cause: {failed:?}"
        );
    }

    #[tokio::test]
    async fn take_reaped_returns_the_last_reap_and_clears_it() {
        let (_dir, layout) = layout();
        // Seed a Reap's results on disk, as a Reap would.
        let mut file = RunRegistryFile::new();
        file.set_reaped(vec![reaped()]);
        let seed_layout = layout.clone();
        tokio::task::spawn_blocking(move || file.save(&seed_layout))
            .await
            .unwrap()
            .unwrap();

        let registry = RunRegistry::open(&layout).await.unwrap();
        assert_eq!(registry.take_reaped().await, vec![reaped()]);
        // It is cleared on disk, and a second take is empty.
        assert!(on_disk(&layout).await.registry().reaped.is_empty());
        assert!(registry.take_reaped().await.is_empty());
    }

    #[test]
    fn chat_origin_carries_the_transport_and_the_message() {
        let turn = Turn {
            scope: ChatScope::Thread {
                transport: Transport::Discord,
                id: "12".into(),
                parent: "7".into(),
            },
            origin: MessageRef {
                channel: "12".into(),
                message: "34".into(),
            },
            author: Author {
                display_name: "somebody".into(),
            },
            text: "a prompt".into(),
            context_lines: Vec::new(),
            attachments: Vec::new(),
            agent_dir: None,
        };
        assert_eq!(
            chat_origin(&turn),
            ChatOrigin {
                adapter: ApiTransport::Discord,
                channel: "12".into(),
                message: "34".into(),
            }
        );
    }

    #[test]
    fn every_chat_transport_maps_to_its_wire_form() {
        assert_eq!(to_api_transport(Transport::Discord), ApiTransport::Discord);
        assert_eq!(
            to_api_transport(Transport::Telegram),
            ApiTransport::Telegram
        );
        assert_eq!(to_api_transport(Transport::Slack), ApiTransport::Slack);
    }

    #[test]
    fn every_runner_stamp_maps_onto_the_wire_stamp() {
        assert_eq!(
            to_api_stamp(&RunnerProcessStamp::Macos { sec: 1, usec: 2 }),
            ApiProcessStamp {
                macos: Some(MacosStamp { sec: 1, usec: 2 }),
                ..ApiProcessStamp::default()
            }
        );
        assert_eq!(
            to_api_stamp(&runner_stamp()),
            ApiProcessStamp {
                linux: Some(LinuxStamp {
                    boot_id: "b007".into(),
                    start_ticks: 88_000,
                }),
                ..ApiProcessStamp::default()
            }
        );
        assert_eq!(
            to_api_stamp(&RunnerProcessStamp::Windows { creation_time: 7 }),
            ApiProcessStamp {
                windows: Some(WindowsStamp { creation_time: 7 }),
                ..ApiProcessStamp::default()
            }
        );
    }

    /// A minimal subscriber that keeps every event's fields, to check a catalogue event (#74).
    #[derive(Clone, Default)]
    struct Events(Arc<Mutex<Vec<EventFields>>>);

    /// One event's fields, as `(name, value)` pairs.
    type EventFields = Vec<(String, String)>;

    impl Events {
        fn named(&self, name: &str) -> Vec<EventFields> {
            let all = self.0.lock().unwrap();
            all.iter()
                .filter(|fields| fields.iter().any(|(k, v)| k == "event" && v == name))
                .cloned()
                .collect()
        }
    }

    struct Fields(Vec<(String, String)>);

    impl tracing::field::Visit for Fields {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push((field.name().to_owned(), value.to_owned()));
        }
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
            self.0.push((field.name().to_owned(), format!("{value:?}")));
        }
    }

    impl tracing::Subscriber for Events {
        fn register_callsite(
            &self,
            _: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut fields = Fields(Vec::new());
            event.record(&mut fields);
            self.0.lock().unwrap().push(fields.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
}
