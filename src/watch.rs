//! Watches folders and reports what changed in each file.
//!
//! On startup every folder under the roots gets its own (non-recursive)
//! inotify watch and every small text file is read into memory, so the first
//! edit to a file can already be shown as a diff. Folders created later are
//! picked up as they appear. Ignored folders (`target/`, `node_modules/`, ...)
//! are never watched at all, which keeps us far away from the kernel's watch
//! limit on big trees.
//!
//! The watcher runs on its own thread ([`spawn`]). File events and requests
//! to watch other folders ([`Handle::watch`]) arrive on the same channel, so
//! the thread only ever waits in one place.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::filter::Filter;

/// Files bigger than this are tracked by size instead of diffed.
const MAX_FILE_SIZE: u64 = 1024 * 1024;
/// How much file text is kept in memory for diffing, in total. The most
/// recently modified files get it first; a file without its text still shows
/// up when it changes (as a size change) and is diffed from then on.
const TEXT_BUDGET: usize = 16 * 1024 * 1024;
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
    /// The file before the change; empty for `Created`.
    pub before: Snapshot,
    /// The file after the change; empty for `Deleted`.
    pub after: Snapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
}

/// What we know about a file at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub size: u64,
    /// The content, for files that can be diffed: valid UTF-8, no NUL bytes,
    /// at most [`MAX_FILE_SIZE`]. Other files are tracked by size and time.
    pub text: Option<String>,
    pub modified: Option<SystemTime>,
}

impl Snapshot {
    /// Reads the file's metadata, and its content too if `read` is set.
    fn take(path: &Path, meta: &Metadata, read: bool) -> Self {
        Self {
            size: meta.len(),
            text: read.then(|| read_text(path, meta.len())).flatten(),
            modified: meta.modified().ok(),
        }
    }

    /// Text is compared by content, so saving a file unchanged is no change.
    /// Anything else falls back to size and modification time.
    fn same_as(&self, other: &Self) -> bool {
        match (&self.text, &other.text) {
            (Some(a), Some(b)) => a == b,
            _ => self.size == other.size && self.modified == other.modified,
        }
    }
}

/// Everything the watcher thread waits for, on a single channel.
enum Input {
    Fs(notify::Result<notify::Event>),
    /// Stop watching the current folders and watch these instead.
    Watch(Vec<PathBuf>),
}

/// Lets other threads steer a running watcher.
pub struct Handle(Sender<Input>);

impl Handle {
    /// Switches to watching `roots` (absolute, canonical folder paths). A
    /// fresh scan follows, finishing with another [`Event::Ready`].
    pub fn watch(&self, roots: Vec<PathBuf>) {
        let _ = self.0.send(Input::Watch(roots));
    }
}

/// Starts the watcher thread, which calls `emit` for everything that happens.
pub fn spawn(options: Options, emit: impl FnMut(Event) + Send + 'static) -> Handle {
    let (tx, rx) = mpsc::channel();
    let handle = Handle(tx.clone());
    thread::spawn(move || run(options, tx, rx, emit));
    handle
}

fn run(mut options: Options, tx: Sender<Input>, rx: Receiver<Input>, mut emit: impl FnMut(Event)) {
    let mut state = match State::start(&options, &tx, &mut emit) {
        Ok(state) => state,
        Err(err) => return emit(Event::Warning(format!("can't watch: {err}"))),
    };
    while let Some(batch) = next_batch(&rx) {
        match batch {
            Batch::Paths(paths) => {
                for path in paths {
                    state.update(path, &mut emit);
                }
            }
            Batch::Watch(roots) => {
                options.roots = roots;
                // Dropping the old state drops its watcher, which removes
                // every old watch. Events from them still in the channel
                // are outside the new roots, so the filter ignores them.
                match State::start(&options, &tx, &mut emit) {
                    Ok(new) => state = new,
                    Err(err) => emit(Event::Warning(format!("can't watch: {err}"))),
                }
            }
        }
    }
}

struct State {
    filter: Filter,
    watcher: RecommendedWatcher,
    watched: HashSet<PathBuf>,
    /// What we last saw of every file.
    cache: HashMap<PathBuf, Snapshot>,
    cached_bytes: usize,
    warned: bool,
}

