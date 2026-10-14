//! The Run PATH: its composition, its sources and the rate-limited rebuild (#26 D1, D3, D4).

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use super::probe::{CaptureErrorKind, LoginShellProbe};

/// `run_path.source` (#26 D1). Mirrors `claudeclaw_api::settings::RunPathSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunPathSource {
    /// `login_shell` on macOS and Linux, `registry` on Windows.
    #[default]
    Auto,
    /// The user's login shell (#26 D2). Invalid on Windows.
    LoginShell,
    /// The registry's machine and user `Path` (#26 D4). Invalid off Windows.
    Registry,
    /// Only the PATH the Daemon inherited.
    Inherit,
}

impl RunPathSource {
    /// The wire name, as in `settings.json` and the Daemon log.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::LoginShell => "login_shell",
            Self::Registry => "registry",
            Self::Inherit => "inherit",
        }
    }

    /// What this source means on this OS: `auto` becomes the OS's own source, and a source
    /// that is invalid here becomes `auto`'s (#26 D1). The flag is `true` when the requested
    /// value was invalid, which the caller reports as a settings problem.
    pub fn effective(self) -> (RunPathSource, bool) {
        let native = if cfg!(windows) {
            Self::Registry
        } else {
            Self::LoginShell
        };
        match self {
            Self::Auto => (native, false),
            Self::Inherit => (Self::Inherit, false),
            Self::LoginShell | Self::Registry if self == native => (native, false),
            Self::LoginShell | Self::Registry => (native, true),
        }
    }
}

/// The `run_path` settings (#26 D1). Mirrors `claudeclaw_api::settings::RunPathSettings`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunPathSettings {
    pub source: RunPathSource,
    /// Dirs put first. Settings validation rejects relative entries; any that get here are
    /// dropped like any other relative entry.
    pub prepend: Vec<PathBuf>,
}

/// The Run PATH (glossary; #26 D1): absolute dirs, each once, in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunPath {
    dirs: Vec<PathBuf>,
}

impl RunPath {
    /// `prepend ++ source ++ inherited-not-present` (#26 D1). Empty and relative entries are
    /// dropped and counted; a duplicate keeps its first position; dirs that don't exist are
    /// kept. On Windows, entries that differ only in case or a trailing separator are
    /// duplicates.
    pub fn compose(
        prepend: &[PathBuf],
        source: Option<&OsStr>,
        inherited: Option<&OsStr>,
    ) -> ComposedRunPath {
        let mut composer = Composer::default();
        for dir in prepend {
            composer.push(dir.clone());
        }
        for list in [source, inherited].into_iter().flatten() {
            for dir in std::env::split_paths(list) {
                composer.push(dir);
            }
        }
        ComposedRunPath {
            path: RunPath {
                dirs: composer.dirs,
            },
            dropped_entries: composer.dropped,
        }
    }

    /// The dirs, in order.
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// The `PATH` value to give a child.
    pub fn to_os_string(&self) -> OsString {
        // `compose` only keeps absolute entries, and an absolute entry with the separator in
        // it can't be joined; such an entry is left out rather than failing the whole PATH.
        std::env::join_paths(&self.dirs).unwrap_or_else(|_| {
            std::env::join_paths(
                self.dirs
                    .iter()
                    .filter(|d| std::env::join_paths([d]).is_ok()),
            )
            .unwrap_or_default()
        })
    }

    /// How many dirs `self` has that `old` lacks, and the reverse; `run_path.changed`'s
    /// `added` and `removed` (#26 D7).
    pub fn changes_from(&self, old: &RunPath) -> (usize, usize) {
        let new_keys: HashSet<_> = self.dirs.iter().map(|d| dedupe_key(d)).collect();
        let old_keys: HashSet<_> = old.dirs.iter().map(|d| dedupe_key(d)).collect();
        (
            new_keys.difference(&old_keys).count(),
            old_keys.difference(&new_keys).count(),
        )
    }
}

/// A composed [`RunPath`] and how many entries were dropped (`RunPath.dropped_entries`, #26 D7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedRunPath {
    pub path: RunPath,
    pub dropped_entries: u32,
}

