//! The terminal side: setup, the frame loop, drawing, keys, mouse, and the
//! `:` command line.

use std::io;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{DisableLineWrap, EnableLineWrap};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::command;
use crate::feed::Feed;
use crate::reload::{self, Saved, SavedLine, Upgrade};
use crate::render::tilde;
use crate::theme::UiColors;
use crate::watch;
use crate::{ansi, shell};

/// About 30 frames per second: smooth enough, and cheap when idle.
const FRAME: Duration = Duration::from_millis(33);
/// How long the second Esc has to come to quit.
const QUIT_CONFIRM: Duration = Duration::from_secs(3);
/// How long an error from the command line stays in the footer.
const NOTICE: Duration = Duration::from_secs(4);
/// Lines moved per mouse-wheel notch.
const WHEEL: isize = 3;
/// Speed change per `+`/`-` press, and per wheel notch over the speed display.
const KEY_SPEED_STEP: f64 = 1.5;
const WHEEL_SPEED_STEP: f64 = 1.2;

/// One line of the feed. Consecutive lines with the same `group` are drawn
/// inside one box: each shell command gets a group, so its prompt, output
/// and exit status are boxed. Diffs have none (yet), so they stay unboxed;
/// giving each change a group would box them too.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub line: Line<'static>,
    pub group: Option<u32>,
}

impl Entry {
    fn plain(line: Line<'static>) -> Self {
        Self { line, group: None }
    }
}

/// What the watcher thread sends to the screen.
pub enum Msg {
    Lines(Vec<Line<'static>>),
    Ready { files: usize },
    Warning(String),
    Shell(shell::Output),
}

pub struct App {
    pub feed: Feed<Entry>,
    pub colors: UiColors,
    /// What is being watched, for the footer.
    pub label: String,
    pub files: Option<usize>,
    pub warning: Option<String>,
    /// Height of the feed area at the last draw, for paging.
    height: usize,
    /// Width of the feed area at the last draw, for shell commands' terminal.
    width: u16,
    /// Where the "follow" toast was drawn, so a click on it can be recognised.
    toast: Option<Rect>,
    /// Where the speed was drawn, so the wheel over it changes the speed.
    speed: Option<Rect>,
    watcher: watch::Handle,
    /// The command being typed, while the `:` line is open.
    input: Option<String>,
    /// The folder before the last `cd`, for `cd -`.
    previous: Option<PathBuf>,
    /// A command-line error, shown in the footer for a few seconds.
    notice: Option<(String, Instant)>,
    /// The folders being watched, to hand over on a reload.
    roots: Vec<PathBuf>,
    /// Notices when the binary on disk is replaced.
    upgrade: Option<Upgrade>,
    /// For shell output to reach the frame loop.
    tx: Sender<Msg>,
    /// The warm shell waiting for the next command (see `shell`), or running it.
    shell: Option<shell::Shell>,
    /// Whether it has been given a command that hasn't finished yet.
    running: bool,
    /// Prompt lines run so far, oldest first, and where ↑/↓ are in them.
    history: Vec<String>,
    history_pos: Option<usize>,
    /// When Esc was pressed once, asking for a second one to quit.
    quit_armed: Option<Instant>,
    /// The box of the latest shell command; see [`Entry`].
    group: u32,
    /// Set by Ctrl-L: redraw the whole screen on the next frame.
    repaint: bool,
    /// The user's own shell prompt; empty until their shell has answered.
    ps1: Vec<Line<'static>>,
}

/// Takes over the terminal. Mouse reporting makes the wheel scroll, and
/// bracketed paste makes pasted text (a right-click in many terminals)
/// arrive as one paste event instead of a stream of fake keypresses.
/// Line wrapping goes off so drawing the bottom-right cell can never make
/// the terminal scroll (Windows Terminal does, via ConPTY), which would
/// shift the real screen away from what ratatui thinks is on it.
pub fn start() -> Result<DefaultTerminal> {
    let mut terminal = ratatui::init();
    // After a reload the screen still shows the old process's last frame.
    terminal.clear()?;
    execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        DisableLineWrap
    )?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        stop();
        previous(info);
    }));
    Ok(terminal)
}

