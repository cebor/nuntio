//! Status bar layout and drawing: system graphs, date and time.

use std::collections::VecDeque;

use nuntio_config::StatusItem;
use nuntio_render::{CellMetrics, UiRect, UiText};
use nuntio_term::Rgb;
use unicode_width::UnicodeWidthStr;

use crate::sysmon::Sample;
use crate::tab_bar::{bar_background, mix};

/// Samples kept per graph: one minute at one sample per second.
const HISTORY: usize = 60;
/// Space above and below the text, in logical pixels.
const BAR_PADDING: f64 = 4.0;
/// Width of a sparkline, in cells.
const GRAPH_CELLS: usize = 6;
/// Space between two items, in cells.
const GAP_CELLS: usize = 2;
/// Width of an item's icon, in cells.
const ICON_CELLS: usize = 2;
/// Where an item's content starts after its icon, in cells.
const CONTENT: usize = ICON_CELLS + 1;
/// Network graphs scale to at least this rate, so idle chatter stays flat.
const MIN_NET_SCALE: f32 = 1024.0;

/// The most recent values of one graph, oldest first.
#[derive(Debug, Clone, Default)]
struct History(VecDeque<f32>);

impl History {
    fn push(&mut self, value: f32) {
        if self.0.len() == HISTORY {
            self.0.pop_front();
        }
        self.0.push_back(value);
    }

    /// The newest `n` values, oldest first.
    fn recent(&self, n: usize) -> impl Iterator<Item = f32> + '_ {
        self.0.iter().skip(self.0.len().saturating_sub(n)).copied()
    }

    fn max(&self, n: usize) -> f32 {
        self.recent(n).fold(0.0, f32::max)
    }
}

/// Collected samples: graph histories and the latest reading.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    cpu: History,
    memory: History,
    down: History,
    up: History,
    latest: Sample,
}

impl Stats {
    pub fn push(&mut self, sample: Sample) {
        if let Some(cpu) = sample.cpu {
            self.cpu.push(cpu);
        }
        if let Some(memory) = sample.memory {
            self.memory
                .push(memory.used as f32 / memory.total.max(1) as f32 * 100.0);
        }
        if let Some(net) = sample.network {
            self.down.push(net.down as f32);
            self.up.push(net.up as f32);
        }
        self.latest = sample;
    }

    /// Forget everything, e.g. when the bar is turned off.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone)]
pub struct StatusBar {
    pub top: f32,
    pub height: f32,
    width: f32,
    padding: f32,
    scale: f32,
    cell: CellMetrics,
    /// Items that fit, with their left edge.
    slots: Vec<(StatusItem, f32)>,
}

impl StatusBar {
    /// Height of the bar for a font's cell size.
    pub fn height(cell: CellMetrics, scale: f64) -> f32 {
        cell.height as f32 + 2.0 * (BAR_PADDING * scale).round() as f32
    }

    /// Lay out `items` in a bar of `width` pixels whose top edge is at
    /// `top`. Items without data to show (no battery) are left out.
    pub fn new(
        width: f32,
        top: f32,
        items: &[StatusItem],
        stats: &Stats,
        datetime: &str,
        cell: CellMetrics,
        scale: f64,
    ) -> Self {
        let padding = (BAR_PADDING * scale).round() as f32;
        let height = Self::height(cell, scale);
        let shown: Vec<StatusItem> = items
            .iter()
            .copied()
            .filter(|&item| item != StatusItem::Battery || stats.latest.battery.is_some())
            .collect();
        let cw = cell.width as f32;
        let widths: Vec<f32> = shown
            .iter()
            .map(|&item| item_cells(item, datetime) as f32 * cw)
            .collect();
        let slots = layout(width, cw, GAP_CELLS as f32 * cw, &widths)
            .into_iter()
            .zip(shown)
            .filter_map(|(x, item)| Some((item, x?)))
            .collect();
        Self {
            top,
            height,
            width,
            padding,
            scale: scale as f32,
            cell,
            slots,
        }
    }

