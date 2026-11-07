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
const MATRIX: &str = include_str!("../themes/matrix.tmTheme");

pub fn load(name: &str) -> Result<Theme> {
    let path = Path::new(name);
    if path.is_file() {
        return load_file(path);
    }
    if let Some(file) = user_themes().into_iter().find(|p| loose_eq(&stem(p), name)) {
        return load_file(&file);
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
        .chain(std::iter::once(MATRIX_NAME.to_string()))
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

/// A quarter of `color` over `background`: enough to mark the row while the
/// syntax colors on top stay readable.
fn tint(background: Color, color: Color, fallback: Color) -> Color {
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
    let mix = |bg: u8, c: u8| ((bg as u16 * 3 + c as u16) / 4) as u8;
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
    /// Secondary text: context lines, separators, the status line.
    pub muted: Style,
}

impl UiColors {
    pub fn from_theme(theme: &Theme) -> Self {
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
            added_bg: tint(background, added, Color::Indexed(22)),
            removed_bg: tint(background, removed, Color::Indexed(52)),
            muted: Style::new().fg(foreground).add_modifier(Modifier::DIM),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
