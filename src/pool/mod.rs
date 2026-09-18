//! The Pool engine: the music falls as rain on a pool. Every onset in one of 24 spectrum bands is
//! a drop, placed by stereo pan (x) and pitch (y, bass at the bottom), and the beat is a stone in
//! the middle. The pool floor is lit through the surface: each surface pixel refracts one ray of
//! overhead light onto the floor, and where the rays bunch up the caustics dance. Original
//! design, no source port.

use rand::RngExt;

use crate::engine::{Clock, CpuEngine};
use crate::geissoxide::palette::{self, Fade};
use crate::milkdrop::audio::{Audio, FFT_SIZE, log_bands};

/// Spectrum bands, each a possible rain drop.
const BANDS: usize = 24;
/// Wave speed² on the grid (stable below 0.5) and per-step damping.
const C2: f32 = 0.3;
const DAMPING: f32 = 0.993;
/// A band drops when it jumps this much over its long-term average, at most every `COOLDOWN` s.
const ONSET: f32 = 2.0;
const COOLDOWN: f32 = 0.15;
/// Floor brightness of undisturbed water (palette index); caustics go up to 255.
const AMBIENT: f32 = 60.0;
/// Sum of band levels below which no drop falls (a noisy mic would otherwise rain forever).
/// ponytail: absolute threshold in MilkDrop spectrum units.
const SILENCE: f32 = 2.0;

/// A running Pool visualizer.
pub struct Pool {
    width: usize,
    height: usize,
    audio: Audio,
    bands: [(usize, usize); BANDS],
    avg: [f32; BANDS],
    since_drop: [f32; BANDS],
    /// Surface height, previous height (leap-frog), floor light accumulator and its blur.
    h: Vec<f32>,
    h_prev: Vec<f32>,
    floor: Vec<f32>,
    blur: Vec<f32>,
    /// Ray displacement per unit slope: depth × (1 − 1/n) for water.
    refraction: f32,
    palette: Fade,
    rng: rand::rngs::ThreadRng,
    clock: Clock,
    rgba: Vec<u8>,
}

impl Pool {
    /// Creates an engine rendering at `width`×`height` for audio at `sample_rate`, changing the
    /// palette and depth every `duration` seconds.
    pub fn new(width: usize, height: usize, sample_rate: u32, duration: f32) -> Self {
        let mut rng = rand::rng();
        let palette = palette::random_tinted(&mut rng, false);
        Self {
            width,
            height,
            audio: Audio::new(sample_rate),
            bands: log_bands::<BANDS>(),
            avg: [1.0; BANDS],
            since_drop: [0.0; BANDS],
            h: vec![0.0; width * height],
            h_prev: vec![0.0; width * height],
            floor: vec![0.0; width * height],
            blur: vec![0.0; width * height],
            refraction: 16.0,
            palette: Fade::new(palette),
            rng,
            clock: Clock::new(duration),
            rgba: vec![255; width * height * 4],
        }
    }
}

impl CpuEngine for Pool {
    fn frames_needed(&self) -> usize {
        FFT_SIZE
    }

    fn step(&mut self, pcm: &[f32]) -> &[u8] {
        if self.clock.tick() {
            self.next();
        }
        self.audio.update(pcm, self.clock.fps, self.clock.frame);
        self.rain();
        if self.clock.beat(&self.audio) {
            let (x, y) = (self.width as f32 * 0.5, self.height as f32 * 0.5);
            self.drop(
                x,
                y,
                14.0,
                1.5 * (self.audio.level[0] - self.audio.att[0]).min(3.0),
            );
        }
        self.propagate();
        self.draw();
        &self.rgba
    }

    /// New palette and depth.
    fn next(&mut self) {
        self.clock.reset_switch();
        self.palette
            .to(palette::random_tinted(&mut self.rng, false));
        self.refraction = self.rng.random_range(10.0..22.0);
    }
}

impl Pool {
    /// One drop per band whose energy jumps: x from the stereo pan of that band, y from its
    /// pitch, size from its depth in the spectrum, weight from the jump.
    fn rain(&mut self) {
        let r = self.clock.rate(0.995);
        let mut total = 0.0;
        let mut drops = Vec::new();
        for (k, &(lo, hi)) in self.bands.iter().enumerate() {
            let left: f32 = self.audio.freq[0][lo..hi].iter().sum();
            let right: f32 = self.audio.freq[1][lo..hi].iter().sum();
            let imm = 0.5 * (left + right) / (hi - lo) as f32;
            total += imm;
            let level = imm / self.avg[k].max(1e-3);
            self.avg[k] = self.avg[k] * r + imm * (1.0 - r);
            self.since_drop[k] += self.clock.dt;
            if level > ONSET && self.since_drop[k] >= COOLDOWN {
                self.since_drop[k] = 0.0;
                let pan = if left + right > 0.0 {
                    (right - left) / (right + left)
                } else {
                    0.0
                };
                let depth = 1.0 - k as f32 / (BANDS - 1) as f32; // 1 = bass
                let x = self.width as f32 * (0.5 + 0.4 * pan);
                let y = self.height as f32 * (0.08 + 0.84 * depth);
                drops.push((x, y, 3.0 + 6.0 * depth, 0.4 * level.min(4.0)));
            }
        }
        if total >= SILENCE {
            for (x, y, radius, weight) in drops {
                self.drop(x, y, radius, weight);
            }
        }
    }

