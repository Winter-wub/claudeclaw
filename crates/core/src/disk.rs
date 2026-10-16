//! The free-space guard (architecture §4 `core::disk`; #44 D6).
//!
//! [`DiskGuard`] reads free space on the project dir's volume every [`CHECK_INTERVAL`] and before
//! each attachment write. Below `disk.warnFreeMiB` it logs `disk.low{free_mib}` (WARN) once per
//! episode and calls the owner-notice hook; it clears at the warn level plus
//! [`CLEAR_HYSTERESIS_MIB`] with `disk.ok` (INFO). Below `disk.minFreeMiB` attachment intake is
//! also refused ([`DiskState::allows_intake`]).
//!
//! The guard **never deletes anything** and **never refuses a run**: the caller deletes through
//! [`crate::housekeeping`], and only a large attachment write consults [`DiskGuard::allows_intake`].
//! It does not depend on `core::notify`; the low episode is surfaced as an event/log and an
//! optional callback the composition root wires to the notifier.
//!
//! The free-space probe is a [`FreeSpaceProbe`]: `statvfs` on Unix, `GetDiskFreeSpaceExW` on
//! Windows, and a fake one in tests. A probe error leaves the last known state in place, so a
//! disk that was known low is never waved through.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use claudeclaw_api::log_event::daemon as event;
use claudeclaw_api::settings::DiskSettings;
use tokio::time::MissedTickBehavior;

/// One MiB, for the free-space floors.
pub const MIB: u64 = 1024 * 1024;
/// How often [`DiskGuard::run`] checks free space (#44 D6).
pub const CHECK_INTERVAL: Duration = Duration::from_secs(600);
/// How far above `disk.warnFreeMiB` free space must rise to clear the low episode (#44 D6).
pub const CLEAR_HYSTERESIS_MIB: u64 = 512;

/// Where the guard reads free space. The composition root uses [`SystemProbe`]; tests inject a
/// fake, so the thresholds are testable without touching a real volume (#44 D6).
pub trait FreeSpaceProbe: Send + Sync + 'static {
    /// Free bytes on the volume holding `dir`.
    fn free_bytes(&self, dir: &Path) -> io::Result<u64>;
}

/// The OS free-space probe: `statvfs` on Unix, `GetDiskFreeSpaceExW` on Windows (#44 D6).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbe;

impl FreeSpaceProbe for SystemProbe {
    fn free_bytes(&self, dir: &Path) -> io::Result<u64> {
        imp::free_bytes(dir)
    }
}

/// Called with the free MiB once when free space falls below the warn floor (#44 D6). The
/// composition root wires this to `core::notify`; `core::disk` stays free of it.
pub type LowNotice = Box<dyn Fn(u64) + Send + Sync>;

/// The guard's view of the volume (#44 D6). `Ok` also means "not yet read", so an unreadable
/// disk is never mistaken for a full one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskState {
    /// At or above the warn floor, or not yet known.
    Ok,
    /// Below `disk.warnFreeMiB`: attachments are still accepted.
    Low {
        /// Free MiB, as measured.
        free_mib: u64,
    },
    /// Below `disk.minFreeMiB`: attachment intake is refused.
    BelowMin {
        /// Free MiB, as measured.
        free_mib: u64,
    },
}

impl DiskState {
    /// The free MiB this state was measured at, or `None` for [`DiskState::Ok`].
    pub fn free_mib(self) -> Option<u64> {
        match self {
            DiskState::Ok => None,
            DiskState::Low { free_mib } | DiskState::BelowMin { free_mib } => Some(free_mib),
        }
    }

    /// Whether free space is below the warn floor.
    pub fn is_low(self) -> bool {
        matches!(self, DiskState::Low { .. } | DiskState::BelowMin { .. })
    }

    /// Whether an attachment write is allowed. Only [`DiskState::BelowMin`] refuses one; runs are
    /// never refused, which is the caller's job to honour.
    pub fn allows_intake(self) -> bool {
        !matches!(self, DiskState::BelowMin { .. })
    }
}

/// The free-space guard for one project dir (#44 D6).
pub struct DiskGuard {
    dir: PathBuf,
    warn_free_mib: u64,
    min_free_mib: u64,
    probe: Arc<dyn FreeSpaceProbe>,
    /// The last successful reading, in bytes; `None` before the first probe.
    last_bytes: Mutex<Option<u64>>,
    /// Whether a low episode is open, so the notice fires once per episode.
    low: Mutex<bool>,
    notice: Mutex<Option<LowNotice>>,
}

impl DiskGuard {
    /// A guard for `dir`'s volume with the `disk` floors, reading through `probe`.
    pub fn new(dir: PathBuf, settings: DiskSettings, probe: Arc<dyn FreeSpaceProbe>) -> Self {
        DiskGuard {
            dir,
            warn_free_mib: u64::from(settings.warn_free_mib),
            min_free_mib: u64::from(settings.min_free_mib),
            probe,
            last_bytes: Mutex::new(None),
            low: Mutex::new(false),
            notice: Mutex::new(None),
        }
    }

