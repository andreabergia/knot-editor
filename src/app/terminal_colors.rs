use alacritty_terminal::{
    term::color::Colors,
    vte::ansi::{Color, NamedColor, Rgb},
};

pub(crate) const BACKGROUND: u32 = 0x181818;
pub(crate) const FOREGROUND: u32 = 0xd4d4d4;

pub(crate) fn resolve_color(color: Color, overrides: &Colors, default: u32) -> u32 {
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Named(named) => overrides[named].unwrap_or_else(|| named_color(named, default)),
        Color::Indexed(index) => overrides[index as usize].unwrap_or_else(|| indexed_color(index)),
    };
    (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)
}

fn named_color(color: NamedColor, default: u32) -> Rgb {
    if color == NamedColor::Foreground || color == NamedColor::BrightForeground {
        return hex_rgb(default);
    }
    if color == NamedColor::Background {
        return hex_rgb(BACKGROUND);
    }
    let index = color as usize;
    if index < 16 {
        indexed_color(index as u8)
    } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&index) {
        indexed_color((index - NamedColor::DimBlack as usize) as u8)
    } else {
        hex_rgb(default)
    }
}

fn indexed_color(index: u8) -> Rgb {
    const ANSI: [u32; 16] = [
        0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
        0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ];
    match index {
        0..=15 => hex_rgb(ANSI[index as usize]),
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

pub(crate) fn query_color(index: usize, colors: &Colors) -> Rgb {
    colors[index].unwrap_or_else(|| {
        if index < 256 {
            indexed_color(index as u8)
        } else if index == NamedColor::Background as usize {
            hex_rgb(BACKGROUND)
        } else {
            hex_rgb(FOREGROUND)
        }
    })
}