/// Gives the terminal back as it was.
pub fn stop() {
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        EnableLineWrap
    );
    ratatui::restore();
}

pub fn run(terminal: &mut DefaultTerminal, mut app: App, rx: Receiver<Msg>) -> Result<()> {
    let mut last = Instant::now();
    loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Lines(lines) => app.feed.push(lines.into_iter().map(Entry::plain)),
                Msg::Ready { files } => app.files = Some(files),
                Msg::Warning(text) => app.warning = Some(text),
                Msg::Shell(out) => app.on_shell(out),
            }
        }
        let now = Instant::now();
        app.feed.tick(now - last);
        last = now;

        if app.upgrade.as_mut().is_some_and(Upgrade::ready) {
            app.reload();
        }

        if std::mem::take(&mut app.repaint) {
            terminal.clear()?;
        }
        terminal.draw(|frame| app.draw(frame))?;

        if event::poll(FRAME)? {
            let flow = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                Event::Mouse(mouse) => app.on_mouse(mouse),
                // Pasting a path into the command line is handy; anywhere
                // else a paste (a right-click in many terminals) is ignored.
                Event::Paste(text) => {
                    if let Some(input) = &mut app.input {
                        input.extend(text.chars().filter(|c| !c.is_control()));
                    }
                    ControlFlow::Continue(())
                }
                // Focus changes and resizes need nothing from us.
                _ => ControlFlow::Continue(()),
            };
            if flow.is_break() {
                return Ok(());
            }
        }
    }
}

impl App {
    pub fn new(
        feed: Feed<Entry>,
        colors: UiColors,
        roots: Vec<PathBuf>,
        watcher: watch::Handle,
        tx: Sender<Msg>,
    ) -> Self {
        let label = roots
            .iter()
            .map(|r| tilde(r))
            .collect::<Vec<_>>()
            .join("  ");
        let mut app = Self {
            feed,
            colors,
            label,
            files: None,
            warning: None,
            height: 0,
            width: 80,
            toast: None,
            speed: None,
            watcher,
            input: None,
            previous: None,
            notice: None,
            roots,
            upgrade: Upgrade::for_this_binary(),
            tx,
            shell: None,
            running: false,
            history: Vec::new(),
            history_pos: None,
            quit_armed: None,
            ps1: Vec::new(),
            repaint: false,
            group: 0,
        };
        // Warm up a shell now, so the prompt is there the moment `:` is pressed.
        app.start_shell();
        app
    }

    /// Picks up where a previous difftop left off before it reloaded.
    pub fn resume(&mut self, saved: Saved) {
        let entries = |saved: Vec<SavedLine>| saved.into_iter().map(Entry::from).collect();
        self.feed
            .restore(entries(saved.shown), entries(saved.pending), saved.below);
        self.group = saved.group;
        self.feed.speed = saved.speed.max(1.0);
        self.previous = saved.previous;
        self.marker(&format!("reloaded difftop {}", env!("CARGO_PKG_VERSION")));
    }

    /// Hands everything on screen to the new binary and becomes it. Only
    /// comes back if that failed, with the reason in the footer.
    fn reload(&mut self) {
        let Some(path) = self.upgrade.as_ref().map(|u| u.path().to_path_buf()) else {
            return;
        };
        let (shown, pending, below) = self.feed.contents();
        let saved = Saved {
            roots: self.roots.clone(),
            previous: self.previous.clone(),
            speed: self.feed.speed,
            shown: shown.map(SavedLine::from).collect(),
            pending: pending.map(SavedLine::from).collect(),
            below,
            group: self.group,
        };
        let err = reload::exec(&path, &saved);
        self.notice = Some((format!("reload failed: {err}"), Instant::now()));
    }

