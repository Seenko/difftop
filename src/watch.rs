//! Watches folders and reports what changed in each file.
//!
//! On startup every folder under the roots gets its own (non-recursive)
//! inotify watch and every small text file is read into memory, so the first
//! edit to a file can already be shown as a diff. Folders created later are
//! picked up as they appear. Ignored folders (`target/`, `node_modules/`, ...)
//! are never watched at all, which keeps us far away from the kernel's watch
//! limit on big trees.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::filter::Filter;

/// Files bigger than this are never diffed.
const MAX_FILE_SIZE: u64 = 1024 * 1024;
/// How much file content the startup scan may keep in memory.
// ponytail: files beyond the budget are learned on their first change
// (shown as FirstSeen); add eviction if memory ever matters more than that.
const STARTUP_BUDGET: usize = 256 * 1024 * 1024;
/// Events are collected until the folder has been quiet this long...
const QUIET: Duration = Duration::from_millis(50);
/// ...or this much time has passed, so a constantly written file still shows up.
const MAX_BATCH: Duration = Duration::from_millis(250);

pub struct Options {
    /// Absolute, canonical folder paths.
    pub roots: Vec<PathBuf>,
    pub hidden: bool,
    pub gitignore: bool,
}

#[derive(Debug)]
pub enum Event {
    /// The startup scan finished.
    Ready {
        files: usize,
    },
    Change(Change),
    /// Something the user should know about, e.g. hitting the watch limit.
    Warning(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// Content before the change; empty for `Created` and `FirstSeen`.
    pub before: String,
    /// Content after the change; empty for `Deleted`.
    pub after: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
    /// Changed, but we had no earlier copy to diff against.
    FirstSeen,
}

/// Runs forever on the calling thread, calling `emit` for everything that
/// happens. Only returns if the watcher can't be created.
pub fn run(options: Options, mut emit: impl FnMut(Event)) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut state = State {
        filter: Filter::new(options.roots.clone(), options.hidden, options.gitignore),
        watcher: notify::recommended_watcher(tx)?,
        watched: HashSet::new(),
        cache: HashMap::new(),
        cached_bytes: 0,
        warned: false,
    };
    for root in &options.roots {
        state.scan(root, false, &mut emit);
    }
    emit(Event::Ready {
        files: state.cache.len(),
    });

    while let Some(batch) = next_batch(&rx) {
        for (path, created) in batch {
            state.update(path, created, &mut emit);
        }
    }
    Ok(())
}

struct State {
    filter: Filter,
    watcher: RecommendedWatcher,
    watched: HashSet<PathBuf>,
    /// Last known content of every tracked file.
    cache: HashMap<PathBuf, String>,
    cached_bytes: usize,
    warned: bool,
}

impl State {
    /// Watches `dir` and everything below it. With `report`, files found are
    /// announced as created (a folder appeared at runtime); without, they are
    /// silently remembered (startup).
    fn scan(&mut self, dir: &Path, report: bool, emit: &mut impl FnMut(Event)) {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if !self.watched.insert(dir.clone()) {
                continue;
            }
            if let Err(err) = self.watcher.watch(&dir, RecursiveMode::NonRecursive) {
                self.warn_about(err, emit);
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // file_type() does not follow symlinks, so we never loop.
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() && !self.filter.is_ignored(&path, true) {
                    stack.push(path);
                } else if kind.is_file() && !self.filter.is_ignored(&path, false) {
                    if report {
                        self.update(path, true, emit);
                    } else if self.cached_bytes < STARTUP_BUDGET
                        && let Some(text) = read_text(&path)
                    {
                        self.cached_bytes += text.len();
                        self.cache.insert(path, text);
                    }
                }
            }
        }
    }

    /// Looks at `path` again and reports how it differs from what we knew.
    fn update(&mut self, path: PathBuf, created: bool, emit: &mut impl FnMut(Event)) {
        if path.file_name().is_some_and(|n| n == ".gitignore")
            && let Some(dir) = path.parent()
        {
            self.filter.forget(dir);
        }

        let Ok(meta) = fs::symlink_metadata(&path) else {
            self.watched.remove(&path);
            if let Some(before) = self.cache.remove(&path) {
                emit(change(path, ChangeKind::Deleted, before, String::new()));
            }
            return;
        };
        if meta.is_dir() {
            if !self.watched.contains(&path) && !self.filter.is_ignored(&path, true) {
                self.scan(&path, true, emit);
            }
            return;
        }
        if !meta.is_file() || self.filter.is_ignored(&path, false) {
            return;
        }
        let Some(after) = read_text(&path) else {
            return;
        };
        match self.cache.insert(path.clone(), after.clone()) {
            Some(before) if before == after => {}
            Some(before) => emit(change(path, ChangeKind::Modified, before, after)),
            None if created => emit(change(path, ChangeKind::Created, String::new(), after)),
            None => emit(change(path, ChangeKind::FirstSeen, String::new(), after)),
        }
    }

    fn warn_about(&mut self, err: notify::Error, emit: &mut impl FnMut(Event)) {
        let limit_hit = matches!(err.kind, notify::ErrorKind::MaxFilesWatch)
            || matches!(&err.kind, notify::ErrorKind::Io(e) if e.raw_os_error() == Some(28));
        // Other failures (a folder vanished or isn't readable) aren't worth a
        // message: there is nothing to watch there anyway.
        if limit_hit && !self.warned {
            self.warned = true;
            emit(Event::Warning(
                "inotify watch limit reached, some folders aren't watched \
                 (raise fs.inotify.max_user_watches)"
                    .into(),
            ));
        }
    }
}

