//! Runs a line typed at the `:` prompt the way the user's terminal would,
//! streaming its output back, and reports which folder the shell ended up in
//! so difftop can follow a `cd`.
//!
//! The command runs in the user's own shell, interactive, so their aliases
//! and settings apply, inside a pseudo-terminal (PTY) the size of difftop's
//! screen. To the programs it looks like a real terminal, so they print
//! colors and columns just as they would outside difftop, and Ctrl-C is a
//! real Ctrl-C.
//!
//! The line is wrapped in a small script that, after the command, writes the
//! shell's working directory to a file we name. That's what makes `cd` work
//! anywhere in the line (`cd src && ls`), not just as a lone command.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use rustix::fs::{Mode, OFlags};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, Winsize};

#[derive(Debug, PartialEq, Eq)]
pub enum Output {
    /// One line of what the command printed, color codes included.
    Line(String),
    /// The command finished. `code` is `None` if the shell itself was killed;
    /// `cwd` is where the shell ended up, if it got that far.
    Done {
        code: Option<i32>,
        cwd: Option<PathBuf>,
    },
}

/// A command that's still running.
pub struct Running {
    /// Our end of its terminal.
    pty: File,
}

impl Running {
    /// Types Ctrl-C into the command's terminal, which interrupts it exactly
    /// like in a real terminal.
    pub fn interrupt(&self) {
        let _ = (&self.pty).write_all(b"\x03");
    }
}

/// Starts `line` in `cwd` on a terminal of `size` (columns, rows) and returns
/// right away; `on_output` is called from a background thread for every line
/// and once at the end. `oldpwd` is where `cd -` goes.
pub fn run(
    line: &str,
    cwd: &Path,
    oldpwd: Option<&Path>,
    (cols, rows): (u16, u16),
    mut on_output: impl FnMut(Output) + Send + 'static,
) -> io::Result<Running> {
    // A fresh pseudo-terminal: we keep the "master" end, the command gets the other.
    let pty = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    pty::grantpt(&pty)?;
    pty::unlockpt(&pty)?;
    let size = Winsize {
        ws_col: cols,
        ws_row: rows,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    termios::tcsetwinsize(&pty, size)?;
    let name = pty::ptsname(&pty, Vec::new())?;
    let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
    let terminal = File::from(rustix::fs::open(name.as_c_str(), flags, Mode::empty())?);

    let pwd_file = crate::reload::private_path("pwd");
    let script = format!(
        "{line}\n__difftop_status=$?\npwd -P > \"$DIFFTOP_PWD_FILE\"\n(exit $__difftop_status)\n"
    );
    // `setsid --ctty` gives the shell its own session with this terminal as
    // its controlling terminal, which interactive shells need for job
    // control (and Ctrl-C) to work.
    let mut command = Command::new("setsid");
    command
        .args(["--ctty", shell(), "-ic", &script])
        .current_dir(cwd)
        .env("DIFFTOP_PWD_FILE", &pwd_file)
        .stdin(terminal.try_clone()?)
        .stdout(terminal.try_clone()?)
        .stderr(terminal);
    if let Some(oldpwd) = oldpwd {
        command.env("OLDPWD", oldpwd);
    }
    let mut child = command.spawn()?;
    // Drop our copies of the command's end, or reading would never end.
    drop(command);
    let pty = File::from(pty);
    let running = Running {
        pty: pty.try_clone()?,
    };

    thread::spawn(move || {
        let mut reader = BufReader::new(pty);
        let mut buf = Vec::new();
        // Reading fails (EIO) once the command's side is closed: that's the end.
        while reader.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
            let text = String::from_utf8_lossy(&buf);
            on_output(Output::Line(
                text.trim_end_matches(['\n', '\r']).to_string(),
            ));
            buf.clear();
        }
        let code = child.wait().ok().and_then(|status| status.code());
        let cwd = std::fs::read_to_string(&pwd_file)
            .ok()
            .map(|text| PathBuf::from(text.trim_end_matches('\n')));
        let _ = std::fs::remove_file(&pwd_file);
        on_output(Output::Done { code, cwd });
    });
    Ok(running)
}

/// The user's shell if the wrapper script speaks its language, else bash.
fn shell() -> &'static str {
    match std::env::var("SHELL")
        .ok()
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_name)
    {
        Some(name) if name == "zsh" => "zsh",
        _ => "bash",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    fn start(line: &str, cwd: &Path) -> (Running, Receiver<Output>) {
        let (tx, rx) = mpsc::channel();
        let running = run(line, cwd, None, (80, 24), move |out| {
            let _ = tx.send(out);
        })
        .unwrap();
        (running, rx)
    }

    /// Everything the command produced, up to and including `Done`.
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
    fn streams_output_and_reports_where_cd_went() {
        let tmp = tempfile::tempdir().unwrap();
        let here = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(here.join("sub")).unwrap();
        let (_, rx) = start("echo out; echo err >&2; cd sub", &here);
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
        let (_, rx) = start("ls /no/such/place", tmp.path());
        let all = collect(&rx);
        assert!(matches!(&all[0], Output::Line(text) if text.contains("No such file")));
        assert!(matches!(
            all.last(),
            Some(Output::Done { code: Some(2), .. })
        ));

        let (_, rx) = start("exit 3", tmp.path());
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
        let (running, rx) = start("sleep 30", tmp.path());
        thread::sleep(Duration::from_millis(500));
        running.interrupt();
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
    fn programs_see_a_terminal_of_the_right_size() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, rx) = start("test -t 1 && echo tty; stty size", tmp.path());
        let all = collect(&rx);
        assert!(all.contains(&Output::Line("tty".into())), "{all:?}");
        assert!(all.contains(&Output::Line("24 80".into())), "{all:?}");
    }
}
