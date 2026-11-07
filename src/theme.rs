//! Finding, loading and converting themes.
//!
//! A theme can come from three places, checked in this order:
//! 1. a path to a `.tmTheme` or base16/base24 `.yaml` file,
//! 2. a file in the user theme folder (`~/.config/difftop/themes`, searched
//!    recursively, matched by file name without extension),
//! 3. the themes built in: `matrix`, and those bundled from bat via two-face
//!    (`ansi`, `Dracula`, `Nord`, ...).
//!
//! Names are matched loosely: case, spaces, dashes, dots, underscores and
//! parentheses are ignored, so `solarized-dark` finds `Solarized (dark)`.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use ratatui::style::{Color, Modifier, Style};
use syntect::highlighting::{
    Color as SynColor, Highlighter, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSet,
    ThemeSettings,
};
use syntect::parsing::Scope;
use two_face::theme::EmbeddedLazyThemeSet;

const MATRIX_NAME: &str = "matrix";
/// Claude Code's look: its syntax colors (Monokai Extended) and its diff
/// colors, taken from Claude Code's dark theme.
const CLAUDE_NAME: &str = "claude";
const MATRIX: &str = include_str!("../themes/matrix.tmTheme");

pub fn load(name: &str) -> Result<Theme> {
    let path = Path::new(name);
    if path.is_file() {
        return load_file(path);
    }
    if let Some(file) = user_themes().into_iter().find(|p| loose_eq(&stem(p), name)) {
        return load_file(&file);
    }
    if loose_eq(CLAUDE_NAME, name) {
        let mut theme = two_face::theme::extra()
            .get(two_face::theme::EmbeddedThemeName::MonokaiExtended)
            .clone();
        theme.name = Some("Claude".into());
        return Ok(theme);
    }
    if loose_eq(MATRIX_NAME, name) {
        return ThemeSet::load_from_reader(&mut Cursor::new(MATRIX))
            .context("reading the built-in matrix theme");
    }
    let set = two_face::theme::extra();
    for &embedded in EmbeddedLazyThemeSet::theme_names() {
        if loose_eq(embedded.as_name(), name) {
            return Ok(set.get(embedded).clone());
        }
    }
    bail!("unknown theme `{name}` (see --list-themes)")
}

/// Every theme name [`load`] accepts, bundled ones first.
pub fn names() -> Vec<String> {
    let bundled = EmbeddedLazyThemeSet::theme_names()
        .iter()
        .map(|t| t.as_name().to_string());
    bundled
        .chain([CLAUDE_NAME, MATRIX_NAME].map(String::from))
        .chain(user_themes().iter().map(|p| stem(p)))
        .collect()
}

/// Whether a theme gets the CRT flicker. Going by the name inside the theme
/// means a user's own file called `matrix` that replaces ours doesn't.
// ponytail: a hard-coded name, since theme files have no field for effects;
// make it a list or a setting if a second theme ever wants it.
pub fn flickers(theme: &Theme) -> bool {
    theme.name.as_deref() == Some("Matrix")
}

fn load_file(path: &Path) -> Result<Theme> {
    let is_yaml = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml" | "yml")
    );
    if is_yaml {
        let text = std::fs::read_to_string(path)?;
        from_base16(&text).with_context(|| format!("reading base16 scheme {}", path.display()))
    } else {
        ThemeSet::get_theme(path).with_context(|| format!("reading theme {}", path.display()))
    }
}

fn theme_dir() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("difftop/themes"))
}

/// All theme files under the user theme folder, sorted.
fn user_themes() -> Vec<PathBuf> {
    let Some(dir) = theme_dir() else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = ignore::WalkBuilder::new(dir)
        .follow_links(true)
        .build()
        .filter_map(Result::ok)
        .map(|e| e.into_path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("tmTheme" | "yaml" | "yml")
            )
        })
        .collect();
    found.sort();
    found
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn loose_eq(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    norm(a) == norm(b)
}

