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
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::command;
use crate::feed::Feed;
use crate::reload::{self, Saved, SavedLine, Upgrade};
use crate::render::tilde;
use crate::theme::UiColors;
use crate::watch;
use crate::{ansi, prompt, shell};

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

/// What the watcher thread sends to the screen.
pub enum Msg {
    Lines(Vec<Line<'static>>),
    Ready {
        files: usize,
    },
    Warning(String),
    Shell(shell::Output),
    /// The user's prompt, rendered by their shell (see `prompt`).
    Prompt(Option<String>),
}

pub struct App {
    pub feed: Feed<Line<'static>>,
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
    /// The shell command started from the prompt, while it runs.
    running: Option<shell::Running>,
    /// Prompt lines run so far, oldest first, and where ↑/↓ are in them.
    history: Vec<String>,
    history_pos: Option<usize>,
    /// When Esc was pressed once, asking for a second one to quit.
    quit_armed: Option<Instant>,
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
                Msg::Lines(lines) => app.feed.push(lines),
                Msg::Ready { files } => app.files = Some(files),
                Msg::Warning(text) => app.warning = Some(text),
                Msg::Shell(out) => app.on_shell(out),
                Msg::Prompt(ps1) => app.ps1 = ps1.as_deref().map(ansi::lines).unwrap_or_default(),
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
        feed: Feed<Line<'static>>,
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
        let app = Self {
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
            running: None,
            history: Vec::new(),
            history_pos: None,
            quit_armed: None,
            ps1: Vec::new(),
            repaint: false,
        };
        // Fetch the prompt now, so it's there the moment `:` is pressed.
        app.refresh_prompt();
        app
    }