#[derive(Default)]
struct Composer {
    dirs: Vec<PathBuf>,
    seen: HashSet<String>,
    dropped: u32,
}

impl Composer {
    fn push(&mut self, dir: PathBuf) {
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            self.dropped += 1;
            return;
        }
        if self.seen.insert(dedupe_key(&dir)) {
            self.dirs.push(dir);
        }
    }
}

/// The key two Run PATH entries are compared by.
fn dedupe_key(dir: &Path) -> String {
    let text = dir.to_string_lossy();
    if cfg!(windows) {
        windows_dir_key(&text)
    } else {
        // `/usr/bin/` and `/usr/bin` name the same dir.
        let trimmed = text.trim_end_matches('/');
        if trimmed.is_empty() {
            "/".to_owned()
        } else {
            trimmed.to_owned()
        }
    }
}

/// Windows compares dirs case-insensitively and ignores trailing separators.
fn windows_dir_key(text: &str) -> String {
    let trimmed = text.trim_end_matches(['\\', '/']);
    // Keep the root of a drive (`C:\`) distinct from the drive-relative `C:`.
    let trimmed = if trimmed.len() == 2 && trimmed.ends_with(':') {
        text
    } else {
        trimmed
    };
    trimmed.replace('/', "\\").to_lowercase()
}

/// What a Run PATH build did, for `DaemonStatus.run_path` (`claudeclaw_api::status::RunPath`,
/// #26 D7) and the Daemon log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPathReport {
    /// `run_path.source` as set.
    pub source_requested: RunPathSource,
    /// The source used, after `auto` and any fallback.
    pub source_used: RunPathSource,
    /// The requested source isn't valid on this OS, so `auto` was used: a settings problem.
    pub source_invalid: bool,
    /// The login shell's basename; `None` when no shell was run.
    pub shell: Option<String>,
    pub captured_at: DateTime<Utc>,
    pub duration_ms: u64,
    /// Why the capture failed; the Worker then uses `prepend ++ inherited` (#26 D2).
    pub error: Option<CaptureErrorKind>,
    /// No usable login shell (`nologin`, `false`, not executable), so the source fell back to
    /// `inherit`. Info (`no_login_shell`), not a problem (#26 D2).
    pub no_login_shell: bool,
    /// Empty or relative entries left out.
    pub dropped_entries: u32,
}

/// The result of [`build_run_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPathBuild {
    pub path: RunPath,
    pub report: RunPathReport,
}

/// Builds the Run PATH from the settings (#26 D1), capturing the source PATH as
/// [`RunPathSettings::source`] says. `inherited` is the PATH the Daemon inherited and `exe` the
/// Daemon's own binary, which the login shell runs as `__env-probe` (#26 D2).
///
/// Logs `run_path.captured` or `run_path.capture_failed`, and `run_path.changed` against
/// `previous` when one is given (#26 D7). A failed capture still yields a Run PATH:
/// `prepend ++ inherited`.
pub async fn build_run_path(
    settings: &RunPathSettings,
    inherited: Option<&OsStr>,
    exe: &Path,
    previous: Option<&RunPath>,
) -> RunPathBuild {
    let started = Instant::now();
    let captured_at = DateTime::<Utc>::from(std::time::SystemTime::now());
    let (mut source_used, source_invalid) = settings.source.effective();
    let mut shell = None;
    let mut error = None;
    let mut no_login_shell = false;
    let source_path: Option<OsString> = match source_used {
        RunPathSource::LoginShell => match LoginShellProbe::for_current_user(exe) {
            Some(probe) => {
                shell = Some(probe.shell_name());
                match probe.capture().await {
                    Ok(path) => Some(path),
                    Err(e) => {
                        error = Some(e.kind());
                        None
                    }
                }
            }
            None => {
                no_login_shell = true;
                source_used = RunPathSource::Inherit;
                None
            }
        },
        RunPathSource::Registry => match super::registry::registry_path() {
            Ok(path) => Some(path),
            Err(e) => {
                tracing::debug!(errno = e.raw_os_error(), "can't read the registry PATH");
                error = Some(CaptureErrorKind::SpawnFailed);
                None
            }
        },
        RunPathSource::Inherit | RunPathSource::Auto => None,
    };
    let composed = RunPath::compose(&settings.prepend, source_path.as_deref(), inherited);
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match error {
        Some(kind) => tracing::warn!(
            event = "run_path.capture_failed",
            kind = kind.as_str(),
            "the Run PATH capture failed; runs get the prepended and inherited PATH"
        ),
        None => tracing::info!(
            event = "run_path.captured",
            source = source_used.as_str(),
            shell = shell.as_deref(),
            dirs = composed.path.dirs().len(),
            duration_ms,
            "Run PATH built"
        ),
    }
    if let Some(previous) = previous {
        log_change(previous, &composed.path);
    }
    RunPathBuild {
        report: RunPathReport {
            source_requested: settings.source,
            source_used,
            source_invalid,
            shell,
            captured_at,
            duration_ms,
            error,
            no_login_shell,
            dropped_entries: composed.dropped_entries,
        },
        path: composed.path,
    }
}

