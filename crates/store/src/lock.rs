//! `daemon.lock`: one Daemon per project dir (#9), the holder record for Offline writes (#22),
//! the start retry (#20, #45) and the non-creating shared-lock probe (#20 D1).
//!
//! The lock is an OS file lock taken with `std::fs::File::try_lock` (architecture §11 D17). While
//! it is held, the file's contents are the holder record, one JSON line such as
//! `{"pid":1234,"role":"cli","command":"jobs import"}`, so a racing start or `status` can say
//! who holds it (#22 §3).

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::layout::Layout;

/// Who holds `daemon.lock` (#22 §3). Written into the lock file once the lock is taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    pub role: HolderRole,
    /// The CLI command of an Offline write, e.g. `"jobs import"`. Command names only, never
    /// arguments, so the record carries no content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// The kind of process holding `daemon.lock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderRole {
    /// A running Daemon's Watchdog (`lock_holder: watchdog` in `status`, #22).
    Watchdog,
    /// The CLI doing an Offline write (#22 §3) or `clear-give-up` (#45 D3).
    Cli,
    /// A role written by a newer build.
    #[serde(other)]
    Unknown,
}

impl Holder {
    /// The record a Watchdog writes for itself.
    pub fn watchdog(pid: u32) -> Self {
        Holder {
            pid,
            role: HolderRole::Watchdog,
            command: None,
        }
    }

    /// The record an Offline write writes, naming its CLI command (#22 §3).
    pub fn offline_write(pid: u32, command: impl Into<String>) -> Self {
        Holder {
            pid,
            role: HolderRole::Cli,
            command: Some(command.into()),
        }
    }

    /// Whether the holder is an Offline write, which a starting Watchdog waits longer for
    /// (#45 D2).
    pub fn is_offline_write(&self) -> bool {
        self.role == HolderRole::Cli
    }
}

/// How long [`DaemonLock::acquire`] keeps retrying a held lock (#20, #45 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockWait {
    /// The time between attempts.
    pub step: Duration,
    /// Give up after this long, unless the holder is an Offline write.
    pub held: Duration,
    /// Give up after this long when the holder record names an Offline write.
    pub offline_write: Duration,
}

impl LockWait {
    /// A starting Watchdog: 50 ms steps for up to 1 s, so a concurrent read-only lock probe
    /// doesn't make it exit 73 by mistake (#20), or up to 10 s behind an Offline write (#45 D2).
    /// Exiting 73 after a refusal is the caller's.
    pub const WATCHDOG_START: LockWait = LockWait {
        step: Duration::from_millis(50),
        held: Duration::from_secs(1),
        offline_write: Duration::from_secs(10),
    };

    /// One attempt, no waiting.
    pub const ONCE: LockWait = LockWait {
        step: Duration::ZERO,
        held: Duration::ZERO,
        offline_write: Duration::ZERO,
    };
}

/// Why `daemon.lock` couldn't be taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another process holds it. `holder` is its record, when one could be read.
    #[error("daemon.lock is held{}", describe(.holder))]
    Held { holder: Option<Holder> },
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn describe(holder: &Option<Holder>) -> String {
    match holder {
        Some(Holder {
            pid,
            command: Some(command),
            ..
        }) => format!(" by `claudeclaw {command}` (pid {pid})"),
        Some(Holder { pid, .. }) => format!(" (pid {pid})"),
        None => String::new(),
    }
}

/// A held, exclusive `daemon.lock`. Dropping it clears the holder record and releases the lock.
#[derive(Debug)]
pub struct DaemonLock {
    file: File,
    path: PathBuf,
}

impl DaemonLock {
    /// Takes `daemon.lock` once, creating it if needed, and writes `holder` into it.
    pub fn try_acquire(layout: &Layout, holder: &Holder) -> Result<DaemonLock, LockError> {
        Self::acquire(layout, holder, LockWait::ONCE)
    }

