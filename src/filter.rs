//! Decides which paths difftop cares about.
//!
//! The same rules apply to the startup scan and to live events, so a file
//! that was skipped at startup never shows up later and vice versa.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ignore::Match;
use ignore::gitignore::Gitignore;

pub struct Filter {
    roots: Vec<PathBuf>,
    hidden: bool,
    gitignore: bool,
    global: Gitignore,
    /// One matcher per directory, built from that directory's `.gitignore`
    /// (empty when there is none). Dropped via [`Filter::forget`] when the
    /// file changes.
    per_dir: HashMap<PathBuf, Gitignore>,
}

impl Filter {
    /// `roots` must be absolute; they are always watched, even if hidden.
    pub fn new(roots: Vec<PathBuf>, hidden: bool, gitignore: bool) -> Self {
        let global = if gitignore {
            Gitignore::global().0
        } else {
            Gitignore::empty()
        };
        Self {
            roots,
            hidden,
            gitignore,
            global,
            per_dir: HashMap::new(),
        }
    }

    /// True if `path` should be neither watched nor diffed.
    pub fn is_ignored(&mut self, path: &Path, is_dir: bool) -> bool {
        let Some(root) = self.root_of(path).map(Path::to_path_buf) else {
            return true;
        };
        let relative = path.strip_prefix(&root).unwrap_or(path);
        for part in relative.components() {
            let name = part.as_os_str().to_string_lossy();
            if name == ".git" || (!self.hidden && name.starts_with('.')) {
                return true;
            }
        }
        if !self.gitignore {
            return false;
        }

        // Nearest .gitignore wins, like git: walk from the parent up to the root.
        let mut dir = path.parent();
        while let Some(d) = dir.filter(|d| d.starts_with(&root)) {
            match self.matcher(d).matched_path_or_any_parents(path, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => dir = d.parent(),
            }
        }
        self.global.matched(path, is_dir).is_ignore()
    }

    /// Drop the cached rules for `dir`, e.g. after its `.gitignore` changed.
    pub fn forget(&mut self, dir: &Path) {
        self.per_dir.remove(dir);
    }

    /// The watched root that contains `path` (the deepest one if they nest).
    fn root_of(&self, path: &Path) -> Option<&Path> {
        self.roots
            .iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.as_os_str().len())
            .map(PathBuf::as_path)
    }

    fn matcher(&mut self, dir: &Path) -> &Gitignore {
        self.per_dir
            .entry(dir.to_path_buf())
            .or_insert_with(|| Gitignore::new(dir.join(".gitignore")).0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n!keep.log\n").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/.gitignore"), "local.txt\n").unwrap();
        (tmp, root)
    }

    #[test]
    fn applies_gitignore_rules() {
        let (_tmp, root) = setup();
        let mut f = Filter::new(vec![root.clone()], false, true);
        assert!(f.is_ignored(&root.join("target"), true));
        assert!(f.is_ignored(&root.join("target/debug/x.rs"), false));
        assert!(f.is_ignored(&root.join("app.log"), false));
        assert!(!f.is_ignored(&root.join("keep.log"), false));
        assert!(f.is_ignored(&root.join("sub/local.txt"), false));
        assert!(!f.is_ignored(&root.join("local.txt"), false));
        assert!(!f.is_ignored(&root.join("src/main.rs"), false));
    }

    #[test]
    fn hidden_and_git_are_skipped() {
        let (_tmp, root) = setup();
        let mut f = Filter::new(vec![root.clone()], false, true);
        assert!(f.is_ignored(&root.join(".env"), false));
        assert!(f.is_ignored(&root.join(".git/HEAD"), false));

        let mut f = Filter::new(vec![root.clone()], true, true);
        assert!(!f.is_ignored(&root.join(".env"), false));
        assert!(f.is_ignored(&root.join(".git/HEAD"), false));
    }

    #[test]
    fn no_ignore_and_outside_roots() {
        let (_tmp, root) = setup();
        let mut f = Filter::new(vec![root.clone()], false, false);
        assert!(!f.is_ignored(&root.join("app.log"), false));
        assert!(f.is_ignored(Path::new("/somewhere/else.rs"), false));
    }
}
