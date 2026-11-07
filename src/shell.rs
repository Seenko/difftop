//! The shell behind the `:` prompt.
//!
//! Loading a user's shell config can take a quarter of a second (nvm,
//! bash-completion...), so difftop keeps one shell *warm*: [`Shell::start`]
//! launches it ahead of time, it loads its config, reports its prompt, and
//! waits for one command line. [`Shell::run`] hands it the line, which then
//! runs at once. After it finishes, the caller starts the next warm shell.
//!
//! The shell is the user's own (bash or zsh), interactive so their aliases
//! and settings apply, on a pseudo-terminal (PTY) the size of difftop's
//! screen: programs see a real terminal, so colors and columns come out as
//! in the user's terminal, and Ctrl-C is a real Ctrl-C.
//!
//! After the command, the shell writes its working directory to a file we
//! name. That's what makes `cd` work anywhere in the line (`cd src && ls`).

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use rustix::fs::{Mode, OFlags};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, LocalModes, OptionalActions, Winsize};

/// Printed by the shell once it has loaded and is waiting for a command.
const READY: &str = "\x1b]6973;ready\x07";

/// Matches at the end of the line because shell config that prints without a
/// final newline (fastfetch ends on a color reset) leaves text in front of it.
fn is_ready(line: &str) -> bool {
    line.ends_with(READY)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Output {
    /// The shell is loaded and waiting; this is its prompt, color codes and all.
    Ready(String),
    /// One line of what the command printed, color codes included.
    Line(String),
    /// The shell exited. `code` is `None` if it was killed; `cwd` is where it
    /// ended up, if a command ran to the end.
    Done {
        code: Option<i32>,
        cwd: Option<PathBuf>,
    },
}

/// A warm shell: waiting for a command, or running the one it was given.
pub struct Shell {
    /// Our end of its terminal.
    pty: File,
}

impl Shell {
    /// Starts a shell in `cwd` on a terminal of `size` (columns, rows) and
    /// returns right away; `on_output` is called from a background thread,
    /// first with [`Output::Ready`], then for the command's output, and once
    /// with [`Output::Done`]. `oldpwd` is where `cd -` goes.
    pub fn start(
        cwd: &Path,
        oldpwd: Option<&Path>,
        size: (u16, u16),
        mut on_output: impl FnMut(Output) + Send + 'static,
    ) -> io::Result<Self> {
        // A fresh pseudo-terminal: we keep the "master" end, the shell gets the other.
        let pty = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
        pty::grantpt(&pty)?;
        pty::unlockpt(&pty)?;
        // No echo: the command line we hand over shouldn't be printed back.
        let mut modes = termios::tcgetattr(&pty)?;
        modes.local_modes -= LocalModes::ECHO;
        termios::tcsetattr(&pty, OptionalActions::Now, &modes)?;
        let name = pty::ptsname(&pty, Vec::new())?;
        let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
        let terminal = File::from(rustix::fs::open(name.as_c_str(), flags, Mode::empty())?);
        let pty = File::from(pty);
        let shell = Self { pty };
        shell.resize(size)?;

        let prompt_file = crate::reload::private_path("prompt");
        let pwd_file = crate::reload::private_path("pwd");
        let (program, script) = script();
        // `setsid --ctty` gives the shell its own session with this terminal as
        // its controlling terminal, which interactive shells need for job
        // control (and Ctrl-C) to work, and keeps it away from ours.
        let mut command = Command::new("setsid");
        command
            .args(["--ctty", program, "-ic", script])
            .current_dir(cwd)
            .env("DIFFTOP_PROMPT_FILE", &prompt_file)
            .env("DIFFTOP_PWD_FILE", &pwd_file)
            .stdin(terminal.try_clone()?)
            .stdout(terminal.try_clone()?)
            .stderr(terminal);
        if let Some(oldpwd) = oldpwd {
            command.env("OLDPWD", oldpwd);
        }
        let mut child = command.spawn()?;
        // Drop our copies of the shell's end, or reading would never end.
        drop(command);

        let mut reader = BufReader::new(shell.pty.try_clone()?);
        thread::spawn(move || {
            let mut ready = false;
            let mut buf = Vec::new();
            // Reading fails (EIO) once the shell's side is closed: that's the end.
            while reader.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
                let text = String::from_utf8_lossy(&buf);
                let line = text.trim_end_matches(['\n', '\r']);
                if is_ready(line) {
                    ready = true;
                    let prompt = std::fs::read_to_string(&prompt_file).unwrap_or_default();
                    on_output(Output::Ready(prompt));
                } else if ready {
                    // Anything the shell's config prints while loading is dropped.
                    on_output(Output::Line(line.to_string()));
                }
                buf.clear();
            }
            let code = child.wait().ok().and_then(|status| status.code());
            let cwd = std::fs::read_to_string(&pwd_file)
                .ok()
                .map(|text| PathBuf::from(text.trim_end_matches('\n')));
            let _ = std::fs::remove_file(&prompt_file);
            let _ = std::fs::remove_file(&pwd_file);
            on_output(Output::Done { code, cwd });
        });
        Ok(shell)
    }

    /// Runs `line`, at once if the shell is ready, as soon as it is otherwise.
    /// `size` is the terminal size to run it at.
    pub fn run(&self, line: &str, size: (u16, u16)) -> io::Result<()> {
        self.resize(size)?;
        (&self.pty).write_all(format!("{line}\n").as_bytes())
    }

    /// Types Ctrl-C into the shell's terminal, which interrupts the command
    /// exactly like in a real terminal.
    pub fn interrupt(&self) {
        let _ = (&self.pty).write_all(b"\x03");
    }

    fn resize(&self, (cols, rows): (u16, u16)) -> io::Result<()> {
        let size = Winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        Ok(termios::tcsetwinsize(&self.pty, size)?)
    }
}

