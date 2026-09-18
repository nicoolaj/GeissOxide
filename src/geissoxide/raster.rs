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
