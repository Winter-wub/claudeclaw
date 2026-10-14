//! Change scan tests (#46 D9): passes driven over a fake [`FsView`] with explicit times, plus a
//! few over a real temp dir.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;

/// A pass time: `secs` after a fixed base.
fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000 + secs)
}

fn nanos(t: SystemTime) -> i128 {
    t.duration_since(UNIX_EPOCH).unwrap().as_nanos() as i128
}

enum Node {
    Dir,
    File {
        bytes: Vec<u8>,
        stat: FileStat,
    },
    /// A dir that can't be listed, or a file that can't be stat'd or read.
    Denied,
    /// A file that can be stat'd but not read.
    Unreadable {
        stat: FileStat,
    },
}

/// An in-memory tree. Every write bumps a counter that stands in for ctime, so a write always
/// moves the key unless a test says otherwise.
#[derive(Default)]
struct FakeFs {
    nodes: BTreeMap<PathBuf, Node>,
    clock: i128,
    next_ino: u64,
}

impl FakeFs {
    fn mkdir(&mut self, dir: &Path) {
        for a in dir.ancestors() {
            self.nodes.entry(a.to_path_buf()).or_insert(Node::Dir);
        }
    }

    /// An atomic replace (temp + rename): new inode, new ctime.
    fn put(&mut self, path: &Path, bytes: &[u8], mtime: SystemTime) {
        self.mkdir(path.parent().unwrap());
        self.clock += 1;
        self.next_ino += 1;
        let stat = FileStat {
            kind: FileKind::File,
            key: StatKey {
                dev: 1,
                ino: self.next_ino,
                size: bytes.len() as u64,
                modified: nanos(mtime),
                changed: self.clock,
                attributes: 0,
            },
            modified: mtime,
        };
        self.nodes.insert(
            path.to_path_buf(),
            Node::File {
                bytes: bytes.to_vec(),
                stat,
            },
        );
    }

    /// New bytes and nothing else: what a same-size write inside one coarse timestamp tick looks
    /// like to a stat.
    fn set_bytes_only(&mut self, path: &Path, bytes: &[u8]) {
        let Some(Node::File { bytes: b, .. }) = self.nodes.get_mut(path) else {
            panic!("no file {}", path.display());
        };
        *b = bytes.to_vec();
    }

    /// An in-place rewrite with mtime restored (`touch -r`): only ctime moves.
    fn rewrite_keep_mtime(&mut self, path: &Path, bytes: &[u8]) {
        self.clock += 1;
        let clock = self.clock;
        let Some(Node::File { bytes: b, stat }) = self.nodes.get_mut(path) else {
            panic!("no file {}", path.display());
        };
        assert_eq!(b.len(), bytes.len(), "same size");
        *b = bytes.to_vec();
        stat.key.changed = clock;
    }

    fn remove(&mut self, path: &Path) {
        let gone: Vec<_> = self
            .nodes
            .keys()
            .filter(|p| p.starts_with(path))
            .cloned()
            .collect();
        for p in gone {
            self.nodes.remove(&p);
        }
    }

    fn deny(&mut self, path: &Path) {
        self.nodes.insert(path.to_path_buf(), Node::Denied);
    }

    fn deny_read(&mut self, path: &Path) {
        let Some(Node::File { stat, .. }) = self.nodes.get(path) else {
            panic!("no file {}", path.display());
        };
        let stat = *stat;
        self.nodes
            .insert(path.to_path_buf(), Node::Unreadable { stat });
    }

    fn key(&self, path: &Path) -> StatKey {
        self.stat(path).unwrap().key
    }
}

fn denied() -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}

fn not_found() -> io::Error {
    io::Error::from(io::ErrorKind::NotFound)
}

impl FsView for FakeFs {
    fn stat(&self, path: &Path) -> io::Result<FileStat> {
        match self.nodes.get(path) {
            Some(Node::File { stat, .. } | Node::Unreadable { stat }) => Ok(*stat),
            Some(Node::Dir) => Ok(FileStat {
                kind: FileKind::Dir,
                key: StatKey {
                    dev: 1,
                    ino: 0,
                    size: 0,
                    modified: 0,
                    changed: 0,
                    attributes: 0,
                },
                modified: UNIX_EPOCH,
            }),
            Some(Node::Denied) => Err(denied()),
            None => Err(not_found()),
        }
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        match self.nodes.get(dir) {
            Some(Node::Dir) => Ok(self
                .nodes
                .keys()
                .filter(|p| p.parent() == Some(dir))
                .map(|p| p.file_name().unwrap().to_owned())
                .collect()),
            Some(Node::File { .. } | Node::Unreadable { .. }) => {
                Err(io::Error::from(io::ErrorKind::NotADirectory))
            }
            Some(Node::Denied) => Err(denied()),
            None => Err(not_found()),
        }
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        match self.nodes.get(path) {
            Some(Node::File { bytes, .. }) => Ok(bytes.clone()),
            Some(Node::Dir) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            Some(Node::Denied | Node::Unreadable { .. }) => Err(denied()),
            None => Err(not_found()),
        }
    }
}

fn layout() -> Layout {
    Layout::new("/p")
}

fn scanner() -> Scanner {
    Scanner::new(ScanTargets::new(&layout(), None))
}

