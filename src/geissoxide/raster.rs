//! Pixel plotting shared by the waveforms and effects: 8-bit index buffer, "max" blending.

/// An 8-bit indexed frame buffer.
pub struct Canvas<'a> {
    pub buf: &'a mut [u8],
    pub width: usize,
    pub height: usize,
}

impl Canvas<'_> {
    /// Brightens `(x, y)` to at least `c`; out-of-range coordinates are ignored.
    pub fn plot_max(&mut self, x: i32, y: i32, c: u8) {
        if let Some(p) = self.get_mut(x, y) {
            *p = (*p).max(c);
        }
    }

    /// Multiplies `(x, y)` by `factor` (used to dim the warp centre).
    pub fn dim(&mut self, x: i32, y: i32, factor: f32) {
        if let Some(p) = self.get_mut(x, y)
            && *p > 1
        {
            *p = (f32::from(*p) * factor) as u8;
        }
    }
}

impl Canvas<'_> {
    /// Mutable access to `(x, y)` when it is inside the buffer.
    pub fn get_mut(&mut self, x: i32, y: i32) -> Option<&mut u8> {
        (x >= 0 && (x as usize) < self.width && y >= 0 && (y as usize) < self.height)
            .then(|| &mut self.buf[y as usize * self.width + x as usize])
    }

    /// Sets `(x, y)` to `c`.
    pub fn set(&mut self, x: i32, y: i32, c: u8) {
        if let Some(p) = self.get_mut(x, y) {
            *p = c;
        }
    }

    /// Adds `v` to `(x, y)` unless the pixel is already at or above `limit`.
    pub fn add(&mut self, x: i32, y: i32, v: u8, limit: u8) {
        if let Some(p) = self.get_mut(x, y)
            && *p < limit
        {
            *p = p.saturating_add(v);
        }
    }
}

impl Canvas<'_> {
    /// Brightens the pixels of the segment `a`→`b` to at least `c` (Bresenham).
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), c: u8) {
        let (mut x0, mut y0) = (a.0 as i32, a.1 as i32);
        let (x1, y1) = (b.0 as i32, b.1 as i32);
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = ((x1 - x0).signum(), (y1 - y0).signum());
        let mut err = dx + dy;
        loop {
            self.plot_max(x0, y0, c);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    /// Fills the triangle `p` (scanline) with at least `c`.
    pub fn fill_triangle(&mut self, mut p: [(f32, f32); 3], c: u8) {
        p.sort_by(|a, b| a.1.total_cmp(&b.1));
        let [(x0, y0), (x1, y1), (x2, y2)] = p;
        let x_at = |ya: f32, xa: f32, yb: f32, xb: f32, y: f32| {
            if (yb - ya).abs() < 1e-3 {
                xa
            } else {
                xa + (xb - xa) * (y - ya) / (yb - ya)
            }
        };
        for y in (y0.ceil() as i32)..=(y2.floor() as i32) {
            let yf = y as f32;
            let xa = x_at(y0, x0, y2, x2, yf);
            let xb = if yf < y1 {
                x_at(y0, x0, y1, x1, yf)
            } else {
                x_at(y1, x1, y2, x2, yf)
            };
            for x in (xa.min(xb).ceil() as i32)..=(xa.max(xb).floor() as i32) {
                self.plot_max(x, y, c);
            }
        }
    }

    /// Filled disc of radius `r` around `p`.
    pub fn disc(&mut self, p: (f32, f32), r: f32, c: u8) {
        let ri = r.ceil() as i32;
        for dy in -ri..=ri {
            for dx in -ri..=ri {
                if ((dx * dx + dy * dy) as f32) <= r * r {
                    self.plot_max(p.0 as i32 + dx, p.1 as i32 + dy, c);
                }
            }
        }
    }

    /// Circle outline of radius `r` around `p`.
    pub fn circle(&mut self, p: (f32, f32), r: f32, c: u8) {
        let steps = (r * std::f32::consts::TAU).ceil().max(8.0) as usize;
        for i in 0..steps {
            let a = i as f32 / steps as f32 * std::f32::consts::TAU;
            self.plot_max((p.0 + r * a.cos()) as i32, (p.1 + r * a.sin()) as i32, c);
        }
    }
}
