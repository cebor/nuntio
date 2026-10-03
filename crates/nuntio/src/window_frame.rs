//! Putting a frame together: the panes' snapshots, the tab and status
//! bars, dividers, the find bar, the link hint and the banner.

use std::time::Instant;

use nuntio_config::Config;
use nuntio_render::{Frame, FrameStatus, PaneView, Rect, Renderer, UiRect, UiText};
use nuntio_term::{CursorStyle, Link, Rgb, Snapshot, UnderlineStyle};
use winit::dpi::{PhysicalPosition, PhysicalSize};

use super::{WindowState, datetime};
use crate::banner::Banner;
use crate::ime;
use crate::link;
use crate::pane_tree::Layout;
use crate::status_bar::Stats;
use crate::style::mix;
use crate::tab_bar::TabLabel;

/// Radius of the window's corners where nuntio rounds them itself (Linux),
/// in logical pixels, as GNOME's.
const WINDOW_RADIUS: f64 = 12.0;

/// A pane's snapshot and where it goes.
struct PaneShot {
    snapshot: Snapshot,
    rect: Rect,
    focused: bool,
}

/// The UI drawn over the panes, back to front.
#[derive(Default)]
struct Overlay {
    rects: Vec<UiRect>,
    texts: Vec<UiText>,
    /// Drawn over everything else, text included.
    popup_rects: Vec<UiRect>,
    popup_texts: Vec<UiText>,
}

impl Overlay {
    fn add(&mut self, (rects, texts): (Vec<UiRect>, Vec<UiText>)) {
        self.rects.extend(rects);
        self.texts.extend(texts);
    }
}

impl WindowState {
    pub fn redraw(
        &mut self,
        config: &Config,
        stats: &Stats,
        banner: Option<&Banner>,
    ) -> FrameStatus {
        self.with_frame(config, stats, banner, |renderer, frame| {
            renderer.render(frame)
        })
        .unwrap_or(FrameStatus::Skipped)
    }

    /// Put together the frame for the window and hand it to `draw`.
    /// `None` if there is nothing to draw.
    pub fn with_frame<R>(
        &mut self,
        config: &Config,
        stats: &Stats,
        banner: Option<&Banner>,
        draw: impl FnOnce(&mut Renderer, &Frame) -> R,
    ) -> Option<R> {
        let layout = self.layout(config);
        let shots = self.pane_shots(config, &layout);
        let first = &shots.first()?.snapshot;
        let colors = (first.background, first.foreground);
        let overlay = self.overlay(config, stats, banner, &layout, colors);

        let split = shots.len() > 1;
        let panes: Vec<PaneView> = shots
            .iter()
            .map(|shot| {
                let (x, y) = self.grid_origin(config, shot.rect);
                PaneView {
                    snapshot: &shot.snapshot,
                    x,
                    y,
                    area: shot.rect,
                    dim: if split && !shot.focused {
                        config.panes.dim_inactive
                    } else {
                        0.0
                    },
                }
            })
            .collect();
        // Maximized windows sit flush with the screen edges: square corners.
        let corner_radius = if self.chrome.draws_corners() && !self.window.is_maximized() {
            (WINDOW_RADIUS * self.scale()) as f32
        } else {
            0.0
        };
        let frame = Frame {
            background: colors.0,
            background_opacity: config.window.opacity,
            panes: &panes,
            rects: &overlay.rects,
            texts: &overlay.texts,
            popup_rects: &overlay.popup_rects,
            popup_texts: &overlay.popup_texts,
            corner_radius,
        };
        let result = draw(&mut self.renderer, &frame);
        // The buffers go back for the next frame, without the images: a
        // closed pane's must not stay alive.
        self.shot_pool.extend(shots.into_iter().map(|shot| {
            let mut snapshot = shot.snapshot;
            snapshot.images.clear();
            snapshot
        }));
        Some(result)
    }

