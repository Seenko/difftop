//! The matrix theme's flicker: the screen randomly dips in brightness for a
//! few frames, like an old CRT. It works on the finished frame, so nothing
//! else has to know about it.
//!
//! Kept mild for the sake of anyone sensitive to flashing: under one dip a
//! second on average, never to black, and the background doesn't change at all.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};

/// One frame in this many starts a dip. At 30 frames per second that's about
/// one dip every 1.3 seconds.
const DIP_ODDS: u64 = 40;
const MAX_DIP_FRAMES: u64 = 3;
/// A dip's brightness in percent is this plus up to `BRIGHTNESS_RANGE - 1`.
const MIN_BRIGHTNESS: u8 = 65;
const BRIGHTNESS_RANGE: u64 = 26;

pub struct Flicker {
    rng: u64,
    brightness: u8,
    frames_left: u64,
}

impl Flicker {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos() as u64);
        Self::seeded(nanos)
    }

    fn seeded(seed: u64) -> Self {
        Self {
            // xorshift never leaves zero, so it must not start there.
            rng: seed | 1,
            brightness: 100,
            frames_left: 0,
        }
    }

    /// Xorshift: nothing here needs better randomness than a few lines give.
    fn random(&mut self, below: u64) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng % below
    }

    /// This frame's brightness in percent.
    fn next_brightness(&mut self) -> u8 {
        if self.frames_left == 0 {
            if self.random(DIP_ODDS) != 0 {
                return 100;
            }
            self.frames_left = 1 + self.random(MAX_DIP_FRAMES);
            self.brightness = MIN_BRIGHTNESS + self.random(BRIGHTNESS_RANGE) as u8;
        }
        self.frames_left -= 1;
        self.brightness
    }

    /// Dims a finished frame if this one is part of a dip. Steady frames are
    /// left alone, so an idle screen still sends nothing to the terminal.
    pub fn apply(&mut self, buffer: &mut Buffer) {
        let brightness = self.next_brightness();
        if brightness < 100 {
            for cell in &mut buffer.content {
                dim(cell, brightness);
            }
        }
    }
}

/// RGB colors can be scaled exactly. Palette colors belong to the terminal, so
/// the only way to darken those is its dim attribute.
fn dim(cell: &mut Cell, brightness: u8) {
    let scale = |channel: u8| (channel as u16 * brightness as u16 / 100) as u8;
    match cell.fg {
        Color::Rgb(r, g, b) => cell.fg = Color::Rgb(scale(r), scale(g), scale(b)),
        _ => cell.modifier |= Modifier::DIM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn dips_are_rare_short_and_never_black() {
        let mut flicker = Flicker::seeded(42);
        let (mut dipped, mut run, mut longest) = (0, 0, 0);
        for _ in 0..30_000 {
            let brightness = flicker.next_brightness();
            if brightness == 100 {
                run = 0;
            } else {
                assert!((MIN_BRIGHTNESS..=90).contains(&brightness), "{brightness}");
                dipped += 1;
                run += 1;
                longest = longest.max(run);
            }
        }
        assert!(dipped > 0);
        assert!(dipped < 30_000 / 10, "{dipped}");
        // Two dips can touch, so a run may be a little over one dip's length.
        assert!(longest <= 3 * MAX_DIP_FRAMES, "{longest}");
    }

    #[test]
    fn a_seed_of_zero_still_flickers() {
        let mut flicker = Flicker::seeded(0);
        assert!((0..1000).any(|_| flicker.next_brightness() < 100));
    }

    #[test]
    fn dim_scales_rgb_and_leaves_the_background() {
        let mut cell = Cell::new("x");
        cell.fg = Color::Rgb(0, 200, 100);
        cell.bg = Color::Rgb(0, 10, 2);
        dim(&mut cell, 50);
        assert_eq!(cell.fg, Color::Rgb(0, 100, 50));
        assert_eq!(cell.bg, Color::Rgb(0, 10, 2));
        assert!(!cell.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn dim_falls_back_to_the_attribute_for_palette_colors() {
        let mut cell = Cell::new("x");
        cell.fg = Color::Indexed(2);
        dim(&mut cell, 50);
        assert_eq!(cell.fg, Color::Indexed(2));
        assert!(cell.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn only_dips_change_the_frame() {
        let mut flicker = Flicker::seeded(42);
        let mut probe = Flicker::seeded(42);
        let (mut steady, mut dips) = (0, 0);
        for _ in 0..1000 {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
            buffer.content[0].fg = Color::Rgb(0, 200, 100);
            let before = buffer.clone();
            flicker.apply(&mut buffer);
            if probe.next_brightness() == 100 {
                assert_eq!(buffer, before);
                steady += 1;
            } else {
                assert_ne!(buffer, before);
                dips += 1;
            }
        }
        assert!(steady > 0 && dips > 0);
    }
}