    /// Picks up where a previous difftop left off before it reloaded.
    pub fn resume(&mut self, saved: Saved) {
        let lines = |saved: Vec<SavedLine>| saved.into_iter().map(Line::from).collect();
        self.feed
            .restore(lines(saved.shown), lines(saved.pending), saved.below);
        self.feed.speed = saved.speed.max(1.0);
        self.feed.paused = saved.paused;
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
            paused: self.feed.paused,
            shown: shown.map(SavedLine::from).collect(),
            pending: pending.map(SavedLine::from).collect(),
            below,
        };
        let err = reload::exec(&path, &saved);
        self.notice = Some((format!("reload failed: {err}"), Instant::now()));
    }

    /// A quiet line in the feed marking something that happened.
    fn marker(&mut self, text: &str) {
        self.feed.show_now([
            Line::default(),
            Line::styled(format!("── {text} ──"), self.colors.muted),
            Line::default(),
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
                match &self.running {
                    Some(running) => running.interrupt(),
                    None => return ControlFlow::Break(()),
                }
            }
            // Esc quits only when pressed twice, with a warning in between,
            // so one Esc too many after closing the prompt is harmless.
            KeyCode::Esc if quit_armed => return ControlFlow::Break(()),
            KeyCode::Esc => self.quit_armed = Some(Instant::now()),
            KeyCode::Char('q') => return ControlFlow::Break(()),
            KeyCode::Char(':') | KeyCode::Char('!') => {
                self.input = Some(String::new());
                self.refresh_prompt();
            }
            KeyCode::Char(' ') => self.feed.paused = !self.feed.paused,
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
        if self.running.is_some() {
            self.notice = Some((
                "a command is still running (Ctrl-C stops it)".into(),
                Instant::now(),
            ));
            return;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        self.feed.follow();
        self.feed.show_now([Line::default()]);
        self.feed.show_now(self.prompt_with(&line));
        let tx = self.tx.clone();
        let size = (self.width, self.height as u16);
        match shell::run(&line, &cwd, self.previous.as_deref(), size, move |out| {
            let _ = tx.send(Msg::Shell(out));
        }) {
            Ok(running) => self.running = Some(running),
            Err(err) => self.notice = Some((format!("can't run a shell: {err}"), Instant::now())),
        }
    }

    /// Shows shell output as it comes, and follows the shell if it `cd`ed.
    fn on_shell(&mut self, out: shell::Output) {
        match out {
            shell::Output::Line(text) => self.feed.show_now(ansi::lines(&text)),
            shell::Output::Done { code, cwd } => {
                self.running = None;
                let ending = match code {
                    Some(0) => None,
                    Some(130) | None => Some("interrupted".to_string()),
                    Some(code) => Some(format!("exit {code}")),
                };
                if let Some(ending) = ending {
                    self.feed
                        .show_now([Line::styled(ending, Style::new().fg(self.colors.removed))]);
                }
                let moved = cwd.filter(|dir| std::env::current_dir().ok().as_ref() != Some(dir));
                if let Some(Err(err)) = moved.map(|dir| self.cd(dir)) {
                    self.notice = Some((err, Instant::now()));
                }
                // The prompt may show the folder, git branch, last status...
                self.refresh_prompt();
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
        let mut area = frame.area();
        frame.render_widget(
            Block::new().style(Style::new().fg(c.foreground).bg(c.background)),
            area,
        );
        // In the shell, a frame around everything says "you're in a
        // terminal now", and the status line makes way for the prompt.
        let in_shell = self.input.is_some();
        if in_shell {
            let frame_block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(c.muted)
                .title(self.notch("─┐", [("", "shell")], "┌"))
                .title_bottom(
                    self.notch("┘", [("esc", " leave"), ("^C", " stop")], "└─")
                        .right_aligned(),
                );
            let inner = frame_block.inner(area);
            frame.render_widget(frame_block, area);
            area = inner;
        }
        let bottom = match &self.input {
            Some(input) => self.prompt_with(input),
            None => self.keys(area.width),
        };
        let [feed_area, info_area, bottom_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(if in_shell { 0 } else { 1 }),
            Constraint::Length(bottom.len() as u16),
        ])
        .areas(area);

        // Lines sit at the bottom. While watching (not in the shell, not
        // scrolled back) the top third fades out.
        self.height = feed_area.height as usize;
        self.width = feed_area.width;
        let following = self.feed.below() == 0;
        let fading = following && !in_shell;
        let lines: Vec<Line> = self.feed.view(self.height).cloned().collect();
        let padding = self.height - lines.len();
        let fade = Style::new().add_modifier(Modifier::DIM);
        let rows: Vec<Line> = std::iter::repeat_n(Line::default(), padding)
            .chain(lines)
            .enumerate()
            .map(|(row, line)| {
                if fading && row < self.height / 3 {
                    line.patch_style(fade)
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(rows), feed_area);

        self.toast = (!following).then(|| self.draw_toast(frame, feed_area));
        self.speed = (!in_shell).then(|| self.draw_info(frame, info_area));
        frame.render_widget(Paragraph::new(bottom.clone()), bottom_area);
        if in_shell {
            let x = bottom_area.x + bottom.last().map_or(0, Line::width) as u16;
            let y = bottom_area.bottom().saturating_sub(1);
            frame.set_cursor_position((x.min(bottom_area.right().saturating_sub(1)), y));
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

    /// Asks the user's shell for its prompt in the current folder, in the
    /// background; the answer arrives as [`Msg::Prompt`].
    fn refresh_prompt(&self) {
        let (tx, cwd) = (self.tx.clone(), std::env::current_dir().unwrap_or_default());
        std::thread::spawn(move || tx.send(Msg::Prompt(prompt::render(&cwd))));
    }

    /// Multiplies the speed by `factor`, never going below one line a second.
    fn change_speed(&mut self, factor: f64) {
        self.feed.speed = (self.feed.speed * factor).max(1.0);
    }

    /// What's watched on the left, how it's going on the right. Returns
    /// where the speed was drawn.
    fn draw_info(&self, frame: &mut Frame, area: Rect) -> Rect {
        let c = &self.colors;
        let (status, speed_width) = self.status();
        let status_width = status.width() as u16;
        let [label_area, status_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(status_width)]).areas(area);
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
        if self.feed.paused {
            parts.push("paused".to_string());
        }
        if self.running.is_some() {
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

    /// The shortcuts nobody would guess, wrapped onto as many lines as `width` needs.
    fn keys(&self, width: u16) -> Vec<Line<'static>> {
        let pause = if self.feed.paused { "resume" } else { "pause" };
        // Scrolling keys are left out on purpose: arrows, PgUp/PgDn, Home/End
        // and the wheel do what everyone expects, and the toast names End.
        let keys = [
            ("space", pause),
            (":", "shell"),
            ("+/-", "speed"),
            ("c", "clear"),
            ("q", "quit"),
        ];
        let key_style = Style::new()
            .fg(self.colors.accent)
            .add_modifier(Modifier::BOLD);
        let widths = keys.map(|(key, action)| key.chars().count() + 1 + action.len());
        wrap(&widths, width as usize)
            .into_iter()
            .map(|row| {
                let mut spans = Vec::new();
                for (key, action) in &keys[row] {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(*key, key_style));
                    spans.push(Span::styled(format!(" {action}  "), self.colors.muted));
                }
                Line::from(spans)
            })
            .collect()
    }
}

/// Whether the mouse is over `area`.
fn hit(area: Option<Rect>, mouse: MouseEvent) -> bool {
    area.is_some_and(|a| a.contains(Position::new(mouse.column, mouse.row)))
}

/// Packs items of the given widths into rows no wider than `width`, in
/// order, each item followed by a 3-column gap. Returns the index range of
/// each row. An item wider than `width` still gets a row of its own.
fn wrap(widths: &[usize], width: usize) -> Vec<std::ops::Range<usize>> {
    let mut rows = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (i, w) in widths.iter().enumerate() {
        let need = w + 3;
        if used + need > width && i > start {
            rows.push(start..i);
            (start, used) = (i, 0);
        }
        used += need;
    }
    if start < widths.len() {
        rows.push(start..widths.len());
    }
    rows
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
    fn wrap_packs_rows_in_order() {
        let one_row = wrap(&[5, 5, 5], 100);
        assert_eq!((one_row.len(), one_row[0].clone()), (1, 0..3));
        assert_eq!(wrap(&[5, 5, 5], 16), [0..2, 2..3]);
        assert_eq!(wrap(&[5, 50, 5], 16), [0..1, 1..2, 2..3]);
        assert!(wrap(&[], 10).is_empty());
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("~/code/project", 20), "~/code/project");
        assert_eq!(tail("~/code/project", 8), "…project");
        assert_eq!(tail("~/code/project", 0), "");
    }
}