    pub fn draw(
        &self,
        stats: &Stats,
        datetime: &str,
        background: Rgb,
        foreground: Rgb,
    ) -> (Vec<UiRect>, Vec<UiText>) {
        let bar_bg = bar_background(background);
        let colors = Colors {
            background: bar_bg,
            label: mix(foreground, background, 0.45),
            value: foreground,
            track: mix(bar_bg, foreground, 0.06),
            graph: mix(bar_bg, foreground, 0.55),
            graph_alt: mix(bar_bg, foreground, 0.30),
            separator: mix(bar_bg, foreground, 0.15),
        };
        let mut out = Output {
            rects: vec![rect(0.0, self.top, self.width, self.height, bar_bg)],
            texts: Vec::new(),
        };
        // A thin line in the middle of the gap before every item but the first.
        let gap = GAP_CELLS as f32 * self.cell.width as f32;
        let (line_top, line_height) = self.graph_box();
        let stroke = self.stroke();
        for &(_, x) in self.slots.iter().skip(1) {
            out.rects.push(rect(
                (x - (gap + stroke) / 2.0).round(),
                line_top,
                stroke,
                line_height,
                colors.separator,
            ));
        }
        for &(item, x) in &self.slots {
            self.draw_item(&mut out, item, x, stats, datetime, &colors);
        }
        (out.rects, out.texts)
    }

    fn draw_item(
        &self,
        out: &mut Output,
        item: StatusItem,
        x: f32,
        stats: &Stats,
        datetime: &str,
        colors: &Colors,
    ) {
        let cw = self.cell.width as f32;
        let text_y = self.top + self.padding;
        let text = |cells: usize, text: String, color: Rgb, out: &mut Output| {
            out.texts.push(UiText {
                x: (x + cells as f32 * cw).floor(),
                y: text_y,
                text,
                color,
                bold: false,
            });
        };
        let graph_x = x + CONTENT as f32 * cw;
        let after_graph = CONTENT + GRAPH_CELLS + 1;
        let latest = &stats.latest;
        match item {
            StatusItem::Cpu => {
                self.cpu_icon(out, x, colors);
                self.sparkline(out, graph_x, &stats.cpu, 100.0, colors);
                let value = latest.cpu.map_or("--".into(), |cpu| format!("{cpu:.0}%"));
                text(after_graph, format!("{value:>4}"), colors.value, out);
            }
            StatusItem::Memory => {
                self.memory_icon(out, x, colors);
                self.sparkline(out, graph_x, &stats.memory, 100.0, colors);
                let value = latest.memory.map_or("--".into(), |m| format_gib(m.used));
                text(after_graph, format!("{value:>5}"), colors.value, out);
            }
            StatusItem::Network => {
                self.network_icon(out, x, colors);
                self.network_graph(out, graph_x, stats, colors);
                let (down, up) = latest.network.map_or(("--".into(), "--".into()), |net| {
                    (format_rate(net.down), format_rate(net.up))
                });
                text(after_graph, "↓".into(), colors.label, out);
                text(after_graph + 1, format!("{down:>4}"), colors.value, out);
                text(after_graph + 6, "↑".into(), colors.label, out);
                text(after_graph + 7, format!("{up:>4}"), colors.value, out);
            }
            StatusItem::Battery => {
                let Some(battery) = latest.battery else {
                    return;
                };
                self.battery_icon(out, x, battery.level, colors);
                let level = format!("{:.0}%", battery.level);
                text(CONTENT, format!("{level:>4}"), colors.value, out);
                if battery.charging {
                    text(CONTENT + 4, "⚡".into(), colors.value, out);
                }
            }
            StatusItem::Datetime => {
                self.clock_icon(out, x, colors);
                text(CONTENT, datetime.to_owned(), colors.value, out);
            }
        }
    }

    /// Vertical extent of graphs: the text row, slightly inset.
    fn graph_box(&self) -> (f32, f32) {
        let inset = (2.0 * self.scale).round();
        let top = self.top + self.padding + inset;
        (top, self.cell.height as f32 - 2.0 * inset)
    }

    /// Samples that fit into a graph.
    fn graph_samples(&self) -> usize {
        let width = GRAPH_CELLS as f32 * self.cell.width as f32;
        ((width / self.stroke()) as usize).min(HISTORY)
    }