fn job(label: &str) -> PathBuf {
    layout().jobs_dir().join(format!("{label}.md"))
}

fn agent_job(agent: &str, label: &str) -> PathBuf {
    layout().agent_jobs_dir(agent).join(format!("{label}.md"))
}

/// `(kind, job name, bytes)` per change, for compact asserts.
fn summary(changes: &[ScanChange]) -> Vec<(&'static str, String, String)> {
    let text = |b: &FileBytes| String::from_utf8(b.as_bytes().to_vec()).unwrap();
    changes
        .iter()
        .map(|c| match c {
            ScanChange::JobAdded { job, bytes, .. } => ("added", job.name(), text(bytes)),
            ScanChange::JobModified { job, bytes, .. } => ("modified", job.name(), text(bytes)),
            ScanChange::JobRemoved { job } => ("removed", job.name(), String::new()),
            ScanChange::SettingsChanged { bytes } => (
                "settings",
                String::new(),
                bytes.as_ref().map(text).unwrap_or_default(),
            ),
            ScanChange::JobUnreadable { job, .. } => ("unreadable", job.name(), String::new()),
            ScanChange::SettingsUnreadable { .. } => {
                ("settings_unreadable", String::new(), String::new())
            }
            ScanChange::DirUnavailable { dir, .. } => {
                ("unavailable", format!("{dir:?}"), String::new())
            }
            ScanChange::DirAvailable {
                dir, first_listing, ..
            } => (
                "available",
                format!("{dir:?}"),
                if *first_listing { "first" } else { "" }.to_owned(),
            ),
        })
        .collect()
}

fn one(kind: &'static str, name: &str, bytes: &str) -> Vec<(&'static str, String, String)> {
    vec![(kind, name.to_owned(), bytes.to_owned())]
}

fn took_effect(changes: &[ScanChange]) -> SystemTime {
    match changes {
        [ScanChange::JobAdded { took_effect, .. }]
        | [ScanChange::JobModified { took_effect, .. }] => *took_effect,
        other => panic!("expected one job change, got {other:?}"),
    }
}

/// A scanner whose snapshot holds `a.md` with `"one"`, applied through two stat passes ending
/// at t=4.
fn with_job_a(fs: &mut FakeFs) -> Scanner {
    let mut s = scanner();
    fs.put(&job("a"), b"one", at(1));
    assert!(s.stat_pass(fs, at(2)).is_empty());
    assert_eq!(summary(&s.stat_pass(fs, at(4))), one("added", "a", "one"));
    s
}

// ---- Stability (#46 D3) ------------------------------------------------------------------------

#[test]
fn a_new_file_is_applied_once_stable_across_two_passes() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    assert!(s.stat_pass(&fs, at(6)).is_empty(), "nothing changed");

    fs.put(&job("a"), b"two", at(7));
    assert!(s.stat_pass(&fs, at(8)).is_empty(), "pending, not read yet");
    assert_eq!(
        summary(&s.stat_pass(&fs, at(10))),
        one("modified", "a", "two")
    );
    assert!(s.stat_pass(&fs, at(12)).is_empty());
}

#[test]
fn a_file_changing_on_every_pass_waits_until_it_settles() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    // A truncated mid-write version is seen once and replaced before the next pass.
    fs.put(&job("a"), b"tw", at(5));
    assert!(s.stat_pass(&fs, at(6)).is_empty());
    fs.put(&job("a"), b"two", at(7));
    assert!(s.stat_pass(&fs, at(8)).is_empty(), "the key moved again");
    assert_eq!(
        summary(&s.stat_pass(&fs, at(10))),
        one("modified", "a", "two")
    );
}

#[test]
fn stable_bytes_equal_to_the_snapshot_only_refresh_the_key() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    fs.put(&job("a"), b"one", at(5)); // `touch`, or a checkout of the same content
    assert!(s.stat_pass(&fs, at(6)).is_empty());
    assert!(s.stat_pass(&fs, at(8)).is_empty(), "same bytes: no change");
    assert!(s.hash_pass(&fs, at(10)).is_empty());
}

#[test]
fn a_file_unstable_for_30s_is_applied_by_the_next_hash_pass() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    let mut t = 5;
    let mut changes = Vec::new();
    while t < 40 {
        fs.put(&job("a"), format!("v{t}").as_bytes(), at(t));
        changes.extend(s.stat_pass(&fs, at(t + 1)));
        t += 2;
    }
    assert!(
        changes.is_empty(),
        "stat passes never read an unstable file"
    );
    // First seen changing at t=6; 34 s later the hash pass applies what it reads.
    fs.put(&job("a"), b"latest", at(39));
    assert_eq!(
        summary(&s.hash_pass(&fs, at(40))),
        one("modified", "a", "latest")
    );
}

#[test]
fn a_change_reverted_before_it_settles_is_never_reported() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    let applied = fs.key(&job("a"));
    fs.put(&job("a"), b"two", at(5));
    assert!(s.stat_pass(&fs, at(6)).is_empty());
    // Back to exactly the applied file (e.g. an undo that restored the inode via a backup).
    if let Some(Node::File { bytes, stat }) = fs.nodes.get_mut(&job("a")) {
        *bytes = b"one".to_vec();
        stat.key = applied;
    }
    assert!(s.stat_pass(&fs, at(8)).is_empty());
    assert!(s.stat_pass(&fs, at(10)).is_empty());
}