/// Builds a syntect theme from a base16 (or base24) scheme, following the
/// base16 styling guidelines for which color goes where.
///
/// Accepts both the current tinted-theming format (`palette:` block with
/// `"#rrggbb"` values) and the legacy flat format (`base00: "rrggbb"`).
// ponytail: hand-parses `key: value` lines instead of pulling in a YAML crate;
// scheme files are flat, swap in a real parser if that ever changes.
pub fn from_base16(text: &str) -> Result<Theme> {
    let mut base = [None; 16];
    let mut name = None;
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let unquote = |s: &str| s.trim().trim_matches(['"', '\'']).to_string();
        match key.trim() {
            "name" | "scheme" => name = Some(unquote(value)),
            k if k.len() == 6 && k.starts_with("base0") => {
                let slot = usize::from_str_radix(&k[5..], 16)?;
                // First word only, so trailing `# comments` are ignored.
                let hex = unquote(value.split_whitespace().next().unwrap_or(""));
                base[slot] = Some(parse_hex(&hex).with_context(|| format!("bad color for {k}"))?);
            }
            _ => {}
        }
    }
    let mut colors = [SynColor::BLACK; 16];
    for (slot, color) in base.iter().enumerate() {
        colors[slot] = color.with_context(|| format!("missing base0{slot:X}"))?;
    }

    let rules: &[(&str, usize)] = &[
        ("comment, punctuation.definition.comment", 0x3),
        ("string, constant.other.symbol, markup.inserted", 0xB),
        (
            "constant.numeric, constant.language, constant.character, constant.other, entity.other.attribute-name",
            0x9,
        ),
        ("keyword, storage, markup.changed", 0xE),
        ("keyword.operator, punctuation", 0x5),
        (
            "entity.name.function, support.function, variable.function, markup.heading, entity.name.section",
            0xD,
        ),
        (
            "entity.name.type, entity.name.class, support.type, support.class, entity.other.inherited-class",
            0xA,
        ),
        (
            "variable, entity.name.tag, markup.deleted, markup.link",
            0x8,
        ),
        ("support, string.regexp, constant.character.escape", 0xC),
        ("invalid.deprecated, meta.embedded", 0xF),
    ];
    let scopes = rules
        .iter()
        .map(|&(selector, slot)| ThemeItem {
            scope: ScopeSelectors::from_str(selector).expect("static selector"),
            style: StyleModifier {
                foreground: Some(colors[slot]),
                background: None,
                font_style: None,
            },
        })
        .collect();

    Ok(Theme {
        name,
        author: None,
        settings: ThemeSettings {
            foreground: Some(colors[0x5]),
            background: Some(colors[0x0]),
            ..ThemeSettings::default()
        },
        scopes,
    })
}

fn parse_hex(value: &str) -> Result<SynColor> {
    let hex = value.trim_start_matches('#');
    if hex.len() != 6 {
        bail!("expected 6 hex digits, got `{value}`");
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16);
    Ok(SynColor {
        r: byte(0)?,
        g: byte(2)?,
        b: byte(4)?,
        a: 0xFF,
    })
}