/// Logs `run_path.changed {added, removed}` when `new` differs from `old` (#26 D7). The dirs
/// themselves appear only in status.
pub fn log_change(old: &RunPath, new: &RunPath) {
    let (added, removed) = new.changes_from(old);
    if added > 0 || removed > 0 {
        tracing::info!(
            event = "run_path.changed",
            added,
            removed,
            "Run PATH changed"
        );
    }
}

/// The rate limit on rebuilding the Run PATH after a resolution miss: at most once per
/// [`RebuildGate::MIN_INTERVAL`] (#26 D3 step 4).
///
/// On a miss ([`crate::launcher::LauncherError::is_miss`]) the caller asks
/// [`RebuildGate::try_acquire`]; if it allows a rebuild, the caller rebuilds the Run PATH and
/// resolves once more before refusing. Rebuilds for other reasons (Worker start, a settings
/// change, Test launcher) don't go through the gate.
#[derive(Debug, Clone, Default)]
pub struct RebuildGate {
    last: Option<Instant>,
}

impl RebuildGate {
    /// The minimum time between two miss-triggered rebuilds (#26 D3).
    pub const MIN_INTERVAL: Duration = Duration::from_secs(60);

    pub fn new() -> Self {
        Self::default()
    }

    /// `true`, and records `now`, when no rebuild was allowed in the last
    /// [`Self::MIN_INTERVAL`].
    pub fn try_acquire(&mut self, now: Instant) -> bool {
        match self.last {
            Some(last) if now.saturating_duration_since(last) < Self::MIN_INTERVAL => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[cfg(unix)]
    #[test]
    fn composition_table() {
        struct Case {
            name: &'static str,
            prepend: &'static [&'static str],
            source: Option<&'static str>,
            inherited: Option<&'static str>,
            dirs: &'static [&'static str],
            dropped: u32,
        }
        let cases = [
            Case {
                name: "order is prepend, source, inherited",
                prepend: &["/p"],
                source: Some("/s1:/s2"),
                inherited: Some("/i"),
                dirs: &["/p", "/s1", "/s2", "/i"],
                dropped: 0,
            },
            Case {
                name: "inherited entries already present are skipped",
                prepend: &[],
                source: Some("/usr/bin:/opt/x/bin"),
                inherited: Some("/usr/bin:/bin:/opt/x/bin"),
                dirs: &["/usr/bin", "/opt/x/bin", "/bin"],
                dropped: 0,
            },
            Case {
                name: "a duplicate keeps its first position",
                prepend: &["/b"],
                source: Some("/a:/b:/a"),
                inherited: None,
                dirs: &["/b", "/a"],
                dropped: 0,
            },
            Case {
                name: "a trailing slash is the same dir",
                prepend: &[],
                source: Some("/a/:/a"),
                inherited: None,
                dirs: &["/a/"],
                dropped: 0,
            },
            Case {
                name: "empty and relative entries are dropped and counted",
                prepend: &["rel"],
                source: Some(":/a::.:bin:/b"),
                inherited: Some("./x"),
                dirs: &["/a", "/b"],
                dropped: 6,
            },
            Case {
                name: "missing dirs are kept",
                prepend: &["/does/not/exist"],
                source: None,
                inherited: None,
                dirs: &["/does/not/exist"],
                dropped: 0,
            },
            Case {
                name: "nothing at all",
                prepend: &[],
                source: None,
                inherited: None,
                dirs: &[],
                dropped: 0,
            },
        ];
        for case in cases {
            let prepend: Vec<PathBuf> = case.prepend.iter().map(PathBuf::from).collect();
            let source = case.source.map(os);
            let inherited = case.inherited.map(os);
            let got = RunPath::compose(&prepend, source.as_deref(), inherited.as_deref());
            let want: Vec<PathBuf> = case.dirs.iter().map(PathBuf::from).collect();
            assert_eq!(got.path.dirs(), want.as_slice(), "{}", case.name);
            assert_eq!(got.dropped_entries, case.dropped, "{}", case.name);
        }
    }

