//! Application text defaults shared by native editor and terminal surfaces.

use gpui::{Font, FontFallbacks};

pub(crate) const TEXT_SIZE: f32 = 14.;

pub(crate) fn text_font(bold: bool, italic: bool) -> Font {
    let mut font = gpui::font("Menlo");
    if bold {
        font = font.bold();
    }
    if italic {
        font = font.italic();
    }
    // The platform resolves missing families and glyphs through its font stack.
    font.fallbacks = Some(FontFallbacks::from_fonts(vec![
        "Geeza Pro".into(),
        "PingFang SC".into(),
        "Apple Color Emoji".into(),
    ]));
    font
}
