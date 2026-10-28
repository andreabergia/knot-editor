//! Measured terminal cells in logical pixels, with integral physical PTY cells.

use gpui::{Bounds, Pixels, Point, Window, point, px};

use crate::app::{
    terminal::session::TerminalSize,
    typography::{TEXT_SIZE, text_font},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TerminalLayout {
    pub(crate) cell_width: Pixels,
    pub(crate) cell_height: Pixels,
    pub(crate) baseline: Pixels,
    scale_factor: f32,
}

impl TerminalLayout {
    pub(crate) fn measure(window: &Window) -> Self {
        let text = window.text_system();
        let font = text.resolve_font(&text_font(false, false));
        let width = text
            .advance(font, px(TEXT_SIZE), 'M')
            .expect("resolved terminal font must contain M")
            .width;
        Self::from_metrics(
            width,
            text.ascent(font, px(TEXT_SIZE)),
            text.descent(font, px(TEXT_SIZE)),
            window.scale_factor(),
        )
    }

    fn from_metrics(width: Pixels, ascent: Pixels, descent: Pixels, scale_factor: f32) -> Self {
        // Font metrics use a signed descent; shaped lines expose its positive distance.
        let descent = px(f32::from(descent).abs());
        let round =
            |value: Pixels| px((f32::from(value) * scale_factor).ceil().max(1.) / scale_factor);
        let cell_height = round(ascent + descent);
        Self {
            cell_width: round(width),
            cell_height,
            baseline: (cell_height - ascent - descent) / 2. + ascent,
            scale_factor,
        }
    }

    pub(crate) fn grid_size(self, bounds: Bounds<Pixels>) -> TerminalSize {
        TerminalSize {
            columns: (bounds.size.width / self.cell_width).floor().max(0.) as usize,
            lines: (bounds.size.height / self.cell_height).floor().max(0.) as usize,
            cell_width: (f32::from(self.cell_width) * self.scale_factor)
                .round()
                .clamp(1., u16::MAX as f32) as u16,
            cell_height: (f32::from(self.cell_height) * self.scale_factor)
                .round()
                .clamp(1., u16::MAX as f32) as u16,
        }
        .clamped()
    }

    /// Viewport coordinates; selection applies the emulator's display offset separately.
    pub(crate) fn cell_at(
        self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        columns: usize,
        lines: usize,
    ) -> (usize, usize) {
        let column = ((position.x - bounds.origin.x) / self.cell_width)
            .floor()
            .max(0.) as usize;
        let line = ((position.y - bounds.origin.y) / self.cell_height)
            .floor()
            .max(0.) as usize;
        (
            column.min(columns.saturating_sub(1)),
            line.min(lines.saturating_sub(1)),
        )
    }

    pub(crate) fn origin(
        self,
        bounds: Bounds<Pixels>,
        column: usize,
        line: usize,
    ) -> Point<Pixels> {
        bounds.origin + point(self.cell_width * column, self.cell_height * line)
    }

    /// Align fallback glyphs to the primary font baseline without changing cell allocation.
    pub(crate) fn text_origin(
        self,
        origin: Point<Pixels>,
        ascent: Pixels,
        descent: Pixels,
    ) -> Point<Pixels> {
        point(
            origin.x,
            origin.y + self.baseline - (self.cell_height - ascent - descent) / 2. - ascent,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::{MIN_COLUMNS, MIN_SCREEN_LINES};
    use gpui::size;

    #[test]
    fn physical_rounding_fractional_bounds_and_scale_changes() {
        for scale in [1., 1.25, 1.5, 2.] {
            let layout = TerminalLayout::from_metrics(px(8.1), px(11.2), px(3.3), scale);
            let bounds = Bounds::new(
                point(px(10.25), px(21.5)),
                size(
                    layout.cell_width * 80 + px(0.1),
                    layout.cell_height * 24 + px(0.1),
                ),
            );
            let grid = layout.grid_size(bounds);
            assert_eq!((grid.columns, grid.lines), (80, 24));
            assert_eq!(grid.cell_width as f32, (8.1 * scale).ceil());
            assert_eq!(grid.cell_height as f32, (14.5 * scale).ceil());
            let edge = layout.origin(bounds, 3, 2);
            assert_eq!(layout.cell_at(edge, bounds, 80, 24), (3, 2));
            assert_eq!(
                layout.cell_at(edge - point(px(0.01), px(0.01)), bounds, 80, 24),
                (2, 1)
            );
            assert_eq!(
                layout.cell_at(point(px(-100.), px(-100.)), bounds, 80, 24),
                (0, 0)
            );
            assert_eq!(
                layout.cell_at(point(px(10000.), px(10000.)), bounds, 80, 24),
                (79, 23)
            );
        }
    }

    #[test]
    fn minimum_grid_and_fallback_baseline() {
        let layout = TerminalLayout::from_metrics(px(8.1), px(11.2), px(3.3), 2.);
        let grid = layout.grid_size(Bounds::default());
        assert_eq!((grid.columns, grid.lines), (MIN_COLUMNS, MIN_SCREEN_LINES));
        let origin = point(px(23.), px(45.));
        for (ascent, descent) in [(px(11.2), px(3.3)), (px(16.), px(4.)), (px(9.), px(2.))] {
            let painted = layout.text_origin(origin, ascent, descent);
            let baseline = painted.y + (layout.cell_height - ascent - descent) / 2. + ascent;
            assert!((f32::from(baseline - origin.y - layout.baseline)).abs() < 0.001);
            assert_eq!(painted.x, origin.x);
        }
    }

    #[test]
    fn signed_font_descent_preserves_the_full_glyph_height() {
        for scale in [1., 1.25, 1.5, 2.] {
            let layout = TerminalLayout::from_metrics(px(8.1), px(11.2), px(-3.3), scale);
            let shaped_ascent = px(11.2);
            let shaped_descent = px(3.3);
            assert!(layout.cell_height >= shaped_ascent + shaped_descent);
            let origin = point(px(0.), px(0.));
            let painted = layout.text_origin(origin, shaped_ascent, shaped_descent);
            assert!(f32::from(painted.y).abs() < 0.001);
            assert!(layout.baseline >= shaped_ascent);
            assert!(layout.baseline + shaped_descent <= layout.cell_height);
        }
    }
}