// ---- Absence (#46 D3, D4) ----------------------------------------------------------------------

#[test]
fn a_removal_needs_two_passes() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    fs.remove(&job("a"));
    assert!(s.stat_pass(&fs, at(6)).is_empty(), "missing once");
    assert_eq!(summary(&s.stat_pass(&fs, at(8))), one("removed", "a", ""));
    assert!(s.stat_pass(&fs, at(10)).is_empty());
}

#[test]
fn a_brief_disappearance_with_the_same_bytes_is_not_a_change() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    fs.remove(&job("a"));
    assert!(s.stat_pass(&fs, at(6)).is_empty());
    // `git checkout` or a move out and back: same bytes, new inode.
    fs.put(&job("a"), b"one", at(7));
    assert!(s.stat_pass(&fs, at(8)).is_empty());
    assert!(s.stat_pass(&fs, at(10)).is_empty());
    assert!(s.hash_pass(&fs, at(12)).is_empty());
}

#[test]
fn a_new_file_that_vanishes_before_it_settles_is_never_reported() {
    let mut fs = FakeFs::default();
    let mut s = scanner();
    fs.put(&job("tmp"), b"x", at(1));
    assert!(s.stat_pass(&fs, at(2)).is_empty());
    fs.remove(&job("tmp"));
    assert!(s.stat_pass(&fs, at(4)).is_empty());
    assert!(s.stat_pass(&fs, at(6)).is_empty());
}

#[test]
fn a_missing_agent_jobs_dir_removes_its_jobs() {
    let mut fs = FakeFs::default();
    let mut s = scanner();
    fs.put(&agent_job("ops", "daily"), b"d", at(1));
    s.stat_pass(&fs, at(2));
    assert_eq!(
        summary(&s.stat_pass(&fs, at(4))),
        one("added", "ops/daily", "d")
    );
    fs.remove(&layout().agent_jobs_dir("ops"));
    assert!(s.stat_pass(&fs, at(6)).is_empty());
    assert_eq!(
        summary(&s.stat_pass(&fs, at(8))),
        one("removed", "ops/daily", "")
    );
}

#[test]
fn an_unreadable_dir_keeps_its_jobs_and_is_reported_once() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    let dir = layout().jobs_dir();
    let saved = fs.nodes.remove(&job("a")).unwrap();
    fs.deny(&dir);
    assert_eq!(
        summary(&s.stat_pass(&fs, at(6))),
        one("unavailable", "Jobs", "")
    );
    assert!(s.stat_pass(&fs, at(8)).is_empty(), "once per episode");
    assert!(s.hash_pass(&fs, at(10)).is_empty(), "no removal");

    fs.nodes.insert(dir.clone(), Node::Dir);
    fs.nodes.insert(job("a"), saved);
    assert_eq!(
        summary(&s.stat_pass(&fs, at(12))),
        one("available", "Jobs", "")
    );
    assert!(s.stat_pass(&fs, at(14)).is_empty());
}

#[test]
fn an_unreadable_agents_dir_keeps_every_agent_job() {
    let mut fs = FakeFs::default();
    let mut s = scanner();
    fs.put(&agent_job("ops", "daily"), b"d", at(1));
    s.stat_pass(&fs, at(2));
    s.stat_pass(&fs, at(4));
    fs.remove(&layout().agents_dir());
    fs.deny(&layout().agents_dir());
    assert_eq!(s.stat_pass(&fs, at(6)).len(), 1, "unavailable");
    assert!(s.stat_pass(&fs, at(8)).is_empty(), "no removal");
}

#[test]
fn a_missing_jobs_dir_is_zero_jobs_unless_set_explicitly() {
    let fs = FakeFs::default();
    assert!(scanner().stat_pass(&fs, at(2)).is_empty());

    let mut s = Scanner::new(ScanTargets::new(&layout(), Some("my-jobs")));
    let want = layout().project_dir().join("my-jobs");
    assert_eq!(s.targets().path_of(&WatchedDir::Jobs), want);
    match s.stat_pass(&fs, at(2)).as_slice() {
        [
            ScanChange::DirUnavailable {
                dir: WatchedDir::Jobs,
                path,
                error: io::ErrorKind::NotFound,
            },
        ] => assert_eq!(*path, want),
        other => panic!("{other:?}"),
    }
}

// ---- What is scanned (#46 D2) ------------------------------------------------------------------

#[test]
fn dot_files_and_non_md_names_are_ignored() {
    let mut fs = FakeFs::default();
    let mut s = scanner();
    for name in [
        ".x.md",      // TS would load this as job `.x` (src/jobs.ts:117)
        "._a.md",     // macOS AppleDouble on SMB and exFAT
        ".!123!a.md", // BSD `sed -i`
        ".#a.md",     // Emacs lock
        ".md",        // no label
        "a.md.swp",   // vim
        "notes.txt",
        ".tmpAbC123", // the Daemon's own temp files
    ] {
        fs.put(&layout().jobs_dir().join(name), b"x", at(1));
    }
    fs.put(
        &layout().agents_archive_dir().join("jobs/old.md"),
        b"x",
        at(1),
    );
    fs.put(&agent_job(".hidden", "j"), b"x", at(1));
    fs.mkdir(&layout().jobs_dir().join("dir.md"));
    fs.put(&job("real"), b"r", at(1));
    fs.put(&agent_job("ops", "daily"), b"d", at(1));

    assert!(s.stat_pass(&fs, at(2)).is_empty());
    let mut names: Vec<_> = summary(&s.stat_pass(&fs, at(4)))
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    names.sort();
    assert_eq!(names, ["ops/daily", "real"]);
}

