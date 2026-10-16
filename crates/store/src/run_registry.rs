//! `daemon/state/runs.json`: the Run registry (architecture §4 "Run registry", §9.1; #22, #29
//! D2, #37 D3).
//!
//! The file's shape is [`claudeclaw_api::supervisor::RunRegistry`], which the client also reads
//! for a Daemon whose API is down (#29 D2), so it is used here as is. This module loads it,
//! applies the per-run write order as typed transitions, and saves it atomically:
//!
//! 1. [`RunRegistryFile::add`] a `spawning` entry before the spawn;
//! 2. [`RunRegistryFile::mark_running`] with the pid and start stamp once spawned (the stamp is
//!    optional: an OS start stamp that couldn't be read leaves it `null`);
//! 3. [`RunRegistryFile::mark_settling`] at leader exit (#37 D3);
//! 4. [`RunRegistryFile::remove`] once settled.
//!
//! `core::runs` drives it and `stop --force` and every Reap read it; when to do what is theirs.
//! Keys this build doesn't know survive a load and save.

use std::fs;

use claudeclaw_api::supervisor::{
    Leftover, ProcessStamp, ReapedRun, RegistryEntry, RegistryState, RegistryWorker, RunRegistry,
};
use serde_json::Value;

use crate::atomic::{self, DirMode, FileMode};
use crate::error::StoreError;
use crate::layout::Layout;
use crate::preserve::{Shape, merge_unknown};