    fn track(&self, out: &mut Output, x: f32, colors: &Colors) {
        let (top, height) = self.graph_box();
        let width = GRAPH_CELLS as f32 * self.cell.width as f32;
        let mut track = rect(x, top, width, height, colors.track);
        track.radius = (2.0 * self.scale).round();
        out.rects.push(track);
    }

    /// Bars growing up from the bottom, newest at the right edge.
    fn sparkline(&self, out: &mut Output, x: f32, history: &History, max: f32, colors: &Colors) {
        self.track(out, x, colors);
        let (top, height) = self.graph_box();
        let bottom = top + height;
        for (bx, value) in self.graph_bars(x, history) {
            let h = (value / max).clamp(0.0, 1.0) * height;
            if h > 0.0 {
                out.rects
                    .push(rect(bx, bottom - h, self.stroke(), h, colors.graph));
            }
        }
    }

    /// Download grows up from the middle, upload down, on a shared scale.
    fn network_graph(&self, out: &mut Output, x: f32, stats: &Stats, colors: &Colors) {
        self.track(out, x, colors);
        let n = self.graph_samples();
        let max = stats.down.max(n).max(stats.up.max(n)).max(MIN_NET_SCALE);
        let (top, height) = self.graph_box();
        let middle = (top + height / 2.0).round();
        let half = height / 2.0;
        for (bx, value) in self.graph_bars(x, &stats.down) {
            let h = (value / max).clamp(0.0, 1.0) * half;
            if h > 0.0 {
                out.rects
                    .push(rect(bx, middle - h, self.stroke(), h, colors.graph));
            }
        }
        for (bx, value) in self.graph_bars(x, &stats.up) {
            let h = (value / max).clamp(0.0, 1.0) * half;
            if h > 0.0 {
                out.rects
                    .push(rect(bx, middle, self.stroke(), h, colors.graph_alt));
            }
        }
    }

