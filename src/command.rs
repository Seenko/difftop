//! The `:` command line: parsing what was typed, resolving folders, and
//! Tab completion. No terminal code here; `ui` runs the results.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Watch another folder. The argument as typed; empty means home.
    Cd(String),
    Quit,
}

pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (name, arg) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(name, arg)| (name, arg.trim()));
    match name {
        // Everything after `cd` is the folder, so names with spaces need no quotes.
        "cd" => Ok(Command::Cd(arg.to_string())),
        "q" | "quit" => Ok(Command::Quit),
        _ => Err(format!("unknown command `{name}` (try `cd <folder>`)")),
    }
}

/// Turns a `cd` argument into an absolute, canonical folder, like a shell
/// would: empty or `~` is home, `-` is the previous folder, and relative
/// paths start from the current directory.
pub fn resolve(arg: &str, previous: Option<&Path>) -> Result<PathBuf, String> {
    let target = match arg {
        "-" => previous.ok_or("no previous folder")?.to_path_buf(),
        _ => expand(arg),
    };
    let dir = target
        .canonicalize()
        .map_err(|_| format!("no such folder: {arg}"))?;
    if !dir.is_dir() {
        return Err(format!("not a folder: {arg}"));
    }
    Ok(dir)
}

/// Completes the folder name at the end of a command line, as far as it is
/// unambiguous. Returns the new line, or `None` if nothing matches.
pub fn complete(line: &str) -> Option<String> {
    let arg = line.strip_prefix("cd ")?.trim_start();
    let (dir, prefix) = match arg.rfind('/') {
        Some(slash) => arg.split_at(slash + 1),
        None => ("", arg),
    };
    let base = if dir.is_empty() {
        PathBuf::from(".")
    } else {
        expand(dir)
    };
    let mut names: Vec<String> = fs::read_dir(base)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        // Hidden folders only when the user started typing a dot.
        .filter(|name| {
            name.starts_with(prefix) && (prefix.starts_with('.') || !name.starts_with('.'))
        })
        .collect();
    names.sort();
    let completed = match names.as_slice() {
        [] => return None,
        [only] => format!("{only}/"),
        [first, rest @ ..] => rest
            .iter()
            .fold(first.as_str(), |common, name| common_prefix(common, name))
            .to_string(),
    };
    Some(format!("cd {dir}{completed}"))
}

/// `~` and `~/...` point into the home folder; everything else is as typed.
fn expand(arg: &str) -> PathBuf {
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    };
    match arg {
        "" | "~" => home(),
        _ => match arg.strip_prefix("~/") {
            Some(rest) => home().join(rest),
            None => PathBuf::from(arg),
        },
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
    fn parses_commands() {
        assert_eq!(parse("cd  my folder "), Ok(Command::Cd("my folder".into())));
        assert_eq!(parse("cd"), Ok(Command::Cd(String::new())));
        assert_eq!(parse(" q"), Ok(Command::Quit));
        assert!(
            parse("rm -rf /")
                .unwrap_err()
                .contains("unknown command `rm`")
        );
    }

    #[test]
    fn resolves_like_a_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("file"), "").unwrap();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap())
            .canonicalize()
            .unwrap();

        assert_eq!(resolve(root.to_str().unwrap(), None), Ok(root.clone()));
        assert_eq!(resolve("", None), Ok(home.clone()));
        assert_eq!(resolve("~", None), Ok(home));
        assert_eq!(resolve("-", Some(&root)), Ok(root.clone()));
        assert!(resolve("-", None).is_err());
        assert!(
            resolve(root.join("file").to_str().unwrap(), None)
                .unwrap_err()
                .starts_with("not a folder")
        );
        assert!(
            resolve("/no/such/place", None)
                .unwrap_err()
                .starts_with("no such folder")
        );
    }

    #[test]
    fn completes_folder_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap().to_string();
        for dir in ["project-a", "project-b", "notes", ".hidden"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
        }
        fs::write(tmp.path().join("notes.txt"), "").unwrap();

        let complete_in = |typed: &str| complete(&format!("cd {root}/{typed}"));
        let expect = |rest: &str| Some(format!("cd {root}/{rest}"));
        assert_eq!(complete_in("no"), expect("notes/"));
        assert_eq!(complete_in("pro"), expect("project-"));
        assert_eq!(complete_in(".h"), expect(".hidden/"));
        assert_eq!(complete_in("zzz"), None);
        assert_eq!(complete("quit"), None);
    }
}
