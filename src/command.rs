//! Tab completion for the `:` prompt. Running what's typed is `shell`'s job.

use std::fs;
use std::path::PathBuf;

/// Completes the file or folder name at the end of `line`, as far as it is
/// unambiguous, and returns the new line. After `cd`, only folders count.
/// Folders get a trailing `/`, so the next Tab goes inside.
pub fn complete(line: &str) -> Option<String> {
    let (head, word) = match line.rfind(' ') {
        Some(space) => line.split_at(space + 1),
        None => ("", line),
    };
    let folders_only = head.trim() == "cd";
    let (dir, prefix) = match word.rfind('/') {
        Some(slash) => word.split_at(slash + 1),
        None => ("", word),
    };
    let base = if dir.is_empty() {
        PathBuf::from(".")
    } else {
        expand(dir)
    };
    let mut matches: Vec<(String, bool)> = fs::read_dir(base)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let is_dir = entry.path().is_dir();
            let name = entry.file_name().into_string().ok()?;
            // Hidden entries only when the user started typing a dot.
            let visible = prefix.starts_with('.') || !name.starts_with('.');
            (name.starts_with(prefix) && visible && (is_dir || !folders_only))
                .then_some((name, is_dir))
        })
        .collect();
    matches.sort();
    let completed = match matches.as_slice() {
        [] => return None,
        [(name, true)] => format!("{name}/"),
        [(name, false)] => format!("{name} "),
        [(first, _), rest @ ..] => rest
            .iter()
            .fold(first.as_str(), |common, (name, _)| {
                common_prefix(common, name)
            })
            .to_string(),
    };
    Some(format!("{head}{dir}{completed}"))
}

/// `~/...` points into the home folder; everything else is as typed.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest),
        None => PathBuf::from(path),
    }
}

fn common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let len = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    &a[..len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_files_and_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap().to_string();
        for dir in ["project-a", "project-b", "notes", ".hidden"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
        }
        fs::write(tmp.path().join("readme.md"), "").unwrap();
        fs::write(tmp.path().join("notes.txt"), "").unwrap();

        let complete_in = |cmd: &str, typed: &str| complete(&format!("{cmd}{root}/{typed}"));
        let expect = |cmd: &str, rest: &str| Some(format!("{cmd}{root}/{rest}"));
        assert_eq!(
            complete_in("cd ", "no"),
            expect("cd ", "notes/"),
            "cd skips files"
        );
        assert_eq!(
            complete_in("cat ", "no"),
            expect("cat ", "notes"),
            "ambiguous"
        );
        assert_eq!(complete_in("cat ", "re"), expect("cat ", "readme.md "));
        assert_eq!(complete_in("ls ", "pro"), expect("ls ", "project-"));
        assert_eq!(complete_in("ls ", ".h"), expect("ls ", ".hidden/"));
        assert_eq!(complete_in("ls ", "zzz"), None);
    }
}
