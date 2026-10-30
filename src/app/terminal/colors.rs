use alacritty_terminal::{
    term::color::Colors,
    vte::ansi::{Color, NamedColor, Rgb},
};
use gpui::{Hsla, Rgba};

use crate::app::theme::{Theme, color};

/// The theme's terminal colors as packed `0xrrggbb` values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TerminalPalette {
    ansi: [u32; 16],
    pub(crate) foreground: u32,
    pub(crate) background: u32,
}

impl TerminalPalette {
    pub(crate) fn new(theme: &Theme) -> Self {
        let packed = |key| pack(theme.color(key));
        Self {
            ansi: [
                color::TERMINAL_ANSI_BLACK,
                color::TERMINAL_ANSI_RED,
                color::TERMINAL_ANSI_GREEN,
                color::TERMINAL_ANSI_YELLOW,
                color::TERMINAL_ANSI_BLUE,
                color::TERMINAL_ANSI_MAGENTA,
                color::TERMINAL_ANSI_CYAN,
                color::TERMINAL_ANSI_WHITE,
                color::TERMINAL_ANSI_BRIGHT_BLACK,
                color::TERMINAL_ANSI_BRIGHT_RED,
                color::TERMINAL_ANSI_BRIGHT_GREEN,
                color::TERMINAL_ANSI_BRIGHT_YELLOW,
                color::TERMINAL_ANSI_BRIGHT_BLUE,
                color::TERMINAL_ANSI_BRIGHT_MAGENTA,
                color::TERMINAL_ANSI_BRIGHT_CYAN,
                color::TERMINAL_ANSI_BRIGHT_WHITE,
            ]
            .map(packed),
            foreground: packed(color::TERMINAL_FOREGROUND),
            background: packed(color::TERMINAL_BACKGROUND),
        }
    }
}

/// `color` as `0xrrggbb`, dropping alpha.
fn pack(color: Hsla) -> u32 {
    let rgba = Rgba::from(color);
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    (channel(rgba.r) << 16) | (channel(rgba.g) << 8) | channel(rgba.b)
}

pub(crate) fn resolve_color(
    color: Color,
    overrides: &Colors,
    default: u32,
    palette: &TerminalPalette,
) -> u32 {
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Named(named) => {
            overrides[named].unwrap_or_else(|| named_color(named, default, palette))
        }
        Color::Indexed(index) => {
            overrides[index as usize].unwrap_or_else(|| indexed_color(index, palette))
        }
    };
    (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)
}

fn named_color(color: NamedColor, default: u32, palette: &TerminalPalette) -> Rgb {
    if color == NamedColor::Foreground || color == NamedColor::BrightForeground {
        return hex_rgb(default);
    }
    if color == NamedColor::Background {
        return hex_rgb(palette.background);
    }
    let index = color as usize;
    if index < 16 {
        indexed_color(index as u8, palette)
    } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&index) {
        indexed_color((index - NamedColor::DimBlack as usize) as u8, palette)
    } else {
        hex_rgb(default)
    }
}

fn indexed_color(index: u8, palette: &TerminalPalette) -> Rgb {
    match index {
        0..=15 => hex_rgb(palette.ansi[index as usize]),
        16..=231 => {
            let value = index - 16;
            let component = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
            Rgb {
                r: component(value / 36),
                g: component((value % 36) / 6),
                b: component(value % 6),
            }
        }
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            Rgb {
                r: level,
                g: level,
                b: level,
            }
        }
    }
}

fn hex_rgb(color: u32) -> Rgb {
    Rgb {
        r: (color >> 16) as u8,
        g: (color >> 8) as u8,
        b: color as u8,
    }
}

pub(crate) fn dim(color: u32) -> u32 {
    let rgb = hex_rgb(color);
    ((u32::from(rgb.r) * 2 / 3) << 16)
        | ((u32::from(rgb.g) * 2 / 3) << 8)
        | (u32::from(rgb.b) * 2 / 3)
}

pub(crate) fn query_color(index: usize, colors: &Colors, palette: &TerminalPalette) -> Rgb {
    colors[index].unwrap_or_else(|| {
        if index < 256 {
            indexed_color(index as u8, palette)
        } else if index == NamedColor::Background as usize {
            hex_rgb(palette.background)
        } else {
            hex_rgb(palette.foreground)
        }
    })
}