    /// A quiet line in the feed marking something that happened.
    fn marker(&mut self, text: &str) {
        self.feed.show_now([
            Entry::plain(Line::default()),
            Entry::plain(Line::styled(format!("── {text} ──"), self.colors.muted)),
            Entry::plain(Line::default()),
        ]);
    }

    fn on_key(&mut self, key: KeyEvent) -> ControlFlow<()> {
        // Ctrl-L repaints everything, as in any terminal, in case something
        // outside difftop scribbled on the screen.
        if key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.repaint = true;
            return ControlFlow::Continue(());
        }
        if self.input.is_some() {
            return self.on_input_key(key);
        }
        let page = self.height.max(1) as isize;
        // Any key answers the "Esc again to quit" warning; only Esc confirms it.
        let quit_armed = self
            .quit_armed
            .take()
            .is_some_and(|at| at.elapsed() < QUIT_CONFIRM);
        match key.code {
            // Like a terminal: Ctrl-C stops a running command first.
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match (&self.shell, self.running) {
                    (Some(shell), true) => shell.interrupt(),
                    _ => return ControlFlow::Break(()),
                }
            }
            // Esc quits only when pressed twice, with a warning in between,
            // so one Esc too many after closing the prompt is harmless.
            KeyCode::Esc if quit_armed => return ControlFlow::Break(()),
            KeyCode::Esc => self.quit_armed = Some(Instant::now()),
            KeyCode::Char(':') | KeyCode::Char('!') => {
                self.input = Some(String::new());
                if self.shell.is_none() {
                    self.start_shell();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.feed.scroll(1, self.height),
            KeyCode::Down | KeyCode::Char('j') => self.feed.scroll(-1, self.height),
            KeyCode::PageUp => self.feed.scroll(page, self.height),
            KeyCode::PageDown => self.feed.scroll(-page, self.height),
            KeyCode::Home | KeyCode::Char('g') => self.feed.scroll(isize::MAX, self.height),
            KeyCode::End | KeyCode::Char('G') => self.feed.follow(),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_speed(KEY_SPEED_STEP),
            KeyCode::Char('-') => self.change_speed(1.0 / KEY_SPEED_STEP),
            KeyCode::Char('c') => self.feed.clear(),
            _ => {}
        }
        ControlFlow::Continue(())
    }

    /// Keys while the `:` prompt is open: editing, history, Tab completion,
    /// Enter to run.
    fn on_input_key(&mut self, key: KeyEvent) -> ControlFlow<()> {
        let Some(input) = &mut self.input else {
            return ControlFlow::Continue(());
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Char('c') if ctrl => self.input = None,
            KeyCode::Char('u') if ctrl => input.clear(),
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Tab => {
                if let Some(completed) = command::complete(input) {
                    *input = completed;
                }
            }
            KeyCode::Up | KeyCode::Down => {
                let len = self.history.len();
                let pos = match (key.code, self.history_pos) {
                    (KeyCode::Up, None) => len.checked_sub(1),
                    (KeyCode::Up, Some(pos)) => Some(pos.saturating_sub(1)),
                    (_, Some(pos)) if pos + 1 < len => Some(pos + 1),
                    _ => None,
                };
                self.history_pos = pos;
                *input = pos.map(|pos| self.history[pos].clone()).unwrap_or_default();
            }
            // The prompt stays open for the next command, like a terminal.
            KeyCode::Enter => {
                let line = std::mem::take(input);
                self.run_command(line);
            }
            KeyCode::Char(ch) if !ctrl => input.push(ch),
            _ => {}
        }
        ControlFlow::Continue(())
    }

