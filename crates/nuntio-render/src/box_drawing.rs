//! Procedurally drawn box-drawing and block characters (U+2500–U+259F), so
//! lines connect seamlessly across cells regardless of the font's metrics.
//!
//! Double lines (U+2550–U+256C) are left to the font for now.

/// Line weight of one arm of a box-drawing character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Weight {
    None,
    Light,
    Heavy,
}

/// Arms in the order up, right, down, left.
type Arms = [Weight; 4];

const UP: usize = 0;
const RIGHT: usize = 1;
const DOWN: usize = 2;
const LEFT: usize = 3;

/// A coverage mask of one cell.
struct Canvas {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; (width * height) as usize],
        }
    }

    /// Fill a rectangle given in pixels, clipped to the cell.
    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        let (x0, x1) = (x0.max(0) as u32, (x1.max(0) as u32).min(self.width));
        let (y0, y1) = (y0.max(0) as u32, (y1.max(0) as u32).min(self.height));
        for y in y0..y1 {
            let row = (y * self.width) as usize;
            self.data[row + x0 as usize..row + x1 as usize].fill(alpha);
        }
    }

    /// Antialiased coverage from a signed distance function (in pixels).
    fn shape(&mut self, half_thickness: f32, distance: impl Fn(f32, f32) -> f32) {
        for y in 0..self.height {
            for x in 0..self.width {
                let d = distance(x as f32 + 0.5, y as f32 + 0.5);
                let coverage = (half_thickness + 0.5 - d).clamp(0.0, 1.0);
                let px = &mut self.data[(y * self.width + x) as usize];
                *px = (*px).max((coverage * 255.0) as u8);
            }
        }
    }
}

/// Render `c` into a `width`×`height` mask, or `None` if the font should
/// draw it. `stroke` is the light line thickness.
pub fn rasterize(c: char, width: u32, height: u32, stroke: u32) -> Option<Vec<u8>> {
    let mut canvas = Canvas::new(width, height);
    let light = stroke.max(1) as i32;
    let heavy = light * 2;
    let (w, h) = (width as i32, height as i32);

    match c as u32 {
        0x2500..=0x254B | 0x2574..=0x257F => {
            if let Some((segments, arms)) = dashed(c) {
                draw_dashed(&mut canvas, arms, segments, light, heavy);
            } else {
                draw_arms(&mut canvas, lines(c)?, light, heavy);
            }
        }
        0x254C..=0x254F => {
            let (segments, arms) = dashed(c)?;
            draw_dashed(&mut canvas, arms, segments, light, heavy);
        }
        // Rounded corners: two straight arms joined by a quarter circle.
        0x256D..=0x2570 => {
            // Centered on the pixels of the straight lines in `draw_arms`.
            let center = |size: i32| ((size - light) / 2) as f32 + light as f32 / 2.0;
            let (cx, cy) = (center(w), center(h));
            let r = cx.min(cy);
            let (dx, dy) = match c {
                '╭' => (1.0, 1.0),
                '╮' => (-1.0, 1.0),
                '╯' => (-1.0, -1.0),
                _ => (1.0, -1.0), // ╰
            };
            let (ccx, ccy) = (cx + dx * r, cy + dy * r);
            let half = light as f32 / 2.0;
            canvas.shape(half, |x, y| {
                let in_quadrant = (x - ccx) * dx <= 0.0 && (y - ccy) * dy <= 0.0;
                if in_quadrant {
                    ((x - ccx).hypot(y - ccy) - r).abs()
                } else if (x - ccx) * dx > 0.0 && (y - ccy) * dy <= 0.0 {
                    // Straight horizontal arm towards the edge.
                    (y - cy).abs()
                } else if (y - ccy) * dy > 0.0 && (x - ccx) * dx <= 0.0 {
                    // Straight vertical arm towards the edge.
                    (x - cx).abs()
                } else {
                    f32::MAX
                }
            });
        }
        // Diagonals.
        0x2571..=0x2573 => {
            let (wf, hf) = (w as f32, h as f32);
            let len = wf.hypot(hf);
            let half = light as f32 / 2.0;
            // Distance to the line through (0,h)-(w,0) and (0,0)-(w,h).
            let rising = move |x: f32, y: f32| (hf * x + wf * y - wf * hf).abs() / len;
            let falling = move |x: f32, y: f32| (hf * x - wf * y).abs() / len;
            match c {
                '╱' => canvas.shape(half, rising),
                '╲' => canvas.shape(half, falling),
                _ => canvas.shape(half, |x, y| rising(x, y).min(falling(x, y))),
            }
        }
        0x2580..=0x259F => draw_block(&mut canvas, c)?,
        _ => return None,
    }
    Some(canvas.data)
}

