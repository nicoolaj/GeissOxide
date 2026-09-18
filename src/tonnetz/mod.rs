//! The Tonnetz engine: the sound is folded into the 12 pitch classes and drawn on Euler's
//! Tonnetz, the hexagonal lattice where a step right is a fifth and a step up-right a major
//! third, so every major triad is an up-pointing triangle and every minor triad a down-pointing
//! one. Lit nodes are the notes heard, filled triangles the chords, the camera follows the key
//! and the chords leave a fading path. Original design, no source port.

use std::sync::Arc;
use std::time::Instant;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::geissoxide::palette::{self, Palette};
use crate::geissoxide::raster::Canvas;
use crate::milkdrop::audio::{Audio, FFT_SIZE};

/// FFT length for pitch detection (10.8 Hz per bin at 44.1 kHz).
const FFT: usize = 4096;
/// Pitch range folded into the chroma.
const LO_HZ: f32 = 80.0;
const HI_HZ: f32 = 5000.0;
/// Peak chroma energy below which the frame counts as silent.
/// ponytail: absolute threshold on 1/bin-weighted FFT magnitudes; raise it for a noisy mic.
const SILENCE: f32 = 0.5;
/// Chroma level from which a pitch class counts as sounding.
const LIT: f32 = 0.45;
/// Lattice spacing as a fraction of the frame height.
const SPACING: f32 = 0.22;
/// Per-frame decay of the note glow (~3 s), of the chord path (~10 s) and of the drawing.
const GLOW_DECAY: f32 = 0.985;
const PATH_DECAY: f32 = 0.995;
const TRAIL_DECAY: f32 = 0.75;
/// Beat when the bass jumps this much over its smoothed level; seconds between kicks.
const BEAT: f32 = 1.4;
const BEAT_HOLD: f32 = 0.25;
/// Camera easing per frame toward the sounding notes.
const EASE: f32 = 0.02;

/// A running Tonnetz visualizer.
pub struct Tonnetz {
    width: usize,
    height: usize,
    audio: Audio,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    scratch: Vec<Complex<f32>>,
    /// `(bin, pitch class, weight)` for every FFT bin in the pitch range.
    bins: Vec<(usize, usize, f32)>,
    /// Smoothed chroma, `0..=1` relative to the loudest class.
    chroma: [f32; 12],
    /// Slowly fading memory of each class, drives the camera.
    glow: [f32; 12],
    /// Camera position in lattice pixels.
    cam: (f32, f32),
    /// Beat ring: centre and radius.
    ring: Option<(f32, f32, f32)>,
    warm: Vec<u8>,
    cool: Vec<u8>,
    path: Vec<u8>,
    pal_warm: Palette,
    pal_cool: Palette,
    from: (Palette, Palette),
    to: (Palette, Palette),
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

impl Tonnetz {
    /// Creates an engine rendering at `width`×`height` for audio at `sample_rate`, changing
    /// palettes every `duration` seconds.
    pub fn new(width: usize, height: usize, sample_rate: u32, duration: f32) -> Self {
        let mut rng = rand::rng();
        let hz_per_bin = sample_rate as f32 / FFT as f32;
        let bins = (1..FFT / 2)
            .filter_map(|bin| {
                let hz = bin as f32 * hz_per_bin;
                (LO_HZ..=HI_HZ).contains(&hz).then(|| {
                    let semitone = (12.0 * (hz / 440.0).log2()).round() as i32 + 69;
                    // 1/bin de-emphasises the harmonics of low notes.
                    (bin, semitone.rem_euclid(12) as usize, 1.0 / bin as f32)
                })
            })
            .collect();
        let window = (0..FFT)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT as f32).cos())
            .collect();
        let pal_warm = warm_palette(&mut rng, true);
        let pal_cool = warm_palette(&mut rng, false);
        Self {
            width,
            height,
            audio: Audio::new(sample_rate),
            fft: FftPlanner::new().plan_fft_forward(FFT),
            window,
            scratch: vec![Complex::default(); FFT],
            bins,
            chroma: [0.0; 12],
            glow: [0.0; 12],
            cam: (0.0, 0.0),
            ring: None,
            warm: vec![0; width * height],
            cool: vec![0; width * height],
            path: vec![0; width * height],
            pal_warm,
            pal_cool,
            from: (pal_warm, pal_cool),
            to: (pal_warm, pal_cool),
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
        FFT
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

        // MilkDrop's analysis only needs the most recent FFT_SIZE frames.
        let recent = &pcm[pcm.len().saturating_sub(FFT_SIZE * 2)..];
        self.audio.update(recent, self.fps, self.frame);
        self.update_chroma(pcm);
        self.move_camera();

        let spacing = SPACING * self.height as f32;
        let zoom = 1.0 + 0.06 * (self.audio.att[0] - 1.0).clamp(-0.5, 1.0);
        let nodes = self.visible_nodes(spacing, zoom);

        let beat = self.since_beat >= BEAT_HOLD && self.audio.level[0] > BEAT * self.audio.att[0];
        if beat {
            self.since_beat = 0.0;
            let root = (0..12).max_by(|&a, &b| self.chroma[a].total_cmp(&self.chroma[b]));
            if let Some(&(x, y, ..)) = root.and_then(|c| nodes.iter().find(|n| n.2 == c)) {
                self.ring = Some((x, y, 0.0));
            }
        }
        self.draw(&nodes, spacing * zoom);
        &self.rgba
    }