    /// Takes `daemon.lock`, retrying while it is held as `wait` says, and writes `holder` into
    /// it. The `.claude/claudeclaw/` dir must exist.
    pub fn acquire(
        layout: &Layout,
        holder: &Holder,
        wait: LockWait,
    ) -> Result<DaemonLock, LockError> {
        let path = layout.daemon_lock();
        // Never truncate on open: until the lock is ours, the contents are the holder's record.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(StoreError::io("open", &path))?;
        let start = Instant::now();
        let mut logged_wait = false;
        // Sticky: once the holder was seen as an Offline write, keep its longer limit. Its
        // release empties the record just before unlocking, and a read in that window must not
        // fall back to the short limit and give up.
        let mut saw_offline = false;
        loop {
            match file.try_lock() {
                Ok(()) => {
                    let mut lock = DaemonLock { file, path };
                    lock.write_holder(holder)?;
                    return Ok(lock);
                }
                Err(TryLockError::WouldBlock) => {
                    let current = read_holder(&path);
                    saw_offline |= current.as_ref().is_some_and(Holder::is_offline_write);
                    let offline = saw_offline;
                    let limit = if offline {
                        wait.offline_write
                    } else {
                        wait.held
                    };
                    if start.elapsed() >= limit {
                        return Err(LockError::Held { holder: current });
                    }
                    if offline && !logged_wait {
                        logged_wait = true;
                        tracing::info!(
                            event = "lock.wait_offline_write",
                            holder_pid = current.as_ref().map(|h| h.pid),
                            "waiting for an Offline write to release daemon.lock"
                        );
                    }
                    std::thread::sleep(wait.step);
                }
                Err(TryLockError::Error(e)) => return Err(StoreError::io("lock", &path)(e).into()),
            }
        }
    }

    /// The lock file, e.g. to clear `FD_CLOEXEC` before a re-exec so the lock is never released
    /// (architecture §4 "Update").
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Replaces the holder record, e.g. when the lock passes to a re-executed Watchdog.
    pub fn write_holder(&mut self, holder: &Holder) -> Result<(), StoreError> {
        let mut line = serde_json::to_vec(holder).expect("Holder serialises");
        line.push(b'\n');
        let io = StoreError::io("write holder record to", &self.path);
        self.file
            .set_len(0)
            .and_then(|()| self.file.seek(SeekFrom::Start(0)))
            .and_then(|_| self.file.write_all(&line))
            .and_then(|()| self.file.sync_data())
            .map_err(io)
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // Best effort: a stale record is harmless, because it is only read while the lock is held.
        let _ = self.file.set_len(0);
        let _ = self.file.unlock();
    }
}

/// What the non-creating lock probe saw (#20 D1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockProbe {
    /// No `daemon.lock`: a Daemon never started here. Nothing was created.
    Absent,
    /// The file exists and nobody holds it: no Watchdog (it exited or was killed).
    Free,
    /// Someone holds it. `holder` is its record, when one could be read.
    Held { holder: Option<Holder> },
}

/// Probes `daemon.lock` without creating it or disturbing a holder (#20 D1): opens it read-only,
/// tries a **shared** lock and releases it at once. Used by `client::offline_status` and
/// `claudeclaw status` through the binary.
pub fn probe(layout: &Layout) -> Result<LockProbe, StoreError> {
    let path = layout.daemon_lock();
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LockProbe::Absent),
        Err(e) => return Err(StoreError::io("open", &path)(e)),
    };
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            Ok(LockProbe::Free)
        }
        Err(TryLockError::WouldBlock) => Ok(LockProbe::Held {
            holder: read_holder(&path),
        }),
        Err(TryLockError::Error(e)) => Err(StoreError::io("lock", &path)(e)),
    }
}