#[test]
fn job_names_follow_ts() {
    // TS: flat jobs are named by the stem (src/jobs.ts:118), agent jobs `<agent>/<label>`
    // (src/jobs.ts:141).
    let cases = [
        (job("daily"), None, "daily", "daily"),
        (agent_job("ops", "daily"), Some("ops"), "daily", "ops/daily"),
        (job("a.b"), None, "a.b", "a.b"),
    ];
    let s = scanner();
    for (path, agent, label, name) in cases {
        let Some(Target::Job(file)) = s.classify(&path) else {
            panic!("{} is not a job", path.display());
        };
        assert_eq!(
            (file.agent(), file.label(), file.name().as_str()),
            (agent, label, name)
        );
        assert_eq!(file.path(), path);
    }
    assert!(s.classify(&job(".x")).is_none());
    assert!(s.classify(&agent_job(".archive", "x")).is_none());
    assert!(s.classify(Path::new("/p/agents/ops/x.md")).is_none());
    assert!(s.classify(&layout().settings()).is_some());
}

#[test]
fn jobs_dir_resolves_like_ts() {
    // src/config.ts:22-23 (`isAbsolute ? dir : join(cwd, dir)`) and :456 (trimmed, blank unset).
    let l = layout();
    let cases: [(Option<&str>, &str); 5] = [
        (None, "/p/.claude/claudeclaw/jobs"),
        (Some("   "), "/p/.claude/claudeclaw/jobs"),
        (Some("jobs"), "/p/jobs"),
        (Some(" /abs/jobs "), "/abs/jobs"),
        (
            Some(".claude/claudeclaw/jobs"),
            "/p/.claude/claudeclaw/jobs",
        ),
    ];
    for (setting, want) in cases {
        assert_eq!(l.jobs_dir_for(setting), Path::new(want), "{setting:?}");
    }
    assert!(!ScanTargets::new(&l, Some(" ")).jobs_dir_explicit());
    assert!(ScanTargets::new(&l, Some("jobs")).jobs_dir_explicit());
}

// ---- Stat key and hash pass (#46 D1) -----------------------------------------------------------

#[test]
fn a_ctime_only_change_is_detected_by_the_stat_pass() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    let before = fs.key(&job("a"));
    fs.rewrite_keep_mtime(&job("a"), b"two");
    let after = fs.key(&job("a"));
    assert_eq!(
        (after.size, after.modified, after.ino),
        (before.size, before.modified, before.ino)
    );
    assert!(s.stat_pass(&fs, at(6)).is_empty());
    assert_eq!(
        summary(&s.stat_pass(&fs, at(8))),
        one("modified", "a", "two")
    );
}

#[test]
fn a_change_with_the_same_stat_key_is_found_only_by_the_hash_pass() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    fs.set_bytes_only(&job("a"), b"two");
    for t in [6, 8, 10] {
        assert!(
            s.stat_pass(&fs, at(t)).is_empty(),
            "the stat key didn't move"
        );
    }
    let changes = s.hash_pass(&fs, at(12));
    assert_eq!(summary(&changes), one("modified", "a", "two"));
    // The mtime (t=1) predates the last read of the old bytes (t=4), so that read is the bound.
    assert_eq!(took_effect(&changes), at(4));
    assert!(s.hash_pass(&fs, at(14)).is_empty());

    fs.set_bytes_only(&job("a"), b"six");
    let changes = s.hash_pass(&fs, at(44));
    assert_eq!(
        took_effect(&changes),
        at(14),
        "the hash pass that last read the old bytes"
    );
}

// ---- When a schedule took effect (#50) ---------------------------------------------------------

#[test]
fn took_effect_is_the_mtime_clamped_to_the_pass_window() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    assert!(s.stat_pass(&fs, at(10)).is_empty());

    // Saved at t=12, first seen at t=14 (previous pass t=10), applied at t=16.
    fs.put(&job("a"), b"two", at(12));
    s.stat_pass(&fs, at(14));
    assert_eq!(took_effect(&s.stat_pass(&fs, at(16))), at(12));

    // An old mtime (`cp -p`, a restored backup) is clamped up to the previous pass.
    fs.put(&job("a"), b"three", at(1));
    s.stat_pass(&fs, at(18));
    assert_eq!(took_effect(&s.stat_pass(&fs, at(20))), at(16));

    // A future mtime (clock skew, `touch -d`) is clamped down to the first sighting.
    fs.put(&job("a"), b"four", at(999));
    s.stat_pass(&fs, at(22));
    assert_eq!(took_effect(&s.stat_pass(&fs, at(24))), at(22));
}

#[test]
fn clamp_table() {
    let cases = [
        (5, Some(3), 7, 5),
        (1, Some(3), 7, 3),
        (9, Some(3), 7, 7),
        (1, None, 7, 1),
        (9, None, 7, 7),
    ];
    for (mtime, lower, upper, want) in cases {
        assert_eq!(clamp(at(mtime), lower.map(at), at(upper)), at(want));
    }
}

