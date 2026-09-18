//! The Chladni engine: the spectrum drives the standing-wave modes of a vibrating plate and a
//! bed of sand grains drifts toward the nodal lines, drawing Chladni figures that re-form as the
//! music changes. A beat knocks the plate and scatters the sand. Original design, no source port.

use rand::RngExt;

use crate::engine::{Clock, CpuEngine};
use crate::geissoxide::palette::{self, Fade};
use crate::geissoxide::raster::Canvas;
use crate::milkdrop::audio::{Audio, FFT_SIZE, log_bands};

/// Spectrum bands, each driving one plate mode.
const BANDS: usize = 24;
/// Sand grains.
const GRAINS: usize = 40_000;
/// Fraction of the trail buffer kept per frame.
const DECAY: f32 = 0.9;
/// Grain velocity kept per frame.
const DRAG: f32 = 0.8;
/// Longest grain step per frame, in pixels (keeps grains from tunnelling through nodal lines).
const MAX_STEP: f32 = 3.0;
/// Random shake added per frame, in pixels, scaled by the local vibration.
const JITTER: f32 = 1.5;
/// Sum of band levels below which the plate is considered silent.
/// ponytail: fixed threshold in MilkDrop spectrum units; raise it if a noisy mic keeps the sand shaking.
const SILENCE: f32 = 2.0;
/// Brightness a grain adds to the pixel it sits on.
const GRAIN: u8 = 60;
/// Palette index of the antinodes' glow at full displacement.
const GLOW: f32 = 40.0;

struct Grain {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

/// A running Chladni visualizer.
pub struct Chladni {
    width: usize,
    height: usize,
    audio: Audio,
    /// FFT bin range of each band.
    bands: [(usize, usize); BANDS],
    /// Per band, `cos(m·π·x/W)` for every column and `cos(n·π·y/H)` for every row.
    cx: Vec<Vec<f32>>,
    cy: Vec<Vec<f32>>,
    /// Long-term average energy of each band.
    avg: [f32; BANDS],
    /// Smoothed, normalised mode amplitudes (sum ≤ 1).
    amp: [f32; BANDS],
    /// Plate displacement per pixel, `-1..=1`.
    field: Vec<f32>,
    grains: Vec<Grain>,
    trail: Vec<u8>,
    palette: Fade,
    rng: rand::rngs::ThreadRng,
    clock: Clock,
    rgba: Vec<u8>,
}

impl Chladni {
    /// Creates an engine rendering at `width`×`height` for audio at `sample_rate`, changing the
    /// mode assignment and palette every `duration` seconds.
    pub fn new(width: usize, height: usize, sample_rate: u32, duration: f32) -> Self {
        let mut rng = rand::rng();
        let bands = log_bands::<BANDS>();
        let palette = palette::random(&mut rng, false);
        let grains = (0..GRAINS)
            .map(|_| Grain {
                x: rng.random_range(0.0..width as f32),
                y: rng.random_range(0.0..height as f32),
                vx: 0.0,
                vy: 0.0,
            })
            .collect();
        let mut engine = Self {
            width,
            height,
            audio: Audio::new(sample_rate),
            bands,
            cx: Vec::new(),
            cy: Vec::new(),
            avg: [1.0; BANDS],
            amp: [0.0; BANDS],
            field: vec![0.0; width * height],
            grains,
            trail: vec![0; width * height],
            palette: Fade::new(palette),
            rng,
            clock: Clock::new(duration),
            rgba: vec![255; width * height * 4],
        };
        engine.pick_modes();
        engine
    }
}

impl CpuEngine for Chladni {
    fn frames_needed(&self) -> usize {
        FFT_SIZE
    }

    fn step(&mut self, pcm: &[f32]) -> &[u8] {
        if self.clock.tick() {
            self.next();
        }

        self.audio.update(pcm, self.clock.fps, self.clock.frame);
        self.update_amplitudes();
        self.compute_field();
        if self.clock.beat(&self.audio) {
            let kick = (2.0 * (self.audio.level[0] - self.audio.att[0])).min(8.0);
            self.kick(kick);
        }
        self.move_grains();
        self.draw();
        &self.rgba
    }

    /// New mode assignment and palette, with a knock on the plate.
    fn next(&mut self) {
        self.clock.reset_switch();
        self.pick_modes();
        self.palette.to(palette::random(&mut self.rng, false));
        self.kick(6.0);
    }
}

impl Chladni {
    /// Assigns a random `(m, n)` mode to each band, low bands getting the coarse modes, and
    /// rebuilds the separable cosine tables.
    fn pick_modes(&mut self) {
        let (w, h) = (self.width as f32, self.height as f32);
        self.cx.clear();
        self.cy.clear();
        for k in 0..BANDS {
            let order = 2 + k * 9 / (BANDS - 1); // m + n, from 2 (bass) to 11 (treble)
            let m = self.rng.random_range(1..order);
            let n = order - m;
            let table = |len: usize, size: f32, mode: usize| -> Vec<f32> {
                (0..len)
                    .map(|i| (mode as f32 * std::f32::consts::PI * i as f32 / size).cos())
                    .collect()
            };
            self.cx.push(table(self.width, w, m));
            self.cy.push(table(self.height, h, n));
        }
    }

