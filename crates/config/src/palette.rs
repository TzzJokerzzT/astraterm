//! Colour types shared by themes, the renderer and the settings panel.

use serde::{Deserialize, Serialize};

/// 8-bit-per-channel RGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const BLACK: Rgb = Rgb::new(0, 0, 0);
    pub const WHITE: Rgb = Rgb::new(255, 255, 255);

    /// Parses `#RRGGBB` or `#RGB`.
    pub fn parse(value: &str) -> Result<Self, String> {
        let text = value.trim().trim_start_matches('#');
        let expand = |c: char| -> String { format!("{c}{c}") };
        let hex = match text.len() {
            3 => text.chars().map(expand).collect::<String>(),
            6 => text.to_owned(),
            other => {
                return Err(format!(
                    "colour `{value}` must be #RGB or #RRGGBB (found {other} digits)"
                ));
            }
        };
        let parse = |slice: &str| u8::from_str_radix(slice, 16).map_err(|_| value.to_owned());
        let r = parse(&hex[0..2])?;
        let g = parse(&hex[2..4])?;
        let b = parse(&hex[4..6])?;
        Ok(Self { r, g, b })
    }

    /// Returns the colour as a lowercase `#rrggbb` string.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Linear interpolation used by subtle UI highlights.
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: u8, b: u8| {
            (a as f32 + (b as f32 - a as f32) * t)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Rgb {
            r: lerp(self.r, other.r),
            g: lerp(self.g, other.g),
            b: lerp(self.b, other.b),
        }
    }

    /// WCAG relative luminance, used to pick a readable contrast colour.
    pub fn luminance(self) -> f32 {
        let channel = |value: u8| {
            let value = value as f32 / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    /// Chooses black or white text for this background.
    pub fn contrast_text(self) -> Rgb {
        if self.luminance() > 0.45 {
            Rgb::BLACK
        } else {
            Rgb::WHITE
        }
    }

    pub fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl TryFrom<String> for Rgb {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Rgb::parse(&value)
    }
}

impl From<Rgb> for String {
    fn from(value: Rgb) -> Self {
        value.to_hex()
    }
}

/// A semantic colour reference used by terminal cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// Inherit the default foreground/background of the theme.
    Default,
    /// One of the 16 ANSI palette slots.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// The full set of colours a theme provides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub cursor_text: Rgb,
    pub selection_background: Rgb,
    pub selection_foreground: Rgb,
    pub accent: Rgb,
    pub tab_bar_background: Rgb,
    pub tab_active_background: Rgb,
    pub tab_active_foreground: Rgb,
    pub tab_inactive_foreground: Rgb,
    pub pane_border: Rgb,
    pub pane_border_focused: Rgb,
    /// Normal colours 0-7 followed by bright colours 8-15.
    pub ansi: [Rgb; 16],
}

