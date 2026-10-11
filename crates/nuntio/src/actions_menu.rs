//! The menus that open from the status bar: the menu of all actions that
//! the `actions` item opens, a list with a heading per category, each entry
//! with its shortcut, and the menu of shells for a new tab. Also the list of
//! all tabs that the tab bar's ⌄ button opens. They are laid out in the
//! cells of the small UI font (`UiMetrics::small`).

use nuntio_config::StatusItem;
use nuntio_render::{Rect, UiRect, UiText};
use nuntio_term::Rgb;
use unicode_width::UnicodeWidthStr;

use crate::actions::{Action, Bindings};
use crate::pane_tree::Direction::{Down, Left, Right, Up};
use crate::style::{UiMetrics, framed_box, hairline, mix, truncate};

/// Space above the first and below the last row, in logical pixels.
const PADDING: f64 = 4.0;
/// Space above and below the text of a row, in logical pixels.
const ROW_PADDING: f64 = 2.0;
/// Space to the window's left and right edges, in logical pixels.
const MARGIN: f64 = 2.0;
/// Space between label and shortcut, in cells.
const GAP_CELLS: usize = 3;
/// Longest tab title the tab list shows, in cells.
const TAB_MENU_CELLS: usize = 48;

/// Every action the actions menu offers, by category. `select_tab_1` …
/// `select_tab_9` are left out (the tab bar has the tabs) and so is the
/// menu's own action; "New Tab With…" opens the shell menu.
const MENU: &[(&str, &[(&str, Action)])] = &[
    (
        "Tabs",
        &[
            ("New Tab", Action::NewTab),
            ("New Tab With…", Action::OpenShellMenu),
            ("Close Tab", Action::CloseTab),
            ("Next Tab", Action::NextTab),
            ("Previous Tab", Action::PreviousTab),
        ],
    ),
    (
        "Panes",
        &[
            ("Split Side by Side", Action::SplitVertical),
            ("Split Top and Bottom", Action::SplitHorizontal),
            ("Close Pane", Action::ClosePane),
            ("Zoom Pane", Action::ZoomPane),
            ("Focus Pane Left", Action::FocusPane(Left)),
            ("Focus Pane Right", Action::FocusPane(Right)),
            ("Focus Pane Up", Action::FocusPane(Up)),
            ("Focus Pane Down", Action::FocusPane(Down)),
            ("Resize Pane Left", Action::ResizePane(Left)),
            ("Resize Pane Right", Action::ResizePane(Right)),
            ("Resize Pane Up", Action::ResizePane(Up)),
            ("Resize Pane Down", Action::ResizePane(Down)),
        ],
    ),
    (
        "Edit",
        &[
            ("Copy", Action::Copy),
            ("Paste", Action::Paste),
            ("Find…", Action::Search),
            ("Clear Scrollback", Action::ClearScrollback),
        ],
    ),
    (
        "View",
        &[
            ("Bigger", Action::FontIncrease),
            ("Smaller", Action::FontDecrease),
            ("Actual Size", Action::FontReset),
            ("Toggle Full Screen", Action::ToggleFullscreen),
            ("Scroll Page Up", Action::ScrollPageUp),
            ("Scroll Page Down", Action::ScrollPageDown),
            ("Scroll Line Up", Action::ScrollLineUp),
            ("Scroll Line Down", Action::ScrollLineDown),
        ],
    ),
    (
        "Notes",
        &[
            ("Open Notes", Action::OpenNotes),
            ("Open Notes in Split", Action::OpenNotesSplit),
        ],
    ),
    (
        "nuntio",
        &[
            ("Settings…", Action::OpenSettings),
            ("Reload Config", Action::ReloadConfig),
            ("Check for Updates…", Action::CheckForUpdates),
        ],
    ),
];

/// What a menu lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Actions,
    Shells,
    Tabs,
}

