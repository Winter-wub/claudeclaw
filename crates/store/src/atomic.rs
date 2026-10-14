//! Atomic writes (architecture §4 `claudeclaw-store`, §9.1 "Write discipline").
//!
//! Every write goes to a `tempfile::NamedTempFile` in the target's own dir, is synced, gets its
//! final permission mode, and is then renamed over the target with `persist`; the dir is synced
//! last so the rename survives a crash. A reader therefore sees the old file or the new one,
//! never a torn one, and never a file with wider permissions than its mode.
//!
//! On Windows the modes are no-ops: files inherit the user-profile ACL (§9.1).

use std::fs;
use std::io::Write;
use std::path::Path;

use tempfile::NamedTempFile;

use crate::error::StoreError;

/// The permission mode a file gets before it is renamed into place (§9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    /// `0600`: `settings.json`, every file under `daemon/secrets/`, System-prompt files.
    Private,
    /// Keep the existing file's mode; a new file gets `0644`, like a TS write under the usual
    /// umask.
    Keep,
}

/// The permission mode of a dir the store creates (§9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirMode {
    /// `0700`: `daemon/`, `daemon/secrets/`, `import-backup/`.
    Private,
    /// Whatever `create_dir_all` gives under the process umask.
    Default,
}

/// Writes `bytes` to `path` atomically, with `mode` applied before the rename. The parent dir
/// must exist.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: FileMode) -> Result<(), StoreError> {
    let dir = parent(path)?;
    let mut tmp = NamedTempFile::new_in(dir).map_err(StoreError::io("create temp file in", dir))?;
    tmp.write_all(bytes)
        .map_err(StoreError::io("write", tmp.path()))?;
    apply_file_mode(tmp.as_file(), path, mode)?;
    tmp.as_file()
        .sync_all()
        .map_err(StoreError::io("fsync", tmp.path()))?;
    tmp.persist(path).map_err(|e| StoreError::Io {
        op: "rename over",
        path: path.to_path_buf(),
        source: e.error,
    })?;
    sync_dir(dir)
}

/// Creates `dir` and its missing parents, then applies `mode` to `dir` itself (also when it
/// already existed, so a widened `daemon/` is narrowed again).
pub fn create_dir(dir: &Path, mode: DirMode) -> Result<(), StoreError> {
    fs::create_dir_all(dir).map_err(StoreError::io("create dir", dir))?;
    if mode == DirMode::Private {
        set_mode(dir, 0o700)?;
    }
    Ok(())
}

/// Sets `path`'s Unix permission bits. A no-op on Windows (§9.1).
pub fn set_mode(path: &Path, mode: u32) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(StoreError::io("chmod", path))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

fn parent(path: &Path) -> Result<&Path, StoreError> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => Ok(dir),
        _ => Ok(Path::new(".")),
    }
}

fn apply_file_mode(file: &fs::File, target: &Path, mode: FileMode) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bits = match mode {
            FileMode::Private => 0o600,
            FileMode::Keep => match fs::metadata(target) {
                Ok(meta) => meta.permissions().mode() & 0o7777,
                Err(_) => 0o644,
            },
        };
        file.set_permissions(fs::Permissions::from_mode(bits))
            .map_err(StoreError::io("chmod", target))?;
    }
    #[cfg(not(unix))]
    let _ = (file, target, mode);
    Ok(())
}

/// Syncs a dir so a rename in it is durable. Windows can't open a dir as a file; there the
/// rename is already durable once `MoveFileEx` returns.
fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(StoreError::io("fsync", dir))?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_atomic(&path, b"one", FileMode::Keep).unwrap();
        write_atomic(&path, b"two", FileMode::Keep).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["state.json"]);
    }

    #[test]
    fn a_missing_parent_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = write_atomic(&dir.path().join("no/such.json"), b"x", FileMode::Keep);
        assert!(err.unwrap_err().is_not_found());
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::PermissionsExt;

        use super::*;

        fn mode(path: &Path) -> u32 {
            fs::metadata(path).unwrap().permissions().mode() & 0o7777
        }

        #[test]
        fn private_files_are_0600_even_over_a_wider_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("settings.json");
            fs::write(&path, "{}").unwrap();
            set_mode(&path, 0o644).unwrap();
            write_atomic(&path, b"{}\n", FileMode::Private).unwrap();
            assert_eq!(mode(&path), 0o600);
        }

        #[test]
        fn keep_preserves_the_mode_and_new_files_get_0644() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("job.md");
            write_atomic(&path, b"a", FileMode::Keep).unwrap();
            assert_eq!(mode(&path), 0o644);
            set_mode(&path, 0o640).unwrap();
            write_atomic(&path, b"b", FileMode::Keep).unwrap();
            assert_eq!(mode(&path), 0o640);
        }

        #[test]
        fn private_dirs_are_0700_and_narrowed_again() {
            let dir = tempfile::tempdir().unwrap();
            let daemon = dir.path().join("a/daemon");
            create_dir(&daemon, DirMode::Private).unwrap();
            assert_eq!(mode(&daemon), 0o700);
            set_mode(&daemon, 0o755).unwrap();
            create_dir(&daemon, DirMode::Private).unwrap();
            assert_eq!(mode(&daemon), 0o700);
        }
    }
}