    /// Snapshots of the active tab's visible panes, with search matches,
    /// the hovered link and the cursor as they are shown.
    fn pane_shots(&mut self, config: &Config, layout: &Layout) -> Vec<PaneShot> {
        let focused_id = self.content().focused;
        let mut shots = Vec::with_capacity(layout.panes.len());
        for &(id, rect) in &layout.panes {
            let Some(pane) = self.tabs.active().content.pane(id) else {
                continue;
            };
            let focused = id == focused_id;
            let search = self.search.as_mut().and_then(|bar| bar.search_mut());
            let mut snapshot = self.shot_pool.pop().unwrap_or_default();
            match search {
                Some(search) if focused => pane.term.search_snapshot_into(search, &mut snapshot),
                _ => pane.term.snapshot_into(&mut snapshot),
            }
            if let Some((link_pane, link)) = &self.mouse.hover_link
                && *link_pane == id
            {
                underline(&mut snapshot, link);
            }
            if focused {
                self.prepare_focused(config, &mut snapshot, rect);
            } else if let Some(cursor) = snapshot.cursor.as_mut() {
                cursor.style = CursorStyle::HollowBlock;
            }
            shots.push(PaneShot {
                snapshot,
                rect,
                focused,
            });
        }
        shots
    }

    /// Everything drawn over the panes, in the colors of the terminal's
    /// `(background, foreground)`.
    fn overlay(
        &mut self,
        config: &Config,
        stats: &Stats,
        banner: Option<&Banner>,
        layout: &Layout,
        (background, foreground): (Rgb, Rgb),
    ) -> Overlay {
        let metrics = self.ui_metrics();
        let mut overlay = Overlay::default();
        let labels = self.tab_labels(config);
        if let Some(bar) = self.tab_bar(config) {
            overlay.add(bar.draw(
                &labels,
                self.mouse.hovered_bar,
                self.window.is_maximized(),
                background,
                foreground,
            ));
        }

        let divider_color = mix(background, foreground, 0.25);
        for divider in &layout.dividers {
            overlay
                .rects
                .push(UiRect::fill(divider.rect, divider_color));
        }

        if config.status_bar.visible() {
            let datetime = datetime(config);
            if let Some(bar) = self.status_bar(config, stats, &datetime) {
                let rainbow = config.status_bar.rainbow;
                overlay.add(bar.draw(stats, &datetime, background, foreground, rainbow));
            }
        }

        if let Some(bar) = &self.search
            && let Some(rect) = layout.rect(self.content().focused)
        {
            overlay.add(bar.draw(rect, metrics, background, foreground));
        }

        // Where the link under the pointer leads: OSC 8 links may show
        // other text.
        if let Some((id, link)) = &self.mouse.hover_link
            && let Some(mut rect) = layout.rect(*id)
            && let Some(pos) = self.mouse.position
        {
            // Stay above the banner, which covers the bottom of the window.
            if let Some(banner) = banner {
                let (top, _) = banner.bounds(self.banner_bottom(config), metrics);
                rect.height = rect.height.min(top - rect.y).max(0.0);
            }
            let pointer = (pos.x as f32, pos.y as f32);
            overlay.add(link::draw_hint(
                &link.url, rect, pointer, metrics, background, foreground,
            ));
        }

        if let Some(banner) = banner {
            let width = self.window.inner_size().width as f32;
            let (rect, text) = banner.draw(width, self.banner_bottom(config), metrics);
            overlay.rects.push(rect);
            overlay.texts.push(text);
        }

        if let Some(menu) = &self.actions_menu {
            let (rects, texts) = menu.draw(self.menu_view(), background, foreground);
            overlay.popup_rects = rects;
            overlay.popup_texts = texts;
        }
        overlay
    }

    /// The labels of the tabs, and the window title from the active one.
    /// Titles look up the foreground process, so only the shown ones are
    /// computed: all with a tab bar, else the active one's.
    fn tab_labels(&mut self, config: &Config) -> Vec<TabLabel> {
        let active = self.tabs.active_index();
        let bar_visible = self.bar_visible(config);
        let now = Instant::now();
        let mut refresh: Option<Instant> = None;
        let labels: Vec<TabLabel> = self
            .tabs
            .iter_mut()
            .enumerate()
            .filter(|&(i, _)| bar_visible || i == active)
            .map(|(i, tab)| {
                let focused = tab.content.focused;
                let (title, expiry) = match tab.content.pane_mut(focused) {
                    Some(pane) => pane.cached_title(config.tabs.title, now),
                    None => (String::new(), None),
                };
                if let Some(expiry) = expiry {
                    refresh = Some(refresh.map_or(expiry, |r| r.min(expiry)));
                }
                TabLabel {
                    title,
                    active: i == active,
                    activity: tab.activity,
                    bell: tab.bell,
                }
            })
            .collect();
        self.title_refresh = refresh;
        if let Some(label) = labels.iter().find(|l| l.active)
            && label.title != self.title
        {
            self.window.set_title(&label.title);
            self.title = label.title.clone();
        }
        labels
    }

