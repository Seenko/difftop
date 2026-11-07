//! Turns a [`Change`] into the terminal lines that rise up the screen.

use std::cmp::Ordering;
use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::bytemap::{ByteMap, State};
use crate::diff::{self, Kind};
use crate::highlight::{Highlighter, clean};
use crate::theme::{self, UiColors};
#[cfg(test)]
use crate::watch::Snapshot;
use crate::watch::{Change, ChangeKind};

/// Everything under a header is indented this much, like Claude Code's.
const INDENT: &str = "  ";

/// Longer diffs are cut off so one huge change can't flood the screen.
const MAX_LINES_PER_CHANGE: usize = 200;

pub struct Renderer {
    pub highlighter: Highlighter,
    pub colors: UiColors,
    pub context: usize,
    pub line_numbers: bool,
    pub change_background: bool,
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

        let count = |hunks: &[Vec<diff::DiffLine>], kind| {
            hunks.iter().flatten().filter(|l| l.kind == kind).count()
        };
        // Like Claude Code's: what happened, the path, and the numbers in
        // quiet parentheses.
        let (verb, details) = match (change.kind, &hunks) {
            (ChangeKind::Created, Some(h)) => ("Created", format!("+{}", count(h, Kind::Added))),
            (ChangeKind::Created, None) => ("Created", size(after.size)),
            (ChangeKind::Modified, Some(h)) => (
                "Updated",
                format!("+{} −{}", count(h, Kind::Added), count(h, Kind::Removed)),
            ),
            (ChangeKind::Modified, None) => {
                let delta = match after.size.cmp(&before.size) {
                    Ordering::Greater => format!("+{}", size(after.size - before.size)),
                    Ordering::Less => format!("−{}", size(before.size - after.size)),
                    Ordering::Equal => "±0 B".into(),
                };
                let sizes = format!("{} → {}, {delta}", size(before.size), size(after.size));
                ("Updated", sizes)
            }
            (ChangeKind::Deleted, _) => ("Deleted", size(before.size)),
        };
        let header = vec![
            Span::styled(format!("{verb} "), Style::new().fg(c.foreground)),
            Span::styled(
                display_path(&change.path),
                Style::new().fg(c.foreground).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" ({details})"), c.muted),
        ];

        let mut out = vec![Line::from(header)];
        if let (Some((old, new)), Some(hunks)) = (texts, &hunks) {
            // A new file is all additions: tinting every line says nothing
            // and floods the screen with green, so only edits are tinted.
            let tinted = self.change_background && change.kind == ChangeKind::Modified;
            out.extend(self.body(&change.path, old, new, hunks, tinted));
        }
        if let Some(map) = &change.map {
            out.extend(self.byte_map(map));
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
        tinted: bool,
    ) -> Vec<Line<'static>> {
        let c = &self.colors;
        let last_new = hunks
            .iter()
            .flatten()
            .filter(|l| l.kind != Kind::Removed)
            .map(|l| l.index)
            .max();
        let last_old = hunks
            .iter()
            .flatten()
            .filter(|l| l.kind == Kind::Removed)
            .map(|l| l.index)
            .max();
        let highlight = |text, last: Option<usize>| {
            last.map_or_else(Vec::new, |last| self.highlighter.lines(path, text, last))
        };
        let (new_lines, old_lines) = (highlight(new, last_new), highlight(old, last_old));
        // Every row's number gets the same width, so the code lines up.
        let width = match self.line_numbers {
            true => last_new
                .max(last_old)
                .map_or(0, |i| (i + 1).to_string().len() + 1),
            false => 0,
        };

        let mut out = Vec::new();
        for (i, hunk) in hunks.iter().enumerate() {
            if i > 0 {
                out.push(Line::styled(format!("{INDENT}{:width$}  ┈┈┈", ""), c.muted));
            }
            for line in hunk {
                let (mark, code, tint) = match line.kind {
                    Kind::Added => ("+", &new_lines[line.index], Some((c.added, c.added_bg))),
                    Kind::Removed => ("-", &old_lines[line.index], Some((c.removed, c.removed_bg))),
                    Kind::Context => (" ", &new_lines[line.index], None),
                };
                let number = match self.line_numbers {
                    true => format!("{:>w$} ", line.index + 1, w = width - 1),
                    false => String::new(),
                };
                // The indent keeps the screen's own background, so a
                // tinted row starts where its text does.
                let indent = Span::styled(INDENT, Style::new().bg(c.background));
                let mut row = Line::from(vec![indent, Span::styled(number, c.muted)]);
                match tint {
                    // The tint fills the whole row and leaves the syntax
                    // colors on the text alone.
                    Some((fg, bg)) if tinted => {
                        // On the tint, a syntax color close to it (green text
                        // on green) becomes the normal text color instead.
                        let readable = |color: Color| match theme::readable_on(color, bg) {
                            true => color,
                            false => c.foreground,
                        };
                        let mark = Span::styled(format!("{mark} "), Style::new().fg(readable(fg)));
                        row.push_span(mark);
                        row.extend(code.iter().map(|span| {
                            let mut span = span.clone();
                            span.style.fg = span.style.fg.map(readable);
                            span
                        }));
                        row = row.style(Style::new().bg(bg));
                    }
                    Some((fg, _)) => {
                        row.push_span(Span::styled(format!("{mark} "), Style::new().fg(fg)));
                        row.extend(code.iter().cloned());
                    }
                    // Unchanged lines keep their full colors: dimming them on
                    // top of a dark palette made them unreadable, and the
                    // tint already says what changed.
                    None => {
                        row.push_span(Span::raw("  "));
                        row.extend(code.iter().cloned());
                    }
                }
                out.push(row);
            }
        }
        if out.len() > MAX_LINES_PER_CHANGE {
            let hidden = out.len() - MAX_LINES_PER_CHANGE;
            out.truncate(MAX_LINES_PER_CHANGE);
            out.push(Line::styled(
                format!("{INDENT}  … {hidden} more lines"),
                c.muted,
            ));
        }
        out
    }
}

