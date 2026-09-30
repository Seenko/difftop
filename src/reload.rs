//! Hot reload: when the difftop binary on disk is replaced (a new build, a
//! `cargo install`, an update), the running difftop swaps itself for the new
//! one, keeping everything on screen.
//!
//! How: [`Upgrade`] notices the file changing. The UI then calls [`exec`],
//! which saves the screen's state to a private temp file and replaces this
//! process with the new binary via `exec`, keeping the same PID and terminal.
//! The new process finds the file through an environment variable and picks
//! up where the old one stopped ([`take`]).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};

/// How often the binary on disk is looked at.
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// Tells a freshly started difftop where the saved state is.
const RESUME_VAR: &str = "DIFFTOP_RESUME";

/// Everything that survives a reload. Fields default when missing, so a
/// newer difftop can still read what an older one saved.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub roots: Vec<PathBuf>,
    pub previous: Option<PathBuf>,
    pub speed: f64,
    pub paused: bool,
    /// Lines already on screen (and scrolled past), oldest first.
    pub shown: Vec<SavedLine>,
    /// Lines still waiting to rise.
    pub pending: Vec<SavedLine>,
    /// Lines below a scrolled-back view; 0 when following.
    pub below: usize,
}

/// A [`Line`] in a form that can be written to disk.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedLine {
    style: Style,
    spans: Vec<(String, Style)>,
}

impl From<&Line<'_>> for SavedLine {
    fn from(line: &Line) -> Self {
        Self {
            style: line.style,
            spans: line
                .spans
                .iter()
                .map(|span| (span.content.to_string(), span.style))
                .collect(),
        }
    }
}

impl From<SavedLine> for Line<'static> {
    fn from(saved: SavedLine) -> Self {
        let spans: Vec<Span> = saved
            .spans
            .into_iter()
            .map(|(text, style)| Span::styled(text, style))
            .collect();
        Line::from(spans).style(saved.style)
    }
}

/// Identifies one version of a file: a replaced file gets a new inode, and
/// size and modification time catch the rest.
type Stamp = (u64, u64, Option<SystemTime>);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.ino(), meta.len(), meta.modified().ok()))
}

/// Watches the running binary's file for a replacement.
pub struct Upgrade {
    path: PathBuf,
    /// The version that's running (or one we decided to ignore).
    current: Option<Stamp>,
    /// A new version seen once, waiting to prove it's done being written.
    candidate: Option<Stamp>,
    next_check: Instant,
}

impl Upgrade {
    /// Starts watching the binary this process was started from.
    pub fn for_this_binary() -> Option<Self> {
        Some(Self::for_path(std::env::current_exe().ok()?))
    }

    fn for_path(path: PathBuf) -> Self {
        Self {
            current: stamp(&path),
            candidate: None,
            path,
            next_check: Instant::now() + CHECK_EVERY,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Cheap to call every frame: looks at the disk at most once a second.
    pub fn ready(&mut self) -> bool {
        if Instant::now() < self.next_check {
            return false;
        }
        self.next_check = Instant::now() + CHECK_EVERY;
        self.check()
    }

    /// True once the file has been replaced, has then stayed the same for
    /// one check (so it's not still being copied), and actually runs.
    fn check(&mut self) -> bool {
        let now = stamp(&self.path);
        if now.is_none() || now == self.current {
            self.candidate = None;
            return false;
        }
        if now != self.candidate {
            self.candidate = now;
            return false;
        }
        let works = Command::new(&self.path)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|out| out.status.success());
        if !works {
            // Don't keep trying a broken build; wait for the next one.
            self.current = now;
        }
        works
    }
}

/// Saves `saved` and replaces this process with the binary at `path`, run
/// with the same arguments. Only returns if that failed.
pub fn exec(path: &Path, saved: &Saved) -> io::Error {
    let file = match write_state(saved) {
        Ok(file) => file,
        Err(err) => return err,
    };
    let err = Command::new(path)
        .args(std::env::args_os().skip(1))
        .env(RESUME_VAR, &file)
        .exec();
    let _ = fs::remove_file(&file);
    err
}

/// The state a previous difftop handed over, if this process is a reload.
/// The file is deleted once read.
pub fn take() -> Option<Saved> {
    let file = std::env::var_os(RESUME_VAR)?;
    let bytes = fs::read(&file);
    let _ = fs::remove_file(&file);
    serde_json::from_slice(&bytes.ok()?).ok()
}

/// Writes the state to a new file only this user can read. `create_new`
/// refuses to follow a planted symlink or reuse an existing file.
fn write_state(saved: &Saved) -> io::Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = dir.join(format!("difftop-{}-{nanos}.json", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&serde_json::to_vec(saved)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn lines_survive_the_trip_to_disk() {
        let line = Line::from(vec![
            Span::styled("+ ", Style::new().fg(Color::Indexed(2))),
            Span::styled("let x = 1;", Style::new().fg(Color::Rgb(1, 2, 3))),
        ])
        .style(Style::new().add_modifier(Modifier::DIM));
        let saved = Saved {
            roots: vec!["/tmp".into()],
            speed: 42.0,
            shown: vec![SavedLine::from(&line)],
            below: 3,
            ..Saved::default()
        };
        let json = serde_json::to_vec(&saved).unwrap();
        let mut back: Saved = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, saved);
        assert_eq!(Line::from(back.shown.remove(0)), line);

        // Unknown or missing fields don't break older/newer versions.
        let partial: Saved = serde_json::from_str(r#"{"speed": 5.0, "future": 1}"#).unwrap();
        assert_eq!((partial.speed, partial.shown.len()), (5.0, 0));
    }

    #[test]
    fn a_replaced_binary_is_picked_up_once_it_settles_and_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("difftop");
        let install = |script: &str| {
            // Write elsewhere and rename, like `install` and `cargo install` do.
            let staged = tmp.path().join("staged");
            fs::write(&staged, script).unwrap();
            fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
            fs::rename(&staged, &bin).unwrap();
        };
        install("#!/bin/sh\nexit 0\n");
        let mut upgrade = Upgrade::for_path(bin.clone());
        assert!(!upgrade.check(), "nothing changed yet");

        install("#!/bin/sh\nexit 1\n");
        assert!(!upgrade.check(), "first sighting only makes it a candidate");
        assert!(!upgrade.check(), "a build that fails --version is skipped");
        assert!(!upgrade.check(), "and not retried");

        install("#!/bin/sh\nexit 0\n");
        assert!(!upgrade.check());
        assert!(upgrade.check(), "settled and working");
    }
}
