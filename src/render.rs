//! Turns a [`Change`] into the terminal lines that rise up the screen.

use std::path::Path;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::diff::{self, Kind};
use crate::highlight::{Highlighter, clean};
use crate::theme::UiColors;
use crate::watch::{Change, ChangeKind};

/// Longer diffs are cut off so one huge change can't flood the screen.
const MAX_LINES_PER_CHANGE: usize = 200;

pub struct Renderer {
    pub highlighter: Highlighter,
    pub colors: UiColors,
    pub context: usize,
}

impl Renderer {
    /// A header line, the diff (if there is one to show) and a blank spacer.
    pub fn render(&self, change: &Change) -> Vec<Line<'static>> {
        let c = &self.colors;
        let path = display_path(&change.path);
        let hunks = match change.kind {
            ChangeKind::Created | ChangeKind::Modified => {
                diff::hunks(&change.before, &change.after, self.context)
            }
            ChangeKind::Deleted | ChangeKind::FirstSeen => Vec::new(),
        };
        if change.kind == ChangeKind::Modified && hunks.is_empty() {
            return Vec::new();
        }

        let count = |kind| hunks.iter().flatten().filter(|l| l.kind == kind).count();
        let mut header = vec![
            Span::styled("▍ ", Style::new().fg(c.accent)),
            Span::styled(path, Style::new().fg(c.accent).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
        ];
        match change.kind {
            ChangeKind::Created => {
                header.push(Span::styled("new ", Style::new().fg(c.added)));
                header.push(Span::styled(
                    format!("+{}", count(Kind::Added)),
                    Style::new().fg(c.added),
                ));
            }
            ChangeKind::Modified => {
                header.push(Span::styled(
                    format!("+{}", count(Kind::Added)),
                    Style::new().fg(c.added),
                ));
                header.push(Span::styled(
                    format!(" −{}", count(Kind::Removed)),
                    Style::new().fg(c.removed),
                ));
            }
            ChangeKind::Deleted => header.push(Span::styled("deleted", Style::new().fg(c.removed))),
            ChangeKind::FirstSeen => {
                header.push(Span::styled("changed, diffing from now on", c.muted));
            }
        }

        let mut out = vec![Line::from(header)];
        out.extend(self.body(change, &hunks));
        out.push(Line::default());
        out
    }

    fn body(&self, change: &Change, hunks: &[Vec<diff::DiffLine>]) -> Vec<Line<'static>> {
        let c = &self.colors;
        let last_new = hunks
            .iter()
            .flatten()
            .filter(|l| l.kind != Kind::Removed)
            .map(|l| l.index)
            .max();
        let new_lines = last_new
            .map(|last| self.highlighter.lines(&change.path, &change.after, last))
            .unwrap_or_default();
        // Removed lines are shown as plain text in the "removed" color, so
        // the old file never needs highlighting.
        let old_lines: Vec<&str> = change.before.lines().collect();

        let mut out = Vec::new();
        for (i, hunk) in hunks.iter().enumerate() {
            if i > 0 {
                out.push(Line::styled("  ┈┈┈", c.muted));
            }
            for line in hunk {
                let row = match line.kind {
                    Kind::Added => gutter(
                        "+ ",
                        Style::new().fg(c.added),
                        new_lines[line.index].clone(),
                    ),
                    Kind::Context => gutter("  ", c.muted, new_lines[line.index].clone())
                        .patch_style(Style::new().add_modifier(Modifier::DIM)),
                    Kind::Removed => {
                        let text = clean(old_lines[line.index]);
                        let code = vec![Span::styled(text, Style::new().fg(c.removed))];
                        gutter("- ", Style::new().fg(c.removed), code)
                            .patch_style(Style::new().add_modifier(Modifier::DIM))
                    }
                };
                out.push(row);
            }
        }
        if out.len() > MAX_LINES_PER_CHANGE {
            let hidden = out.len() - MAX_LINES_PER_CHANGE;
            out.truncate(MAX_LINES_PER_CHANGE);
            out.push(Line::styled(format!("  … {hidden} more lines"), c.muted));
        }
        out
    }
}

fn gutter(mark: &'static str, style: Style, code: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(mark, style)];
    spans.extend(code);
    Line::from(spans)
}

/// Relative to the current directory when inside it, otherwise see [`tilde`].
/// Control characters in names are defused like file content.
pub fn display_path(path: &Path) -> String {
    let cwd = std::env::current_dir().ok();
    match cwd.as_deref().and_then(|cwd| path.strip_prefix(cwd).ok()) {
        Some(rel) => clean(&rel.display().to_string()),
        None => tilde(path),
    }
}

/// `~/...` when inside the home folder, absolute otherwise.
pub fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rel) if rel.as_os_str().is_empty() => "~".into(),
        Some(rel) => clean(&format!("~/{}", rel.display())),
        None => clean(&path.display().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn renderer() -> Renderer {
        let theme = crate::theme::load("ansi").unwrap();
        Renderer {
            colors: UiColors::from_theme(&theme),
            highlighter: Highlighter::new(theme),
            context: 1,
        }
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn change(kind: ChangeKind, before: &str, after: &str) -> Change {
        Change {
            path: PathBuf::from("/nowhere/lib.rs"),
            kind,
            before: before.into(),
            after: after.into(),
        }
    }

    #[test]
    fn modified_shows_header_diff_and_spacer() {
        let lines = renderer().render(&change(ChangeKind::Modified, "a\nb\nc\n", "a\nB\nc\n"));
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            texts,
            ["▍ /nowhere/lib.rs  +1 −1", "  a", "- b", "+ B", "  c", ""]
        );
    }

    #[test]
    fn whitespace_only_noop_and_deleted() {
        let r = renderer();
        assert!(
            r.render(&change(ChangeKind::Modified, "a\n", "a\n"))
                .is_empty()
        );
        let deleted = r.render(&change(ChangeKind::Deleted, "a\n", ""));
        assert_eq!(text(&deleted[0]), "▍ /nowhere/lib.rs  deleted");
        assert_eq!(deleted.len(), 2);
    }

    #[test]
    fn huge_changes_are_cut_off() {
        let after: String = (0..500).map(|i| format!("{i}\n")).collect();
        let lines = renderer().render(&change(ChangeKind::Created, "", &after));
        // header + capped body + "more" note + spacer
        assert_eq!(lines.len(), 1 + MAX_LINES_PER_CHANGE + 1 + 1);
        assert_eq!(text(&lines[lines.len() - 2]), "  … 300 more lines");
    }
}