    /// Presses a Gaussian dimple of `radius` and depth `weight` into the surface at `(x, y)`.
    fn drop(&mut self, x: f32, y: f32, radius: f32, weight: f32) {
        let (w, h) = (self.width as i32, self.height as i32);
        let reach = (radius * 2.0) as i32;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let (px, py) = (x as i32 + dx, y as i32 + dy);
                if px >= 0 && px < w && py >= 0 && py < h {
                    let d2 = (dx * dx + dy * dy) as f32;
                    self.h[(py * w + px) as usize] -= weight * (-d2 / (radius * radius)).exp();
                }
            }
        }
    }

    /// Leap-frog step of the damped wave equation with clamped (reflecting) edges.
    fn propagate(&mut self) {
        let (w, h) = (self.width, self.height);
        let mut next = std::mem::take(&mut self.h_prev);
        for y in 0..h {
            let (up, down) = (y.saturating_sub(1) * w, (y + 1).min(h - 1) * w);
            for x in 0..w {
                let i = y * w + x;
                let (left, right) = (y * w + x.saturating_sub(1), y * w + (x + 1).min(w - 1));
                let lap = self.h[up + x] + self.h[down + x] + self.h[left] + self.h[right]
                    - 4.0 * self.h[i];
                next[i] = (2.0 * self.h[i] - next[i] + C2 * lap) * DAMPING;
            }
        }
        self.h_prev = std::mem::replace(&mut self.h, next);
    }

    /// Refracts one overhead ray per surface pixel onto the floor, blurs the light and maps it
    /// through the palette.
    fn draw(&mut self) {
        let (w, h) = (self.width, self.height);
        self.floor.fill(0.0);
        for y in 0..h {
            let (up, down) = (y.saturating_sub(1) * w, (y + 1).min(h - 1) * w);
            for x in 0..w {
                let gx = (self.h[y * w + (x + 1).min(w - 1)] - self.h[y * w + x.saturating_sub(1)])
                    * 0.5;
                let gy = (self.h[down + x] - self.h[up + x]) * 0.5;
                let tx = (x as f32 + self.refraction * gx).clamp(0.0, (w - 1) as f32 - 1e-3);
                let ty = (y as f32 + self.refraction * gy).clamp(0.0, (h - 1) as f32 - 1e-3);
                let (x0, y0) = (tx as usize, ty as usize);
                let (fx, fy) = (tx.fract(), ty.fract());
                // Bilinear splat so the caustic lines stay smooth.
                self.floor[y0 * w + x0] += (1.0 - fx) * (1.0 - fy);
                self.floor[y0 * w + x0 + 1] += fx * (1.0 - fy);
                self.floor[(y0 + 1) * w + x0] += (1.0 - fx) * fy;
                self.floor[(y0 + 1) * w + x0 + 1] += fx * fy;
            }
        }
        for y in 0..h {
            let (up, down) = (y.saturating_sub(1) * w, (y + 1).min(h - 1) * w);
            for x in 0..w {
                let (left, right) = (y * w + x.saturating_sub(1), y * w + (x + 1).min(w - 1));
                self.blur[y * w + x] = 0.4 * self.floor[y * w + x]
                    + 0.15
                        * (self.floor[up + x]
                            + self.floor[down + x]
                            + self.floor[left]
                            + self.floor[right]);
            }
        }
        let palette = self.palette.tick();
        for (px, &light) in self.rgba.chunks_exact_mut(4).zip(&self.blur) {
            let i = (light * AMBIENT).min(255.0) as usize;
            px[..3].copy_from_slice(&palette[i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_spreads_as_a_ring_and_dies_out() {
        let mut pool = Pool::new(200, 200, 44_100, 1e9);
        pool.drop(100.0, 100.0, 4.0, 1.0);
        let energy = |p: &Pool| p.h.iter().map(|v| v * v).sum::<f32>();
        let at = |p: &Pool, x: usize| p.h[100 * 200 + x].abs();
        let mut peak = 0.0f32;
        for _ in 0..60 {
            pool.propagate();
            peak = peak.max(energy(&pool));
        }
        assert!(
            at(&pool, 130) > at(&pool, 100),
            "ring should have left the centre"
        );
        for _ in 0..600 {
            pool.propagate();
        }
        let rest = energy(&pool);
        assert!(rest < 0.1 * peak, "waves should die out: {peak} -> {rest}");
        pool.draw();
        assert!(pool.blur.iter().all(|v| v.is_finite()));
    }
}
