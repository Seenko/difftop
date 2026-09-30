//! Turns a [`Change`] into the terminal lines that rise up the screen.

use std::cmp::Ordering;
use std::path::Path;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::diff::{self, Kind};
use crate::highlight::{Highlighter, clean};
use crate::theme::UiColors;
#[cfg(test)]
use crate::watch::Snapshot;
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
    /// Files that can't be diffed (binary, too big, or not remembered yet)
    /// still get a header, showing how their size changed.
    pub fn render(&self, change: &Change) -> Vec<Line<'static>> {
        let c = &self.colors;
        let (before, after) = (&change.before, &change.after);
        let texts = match change.kind {
            ChangeKind::Created => after.text.as_deref().map(|new| ("", new)),
            ChangeKind::Modified => before.text.as_deref().zip(after.text.as_deref()),
            ChangeKind::Deleted => None,
        };
        let hunks = texts.map(|(old, new)| diff::hunks(old, new, self.context));
        if change.kind == ChangeKind::Modified && hunks.as_ref().is_some_and(Vec::is_empty) {
            return Vec::new();
        }

        let green = |text: String| Span::styled(text, Style::new().fg(c.added));
        let red = |text: String| Span::styled(text, Style::new().fg(c.removed));
        let count = |hunks: &[Vec<diff::DiffLine>], kind| {
            hunks.iter().flatten().filter(|l| l.kind == kind).count()
        };
        let mut header = vec![
            Span::styled("▍ ", Style::new().fg(c.accent)),
            Span::styled(
                display_path(&change.path),
                Style::new().fg(c.accent).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
        ];
        match (change.kind, &hunks) {
            (ChangeKind::Created, Some(h)) => {
                header.push(green(format!("new +{}", count(h, Kind::Added))));
            }
            (ChangeKind::Created, None) => header.push(green(format!("new {}", size(after.size)))),
            (ChangeKind::Modified, Some(h)) => {
                header.push(green(format!("+{}", count(h, Kind::Added))));
                header.push(red(format!(" −{}", count(h, Kind::Removed))));
            }
            (ChangeKind::Modified, None) => {
                let sizes = format!("{} → {}  ", size(before.size), size(after.size));
                header.push(Span::styled(sizes, c.muted));
                header.push(match after.size.cmp(&before.size) {
                    Ordering::Greater => green(format!("+{}", size(after.size - before.size))),
                    Ordering::Less => red(format!("−{}", size(before.size - after.size))),
                    Ordering::Equal => Span::styled("±0 B", c.muted),
                });
            }
            (ChangeKind::Deleted, _) => header.push(red(format!("deleted {}", size(before.size)))),
        }

        let mut out = vec![Line::from(header)];
        if let (Some((old, new)), Some(hunks)) = (texts, &hunks) {
            out.extend(self.body(&change.path, old, new, hunks));
        }
        out.push(Line::default());
        out
    }

    fn body(
        &self,
        path: &Path,
        old: &str,
        new: &str,
        hunks: &[Vec<diff::DiffLine>],
    ) -> Vec<Line<'static>> {
        let c = &self.colors;
        let last_new = hunks
            .iter()
            .flatten()
            .filter(|l| l.kind != Kind::Removed)
            .map(|l| l.index)
            .max();
        let new_lines = last_new
            .map(|last| self.highlighter.lines(path, new, last))
            .unwrap_or_default();
        // Removed lines are shown as plain text in the "removed" color, so
        // the old file never needs highlighting.
        let old_lines: Vec<&str> = old.lines().collect();

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

/// A byte count for people, to three significant digits so that nearby
/// sizes still read differently: `512 B`, `1.26 MiB`, `14.1 KiB`, `143 KiB`.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ => {
            let decimals = if value < 10.0 {
                2
            } else if value < 100.0 {
                1
            } else {
                0
            };
            format!("{value:.decimals$} {}", UNITS[unit])
        }
    }
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

    fn snapshot(size: u64, text: Option<&str>) -> Snapshot {
        Snapshot {
            size,
            text: text.map(String::from),
            modified: None,
        }
    }

    fn change(kind: ChangeKind, before: &str, after: &str) -> Change {
        Change {
            path: PathBuf::from("/nowhere/lib.rs"),
            kind,
            before: snapshot(before.len() as u64, Some(before)),
            after: snapshot(after.len() as u64, Some(after)),
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
        assert_eq!(text(&deleted[0]), "▍ /nowhere/lib.rs  deleted 2 B");
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

    #[test]
    fn undiffable_files_show_their_size_change() {
        let r = renderer();
        let binary = |before, after| Change {
            path: PathBuf::from("/nowhere/logo.png"),
            kind: ChangeKind::Modified,
            before: snapshot(before, None),
            after: snapshot(after, None),
        };
        let lines = r.render(&binary(12 * 1024, 14 * 1024 + 100));
        assert_eq!(
            text(&lines[0]),
            "▍ /nowhere/logo.png  12.0 KiB → 14.1 KiB  +2.10 KiB"
        );
        assert_eq!(lines.len(), 2);
        let shrunk = r.render(&binary(2048, 1000));
        assert!(text(&shrunk[0]).ends_with("2.00 KiB → 1000 B  −1.02 KiB"));

        let mut created = binary(0, 3 << 20);
        created.kind = ChangeKind::Created;
        assert_eq!(
            text(&r.render(&created)[0]),
            "▍ /nowhere/logo.png  new 3.00 MiB"
        );
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.50 KiB");
        assert_eq!(size(12 * 1024 + 100), "12.1 KiB");
        assert_eq!(size(150 * 1024), "150 KiB");
        assert_eq!(size(5 << 30), "5.00 GiB");
    }
}