    #[cfg(unix)]
    #[test]
    fn joins_back_to_a_path_value() {
        let got = RunPath::compose(&[PathBuf::from("/p")], Some(&os("/a:/b")), None);
        assert_eq!(got.path.to_os_string(), os("/p:/a:/b"));
        assert_eq!(RunPath::default().to_os_string(), os(""));
    }

    #[cfg(windows)]
    #[test]
    fn windows_composition() {
        let got = RunPath::compose(
            &[],
            Some(&os(r"C:\Windows\system32;C:\Tools\;relative;;C:")),
            Some(&os(r"C:\WINDOWS\System32;c:\tools;D:\x")),
        );
        let want: Vec<PathBuf> = [r"C:\Windows\system32", r"C:\Tools\", r"D:\x"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(got.path.dirs(), want.as_slice());
        assert_eq!(got.dropped_entries, 3);
    }

    #[test]
    fn windows_dir_keys() {
        assert_eq!(
            windows_dir_key(r"C:\Windows\System32\"),
            windows_dir_key(r"c:\WINDOWS\system32")
        );
        assert_eq!(windows_dir_key("C:/Tools"), windows_dir_key(r"C:\tools"));
        assert_ne!(windows_dir_key(r"C:\"), windows_dir_key("C:"));
    }

    #[test]
    fn changes_count_added_and_removed() {
        let old = RunPath {
            dirs: vec!["/a".into(), "/b".into()],
        };
        let new = RunPath {
            dirs: vec!["/b".into(), "/c".into(), "/d".into()],
        };
        assert_eq!(new.changes_from(&old), (2, 1));
        assert_eq!(old.changes_from(&old), (0, 0));
    }

    #[test]
    fn source_resolution() {
        let native = if cfg!(windows) {
            RunPathSource::Registry
        } else {
            RunPathSource::LoginShell
        };
        let foreign = if cfg!(windows) {
            RunPathSource::LoginShell
        } else {
            RunPathSource::Registry
        };
        assert_eq!(RunPathSource::Auto.effective(), (native, false));
        assert_eq!(native.effective(), (native, false));
        assert_eq!(foreign.effective(), (native, true));
        assert_eq!(
            RunPathSource::Inherit.effective(),
            (RunPathSource::Inherit, false)
        );
    }

    #[test]
    fn rebuild_gate_allows_one_rebuild_per_minute() {
        let mut gate = RebuildGate::new();
        let t0 = Instant::now();
        assert!(gate.try_acquire(t0));
        assert!(!gate.try_acquire(t0 + Duration::from_secs(1)));
        assert!(!gate.try_acquire(t0 + Duration::from_secs(59)));
        assert!(gate.try_acquire(t0 + Duration::from_secs(60)));
        assert!(!gate.try_acquire(t0 + Duration::from_secs(61)));
    }

    #[tokio::test]
    async fn inherit_source_builds_without_a_shell() {
        let settings = RunPathSettings {
            source: RunPathSource::Inherit,
            prepend: vec![std::env::temp_dir()],
        };
        let inherited = std::env::temp_dir().into_os_string();
        let built = build_run_path(&settings, Some(&inherited), Path::new("unused"), None).await;
        assert_eq!(built.path.dirs(), &[std::env::temp_dir()]);
        assert_eq!(built.report.source_used, RunPathSource::Inherit);
        assert_eq!(built.report.shell, None);
        assert_eq!(built.report.error, None);
    }
}