/// Whether text in `fg` can be read on `bg`: a contrast ratio of at least 3
/// (the WCAG minimum for large text, generous for a glance at a diff).
/// Terminal colors that aren't RGB are estimated with standard values.
pub fn readable_on(fg: Color, bg: Color) -> bool {
    // A theme's own RGB colors were chosen to go with its diff colors.
    if matches!(fg, Color::Rgb(..)) {
        return true;
    }
    let (Some(fg), Some(bg)) = (rgb(fg), rgb(bg)) else {
        return true; // the terminal's own default colors: trust them
    };
    let (a, b) = (luminance(fg), luminance(bg));
    (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 3.0
}

/// An estimate of what a terminal shows for `color`.
fn rgb(color: Color) -> Option<(u8, u8, u8)> {
    // ponytail: the 16 base colors are Windows Terminal's default scheme
    // (Campbell); every terminal theme differs, so this is only an estimate.
    // Ask the terminal for its palette (OSC 4) if it guesses wrong too often.
    const BASE: [(u8, u8, u8); 16] = [
        (12, 12, 12),
        (197, 15, 31),
        (19, 161, 14),
        (193, 156, 0),
        (0, 55, 218),
        (136, 23, 152),
        (58, 150, 221),
        (204, 204, 204),
        (118, 118, 118),
        (231, 72, 86),
        (22, 198, 12),
        (249, 241, 165),
        (59, 120, 255),
        (180, 0, 158),
        (97, 214, 214),
        (242, 242, 242),
    ];
    let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
    Some(match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(i @ 0..16) => BASE[i as usize],
        Color::Indexed(i @ 16..232) => {
            let i = i - 16;
            (level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        Color::Indexed(i) => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
        Color::Black => BASE[0],
        Color::Red => BASE[1],
        Color::Green => BASE[2],
        Color::Yellow => BASE[3],
        Color::Blue => BASE[4],
        Color::Magenta => BASE[5],
        Color::Cyan => BASE[6],
        Color::Gray => BASE[7],
        Color::DarkGray => BASE[8],
        Color::LightRed => BASE[9],
        Color::LightGreen => BASE[10],
        Color::LightYellow => BASE[11],
        Color::LightBlue => BASE[12],
        Color::LightMagenta => BASE[13],
        Color::LightCyan => BASE[14],
        Color::White => BASE[15],
        Color::Reset => return None,
    })
}

/// Relative luminance, as WCAG defines it.
fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let channel = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// Whether the terminal says it shows 24-bit color, the way Claude Code's
/// color library decides it.
fn truecolor() -> bool {
    std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit")
}

/// `share` of `color` over `background`: a quarter marks a row while the
/// syntax colors on top stay readable.
fn tint(background: Color, color: Color, share: f64, fallback: Color) -> Color {
    let (Color::Rgb(r, g, b), Color::Rgb(cr, cg, cb)) = (
        match background {
            Color::Reset => Color::Rgb(0, 0, 0),
            other => other,
        },
        color,
    ) else {
        // ponytail: palette colors (the default `ansi` theme) can't be mixed,
        // so they get xterm's dark green and red (#005f00, #5f0000), the same
        // as Claude Code. That assumes a dark terminal; ask the terminal for
        // its background (OSC 11) if light ones come up.
        return fallback;
    };
    let mix = |bg: u8, c: u8| (bg as f64 + (c as f64 - bg as f64) * share).round() as u8;
    Color::Rgb(mix(r, cr), mix(g, cg), mix(b, cb))
}

/// Converts a syntect color to a terminal color. bat's `ansi` and `base16`
/// themes smuggle palette indexes through the alpha channel: alpha 0 means
/// "palette color number `r`", alpha 1 means "the terminal's default". That is
/// what makes those themes follow the terminal's colors live.
pub fn to_color(c: SynColor) -> Option<Color> {
    match c.a {
        0 => Some(Color::Indexed(c.r)),
        1 => None,
        _ => Some(Color::Rgb(c.r, c.g, c.b)),
    }
}

/// The handful of non-syntax styles the interface needs, taken from the
/// theme where it defines them sensibly and from the terminal palette where
/// it doesn't.
#[derive(Debug, Clone, Copy)]
pub struct UiColors {
    pub added: Color,
    pub removed: Color,
    /// Bytes that changed in place, in a binary file's map.
    pub changed: Color,
    /// File names in change headers.
    pub accent: Color,
    pub foreground: Color,
    pub background: Color,
    /// Row backgrounds behind added and removed lines.
    pub added_bg: Color,
    pub removed_bg: Color,
    /// Brighter backgrounds behind the words that changed within a line.
    pub added_word_bg: Color,
    pub removed_word_bg: Color,
    /// Line numbers and `+`/`-` marks on changed rows.
    pub added_mark: Color,
    pub removed_mark: Color,
    /// Secondary text: context lines, separators, the status line.
    pub muted: Style,
}

impl UiColors {
    pub fn from_theme(theme: &Theme) -> Self {
        if theme.name.as_deref() == Some("Claude") {
            return Self::claude(theme);
        }
        let highlighter = Highlighter::new(theme);
        let default_fg = theme.settings.foreground;
        // The color a theme gives `scope`, if it gives it a color of its own.
        let scope_color = |scope: &str| {
            let style = highlighter.style_for_stack(&[Scope::new(scope).expect("static scope")]);
            (Some(style.foreground) != default_fg)
                .then(|| to_color(style.foreground))
                .flatten()
        };

        let (mut added, mut removed) = (
            scope_color("markup.inserted"),
            scope_color("markup.deleted"),
        );
        if added == removed {
            // Some themes style both the same, which says nothing.
            (added, removed) = (None, None);
        }
        // Palette themes say magenta here, which reads as just another
        // syntax color; yellow reads as "changed", like the theme's own
        // colors do in true-color themes.
        let changed = scope_color("markup.changed")
            .filter(|c| Some(*c) != added && Some(*c) != removed && matches!(c, Color::Rgb(..)))
            .unwrap_or(Color::Indexed(3));
        let added = added.unwrap_or(Color::Indexed(2));
        let removed = removed.unwrap_or(Color::Indexed(1));
        let accent = ["markup.heading", "entity.name.function", "keyword"]
            .into_iter()
            .filter_map(scope_color)
            .find(|c| *c != added && *c != removed)
            .unwrap_or(Color::Indexed(4));
        let foreground = default_fg.and_then(to_color).unwrap_or(Color::Reset);
        let background = theme
            .settings
            .background
            .and_then(to_color)
            .unwrap_or(Color::Reset);

        Self {
            added,
            removed,
            changed,
            accent,
            foreground,
            background,
            added_bg: tint(background, added, 0.25, Color::Indexed(22)),
            removed_bg: tint(background, removed, 0.25, Color::Indexed(52)),
            added_word_bg: tint(background, added, 0.45, Color::Indexed(28)),
            removed_word_bg: tint(background, removed, 0.45, Color::Indexed(88)),
            added_mark: added,
            removed_mark: removed,
            muted: Style::new().fg(foreground).add_modifier(Modifier::DIM),
        }
    }

    /// Claude Code's dark-theme diff colors. Like Claude Code, row colors
    /// are exact only where the terminal says it shows true color; elsewhere
    /// they are the nearest of the 256 colors, which is what Claude Code
    /// ends up showing there too.
    fn claude(theme: &Theme) -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        let exact = truecolor();
        let pick = |true_color, fallback| if exact { true_color } else { fallback };
        let foreground = theme
            .settings
            .foreground
            .and_then(to_color)
            .unwrap_or(Color::Reset);
        let (added, removed) = (rgb(80, 200, 80), rgb(220, 90, 90));
        Self {
            added,
            removed,
            changed: rgb(255, 193, 7),
            accent: rgb(215, 119, 87),
            foreground,
            // The terminal's own background, as in Claude Code.
            background: Color::Reset,
            added_bg: pick(rgb(2, 40, 0), Color::Indexed(22)),
            removed_bg: pick(rgb(61, 1, 0), Color::Indexed(52)),
            added_word_bg: pick(rgb(4, 71, 0), Color::Indexed(28)),
            removed_word_bg: pick(rgb(92, 2, 0), Color::Indexed(88)),
            added_mark: added,
            removed_mark: removed,
            muted: Style::new().fg(foreground).add_modifier(Modifier::DIM),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_too_close_to_a_tint_are_unreadable() {
        let (green_bg, red_bg) = (Color::Indexed(22), Color::Indexed(52));
        // The palette's own green and blue vanish on the green tint...
        assert!(!readable_on(Color::Indexed(2), green_bg));
        assert!(!readable_on(Color::Indexed(4), green_bg));
        assert!(!readable_on(Color::Indexed(1), red_bg));
        // ...while light text stays, and so does the terminal's default.
        assert!(readable_on(Color::Indexed(15), green_bg));
        assert!(readable_on(Color::Indexed(11), red_bg));
        assert!(readable_on(Color::Reset, green_bg));
        assert!(readable_on(Color::Rgb(255, 255, 255), Color::Rgb(0, 95, 0)));
        // A theme's own RGB colors are trusted, as Claude Code's pink on green.
        assert!(readable_on(Color::Rgb(249, 38, 114), Color::Indexed(22)));
    }

    const TINTED: &str = r##"
system: "base16"
name: "Test Scheme"
author: "someone"
variant: "dark"
palette:
  base00: "#000000"
  base01: "#111111"
  base02: "#222222"
  base03: "#333333" # comments
  base04: "#444444"
  base05: "#555555"
  base06: "#666666"
  base07: "#777777"
  base08: "#880000"
  base09: "#999999"
  base0A: "#aaaaaa"
  base0B: "#00bb00"
  base0C: "#cccccc"
  base0D: "#0000dd"
  base0E: "#eeeeee"
  base0F: "#ffffff"
"##;

    #[test]
    fn parses_tinted_base16() {
        let theme = from_base16(TINTED).unwrap();
        assert_eq!(theme.name.as_deref(), Some("Test Scheme"));
        let ui = UiColors::from_theme(&theme);
        assert_eq!(ui.added, Color::Rgb(0, 0xbb, 0));
        assert_eq!(ui.removed, Color::Rgb(0x88, 0, 0));
        assert_eq!(ui.accent, Color::Rgb(0, 0, 0xdd));
        assert_eq!(ui.background, Color::Rgb(0, 0, 0));
    }

    #[test]
    fn parses_legacy_base16_and_rejects_incomplete() {
        let legacy: String = TINTED
            .lines()
            .filter(|l| l.contains("base"))
            .map(|l| l.replace('#', "") + "\n")
            .collect();
        assert!(from_base16(&legacy).is_ok());
        assert!(from_base16("base00: \"000000\"").is_err());
    }

    #[test]
    fn ansi_colors_follow_the_terminal() {
        let palette = SynColor {
            r: 3,
            g: 0,
            b: 0,
            a: 0,
        };
        let default = SynColor {
            r: 0,
            g: 0,
            b: 0,
            a: 1,
        };
        let rgb = SynColor {
            r: 1,
            g: 2,
            b: 3,
            a: 255,
        };
        assert_eq!(to_color(palette), Some(Color::Indexed(3)));
        assert_eq!(to_color(default), None);
        assert_eq!(to_color(rgb), Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn every_theme_gets_distinct_interface_colors() {
        for name in names() {
            let ui = UiColors::from_theme(&load(&name).unwrap());
            assert_ne!(ui.added, ui.removed, "{name}");
            assert_ne!(ui.accent, ui.added, "{name}");
            assert_ne!(ui.accent, ui.removed, "{name}");
        }
    }

    #[test]
    fn matrix_is_green_all_the_way_through() {
        let theme = load("Matrix").unwrap();
        // Strict enough to reject yellow-greens and near-white mints.
        let green = |c: SynColor| c.g / 2 >= c.r && c.g / 2 >= c.b;
        assert!(green(theme.settings.foreground.unwrap()));
        assert!(green(theme.settings.background.unwrap()));
        for item in &theme.scopes {
            assert!(green(item.style.foreground.unwrap()), "{:?}", item.scope);
        }
        assert!(names().iter().any(|n| n == "matrix"));
    }

    #[test]
    fn only_matrix_flickers() {
        assert!(flickers(&load("matrix").unwrap()));
        assert!(!flickers(&load("ansi").unwrap()));
        assert!(!flickers(&load("Dracula").unwrap()));
    }

    #[test]
    fn every_bundled_theme_loads() {
        for name in names() {
            load(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        assert!(load("solarized-dark").is_ok());
        assert!(load("no-such-theme").is_err());
    }
}
