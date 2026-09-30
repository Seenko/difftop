//! The terminal side: the frame loop, drawing, and keys.

use std::ops::ControlFlow;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::feed::Feed;
use crate::theme::UiColors;

/// About 30 frames per second: smooth enough, and cheap when idle.
const FRAME: Duration = Duration::from_millis(33);

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

        if event::poll(FRAME)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && app.on_key(key.code, key.modifiers).is_break()
        {
            return Ok(());
        }
    }
}

impl App {
    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> ControlFlow<()> {
        match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                return ControlFlow::Break(());
            }
            KeyCode::Char('q') | KeyCode::Esc => return ControlFlow::Break(()),
            KeyCode::Char(' ') | KeyCode::Char('p') => self.feed.paused = !self.feed.paused,
            KeyCode::Char('+') | KeyCode::Char('=') => self.feed.speed *= 1.5,
            KeyCode::Char('-') => self.feed.speed = (self.feed.speed / 1.5).max(1.0),
            KeyCode::Char('c') => self.feed.clear(),
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn draw(&self, frame: &mut Frame) {
        let c = &self.colors;
        let area = frame.area();
        frame.render_widget(
            Block::new().style(Style::new().fg(c.foreground).bg(c.background)),
            area,
        );
        let [feed_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);

        // Newest lines sit at the bottom; the top third fades out.
        let height = feed_area.height as usize;
        let lines: Vec<Line> = self.feed.newest(height).cloned().collect();
        let padding = height - lines.len();
        let fade = Style::new().add_modifier(Modifier::DIM);
        let rows: Vec<Line> = std::iter::repeat_n(Line::default(), padding)
            .chain(lines)
            .enumerate()
            .map(|(row, line)| {
                if row < height / 3 {
                    line.patch_style(fade)
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(rows), feed_area);

        let status = self.status();
        let status_width = status.width() as u16;
        let [label_area, status_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(status_width)])
                .areas(footer_area);
        // Leave a one-column gap before the status and keep the end of long
        // paths, which is the part that tells folders apart.
        let room = (label_area.width as usize).saturating_sub(2);
        let label = match &self.warning {
            Some(warning) => Line::styled(format!(" {warning}"), Style::new().fg(c.removed)),
            None => Line::styled(
                format!(" {}", tail(&self.label, room)),
                Style::new().fg(c.accent),
            ),
        };
        frame.render_widget(label, label_area);
        frame.render_widget(status, status_area);
    }

    /// Right side of the footer, most important first.
    fn status(&self) -> Line<'static> {
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
        parts.push(format!("{:.0}/s ", self.feed.speed));
        Line::from(Span::styled(parts.join(" · "), self.colors.muted))
    }
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
    fn tail_keeps_the_end() {
        assert_eq!(tail("~/code/project", 20), "~/code/project");
        assert_eq!(tail("~/code/project", 8), "…project");
        assert_eq!(tail("~/code/project", 0), "");
    }
}
