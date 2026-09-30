//! difftop: watch folders and see every file change rise up the terminal.
//!
//! Two threads: the watcher thread notices changes, diffs and highlights
//! them ([`watch`] → [`render`]); the main thread animates and draws ([`ui`]).
//! See ARCHITECTURE.md for the full picture.

mod cli;
mod diff;
mod feed;
mod filter;
mod highlight;
mod render;
mod theme;
mod ui;
mod watch;

use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::render::Renderer;
use crate::ui::{App, Msg};

fn main() -> Result<()> {
    let args = cli::Args::parse();
    if args.list_themes {
        for name in theme::names() {
            println!("{name}");
        }
        return Ok(());
    }

    let theme = theme::load(&args.theme)?;
    let colors = theme::UiColors::from_theme(&theme);
    let mut roots = Vec::new();
    for path in &args.paths {
        let root = path
            .canonicalize()
            .with_context(|| format!("can't watch {}", path.display()))?;
        if !root.is_dir() {
            bail!("{} is not a folder", path.display());
        }
        roots.push(root);
    }
    let label = roots
        .iter()
        .map(|r| render::tilde(r))
        .collect::<Vec<_>>()
        .join("  ");

    let (tx, rx) = mpsc::channel();
    let options = watch::Options {
        roots,
        hidden: args.hidden,
        gitignore: !args.no_ignore,
    };
    let context = args.context;
    thread::spawn(move || {
        let renderer = Renderer {
            highlighter: highlight::Highlighter::new(theme),
            colors,
            context,
        };
        let result = watch::run(options, |event| {
            let msg = match event {
                watch::Event::Ready { files } => Msg::Ready { files },
                watch::Event::Change(change) => Msg::Lines(renderer.render(&change)),
                watch::Event::Warning(text) => Msg::Warning(text),
            };
            // The screen is gone once the user quit; nothing left to do.
            let _ = tx.send(msg);
        });
        if let Err(err) = result {
            let _ = tx.send(Msg::Warning(format!("watcher stopped: {err}")));
        }
    });

    let app = App::new(feed::Feed::new(args.speed.max(1.0)), colors, label);
    let mut terminal = ui::start()?;
    let result = ui::run(&mut terminal, app, rx);
    ui::stop();
    result
}