impl MenuKind {
    /// The action that opens (and, pressed again, closes) the menu. The tab
    /// list has none: only the tab bar's ⌄ button opens it.
    pub fn opener(self) -> Option<Action> {
        match self {
            Self::Actions => Some(Action::OpenActionsMenu),
            Self::Shells => Some(Action::OpenShellMenu),
            Self::Tabs => None,
        }
    }

    /// The status bar item shown pressed while it is open.
    pub fn status_item(self) -> Option<StatusItem> {
        match self {
            Self::Actions => Some(StatusItem::Actions),
            Self::Shells => Some(StatusItem::Shell),
            Self::Tabs => None,
        }
    }
}

/// Where the menu grows from: the left edge of the status bar item and the
/// edge of the bar it touches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub x: f32,
    pub y: f32,
    /// The menu sits above `y` instead of below it.
    pub opens_up: bool,
    /// Ignore `x` and center the menu in the window, for when there is no
    /// item to grow from.
    pub centered: bool,
}

/// The window the menu is laid out in.
#[derive(Debug, Clone, Copy)]
pub struct View {
    /// Width and height in pixels.
    pub window: (f32, f32),
    pub metrics: UiMetrics,
}

#[derive(Debug, Clone, PartialEq)]
enum Row {
    Heading(&'static str),
    Entry {
        label: String,
        action: Action,
        shortcut: Option<String>,
        /// The active tab, marked in the tab list.
        current: bool,
    },
}

impl Row {
    fn is_entry(&self) -> bool {
        matches!(self, Row::Entry { .. })
    }
}

/// Which entry to select next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Next,
    Previous,
    First,
    Last,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionsMenu {
    kind: MenuKind,
    anchor: Anchor,
    rows: Vec<Row>,
    /// Index into `rows`, always an entry.
    selected: Option<usize>,
    /// The first row shown when the menu doesn't fit.
    scroll: usize,
}

/// Where the rows of a menu are, in pixels.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    bounds: Rect,
    row_height: f32,
    padding: f32,
    /// Width of the frame.
    border: f32,
    /// One cell: the space left and right of the text, and the indent of
    /// the entries.
    cell_width: f32,
    /// The rows shown: `first..first + visible`.
    first: usize,
    visible: usize,
}

impl Geometry {
    /// The box of the row `n` places below the first one shown.
    fn row(&self, n: usize) -> Rect {
        let inner = self.bounds.inset(self.border);
        Rect::new(
            inner.x,
            self.bounds.y + self.padding + n as f32 * self.row_height,
            inner.width,
            self.row_height,
        )
    }

    /// The row at `y`, if it is inside the rows.
    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        let top = self.bounds.y + self.padding;
        let inside = x >= self.bounds.x
            && x < self.bounds.x + self.bounds.width
            && y >= top
            && y < top + self.visible as f32 * self.row_height;
        inside.then(|| self.first + ((y - top) / self.row_height) as usize)
    }
}

impl ActionsMenu {
    /// The actions menu, whose shortcuts are the ones `bindings` has now.
    pub fn new(anchor: Anchor, bindings: &Bindings) -> Self {
        let mut rows = Vec::new();
        for &(title, entries) in MENU {
            rows.push(Row::Heading(title));
            for &(label, action) in entries {
                rows.push(Row::Entry {
                    label: label.to_owned(),
                    action,
                    shortcut: bindings.shortcut(action),
                    current: false,
                });
            }
        }
        Self::with_rows(MenuKind::Actions, anchor, rows)
    }

    /// The menu of shells for a new tab: entry `i` runs
    /// `Action::NewTabWithShell(i)`; the first, the default shell, shows the
    /// `new_tab` shortcut.
    pub fn shells(anchor: Anchor, names: &[String], bindings: &Bindings) -> Self {
        let mut rows = vec![Row::Heading("New Tab With")];
        rows.extend(names.iter().enumerate().map(|(i, name)| Row::Entry {
            label: name.clone(),
            action: Action::NewTabWithShell(i),
            shortcut: if i == 0 {
                bindings.shortcut(Action::NewTab)
            } else {
                None
            },
            current: false,
        }));
        Self::with_rows(MenuKind::Shells, anchor, rows)
    }

