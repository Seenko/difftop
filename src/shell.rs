//! Runs a line typed at the `:` prompt in a real shell, streaming its output
//! back, and reports which folder the shell ended up in so difftop can
//! follow a `cd`.
//!
//! The line is wrapped in a small script that, after the command, writes the
//! shell's working directory to a file we name. That's what makes `cd` work
//! anywhere in the line (`cd src && ls`), not just as a lone command.

use std::io::{self, BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

#[derive(Debug, PartialEq, Eq)]
pub enum Output {
    /// One line of what the command printed (stdout and stderr, in order).
    Line(String),
    /// The command finished. `code` is `None` if it was killed by a signal;
    /// `cwd` is where the shell ended up, if it got that far.
    Done {
        code: Option<i32>,
        cwd: Option<PathBuf>,
    },
}

/// A command that's still running.
pub struct Running {
    pgid: u32,
}

impl Running {
    /// Sends Ctrl-C (SIGINT) to the command and everything it started.
    pub fn interrupt(&self) {
        // std can't signal a process group without unsafe code, so ask `kill`.
        let _ = Command::new("kill")
            .args(["-INT", "--", &format!("-{}", self.pgid)])
            .stderr(Stdio::null())
            .status();
    }
}

/// Starts `line` in `cwd` and returns right away; `on_output` is called from
/// a background thread for every line and once at the end. `oldpwd` is
/// where `cd -` goes.
pub fn run(
    line: &str,
    cwd: &Path,
    oldpwd: Option<&Path>,
    mut on_output: impl FnMut(Output) + Send + 'static,
) -> io::Result<Running> {
    let pwd_file = crate::reload::private_path("pwd");
    let script = format!(
        "{line}\n__difftop_status=$?\npwd -P > \"$DIFFTOP_PWD_FILE\"\nexit $__difftop_status\n"
    );
    let (reader, writer) = io::pipe()?;
    let mut command = Command::new(shell());
    command
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .env("DIFFTOP_PWD_FILE", &pwd_file)
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer)
        // Its own process group, so Ctrl-C reaches everything it starts.
        .process_group(0);
    if let Some(oldpwd) = oldpwd {
        command.env("OLDPWD", oldpwd);
    }
    let mut child = command.spawn()?;
    // Drop our copies of the pipe's write end, or reading would never end.
    drop(command);
    let running = Running { pgid: child.id() };

    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
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

/// bash where there is one (nearly everywhere), plain sh otherwise.
fn shell() -> &'static str {
    let has_bash = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("bash").is_file()));
    if has_bash { "bash" } else { "sh" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    fn start(line: &str, cwd: &Path) -> (Running, Receiver<Output>) {
        let (tx, rx) = mpsc::channel();
        let running = run(line, cwd, None, move |out| {
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
        assert_eq!(
            collect(&rx),
            [Output::Done {
                code: Some(3),
                cwd: None
            }]
        );
    }

    #[test]
    fn interrupt_stops_a_long_command() {
        let tmp = tempfile::tempdir().unwrap();
        let (running, rx) = start("sleep 30", tmp.path());
        thread::sleep(Duration::from_millis(200));
        running.interrupt();
        let all = collect(&rx);
        assert!(matches!(all.last(), Some(Output::Done { cwd: None, .. })));
    }
}
