//! The Tonnetz engine: the sound is folded into the 12 pitch classes and drawn on Euler's
//! Tonnetz, the hexagonal lattice where a step right is a fifth and a step up-right a major
//! third, so every major triad is an up-pointing triangle and every minor triad a down-pointing
//! one. Lit nodes are the notes heard, filled triangles the chords, the camera follows the key
//! and the chords leave a fading path. Original design, no source port.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::engine::{Clock, CpuEngine};
use crate::geissoxide::palette::{self, Fade};
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
    warm_pal: Fade,
    cool_pal: Fade,
    rng: rand::rngs::ThreadRng,
    clock: Clock,
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
            warm_pal: Fade::new(palette::random_tinted(&mut rng, true)),
            cool_pal: Fade::new(palette::random_tinted(&mut rng, false)),
            rng,
            clock: Clock::new(duration),
            rgba: vec![255; width * height * 4],
        }
    }
}

impl CpuEngine for Tonnetz {
    fn frames_needed(&self) -> usize {
        FFT
    }

    fn step(&mut self, pcm: &[f32]) -> &[u8] {
        if self.clock.tick() {
            self.next();
        }
        // MilkDrop's analysis only needs the most recent FFT_SIZE frames.
        let recent = &pcm[pcm.len().saturating_sub(FFT_SIZE * 2)..];
        self.audio.update(recent, self.clock.fps, self.clock.frame);
        self.update_chroma(pcm);
        self.move_camera();

        let spacing = SPACING * self.height as f32;
        let zoom = 1.0 + 0.06 * (self.audio.att[0] - 1.0).clamp(-0.5, 1.0);
        let nodes = self.visible_nodes(spacing, zoom);

        if self.clock.beat(&self.audio) {
            let root = (0..12).max_by(|&a, &b| self.chroma[a].total_cmp(&self.chroma[b]));
            if let Some(&(x, y, ..)) = root.and_then(|c| nodes.iter().find(|n| n.2 == c)) {
                self.ring = Some((x, y, 0.0));
            }
        }
        self.draw(&nodes, spacing * zoom);
        &self.rgba
    }

    fn next(&mut self) {
        self.clock.reset_switch();
        self.warm_pal
            .to(palette::random_tinted(&mut self.rng, true));
        self.cool_pal
            .to(palette::random_tinted(&mut self.rng, false));
    }
}

impl Tonnetz {
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
        let rate = |r: f32| self.clock.rate(r);
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
                canvas.fill_triangle([a, b, c], v);
                let mut canvas = Canvas {
                    buf: &mut self.path,
                    width: w,
                    height: h,
                };
                canvas.fill_triangle([a, b, c], v / 2);
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
                    canvas.fill_triangle([b, c, d], v);
                    let mut canvas = Canvas {
                        buf: &mut self.path,
                        width: w,
                        height: h,
                    };
                    canvas.fill_triangle([b, c, d], v / 2);
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
                    canvas.line(a, to, (l * 200.0) as u8);
                }
            }
            let l = lit[1].min(lit[2]);
            if l > LIT {
                canvas.line(b, c, (l * 200.0) as u8);
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
            canvas.disc((x, y), radius, 25 + (l * 230.0) as u8);
        }
        if let Some((x, y, r)) = self.ring {
            let fade = 1.0 - r / (w.max(h) as f32);
            canvas.circle((x, y), r, (fade * 180.0) as u8);
            self.ring = (fade > 0.0).then_some((x, y, r + 6.0));
        }

        let (warm_pal, cool_pal) = (self.warm_pal.tick(), self.cool_pal.tick());
        for (i, px) in self.rgba.chunks_exact_mut(4).enumerate() {
            let warm = warm_pal[usize::from(self.warm[i])];
            let cool = cool_pal[usize::from(self.cool[i])];
            let path = cool_pal[usize::from(self.path[i])];
            for c in 0..3 {
                px[c] = warm[c].saturating_add(cool[c]).saturating_add(path[c] / 2);
            }
        }
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