    /// Sets the hook called once per low episode with the free MiB. Replaces any earlier one.
    pub fn set_low_notice(&self, notice: LowNotice) {
        *self.notice.lock().unwrap_or_else(|e| e.into_inner()) = Some(notice);
    }

    /// Probes the volume and updates the state, logging `disk.low`/`disk.ok` at the episode
    /// boundaries. A probe error leaves the last known state untouched.
    pub fn check(&self) {
        let free_bytes = match self.probe.free_bytes(&self.dir) {
            Ok(bytes) => bytes,
            Err(_) => return,
        };
        *self.last_bytes.lock().unwrap_or_else(|e| e.into_inner()) = Some(free_bytes);
        let free_mib = free_bytes / MIB;

        let was_low = *self.low.lock().unwrap_or_else(|e| e.into_inner());
        let now_low = low_after(free_mib, was_low, self.warn_free_mib);
        if now_low != was_low {
            *self.low.lock().unwrap_or_else(|e| e.into_inner()) = now_low;
        }
        if now_low && !was_low {
            tracing::warn!(
                event = event::DISK_LOW,
                free_mib,
                "free space is below the warn floor"
            );
            let notice = self.notice.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(notice) = notice.as_ref() {
                notice(free_mib);
            }
        } else if !now_low && was_low {
            tracing::info!(
                event = event::DISK_OK,
                "free space is back above the warn floor"
            );
        }
    }

    /// The state after a fresh probe, for the attachment-write path (#44 D6).
    pub fn check_before_write(&self) -> DiskState {
        self.check();
        self.state()
    }

    /// The last known state, without probing.
    pub fn state(&self) -> DiskState {
        let Some(bytes) = *self.last_bytes.lock().unwrap_or_else(|e| e.into_inner()) else {
            return DiskState::Ok;
        };
        let free_mib = bytes / MIB;
        if free_mib < self.min_free_mib {
            DiskState::BelowMin { free_mib }
        } else if *self.low.lock().unwrap_or_else(|e| e.into_inner()) {
            DiskState::Low { free_mib }
        } else {
            DiskState::Ok
        }
    }

    /// The last measured free MiB, or `None` before the first successful probe.
    pub fn free_mib(&self) -> Option<u64> {
        self.last_bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|bytes| bytes / MIB)
    }

    /// Whether an attachment write is allowed right now (never refuses a run).
    pub fn allows_intake(&self) -> bool {
        self.state().allows_intake()
    }

    /// Checks the volume every [`CHECK_INTERVAL`] until dropped, logging and noticing on each
    /// episode boundary.
    pub async fn run(&self) {
        let mut interval = tokio::time::interval(CHECK_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.check();
        }
    }
}

impl fmt::Debug for DiskGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiskGuard")
            .field("warn_free_mib", &self.warn_free_mib)
            .field("min_free_mib", &self.min_free_mib)
            .field("free_mib", &self.free_mib())
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

