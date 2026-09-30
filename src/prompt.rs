//! The user's own shell prompt, so the `:` prompt looks like their terminal.
//!
//! Rather than re-implementing prompt languages, we ask the user's shell to
//! print its prompt, fully expanded, for a given folder: bash's `${PS1@P}`,
//! zsh's `%` expansion, fish's `fish_prompt`. Prompt hooks (`PROMPT_COMMAND`,
//! `precmd`) run first, so tools like starship work too.

use std::path::Path;
use std::process::{Command, Stdio};

/// The prompt as the user's shell would print it in `cwd`, color codes and
/// all, or `None` for shells we don't know or if it failed.
pub fn render(cwd: &Path) -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;
    let name = Path::new(&shell).file_name()?.to_str()?;
    let (flags, script) = match name {
        "bash" => (
            "-ic",
            r#"for c in "${PROMPT_COMMAND[@]}"; do eval "$c"; done; printf %s "${PS1@P}""#,
        ),
        "zsh" => (
            "-ic",
            r#"for f in $precmd_functions; do $f; done; print -rn -- "${(%%)PROMPT}""#,
        ),
        "fish" => ("-c", "fish_prompt"),
        _ => return None,
    };
    let out = Command::new(&shell)
        .args([flags, script])
        .current_dir(cwd)
        .stdin(Stdio::null())
        // Interactive shells without a terminal complain about job control.
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let prompt = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success() && !prompt.trim().is_empty()).then_some(prompt)
}
