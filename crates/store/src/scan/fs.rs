//! The Change scan's filesystem seam and its per-OS stat key (#46 D1, D9).
//!
//! [`FsView`] is the three calls the scan makes (stat, list a dir, read a file), so tests can
//! drive passes over a fake tree without sleeping. [`StdFs`] is the real one over `std::fs`.

use std::ffi::OsString;
use std::fs::{self, Metadata};
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// What the scan needs from the filesystem (#46 D9). Implementations follow symlinks, like
/// `std::fs::metadata`, and report a missing path as [`io::ErrorKind::NotFound`] and a listed
/// path that is a file as [`io::ErrorKind::NotADirectory`].
pub trait FsView {
    /// Stats `path`, following symlinks.
    fn stat(&self, path: &Path) -> io::Result<FileStat>;
    /// The entry names in `dir`, without `.` and `..`, in any order.
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<OsString>>;
    /// The whole file.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
}

/// The real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdFs;

impl FsView for StdFs {
    fn stat(&self, path: &Path) -> io::Result<FileStat> {
        fs::metadata(path).map(|meta| FileStat::from_metadata(&meta))
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        fs::read_dir(dir)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }
}

/// What a stat says about a path (#46 D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    /// A regular file, a dir, or something else.
    pub kind: FileKind,
    /// The key the stat pass compares.
    pub key: StatKey,
    /// The modification time, for when a job's schedule took effect (#50). The Unix epoch if the
    /// OS gives none.
    pub modified: SystemTime,
}

impl FileStat {
    /// The stat of `meta`, which should come from a call that follows symlinks.
    pub fn from_metadata(meta: &Metadata) -> Self {
        let kind = if meta.is_file() {
            FileKind::File
        } else if meta.is_dir() {
            FileKind::Dir
        } else {
            FileKind::Other
        };
        FileStat {
            kind,
            key: StatKey::from_metadata(meta),
            modified: meta.modified().unwrap_or(UNIX_EPOCH),
        }
    }
}

/// The kind of a stat'd path. Only [`FileKind::File`] can be a job file (#46 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Other,
}

/// The stat key: a file whose key is unchanged is not read by the stat pass (#46 D1). Fields an
/// OS doesn't have are 0.
///
/// - **Unix**: `(dev, ino, size, mtime, ctime)`, times in ns. ctime is in the key because a tool
///   can restore mtime (`touch -r`) but not ctime (#46 Evidence 3).
/// - **Windows**: `(size, last_write_time, creation_time, attributes)` from
///   `std::os::windows::fs::MetadataExt`, times in 100 ns ticks. NTFS's ChangeTime, the real
///   ctime, and the file index are not in stable `std`, so the creation time and the attribute
///   bits stand in for them. The 30 s hash pass bounds whatever this key misses (#46 Review
///   point 7).
/// - **Other OSes**: `(size, mtime)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatKey {
    /// Unix `st_dev`.
    pub dev: u64,
    /// Unix `st_ino`.
    pub ino: u64,
    /// The file size in bytes.
    pub size: u64,
    /// Unix mtime in ns; Windows `last_write_time`.
    pub modified: i128,
    /// Unix ctime in ns; Windows `creation_time`.
    pub changed: i128,
    /// Windows `file_attributes`.
    pub attributes: u32,
}

impl StatKey {
    /// The key of `meta` on this OS.
    pub fn from_metadata(meta: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let ns = |secs: i64, nsec: i64| i128::from(secs) * 1_000_000_000 + i128::from(nsec);
            StatKey {
                dev: meta.dev(),
                ino: meta.ino(),
                size: meta.size(),
                modified: ns(meta.mtime(), meta.mtime_nsec()),
                changed: ns(meta.ctime(), meta.ctime_nsec()),
                attributes: 0,
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            StatKey {
                dev: 0,
                ino: 0,
                size: meta.file_size(),
                modified: i128::from(meta.last_write_time()),
                changed: i128::from(meta.creation_time()),
                attributes: meta.file_attributes(),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as i128);
            StatKey {
                dev: 0,
                ino: 0,
                size: meta.len(),
                modified,
                changed: 0,
                attributes: 0,
            }
        }
    }
}
