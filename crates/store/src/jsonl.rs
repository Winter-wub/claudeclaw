//! Month-split, append-only JSONL files: the Run index and the audit log (§9.1 "JSONL logs are
//! append-only", #44 D3).

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Months, NaiveDate, Utc};

use crate::error::StoreError;

/// `YYYY-MM`, the UTC month of `at`.
pub(crate) fn month_of(at: DateTime<Utc>) -> String {
    at.format("%Y-%m").to_string()
}

/// The first instant of month `month` (`YYYY-MM`), or `None` if it isn't one.
pub(crate) fn month_start(month: &str) -> Option<DateTime<Utc>> {
    if month.len() != 7 || month.as_bytes()[4] != b'-' {
        return None;
    }
    let date = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").ok()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc())
}

/// The first instant after month `month`.
pub(crate) fn month_end(month: &str) -> Option<DateTime<Utc>> {
    month_start(month)?.checked_add_months(Months::new(1))
}

/// The `<YYYY-MM>.jsonl` files in `dir`, newest month first. Other names are ignored; a missing
/// dir has none.
pub(crate) fn month_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, StoreError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(StoreError::io("list", dir)(e)),
    };
    let mut months = Vec::new();
    for entry in entries {
        let entry = entry.map_err(StoreError::io("list", dir))?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if let Some(month) = name.strip_suffix(".jsonl")
            && month_start(month).is_some()
        {
            months.push((month.to_owned(), entry.path()));
        }
    }
    months.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(months)
}

/// A JSONL file's complete lines, and whether it ended in a torn line (no final `\n`), which
/// is left out.
pub(crate) struct Lines {
    pub lines: Vec<String>,
    pub torn: bool,
    /// The file's size in bytes.
    pub len: u64,
}

/// Reads `path`'s lines. A missing file has none.
pub(crate) fn read_lines(path: &Path) -> Result<Lines, StoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(StoreError::io("read", path)(e)),
    };
    let len = bytes.len() as u64;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    // `split` leaves the text after the last `\n`: empty when the file ends in one.
    let tail = lines.pop().unwrap_or_default();
    Ok(Lines {
        lines,
        torn: !tail.is_empty(),
        len,
    })
}

/// Appends `bytes` (whole lines, each ending in `\n`) to `path`, creating it. If the file doesn't
/// end in `\n` (a torn last line), one is written first, so the new lines start clean (#44 D3).
/// Synced before it returns.
pub(crate) fn append(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .map_err(StoreError::io("open", path))?;
    let len = file.metadata().map_err(StoreError::io("stat", path))?.len();
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if len > 0 {
        let mut last = [0u8];
        file.seek(SeekFrom::Start(len - 1))
            .and_then(|_| file.read_exact(&mut last))
            .map_err(StoreError::io("read", path))?;
        if last[0] != b'\n' {
            out.push(b'\n');
        }
    }
    out.extend_from_slice(bytes);
    file.write_all(&out)
        .map_err(StoreError::io("append to", path))?;
    file.sync_data().map_err(StoreError::io("fsync", path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_parse_and_bound() {
        assert_eq!(month_of("2026-10-31T23:59:59Z".parse().unwrap()), "2026-10");
        assert_eq!(
            month_end("2026-12"),
            Some("2027-01-01T00:00:00Z".parse().unwrap())
        );
        for bad in ["2026-13", "2026-1", "26-10", "2026_10", "2026-10-01"] {
            assert_eq!(month_start(bad), None, "{bad}");
        }
    }

    #[test]
    fn append_repairs_a_torn_tail_and_reads_skip_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("2026-10.jsonl");
        assert!(read_lines(&path).unwrap().lines.is_empty());
        append(&path, b"{\"a\":1}\n").unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"a\":")
            .unwrap();
        let read = read_lines(&path).unwrap();
        assert_eq!(read.lines, ["{\"a\":1}"]);
        assert!(read.torn);

        append(&path, b"{\"a\":3}\n").unwrap();
        let read = read_lines(&path).unwrap();
        assert_eq!(read.lines, ["{\"a\":1}", "{\"a\":", "{\"a\":3}"]);
        assert!(!read.torn);
    }

    #[test]
    fn month_files_are_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "2026-09.jsonl",
            "2026-10.jsonl",
            "notes.jsonl",
            "2026-11.json",
        ] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let months: Vec<_> = month_files(dir.path())
            .unwrap()
            .into_iter()
            .map(|(m, _)| m)
            .collect();
        assert_eq!(months, ["2026-10", "2026-09"]);
        assert!(month_files(&dir.path().join("none")).unwrap().is_empty());
    }
}