// ---- Startup and settings (#46 D7, D8) ---------------------------------------------------------

#[test]
fn the_startup_pass_reports_everything_at_once() {
    let mut fs = FakeFs::default();
    fs.put(&job("a"), b"one", at(1));
    fs.put(&agent_job("ops", "daily"), b"d", at(500));
    let mut s = scanner();
    let changes = s.startup_pass(&fs, at(100));
    assert_eq!(
        summary(&changes),
        vec![
            ("added", "a".to_owned(), "one".to_owned()),
            ("added", "ops/daily".to_owned(), "d".to_owned()),
            ("settings", String::new(), String::new()),
        ]
    );
    assert!(matches!(
        changes[2],
        ScanChange::SettingsChanged { bytes: None }
    ));
    let effects: Vec<_> = changes[..2]
        .iter()
        .map(|c| took_effect(std::slice::from_ref(c)))
        .collect();
    assert_eq!(effects, [at(1), at(100)], "mtime, capped at now");
    assert!(s.stat_pass(&fs, at(102)).is_empty());
    assert!(s.hash_pass(&fs, at(104)).is_empty());
}

#[test]
fn settings_follow_the_same_rules() {
    let mut fs = FakeFs::default();
    let settings = layout().settings();
    fs.put(&settings, b"{}", at(1));
    let mut s = scanner();
    assert_eq!(
        summary(&s.startup_pass(&fs, at(2))),
        one("settings", "", "{}")
    );

    fs.put(&settings, br#"{"a":1}"#, at(3));
    assert!(s.stat_pass(&fs, at(4)).is_empty());
    assert_eq!(
        summary(&s.stat_pass(&fs, at(6))),
        one("settings", "", r#"{"a":1}"#)
    );

    fs.remove(&settings);
    assert!(s.stat_pass(&fs, at(8)).is_empty());
    assert_eq!(
        s.stat_pass(&fs, at(10)),
        [ScanChange::SettingsChanged { bytes: None }]
    );
}

// ---- The Daemon's own writes (#46 D5) ----------------------------------------------------------

#[test]
fn the_daemons_own_write_is_not_reported_back() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    let path = job("a");

    // Step 1: read the bytes and their key. Step 5: re-stat before the rename.
    let before = stat_key(&fs, &path).unwrap();
    check_unchanged(&fs, &path, before).unwrap();
    // Step 6: rename, then record.
    fs.put(&path, b"api edit", at(5));
    let key = s.record_own_write(&fs, &path, b"api edit").unwrap();
    assert_eq!(key, fs.key(&path));

    for t in [6, 8, 10] {
        assert!(s.stat_pass(&fs, at(t)).is_empty());
    }
    assert!(s.hash_pass(&fs, at(12)).is_empty());

    // A new job the Daemon creates is not reported either.
    let new = job("new");
    check_unchanged(&fs, &new, None).unwrap();
    fs.put(&new, b"created", at(13));
    s.record_own_write(&fs, &new, b"created").unwrap();
    assert!(s.stat_pass(&fs, at(14)).is_empty());
    assert!(s.stat_pass(&fs, at(16)).is_empty());
}

#[test]
fn the_restat_refuses_a_file_changed_since_the_read() {
    let mut fs = FakeFs::default();
    let path = job("a");
    fs.put(&path, b"one", at(1));
    let before = stat_key(&fs, &path).unwrap();
    fs.put(&path, b"editor save", at(2));
    assert!(matches!(
        check_unchanged(&fs, &path, before),
        Err(ScanError::ChangedOnDisk { .. })
    ));
    // Created by someone else after the writer saw it absent.
    assert!(matches!(check_unchanged(&fs, &job("b"), None), Ok(())));
    fs.put(&job("b"), b"x", at(3));
    assert!(matches!(
        check_unchanged(&fs, &job("b"), None),
        Err(ScanError::ChangedOnDisk { .. })
    ));
}

#[test]
fn an_outside_write_racing_the_own_write_is_caught_by_the_hash_pass() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    let path = job("a");
    // The Daemon renamed "ours" into place, but an editor's save landed before the stat.
    fs.put(&path, b"theirs", at(5));
    s.record_own_write(&fs, &path, b"ours").unwrap();
    assert!(s.stat_pass(&fs, at(6)).is_empty());
    assert_eq!(
        summary(&s.hash_pass(&fs, at(8))),
        one("modified", "a", "theirs")
    );
}

#[test]
fn own_writes_outside_the_targets_are_refused_and_forget_drops_a_job() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    let other = PathBuf::from("/p/elsewhere/x.md");
    fs.put(&other, b"x", at(1));
    assert!(matches!(
        s.record_own_write(&fs, &other, b"x"),
        Err(ScanError::NotWatched { .. })
    ));

    // An API delete: the snapshot entry goes first, so the scan has nothing to orphan.
    assert!(s.forget(&job("a")));
    fs.remove(&job("a"));
    assert!(s.stat_pass(&fs, at(6)).is_empty());
    assert!(s.stat_pass(&fs, at(8)).is_empty());
}

