//! Underline shapes that a solid quad can't draw: curly, dotted and dashed.
//! Each is rasterized once per cell size into a coverage mask one cell wide,
//! so consecutive cells continue the pattern.

use nuntio_term::UnderlineStyle;

/// A coverage mask for one cell's underline.
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Rasterize `style` for cells `width` pixels wide with lines `stroke`
/// pixels thick. `None` for the styles drawn with solid quads.
pub fn rasterize(style: UnderlineStyle, width: u32, stroke: u32) -> Option<Mask> {
    let stroke = stroke.max(1);
    let (height, data) = match style {
        UnderlineStyle::Single | UnderlineStyle::Double => return None,
        UnderlineStyle::Curly => curly(width, stroke),
        UnderlineStyle::Dotted => {
            // Square dots, one stroke apart.
            let row: Vec<u8> = (0..width)
                .map(|x| {
                    if (x / stroke).is_multiple_of(2) {
                        255
                    } else {
                        0
                    }
                })
                .collect();
            (stroke, row.repeat(stroke as usize))
        }
        UnderlineStyle::Dashed => {
            // One dash per cell, centered, so neighbors leave a gap between.
            let gap = width / 6;
            let row: Vec<u8> = (0..width)
                .map(|x| if x >= gap && x < width - gap { 255 } else { 0 })
                .collect();
            (stroke, row.repeat(stroke as usize))
        }
    };
    Some(Mask {
        width,
        height,
        data,
    })
}

/// One period of a wave per cell, antialiased.
fn curly(width: u32, stroke: u32) -> (u32, Vec<u8>) {
    let height = (stroke * 4).max(4);
    let half = stroke as f32 / 2.0;
    let amplitude = (height as f32 - stroke as f32) / 2.0;
    let period = std::f32::consts::TAU / width.max(1) as f32;
    let mut data = vec![0; (width * height) as usize];
    for x in 0..width {
        let px = x as f32 + 0.5;
        let center = half + amplitude * (1.0 + (px * period).cos());
        // Distance across the line rather than straight down, so the
        // slopes are as thick as the crests.
        let slope = -amplitude * period * (px * period).sin();
        let scale = (1.0 + slope * slope).sqrt();
        for y in 0..height {
            let distance = ((y as f32 + 0.5) - center).abs() / scale;
            let coverage = (half + 0.5 - distance).clamp(0.0, 1.0);
            data[(y * width + x) as usize] = (coverage * 255.0) as u8;
        }
    }
    (height, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column_covered(mask: &Mask, x: u32) -> bool {
        (0..mask.height).any(|y| mask.data[(y * mask.width + x) as usize] > 0)
    }

    #[test]
    fn solid_styles_have_no_mask() {
        assert!(rasterize(UnderlineStyle::Single, 10, 1).is_none());
        assert!(rasterize(UnderlineStyle::Double, 10, 1).is_none());
    }

    #[test]
    fn curly_line_is_continuous_and_fits() {
        let mask = rasterize(UnderlineStyle::Curly, 10, 1).unwrap();
        assert_eq!(mask.data.len(), (mask.width * mask.height) as usize);
        assert_eq!(mask.height, 4);
        assert!((0..10).all(|x| column_covered(&mask, x)));
        // It goes up and down: the top and bottom rows are both used.
        let row = |y: u32| mask.data[(y * 10) as usize..((y + 1) * 10) as usize].to_vec();
        assert!(row(0).iter().any(|&a| a > 0));
        assert!(row(3).iter().any(|&a| a > 0));
    }

    #[test]
    fn dots_and_dashes_leave_gaps() {
        let dotted = rasterize(UnderlineStyle::Dotted, 8, 2).unwrap();
        assert_eq!(dotted.height, 2);
        assert_eq!(&dotted.data[..8], &[255, 255, 0, 0, 255, 255, 0, 0]);

        let dashed = rasterize(UnderlineStyle::Dashed, 12, 1).unwrap();
        assert_eq!(
            &dashed.data[..],
            &[0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0]
        );
    }
}