/// A failed Run registry operation. Messages carry run ids and states, never contents.
#[derive(Debug, thiserror::Error)]
pub enum RunRegistryError {
    /// The file was written by a newer build (#29 D4 treats it as unreadable).
    #[error("runs.json has schema {found}, newer than this build's {supported}")]
    NewerSchema { found: u64, supported: u32 },
    /// [`RunRegistryFile::add`] of a `run_id` already registered.
    #[error("run {run_id} is already registered")]
    Duplicate { run_id: String },
    /// A transition for a `run_id` that isn't registered.
    #[error("run {run_id} is not registered")]
    NotRegistered { run_id: String },
    /// A transition out of order, e.g. `settling` before `running`.
    #[error("run {run_id} can't go from {from:?} to {to:?}")]
    Transition {
        run_id: String,
        from: RegistryState,
        to: RegistryState,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// `runs.json`, loaded. Mutate it, then [`RunRegistryFile::save`].
#[derive(Debug, Clone, PartialEq)]
pub struct RunRegistryFile {
    registry: RunRegistry,
    /// The file as loaded, whose unknown keys [`RunRegistryFile::save`] keeps.
    loaded: Option<Value>,
}

impl Default for RunRegistryFile {
    fn default() -> Self {
        Self::new()
    }
}

impl RunRegistryFile {
    /// An empty registry: no Worker, no runs.
    pub fn new() -> Self {
        RunRegistryFile {
            registry: RunRegistry {
                schema: RunRegistry::SCHEMA,
                worker: None,
                runs: Vec::new(),
                leftovers: Vec::new(),
                reaped: Vec::new(),
            },
            loaded: None,
        }
    }

    /// Reads `runs.json`. `None` when there is no file; a reaper reports that as it reports an
    /// unreadable one (#29 D4).
    pub fn load(layout: &Layout) -> Result<Option<Self>, RunRegistryError> {
        let path = layout.run_registry();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::io("read", &path)(e).into()),
        };
        let json_error = |source| StoreError::Json {
            path: path.clone(),
            source,
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(json_error)?;
        if !value.is_object() {
            return Err(StoreError::NotAnObject { path }.into());
        }
        let found = value.get("schema").and_then(Value::as_u64).unwrap_or(0);
        if found > u64::from(RunRegistry::SCHEMA) {
            return Err(RunRegistryError::NewerSchema {
                found,
                supported: RunRegistry::SCHEMA,
            });
        }
        let registry = serde_json::from_value(value.clone()).map_err(json_error)?;
        Ok(Some(RunRegistryFile {
            registry,
            loaded: Some(value),
        }))
    }

    /// Writes the registry atomically, creating `daemon/` (0700) and `daemon/state/` when
    /// missing (§9.1, #29 D2).
    pub fn save(&self, layout: &Layout) -> Result<(), RunRegistryError> {
        let mut value = serde_json::to_value(&self.registry).expect("a registry serialises");
        if let Some(loaded) = &self.loaded {
            merge_unknown(
                &mut value,
                loaded,
                &Shape {
                    id: &run_id,
                    maps: &[],
                },
            );
        }
        let mut bytes = serde_json::to_vec_pretty(&value).expect("a Value serialises");
        bytes.push(b'\n');
        atomic::create_dir(&layout.daemon_dir(), DirMode::Private)?;
        atomic::create_dir(&layout.state_dir(), DirMode::Default)?;
        atomic::write_atomic(&layout.run_registry(), &bytes, FileMode::Keep)?;
        Ok(())
    }

    /// The registry as it stands.
    pub fn registry(&self) -> &RunRegistry {
        &self.registry
    }

    /// The registered run `run_id`.
    pub fn get(&self, run_id: &str) -> Option<&RegistryEntry> {
        self.registry.runs.iter().find(|e| e.run_id == run_id)
    }

    /// Sets `worker`: written first thing at Worker start, `None` when no Worker runs (#29 D2).
    pub fn set_worker(&mut self, worker: Option<RegistryWorker>) {
        self.registry.worker = worker;
    }

    /// Registers a run; step 1 writes it `spawning`, with no pid or stamp (#29 D2).
    pub fn add(&mut self, entry: RegistryEntry) -> Result<(), RunRegistryError> {
        if self.get(&entry.run_id).is_some() {
            return Err(RunRegistryError::Duplicate {
                run_id: entry.run_id,
            });
        }
        self.registry.runs.push(entry);
        Ok(())
    }

    /// `spawning` → `running`, with the child's pid and OS start stamp (#29 D2 steps 3–4).
    /// `stamp` is `None` when the OS start stamp couldn't be read: the pid is still recorded, so
    /// a later Reap reports `unverified`, never `unknown_pid`.
    pub fn mark_running(
        &mut self,
        run_id: &str,
        pid: u32,
        stamp: Option<ProcessStamp>,
    ) -> Result<(), RunRegistryError> {
        let entry = self.transition(run_id, RegistryState::Spawning, RegistryState::Running)?;
        entry.pid = Some(pid);
        entry.stamp = stamp;
        Ok(())
    }

    /// `running` → `settling` at leader exit; the leader's pid and stamp stay (#37 D3).
    pub fn mark_settling(&mut self, run_id: &str) -> Result<(), RunRegistryError> {
        self.transition(run_id, RegistryState::Running, RegistryState::Settling)?;
        Ok(())
    }

    /// Removes a run once settled (#37 D3), or after a failed spawn.
    pub fn remove(&mut self, run_id: &str) -> Option<RegistryEntry> {
        let i = self.registry.runs.iter().position(|e| e.run_id == run_id)?;
        Some(self.registry.runs.remove(i))
    }

    /// Moves `leftover` into `leftovers`, out of `runs` (#29 D5).
    pub fn push_leftover(&mut self, leftover: Leftover) {
        self.remove(&leftover.entry.run_id);
        self.registry
            .leftovers
            .retain(|l| l.entry.run_id != leftover.entry.run_id);
        self.registry.leftovers.push(leftover);
    }

    /// Drops a leftover whose group is gone (#29 D5).
    pub fn remove_leftover(&mut self, run_id: &str) -> Option<Leftover> {
        let i = self
            .registry
            .leftovers
            .iter()
            .position(|l| l.entry.run_id == run_id)?;
        Some(self.registry.leftovers.remove(i))
    }

    /// Records the last Reap's results (#29 D2, D6).
    pub fn set_reaped(&mut self, reaped: Vec<ReapedRun>) {
        self.registry.reaped = reaped;
    }

    /// Takes the last Reap's results; the next Worker consumes them at start (#29 D6).
    pub fn take_reaped(&mut self) -> Vec<ReapedRun> {
        std::mem::take(&mut self.registry.reaped)
    }

    fn transition(
        &mut self,
        run_id: &str,
        from: RegistryState,
        to: RegistryState,
    ) -> Result<&mut RegistryEntry, RunRegistryError> {
        let entry = self
            .registry
            .runs
            .iter_mut()
            .find(|e| e.run_id == run_id)
            .ok_or_else(|| RunRegistryError::NotRegistered {
                run_id: run_id.to_owned(),
            })?;
        if entry.state != from {
            return Err(RunRegistryError::Transition {
                run_id: run_id.to_owned(),
                from: entry.state,
                to,
            });
        }
        entry.state = to;
        Ok(entry)
    }
}

/// Matches a run, leftover or reaped record across a load and a save.
fn run_id(v: &Value) -> Option<String> {
    v.get("run_id").and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use claudeclaw_api::scope::Scope;
    use claudeclaw_api::supervisor::{KillMethod, LinuxStamp, ReapOutcome, SpawnPurpose};
    use serde_json::json;

    use super::*;

    fn at() -> DateTime<Utc> {
        "2026-10-14T08:00:00Z".parse().unwrap()
    }

    fn stamp() -> ProcessStamp {
        ProcessStamp {
            linux: Some(LinuxStamp {
                boot_id: "b007".into(),
                start_ticks: 88_000,
            }),
            ..ProcessStamp::default()
        }
    }

    fn spawning(run_id: &str) -> RegistryEntry {
        RegistryEntry {
            run_id: run_id.into(),
            state: RegistryState::Spawning,
            pid: None,
            stamp: None,
            kill: KillMethod::ProcessGroup,
            purpose: SpawnPurpose::Turn,
            scope: Scope::Global,
            origin: None,
            started_at: at(),
        }
    }

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        (dir, layout)
    }

