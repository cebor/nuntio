use nuntio_term::{Rgb, Snapshot};

/// Everything drawn in one frame, back to front: terminal panes, then UI
/// rectangles, then UI text.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Clear color for areas nothing else covers.
    pub background: Rgb,
    /// Opacity of `background`, and so of cells with the default
    /// background, from 0.0 to 1.0. Needs a transparent surface.
    pub background_opacity: f32,
    pub panes: &'a [PaneView<'a>],
    pub rects: &'a [UiRect],
    pub texts: &'a [UiText],
    /// Radius of the window's outer corners in physical pixels, cut out to
    /// transparent; 0 keeps them square. Needs a transparent surface.
    pub corner_radius: f32,
}

/// A terminal snapshot and where its grid starts, in physical pixels.
#[derive(Debug, Clone, Copy)]
pub struct PaneView<'a> {
    pub snapshot: &'a Snapshot,
    pub x: f32,
    pub y: f32,
    /// Area covered by the pane (including padding), for dimming.
    pub area: [f32; 4],
    /// 0 = normal, 1 = fully covered by the background color.
    pub dim: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Rgb,
    /// Corner radius in physical pixels; 0 for a sharp rectangle.
    pub radius: f32,
}

/// A line of UI text in the terminal font. Characters advance by the cell
/// width (two cells for wide characters); `y` is the top of the text row.
#[derive(Debug, Clone, PartialEq)]
pub struct UiText {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub color: Rgb,
    pub bold: bool,
    /// Drawn in the smaller UI font, whose cells are
    /// `Renderer::small_cell_metrics`.
    pub small: bool,
}