fn weight(digit: u8) -> Weight {
    match digit {
        b'1' => Weight::Light,
        b'2' => Weight::Heavy,
        _ => Weight::None,
    }
}

/// Arms of the solid line characters, as "up right down left" digits
/// (0 = none, 1 = light, 2 = heavy).
fn lines(c: char) -> Option<Arms> {
    const TABLE: [&str; 0x4C] = [
        "0101", "0202", "1010", "2020", // ─ ━ │ ┃
        "", "", "", "", "", "", "", "", // dashed, see `dashed`
        "0110", "0210", "0120", "0220", // ┌ ┍ ┎ ┏
        "0011", "0012", "0021", "0022", // ┐ ┑ ┒ ┓
        "1100", "1200", "2100", "2200", // └ ┕ ┖ ┗
        "1001", "1002", "2001", "2002", // ┘ ┙ ┚ ┛
        "1110", "1210", "2110", "1120", "2120", "2210", "1220", "2220", // ├ … ┣
        "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022", // ┤ … ┫
        "0111", "0112", "0211", "0212", "0121", "0122", "0221", "0222", // ┬ … ┳
        "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", // ┴ … ┻
        "1111", "1112", "1211", "1212", "2111", "1121", "2121", "2112", // ┼ … ╃
        "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222", // ╄ … ╋
    ];
    const HALVES: [&str; 12] = [
        "0001", "1000", "0100", "0010", // ╴ ╵ ╶ ╷
        "0002", "2000", "0200", "0020", // ╸ ╹ ╺ ╻
        "0201", "1020", "0102", "2010", // ╼ ╽ ╾ ╿
    ];
    let code = c as u32;
    let spec = match code {
        0x2500..=0x254B => TABLE[(code - 0x2500) as usize],
        0x2574..=0x257F => HALVES[(code - 0x2574) as usize],
        _ => return None,
    };
    let b = spec.as_bytes();
    (b.len() == 4).then(|| [weight(b[0]), weight(b[1]), weight(b[2]), weight(b[3])])
}

/// Dashed lines: number of segments and the (straight) arms.
fn dashed(c: char) -> Option<(i32, Arms)> {
    use Weight::{Heavy, Light, None};
    let horizontal = |w| [None, w, None, w];
    let vertical = |w| [w, None, w, None];
    Some(match c {
        '┄' => (3, horizontal(Light)),
        '┅' => (3, horizontal(Heavy)),
        '┆' => (3, vertical(Light)),
        '┇' => (3, vertical(Heavy)),
        '┈' => (4, horizontal(Light)),
        '┉' => (4, horizontal(Heavy)),
        '┊' => (4, vertical(Light)),
        '┋' => (4, vertical(Heavy)),
        '╌' => (2, horizontal(Light)),
        '╍' => (2, horizontal(Heavy)),
        '╎' => (2, vertical(Light)),
        '╏' => (2, vertical(Heavy)),
        _ => return Option::None,
    })
}

fn thickness(weight: Weight, light: i32, heavy: i32) -> i32 {
    match weight {
        Weight::None => 0,
        Weight::Light => light,
        Weight::Heavy => heavy,
    }
}

