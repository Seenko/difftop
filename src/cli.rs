//! Command-line interface. Every flag lives here so `--help` is the single
//! source of truth for what users can configure.

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Folders to watch, recursively. Defaults to the current directory.
    #[arg(default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// Theme name or path to a .tmTheme / base16 .yaml file. The default,
    /// `claude`, looks like Claude Code's diffs; `ansi` uses your terminal's
    /// own palette and follows it live.
    #[arg(short, long, default_value = "claude")]
    pub theme: String,

    /// Print every available theme name and exit.
    #[arg(long)]
    pub list_themes: bool,

    /// How many lines rise per second when nothing is queued up. Bursts
    /// drain faster automatically. Adjust live with + and -.
    #[arg(short, long, default_value_t = 30.0)]
    pub speed: f64,

    /// Unchanged lines shown around each change.
    #[arg(short = 'C', long, default_value_t = 2)]
    pub context: usize,

    /// Hide the line-number column next to each diff.
    #[arg(long)]
    pub no_line_numbers: bool,

    /// Mark changed lines by their +/- sign alone, without the colored row
    /// background.
    #[arg(long)]
    pub no_change_background: bool,

    /// Also watch hidden files and folders (names starting with a dot).
    #[arg(long)]
    pub hidden: bool,

    /// Don't respect .gitignore files.
    #[arg(long)]
    pub no_ignore: bool,
}
