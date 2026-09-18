//! The Ferrofluid engine: a Swift–Hohenberg pattern-forming field,
//! `∂u/∂t = ε·u − (q² + ∇²)²·u + g·u² − u³`, whose control parameter `ε` follows the bass so
//! hexagonal spikes surge out of a flat pool on every kick and sink back in silence, like a
//! ferrofluid under a magnet. The field is lit as black liquid metal: Blinn–Phong specular from
//! an orbiting light plus a palette "sky" reflection. The field runs on a coarse grid where the
//! spike spacing is ~2π cells (so `q ≈ 1` and the flat mode stays damped) and is upsampled for
//! lighting. Original design, no source port.

use std::time::Instant;

use rand::RngExt;

use crate::geissoxide::palette::{self, Palette};
use crate::milkdrop::audio::{Audio, FFT_SIZE, SAMPLES};

/// Screen pixels per simulation cell.
const SCALE: usize = 5;
/// Explicit Euler step (the biharmonic term needs `dt < 2/49` at unit spacing, and the
/// nonlinear terms a margin) and substeps per frame.
const DT: f32 = 0.02;
const SUBSTEPS: usize = 30;
/// Quadratic coefficient `g`: positive selects up-pointing hexagonal peaks.
const G: f32 = 1.0;
/// Wavenumber (spike spacing = 2π/q cells) at the lowest and highest spectral centroid; kept
/// near 1 so the uniform mode, damped by `q⁴`, never outgrows the pattern.
const Q_COARSE: f32 = 0.85;
const Q_FINE: f32 = 1.2;
/// Waveform RMS (±128 scale) below which the pool is silent and flattens.
/// ponytail: absolute threshold; raise it for a noisy mic.
const SILENCE: f32 = 0.5;
/// Height exaggeration for the lighting normals (gradient per cell).
const RELIEF: f32 = 1.5;
/// Beat when the bass jumps this much over its smoothed level; seconds between kicks.
const BEAT: f32 = 1.4;
const BEAT_HOLD: f32 = 0.25;

/// A running Ferrofluid visualizer.
pub struct Ferrofluid {
    width: usize,
    height: usize,
    audio: Audio,
    /// Simulation grid size.
    sw: usize,
    sh: usize,
    /// Field, its Laplacian, the intermediate `q²u + ∇²u` and the gradient for lighting.
    u: Vec<f32>,
    lap: Vec<f32>,
    v: Vec<f32>,
    grad: Vec<(f32, f32)>,
    /// Smoothed bass level, spectral centroid (bin) and beat boost of `ε`.
    bass: f32,
    centroid: f32,
    boost: f32,
    /// Random wavenumber multiplier, changed by `next`.
    q_scale: f32,
    /// Light azimuth.
    theta: f32,
    palette: Palette,
    palette_from: Palette,
    palette_to: Palette,
    blends_left: u32,
    rng: rand::rngs::ThreadRng,
    frame: u64,
    fps: f32,
    last_frame: Instant,
    since_beat: f32,
    since_switch: f32,
    duration: f32,
    rgba: Vec<u8>,
}

impl Ferrofluid {
    /// Creates an engine rendering at `width`×`height` for audio at `sample_rate`, changing the
    /// palette and spike spacing every `duration` seconds.
    pub fn new(width: usize, height: usize, sample_rate: u32, duration: f32) -> Self {
        let mut rng = rand::rng();
        let palette = palette::random(&mut rng, false);
        let (sw, sh) = (width.div_ceil(SCALE).max(3), height.div_ceil(SCALE).max(3));
        let u = (0..sw * sh)
            .map(|_| rng.random_range(-0.01..0.01))
            .collect();
        Self {
            width,
            height,
            audio: Audio::new(sample_rate),
            sw,
            sh,
            u,
            lap: vec![0.0; sw * sh],
            v: vec![0.0; sw * sh],
            grad: vec![(0.0, 0.0); sw * sh],
            bass: 1.0,
            centroid: 60.0,
            boost: 0.0,
            q_scale: 1.0,
            theta: 0.0,
            palette,
            palette_from: palette,
            palette_to: palette,
            blends_left: 0,
            rng,
            frame: 0,
            fps: 60.0,
            last_frame: Instant::now(),
            since_beat: 0.0,
            since_switch: 0.0,
            duration,
            rgba: vec![255; width * height * 4],
        }
    }