/// Draw straight arms from the edges into the center. Arms extend across
/// the perpendicular band so joints are filled without gaps.
fn draw_arms(canvas: &mut Canvas, arms: Arms, light: i32, heavy: i32) {
    let (w, h) = (canvas.width as i32, canvas.height as i32);
    let t = |i: usize| thickness(arms[i], light, heavy);
    let band_h = t(LEFT).max(t(RIGHT)); // horizontal band thickness
    let band_v = t(UP).max(t(DOWN)); // vertical band thickness

    // Where the bands sit; arms without a perpendicular partner meet at
    // the center.
    let hy0 = (h - band_h) / 2;
    let vx0 = (w - band_v) / 2;

    if arms[UP] != Weight::None {
        let t = t(UP);
        let x = (w - t) / 2;
        let end = if band_h > 0 {
            hy0 + band_h
        } else {
            (h + t) / 2
        };
        canvas.rect(x, 0, x + t, end, 255);
    }
    if arms[DOWN] != Weight::None {
        let t = t(DOWN);
        let x = (w - t) / 2;
        let start = if band_h > 0 { hy0 } else { (h - t) / 2 };
        canvas.rect(x, start, x + t, h, 255);
    }
    if arms[LEFT] != Weight::None {
        let t = t(LEFT);
        let y = (h - t) / 2;
        let end = if band_v > 0 {
            vx0 + band_v
        } else {
            (w + t) / 2
        };
        canvas.rect(0, y, end, y + t, 255);
    }
    if arms[RIGHT] != Weight::None {
        let t = t(RIGHT);
        let y = (h - t) / 2;
        let start = if band_v > 0 { vx0 } else { (w - t) / 2 };
        canvas.rect(start, y, w, y + t, 255);
    }
}

fn draw_dashed(canvas: &mut Canvas, arms: Arms, segments: i32, light: i32, heavy: i32) {
    let (w, h) = (canvas.width as i32, canvas.height as i32);
    let horizontal = arms[LEFT] != Weight::None;
    let weight = if horizontal { arms[LEFT] } else { arms[UP] };
    let t = thickness(weight, light, heavy);
    let length = if horizontal { w } else { h };
    let gap = (length / segments / 3).max(1);
    for i in 0..segments {
        // Segment boundaries spread evenly; the gap is centered on each
        // boundary so dashes line up across neighbouring cells.
        let start = length * i / segments + gap / 2;
        let end = length * (i + 1) / segments - (gap - gap / 2);
        if horizontal {
            let y = (h - t) / 2;
            canvas.rect(start, y, end, y + t, 255);
        } else {
            let x = (w - t) / 2;
            canvas.rect(x, start, x + t, end, 255);
        }
    }
}