#[test]
fn file_bytes_debug_shows_no_content() {
    let bytes = FileBytes::new(b"secret prompt".to_vec());
    let debug = format!("{bytes:?}");
    assert!(!debug.contains("secret"), "{debug}");
    assert!(debug.contains("len: 13"), "{debug}");
    assert_eq!(bytes.revision(), Revision::of(b"secret prompt"));
}

// ---- Ordering and full listings (#46 D4, D7) --------------------------------------------------

#[test]
fn removals_come_before_additions_on_a_retarget() {
    let mut fs = FakeFs::default();
    let mut s = with_job_a(&mut fs);
    // The new dir's path sorts before the old one's, so path order alone would add first.
    let other = Path::new("/a/jobs/a.md");
    fs.put(other, b"other", at(5));
    s.set_targets(ScanTargets::new(&layout(), Some("/a/jobs")));
    assert!(s.hash_pass(&fs, at(6)).is_empty());
    let changes = s.stat_pass(&fs, at(8));
    assert_eq!(
        summary(&changes),
        vec![
            ("removed", "a".to_owned(), String::new()),
            ("added", "a".to_owned(), "other".to_owned()),
        ]
    );

    // A consumer keyed by job name ends with the file that is there now.
    let mut jobs = BTreeMap::new();
    for change in &changes {
        match change {
            ScanChange::JobAdded { job, .. } | ScanChange::JobModified { job, .. } => {
                jobs.insert(job.name(), job.path().to_path_buf());
            }
            ScanChange::JobRemoved { job } => {
                jobs.remove(&job.name());
            }
            _ => {}
        }
    }
    assert_eq!(jobs.get("a").map(PathBuf::as_path), Some(other));
}

#[test]
fn startup_reports_unreadable_job_files_and_keeps_tracking_them() {
    let mut fs = FakeFs::default();
    fs.put(&job("a"), b"one", at(1));
    fs.deny_read(&job("a")); // stat works, read fails
    fs.put(&job("b"), b"two", at(1));
    fs.deny(&job("b")); // stat fails
    fs.put(&job("c"), b"three", at(1));
    let mut s = scanner();

    let mut changes = summary(&s.startup_pass(&fs, at(10)));
    changes.sort();
    assert_eq!(
        changes,
        vec![
            ("added", "c".to_owned(), "three".to_owned()),
            ("settings", String::new(), String::new()),
            ("unreadable", "a".to_owned(), String::new()),
            ("unreadable", "b".to_owned(), String::new()),
        ]
    );
    for t in [12, 14] {
        assert!(s.stat_pass(&fs, at(t)).is_empty(), "kept as is");
    }
    assert!(s.hash_pass(&fs, at(16)).is_empty());

    // Readable again: the caller learns its bytes. Gone: the caller learns that too.
    fs.put(&job("a"), b"one", at(1));
    fs.remove(&job("b"));
    assert!(s.stat_pass(&fs, at(18)).is_empty());
    assert_eq!(
        summary(&s.stat_pass(&fs, at(20))),
        vec![
            ("removed", "b".to_owned(), String::new()),
            ("modified", "a".to_owned(), "one".to_owned()),
        ]
    );
}

#[test]
fn startup_reports_exactly_one_settings_event() {
    let settings = layout().settings();
    type Setup = fn(&mut FakeFs, &Path);
    let cases: [(&str, Setup, &str); 4] = [
        ("absent", |fs, p| fs.remove(p), "settings"),
        ("readable", |_, _| {}, "settings"),
        ("stat fails", |fs, p| fs.deny(p), "settings_unreadable"),
        ("read fails", |fs, p| fs.deny_read(p), "settings_unreadable"),
    ];
    for (case, setup, want) in cases {
        let mut fs = FakeFs::default();
        fs.put(&settings, b"{}", at(1));
        setup(&mut fs, &settings);
        let changes = scanner().startup_pass(&fs, at(2));
        let kinds: Vec<_> = summary(&changes).into_iter().map(|(k, ..)| k).collect();
        assert_eq!(kinds, [want], "{case}");
    }
}

#[test]
fn unreadable_settings_at_startup_are_kept_and_later_applied() {
    let mut fs = FakeFs::default();
    let settings = layout().settings();
    fs.put(&settings, b"{}", at(1));
    fs.deny_read(&settings);
    let mut s = scanner();
    assert_eq!(
        summary(&s.startup_pass(&fs, at(2))),
        one("settings_unreadable", "", "")
    );
    fs.put(&settings, b"{}", at(3));
    assert!(s.stat_pass(&fs, at(4)).is_empty());
    assert_eq!(summary(&s.stat_pass(&fs, at(6))), one("settings", "", "{}"));
}