    #[test]
    fn a_run_goes_spawning_running_settling_removed() {
        let mut file = RunRegistryFile::new();
        file.add(spawning("run-1")).unwrap();
        assert_eq!(file.get("run-1").unwrap().state, RegistryState::Spawning);

        file.mark_running("run-1", 701, Some(stamp())).unwrap();
        let entry = file.get("run-1").unwrap();
        assert_eq!(entry.state, RegistryState::Running);
        assert_eq!(entry.pid, Some(701));
        assert_eq!(entry.stamp, Some(stamp()));

        file.mark_settling("run-1").unwrap();
        let entry = file.get("run-1").unwrap();
        assert_eq!(entry.state, RegistryState::Settling);
        assert_eq!(entry.pid, Some(701), "the leader's pid stays (#37 D3)");

        assert_eq!(file.remove("run-1").unwrap().run_id, "run-1");
        assert_eq!(file.get("run-1"), None);
        assert_eq!(file.remove("run-1"), None);
    }

    #[test]
    fn running_without_a_stamp_still_records_the_pid() {
        let mut file = RunRegistryFile::new();
        file.add(spawning("run-1")).unwrap();
        file.mark_running("run-1", 701, None).unwrap();
        let entry = file.get("run-1").unwrap();
        assert_eq!(entry.state, RegistryState::Running);
        assert_eq!(entry.pid, Some(701));
        assert_eq!(
            entry.stamp, None,
            "a stamp-less running entry is reaped as `unverified`, never `unknown_pid`"
        );
    }

    #[test]
    fn transitions_out_of_order_are_refused() {
        let mut file = RunRegistryFile::new();
        file.add(spawning("run-1")).unwrap();
        assert!(matches!(
            file.add(spawning("run-1")),
            Err(RunRegistryError::Duplicate { .. })
        ));
        assert!(matches!(
            file.mark_settling("run-1"),
            Err(RunRegistryError::Transition {
                from: RegistryState::Spawning,
                to: RegistryState::Settling,
                ..
            })
        ));
        file.mark_running("run-1", 1, Some(stamp())).unwrap();
        assert!(matches!(
            file.mark_running("run-1", 1, Some(stamp())),
            Err(RunRegistryError::Transition { .. })
        ));
        assert!(matches!(
            file.mark_settling("run-9"),
            Err(RunRegistryError::NotRegistered { .. })
        ));
    }