/// Reads the holder record through a fresh handle. `None` when it is empty, torn by a concurrent
/// rewrite, or unreadable (on Windows a held lock also blocks reads).
fn read_holder(path: &Path) -> Option<Holder> {
    let mut text = String::new();
    File::open(path).ok()?.read_to_string(&mut text).ok()?;
    serde_json::from_str(text.trim()).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.root()).unwrap();
        (dir, layout)
    }

    #[test]
    fn a_second_try_lock_is_refused_and_names_the_holder() {
        let (_dir, layout) = layout();
        let first = DaemonLock::try_acquire(&layout, &Holder::watchdog(4321)).unwrap();
        let err = DaemonLock::try_acquire(&layout, &Holder::watchdog(1)).unwrap_err();
        match err {
            LockError::Held { holder } if !cfg!(windows) => {
                assert_eq!(holder, Some(Holder::watchdog(4321)));
            }
            LockError::Held { .. } => {}
            other => panic!("{other:?}"),
        }
        drop(first);
        DaemonLock::try_acquire(&layout, &Holder::watchdog(1)).unwrap();
    }

    #[test]
    fn the_retry_gets_the_lock_when_the_holder_drops_within_a_second() {
        let (_dir, layout) = layout();
        let (tx, rx) = mpsc::channel();
        let holder_layout = layout.clone();
        let holder = thread::spawn(move || {
            let lock = DaemonLock::try_acquire(&holder_layout, &Holder::watchdog(7)).unwrap();
            tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(300));
            drop(lock);
        });
        rx.recv().unwrap();
        let start = Instant::now();
        let lock = DaemonLock::acquire(&layout, &Holder::watchdog(8), LockWait::WATCHDOG_START);
        assert!(lock.is_ok(), "{:?}", lock.err());
        assert!(start.elapsed() < Duration::from_secs(1));
        holder.join().unwrap();
    }

    #[test]
    fn the_retry_gives_up_after_its_limit() {
        let (_dir, layout) = layout();
        let _held = DaemonLock::try_acquire(&layout, &Holder::watchdog(7)).unwrap();
        let wait = LockWait {
            step: Duration::from_millis(10),
            held: Duration::from_millis(100),
            offline_write: Duration::from_secs(10),
        };
        let start = Instant::now();
        let err = DaemonLock::acquire(&layout, &Holder::watchdog(8), wait).unwrap_err();
        assert!(matches!(err, LockError::Held { .. }));
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(100), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
    }

    // On Windows a held lock blocks reading the record, so the longer wait can't be seen there.
    #[cfg(unix)]
    #[test]
    fn an_offline_write_gets_the_longer_wait() {
        let (_dir, layout) = layout();
        let (tx, rx) = mpsc::channel();
        let holder_layout = layout.clone();
        let holder = thread::spawn(move || {
            let cli = Holder::offline_write(7, "jobs import");
            let lock = DaemonLock::try_acquire(&holder_layout, &cli).unwrap();
            tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(400));
            drop(lock);
        });
        rx.recv().unwrap();
        let wait = LockWait {
            step: Duration::from_millis(10),
            held: Duration::from_millis(100),
            offline_write: Duration::from_secs(5),
        };
        let start = Instant::now();
        DaemonLock::acquire(&layout, &Holder::watchdog(8), wait).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(300));
        holder.join().unwrap();
    }

    // An Offline write's release empties the record just before it unlocks. A waiter that reads
    // the empty record in that window must keep the longer wait, not give up on the short one.
    #[cfg(unix)]
    #[test]
    fn an_emptied_record_keeps_the_offline_write_wait() {
        let (_dir, layout) = layout();
        let (tx, rx) = mpsc::channel();
        let holder_layout = layout.clone();
        let holder = thread::spawn(move || {
            let cli = Holder::offline_write(7, "jobs import");
            let lock = DaemonLock::try_acquire(&holder_layout, &cli).unwrap();
            tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(50));
            OpenOptions::new()
                .write(true)
                .open(holder_layout.daemon_lock())
                .unwrap()
                .set_len(0)
                .unwrap();
            thread::sleep(Duration::from_millis(300));
            drop(lock);
        });
        rx.recv().unwrap();
        let wait = LockWait {
            step: Duration::from_millis(10),
            held: Duration::from_millis(100),
            offline_write: Duration::from_secs(5),
        };
        let start = Instant::now();
        DaemonLock::acquire(&layout, &Holder::watchdog(8), wait).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(300));
        holder.join().unwrap();
    }

    #[test]
    fn the_probe_creates_nothing_and_tells_free_from_held() {
        let (_dir, layout) = layout();
        assert_eq!(probe(&layout).unwrap(), LockProbe::Absent);
        assert!(!layout.daemon_lock().exists());

        let lock =
            DaemonLock::try_acquire(&layout, &Holder::offline_write(9, "clear-give-up")).unwrap();
        match probe(&layout).unwrap() {
            LockProbe::Held { holder } if !cfg!(windows) => {
                assert_eq!(holder, Some(Holder::offline_write(9, "clear-give-up")));
            }
            LockProbe::Held { .. } => {}
            other => panic!("{other:?}"),
        }
        drop(lock);
        assert_eq!(probe(&layout).unwrap(), LockProbe::Free);
        assert_eq!(std::fs::read(layout.daemon_lock()).unwrap(), b"");
    }

    #[test]
    fn the_probe_does_not_block_a_start() {
        let (_dir, layout) = layout();
        drop(DaemonLock::try_acquire(&layout, &Holder::watchdog(1)).unwrap());
        // A shared probe lock is released at once, so an exclusive try right after succeeds.
        assert_eq!(probe(&layout).unwrap(), LockProbe::Free);
        DaemonLock::try_acquire(&layout, &Holder::watchdog(2)).unwrap();
    }

    #[test]
    fn the_holder_record_is_one_json_line() {
        let cli = Holder::offline_write(42, "jobs import");
        assert_eq!(
            serde_json::to_string(&cli).unwrap(),
            r#"{"pid":42,"role":"cli","command":"jobs import"}"#
        );
        assert_eq!(
            serde_json::to_string(&Holder::watchdog(42)).unwrap(),
            r#"{"pid":42,"role":"watchdog"}"#
        );
        let future: Holder = serde_json::from_str(r#"{"pid":1,"role":"app"}"#).unwrap();
        assert_eq!(future.role, HolderRole::Unknown);
        assert_eq!(
            LockError::Held { holder: Some(cli) }.to_string(),
            "daemon.lock is held by `claudeclaw jobs import` (pid 42)"
        );
    }
}
