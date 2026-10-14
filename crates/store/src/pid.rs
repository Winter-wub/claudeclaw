//! `daemon.pid`: guard the other daemon (#14 rule 10, architecture §8 `PidGuard`).
//!
//! The Watchdog takes `daemon.lock` first, then checks `daemon.pid` and writes its own pid as
//! `"<pid>\n"`, TS's format, so a stray TS start in the same dir sees a live daemon and aborts.
//! The TS daemon never takes `daemon.lock`, so this file is the only way to see it.

use std::fs;

use crate::atomic::{self, FileMode};
use crate::error::StoreError;
use crate::layout::Layout;

/// What `daemon.pid` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidCheck {
    /// No file.
    Absent,
    /// Not a pid, or a pid whose process is gone; TS deletes such a file (`src/pid.ts:22-35`).
    Stale,
    /// A live process this Daemon owns: this process, or the Watchdog `supervisor.json`
    /// recorded, so pid reuse after a `kill -9` isn't taken for a foreign daemon (#20 D1).
    Ours(u32),
    /// A live process that is not this Daemon: a TS daemon or another Rust one.
    Foreign(u32),
}

/// `daemon.pid` names a live process that is not this Daemon (#14 rule 10). `start` exits 73,
/// other commands 6 (#22); that is the caller's.
#[derive(Debug, thiserror::Error)]
pub enum PidError {
    #[error("daemon.pid names a live process that is not this Daemon (pid {0})")]
    Foreign(u32),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Reads `daemon.pid` and classifies it. `ours` lists the pids that count as this Daemon;
/// `is_alive` says whether a pid names a running process (the binary passes `kill(pid, 0)` on
/// Unix and `OpenProcess` on Windows).
pub fn check(
    layout: &Layout,
    ours: &[u32],
    is_alive: impl Fn(u32) -> bool,
) -> Result<PidCheck, StoreError> {
    let path = layout.daemon_pid();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PidCheck::Absent),
        Err(e) => return Err(StoreError::io("read", &path)(e)),
    };
    // TS: `Number(raw.trim())`, and a 0 or NaN pid is stale (`src/pid.ts:16-28`).
    let pid = match text.trim().parse::<u32>() {
        Ok(pid) if pid != 0 => pid,
        _ => return Ok(PidCheck::Stale),
    };
    Ok(if !is_alive(pid) {
        PidCheck::Stale
    } else if ours.contains(&pid) {
        PidCheck::Ours(pid)
    } else {
        PidCheck::Foreign(pid)
    })
}

/// Checks `daemon.pid`, then writes `pid` into it (#14 rule 10). Call it while holding
/// `daemon.lock`. `pid` itself always counts as ours.
pub fn claim(
    layout: &Layout,
    pid: u32,
    ours: &[u32],
    is_alive: impl Fn(u32) -> bool,
) -> Result<(), PidError> {
    let mut ours = ours.to_vec();
    ours.push(pid);
    if let PidCheck::Foreign(other) = check(layout, &ours, is_alive)? {
        return Err(PidError::Foreign(other));
    }
    write(layout, pid)?;
    Ok(())
}

/// Writes `"<pid>\n"`, TS's format (`src/pid.ts:39-41`).
pub fn write(layout: &Layout, pid: u32) -> Result<(), StoreError> {
    atomic::write_atomic(
        &layout.daemon_pid(),
        format!("{pid}\n").as_bytes(),
        FileMode::Keep,
    )
}

/// Deletes `daemon.pid`; a missing file is fine.
pub fn remove(layout: &Layout) -> Result<(), StoreError> {
    let path = layout.daemon_pid();
    match fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(StoreError::io("delete", &path)(e))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        fs::create_dir_all(layout.root()).unwrap();
        (dir, layout)
    }

    #[test]
    fn readings_follow_ts() {
        let (_dir, layout) = layout();
        let alive = |pid| pid == 100 || pid == 200;
        assert_eq!(check(&layout, &[], alive).unwrap(), PidCheck::Absent);
        // Expected values from `src/pid.ts:15-36`: trimmed, 0 and NaN are stale, dead is stale.
        let table: &[(&str, PidCheck)] = &[
            ("100\n", PidCheck::Foreign(100)),
            ("  200  ", PidCheck::Ours(200)),
            ("300\n", PidCheck::Stale),
            ("0\n", PidCheck::Stale),
            ("abc\n", PidCheck::Stale),
            ("", PidCheck::Stale),
        ];
        for (text, want) in table {
            fs::write(layout.daemon_pid(), text).unwrap();
            assert_eq!(check(&layout, &[200], alive).unwrap(), *want, "{text:?}");
        }
    }

    #[test]
    fn claim_refuses_a_foreign_live_pid_and_writes_ts_format() {
        let (_dir, layout) = layout();
        fs::write(layout.daemon_pid(), "100\n").unwrap();
        let err = claim(&layout, 5, &[], |pid| pid == 100).unwrap_err();
        assert!(matches!(err, PidError::Foreign(100)), "{err:?}");
        assert_eq!(fs::read_to_string(layout.daemon_pid()).unwrap(), "100\n");

        // The recorded Watchdog's pid, or a dead one, is replaced.
        claim(&layout, 5, &[100], |pid| pid == 100).unwrap();
        assert_eq!(fs::read_to_string(layout.daemon_pid()).unwrap(), "5\n");
        claim(&layout, 6, &[], |_| false).unwrap();
        assert_eq!(fs::read_to_string(layout.daemon_pid()).unwrap(), "6\n");

        remove(&layout).unwrap();
        remove(&layout).unwrap();
        assert!(!layout.daemon_pid().exists());
    }
}
