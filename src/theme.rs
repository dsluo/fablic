//! Colours. Catppuccin Mocha on dark terminals, Latte on light ones.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub text: Color,
    pub dim: Color,
    pub faint: Color,
    pub accent: Color,
    pub title: Color,
    pub key: Color,
    pub select_bg: Color,
    pub ok: Color,
    pub pending: Color,
    pub warn: Color,
    pub bad: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Theme {
    pub const MOCHA: Theme = Theme {
        text: rgb(0xcdd6f4),
        dim: rgb(0xa6adc8),
        faint: rgb(0x6c7086),
        accent: rgb(0x89b4fa),
        title: rgb(0xcba6f7),
        key: rgb(0x94e2d5),
        select_bg: rgb(0x45475a),
        ok: rgb(0xa6e3a1),
        pending: rgb(0x89dceb),
        warn: rgb(0xf9e2af),
        bad: rgb(0xf38ba8),
    };

    pub const LATTE: Theme = Theme {
        text: rgb(0x4c4f69),
        dim: rgb(0x6c6f85),
        faint: rgb(0x9ca0b0),
        accent: rgb(0x1e66f5),
        title: rgb(0x8839ef),
        key: rgb(0x179299),
        select_bg: rgb(0xbcc0cc),
        ok: rgb(0x40a02b),
        pending: rgb(0x04a5e5),
        warn: rgb(0xdf8e1d),
        bad: rgb(0xd20f39),
    };

    /// Ask the terminal for its background. Must run before raw mode / the
    /// alternate screen, and falls back to dark when the terminal won't say.
    pub fn detect() -> Theme {
        use terminal_colorsaurus::{QueryOptions, ThemeMode, theme_mode};
        match theme_mode(QueryOptions::default()) {
            Ok(ThemeMode::Light) => Theme::LATTE,
            _ => Theme::MOCHA,
        }
    }
}
