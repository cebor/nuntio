//! Popups over what an app has drawn.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::widgets::Clear;

use crate::style::{heading, panel};

/// A bordered popup of about `width`×`height` in the middle of the screen,
/// over what is below it, with `title` in the top border. It leaves four
/// columns and two rows of the screen free around it. Returns the area
/// inside the border.
pub fn open(frame: &mut Frame, width: u16, height: u16, title: &str) -> Rect {
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    let block = panel(true)
        .title(format!(" {title} "))
        .title_style(heading());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// A centered area of the given size, clamped to `area` less a margin.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_keeps_a_margin_to_the_screen() {
        let screen = Rect::new(0, 0, 40, 10);
        assert_eq!(centered(screen, 20, 4), Rect::new(10, 3, 20, 4));
        assert_eq!(centered(screen, 60, 20), Rect::new(2, 1, 36, 8));
    }
}