    /// Preedit, blinking and IME placement for the focused pane.
    fn prepare_focused(&mut self, config: &Config, snapshot: &mut Snapshot, rect: Rect) {
        if let Some(preedit) = &self.preedit {
            ime::overlay_preedit(snapshot, preedit);
        }
        let blinking =
            self.focused && self.preedit.is_none() && snapshot.cursor.is_some_and(|c| c.blinking);
        if blinking != self.blink.active {
            self.blink.active = blinking;
            self.blink.reset();
        }
        if let Some(cursor) = snapshot.cursor.as_mut() {
            let (x, y) = self.grid_origin(config, rect);
            let cell = self.renderer.cell_metrics();
            self.update_ime_area(
                x as u32 + cursor.column as u32 * cell.width,
                y as u32 + cursor.line as u32 * cell.height,
            );
            if !self.focused {
                cursor.style = CursorStyle::HollowBlock;
            }
        }
        if self.blink.active && !self.blink.visible {
            snapshot.cursor = None;
        }
    }

    /// Keep the IME candidate window next to the cursor.
    fn update_ime_area(&mut self, x: u32, y: u32) {
        if self.ime_cell == Some((x, y)) {
            return;
        }
        self.ime_cell = Some((x, y));
        let cell = self.renderer.cell_metrics();
        self.window.set_ime_cursor_area(
            PhysicalPosition::new(x, y),
            PhysicalSize::new(cell.width, cell.height),
        );
    }
}

/// Underline the cells of a link (viewport positions, inclusive; its
/// lines may start above the viewport).
fn underline(snapshot: &mut Snapshot, link: &Link) {
    let columns = snapshot.columns as i64;
    if columns == 0 {
        return;
    }
    let index =
        |(column, line): (usize, i32)| i64::from(line) * columns + (column as i64).min(columns - 1);
    let Ok(end) = usize::try_from(index(link.end)) else {
        return;
    };
    let start = usize::try_from(index(link.start)).unwrap_or(0);
    for cell in snapshot.cells.iter_mut().take(end + 1).skip(start) {
        cell.style.underline = Some(UnderlineStyle::Single);
    }
}

#[cfg(test)]
mod tests {
    use nuntio_term::{CellStyle, SnapshotCell};

    use super::*;

    #[test]
    fn underline_covers_a_wrapped_link() {
        let cell = SnapshotCell {
            c: 'x',
            zerowidth: None,
            fg: Rgb::default(),
            bg: Rgb::default(),
            style: CellStyle::default(),
            underline_color: None,
        };
        let mut snapshot = Snapshot {
            columns: 4,
            lines: 3,
            cells: vec![cell.clone(); 12],
            ..Snapshot::default()
        };
        let link = Link {
            url: "https://x".into(),
            start: (2, 0),
            end: (1, 1),
        };
        underline(&mut snapshot, &link);
        let underlined: Vec<bool> = snapshot
            .cells
            .iter()
            .map(|c| c.style.underline.is_some())
            .collect();
        #[rustfmt::skip]
        let expected = [
            false, false, true, true,
            true, true, false, false,
            false, false, false, false,
        ];
        assert_eq!(underlined, expected);

        let mut snapshot = Snapshot {
            cells: vec![cell; 12],
            ..snapshot
        };
        let above = Link {
            url: "https://x".into(),
            start: (3, -2),
            end: (0, 1),
        };
        underline(&mut snapshot, &above);
        let count = snapshot
            .cells
            .iter()
            .filter(|c| c.style.underline.is_some());
        assert_eq!(count.count(), 5, "from the top to the second line's start");
    }
}