#[test]
fn a_dir_unreadable_since_startup_is_listed_in_full_when_it_recovers() {
    let mut fs = FakeFs::default();
    fs.put(&job("a"), b"one", at(1));
    fs.put(&agent_job("ops", "daily"), b"d", at(1));
    let saved_a = fs.nodes.remove(&job("a")).unwrap();
    fs.deny(&layout().jobs_dir());
    fs.remove(&layout().agents_dir());
    fs.deny(&layout().agents_dir());
    let mut s = scanner();
    assert_eq!(
        summary(&s.startup_pass(&fs, at(2))),
        vec![
            ("unavailable", "Jobs".to_owned(), String::new()),
            ("unavailable", "Agents".to_owned(), String::new()),
            ("settings", String::new(), String::new()),
        ]
    );
    assert!(s.stat_pass(&fs, at(4)).is_empty());

    // The flat dir recovers: its whole contents come in the same pass, without a wait.
    fs.nodes.insert(layout().jobs_dir(), Node::Dir);
    fs.nodes.insert(job("a"), saved_a);
    assert_eq!(
        summary(&s.stat_pass(&fs, at(6))),
        vec![
            ("available", "Jobs".to_owned(), "first".to_owned()),
            ("added", "a".to_owned(), "one".to_owned()),
        ]
    );

    // `agents/` recovers, but one agent's `jobs/` is still unreadable: that dir now starts its
    // own episode, and is listed in full once it recovers too.
    fs.nodes.insert(layout().agents_dir(), Node::Dir);
    fs.mkdir(&layout().agent_dir("ops"));
    fs.deny(&layout().agent_jobs_dir("ops"));
    assert_eq!(
        summary(&s.stat_pass(&fs, at(8))),
        vec![
            (
                "unavailable",
                r#"AgentJobs("ops")"#.to_owned(),
                String::new()
            ),
            ("available", "Agents".to_owned(), "first".to_owned()),
        ]
    );
    fs.nodes.insert(layout().agent_jobs_dir("ops"), Node::Dir);
    fs.put(&agent_job("ops", "daily"), b"d", at(9));
    assert_eq!(
        summary(&s.stat_pass(&fs, at(10))),
        vec![
            (
                "available",
                r#"AgentJobs("ops")"#.to_owned(),
                "first".to_owned()
            ),
            ("added", "ops/daily".to_owned(), "d".to_owned()),
        ]
    );
}

#[test]
fn watched_dirs_hold_their_jobs() {
    let s = scanner();
    let Some(Target::Job(flat)) = s.classify(&job("a")) else {
        panic!()
    };
    let Some(Target::Job(ops)) = s.classify(&agent_job("ops", "a")) else {
        panic!()
    };
    let table = [
        (WatchedDir::Jobs, true, false),
        (WatchedDir::Agents, false, true),
        (WatchedDir::AgentJobs("ops".into()), false, true),
        (WatchedDir::AgentJobs("dev".into()), false, false),
    ];
    for (dir, holds_flat, holds_ops) in table {
        assert_eq!(
            (dir.holds(&flat), dir.holds(&ops)),
            (holds_flat, holds_ops),
            "{dir:?}"
        );
    }
    assert_eq!(
        s.targets().path_of(&WatchedDir::AgentJobs("ops".into())),
        layout().agent_jobs_dir("ops")
    );
}

// ---- The real filesystem -----------------------------------------------------------------------

mod real {
    use super::*;
    use crate::atomic::{FileMode, write_atomic};