    /// The list of all tabs: entry `i` runs `Action::SelectTab(i)`. The
    /// active tab is marked, selected and scrolled into view.
    pub fn tabs(anchor: Anchor, titles: &[String], active: usize, view: View) -> Self {
        let mut rows = vec![Row::Heading("Tabs")];
        rows.extend(titles.iter().enumerate().map(|(i, title)| Row::Entry {
            label: truncate(title, TAB_MENU_CELLS),
            action: Action::SelectTab(i),
            shortcut: None,
            current: i == active,
        }));
        let mut menu = Self::with_rows(MenuKind::Tabs, anchor, rows);
        menu.selected = Some(active + 1);
        menu.reveal(view);
        menu
    }

    fn with_rows(kind: MenuKind, anchor: Anchor, rows: Vec<Row>) -> Self {
        Self {
            kind,
            anchor,
            rows,
            selected: None,
            scroll: 0,
        }
    }

    /// What the menu lists.
    pub fn kind(&self) -> MenuKind {
        self.kind
    }

    fn geometry(&self, view: View) -> Geometry {
        let View { window, metrics } = view;
        let cell = metrics.small;
        let cw = cell.width as f32;
        let padding = metrics.logical(PADDING);
        let row_height = cell.height as f32 + 2.0 * metrics.logical(ROW_PADDING);
        let margin = metrics.logical(MARGIN);

        // Entries are indented by a cell, below their heading.
        let labels = self.rows.iter().map(|row| match row {
            Row::Heading(title) => title.width(),
            Row::Entry { label, .. } => 1 + label.width(),
        });
        let shortcuts = self.rows.iter().filter_map(|row| match row {
            Row::Entry { shortcut, .. } => shortcut.as_deref().map(str::width),
            Row::Heading(_) => None,
        });
        let (labels, shortcuts) = (labels.max().unwrap_or(0), shortcuts.max().unwrap_or(0));
        let text_cells = labels
            + if shortcuts > 0 {
                GAP_CELLS + shortcuts
            } else {
                0
            };
        // A cell to the left, two to the right: the scroll arrows go there.
        let width = ((text_cells + 3) as f32 * cw)
            .min(window.0 - 2.0 * margin)
            .max(0.0);

        let room = if self.anchor.opens_up {
            self.anchor.y
        } else {
            window.1 - self.anchor.y
        };
        let fit = ((room - 2.0 * padding) / row_height).floor().max(1.0) as usize;
        let visible = fit.min(self.rows.len());
        let first = self.scroll.min(self.rows.len() - visible);
        let height = visible as f32 * row_height + 2.0 * padding;
        let x = if self.anchor.centered {
            (window.0 - width) / 2.0
        } else {
            self.anchor.x
        };
        let x = x.min(window.0 - margin - width).max(margin);
        let y = if self.anchor.opens_up {
            self.anchor.y - height
        } else {
            self.anchor.y
        };
        Geometry {
            bounds: Rect::new(x.round(), y.round(), width, height),
            row_height,
            padding,
            border: hairline(metrics.scale),
            cell_width: cw,
            first,
            visible,
        }
    }

    /// The pointer is over the menu.
    pub fn contains(&self, view: View, x: f32, y: f32) -> bool {
        self.geometry(view).bounds.contains(x, y)
    }

    /// The pointer moved to `(x, y)`: select the entry under it. Other
    /// places keep the selection, so the keys and the pointer can share it.
    /// Returns whether the selection changed.
    pub fn hover(&mut self, view: View, x: f32, y: f32) -> bool {
        match self.entry_at(view, x, y) {
            Some(row) if self.selected != Some(row) => {
                self.selected = Some(row);
                true
            }
            _ => false,
        }
    }