    /// Left edge and value of each bar, aligned to the graph's right edge.
    fn graph_bars<'a>(
        &self,
        x: f32,
        history: &'a History,
    ) -> impl Iterator<Item = (f32, f32)> + 'a {
        let n = self.graph_samples();
        let count = history.recent(n).count();
        let bar = self.stroke();
        let right = x + GRAPH_CELLS as f32 * self.cell.width as f32;
        history
            .recent(n)
            .enumerate()
            .map(move |(i, value)| (right - (count - i) as f32 * bar, value))
    }

    fn stroke(&self) -> f32 {
        self.scale.round().max(1.0)
    }

    /// Left edge, top edge and size of the square an icon is drawn in,
    /// centered in the icon cells.
    fn icon_box(&self, x: f32) -> (f32, f32, f32) {
        let (top, size) = self.graph_box();
        let width = ICON_CELLS as f32 * self.cell.width as f32;
        ((x + (width - size) / 2.0).floor(), top, size)
    }

    /// A chip: an outlined square with a filled core and three pins per side.
    fn cpu_icon(&self, out: &mut Output, x: f32, colors: &Colors) {
        let st = self.stroke();
        let (x0, y0, size) = self.icon_box(x);
        let pin = (size * 0.2).round().max(st);
        let body = size - 2.0 * pin;
        let color = colors.label;
        outline(out, x0 + pin, y0 + pin, body, body, st, color);
        let core = (body * 0.4).round();
        let offset = ((body - core) / 2.0).round();
        out.rects.push(rect(
            x0 + pin + offset,
            y0 + pin + offset,
            core,
            core,
            color,
        ));
        for k in 1..=3 {
            let along = (pin + body * k as f32 / 4.0 - st / 2.0).round();
            out.rects.push(rect(x0 + along, y0, st, pin, color));
            out.rects
                .push(rect(x0 + along, y0 + size - pin, st, pin, color));
            out.rects.push(rect(x0, y0 + along, pin, st, color));
            out.rects
                .push(rect(x0 + size - pin, y0 + along, pin, st, color));
        }
    }

    /// A memory module: a wide board with chips and a row of contacts.
    fn memory_icon(&self, out: &mut Output, x: f32, colors: &Colors) {
        let st = self.stroke();
        let (_, y0, size) = self.icon_box(x);
        let width = ICON_CELLS as f32 * self.cell.width as f32;
        let (x0, board_w) = (x.floor() + st, (width - 2.0 * st).floor());
        let board_top = y0 + (size * 0.15).round();
        let board_h = (size * 0.55).round();
        let color = colors.label;
        outline(out, x0, board_top, board_w, board_h, st, color);
        // Three chips on the board.
        let gap = 2.0 * st;
        let chip_w = ((board_w - 2.0 * st - 4.0 * gap) / 3.0).floor();
        let chip_h = board_h - 2.0 * st - 2.0 * gap;
        for k in 0..3 {
            let cx = x0 + st + gap + k as f32 * (chip_w + gap);
            out.rects
                .push(rect(cx, board_top + st + gap, chip_w, chip_h, color));
        }
        // Contacts below the board.
        let contacts_top = board_top + board_h;
        let contacts_h = (size * 0.2).round().max(st);
        let mut cx = x0 + gap;
        while cx + st <= x0 + board_w - gap {
            out.rects
                .push(rect(cx, contacts_top, st, contacts_h, color));
            cx += 2.0 * st;
        }
    }

    /// A small network: one node linked to two below it.
    fn network_icon(&self, out: &mut Output, x: f32, colors: &Colors) {
        let st = self.stroke();
        let (x0, y0, size) = self.icon_box(x);
        let node_w = (size * 0.4).round();
        let node_h = (size * 0.3).round();
        let color = colors.label;
        let center = (x0 + size / 2.0 - st / 2.0).round();
        out.rects.push(rect(
            (x0 + (size - node_w) / 2.0).round(),
            y0,
            node_w,
            node_h,
            color,
        ));
        let bottom_top = y0 + size - node_h;
        let bus_y = (y0 + size / 2.0 - st / 2.0).round();
        let left = x0 + (node_w / 2.0 - st / 2.0).round();
        let right = x0 + size - (node_w / 2.0 + st / 2.0).round();
        out.rects
            .push(rect(center, y0 + node_h, st, bus_y - y0 - node_h, color));
        out.rects
            .push(rect(left, bus_y, right - left + st, st, color));
        out.rects
            .push(rect(left, bus_y, st, bottom_top - bus_y, color));
        out.rects
            .push(rect(right, bus_y, st, bottom_top - bus_y, color));
        out.rects.push(rect(x0, bottom_top, node_w, node_h, color));
        out.rects
            .push(rect(x0 + size - node_w, bottom_top, node_w, node_h, color));
    }

    /// A clock face: a ring with hands at three o'clock.
    fn clock_icon(&self, out: &mut Output, x: f32, colors: &Colors) {
        let st = self.stroke();
        let (x0, y0, size) = self.icon_box(x);
        let color = colors.label;
        let circle = |x, y, size, color| UiRect {
            radius: size / 2.0,
            ..rect(x, y, size, size, color)
        };
        out.rects.push(circle(x0, y0, size, color));
        out.rects
            .push(circle(x0 + st, y0 + st, size - 2.0 * st, colors.background));
        let center = (size / 2.0 - st / 2.0).round();
        let hand = (size * 0.3).round();
        out.rects
            .push(rect(x0 + center, y0 + center - hand + st, st, hand, color));
        out.rects
            .push(rect(x0 + center, y0 + center, hand, st, color));
    }

    /// An outlined battery with a knob on the right, filled to `level`.
    fn battery_icon(&self, out: &mut Output, x: f32, level: f32, colors: &Colors) {
        let stroke = self.stroke();
        let cw = self.cell.width as f32;
        let (top, height) = self.graph_box();
        let knob = (2.0 * self.scale).round().max(1.0);
        let body = ICON_CELLS as f32 * cw - knob - stroke;
        let color = colors.label;
        outline(out, x, top, body, height, stroke, color);
        let knob_height = (height / 3.0).round();
        out.rects.push(rect(
            x + body,
            (top + (height - knob_height) / 2.0).round(),
            knob,
            knob_height,
            color,
        ));
        // One (device-independent) pixel of air between outline and fill.
        let inner = 2.0 * stroke;
        let fill = ((body - 2.0 * inner) * level / 100.0).round();
        if fill > 0.0 {
            out.rects.push(rect(
                x + inner,
                top + inner,
                fill,
                height - 2.0 * inner,
                colors.graph,
            ));
        }
    }
}