fn draw_block(canvas: &mut Canvas, c: char) -> Option<()> {
    let (w, h) = (canvas.width as i32, canvas.height as i32);
    let eighth_h = |n: i32| h * n / 8;
    let eighth_w = |n: i32| w * n / 8;
    let (half_w, half_h) = (w / 2, h / 2);
    let code = c as u32;
    match code {
        0x2580 => canvas.rect(0, 0, w, half_h, 255), // ▀
        // ▁▂▃▄▅▆▇: lower n eighths.
        0x2581..=0x2587 => {
            let n = (code - 0x2580) as i32;
            canvas.rect(0, h - eighth_h(n), w, h, 255);
        }
        0x2588 => canvas.rect(0, 0, w, h, 255), // █
        // ▉▊▋▌▍▎▏: left 7..1 eighths.
        0x2589..=0x258F => {
            let n = 8 - (code - 0x2588) as i32;
            canvas.rect(0, 0, eighth_w(n), h, 255);
        }
        0x2590 => canvas.rect(half_w, 0, w, h, 255), // ▐
        0x2591 => canvas.rect(0, 0, w, h, 64),       // ░
        0x2592 => canvas.rect(0, 0, w, h, 128),      // ▒
        0x2593 => canvas.rect(0, 0, w, h, 191),      // ▓
        0x2594 => canvas.rect(0, 0, w, eighth_h(1), 255), // ▔
        0x2595 => canvas.rect(w - eighth_w(1), 0, w, h, 255), // ▕
        // Quadrants ▖▗▘▙▚▛▜▝▞▟ as bits: upper-left, upper-right, lower-left, lower-right.
        0x2596..=0x259F => {
            const QUADRANTS: [u8; 10] = [
                0b0010, 0b0001, 0b1000, 0b1011, 0b1001, 0b1110, 0b1101, 0b0100, 0b0110, 0b0111,
            ];
            let bits = QUADRANTS[(code - 0x2596) as usize];
            if bits & 0b1000 != 0 {
                canvas.rect(0, 0, half_w, half_h, 255);
            }
            if bits & 0b0100 != 0 {
                canvas.rect(half_w, 0, w, half_h, 255);
            }
            if bits & 0b0010 != 0 {
                canvas.rect(0, half_h, half_w, h, 255);
            }
            if bits & 0b0001 != 0 {
                canvas.rect(half_w, half_h, w, h, 255);
            }
        }
        _ => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 10;
    const H: u32 = 20;

    fn render(c: char) -> Vec<u8> {
        rasterize(c, W, H, 2).unwrap_or_else(|| panic!("{c} not drawn"))
    }

    fn at(mask: &[u8], x: u32, y: u32) -> u8 {
        mask[(y * W + x) as usize]
    }

    #[test]
    fn lines_reach_the_edges() {
        let h = render('─');
        assert_eq!(at(&h, 0, H / 2), 255);
        assert_eq!(at(&h, W - 1, H / 2), 255);
        assert_eq!(at(&h, W / 2, 0), 0);

        let v = render('│');
        assert_eq!(at(&v, W / 2, 0), 255);
        assert_eq!(at(&v, W / 2, H - 1), 255);
    }

    #[test]
    fn corner_has_no_gap() {
        let m = render('┌');
        // Right arm and down arm are both present and meet.
        assert_eq!(at(&m, W - 1, H / 2), 255);
        assert_eq!(at(&m, W / 2, H - 1), 255);
        assert_eq!(at(&m, W / 2, H / 2), 255);
        // Nothing towards up or left.
        assert_eq!(at(&m, W / 2, 0), 0);
        assert_eq!(at(&m, 0, H / 2), 0);
    }

    #[test]
    fn heavy_is_thicker_than_light() {
        let count = |m: &[u8]| (0..H).filter(|&y| at(m, W / 2, y) == 255).count();
        assert!(count(&render('━')) > count(&render('─')));
    }

    #[test]
    fn every_line_character_is_covered() {
        for code in (0x2500..=0x254F).chain(0x2574..=0x257F) {
            let c = char::from_u32(code).unwrap();
            let mask = render(c);
            assert!(mask.iter().any(|&p| p > 0), "{c} is empty");
        }
    }

    #[test]
    fn blocks_and_shades() {
        assert!(render('█').iter().all(|&p| p == 255));
        assert!(render('▒').iter().all(|&p| p == 128));
        let upper = render('▀');
        assert_eq!(at(&upper, 0, 0), 255);
        assert_eq!(at(&upper, 0, H - 1), 0);
        let quadrant = render('▗');
        assert_eq!(at(&quadrant, W - 1, H - 1), 255);
        assert_eq!(at(&quadrant, 0, 0), 0);
        for code in 0x2580..=0x259F {
            let c = char::from_u32(code).unwrap();
            assert!(render(c).iter().any(|&p| p > 0), "{c} is empty");
        }
    }

    #[test]
    fn rounded_and_diagonal() {
        let arc = render('╭');
        assert!(at(&arc, W - 1, H / 2) > 0, "right arm");
        assert!(at(&arc, W / 2, H - 1) > 0, "down arm");
        assert_eq!(at(&arc, 0, 0), 0);

        let diag = render('╲');
        assert!(at(&diag, 0, 0) > 0);
        assert!(at(&diag, W - 1, H - 1) > 0);
        assert_eq!(at(&diag, W - 1, 0), 0);
    }

    #[test]
    fn rounded_corners_line_up_with_straight_lines() {
        // Odd stroke in an even cell: straight lines sit left of the middle.
        let mask = |c| rasterize(c, W, H, 1).unwrap();
        let column = |m: &[u8], y| (0..W).filter(|&x| at(m, x, y) == 255).collect::<Vec<_>>();
        let row = |m: &[u8], x| (0..H).filter(|&y| at(m, x, y) == 255).collect::<Vec<_>>();
        let (vertical, horizontal) = (mask('│'), mask('─'));
        let arc = mask('╭');
        assert_eq!(column(&arc, H - 1), column(&vertical, H - 1), "down arm");
        assert_eq!(row(&arc, W - 1), row(&horizontal, W - 1), "right arm");
    }

    #[test]
    fn double_lines_are_left_to_the_font() {
        assert_eq!(rasterize('═', W, H, 2), None);
        assert_eq!(rasterize('a', W, H, 2), None);
    }
}