fn change(path: PathBuf, kind: ChangeKind, before: String, after: String) -> Event {
    Event::Change(Change {
        path,
        kind,
        before,
        after,
    })
}

/// The file's content if it is small, valid UTF-8 and not binary.
fn read_text(path: &Path) -> Option<String> {
    if fs::metadata(path).ok()?.len() > MAX_FILE_SIZE {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Waits for the next burst of events and returns the paths it touched, each
/// flagged with whether it was created (or renamed into place) during the burst.
/// Returns `None` once the watcher is gone.
fn next_batch(rx: &Receiver<notify::Result<notify::Event>>) -> Option<BTreeMap<PathBuf, bool>> {
    let mut batch = BTreeMap::new();
    let mut add = |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        // Access events fire when *we* read a file; following them would loop.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let created = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        for path in event.paths {
            *batch.entry(path).or_insert(false) |= created;
        }
    };

    add(rx.recv().ok()?);
    let deadline = Instant::now() + MAX_BATCH;
    loop {
        let wait = QUIET.min(deadline.saturating_duration_since(Instant::now()));
        match rx.recv_timeout(wait) {
            Ok(event) => add(event),
            Err(_) => return Some(batch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;
    use std::thread;

    /// Starts a real watcher on a temp folder and returns its event stream
    /// once the startup scan is done.
    fn start(root: &Path) -> Receiver<Event> {
        let (tx, rx) = mpsc::channel();
        let options = Options {
            roots: vec![root.to_path_buf()],
            hidden: false,
            gitignore: true,
        };
        thread::spawn(move || run(options, |e| tx.send(e).unwrap()));
        assert!(matches!(next(&rx), Event::Ready { .. }));
        rx
    }

    fn next(rx: &Receiver<Event>) -> Event {
        rx.recv_timeout(Duration::from_secs(5))
            .expect("no event within 5s")
    }

    fn next_change(rx: &Receiver<Event>) -> Change {
        match next(rx) {
            Event::Change(change) => change,
            other => panic!("expected a change, got {other:?}"),
        }
    }

    #[test]
    fn reports_edits_creations_and_deletions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("a.txt"), "one\n").unwrap();
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        let rx = start(&root);

        fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        let edit = next_change(&rx);
        assert_eq!(edit.kind, ChangeKind::Modified);
        assert_eq!(
            (edit.before.as_str(), edit.after.as_str()),
            ("one\n", "one\ntwo\n")
        );

        // Ignored and binary files stay silent; the next event is the new file.
        fs::write(root.join("noise.log"), "x\n").unwrap();
        fs::write(root.join("image.bin"), [0u8, 1, 2]).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/new.rs"), "fn main() {}\n").unwrap();
        let created = next_change(&rx);
        assert_eq!(created.kind, ChangeKind::Created);
        assert_eq!(created.path, root.join("src/new.rs"));

        fs::remove_file(root.join("a.txt")).unwrap();
        let deleted = next_change(&rx);
        assert_eq!(deleted.kind, ChangeKind::Deleted);
        assert_eq!(deleted.before, "one\ntwo\n");
    }
}