    /// The action of the entry at `(x, y)`.
    pub fn click(&self, view: View, x: f32, y: f32) -> Option<Action> {
        self.entry_at(view, x, y).and_then(|row| self.action(row))
    }

    /// The action of the selected entry.
    pub fn selected_action(&self) -> Option<Action> {
        self.selected.and_then(|row| self.action(row))
    }

    /// The label of the selected entry, for the debug server.
    #[cfg(feature = "debug-server")]
    pub fn selected_label(&self) -> Option<&str> {
        match self.rows.get(self.selected?) {
            Some(Row::Entry { label, .. }) => Some(label),
            _ => None,
        }
    }

    /// The labels of all entries in order, for the debug server.
    #[cfg(feature = "debug-server")]
    pub fn labels(&self) -> Vec<&str> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Entry { label, .. } => Some(label.as_str()),
                Row::Heading(_) => None,
            })
            .collect()
    }

    fn action(&self, row: usize) -> Option<Action> {
        match self.rows.get(row) {
            Some(Row::Entry { action, .. }) => Some(*action),
            _ => None,
        }
    }

    fn entry_at(&self, view: View, x: f32, y: f32) -> Option<usize> {
        let row = self.geometry(view).row_at(x, y)?;
        self.rows.get(row)?.is_entry().then_some(row)
    }

    /// Select another entry, wrapping around, and scroll it into view.
    pub fn select(&mut self, step: Step, view: View) {
        let entries: Vec<usize> = (0..self.rows.len())
            .filter(|&i| self.rows[i].is_entry())
            .collect();
        let position = self
            .selected
            .and_then(|row| entries.iter().position(|&i| i == row));
        let n = entries.len();
        let next = match (step, position) {
            (Step::First, _) | (Step::Next, None) => 0,
            (Step::Last, _) | (Step::Previous, None) => n - 1,
            (Step::Next, Some(i)) => (i + 1) % n,
            (Step::Previous, Some(i)) => (i + n - 1) % n,
        };
        self.selected = Some(entries[next]);
        self.reveal(view);
    }

    /// Scroll so that the selected row (with its heading, if it is the
    /// first of its category) is shown.
    fn reveal(&mut self, view: View) {
        let Some(selected) = self.selected else {
            return;
        };
        let visible = self.geometry(view).visible;
        let top = match selected.checked_sub(1) {
            Some(above) if !self.rows[above].is_entry() => above,
            _ => selected,
        };
        if top < self.scroll {
            self.scroll = top;
        } else if selected >= self.scroll + visible {
            self.scroll = selected + 1 - visible;
        }
        self.scroll = self.geometry(view).first;
    }

    /// Scroll by `rows` (positive: down). Returns whether anything moved.
    pub fn scroll_by(&mut self, rows: isize, view: View) -> bool {
        let geometry = self.geometry(view);
        let last = self.rows.len() - geometry.visible;
        let scroll = geometry.first.saturating_add_signed(rows).min(last);
        let moved = scroll != geometry.first;
        self.scroll = scroll;
        moved
    }

    /// Draw the menu in the colors of the terminal.
    pub fn draw(&self, view: View, background: Rgb, foreground: Rgb) -> (Vec<UiRect>, Vec<UiText>) {
        let geometry = self.geometry(view);
        let cell = view.metrics.small;
        let mut rects = framed_box(geometry.bounds, geometry.border, background, foreground);
        let mut texts = Vec::new();

        let text_color = mix(foreground, background, 0.1);
        let muted = mix(foreground, background, 0.5);
        let highlight = mix(background, foreground, 0.28);
        let text_y = (geometry.row_height - cell.height as f32) / 2.0;
        let x = geometry.bounds.x + geometry.cell_width;
        let right = geometry.bounds.x + geometry.bounds.width - 2.0 * geometry.cell_width;

        let shown = &self.rows[geometry.first..geometry.first + geometry.visible];
        for (n, row) in shown.iter().enumerate() {
            let row_rect = geometry.row(n);
            let y = (row_rect.y + text_y).floor();
            match row {
                Row::Heading(title) => {
                    texts.push(UiText::new(x, y, title.to_uppercase(), muted).small());
                }
                Row::Entry {
                    label,
                    shortcut,
                    current,
                    ..
                } => {
                    let selected = self.selected == Some(geometry.first + n);
                    if selected {
                        rects.push(UiRect::fill(row_rect, highlight));
                    }
                    if *current {
                        texts.push(UiText::new(x, y, "•", text_color).small());
                    }
                    let label_x = x + geometry.cell_width;
                    texts.push(UiText::new(label_x, y, label.clone(), text_color).small());
                    if let Some(shortcut) = shortcut {
                        let width = shortcut.width() as f32 * geometry.cell_width;
                        texts.push(
                            UiText::new((right - width).floor(), y, shortcut.clone(), muted)
                                .small(),
                        );
                    }
                }
            }
        }
        // Arrows in the right margin: more rows above or below.
        let arrow_x = (right + geometry.cell_width / 2.0).floor();
        if geometry.first > 0 {
            let y = (geometry.row(0).y + text_y).floor();
            texts.push(UiText::new(arrow_x, y, "▲", muted).small());
        }
        if geometry.first + geometry.visible < self.rows.len() {
            let y = (geometry.row(geometry.visible - 1).y + text_y).floor();
            texts.push(UiText::new(arrow_x, y, "▼", muted).small());
        }
        (rects, texts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::NAMES;
    use crate::style::test_metrics::*;

    const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };
    const WHITE: Rgb = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };

    fn view(width: f32, height: f32) -> View {
        View {
            window: (width, height),
            metrics: METRICS,
        }
    }

    fn menu(opens_up: bool, y: f32) -> ActionsMenu {
        let anchor = Anchor {
            x: 10.0,
            y,
            opens_up,
            centered: false,
        };
        ActionsMenu::new(anchor, &Bindings::platform_defaults())
    }

    fn entries(menu: &ActionsMenu) -> Vec<Action> {
        menu.rows
            .iter()
            .filter_map(|row| match row {
                Row::Entry { action, .. } => Some(*action),
                Row::Heading(_) => None,
            })
            .collect()
    }

    #[test]
    fn every_action_is_in_the_menu_once() {
        let listed = entries(&menu(true, 500.0));
        for &(name, action) in NAMES {
            let count = listed.iter().filter(|&&a| a == action).count();
            let left_out = matches!(action, Action::SelectTab(_) | Action::OpenActionsMenu);
            assert_eq!(count, usize::from(!left_out), "{name}");
        }
    }

    #[test]
    fn opens_upwards_from_a_bottom_bar_and_downwards_from_a_top_bar() {
        let view = view(1000.0, 1200.0);
        let up = menu(true, 1172.0).geometry(view);
        assert_eq!(up.bounds.y + up.bounds.height, 1172.0);
        assert_eq!(up.bounds.x, 10.0);
        assert_eq!(up.visible, menu(true, 1172.0).rows.len());

        let down = menu(false, 28.0).geometry(view);
        assert_eq!(down.bounds.y, 28.0);
        assert!(down.bounds.y + down.bounds.height <= 1200.0);
    }

    #[test]
    fn stays_inside_a_narrow_window() {
        let mut menu = menu(true, 500.0);
        menu.anchor.x = 380.0;
        let bounds = menu.geometry(view(400.0, 600.0)).bounds;
        assert!(
            bounds.x >= 0.0 && bounds.x + bounds.width <= 400.0,
            "{bounds:?}"
        );
    }

    #[test]
    fn a_short_window_shows_some_rows_and_scrolls() {
        let view = view(1000.0, 200.0);
        let mut menu = menu(true, 200.0);
        let total = menu.rows.len();
        let geometry = menu.geometry(view);
        assert!(geometry.visible < total);
        assert!(geometry.bounds.y >= 0.0);

        assert!(menu.scroll_by(3, view));
        assert_eq!(menu.geometry(view).first, 3);
        assert!(menu.scroll_by(-3, view));
        assert_eq!(menu.geometry(view).first, 0);
        assert!(!menu.scroll_by(-1, view));
        menu.scroll_by(1000, view);
        let geometry = menu.geometry(view);
        assert_eq!(geometry.first + geometry.visible, total);

        // Selecting the last entry keeps it in view; the first shows its heading.
        menu.scroll = 0;
        menu.select(Step::Last, view);
        let geometry = menu.geometry(view);
        assert!(menu.selected.unwrap() < geometry.first + geometry.visible);
        menu.select(Step::First, view);
        assert_eq!(menu.geometry(view).first, 0);
    }

    #[test]
    fn selection_skips_headings_and_wraps() {
        let view = view(1000.0, 1200.0);
        let mut menu = menu(true, 1172.0);
        assert_eq!(menu.selected_action(), None);
        menu.select(Step::Next, view);
        assert_eq!(menu.selected_action(), Some(Action::NewTab));
        menu.select(Step::Previous, view);
        assert_eq!(menu.selected_action(), Some(Action::CheckForUpdates));
        menu.select(Step::Next, view);
        assert_eq!(menu.selected_action(), Some(Action::NewTab));
        for _ in 0..5 {
            menu.select(Step::Next, view);
        }
        // Past the last tab entry, over the "Panes" heading.
        assert_eq!(menu.selected_action(), Some(Action::SplitVertical));
        menu.select(Step::Last, view);
        assert_eq!(menu.selected_action(), Some(Action::CheckForUpdates));
        menu.select(Step::First, view);
        assert_eq!(menu.selected_action(), Some(Action::NewTab));
    }

    #[test]
    fn the_pointer_selects_and_clicks_entries() {
        let view = view(1000.0, 800.0);
        let mut menu = menu(false, 28.0);
        let geometry = menu.geometry(view);
        let x = geometry.bounds.x + 30.0;
        let row_y = |n: usize| geometry.row(n).y + 5.0;

        // Row 0 is the "Tabs" heading, row 1 "New Tab".
        assert!(!menu.hover(view, x, row_y(0)));
        assert_eq!(menu.click(view, x, row_y(0)), None);
        assert!(menu.hover(view, x, row_y(1)));
        assert!(!menu.hover(view, x, row_y(1)));
        assert_eq!(menu.selected_action(), Some(Action::NewTab));
        assert_eq!(menu.click(view, x, row_y(2)), Some(Action::OpenShellMenu));
        // Elsewhere keeps the selection.
        assert!(!menu.hover(view, 900.0, 700.0));
        assert_eq!(menu.selected_action(), Some(Action::NewTab));
        assert!(menu.contains(view, x, row_y(0)));
        assert!(!menu.contains(view, 900.0, 700.0));
    }

    #[test]
    fn scroll_arrows_stay_clear_of_the_text_and_the_frame() {
        let view = view(1000.0, 200.0);
        let mut menu = menu(true, 200.0);
        menu.scroll_by(3, view);
        let bounds = menu.geometry(view).bounds;
        let (_, texts) = menu.draw(view, BLACK, WHITE);
        let arrows: Vec<_> = texts
            .iter()
            .filter(|t| t.text == "▲" || t.text == "▼")
            .collect();
        assert_eq!(arrows.len(), 2);
        let text_end = texts
            .iter()
            .filter(|t| t.text != "▲" && t.text != "▼")
            .map(|t| t.x + t.text.width() as f32 * 10.0)
            .fold(0.0, f32::max);
        for arrow in arrows {
            assert!(arrow.x >= text_end + 5.0, "{} vs {text_end}", arrow.x);
            assert!(arrow.x + 10.0 <= bounds.x + bounds.width - 5.0);
        }
    }

    #[test]
    fn draws_headings_entries_and_shortcuts_inside_the_menu() {
        let view = view(1000.0, 1200.0);
        let mut menu = menu(true, 1172.0);
        menu.select(Step::First, view);
        let bounds = menu.geometry(view).bounds;
        let (rects, texts) = menu.draw(view, BLACK, WHITE);
        let strings: Vec<&str> = texts.iter().map(|t| t.text.as_str()).collect();
        for expected in ["TABS", "New Tab", "PANES", "Split Side by Side", "NUNTIO"] {
            assert!(strings.contains(&expected), "{expected:?} in {strings:?}");
        }
        let new_tab = if cfg!(target_os = "macos") {
            "Cmd+T"
        } else {
            "Ctrl+Shift+T"
        };
        assert!(strings.contains(&new_tab), "{new_tab:?} in {strings:?}");
        for r in &rects {
            assert!(
                r.x >= bounds.x && r.x + r.width <= bounds.x + bounds.width,
                "{r:?}"
            );
            assert!(
                r.y >= bounds.y && r.y + r.height <= bounds.y + bounds.height,
                "{r:?}"
            );
        }
        // Everything fits: no scroll arrows.
        assert!(!strings.contains(&"▲") && !strings.contains(&"▼"));
    }

    #[test]
    fn the_shell_menu_opens_numbered_tabs_and_shows_the_new_tab_shortcut() {
        let anchor = Anchor {
            x: 10.0,
            y: 28.0,
            opens_up: false,
            centered: false,
        };
        let names = ["Ubuntu".to_owned(), "PowerShell 7".to_owned()];
        let bindings = Bindings::platform_defaults();
        let menu = ActionsMenu::shells(anchor, &names, &bindings);
        assert_eq!(menu.kind(), MenuKind::Shells);
        assert_eq!(
            entries(&menu),
            [Action::NewTabWithShell(0), Action::NewTabWithShell(1)]
        );
        let (_, texts) = menu.draw(view(1000.0, 800.0), BLACK, WHITE);
        let strings: Vec<&str> = texts.iter().map(|t| t.text.as_str()).collect();
        let new_tab = bindings.shortcut(Action::NewTab).unwrap();
        for expected in ["NEW TAB WITH", "Ubuntu", "PowerShell 7", new_tab.as_str()] {
            assert!(strings.contains(&expected), "{expected:?} in {strings:?}");
        }
        let shortcuts = texts.iter().filter(|t| t.text.contains('+')).count();
        assert_eq!(shortcuts, 1, "{strings:?}");
    }

    #[test]
    fn the_tab_list_marks_and_selects_the_active_tab() {
        let anchor = Anchor {
            x: 10.0,
            y: 34.0,
            opens_up: false,
            centered: false,
        };
        let titles: Vec<String> = (0..100)
            .map(|i| {
                if i == 0 {
                    "x".repeat(100)
                } else {
                    format!("tab {i}")
                }
            })
            .collect();
        let view = view(1000.0, 300.0);
        let menu = ActionsMenu::tabs(anchor, &titles, 60, view);
        assert_eq!(menu.kind(), MenuKind::Tabs);
        assert_eq!(
            entries(&menu),
            (0..100).map(Action::SelectTab).collect::<Vec<_>>()
        );
        assert_eq!(menu.selected_action(), Some(Action::SelectTab(60)));
        let geometry = menu.geometry(view);
        assert!((geometry.first..geometry.first + geometry.visible).contains(&61));
        let (_, texts) = menu.draw(view, BLACK, WHITE);
        assert_eq!(texts.iter().filter(|t| t.text == "•").count(), 1);
        let Row::Entry { label, .. } = &menu.rows[1] else {
            panic!("row 1 is an entry");
        };
        assert!(label.width() <= TAB_MENU_CELLS, "{label}");
    }
}