    /// New palettes right away (key binding).
    pub fn next(&mut self) {
        self.since_switch = 0.0;
        self.from = (self.pal_warm, self.pal_cool);
        self.to = (
            warm_palette(&mut self.rng, true),
            warm_palette(&mut self.rng, false),
        );
        self.blends_left = palette::BLEND_FRAMES;
    }

    /// Hann-windowed FFT of the mono signal folded into the 12 pitch classes, normalised to the
    /// loudest class, smoothed with a fast attack and slower release.
    fn update_chroma(&mut self, pcm: &[f32]) {
        let start = pcm.len().saturating_sub(FFT * 2);
        for (i, c) in self.scratch.iter_mut().enumerate() {
            let j = start + i * 2;
            let mono = if j + 1 < pcm.len() {
                0.5 * (pcm[j] + pcm[j + 1])
            } else {
                0.0
            };
            *c = Complex::new(mono * self.window[i], 0.0);
        }
        self.fft.process(&mut self.scratch);
        let mut acc = [0.0f32; 12];
        for &(bin, class, weight) in &self.bins {
            acc[class] += self.scratch[bin].norm() * weight;
        }
        let peak = acc.iter().cloned().fold(0.0, f32::max);
        let fps = self.fps.clamp(15.0, 144.0);
        let rate = |r: f32| r.powf(30.0 / fps);
        for (c, &energy) in acc.iter().enumerate() {
            let target = if peak < SILENCE { 0.0 } else { energy / peak };
            let r = rate(if target > self.chroma[c] { 0.4 } else { 0.8 });
            self.chroma[c] = self.chroma[c] * r + target * (1.0 - r);
            self.glow[c] = (self.glow[c] * rate(GLOW_DECAY)).max(self.chroma[c]);
        }
    }

    /// Lattice position of node `(q, r)`, in pixels before zoom and camera.
    fn lattice(q: i32, r: i32, spacing: f32) -> (f32, f32) {
        (
            spacing * (q as f32 + r as f32 * 0.5),
            -spacing * r as f32 * 3f32.sqrt() * 0.5,
        )
    }

    /// Pitch class of node `(q, r)`: right is a fifth, up-right a major third.
    fn class(q: i32, r: i32) -> usize {
        (7 * q + 4 * r).rem_euclid(12) as usize
    }

    /// Eases the camera toward the glow-weighted centroid of the sounding classes, each taken at
    /// its lattice instance nearest the camera (the lattice repeats every 12 nodes).
    fn move_camera(&mut self) {
        let spacing = SPACING * self.height as f32;
        let (cq, cr) = self.cam_cell(spacing);
        let mut nearest = [(f32::MAX, 0.0f32, 0.0f32); 12];
        for r in cr - 8..=cr + 8 {
            for q in cq - 8..=cq + 8 {
                let (x, y) = Self::lattice(q, r, spacing);
                let d = (x - self.cam.0).hypot(y - self.cam.1);
                let slot = &mut nearest[Self::class(q, r)];
                if d < slot.0 {
                    *slot = (d, x, y);
                }
            }
        }
        let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
        for (c, &(_, x, y)) in nearest.iter().enumerate() {
            if self.glow[c] > 0.3 {
                sx += x * self.glow[c];
                sy += y * self.glow[c];
                sw += self.glow[c];
            }
        }
        if sw > 0.0 {
            self.cam.0 += (sx / sw - self.cam.0) * EASE;
            self.cam.1 += (sy / sw - self.cam.1) * EASE;
        }
    }