struct Colors {
    background: Rgb,
    label: Rgb,
    value: Rgb,
    track: Rgb,
    graph: Rgb,
    graph_alt: Rgb,
    separator: Rgb,
}

struct Output {
    rects: Vec<UiRect>,
    texts: Vec<UiText>,
}

fn rect(x: f32, y: f32, width: f32, height: f32, color: Rgb) -> UiRect {
    UiRect {
        x,
        y,
        width,
        height,
        color,
        radius: 0.0,
    }
}

/// A rectangle drawn with lines `stroke` pixels thick.
fn outline(out: &mut Output, x: f32, y: f32, width: f32, height: f32, stroke: f32, color: Rgb) {
    out.rects.push(rect(x, y, width, stroke, color));
    out.rects
        .push(rect(x, y + height - stroke, width, stroke, color));
    out.rects.push(rect(x, y, stroke, height, color));
    out.rects
        .push(rect(x + width - stroke, y, stroke, height, color));
}

/// Width of an item in cells. Values have a fixed width so items don't
/// shift as the numbers change.
fn item_cells(item: StatusItem, datetime: &str) -> usize {
    match item {
        // icon graph " 100%"
        StatusItem::Cpu => CONTENT + GRAPH_CELLS + 1 + 4,
        // icon graph " 12.3G"
        StatusItem::Memory => CONTENT + GRAPH_CELLS + 1 + 5,
        // icon graph " ↓1.2M ↑ 30K"
        StatusItem::Network => CONTENT + GRAPH_CELLS + 1 + 5 + 1 + 5,
        // icon " 100%" "⚡"
        StatusItem::Battery => CONTENT + 4 + 2,
        // icon "Fri 25 Sep 10:50"
        StatusItem::Datetime => CONTENT + datetime.width(),
    }
}

/// Left edges of items of the given widths: the last at the right edge,
/// the others from the left. Items that don't fit before the last one are
/// dropped from the end, so earlier items take precedence.
fn layout(width: f32, margin: f32, gap: f32, widths: &[f32]) -> Vec<Option<f32>> {
    let Some((&last, rest)) = widths.split_last() else {
        return Vec::new();
    };
    let right = (width - margin - last).max(margin);
    let mut x = margin;
    let mut fits = true;
    let mut out: Vec<Option<f32>> = rest
        .iter()
        .map(|&w| {
            fits &= x + w + gap <= right;
            let pos = fits.then_some(x);
            x += w + gap;
            pos
        })
        .collect();
    out.push(Some(right));
    out
}

/// A rate in at most four characters: `999B`, `1.2K`, `34M`.
fn format_rate(bytes_per_sec: f64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes_per_sec.max(0.0);
    let mut unit = 0;
    while value >= 999.5 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{value:.0}B"),
        _ if value < 9.95 => format!("{value:.1}{}", UNITS[unit]),
        _ => format!("{value:.0}{}", UNITS[unit]),
    }
}