    #[test]
    fn a_missing_file_is_none_and_a_save_round_trips() {
        let (_dir, layout) = layout();
        assert_eq!(RunRegistryFile::load(&layout).unwrap(), None);

        let mut file = RunRegistryFile::new();
        file.set_worker(Some(RegistryWorker {
            pid: 700,
            boot_id: "boot-1".into(),
            stamp: stamp(),
        }));
        file.add(spawning("run-1")).unwrap();
        file.save(&layout).unwrap();

        let loaded = RunRegistryFile::load(&layout).unwrap().unwrap();
        assert_eq!(loaded.registry(), file.registry());
        let on_disk: Value =
            serde_json::from_slice(&fs::read(layout.run_registry()).unwrap()).unwrap();
        assert_eq!(on_disk["schema"], json!(1));
        assert_eq!(on_disk["runs"][0]["state"], json!("spawning"));
        assert_eq!(on_disk["runs"][0]["pid"], Value::Null);
    }

    #[test]
    fn unknown_keys_survive_a_transition_and_save() {
        let (_dir, layout) = layout();
        let mut entry = serde_json::to_value(spawning("run-1")).unwrap();
        entry["added_later"] = json!("kept");
        let on_disk = json!({
            "schema": 1,
            "worker": null,
            "runs": [entry],
            "leftovers": [],
            "reaped": [],
            "top_level_later": 1,
        });
        atomic::create_dir(&layout.state_dir(), DirMode::Default).unwrap();
        fs::write(layout.run_registry(), on_disk.to_string()).unwrap();

        let mut file = RunRegistryFile::load(&layout).unwrap().unwrap();
        file.mark_running("run-1", 701, Some(stamp())).unwrap();
        file.save(&layout).unwrap();
        let saved: Value =
            serde_json::from_slice(&fs::read(layout.run_registry()).unwrap()).unwrap();
        assert_eq!(saved["top_level_later"], json!(1));
        assert_eq!(saved["runs"][0]["added_later"], json!("kept"));
        assert_eq!(saved["runs"][0]["state"], json!("running"));
    }

    #[test]
    fn a_newer_schema_or_a_bad_shape_is_an_error() {
        let (_dir, layout) = layout();
        atomic::create_dir(&layout.state_dir(), DirMode::Default).unwrap();
        fs::write(
            layout.run_registry(),
            r#"{"schema": 2, "worker": null, "runs": [], "leftovers": [], "reaped": []}"#,
        )
        .unwrap();
        assert!(matches!(
            RunRegistryFile::load(&layout),
            Err(RunRegistryError::NewerSchema { found: 2, .. })
        ));
        fs::write(layout.run_registry(), r#"{"schema": 1}"#).unwrap();
        assert!(matches!(
            RunRegistryFile::load(&layout),
            Err(RunRegistryError::Store(StoreError::Json { .. }))
        ));
    }

    #[test]
    fn leftovers_and_reaped_results() {
        let mut file = RunRegistryFile::new();
        file.add(spawning("run-1")).unwrap();
        let entry = file.get("run-1").unwrap().clone();
        file.push_leftover(Leftover {
            entry: entry.clone(),
            outcome: ReapOutcome::UnknownPid,
            checked_at: at(),
        });
        assert_eq!(file.get("run-1"), None, "a leftover leaves `runs`");
        assert_eq!(file.registry().leftovers.len(), 1);
        assert_eq!(
            file.remove_leftover("run-1").unwrap().outcome,
            ReapOutcome::UnknownPid
        );
        assert!(file.registry().leftovers.is_empty());

        file.set_reaped(vec![ReapedRun {
            run_id: "run-1".into(),
            scope: Scope::Global,
            origin: None,
            purpose: SpawnPurpose::Turn,
            state: RegistryState::Settling,
            outcome: ReapOutcome::Killed,
        }]);
        assert_eq!(file.take_reaped().len(), 1);
        assert!(file.registry().reaped.is_empty());
    }
}
