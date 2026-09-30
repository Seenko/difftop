//! The terminal side: setup, the frame loop, drawing, keys, mouse, and the
//! `:` command line.

use std::io;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::command::{self, Command};
use crate::feed::Feed;
use crate::render::tilde;
use crate::theme::UiColors;
use crate::watch;

/// About 30 frames per second: smooth enough, and cheap when idle.
const FRAME: Duration = Duration::from_millis(33);
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
    Ready { files: usize },
    Warning(String),
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
}

/// Takes over the terminal. Mouse reporting makes the wheel scroll, and
/// bracketed paste makes pasted text (a right-click in many terminals)
/// arrive as one paste event instead of a stream of fake keypresses.
pub fn start() -> Result<DefaultTerminal> {
    let terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        stop();
        previous(info);
    }));
    Ok(terminal)
}

/// Gives the terminal back as it was.
pub fn stop() {
    let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
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
            }
        }
        let now = Instant::now();
        app.feed.tick(now - last);
        last = now;

        terminal.draw(|frame| app.draw(frame))?;

        if event::poll(FRAME)? {
            let flow = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                Event::Mouse(mouse) => app.on_mouse(mouse),
                // Pasting a path into the command line is handy; anywhere
                // else a paste (a right-click in many terminals) is ignored.
                Event::Paste(text) => {
                    if let Some(input) = &mut app.input {
                        input.push_str(text.trim_end_matches(['\n', '\r']));
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
        label: String,
        watcher: watch::Handle,
    ) -> Self {
        Self {
            feed,
            colors,
            label,
            files: None,
            warning: None,
            height: 0,
            toast: None,
            speed: None,
            watcher,
            input: None,
            previous: None,
            notice: None,
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> ControlFlow<()> {
        if self.input.is_some() {
            return self.on_input_key(key);
        }
        let page = self.height.max(1) as isize;
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return ControlFlow::Break(());
            }
            // Esc deliberately doesn't quit: it closes the command line, and
            // pressing it once too often shouldn't end the session.
            KeyCode::Char('q') => return ControlFlow::Break(()),
            KeyCode::Char(':') | KeyCode::Char('!') => self.input = Some(String::new()),
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

    /// Keys while the `:` line is open: editing, Tab completion, Enter to run.
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
            KeyCode::Enter => {
                let line = self.input.take().unwrap_or_default();
                return self.run_command(&line);
            }
            KeyCode::Char(ch) if !ctrl => input.push(ch),
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn run_command(&mut self, line: &str) -> ControlFlow<()> {
        let result = match command::parse(line) {
            Ok(Command::Quit) => return ControlFlow::Break(()),
            Ok(Command::Cd(arg)) => {
                command::resolve(&arg, self.previous.as_deref()).and_then(|dir| self.cd(dir))
            }
            Err(err) => Err(err),
        };
        if let Err(err) = result {
            self.notice = Some((err, Instant::now()));
        }
        ControlFlow::Continue(())
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
        self.feed.push([
            Line::default(),
            Line::styled(
                format!("── now watching {} ──", self.label),
                self.colors.muted,
            ),
            Line::default(),
        ]);
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
        let keys = match &self.input {
            Some(input) => vec![Line::from(vec![
                Span::styled(" :", Style::new().fg(c.accent).add_modifier(Modifier::BOLD)),
                Span::raw(input.clone()),
            ])],
            None => self.keys(area.width),
        };
        let [feed_area, info_area, keys_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(keys.len() as u16),
        ])
        .areas(area);

        // Lines sit at the bottom. While following, the top third fades out;
        // a scrolled view stays at full strength for reading.
        self.height = feed_area.height as usize;
        let following = self.feed.below() == 0;
        let lines: Vec<Line> = self.feed.view(self.height).cloned().collect();
        let padding = self.height - lines.len();
        let fade = Style::new().add_modifier(Modifier::DIM);
        let rows: Vec<Line> = std::iter::repeat_n(Line::default(), padding)
            .chain(lines)
            .enumerate()
            .map(|(row, line)| {
                if following && row < self.height / 3 {
                    line.patch_style(fade)
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(rows), feed_area);

        self.toast = (!following).then(|| self.draw_toast(frame, feed_area));
        self.speed = Some(self.draw_info(frame, info_area));
        frame.render_widget(Paragraph::new(keys), keys_area);
        if let Some(input) = &self.input {
            let x = keys_area.x + 2 + input.chars().count() as u16;
            frame.set_cursor_position((x.min(keys_area.right().saturating_sub(1)), keys_area.y));
        }
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
        let label = match notice.map(|(text, _)| text).or(self.warning.as_ref()) {
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
            (":", "command"),
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
