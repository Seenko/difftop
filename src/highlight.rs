//! Syntax highlighting with bat's language definitions (via two-face).

use std::path::Path;

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::theme::to_color;

/// A highlighted line: styled pieces of text, no trailing newline.
pub type StyledLine = Vec<Span<'static>>;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new(theme: Theme) -> Self {
        Self {
            syntaxes: two_face::syntax::extra_newlines(),
            theme,
        }
    }

    /// Highlights `text` from the top down to line `last` (0-based, inclusive).
    ///
    /// Always starting at the top is what keeps highlighting correct inside
    /// multi-line strings and comments; stopping at `last` keeps it cheap when
    /// only the start of a big file changed.
    pub fn lines(&self, path: &Path, text: &str, last: usize) -> Vec<StyledLine> {
        let mut state = HighlightLines::new(self.syntax_for(path, text), &self.theme);
        LinesWithEndings::from(text)
            .take(last + 1)
            .map(|line| match state.highlight_line(line, &self.syntaxes) {
                Ok(pieces) => pieces
                    .into_iter()
                    .map(|(style, piece)| Span::styled(clean(piece), to_style(style)))
                    .collect(),
                // A broken syntax definition shouldn't hide the change.
                Err(_) => vec![Span::raw(clean(line))],
            })
            .collect()
    }

    fn syntax_for(&self, path: &Path, text: &str) -> &SyntaxReference {
        let by_name = |name: Option<&std::ffi::OsStr>| {
            name.and_then(|n| n.to_str())
                .and_then(|n| self.syntaxes.find_syntax_by_extension(n))
        };
        by_name(path.file_name())
            .or_else(|| by_name(path.extension()))
            .or_else(|| {
                let first = text.lines().next().unwrap_or("");
                self.syntaxes.find_syntax_by_first_line(first)
            })
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }
}

/// Makes file content safe and predictable to print: strips the line ending,
/// expands tabs, and replaces control characters with visible symbols (`␛`
/// for escape) so a watched file can never send commands to the terminal.
pub fn clean(piece: &str) -> String {
    let mut out = String::with_capacity(piece.len());
    for ch in piece.trim_end_matches(['\n', '\r']).chars() {
        match ch {
            '\t' => out.push_str("    "),
            // C0 controls have printable "control picture" twins at U+2400.
            '\0'..='\x1f' => out.push(char::from_u32(0x2400 + ch as u32).unwrap_or('�')),
            '\x7f' => out.push('␡'),
            ch if ch.is_control() => out.push('�'),
            ch => out.push(ch),
        }
    }
    out
}

fn to_style(style: syntect::highlighting::Style) -> Style {
    let mut out = Style::default();
    if let Some(fg) = to_color(style.foreground) {
        out = out.fg(fg);
    }
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(line: &StyledLine) -> String {
        line.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn highlights_rust_and_keeps_text() {
        let h = Highlighter::new(crate::theme::load("Monokai Extended").unwrap());
        let src = "fn main() {\n\tlet x = 1;\n}\n";
        let lines = h.lines(Path::new("main.rs"), src, 1);
        assert_eq!(lines.len(), 2);
        assert_eq!(text_of(&lines[0]), "fn main() {");
        assert_eq!(text_of(&lines[1]), "    let x = 1;");
        // A real syntax was picked: `fn` is colored differently from `main`.
        assert_ne!(lines[0][0].style, lines[0].last().unwrap().style);
    }

    #[test]
    fn clean_defuses_terminal_escapes() {
        assert_eq!(clean("\x1b[2Jboom\u{9b}\x7f\tx\r\n"), "␛[2Jboom�␡    x");
        assert_eq!(clean("plain ünïcödé"), "plain ünïcödé");
    }

    #[test]
    fn detects_by_file_name_and_shebang() {
        let h = Highlighter::new(crate::theme::load("ansi").unwrap());
        assert_eq!(h.syntax_for(Path::new("Makefile"), "").name, "Makefile");
        assert_eq!(
            h.syntax_for(Path::new("run"), "#!/bin/bash\n").name,
            "Bourne Again Shell (bash)"
        );
        assert_eq!(h.syntax_for(Path::new("notes"), "hi").name, "Plain Text");
    }
}
