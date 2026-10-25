//! Where things are on the screen: the header, the framed body with the note
//! list and the editor, the footer. `state` asks for the areas of a screen
//! size, `ui` draws into them.

use nuntio_config::schema::NOTES_SIDEBAR_WIDTH;
use ratatui::layout::{Constraint, Layout, Margin, Rect, Size};

/// Width below which only the list or only the editor is shown.
const NARROW: u16 = 60;
/// Columns the editor keeps at least next to the list.
const MIN_EDITOR: u16 = 30;
/// Column of a todo's box in the overview, from the left of the body.
pub const TODO_BOX_X: u16 = 5;

/// The panel that has the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Editor,
}

/// What decides how the body is divided.
#[derive(Debug, Clone, Copy)]
pub struct Panels {
    /// The todo overview takes the whole body.
    pub todos: bool,
    /// The panel shown on a narrow screen.
    pub focus: Focus,
    /// Whether the list is shown next to the editor on a wide screen.
    pub sidebar: bool,
    /// Columns of the list, at most; narrowed to fit.
    pub sidebar_width: u16,
}

/// Where things are on the screen; computed by `App::prepare`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Areas {
    pub header: Rect,
    /// Below the header, frame included.
    pub body: Rect,
    /// The body without its frame.
    pub inner: Rect,
    pub footer: Rect,
    /// The note list, without its border.
    pub list: Option<Rect>,
    pub editor: Option<Rect>,
    /// The note's text in the editor.
    pub text: Option<Rect>,
    pub too_small: bool,
}

/// The widest the list can be in a body `body_width` columns wide,
/// frame included.
pub fn max_list_width(body_width: u16) -> u16 {
    NOTES_SIDEBAR_WIDTH
        .1
        .min(body_width.saturating_sub(2 + 1 + MIN_EDITOR))
}

/// Header, body and footer of a screen of `size`; `None` if it is too small.
pub fn split(size: Size) -> Option<[Rect; 3]> {
    if size.width < 40 || size.height < 8 {
        return None;
    }
    Some(
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(Rect::new(0, 0, size.width, size.height)),
    )
}

pub fn areas(size: Size, panels: Panels) -> Areas {
    let Some([header, body, footer]) = split(size) else {
        return Areas {
            too_small: true,
            ..Areas::default()
        };
    };
    // Inside the frame; the body is at least 40×6.
    let inner = body.inner(Margin::new(1, 1));
    let mut areas = Areas {
        header,
        body,
        inner,
        footer,
        ..Areas::default()
    };
    if panels.todos {
        return areas;
    }
    if size.width < NARROW {
        match panels.focus {
            Focus::List => areas.list = Some(inner),
            Focus::Editor => areas.editor = Some(inner),
        }
    } else if panels.sidebar {
        let width = panels.sidebar_width.min(max_list_width(body.width));
        areas.list = Some(Rect { width, ..inner });
        // The column after the list is the divider.
        areas.editor = Some(Rect {
            x: inner.x + width + 1,
            width: inner.width - width - 1,
            ..inner
        });
    } else {
        areas.editor = Some(inner);
    }
    areas.text = areas.editor.map(|e| Rect {
        x: e.x + 1,
        y: e.y,
        width: e.width.saturating_sub(2),
        height: e.height,
    });
    areas
}