    /// Runs a prompt line in a shell; its output arrives as [`Msg::Shell`].
    fn run_command(&mut self, line: String) {
        self.history_pos = None;
        if line.trim().is_empty() {
            return;
        }
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
        }
        if self.running {
            self.notice = Some((
                "a command is still running (Ctrl-C stops it)".into(),
                Instant::now(),
            ));
            return;
        }
        if self.shell.is_none() {
            self.start_shell();
        }
        let Some(shell) = &self.shell else {
            return;
        };
        if let Err(err) = shell.run(&line, self.pty_size()) {
            self.notice = Some((format!("can't run that: {err}"), Instant::now()));
            return;
        }
        self.running = true;
        self.group += 1;
        self.feed.follow();
        let echo = self.prompt_with(&line);
        self.show_in_box(echo);
    }

    /// Shows shell output as it comes, and follows the shell if it `cd`ed.
    /// Adds lines to the current command's box.
    fn show_in_box(&mut self, lines: Vec<Line<'static>>) {
        let group = Some(self.group);
        self.feed
            .show_now(lines.into_iter().map(|line| Entry { line, group }));
    }

    fn on_shell(&mut self, out: shell::Output) {
        match out {
            shell::Output::Ready(prompt) => self.ps1 = ansi::lines(&prompt),
            shell::Output::Line(text) => self.show_in_box(ansi::lines(&text)),
            // A shell that dies while idle (a broken config, say) isn't
            // restarted until the next command, so it can't restart in a loop.
            shell::Output::Done { .. } if !self.running => self.shell = None,
            shell::Output::Done { code, cwd } => {
                self.running = false;
                self.shell = None;
                let ending = match code {
                    Some(0) => None,
                    Some(130) | None => Some("interrupted".to_string()),
                    Some(code) => Some(format!("exit {code}")),
                };
                if let Some(ending) = ending {
                    let style = Style::new().fg(self.colors.removed);
                    self.show_in_box(vec![Line::styled(ending, style)]);
                }
                let moved = cwd.filter(|dir| std::env::current_dir().ok().as_ref() != Some(dir));
                if let Some(Err(err)) = moved.map(|dir| self.cd(dir)) {
                    self.notice = Some((err, Instant::now()));
                }
                // Warm up the next one, in the folder this one ended up in.
                self.start_shell();
            }
        }
    }

    /// Moves difftop into `dir` and watches it instead of what it watched.
    /// History already on screen stays; a marker shows where the move happened.
    fn cd(&mut self, dir: PathBuf) -> Result<(), String> {
        let here = std::env::current_dir().ok();
        std::env::set_current_dir(&dir)
            .map_err(|err| format!("can't enter {}: {err}", tilde(&dir)))?;
        self.previous = here;
        self.label = tilde(&dir);
        self.files = None;
        self.marker(&format!("now watching {}", self.label));
        self.roots = vec![dir.clone()];
        self.watcher.watch(vec![dir]);
        Ok(())
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> ControlFlow<()> {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if hit(self.speed, mouse) => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                self.change_speed(if up {
                    WHEEL_SPEED_STEP
                } else {
                    1.0 / WHEEL_SPEED_STEP
                });
            }
            MouseEventKind::ScrollUp => self.feed.scroll(WHEEL, self.height),
            MouseEventKind::ScrollDown => self.feed.scroll(-WHEEL, self.height),
            MouseEventKind::Down(MouseButton::Left) if hit(self.toast, mouse) => self.feed.follow(),
            // Everything else, right-click included, is ignored.
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn draw(&mut self, frame: &mut Frame) {
        let c = self.colors;
        let area = frame.area();
        frame.render_widget(
            Block::new().style(Style::new().fg(c.foreground).bg(c.background)),
            area,
        );
        // In the shell, the prompt sits in its own box and the status line
        // makes way for it.
        let in_shell = self.input.is_some();
        let prompt = self.input.as_deref().map(|input| self.prompt_with(input));
        let bottom_height = prompt.as_ref().map_or(1, |lines| lines.len() as u16 + 2);
        let [feed_area, bottom_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(bottom_height)]).areas(area);

        self.height = feed_area.height as usize;
        // Shell commands print inside a box: two border columns, two of padding.
        self.width = feed_area.width.saturating_sub(4);
        let following = self.feed.below() == 0;
        self.draw_feed(frame, feed_area, following && !in_shell);

        self.toast = (!following).then(|| self.draw_toast(frame, feed_area));
        let Some(prompt) = prompt else {
            self.speed = Some(self.draw_info(frame, bottom_area));
            return;
        };
        self.speed = None;
        let prompt_box = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(c.muted)
            .padding(Padding::horizontal(1))
            .title(self.notch("─┐", [("", "shell")], "┌"))
            .title_bottom(
                self.notch("┘", [("esc", " leave"), ("^C", " stop")], "└─")
                    .right_aligned(),
            );
        let inner = prompt_box.inner(bottom_area);
        frame.render_widget(prompt_box, bottom_area);
        let x = inner.x + prompt.last().map_or(0, Line::width) as u16;
        let y = inner.bottom().saturating_sub(1);
        frame.render_widget(Paragraph::new(prompt), inner);
        frame.set_cursor_position((x.min(inner.right().saturating_sub(1)), y));
    }

    /// The feed, newest line at the bottom, shell commands in their boxes.
    /// With `fading`, the top third is dimmed.
    fn draw_feed(&self, frame: &mut Frame, area: Rect, fading: bool) {
        let entries: Vec<&Entry> = self.feed.view(self.height).collect();
        let pieces = pieces(&entries);
        // Borders take rows too: keep the newest rows that fit.
        let shown = &pieces[pieces.len().saturating_sub(self.height)..];
        let top = area.bottom() - shown.len() as u16;
        let border = |left: &str, right: &str| {
            let middle = "─".repeat(area.width.saturating_sub(2) as usize);
            Line::styled(format!("{left}{middle}{right}"), self.colors.muted)
        };
        for (i, piece) in shown.iter().enumerate() {
            let y = top + i as u16;
            let row = Rect {
                y,
                height: 1,
                ..area
            };
            let dim = |line: Line<'static>| {
                if fading && (y - area.y) < area.height / 3 {
                    line.patch_style(Style::new().add_modifier(Modifier::DIM))
                } else {
                    line
                }
            };
            match piece {
                Piece::Plain(line) => frame.render_widget(dim((*line).clone()), row),
                Piece::Top => frame.render_widget(dim(border("╭", "╮")), row),
                Piece::Bottom => frame.render_widget(dim(border("╰", "╯")), row),
                Piece::Inside(line) => {
                    let side = dim(Line::styled("│", self.colors.muted));
                    frame.render_widget(side.clone(), Rect { width: 1, ..row });
                    let right = Rect {
                        x: row.right().saturating_sub(1),
                        width: 1,
                        ..row
                    };
                    frame.render_widget(side, right);
                    let inner = Rect {
                        x: row.x + 2,
                        width: row.width.saturating_sub(4),
                        ..row
                    };
                    frame.render_widget(dim((*line).clone()), inner);
                }
            }
        }
    }

    /// A btop-style label cut into a border: `┐shell┌`, or for key hints
    /// `┘esc leave└┘^C stop└`, keys highlighted and the rest in border color.
    fn notch<const N: usize>(
        &self,
        open: &'static str,
        items: [(&'static str, &'static str); N],
        close: &'static str,
    ) -> Line<'static> {
        let c = &self.colors;
        let key = Style::new().fg(c.accent).add_modifier(Modifier::BOLD);
        let mut spans = vec![Span::styled(open, c.muted)];
        for (i, (hotkey, label)) in items.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("└┘", c.muted));
            }
            spans.push(Span::styled(hotkey, key));
            // A label with no key is a title: plain, bold text.
            let label_style = if hotkey.is_empty() {
                Style::new().fg(c.foreground).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(c.foreground)
            };
            spans.push(Span::styled(label, label_style));
        }
        spans.push(Span::styled(close, c.muted));
        Line::from(spans)
    }

    /// A small notice at the bottom middle of the feed while scrolled back.
    fn draw_toast(&self, frame: &mut Frame, feed_area: Rect) -> Rect {
        let below = self.feed.below();
        let noun = if below == 1 { "line" } else { "lines" };
        let text = format!(" ↓ {below} newer {noun} · End or click to follow ");
        let width = (text.chars().count() as u16).min(feed_area.width);
        let area = Rect {
            x: feed_area.x + (feed_area.width - width) / 2,
            y: feed_area.bottom().saturating_sub(1),
            width,
            height: 1.min(feed_area.height),
        };
        let style = Style::new()
            .fg(self.colors.accent)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD);
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(text).style(style), area);
        area
    }

    /// The prompt followed by `typed`, as the user's shell shows it. Until
    /// (or unless) their shell has told us its prompt: a plain `~/proj ❯ `.
    fn prompt_with(&self, typed: &str) -> Vec<Line<'static>> {
        let mut lines = if self.ps1.is_empty() {
            let cwd = std::env::current_dir().unwrap_or_default();
            vec![Line::from(vec![
                Span::styled(tail(&tilde(&cwd), 30), self.colors.muted),
                Span::raw(" ❯ "),
            ])]
        } else {
            self.ps1.clone()
        };
        let last = lines.last_mut().expect("never empty");
        last.spans.push(Span::raw(typed.to_string()));
        lines
    }

    /// Starts the next warm shell in the current folder; its prompt and
    /// output arrive as [`Msg::Shell`].
    fn start_shell(&mut self) {
        let tx = self.tx.clone();
        let cwd = std::env::current_dir().unwrap_or_default();
        let started = shell::Shell::start(
            &cwd,
            self.previous.as_deref(),
            self.pty_size(),
            move |out| {
                let _ = tx.send(Msg::Shell(out));
            },
        );
        match started {
            Ok(shell) => self.shell = Some(shell),
            Err(err) => self.notice = Some((format!("can't start a shell: {err}"), Instant::now())),
        }
    }

    /// The terminal size commands run at: the inside of a command box.
    fn pty_size(&self) -> (u16, u16) {
        (self.width.max(1), (self.height as u16).max(1))
    }

    /// Multiplies the speed by `factor`, never going below one line a second.
    fn change_speed(&mut self, factor: f64) {
        self.feed.speed = (self.feed.speed * factor).max(1.0);
    }

    /// The status line: what's watched on the left, the key hints, how it's
    /// going on the right. Returns where the speed was drawn.
    fn draw_info(&self, frame: &mut Frame, area: Rect) -> Rect {
        let c = &self.colors;
        let (status, speed_width) = self.status();
        let hints = self.hints();
        let [label_area, hints_area, status_area] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(hints.width() as u16),
            Constraint::Length(status.width() as u16),
        ])
        .areas(area);
        // Leave a one-column gap before the status and keep the end of long
        // paths, which is the part that tells folders apart.
        let room = (label_area.width as usize).saturating_sub(2);
        let notice = self.notice.as_ref().filter(|(_, at)| at.elapsed() < NOTICE);
        let quitting = self
            .quit_armed
            .is_some_and(|at| at.elapsed() < QUIT_CONFIRM);
        let label = match notice.map(|(text, _)| text).or(self.warning.as_ref()) {
            _ if quitting => Line::from(Span::styled(
                " Press Esc again to quit difftop",
                Style::new().fg(c.removed).add_modifier(Modifier::BOLD),
            )),
            Some(warning) => Line::styled(format!(" {warning}"), Style::new().fg(c.removed)),
            None => Line::styled(
                format!(" {}", tail(&self.label, room)),
                Style::new().fg(c.accent),
            ),
        };
        frame.render_widget(label, label_area);
        frame.render_widget(hints, hints_area);
        frame.render_widget(status, status_area);
        Rect {
            x: status_area.right().saturating_sub(speed_width),
            width: speed_width.min(status_area.width),
            ..status_area
        }
    }

    /// Right side of the info line, most important first.
    /// Also returns the width of the speed, which is always last.
    fn status(&self) -> (Line<'static>, u16) {
        let mut parts = Vec::new();
        if self.running {
            parts.push("running…".to_string());
        }
        match self.files {
            None => parts.push("scanning…".into()),
            Some(1) => parts.push("1 file".into()),
            Some(files) => parts.push(format!("{files} files")),
        }
        if self.feed.queued() > 0 {
            parts.push(format!("{} queued", self.feed.queued()));
        }
        if self.feed.dropped > 0 {
            parts.push(format!("{} skipped", self.feed.dropped));
        }
        let speed = format!("{:.0}/s ", self.feed.speed);
        let width = speed.chars().count() as u16;
        parts.push(speed);
        let line = Line::from(Span::styled(parts.join(" · "), self.colors.muted));
        (line, width)
    }

    /// The keys nobody would guess. Scrolling, speed and quitting are left
    /// out on purpose: they work the way people expect.
    fn hints(&self) -> Line<'static> {
        let key = Style::new()
            .fg(self.colors.accent)
            .add_modifier(Modifier::BOLD);
        let mut spans = Vec::new();
        for (hotkey, action) in [(":", "shell"), ("c", "clear")] {
            spans.push(Span::styled(hotkey, key));
            spans.push(Span::styled(format!(" {action}   "), self.colors.muted));
        }
        Line::from(spans)
    }
}