impl Renderer {
    /// A binary file as a bar from its start to its end, lit where it
    /// changed, with the offset where each change starts written below.
    fn byte_map(&self, map: &ByteMap) -> Vec<Line<'static>> {
        let c = &self.colors;
        let color = |state| match state {
            State::Same => None,
            State::Changed => Some(c.changed),
            State::Added => Some(c.added),
            State::Removed => Some(c.removed),
        };
        let mut bar = vec![Span::raw(INDENT)];
        for cell in &map.cells {
            bar.push(match color(cell.state) {
                Some(fg) => Span::styled("█", Style::new().fg(fg)),
                // How random the bytes are, as texture: the "art" of the
                // map. Never a full block, which is what a change looks like.
                None => Span::styled(["·", "░", "▒", "▓"][cell.texture.min(3) as usize], c.muted),
            });
        }

        let width = map.cells.len();
        let mut labels: Vec<(usize, String, Style)> = vec![(0, "0".into(), c.muted)];
        let starts = map.cells.iter().enumerate().filter(|(i, cell)| {
            cell.state != State::Same && (*i == 0 || map.cells[i - 1].state != cell.state)
        });
        for (i, cell) in starts {
            let offset = match cell.state {
                State::Added | State::Removed => map.shared,
                _ => i as u64 * map.cell_size,
            };
            let text = format!("↑{}", size(offset));
            labels.push((
                i,
                text,
                Style::new().fg(color(cell.state).unwrap_or_default()),
            ));
        }
        let end = size(map.len);
        labels.push((width.saturating_sub(end.chars().count()), end, c.muted));
        let mut axis = vec![Span::raw(INDENT)];
        let mut column = 0;
        for (at, text, style) in labels {
            let len = text.chars().count();
            // An arrow must point at its change, so labels are never moved
            // and may run past the bar's right end; one that would touch
            // the label before it is left out.
            if at < column + usize::from(column > 0) {
                continue;
            }
            axis.push(Span::raw(" ".repeat(at - column)));
            axis.push(Span::styled(text, style));
            column = at + len;
        }
        vec![Line::from(bar), Line::from(axis)]
    }
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
            line_numbers: true,
            change_background: true,
        }
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn snapshot(size: u64, text: Option<&str>) -> Snapshot {
        Snapshot {
            size,
            text: text.map(String::from),
            ..Snapshot::default()
        }
    }

    fn change(kind: ChangeKind, before: &str, after: &str) -> Change {
        Change {
            path: PathBuf::from("/nowhere/lib.rs"),
            kind,
            before: snapshot(before.len() as u64, Some(before)),
            after: snapshot(after.len() as u64, Some(after)),
            map: None,
        }
    }

    #[test]
    fn modified_shows_header_diff_and_spacer() {
        let lines = renderer().render(&change(ChangeKind::Modified, "a\nb\nc\n", "a\nB\nc\n"));
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            texts,
            [
                "Updated /nowhere/lib.rs (+1 −1)",
                "  1   a",
                "  2 - b",
                "  2 + B",
                "  3   c",
                ""
            ]
        );
    }

    #[test]
    fn line_numbers_and_background_can_be_turned_off() {
        let modified = change(ChangeKind::Modified, "a\nb\n", "a\nB\n");
        let r = renderer();
        let added = &r.render(&modified)[3];
        assert_eq!(added.style.bg, Some(r.colors.added_bg));
        // The syntax colors stay on the text, and the indent keeps the
        // screen's background so the tint starts where the text does.
        assert_eq!(added.spans[0].style.bg, Some(r.colors.background));
        assert!(added.spans[1..].iter().all(|s| s.style.bg.is_none()));

        let r = Renderer {
            line_numbers: false,
            change_background: false,
            ..renderer()
        };
        let lines = r.render(&modified);
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            texts,
            [
                "Updated /nowhere/lib.rs (+1 −1)",
                "    a",
                "  - b",
                "  + B",
                ""
            ]
        );
        assert_eq!(lines[3].style.bg, None);
    }

    #[test]
    fn progress_bar_output_with_lone_carriage_returns_renders() {
        // A log that redraws a progress bar with `\r`, as tool output does.
        let before = "start\n10%\r20%\n";
        let after = "start\n10%\r20%\r30%\n";
        let lines = renderer().render(&change(ChangeKind::Modified, before, after));
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(texts[2], "  2 - 10%␍20%");
        assert_eq!(texts[3], "  2 + 10%␍20%␍30%");
    }

    #[test]
    fn tinted_rows_swap_unreadable_colors_and_new_files_arent_tinted() {
        let r = renderer();
        let modified = r.render(&change(ChangeKind::Modified, "a\n", "b\n"));
        let added = &modified[2];
        assert_eq!(added.style.bg, Some(r.colors.added_bg));
        // The "+" would be green on green: it takes the text color.
        let mark = added.spans.iter().find(|s| s.content == "+ ").unwrap();
        assert_eq!(mark.style.fg, Some(r.colors.foreground));

        let created = r.render(&change(ChangeKind::Created, "", "a\nb\n"));
        assert!(created.iter().all(|l| l.style.bg.is_none()));
        let mark = created[1].spans.iter().find(|s| s.content == "+ ").unwrap();
        assert_eq!(mark.style.fg, Some(r.colors.added));
    }

    #[test]
    fn whitespace_only_noop_and_deleted() {
        let r = renderer();
        assert!(
            r.render(&change(ChangeKind::Modified, "a\n", "a\n"))
                .is_empty()
        );
        let deleted = r.render(&change(ChangeKind::Deleted, "a\n", ""));
        assert_eq!(text(&deleted[0]), "Deleted /nowhere/lib.rs (2 B)");
        assert_eq!(deleted.len(), 2);
    }

    #[test]
    fn huge_changes_are_cut_off() {
        let after: String = (0..500).map(|i| format!("{i}\n")).collect();
        let lines = renderer().render(&change(ChangeKind::Created, "", &after));
        // header + capped body + "more" note + spacer
        assert_eq!(lines.len(), 1 + MAX_LINES_PER_CHANGE + 1 + 1);
        assert_eq!(text(&lines[lines.len() - 2]), "    … 300 more lines");
    }

    #[test]
    fn undiffable_files_show_their_size_change() {
        let r = renderer();
        let binary = |before, after| Change {
            path: PathBuf::from("/nowhere/logo.png"),
            kind: ChangeKind::Modified,
            before: snapshot(before, None),
            after: snapshot(after, None),
            map: None,
        };
        let lines = r.render(&binary(12 * 1024, 14 * 1024 + 100));
        assert_eq!(
            text(&lines[0]),
            "Updated /nowhere/logo.png (12.0 KiB → 14.1 KiB, +2.10 KiB)"
        );
        assert_eq!(lines.len(), 2);
        let shrunk = r.render(&binary(2048, 1000));
        assert!(text(&shrunk[0]).ends_with("(2.00 KiB → 1000 B, −1.02 KiB)"));

        let mut created = binary(0, 3 << 20);
        created.kind = ChangeKind::Created;
        assert_eq!(
            text(&r.render(&created)[0]),
            "Created /nowhere/logo.png (3.00 MiB)"
        );
    }

    #[test]
    fn binary_changes_draw_a_map_with_offsets() {
        use crate::bytemap::Cell;
        let cell = |state, texture| Cell { state, texture };
        let mut cells = vec![cell(State::Same, 1); 40];
        cells[10] = cell(State::Changed, 1);
        cells[11] = cell(State::Changed, 1);
        cells[36..].fill(cell(State::Added, 0));
        let mut change = change(ChangeKind::Modified, "", "");
        (change.before.text, change.after.text) = (None, None);
        change.map = Some(ByteMap {
            cells,
            cell_size: 1024,
            len: 40 * 1024,
            shared: 36 * 1024 - 100,
        });
        let lines = renderer().render(&change);
        let bar = format!("  {}██{}████", "░".repeat(10), "░".repeat(24));
        assert_eq!(text(&lines[1]), bar);
        // `0`, where each change starts, and the size; the size doesn't fit
        // next to the last label, so it's left out rather than overlapping.
        assert_eq!(
            text(&lines[2]),
            format!("  0{}↑10.0 KiB{}↑35.9 KiB", " ".repeat(9), " ".repeat(17))
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