    /// Lattice cell under the camera.
    fn cam_cell(&self, spacing: f32) -> (i32, i32) {
        let r = -self.cam.1 / (spacing * 3f32.sqrt() * 0.5);
        let q = self.cam.0 / spacing - r * 0.5;
        (q.round() as i32, r.round() as i32)
    }

    /// Screen position and pitch class of every node around the camera, plus its `(q, r)`.
    fn visible_nodes(&self, spacing: f32, zoom: f32) -> Vec<(f32, f32, usize, (i32, i32))> {
        let (cq, cr) = self.cam_cell(spacing);
        let reach = (self.width.max(self.height) as f32 / (spacing * zoom)).ceil() as i32 + 2;
        let (cx, cy) = (self.width as f32 * 0.5, self.height as f32 * 0.5);
        let mut nodes = Vec::new();
        for r in cr - reach..=cr + reach {
            for q in cq - reach..=cq + reach {
                let (x, y) = Self::lattice(q, r, spacing);
                let sx = cx + (x - self.cam.0) * zoom;
                let sy = cy + (y - self.cam.1) * zoom;
                if sx > -spacing
                    && sx < self.width as f32 + spacing
                    && sy > -spacing
                    && sy < self.height as f32 + spacing
                {
                    nodes.push((sx, sy, Self::class(q, r), (q, r)));
                }
            }
        }
        nodes
    }

    /// Fades the buffers, draws the faces (chords), edges, nodes and beat ring, then composes
    /// the warm, cool and path layers through their palettes.
    fn draw(&mut self, nodes: &[(f32, f32, usize, (i32, i32))], spacing: f32) {
        for buf in [&mut self.warm, &mut self.cool] {
            buf.iter_mut()
                .for_each(|v| *v = (f32::from(*v) * TRAIL_DECAY) as u8);
        }
        self.path
            .iter_mut()
            .for_each(|v| *v = (f32::from(*v) * PATH_DECAY) as u8);

        let (w, h) = (self.width, self.height);
        let at = |q: i32, r: i32| nodes.iter().find(|n| n.3 == (q, r)).map(|n| (n.0, n.1));
        let level = |c: usize| self.chroma[c];
        for &(_, _, _, (q, r)) in nodes {
            // Up triangle = major triad on this node; down triangle = minor triad on `pc + 4`.
            let (Some(a), Some(b), Some(c)) = (at(q, r), at(q + 1, r), at(q, r + 1)) else {
                continue;
            };
            let classes = [
                Self::class(q, r),
                Self::class(q + 1, r),
                Self::class(q, r + 1),
            ];
            let lit = classes.map(level);
            if lit.iter().all(|&l| l > LIT) {
                let v = ((lit[0] * lit[1] * lit[2]).cbrt() * 255.0) as u8;
                let mut canvas = Canvas {
                    buf: &mut self.warm,
                    width: w,
                    height: h,
                };
                fill_triangle(&mut canvas, [a, b, c], v);
                let mut canvas = Canvas {
                    buf: &mut self.path,
                    width: w,
                    height: h,
                };
                fill_triangle(&mut canvas, [a, b, c], v / 2);
            }
            if let Some(d) = at(q + 1, r + 1) {
                let lit = [lit[1], lit[2], level(Self::class(q + 1, r + 1))];
                if lit.iter().all(|&l| l > LIT) {
                    let v = ((lit[0] * lit[1] * lit[2]).cbrt() * 255.0) as u8;
                    let mut canvas = Canvas {
                        buf: &mut self.cool,
                        width: w,
                        height: h,
                    };
                    fill_triangle(&mut canvas, [b, c, d], v);
                    let mut canvas = Canvas {
                        buf: &mut self.path,
                        width: w,
                        height: h,
                    };
                    fill_triangle(&mut canvas, [b, c, d], v / 2);
                }
            }
            // Edges between two sounding notes.
            let mut canvas = Canvas {
                buf: &mut self.cool,
                width: w,
                height: h,
            };
            for (to, class) in [(b, classes[1]), (c, classes[2])] {
                let l = lit[0].min(level(class));
                if l > LIT {
                    line(&mut canvas, a, to, (l * 200.0) as u8);
                }
            }
            let l = lit[1].min(lit[2]);
            if l > LIT {
                line(&mut canvas, b, c, (l * 200.0) as u8);
            }
        }
        let mut canvas = Canvas {
            buf: &mut self.cool,
            width: w,
            height: h,
        };
        for &(x, y, c, _) in nodes {
            let l = self.chroma[c];
            let radius = 2.0 + 0.04 * spacing * l;
            disc(&mut canvas, (x, y), radius, 25 + (l * 230.0) as u8);
        }
        if let Some((x, y, r)) = self.ring {
            let fade = 1.0 - r / (w.max(h) as f32);
            circle(&mut canvas, (x, y), r, (fade * 180.0) as u8);
            self.ring = (fade > 0.0).then_some((x, y, r + 6.0));
        }

        if self.blends_left > 0 {
            self.blends_left -= 1;
            let t = 1.0 - self.blends_left as f32 / palette::BLEND_FRAMES as f32;
            self.pal_warm = palette::blend(&self.from.0, &self.to.0, t);
            self.pal_cool = palette::blend(&self.from.1, &self.to.1, t);
        }
        for (i, px) in self.rgba.chunks_exact_mut(4).enumerate() {
            let warm = self.pal_warm[usize::from(self.warm[i])];
            let cool = self.pal_cool[usize::from(self.cool[i])];
            let path = self.pal_cool[usize::from(self.path[i])];
            for c in 0..3 {
                px[c] = warm[c].saturating_add(cool[c]).saturating_add(path[c] / 2);
            }
        }
    }
}