    /// Band energy relative to its own long-term average, squared for contrast, smoothed with a
    /// fast attack and slow release, then normalised so the field stays within `-1..=1`.
    fn update_amplitudes(&mut self) {
        let rate = |r: f32| self.clock.rate(r);
        let spectrum = |i: usize| 0.5 * (self.audio.freq[0][i] + self.audio.freq[1][i]);
        let mut total = 0.0;
        let mut targets = [0.0; BANDS];
        for (k, &(lo, hi)) in self.bands.iter().enumerate() {
            let imm: f32 = (lo..hi).map(spectrum).sum::<f32>() / (hi - lo) as f32;
            total += imm;
            let r = rate(0.995);
            self.avg[k] = self.avg[k] * r + imm * (1.0 - r);
            let level = (imm / self.avg[k].max(1e-3)).clamp(0.0, 4.0);
            targets[k] = level * level;
        }
        let silent = total < SILENCE;
        for (amp, target) in self.amp.iter_mut().zip(targets) {
            let target = if silent { 0.0 } else { target };
            let r = rate(if target > *amp { 0.5 } else { 0.9 });
            *amp = *amp * r + target * (1.0 - r);
        }
        let sum: f32 = self.amp.iter().sum();
        if sum > 1.0 {
            self.amp.iter_mut().for_each(|a| *a /= sum);
        }
    }

    /// `field = Σ_k amp_k · cos(m_k·π·x/W) · cos(n_k·π·y/H)`, one separable pass per band.
    fn compute_field(&mut self) {
        self.field.fill(0.0);
        for (y, row) in self.field.chunks_exact_mut(self.width).enumerate() {
            for k in 0..BANDS {
                let a = self.amp[k] * self.cy[k][y];
                if a.abs() < 1e-4 {
                    continue;
                }
                for (f, c) in row.iter_mut().zip(&self.cx[k]) {
                    *f += a * c;
                }
            }
        }
    }

    /// Displacement at integer pixel `(x, y)`, clamped to the plate.
    fn at(&self, x: f32, y: f32) -> f32 {
        let xi = (x.max(0.0) as usize).min(self.width - 1);
        let yi = (y.max(0.0) as usize).min(self.height - 1);
        self.field[yi * self.width + xi]
    }

    /// Grains slide down the gradient of `field²` (toward the nodal lines) and shake where the
    /// plate vibrates, bouncing off its edges.
    fn move_grains(&mut self) {
        let (w, h) = (self.width as f32, self.height as f32);
        // Gradient gain: `field²` changes by ~m·π/W per pixel, so this yields a few px/frame.
        let gain = 0.08 * w;
        let mut grains = std::mem::take(&mut self.grains);
        for g in &mut grains {
            let u = self.at(g.x, g.y);
            let gx = u * (self.at(g.x + 2.0, g.y) - self.at(g.x - 2.0, g.y)) * 0.5;
            let gy = u * (self.at(g.x, g.y + 2.0) - self.at(g.x, g.y - 2.0)) * 0.5;
            let shake = u.abs() * JITTER;
            g.vx = g.vx * DRAG - gx * gain + self.rng.random_range(-shake..=shake);
            g.vy = g.vy * DRAG - gy * gain + self.rng.random_range(-shake..=shake);
            let speed = g.vx.hypot(g.vy);
            if speed > MAX_STEP {
                g.vx *= MAX_STEP / speed;
                g.vy *= MAX_STEP / speed;
            }
            g.x += g.vx;
            g.y += g.vy;
            if g.x < 0.0 || g.x >= w {
                g.vx = -g.vx;
                g.x = g.x.clamp(0.0, w - 1.0);
            }
            if g.y < 0.0 || g.y >= h {
                g.vy = -g.vy;
                g.y = g.y.clamp(0.0, h - 1.0);
            }
        }
        self.grains = grains;
    }

    /// Random impulse of up to `strength` px/frame on every grain.
    fn kick(&mut self, strength: f32) {
        for g in &mut self.grains {
            let angle = self.rng.random_range(0.0..std::f32::consts::TAU);
            let s = self.rng.random_range(0.0..strength);
            g.vx += s * angle.cos();
            g.vy += s * angle.sin();
        }
    }

    /// Fades the trail, glows the antinodes faintly, splats the grains and maps the palette.
    fn draw(&mut self) {
        for (t, f) in self.trail.iter_mut().zip(&self.field) {
            let faded = (f32::from(*t) * DECAY) as u8;
            *t = faded.max((f.abs() * GLOW) as u8);
        }
        let mut canvas = Canvas {
            buf: &mut self.trail,
            width: self.width,
            height: self.height,
        };
        for g in &self.grains {
            canvas.add(g.x as i32, g.y as i32, GRAIN, 255);
        }
        palette::apply(self.palette.tick(), &self.trail, &mut self.rgba);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sand_settles_where_the_plate_is_still() {
        let tone: Vec<f32> = (0..FFT_SIZE)
            .flat_map(|i| [0.5 * (i as f32 * 220.0 * std::f32::consts::TAU / 44_100.0).sin(); 2])
            .collect();
        let mut engine = Chladni::new(160, 90, 44_100, 1e9);
        let vibration = |e: &Chladni| {
            e.grains.iter().map(|g| e.at(g.x, g.y).powi(2)).sum::<f32>() / GRAINS as f32
        };
        for _ in 0..30 {
            engine.step(&tone);
        }
        let early = vibration(&engine);
        for _ in 0..300 {
            engine.step(&tone);
        }
        let settled = vibration(&engine);
        assert!(early > 0.0, "the plate must vibrate under a tone");
        assert!(
            settled < 0.5 * early,
            "grains should gather on nodal lines: early {early} settled {settled}"
        );
    }
}