/// The user's shell if we know how to ask it for its prompt, else bash, and
/// the script it runs: load, report the prompt, wait for one line, run it,
/// note where it ended up.
fn script() -> (&'static str, &'static str) {
    let zsh = std::env::var("SHELL").is_ok_and(|shell| {
        Path::new(&shell)
            .file_name()
            .is_some_and(|name| name == "zsh")
    });
    if zsh {
        (
            "zsh",
            r#"for __f in $precmd_functions; do $__f; done
print -rn -- "${(%%)PROMPT}" > "$DIFFTOP_PROMPT_FILE"
printf '\033]6973;ready\007\n'
IFS= read -r __difftop_line || exit
eval "$__difftop_line"
__difftop_status=$?
pwd -P > "$DIFFTOP_PWD_FILE"
(exit $__difftop_status)"#,
        )
    } else {
        (
            "bash",
            r#"for __c in "${PROMPT_COMMAND[@]}"; do eval "$__c"; done
printf %s "${PS1@P}" > "$DIFFTOP_PROMPT_FILE"
printf '\033]6973;ready\007\n'
IFS= read -r __difftop_line || exit
eval "$__difftop_line"
__difftop_status=$?
pwd -P > "$DIFFTOP_PWD_FILE"
(exit $__difftop_status)"#,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    #[test]
    fn ready_marker_survives_unterminated_config_output() {
        assert!(is_ready(READY));
        assert!(is_ready(&format!("\x1b[m{READY}")));
        assert!(!is_ready("just some output"));
        assert!(!is_ready(&format!("{READY} and more")));
    }

    /// Starts a shell and waits until it's ready.
    fn start(cwd: &Path) -> (Shell, Receiver<Output>) {
        let (tx, rx) = mpsc::channel();
        let shell = Shell::start(cwd, None, (80, 24), move |out| {
            let _ = tx.send(out);
        })
        .unwrap();
        let first = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("shell never got ready");
        assert!(matches!(first, Output::Ready(_)), "{first:?}");
        (shell, rx)
    }

    /// Everything after Ready, up to and including `Done`.
    fn collect(rx: &Receiver<Output>) -> Vec<Output> {
        let mut all = Vec::new();
        loop {
            let out = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("command hung");
            let done = matches!(out, Output::Done { .. });
            all.push(out);
            if done {
                return all;
            }
        }
    }

    #[test]
    fn runs_a_line_and_reports_where_cd_went() {
        let tmp = tempfile::tempdir().unwrap();
        let here = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(here.join("sub")).unwrap();
        let (shell, rx) = start(&here);
        shell
            .run("echo out; echo err >&2; cd sub", (80, 24))
            .unwrap();
        assert_eq!(
            collect(&rx),
            [
                Output::Line("out".into()),
                Output::Line("err".into()),
                Output::Done {
                    code: Some(0),
                    cwd: Some(here.join("sub"))
                },
            ]
        );
    }

    #[test]
    fn failures_keep_their_exit_code_and_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let (shell, rx) = start(tmp.path());
        shell.run("ls /no/such/place", (80, 24)).unwrap();
        let all = collect(&rx);
        assert!(matches!(&all[0], Output::Line(text) if text.contains("No such file")));
        assert!(matches!(
            all.last(),
            Some(Output::Done { code: Some(2), .. })
        ));

        let (shell, rx) = start(tmp.path());
        shell.run("exit 3", (80, 24)).unwrap();
        let all = collect(&rx);
        assert_eq!(
            all.last(),
            Some(&Output::Done {
                code: Some(3),
                cwd: None
            })
        );
    }

    #[test]
    fn interrupt_stops_a_long_command() {
        let tmp = tempfile::tempdir().unwrap();
        let (shell, rx) = start(tmp.path());
        shell.run("sleep 30", (80, 24)).unwrap();
        thread::sleep(Duration::from_millis(300));
        shell.interrupt();
        let all = collect(&rx);
        assert!(
            matches!(
                all.last(),
                Some(Output::Done {
                    code: Some(130),
                    ..
                })
            ),
            "{all:?}"
        );
    }

    #[test]
    fn programs_see_a_terminal_of_the_size_at_run_time() {
        let tmp = tempfile::tempdir().unwrap();
        let (shell, rx) = start(tmp.path());
        shell
            .run("test -t 1 && echo tty; stty size", (100, 30))
            .unwrap();
        let all = collect(&rx);
        assert!(all.contains(&Output::Line("tty".into())), "{all:?}");
        assert!(all.contains(&Output::Line("30 100".into())), "{all:?}");
    }
}