impl State {
    /// Creates a watcher for `options.roots`, scans them, and reports Ready.
    fn start(options: &Options, tx: &Sender<Input>, emit: &mut impl FnMut(Event)) -> Result<Self> {
        let tx = tx.clone();
        let mut state = State {
            filter: Filter::new(options.roots.clone(), options.hidden, options.gitignore),
            watcher: notify::recommended_watcher(move |event| {
                let _ = tx.send(Input::Fs(event));
            })?,
            watched: HashSet::new(),
            cache: HashMap::new(),
            cached_bytes: 0,
            warned: false,
        };
        for root in &options.roots {
            state.scan(root, false, emit);
        }
        emit(Event::Ready {
            files: state.cache.len(),
        });
        Ok(state)
    }

    /// Watches `dir` and everything below it. With `report`, files found are
    /// announced as created (a folder appeared at runtime); without, they are
    /// silently remembered (startup).
    fn scan(&mut self, dir: &Path, report: bool, emit: &mut impl FnMut(Event)) {
        let mut found = Vec::new();
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
                        self.update(path, emit);
                    } else if let Ok(meta) = entry.metadata() {
                        found.push((path, meta));
                    }
                }
            }
        }

        // Newest first: the files most likely to be edited next get their
        // text remembered while the budget lasts.
        found.sort_by_key(|(_, meta)| std::cmp::Reverse(meta.modified().ok()));
        for (path, meta) in found {
            let snapshot = Snapshot::take(&path, &meta, self.cached_bytes < TEXT_BUDGET);
            self.remember(path, snapshot);
        }
    }

    /// Looks at `path` again and reports how it differs from what we knew.
    fn update(&mut self, path: PathBuf, emit: &mut impl FnMut(Event)) {
        if path.file_name().is_some_and(|n| n == ".gitignore")
            && let Some(dir) = path.parent()
        {
            self.filter.forget(dir);
        }

        let Ok(meta) = fs::symlink_metadata(&path) else {
            self.watched.remove(&path);
            if let Some(before) = self.forget(&path) {
                emit(change(
                    path,
                    ChangeKind::Deleted,
                    before,
                    Snapshot::default(),
                ));
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
        let after = Snapshot::take(&path, &meta, true);
        match self.remember(path.clone(), after.clone()) {
            Some(before) if before.same_as(&after) => {}
            Some(before) => emit(change(path, ChangeKind::Modified, before, after)),
            None => emit(change(
                path,
                ChangeKind::Created,
                Snapshot::default(),
                after,
            )),
        }
    }

    /// Stores what we know about a file, keeping the text within budget.
    /// Returns what was known before.
    fn remember(&mut self, path: PathBuf, snapshot: Snapshot) -> Option<Snapshot> {
        self.cached_bytes += text_len(&snapshot);
        let old = self.cache.insert(path, snapshot);
        self.cached_bytes -= old.as_ref().map_or(0, text_len);
        if self.cached_bytes > TEXT_BUDGET {
            self.evict();
        }
        old
    }

    fn forget(&mut self, path: &Path) -> Option<Snapshot> {
        let old = self.cache.remove(path);
        self.cached_bytes -= old.as_ref().map_or(0, text_len);
        old
    }

    /// Drops the text of the files that changed longest ago until the cache
    /// is comfortably under budget. Their size and time are kept.
    // ponytail: sorts every cached file per eviction; evictions are rare
    // since each frees a quarter of the budget.
    fn evict(&mut self) {
        let mut oldest: Vec<(Option<SystemTime>, PathBuf)> = self
            .cache
            .iter()
            .filter(|(_, s)| s.text.is_some())
            .map(|(path, s)| (s.modified, path.clone()))
            .collect();
        oldest.sort();
        for (_, path) in oldest {
            if self.cached_bytes <= TEXT_BUDGET / 4 * 3 {
                break;
            }
            if let Some(text) = self.cache.get_mut(&path).and_then(|s| s.text.take()) {
                self.cached_bytes -= text.len();
            }
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

fn change(path: PathBuf, kind: ChangeKind, before: Snapshot, after: Snapshot) -> Event {
    Event::Change(Change {
        path,
        kind,
        before,
        after,
    })
}

fn text_len(snapshot: &Snapshot) -> usize {
    snapshot.text.as_ref().map_or(0, String::len)
}

/// The file's content if it is small, valid UTF-8 and not binary.
fn read_text(path: &Path, size: u64) -> Option<String> {
    if size > MAX_FILE_SIZE {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

enum Batch {
    Paths(BTreeSet<PathBuf>),
    Watch(Vec<PathBuf>),
}

/// Waits for the next burst of file events and returns the paths it touched,
/// or a request to watch other folders, which cuts any burst short.
/// Returns `None` once every sender is gone.
fn next_batch(rx: &Receiver<Input>) -> Option<Batch> {
    let mut paths = BTreeSet::new();
    let mut wait = None;
    let deadline = Instant::now() + MAX_BATCH;
    loop {
        let input = match wait {
            None => rx.recv().ok()?,
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(input) => input,
                Err(_) => return Some(Batch::Paths(paths)),
            },
        };
        match input {
            Input::Watch(roots) => return Some(Batch::Watch(roots)),
            // Access events fire when *we* read a file; following them would loop.
            Input::Fs(Ok(event)) if !matches!(event.kind, EventKind::Access(_)) => {
                paths.extend(event.paths);
            }
            Input::Fs(_) => {}
        }
        wait = Some(QUIET.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    /// Starts a real watcher on a temp folder and returns its event stream
    /// once the startup scan is done.
    fn start(root: &Path) -> (Handle, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let options = Options {
            roots: vec![root.to_path_buf()],
            hidden: false,
            gitignore: true,
        };
        let handle = spawn(options, move |e| {
            let _ = tx.send(e);
        });
        assert!(matches!(next(&rx), Event::Ready { .. }));
        (handle, rx)
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
        let (_handle, rx) = start(&root);

        fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        let edit = next_change(&rx);
        assert_eq!(edit.kind, ChangeKind::Modified);
        assert_eq!(edit.before.text.as_deref(), Some("one\n"));
        assert_eq!(edit.after.text.as_deref(), Some("one\ntwo\n"));

        // Ignored files stay silent; binary ones are reported by size.
        fs::write(root.join("noise.log"), "x\n").unwrap();
        fs::write(root.join("image.bin"), [0u8, 1, 2]).unwrap();
        let binary = next_change(&rx);
        assert_eq!(binary.path, root.join("image.bin"));
        assert_eq!((binary.kind, binary.after.size), (ChangeKind::Created, 3));
        assert_eq!(binary.after.text, None);

        fs::write(root.join("image.bin"), [0u8, 1, 2, 3, 4]).unwrap();
        let grown = next_change(&rx);
        assert_eq!((grown.before.size, grown.after.size), (3, 5));

        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/new.rs"), "fn main() {}\n").unwrap();
        let created = next_change(&rx);
        assert_eq!(created.kind, ChangeKind::Created);
        assert_eq!(created.path, root.join("src/new.rs"));

        fs::remove_file(root.join("a.txt")).unwrap();
        let deleted = next_change(&rx);
        assert_eq!(deleted.kind, ChangeKind::Deleted);
        assert_eq!(deleted.before.text.as_deref(), Some("one\ntwo\n"));
    }

    #[test]
    fn switching_folders_moves_the_watch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (old, new) = (root.join("old"), root.join("new"));
        fs::create_dir(&old).unwrap();
        fs::create_dir(&new).unwrap();
        fs::write(new.join("b.txt"), "b\n").unwrap();
        let (handle, rx) = start(&old);

        handle.watch(vec![new.clone()]);
        assert!(matches!(next(&rx), Event::Ready { files: 1 }));

        // The old folder is silent now; the new one is watched and was
        // scanned, so its first edit is already a real diff.
        fs::write(old.join("a.txt"), "a\n").unwrap();
        fs::write(new.join("b.txt"), "b\nB\n").unwrap();
        let change = next_change(&rx);
        assert_eq!(change.path, new.join("b.txt"));
        assert_eq!(change.kind, ChangeKind::Modified);
        assert!(rx.recv_timeout(Duration::from_millis(400)).is_err());
    }

    #[test]
    fn text_budget_drops_the_oldest_texts_first() {
        let tmp = tempfile::tempdir().unwrap();
        let options = Options {
            roots: vec![tmp.path().to_path_buf()],
            hidden: false,
            gitignore: true,
        };
        let (tx, _rx) = mpsc::channel();
        let mut state = State::start(&options, &tx, &mut |_| {}).unwrap();
        let six_mib = "x".repeat(6 * 1024 * 1024);
        let file = |age_secs: u64| Snapshot {
            size: six_mib.len() as u64,
            text: Some(six_mib.clone()),
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 - age_secs)),
        };
        state.remember("old".into(), file(30));
        state.remember("mid".into(), file(20));
        state.remember("new".into(), file(10)); // 18 MiB: over budget

        let has_text = |name: &str| state.cache[Path::new(name)].text.is_some();
        assert!(!has_text("old") && has_text("mid") && has_text("new"));
        assert_eq!(state.cache[Path::new("old")].size, six_mib.len() as u64);
        assert_eq!(state.cached_bytes, 12 * 1024 * 1024);
        state.forget(Path::new("new"));
        assert_eq!(state.cached_bytes, 6 * 1024 * 1024);
    }
}