/// Bytes as GiB in at most five characters: `8.1G`, `12.3G`, `128G`.
fn format_gib(bytes: u64) -> String {
    let gib = bytes as f64 / (1u64 << 30) as f64;
    if gib < 99.95 {
        format!("{gib:.1}G")
    } else {
        format!("{gib:.0}G")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysmon::{Battery, Memory, Throughput};

    const CELL: CellMetrics = CellMetrics {
        width: 10,
        height: 20,
        baseline: 15,
        underline_y: 17,
        stroke: 1,
        strikeout_y: 10,
    };

    #[test]
    fn last_item_is_right_aligned_the_rest_fill_from_the_left() {
        assert_eq!(
            layout(1000.0, 10.0, 20.0, &[100.0, 200.0, 50.0]),
            [Some(10.0), Some(130.0), Some(940.0)]
        );
        assert_eq!(layout(1000.0, 10.0, 20.0, &[50.0]), [Some(940.0)]);
        assert_eq!(layout(1000.0, 10.0, 20.0, &[]), []);
    }

    #[test]
    fn items_before_the_last_are_dropped_when_narrow() {
        // 400px: the last item starts at 340; the second would end at 350.
        assert_eq!(
            layout(400.0, 10.0, 20.0, &[100.0, 200.0, 50.0, 50.0]),
            [Some(10.0), None, None, Some(340.0)]
        );
        // Too narrow for anything else; the last item keeps the margin.
        assert_eq!(layout(40.0, 10.0, 20.0, &[10.0, 50.0]), [None, Some(10.0)]);
    }

    #[test]
    fn history_keeps_the_newest_samples() {
        let mut history = History::default();
        for i in 0..HISTORY + 5 {
            history.push(i as f32);
        }
        assert_eq!(history.0.len(), HISTORY);
        assert_eq!(history.0.front(), Some(&5.0));
        assert_eq!(history.recent(2).collect::<Vec<_>>(), [63.0, 64.0]);
        assert_eq!(history.max(3), 64.0);
        assert_eq!(History::default().max(10), 0.0);
    }

    #[test]
    fn rates_and_sizes_fit_their_columns() {
        assert_eq!(format_rate(0.0), "0B");
        assert_eq!(format_rate(999.0), "999B");
        assert_eq!(format_rate(1000.0), "1.0K");
        assert_eq!(format_rate(30.0 * 1024.0), "30K");
        assert_eq!(format_rate(1.25 * 1024.0 * 1024.0), "1.2M");
        assert_eq!(format_rate(1023.9 * 1024.0), "1.0M");
        assert_eq!(format_gib(8_700_000_000), "8.1G");
        assert_eq!(format_gib(128 << 30), "128G");
    }

    fn stats(battery: bool) -> Stats {
        let mut stats = Stats::default();
        stats.push(Sample {
            cpu: Some(50.0),
            memory: Some(Memory {
                used: 4 << 30,
                total: 16 << 30,
            }),
            network: Some(Throughput {
                down: 2048.0,
                up: 0.0,
            }),
            battery: battery.then_some(Battery {
                level: 80.0,
                charging: true,
            }),
        });
        stats
    }

    #[test]
    fn missing_battery_is_left_out() {
        let items = [StatusItem::Cpu, StatusItem::Battery, StatusItem::Datetime];
        let bar = StatusBar::new(1000.0, 0.0, &items, &stats(false), "12:34", CELL, 1.0);
        let shown: Vec<_> = bar.slots.iter().map(|s| s.0).collect();
        assert_eq!(shown, [StatusItem::Cpu, StatusItem::Datetime]);
        // Icon, space and "12:34" (8 cells) sit at the right margin of one cell.
        assert_eq!(bar.slots[1].1, 1000.0 - 10.0 - 80.0);

        let bar = StatusBar::new(1000.0, 0.0, &items, &stats(true), "12:34", CELL, 1.0);
        assert_eq!(bar.slots.len(), 3);
    }

    #[test]
    fn draws_values_and_graphs_inside_the_bar() {
        let items = [
            StatusItem::Cpu,
            StatusItem::Memory,
            StatusItem::Network,
            StatusItem::Battery,
            StatusItem::Datetime,
        ];
        let stats = stats(true);
        let bar = StatusBar::new(1200.0, 500.0, &items, &stats, "12:34", CELL, 1.0);
        assert_eq!(bar.height, 28.0);
        let (bg, fg) = (
            Rgb { r: 0, g: 0, b: 0 },
            Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
        );
        let (rects, texts) = bar.draw(&stats, "12:34", bg, fg);
        let texts: Vec<&str> = texts.iter().map(|t| t.text.as_str()).collect();
        for expected in [" 50%", " 4.0G", "2.0K", "  0B", " 80%", "⚡", "12:34"] {
            assert!(texts.contains(&expected), "{expected:?} in {texts:?}");
        }
        for r in &rects {
            assert!(r.y >= 500.0 && r.y + r.height <= 528.0, "{r:?}");
        }
        // Half-full CPU bar: half of the 16px graph box.
        assert!(rects.iter().any(|r| r.width == 1.0 && r.height == 8.0));
    }
}