/// How one feed row is drawn.
#[derive(Debug, PartialEq)]
enum Piece<'a> {
    Plain(&'a Line<'static>),
    /// A box's top border, its content, its bottom border.
    Top,
    Inside(&'a Line<'static>),
    Bottom,
}

/// Lays out entries as rows: plain lines as they are, and each run of
/// consecutive lines of the same group wrapped in a box.
fn pieces<'a>(entries: &[&'a Entry]) -> Vec<Piece<'a>> {
    let mut out = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let Some(group) = entry.group else {
            out.push(Piece::Plain(&entry.line));
            continue;
        };
        let same = |other: Option<&&Entry>| other.is_some_and(|e| e.group == Some(group));
        if !same(i.checked_sub(1).and_then(|j| entries.get(j))) {
            out.push(Piece::Top);
        }
        out.push(Piece::Inside(&entry.line));
        if !same(entries.get(i + 1)) {
            out.push(Piece::Bottom);
        }
    }
    out
}

/// Whether the mouse is over `area`.
fn hit(area: Option<Rect>, mouse: MouseEvent) -> bool {
    area.is_some_and(|a| a.contains(Position::new(mouse.column, mouse.row)))
}

/// `text` cut to at most `width` characters, keeping the end: `…/b/c`.
fn tail(text: &str, width: usize) -> String {
    let count = text.chars().count();
    match width {
        _ if count <= width => text.to_string(),
        0 => String::new(),
        _ => std::iter::once('…')
            .chain(text.chars().skip(count - (width - 1)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_become_boxes_and_the_rest_stays_plain() {
        let entry = |text: &str, group| Entry {
            line: Line::raw(text.to_string()),
            group,
        };
        let entries = [
            entry("diff", None),
            entry("$ ls", Some(1)),
            entry("a.rs", Some(1)),
            entry("diff while ls runs", None),
            entry("b.rs", Some(1)),
            entry("$ pwd", Some(2)),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();
        let text = |piece: &Piece| match piece {
            Piece::Plain(line) => format!("plain {}", line.spans[0].content),
            Piece::Inside(line) => format!("| {}", line.spans[0].content),
            Piece::Top => "top".into(),
            Piece::Bottom => "bottom".into(),
        };
        let rows: Vec<String> = pieces(&refs).iter().map(text).collect();
        assert_eq!(
            rows,
            [
                "plain diff",
                "top",
                "| $ ls",
                "| a.rs",
                "bottom",
                "plain diff while ls runs",
                "top",
                "| b.rs",
                "bottom",
                "top",
                "| $ pwd",
                "bottom",
            ]
        );
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("~/code/project", 20), "~/code/project");
        assert_eq!(tail("~/code/project", 8), "…project");
        assert_eq!(tail("~/code/project", 0), "");
    }
}
