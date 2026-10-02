use nuntio_term::{Rgb, Snapshot};

/// Everything drawn in one frame, back to front: terminal panes, then UI
/// rectangles, then UI text, then popups (rectangles and text) that cover
/// all of it.
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
    pub popup_rects: &'a [UiRect],
    pub popup_texts: &'a [UiText],
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
    /// Area covered by the pane (including padding), for dimming. Filled with
    /// the pane's own default background if that differs from
    /// `Frame::background`.
    pub area: Rect,
    /// 0 = normal, 1 = fully covered by the background color.
    pub dim: f32,
}

/// An area of the window, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// The rectangle shrunk by `d` on every side; a negative `d` grows it.
    pub fn inset(self, d: f32) -> Rect {
        Rect {
            x: self.x + d,
            y: self.y + d,
            width: self.width - 2.0 * d,
            height: self.height - 2.0 * d,
        }
    }
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

impl UiRect {
    /// A sharp rectangle filled with `color`.
    pub fn fill(rect: Rect, color: Rgb) -> Self {
        Self::rounded(rect, color, 0.0)
    }

    pub fn rounded(rect: Rect, color: Rgb, radius: f32) -> Self {
        let Rect {
            x,
            y,
            width,
            height,
        } = rect;
        Self {
            x,
            y,
            width,
            height,
            color,
            radius,
        }
    }
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

impl UiText {
    /// Text in the terminal font, not bold.
    pub fn new(x: f32, y: f32, text: impl Into<String>, color: Rgb) -> Self {
        Self {
            x,
            y,
            text: text.into(),
            color,
            bold: false,
            small: false,
        }
    }

    /// The same text in the small UI font.
    pub fn small(self) -> Self {
        Self {
            small: true,
            ..self
        }
    }
}
