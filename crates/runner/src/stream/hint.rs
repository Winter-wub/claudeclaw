//! Where a run's transcript lives, from `init.memory_paths.auto` (#39 D1).

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// The Claude Code project dir a run wrote its transcript into: `<config dir>/projects/<project
/// dir>/` (#39 D1, E1). The transcript is `<dir>/<session_id>.jsonl`.
///
/// It is read from `init.memory_paths.auto` and counts only when that path is absolute and has
/// the shape `…/projects/<one component>/memory/`, so the Daemon never recomputes Claude Code's
/// cwd slug. With auto memory off or relocated (#39 E2) there is no hint and `core` searches.
#[derive(Clone, PartialEq, Eq)]
pub struct TranscriptHint {
    dir: PathBuf,
}

impl fmt::Debug for TranscriptHint {
    // Paths carry the user's home; keep them out of logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TranscriptHint { .. }")
    }
}

/// Why `memory_paths.auto` gave no hint (#39 D1). Carries no path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TranscriptHintError {
    #[error("memory path is not absolute")]
    NotAbsolute,
    #[error("memory path is not shaped …/projects/<project>/memory/")]
    BadShape,
}

impl TranscriptHint {
    /// Applies #39 D1's shape check to `init.memory_paths.auto`. A trailing separator is
    /// optional; `.` and `..` components are refused.
    pub fn from_memory_path(auto: &str) -> Result<Self, TranscriptHintError> {
        let path = Path::new(auto);
        if !path.is_absolute() {
            return Err(TranscriptHintError::NotAbsolute);
        }
        let components: Vec<Component<'_>> = path.components().collect();
        if components
            .iter()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
        {
            return Err(TranscriptHintError::BadShape);
        }
        match components.as_slice() {
            [
                ..,
                Component::Normal(projects),
                Component::Normal(_),
                Component::Normal(memory),
            ] if *projects == "projects" && *memory == "memory" => {
                let dir = path.parent().ok_or(TranscriptHintError::BadShape)?;
                Ok(Self {
                    dir: dir.to_path_buf(),
                })
            }
            _ => Err(TranscriptHintError::BadShape),
        }
    }

    /// `<config dir>/projects/<project dir>/`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<dir>/<session_id>.jsonl`, or `None` unless `session_id` is a UUID: it must parse as
    /// one before it is used in any path (#39 D1).
    pub fn transcript_path(&self, session_id: &str) -> Option<PathBuf> {
        is_uuid(session_id).then(|| self.dir.join(format!("{session_id}.jsonl")))
    }
}

/// `8-4-4-4-12` hex digits, either case.
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        std::env::temp_dir().join("cfg")
    }

    #[test]
    fn shape_check() {
        let r = root();
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        let sep = std::path::MAIN_SEPARATOR;
        let good = r.join("projects").join("-tmp-proj");
        let cases: Vec<(String, Result<PathBuf, TranscriptHintError>)> = vec![
            (format!("{}{sep}", s(good.join("memory"))), Ok(good.clone())),
            (s(good.join("memory")), Ok(good.clone())),
            (
                "projects/-tmp-proj/memory/".into(),
                Err(TranscriptHintError::NotAbsolute),
            ),
            (String::new(), Err(TranscriptHintError::NotAbsolute)),
            // autoMemoryDirectory moved it elsewhere (#39 E2).
            (
                s(r.join("custom-memory")),
                Err(TranscriptHintError::BadShape),
            ),
            // Two components between projects and memory.
            (
                s(r.join("projects").join("a").join("b").join("memory")),
                Err(TranscriptHintError::BadShape),
            ),
            (
                s(r.join("projects").join("..").join("memory")),
                Err(TranscriptHintError::BadShape),
            ),
            (
                s(r.join("project").join("x").join("memory")),
                Err(TranscriptHintError::BadShape),
            ),
            (s(good.join("Memory")), Err(TranscriptHintError::BadShape)),
            (
                s(good.join("memory").join("file.md")),
                Err(TranscriptHintError::BadShape),
            ),
        ];
        for (input, want) in cases {
            let got = TranscriptHint::from_memory_path(&input).map(|h| h.dir().to_path_buf());
            assert_eq!(got, want, "input {input:?}");
        }
    }

    #[test]
    fn transcript_path_needs_a_uuid() {
        let dir = root().join("projects").join("p");
        let hint = TranscriptHint::from_memory_path(dir.join("memory").to_str().unwrap()).unwrap();
        let id = "0000aaaa-0000-4000-8000-00000000000F";
        assert_eq!(
            hint.transcript_path(id),
            Some(dir.join(format!("{id}.jsonl")))
        );
        for bad in [
            "",
            "../../etc/passwd",
            "0000aaaa-0000-4000-8000-00000000000",
            "0000aaaa_0000-4000-8000-000000000000",
            "0000aaaa-0000-4000-8000-00000000000g",
        ] {
            assert_eq!(hint.transcript_path(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn debug_has_no_path() {
        let hint = TranscriptHint::from_memory_path(
            root()
                .join("projects")
                .join("p")
                .join("memory")
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(format!("{hint:?}"), "TranscriptHint { .. }");
    }
}