    /// Stereo frames `step` wants per call.
    pub fn frames_needed(&self) -> usize {
        FFT_SIZE
    }

    /// Renders one frame from the latest interleaved stereo `pcm`; returns RGBA8 pixels.
    pub fn step(&mut self, pcm: &[f32]) -> &[u8] {
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_frame)
            .as_secs_f32()
            .clamp(1.0 / 240.0, 0.5);
        self.last_frame = now;
        self.fps = self.fps * 0.95 + (1.0 / dt) * 0.05;
        self.frame += 1;
        self.since_beat += dt;
        self.since_switch += dt;
        if self.since_switch >= self.duration {
            self.next();
        }
        self.audio.update(pcm, self.fps, self.frame);

        let fps = self.fps.clamp(15.0, 144.0);
        let rate = |r: f32| r.powf(30.0 / fps);
        let level = self.audio.level[0].min(4.0);
        let r = rate(if level > self.bass { 0.3 } else { 0.9 });
        self.bass = self.bass * r + level * (1.0 - r);
        let (mut num, mut den) = (0.0, 0.0);
        for i in 1..SAMPLES {
            let e = self.audio.freq[0][i] + self.audio.freq[1][i];
            num += i as f32 * e;
            den += e;
        }
        if den > 0.0 {
            let r = rate(0.95);
            self.centroid = self.centroid * r + (num / den) * (1.0 - r);
        }
        let rms = (self.audio.time[0].iter().map(|s| s * s).sum::<f32>() / SAMPLES as f32).sqrt();
        let silent = rms < SILENCE;
        let beat = self.since_beat >= BEAT_HOLD && self.audio.level[0] > BEAT * self.audio.att[0];
        if beat && !silent {
            self.since_beat = 0.0;
            self.boost = 0.6;
            for x in &mut self.u {
                *x += self.rng.random_range(-0.15..0.15);
            }
        }
        self.boost *= rate(0.9);
        let eps = if silent {
            -0.6
        } else {
            (0.8 * (self.bass - 0.75) + self.boost).clamp(-0.6, 0.6)
        };
        let t = ((self.centroid - 120.0) / 200.0).clamp(0.0, 1.0);
        let q2 = ((Q_COARSE + (Q_FINE - Q_COARSE) * t) * self.q_scale).powi(2);
        for _ in 0..SUBSTEPS {
            self.substep(eps, q2);
        }
        self.theta += 0.004 * 60.0 / fps;
        self.draw();
        &self.rgba
    }

    /// New palette and spike spacing right away (key binding).
    pub fn next(&mut self) {
        self.since_switch = 0.0;
        self.palette_from = self.palette;
        self.palette_to = palette::random(&mut self.rng, false);
        self.blends_left = palette::BLEND_FRAMES;
        self.q_scale = self.rng.random_range(0.85..1.15);
    }

    /// Five-point periodic Laplacian of `src` into `dst`.
    fn laplacian(&self, src: &[f32], dst: &mut [f32]) {
        let (w, h) = (self.sw, self.sh);
        for y in 0..h {
            let (up, down) = ((y + h - 1) % h * w, (y + 1) % h * w);
            let row = y * w;
            for x in 0..w {
                let (left, right) = ((x + w - 1) % w, (x + 1) % w);
                dst[row + x] = src[up + x] + src[down + x] + src[row + left] + src[row + right]
                    - 4.0 * src[row + x];
            }
        }
    }

    /// One explicit Euler step of Swift–Hohenberg with control `eps` and wavenumber² `q2`; the
    /// mean is removed afterwards (the fluid keeps its volume).
    fn substep(&mut self, eps: f32, q2: f32) {
        let mut lap = std::mem::take(&mut self.lap);
        let mut v = std::mem::take(&mut self.v);
        self.laplacian(&self.u, &mut lap);
        for ((v, &u), &l) in v.iter_mut().zip(&self.u).zip(&lap) {
            *v = q2 * u + l;
        }
        self.laplacian(&v, &mut lap);
        for ((u, &v), &l) in self.u.iter_mut().zip(&v).zip(&lap) {
            let biharmonic = q2 * v + l;
            *u += DT * (eps * *u - biharmonic + G * *u * *u - *u * *u * *u);
        }
        let mean = self.u.iter().sum::<f32>() / self.u.len() as f32;
        self.u.iter_mut().for_each(|u| *u -= mean);
        self.lap = lap;
        self.v = v;
    }

    /// Central-difference gradient of the field on the simulation grid.
    fn gradient(&mut self) {
        let (w, h) = (self.sw, self.sh);
        for y in 0..h {
            let (up, down) = ((y + h - 1) % h * w, (y + 1) % h * w);
            for x in 0..w {
                let (left, right) = ((x + w - 1) % w, (x + 1) % w);
                self.grad[y * w + x] = (
                    (self.u[y * w + right] - self.u[y * w + left]) * 0.5,
                    (self.u[down + x] - self.u[up + x]) * 0.5,
                );
            }
        }
    }

    /// Bilinear sample of the gradient at screen pixel `(x, y)`.
    fn grad_at(&self, x: usize, y: usize) -> (f32, f32) {
        let (w, h) = (self.sw, self.sh);
        let fx = x as f32 / SCALE as f32;
        let fy = y as f32 / SCALE as f32;
        let (x0, y0) = (fx as usize % w, fy as usize % h);
        let (x1, y1) = ((x0 + 1) % w, (y0 + 1) % h);
        let (tx, ty) = (fx.fract(), fy.fract());
        let lerp =
            |a: (f32, f32), b: (f32, f32), t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let top = lerp(self.grad[y0 * w + x0], self.grad[y0 * w + x1], tx);
        let bottom = lerp(self.grad[y1 * w + x0], self.grad[y1 * w + x1], tx);
        lerp(top, bottom, ty)
    }

    /// Lights the height field as black liquid metal: dark diffuse, sharp white specular from
    /// the orbiting light, palette sky reflected more at grazing angles.
    fn draw(&mut self) {
        let (w, h) = (self.width, self.height);
        let light = {
            let (x, y) = (0.6 * self.theta.cos(), 0.6 * self.theta.sin());
            normalize([x, y, 0.7])
        };
        let half = normalize([light[0], light[1], light[2] + 1.0]);
        if self.blends_left > 0 {
            self.blends_left -= 1;
            let t = 1.0 - self.blends_left as f32 / palette::BLEND_FRAMES as f32;
            self.palette = palette::blend(&self.palette_from, &self.palette_to, t);
        }
        self.gradient();
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = self.grad_at(x, y);
                let n = normalize([-RELIEF * dx, -RELIEF * dy, 1.0]);
                let diffuse = dot(n, light).max(0.0);
                let mut spec = dot(n, half).max(0.0);
                for _ in 0..6 {
                    spec *= spec; // ^64
                }
                let fresnel = (1.0 - n[2]).powi(3);
                let sky = self.palette[((n[1] * 0.5 + 0.5) * 255.0) as usize];
                let px = &mut self.rgba[(y * w + x) * 4..][..3];
                for c in 0..3 {
                    let s = f32::from(sky[c]) / 255.0;
                    let v = 0.04 * diffuse + s * (0.12 + 0.6 * fresnel) + spec;
                    px[c] = (v.min(1.0) * 255.0) as u8;
                }
            }
        }
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let len = dot(a, a).sqrt();
    a.map(|c| c / len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variance(e: &Ferrofluid) -> f32 {
        e.u.iter().map(|x| x * x).sum::<f32>() / e.u.len() as f32
    }

    #[test]
    fn spikes_grow_when_driven_and_flatten_when_not() {
        let mut e = Ferrofluid::new(256, 256, 44_100, 1e9);
        let q2 = 1.0;
        let start = variance(&e);
        for _ in 0..600 {
            e.substep(0.5, q2);
        }
        let grown = variance(&e);
        assert!(
            grown > 10.0 * start && grown.is_finite(),
            "{start} -> {grown}"
        );
        for _ in 0..600 {
            e.substep(-0.5, q2);
        }
        let flat = variance(&e);
        assert!(flat < 0.1 * grown, "{grown} -> {flat}");
    }
}