impl Palette {
    /// Resolves an ANSI slot, falling back to a generated 256-colour cube for
    /// indices above 15.
    pub fn ansi_color(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let index = index - 16;
                let steps = [0u8, 95, 135, 175, 215, 255];
                let r = steps[(index / 36) as usize];
                let g = steps[((index % 36) / 6) as usize];
                let b = steps[(index % 6) as usize];
                Rgb::new(r, g, b)
            }
            232..=255 => {
                let level = 8 + (index - 232) * 10;
                Rgb::new(level, level, level)
            }
        }
    }

    /// Resolves a cell colour to RGB.
    pub fn resolve(&self, color: Color, default: Rgb) -> Rgb {
        match color {
            Color::Default => default,
            Color::Indexed(index) => self.ansi_color(index),
            Color::Rgb(r, g, b) => Rgb::new(r, g, b),
        }
    }

    /// Dark charcoal terminal that matches the product's visual direction.
    pub fn midnight() -> Self {
        Self {
            foreground: Rgb::new(0xd4, 0xd8, 0xe0),
            background: Rgb::new(0x1c, 0x1f, 0x26),
            cursor: Rgb::new(0x8b, 0x9c, 0xff),
            cursor_text: Rgb::new(0x16, 0x18, 0x1d),
            selection_background: Rgb::new(0x33, 0x3d, 0x52),
            selection_foreground: Rgb::new(0xf0, 0xf3, 0xf8),
            accent: Rgb::new(0x8b, 0x9c, 0xff),
            tab_bar_background: Rgb::new(0x17, 0x1a, 0x20),
            tab_active_background: Rgb::new(0x25, 0x2a, 0x33),
            tab_active_foreground: Rgb::new(0xe6, 0xea, 0xf2),
            tab_inactive_foreground: Rgb::new(0x8b, 0x92, 0xa2),
            pane_border: Rgb::new(0x2c, 0x32, 0x3c),
            pane_border_focused: Rgb::new(0x8b, 0x9c, 0xff),
            ansi: [
                Rgb::new(0x2a, 0x2f, 0x39),
                Rgb::new(0xe0, 0x6c, 0x75),
                Rgb::new(0x8f, 0xc0, 0x7d),
                Rgb::new(0xe5, 0xc0, 0x7b),
                Rgb::new(0x7d, 0xa6, 0xf0),
                Rgb::new(0xc0, 0x8c, 0xe0),
                Rgb::new(0x74, 0xc0, 0xc4),
                Rgb::new(0xc8, 0xcd, 0xd6),
                Rgb::new(0x4a, 0x51, 0x5f),
                Rgb::new(0xf2, 0x8b, 0x92),
                Rgb::new(0xa8, 0xd8, 0x92),
                Rgb::new(0xf5, 0xd4, 0x92),
                Rgb::new(0x9a, 0xbc, 0xf5),
                Rgb::new(0xd4, 0xa6, 0xf0),
                Rgb::new(0x8f, 0xd6, 0xd9),
                Rgb::new(0xf0, 0xf3, 0xf8),
            ],
        }
    }

    /// Light theme, provided as an option, never as the primary identity.
    pub fn dawn() -> Self {
        Self {
            foreground: Rgb::new(0x2b, 0x30, 0x38),
            background: Rgb::new(0xf7, 0xf8, 0xfa),
            cursor: Rgb::new(0x3f, 0x63, 0xd8),
            cursor_text: Rgb::new(0xff, 0xff, 0xff),
            selection_background: Rgb::new(0xd3, 0xdf, 0xf7),
            selection_foreground: Rgb::new(0x1a, 0x1f, 0x27),
            accent: Rgb::new(0x3f, 0x63, 0xd8),
            tab_bar_background: Rgb::new(0xec, 0xef, 0xf4),
            tab_active_background: Rgb::new(0xff, 0xff, 0xff),
            tab_active_foreground: Rgb::new(0x1a, 0x1f, 0x27),
            tab_inactive_foreground: Rgb::new(0x66, 0x6e, 0x7d),
            pane_border: Rgb::new(0xd5, 0xda, 0xe2),
            pane_border_focused: Rgb::new(0x3f, 0x63, 0xd8),
            ansi: [
                Rgb::new(0x2b, 0x30, 0x38),
                Rgb::new(0xc0, 0x39, 0x4b),
                Rgb::new(0x2f, 0x7d, 0x4f),
                Rgb::new(0x9a, 0x6d, 0x14),
                Rgb::new(0x2f, 0x5f, 0xc0),
                Rgb::new(0x8a, 0x4a, 0xb8),
                Rgb::new(0x1f, 0x7a, 0x83),
                Rgb::new(0x55, 0x5c, 0x68),
                Rgb::new(0x6b, 0x73, 0x80),
                Rgb::new(0xd9, 0x50, 0x5f),
                Rgb::new(0x3f, 0x94, 0x63),
                Rgb::new(0xb4, 0x83, 0x22),
                Rgb::new(0x44, 0x76, 0xd8),
                Rgb::new(0xa0, 0x63, 0xcc),
                Rgb::new(0x2c, 0x93, 0x9d),
                Rgb::new(0xf7, 0xf8, 0xfa),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_and_long_hex() {
        assert_eq!(Rgb::parse("#8B9CFF").unwrap(), Rgb::new(0x8b, 0x9c, 0xff));
        assert_eq!(Rgb::parse("8b9cff").unwrap(), Rgb::new(0x8b, 0x9c, 0xff));
        assert_eq!(Rgb::parse("#fff").unwrap(), Rgb::new(0xff, 0xff, 0xff));
    }

    #[test]
    fn rejects_invalid_hex() {
        assert!(Rgb::parse("#12345").is_err());
        assert!(Rgb::parse("#gggggg").is_err());
        assert!(Rgb::parse("").is_err());
    }

    #[test]
    fn hex_round_trips() {
        let color = Rgb::new(0x12, 0x34, 0x56);
        assert_eq!(Rgb::parse(&color.to_hex()).unwrap(), color);
    }

    #[test]
    fn ansi_cube_and_grayscale_resolution() {
        let palette = Palette::midnight();
        assert_eq!(palette.ansi_color(0), palette.ansi[0]);
        assert_eq!(palette.ansi_color(16), Rgb::new(0, 0, 0));
        assert_eq!(palette.ansi_color(231), Rgb::new(255, 255, 255));
        assert_eq!(palette.ansi_color(232), Rgb::new(8, 8, 8));
        assert_eq!(palette.ansi_color(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn contrast_text_picks_readable_colour() {
        assert_eq!(Rgb::new(0x10, 0x10, 0x10).contrast_text(), Rgb::WHITE);
        assert_eq!(Rgb::new(0xf0, 0xf0, 0xf0).contrast_text(), Rgb::BLACK);
    }
}