    fn setup() -> (tempfile::TempDir, Layout, Scanner) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.jobs_dir()).unwrap();
        let scanner = Scanner::new(ScanTargets::new(&layout, None));
        (dir, layout, scanner)
    }

    #[test]
    fn atomic_writes_are_applied_after_two_passes_and_removals_after_two_more() {
        let (_dir, layout, mut s) = setup();
        let path = layout.jobs_dir().join("a.md");
        let now = SystemTime::now();
        write_atomic(&path, b"one", FileMode::Keep).unwrap();
        std::fs::write(layout.jobs_dir().join(".x.md"), b"dot").unwrap();
        assert!(s.stat_pass(&StdFs, now).is_empty());
        let changes = s.stat_pass(&StdFs, now + Duration::from_secs(2));
        assert_eq!(summary(&changes), one("added", "a", "one"));

        write_atomic(&path, b"two", FileMode::Keep).unwrap();
        assert!(s.stat_pass(&StdFs, now + Duration::from_secs(4)).is_empty());
        let changes = s.stat_pass(&StdFs, now + Duration::from_secs(6));
        assert_eq!(summary(&changes), one("modified", "a", "two"));

        std::fs::remove_file(&path).unwrap();
        assert!(s.stat_pass(&StdFs, now + Duration::from_secs(8)).is_empty());
        let changes = s.stat_pass(&StdFs, now + Duration::from_secs(10));
        assert_eq!(summary(&changes), one("removed", "a", ""));
    }

    #[test]
    fn own_writes_through_write_atomic_are_suppressed() {
        let (_dir, layout, mut s) = setup();
        let path = layout.jobs_dir().join("a.md");
        let now = SystemTime::now();
        assert!(s.startup_pass(&StdFs, now).len() == 1, "settings only");

        let before = stat_key(&StdFs, &path).unwrap();
        check_unchanged(&StdFs, &path, before).unwrap();
        write_atomic(&path, b"created", FileMode::Keep).unwrap();
        s.record_own_write(&StdFs, &path, b"created").unwrap();
        assert!(s.stat_pass(&StdFs, now + Duration::from_secs(2)).is_empty());
        assert!(s.hash_pass(&StdFs, now + Duration::from_secs(4)).is_empty());
    }

    /// The same-size rewrite with mtime restored from #46 Evidence 3: only ctime moves. Unix
    /// only: the Windows key has no ctime, and its hash pass covers this case.
    #[cfg(unix)]
    #[test]
    fn a_rewrite_with_mtime_restored_moves_the_unix_key() {
        let (_dir, layout, _s) = setup();
        let path = layout.jobs_dir().join("a.md");
        std::fs::write(&path, b"aaaa").unwrap();
        let before = StdFs.stat(&path).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        std::io::Write::write_all(&mut &file, b"bbbb").unwrap();
        file.set_modified(before.modified).unwrap();
        drop(file);
        let after = StdFs.stat(&path).unwrap();
        assert_eq!(
            (after.key.size, after.key.modified, after.key.ino),
            (before.key.size, before.key.modified, before.key.ino)
        );
        assert_ne!(after.key, before.key, "ctime is in the key");
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlinks_are_ignored_and_live_ones_followed() {
        let (dir, layout, mut s) = setup();
        let target = dir.path().join("elsewhere.md");
        std::fs::write(&target, b"linked").unwrap();
        std::os::unix::fs::symlink(&target, layout.jobs_dir().join("live.md")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("missing.md"),
            layout.jobs_dir().join("dangling.md"),
        )
        .unwrap();
        let now = SystemTime::now();
        s.stat_pass(&StdFs, now);
        let changes = s.stat_pass(&StdFs, now + Duration::from_secs(2));
        assert_eq!(summary(&changes), one("added", "live", "linked"));
    }

    /// #46 D9's editor patterns against the real filesystem: `a.md` starts as `"one"`, a save is
    /// done with passes 2 s apart in between its steps, then the scan settles (two stat passes
    /// and a hash pass). Each save must give exactly one modification and no removal.
    mod editor_patterns {
        use std::fs;
        use std::io::Write;

        use super::*;

        struct Run {
            _dir: tempfile::TempDir,
            layout: Layout,
            scanner: Scanner,
            now: SystemTime,
            changes: Vec<ScanChange>,
        }

        impl Run {
            fn start() -> Self {
                let (dir, layout, mut scanner) = setup();
                fs::write(layout.jobs_dir().join("a.md"), b"one").unwrap();
                let now = SystemTime::now();
                let start = scanner.startup_pass(&StdFs, now);
                assert_eq!(
                    summary(&start)[0],
                    ("added", "a".to_owned(), "one".to_owned())
                );
                Run {
                    _dir: dir,
                    layout,
                    scanner,
                    now,
                    changes: Vec::new(),
                }
            }

            fn job(&self) -> PathBuf {
                self.layout.jobs_dir().join("a.md")
            }

            fn pass(&mut self) {
                self.now += Duration::from_secs(2);
                let changes = self.scanner.stat_pass(&StdFs, self.now);
                self.changes.extend(changes);
            }

            fn settle(mut self) -> Vec<(&'static str, String, String)> {
                self.pass();
                self.pass();
                self.now += Duration::from_secs(2);
                let changes = self.scanner.hash_pass(&StdFs, self.now);
                self.changes.extend(changes);
                self.pass();
                summary(&self.changes)
            }
        }

        #[test]
        fn a_slow_in_place_truncate_and_write() {
            let mut run = Run::start();
            let mut file = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(run.job())
                .unwrap();
            file.write_all(b"first half, ").unwrap();
            file.sync_all().unwrap();
            run.pass(); // sees the truncated, half-written file
            file.write_all(b"second half").unwrap();
            drop(file);
            assert_eq!(
                run.settle(),
                one("modified", "a", "first half, second half")
            );
        }

        #[test]
        fn a_temp_file_renamed_over_it_like_sed_or_vim() {
            let mut run = Run::start();
            let temp = run.layout.jobs_dir().join(".!123!a.md");
            fs::write(&temp, b"sed save").unwrap();
            run.pass(); // the dot-prefixed temp file is not a job
            fs::rename(&temp, run.job()).unwrap();
            assert_eq!(run.settle(), one("modified", "a", "sed save"));
        }

        #[test]
        fn an_emacs_backup_rename_then_a_new_file() {
            let mut run = Run::start();
            let backup = run.layout.jobs_dir().join("a.md~");
            fs::rename(run.job(), &backup).unwrap();
            run.pass(); // missing once
            fs::write(run.job(), b"emacs save").unwrap();
            assert!(backup.exists());
            assert_eq!(
                run.settle(),
                one("modified", "a", "emacs save"),
                "`a.md~` is no job"
            );
        }

        #[test]
        fn a_git_checkout_like_unlink_and_create() {
            let run = Run::start();
            fs::remove_file(run.job()).unwrap();
            fs::write(run.job(), b"checked out").unwrap();
            assert_eq!(run.settle(), one("modified", "a", "checked out"));
        }

        #[test]
        fn a_move_out_and_back_is_no_change() {
            let mut run = Run::start();
            let outside = run.layout.project_dir().join("a.md.away");
            fs::rename(run.job(), &outside).unwrap();
            run.pass();
            fs::rename(&outside, run.job()).unwrap();
            assert_eq!(run.settle(), []);
        }

        #[test]
        fn a_touch_is_no_change() {
            let run = Run::start();
            let file = fs::OpenOptions::new().write(true).open(run.job()).unwrap();
            file.set_modified(SystemTime::now() + Duration::from_secs(60))
                .unwrap();
            drop(file);
            assert_eq!(run.settle(), []);
        }
    }
}