/// Whether a reading opens or keeps a low episode (#44 D6). Entering low happens at the warn
/// floor; leaving it waits for the warn floor plus [`CLEAR_HYSTERESIS_MIB`], so a disk hovering at
/// the line doesn't flap.
fn low_after(free_mib: u64, was_low: bool, warn_free_mib: u64) -> bool {
    let clear_at = warn_free_mib.saturating_add(CLEAR_HYSTERESIS_MIB);
    if was_low {
        free_mib < clear_at
    } else {
        free_mib < warn_free_mib
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::Path;

    /// `statvfs`, without `unsafe` (§10, #44 D6).
    pub(super) fn free_bytes(dir: &Path) -> io::Result<u64> {
        let stat = nix::sys::statvfs::statvfs(dir).map_err(io::Error::from)?;
        // The block count and fragment size are both platform-dependent, so widen to `u128` and
        // clamp rather than convert each one (which is a no-op on some targets and a widening on
        // others).
        let bytes =
            u128::from(stat.blocks_available()).saturating_mul(u128::from(stat.fragment_size()));
        Ok(u64::try_from(bytes).unwrap_or(u64::MAX))
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    /// `GetDiskFreeSpaceExW` (§10, #44 D6). The one `unsafe` block in this module.
    pub(super) fn free_bytes(dir: &Path) -> io::Result<u64> {
        let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut free: u64 = 0;
        // SAFETY: `wide` is a live, NUL-terminated UTF-16 path, and `free` is a valid writable
        // `u64`. The other two out-params are null, which the API accepts.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(free)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::io;
    use std::path::Path;

    pub(super) fn free_bytes(_dir: &Path) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "free space is not readable on this OS",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WARN: u32 = 2048;
    const MIN: u32 = 256;

    fn mib(n: u64) -> u64 {
        n * MIB
    }

    #[derive(Debug)]
    struct FakeProbe {
        bytes: Mutex<Result<u64, ()>>,
        calls: Mutex<u32>,
    }

    impl FakeProbe {
        fn new(free_mib: u64) -> Arc<Self> {
            Arc::new(FakeProbe {
                bytes: Mutex::new(Ok(mib(free_mib))),
                calls: Mutex::new(0),
            })
        }

        fn set(&self, free_mib: u64) {
            *self.bytes.lock().unwrap() = Ok(mib(free_mib));
        }

        fn fail(&self) {
            *self.bytes.lock().unwrap() = Err(());
        }

        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    impl FreeSpaceProbe for FakeProbe {
        fn free_bytes(&self, _dir: &Path) -> io::Result<u64> {
            *self.calls.lock().unwrap() += 1;
            self.bytes
                .lock()
                .unwrap()
                .map_err(|()| io::Error::other("probe failed"))
        }
    }

    fn make_guard(probe: Arc<FakeProbe>) -> DiskGuard {
        DiskGuard::new(
            PathBuf::from("/tmp"),
            DiskSettings {
                warn_free_mib: WARN,
                min_free_mib: MIN,
            },
            probe,
        )
    }

    #[test]
    fn a_low_episode_notices_once_and_recovers() {
        let probe = FakeProbe::new(4096);
        let guard = make_guard(probe.clone());
        let notices = Arc::new(Mutex::new(Vec::new()));
        let recorded = notices.clone();
        guard.set_low_notice(Box::new(move |free_mib| {
            recorded.lock().unwrap().push(free_mib);
        }));

        assert!(
            guard.free_mib().is_none(),
            "no reading before the first probe"
        );
        guard.check();
        assert_eq!(guard.state(), DiskState::Ok);
        assert_eq!(guard.free_mib(), Some(4096));
        assert!(notices.lock().unwrap().is_empty());

        probe.set(1000);
        guard.check();
        assert_eq!(guard.state(), DiskState::Low { free_mib: 1000 });
        assert!(guard.allows_intake());
        assert_eq!(*notices.lock().unwrap(), [1000]);

        // Still low: no second notice this episode.
        guard.check();
        assert_eq!(*notices.lock().unwrap(), [1000]);

        // Recover, then a new episode notices again.
        probe.set(4096);
        guard.check();
        assert_eq!(guard.state(), DiskState::Ok);
        assert_eq!(*notices.lock().unwrap(), [1000]);
        probe.set(900);
        guard.check();
        assert_eq!(*notices.lock().unwrap(), [1000, 900]);
    }

    #[test]
    fn below_the_min_floor_refuses_intake() {
        let probe = FakeProbe::new(100);
        let guard = make_guard(probe);
        guard.check();
        assert_eq!(guard.state(), DiskState::BelowMin { free_mib: 100 });
        assert!(!guard.allows_intake());
    }

    #[test]
    fn the_low_episode_clears_only_at_the_warn_floor_plus_hysteresis() {
        let probe = FakeProbe::new(2000);
        let guard = make_guard(probe.clone());
        guard.check();
        assert_eq!(guard.state(), DiskState::Low { free_mib: 2000 });

        probe.set(2559);
        guard.check();
        assert_eq!(guard.state(), DiskState::Low { free_mib: 2559 });

        probe.set(2560);
        guard.check();
        assert_eq!(guard.state(), DiskState::Ok);
    }

    #[test]
    fn a_probe_error_keeps_the_last_state() {
        let probe = FakeProbe::new(1000);
        let guard = make_guard(probe.clone());
        guard.check();
        assert_eq!(guard.state(), DiskState::Low { free_mib: 1000 });

        probe.fail();
        guard.check();
        assert_eq!(
            guard.state(),
            DiskState::Low { free_mib: 1000 },
            "a known-low disk is never waved through"
        );
        assert_eq!(guard.free_mib(), Some(1000));

        // A guard whose only probe failed has no reading: it fails open.
        let failing = FakeProbe::new(0);
        failing.fail();
        let fresh = make_guard(failing);
        fresh.check();
        assert_eq!(fresh.state(), DiskState::Ok);
        assert!(fresh.allows_intake());
        assert_eq!(fresh.free_mib(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn run_checks_on_its_interval() {
        let probe = FakeProbe::new(4096);
        let guard = Arc::new(make_guard(probe.clone()));
        let looped = guard.clone();
        tokio::spawn(async move { looped.run().await });

        tokio::task::yield_now().await;
        assert!(probe.calls() >= 1, "the first tick is immediate");

        tokio::time::advance(CHECK_INTERVAL).await;
        tokio::task::yield_now().await;
        assert!(probe.calls() >= 2);
        assert_eq!(guard.free_mib(), Some(4096));
    }
}
