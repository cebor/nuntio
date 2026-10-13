//! An 8-bit coverage bitmap, the shape the glyph atlas stores for masks.

/// A coverage mask of `width`×`height` pixels, row by row.
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Mask {
    /// An empty mask.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; (width * height) as usize],
        }
    }

    /// Fill a rectangle given in pixels, clipped to the mask.
    pub fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        // Reversed ranges (tiny cells) are empty.
        let x1 = (x1.max(0) as u32).min(self.width);
        let x0 = (x0.max(0) as u32).min(x1);
        let y1 = (y1.max(0) as u32).min(self.height);
        let y0 = (y0.max(0) as u32).min(y1);
        for y in y0..y1 {
            let row = (y * self.width) as usize;
            self.data[row + x0 as usize..row + x1 as usize].fill(alpha);
        }
    }

    /// Antialiased coverage of a line `2 * half_thickness` thick from a
    /// distance function (in pixels, evaluated at pixel centers). Keeps the
    /// larger of the old and new coverage.
    pub fn shape(&mut self, half_thickness: f32, distance: impl Fn(f32, f32) -> f32) {
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
