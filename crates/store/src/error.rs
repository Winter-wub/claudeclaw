//! The store's error type (architecture §2: `thiserror` in libraries).

use std::io;
use std::path::{Path, PathBuf};

/// A failed store operation. Messages name the file, never its contents, so an error can be
/// logged without leaking a secret or a prompt (architecture §2, #23).
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// An OS call failed on `path`.
    #[error("{op} {}: {source}", path.display())]
    Io {
        /// What was being done, e.g. `"read"` or `"rename"`.
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A JSON file the store reads is not valid JSON. The serde error carries only a line and
    /// column, never the text.
    #[error("{} is not valid JSON: {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// A settings file whose top level is not a JSON object, so it can't be patched.
    #[error("{} is not a JSON object", path.display())]
    NotAnObject { path: PathBuf },
    /// A path the store refuses to use, with the reason (e.g. a runtime dir owned by another
    /// user, §9.3).
    #[error("{}: {reason}", path.display())]
    Refused { path: PathBuf, reason: &'static str },
}

impl StoreError {
    pub(crate) fn io(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> StoreError {
        let path = path.to_path_buf();
        move |source| StoreError::Io { op, path, source }
    }

    /// Whether this is an OS "not found" error.
    pub fn is_not_found(&self) -> bool {
        matches!(self, StoreError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }
}
