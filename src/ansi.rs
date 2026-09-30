//! Turns text with ANSI color codes (a shell prompt, command output) into
//! styled lines.
//!
//! Only SGR codes (`ESC [ ... m`: colors, bold, ...) are understood. Every
//! other escape sequence and control character is dropped, so nothing in the
//! text can send commands to the terminal.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const ESC: char = '\x1b';
const BEL: char = '\x07';

/// One [`Line`] per line of `text`.
pub fn lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    let mut style = Style::new();
    let mut piece = String::new();
    let mut chars = text.chars().peekable();

    // Ends the current run of same-styled text.
    let flush = |lines: &mut Vec<Line<'static>>, piece: &mut String, style: Style| {
        if !piece.is_empty() {
            let line = lines.last_mut().expect("never empty");
            line.spans.push(Span::styled(std::mem::take(piece), style));
        }
    };

    while let Some(ch) = chars.next() {
        match ch {
            '\n' => {
                flush(&mut lines, &mut piece, style);
                lines.push(Line::default());
            }
            '\t' => piece.push_str("    "),
            ESC => match chars.next() {
                // CSI: parameters, then one final letter. Only `m` matters.
                Some('[') => {
                    let mut params = String::new();
                    let fin = chars.by_ref().find(|c| {
                        let done = ('@'..='~').contains(c);
                        if !done {
                            params.push(*c);
                        }
                        done
                    });
                    if fin == Some('m') {
                        flush(&mut lines, &mut piece, style);
                        style = sgr(style, &params);
                    }
                }
                // OSC (e.g. setting the window title): skip to BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == BEL || (c == ESC && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            ch if ch.is_control() => {}
            ch => piece.push(ch),
        }
    }
    flush(&mut lines, &mut piece, style);
    lines
}

/// Applies one SGR sequence (the part between `ESC [` and `m`) to `style`.
fn sgr(mut style: Style, params: &str) -> Style {
    // `ESC [ m` is a reset, same as `ESC [ 0 m`.
    if params.is_empty() {
        return Style::new();
    }
    let mut codes = params
        .split([';', ':'])
        .map(|p| p.parse::<u8>().unwrap_or(0));
    while let Some(code) = codes.next() {
        style = match code {
            0 => Style::new(),
            1 => style.add_modifier(Modifier::BOLD),
            2 => style.add_modifier(Modifier::DIM),
            3 => style.add_modifier(Modifier::ITALIC),
            4 => style.add_modifier(Modifier::UNDERLINED),
            7 => style.add_modifier(Modifier::REVERSED),
            9 => style.add_modifier(Modifier::CROSSED_OUT),
            22 => style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style.remove_modifier(Modifier::ITALIC),
            24 => style.remove_modifier(Modifier::UNDERLINED),
            27 => style.remove_modifier(Modifier::REVERSED),
            29 => style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style.fg(Color::Indexed(code - 30)),
            90..=97 => style.fg(Color::Indexed(code - 90 + 8)),
            40..=47 => style.bg(Color::Indexed(code - 40)),
            100..=107 => style.bg(Color::Indexed(code - 100 + 8)),
            39 => style.fg(Color::Reset),
            49 => style.bg(Color::Reset),
            38 | 48 => {
                let color = match codes.next() {
                    Some(5) => codes.next().map(Color::Indexed),
                    Some(2) => match (codes.next(), codes.next(), codes.next()) {
                        (Some(r), Some(g), Some(b)) => Some(Color::Rgb(r, g, b)),
                        _ => None,
                    },
                    _ => None,
                };
                match (code, color) {
                    (38, Some(color)) => style.fg(color),
                    (_, Some(color)) => style.bg(color),
                    _ => style,
                }
            }
            _ => style,
        };
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn renders_a_debian_bash_prompt() {
        // What `${PS1@P}` gives for Debian's default colored PS1, window
        // title and the \x01/\x02 markers included.
        let ps1 = "\x01\x1b]0;seenko: ~\x07\x02\x01\x1b[01;32m\x02seenko\x01\x1b[00m\x02:\x01\x1b[01;34m\x02~\x01\x1b[00m\x02$ ";
        let lines = lines(ps1);
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "seenko:~$ ");
        let green = Style::new()
            .add_modifier(Modifier::BOLD)
            .fg(Color::Indexed(2));
        assert_eq!(lines[0].spans[0], Span::styled("seenko", green));
        assert_eq!(lines[0].spans[1], Span::styled(":", Style::new()));
    }

    #[test]
    fn colors_and_resets() {
        let line = &lines("\x1b[38;5;208mA\x1b[38;2;1;2;3;1mB\x1b[39;22mC\x1b[mD")[0];
        assert_eq!(line.spans[0].style, Style::new().fg(Color::Indexed(208)));
        assert_eq!(
            line.spans[1].style,
            Style::new()
                .fg(Color::Rgb(1, 2, 3))
                .add_modifier(Modifier::BOLD)
        );
        let unbolded = line.spans[2].style;
        assert_eq!(unbolded.fg, Some(Color::Reset));
        assert!(!unbolded.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[3].style, Style::new());
    }

    #[test]
    fn drops_everything_that_is_not_a_color() {
        let lines = lines("a\x1b[2Jb\x1b]0;title\x1b\\c\x07\x08\td\re\nf");
        assert_eq!(
            lines.iter().map(text).collect::<Vec<_>>(),
            ["abc    de", "f"]
        );
    }
}