/// A random Geiss palette whose bright end is red-dominant (`warm`) or blue-dominant.
fn warm_palette(rng: &mut rand::rngs::ThreadRng, warm: bool) -> Palette {
    for _ in 0..50 {
        let pal = palette::random(rng, false);
        let [r, _, b] = pal[200];
        if (r > b) == warm {
            return pal;
        }
    }
    palette::random(rng, false)
}

/// Brightens the pixels of the segment `a`→`b` to at least `c` (Bresenham).
fn line(canvas: &mut Canvas, a: (f32, f32), b: (f32, f32), c: u8) {
    let (mut x0, mut y0) = (a.0 as i32, a.1 as i32);
    let (x1, y1) = (b.0 as i32, b.1 as i32);
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = ((x1 - x0).signum(), (y1 - y0).signum());
    let mut err = dx + dy;
    loop {
        canvas.plot_max(x0, y0, c);
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
fn fill_triangle(canvas: &mut Canvas, mut p: [(f32, f32); 3], c: u8) {
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
            canvas.plot_max(x, y, c);
        }
    }
}

/// Filled disc of radius `r` around `p`.
fn disc(canvas: &mut Canvas, p: (f32, f32), r: f32, c: u8) {
    let ri = r.ceil() as i32;
    for dy in -ri..=ri {
        for dx in -ri..=ri {
            if ((dx * dx + dy * dy) as f32) <= r * r {
                canvas.plot_max(p.0 as i32 + dx, p.1 as i32 + dy, c);
            }
        }
    }
}

/// Circle outline of radius `r` around `p`.
fn circle(canvas: &mut Canvas, p: (f32, f32), r: f32, c: u8) {
    let steps = (r * std::f32::consts::TAU).ceil().max(8.0) as usize;
    for i in 0..steps {
        let a = i as f32 / steps as f32 * std::f32::consts::TAU;
        canvas.plot_max((p.0 + r * a.cos()) as i32, (p.1 + r * a.sin()) as i32, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_c_major_chord_lights_c_e_and_g() {
        let notes = [261.63f32, 329.63, 392.0];
        let pcm: Vec<f32> = (0..FFT)
            .flat_map(|i| {
                let t = i as f32 / 44_100.0;
                let v = notes
                    .iter()
                    .map(|hz| 0.3 * (hz * std::f32::consts::TAU * t).sin())
                    .sum::<f32>();
                [v, v]
            })
            .collect();
        let mut engine = Tonnetz::new(160, 90, 44_100, 1e9);
        for _ in 0..20 {
            engine.step(&pcm);
        }
        let mut order: Vec<usize> = (0..12).collect();
        order.sort_by(|&a, &b| engine.chroma[b].total_cmp(&engine.chroma[a]));
        let mut top: Vec<usize> = order[..3].to_vec();
        top.sort_unstable();
        assert_eq!(top, vec![0, 4, 7], "chroma {:?}", engine.chroma);
        assert!(
            engine
                .rgba
                .chunks_exact(4)
                .any(|p| p[0] > 0 || p[1] > 0 || p[2] > 0)
        );
    }
}
