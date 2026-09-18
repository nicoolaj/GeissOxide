//! The Rack engine: a studio rack of measuring instruments drawn on a phosphor screen — two VU
//! meters with ballistic needles and a peak LED, an oscilloscope triggered on the left channel,
//! a 31-band spectrum analyser with falling peak-hold marks, and a goniometer (Lissajous of the
//! stereo field) with a correlation meter under it. Original design, no source port.

use crate::engine::{Clock, CpuEngine};
use crate::geissoxide::palette::{self, Fade};
use crate::geissoxide::raster::Canvas;
use crate::milkdrop::audio::{Audio, FFT_SIZE, SAMPLES, equalize_gain, log_bands};

/// Spectrum analyser bands.
const BANDS: usize = 31;
/// RMS (full scale = 1) that reads 0 VU, and the scale range in dB.
/// ponytail: fixed reference (−15 dBFS sine); make it a CLI knob if it needs calibrating.
const VU_REF: f32 = 0.177;
const VU_MIN: f32 = -20.0;
const VU_MAX: f32 = 3.0;
/// Needle ballistics: natural period in seconds and damping ratio (a real VU overshoots ~1%).
const NEEDLE_PERIOD: f32 = 0.3;
const NEEDLE_ZETA: f32 = 0.7;
/// Analyser floor in dB, bar fall and peak fall in fractions of the scale per second, peak hold.
const DB_FLOOR: f32 = -60.0;
const BAR_FALL: f32 = 1.5;
const PEAK_FALL: f32 = 0.5;
const PEAK_HOLD: f32 = 1.0;
/// Phosphor persistence of the two screens (fraction kept per frame).
const DECAY: f32 = 0.75;
/// Screen tints, cycled by `next`: green, amber, blue-white.
const TINTS: [[f32; 3]; 3] = [[0.2, 1.0, 0.35], [1.0, 0.7, 0.15], [0.55, 0.8, 1.0]];

/// A panel rectangle in pixels.
#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn contains(&self, x: usize, y: usize) -> bool {
        let (x, y) = (x as f32, y as f32);
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// A running Rack visualizer.
pub struct Rack {
    width: usize,
    height: usize,
    audio: Audio,
    /// Panels: VU meters, scope, analyser, goniometer.
    vu: Rect,
    scope: Rect,
    analyser: Rect,
    gonio: Rect,
    /// Needle position and velocity per channel, in dB; peak LED level.
    needle: [(f32, f32); 2],
    led: [f32; 2],
    bands: [(usize, usize); BANDS],
    /// Bar and peak-hold heights (`0..=1`) with the peak's hold timer.
    bar: [f32; BANDS],
    peak: [(f32, f32); BANDS],
    /// Smoothed stereo correlation, `-1..=1`.
    correlation: f32,
    tint: usize,
    trail: Vec<u8>,
    palette: Fade,
    clock: Clock,
    rgba: Vec<u8>,
}

impl Rack {
    /// Creates an engine rendering at `width`×`height` for audio at `sample_rate`, changing the
    /// screen tint every `duration` seconds.
    pub fn new(width: usize, height: usize, sample_rate: u32, duration: f32) -> Self {
        let (w, h) = (width as f32, height as f32);
        let m = 0.03 * w.min(h);
        let (pw, ph) = ((w - 3.0 * m) * 0.5, (h - 3.0 * m) * 0.5);
        let panel = |col: f32, row: f32| Rect {
            x: m + col * (pw + m),
            y: m + row * (ph + m),
            w: pw,
            h: ph,
        };
        Self {
            width,
            height,
            audio: Audio::new(sample_rate),
            vu: panel(0.0, 0.0),
            scope: panel(1.0, 0.0),
            analyser: panel(0.0, 1.0),
            gonio: panel(1.0, 1.0),
            needle: [(VU_MIN, 0.0); 2],
            led: [0.0; 2],
            bands: log_bands::<BANDS>(),
            bar: [0.0; BANDS],
            peak: [(0.0, 0.0); BANDS],
            correlation: 0.0,
            tint: 0,
            trail: vec![0; width * height],
            palette: Fade::new(palette::ramp(TINTS[0])),
            clock: Clock::new(duration),
            rgba: vec![255; width * height * 4],
        }
    }
}

impl CpuEngine for Rack {
    fn frames_needed(&self) -> usize {
        FFT_SIZE
    }

    fn step(&mut self, pcm: &[f32]) -> &[u8] {
        if self.clock.tick() {
            self.next();
        }
        self.audio.update(pcm, self.clock.fps, self.clock.frame);
        self.measure();
        self.draw();
        &self.rgba
    }

    /// Next screen tint.
    fn next(&mut self) {
        self.clock.reset_switch();
        self.tint = (self.tint + 1) % TINTS.len();
        self.palette.to(palette::ramp(TINTS[self.tint]));
    }
}

impl Rack {
    /// Needle ballistics, peak LEDs, analyser bars and peaks, correlation.
    fn measure(&mut self) {
        let dt = self.clock.dt;
        let steps = (dt * 240.0).ceil() as usize;
        let sdt = dt / steps as f32;
        let omega = std::f32::consts::TAU / NEEDLE_PERIOD;
        for ch in 0..2 {
            let wave = &self.audio.time[ch];
            let rms = (wave.iter().map(|s| s * s).sum::<f32>() / SAMPLES as f32).sqrt() / 128.0;
            let target = (20.0 * (rms / VU_REF).max(1e-6).log10()).clamp(VU_MIN, VU_MAX);
            let (x, v) = &mut self.needle[ch];
            for _ in 0..steps {
                *v += (omega * omega * (target - *x) - 2.0 * NEEDLE_ZETA * omega * *v) * sdt;
                *x += *v * sdt;
            }
            let clipped = wave.iter().any(|s| s.abs() >= 126.0);
            self.led[ch] = if clipped {
                1.0
            } else {
                self.led[ch] * self.clock.rate(0.9)
            };
        }

        // Mono magnitudes without MilkDrop's equaliser, scaled so a full-scale sine reads 1.
        let magnitude = |i: usize| {
            0.5 * (self.audio.freq[0][i] + self.audio.freq[1][i])
                / (equalize_gain(i) * FFT_SIZE as f32 * 64.0)
        };
        for (k, &(lo, hi)) in self.bands.iter().enumerate() {
            let mean = (lo..hi).map(magnitude).sum::<f32>() / (hi - lo) as f32;
            let db = 20.0 * mean.max(1e-6).log10();
            let target = ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0);
            self.bar[k] = target.max(self.bar[k] - BAR_FALL * dt);
            let (peak, held) = &mut self.peak[k];
            if target >= *peak {
                *peak = target;
                *held = 0.0;
            } else {
                *held += dt;
                if *held > PEAK_HOLD {
                    *peak = (*peak - PEAK_FALL * dt).max(target);
                }
            }
        }

        let (l, r) = (&self.audio.time[0], &self.audio.time[1]);
        let (mut lr, mut ll, mut rr) = (0.0, 0.0, 0.0);
        for (a, b) in l.iter().zip(r) {
            lr += a * b;
            ll += a * a;
            rr += b * b;
        }
        let corr = if ll * rr > 1e-3 {
            lr / (ll * rr).sqrt()
        } else {
            0.0
        };
        let rate = self.clock.rate(0.8);
        self.correlation = self.correlation * rate + corr * (1.0 - rate);
    }

    /// Persists the two screens, clears the meters, draws every instrument, maps the palette.
    fn draw(&mut self) {
        let (scope, gonio) = (self.scope, self.gonio);
        for (y, row) in self.trail.chunks_exact_mut(self.width).enumerate() {
            for (x, t) in row.iter_mut().enumerate() {
                *t = if scope.contains(x, y) || gonio.contains(x, y) {
                    (f32::from(*t) * DECAY) as u8
                } else {
                    0
                };
            }
        }
        let mut trail = std::mem::take(&mut self.trail);
        let mut canvas = Canvas {
            buf: &mut trail,
            width: self.width,
            height: self.height,
        };
        for panel in [self.vu, self.scope, self.analyser, self.gonio] {
            frame(&mut canvas, panel);
        }
        self.draw_vu(&mut canvas);
        self.draw_scope(&mut canvas);
        self.draw_analyser(&mut canvas);
        self.draw_gonio(&mut canvas);
        self.trail = trail;
        palette::apply(self.palette.tick(), &self.trail, &mut self.rgba);
    }

    /// Two meters side by side: scale arc with ticks, red zone, needle from a pivot below the
    /// scale, peak LED in the corner.
    fn draw_vu(&self, canvas: &mut Canvas) {
        let p = self.vu;
        let angle = |db: f32| {
            let t = (db - VU_MIN) / (VU_MAX - VU_MIN);
            std::f32::consts::FRAC_PI_2 - (t - 0.5) * 1.6 // -45°..+45° around straight up
        };
        for ch in 0..2 {
            let half = p.w * 0.5;
            let pivot = (p.x + half * (ch as f32 + 0.5), p.y + p.h * 0.95);
            let radius = p.h * 0.6;
            let at = |db: f32, r: f32| {
                let a = angle(db);
                (pivot.0 + r * a.cos(), pivot.1 - r * a.sin())
            };
            let mut db = VU_MIN;
            while db <= VU_MAX {
                let c = if db >= 0.0 { 255 } else { 120 };
                canvas.line(at(db, radius), at(db + 0.25, radius), c);
                if db >= 0.0 {
                    canvas.line(at(db, radius - 2.0), at(db + 0.25, radius - 2.0), c);
                }
                db += 0.25;
            }
            for tick in [
                -20.0, -10.0, -7.0, -5.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0,
            ] {
                let len = if tick == 0.0 || tick == -20.0 || tick == -10.0 {
                    8.0
                } else {
                    5.0
                };
                canvas.line(at(tick, radius), at(tick, radius + len), 200);
            }
            let (x, _) = self.needle[ch];
            canvas.line(pivot, at(x, radius * 1.08), 255);
            canvas.disc(pivot, 3.0, 160);
            let led = (
                p.x + half * (ch as f32 + 0.5) + radius * 0.6,
                p.y + p.h * 0.15,
            );
            canvas.circle(led, 4.0, 90);
            if self.led[ch] > 0.05 {
                canvas.disc(led, 4.0, (255.0 * self.led[ch]) as u8);
            }
        }
    }

    /// Both waveforms from the left channel's rising zero crossing, over a faint graticule.
    fn draw_scope(&self, canvas: &mut Canvas) {
        let p = self.scope;
        graticule(canvas, p);
        let wave = &self.audio.time[0];
        let trigger = (1..SAMPLES / 2)
            .find(|&i| wave[i - 1] < 0.0 && wave[i] >= 0.0)
            .unwrap_or(0);
        let n = SAMPLES / 2;
        for (ch, c) in [(0, 230u8), (1, 140u8)] {
            let mut last = None;
            for i in 0..n {
                let s = self.audio.time[ch][(trigger + i).min(SAMPLES - 1)] / 128.0;
                let pt = (
                    p.x + p.w * i as f32 / n as f32,
                    p.y + p.h * 0.5 - s * p.h * 0.45,
                );
                if let Some(prev) = last {
                    canvas.line(prev, pt, c);
                }
                last = Some(pt);
            }
        }
    }

    /// 31 bars rising from the panel floor, a peak-hold mark above each.
    fn draw_analyser(&self, canvas: &mut Canvas) {
        let p = self.analyser;
        let step = p.w / BANDS as f32;
        let floor = p.y + p.h * 0.95;
        let top = p.y + p.h * 0.08;
        for k in 0..BANDS {
            let x0 = p.x + step * (k as f32 + 0.15);
            let x1 = x0 + step * 0.7;
            let y = floor - (floor - top) * self.bar[k];
            for row in (y as i32)..(floor as i32) {
                let t = (floor - row as f32) / (floor - top);
                canvas.line((x0, row as f32), (x1, row as f32), (60.0 + 180.0 * t) as u8);
            }
            let py = floor - (floor - top) * self.peak[k].0;
            canvas.line((x0, py), (x1, py), 255);
            canvas.line((x0, py - 1.0), (x1, py - 1.0), 255);
        }
    }

    /// Lissajous of the stereo field (mono = vertical line) and the correlation bar under it.
    fn draw_gonio(&self, canvas: &mut Canvas) {
        let p = self.gonio;
        let side = p.w.min(p.h) * 0.72;
        let centre = (p.x + p.w * 0.5, p.y + p.h * 0.44);
        let half = side * 0.5;
        let dim = 35;
        canvas.line(
            (centre.0 - half, centre.1 - half),
            (centre.0 + half, centre.1 + half),
            dim,
        );
        canvas.line(
            (centre.0 - half, centre.1 + half),
            (centre.0 + half, centre.1 - half),
            dim,
        );
        canvas.line(
            (centre.0, centre.1 - half),
            (centre.0, centre.1 + half),
            dim,
        );
        canvas.line(
            (centre.0 - half, centre.1),
            (centre.0 + half, centre.1),
            dim,
        );
        let s = std::f32::consts::FRAC_1_SQRT_2 / 128.0;
        for i in 0..SAMPLES {
            let (l, r) = (self.audio.time[0][i], self.audio.time[1][i]);
            let x = centre.0 + (r - l) * s * half;
            let y = centre.1 - (l + r) * s * half;
            canvas.plot_max(x as i32, y as i32, 190);
        }
        let bar_y = p.y + p.h * 0.9;
        let bar_half = p.w * 0.4;
        canvas.line(
            (centre.0 - bar_half, bar_y),
            (centre.0 + bar_half, bar_y),
            80,
        );
        for (t, len) in [(-1.0, 8.0), (0.0, 8.0), (1.0, 8.0), (-0.5, 4.0), (0.5, 4.0)] {
            let x = centre.0 + t * bar_half;
            canvas.line((x, bar_y - len), (x, bar_y), 120);
        }
        let x = centre.0 + self.correlation * bar_half;
        for dy in -3..=3 {
            canvas.line((centre.0, bar_y + dy as f32), (x, bar_y + dy as f32), 220);
        }
        canvas.disc((x, bar_y), 4.0, 255);
    }
}

/// Panel outline.
fn frame(canvas: &mut Canvas, p: Rect) {
    let (x1, y1) = (p.x + p.w - 1.0, p.y + p.h - 1.0);
    canvas.line((p.x, p.y), (x1, p.y), 70);
    canvas.line((x1, p.y), (x1, y1), 70);
    canvas.line((x1, y1), (p.x, y1), 70);
    canvas.line((p.x, y1), (p.x, p.y), 70);
}

/// 8×4 graticule with a brighter centre line.
fn graticule(canvas: &mut Canvas, p: Rect) {
    for i in 1..8 {
        let x = p.x + p.w * i as f32 / 8.0;
        canvas.line((x, p.y), (x, p.y + p.h), 25);
    }
    for i in 1..4 {
        let y = p.y + p.h * i as f32 / 4.0;
        canvas.line((p.x, y), (p.x + p.w, y), if i == 2 { 50 } else { 25 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(amp: f32, right_sign: f32) -> Vec<f32> {
        (0..FFT_SIZE)
            .flat_map(|i| {
                let s = amp * (i as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin();
                [s, right_sign * s]
            })
            .collect()
    }

    #[test]
    fn a_reference_tone_reads_zero_vu_and_mono_correlates() {
        let mut rack = Rack::new(160, 90, 44_100, 1e9);
        let mono = stereo(VU_REF * std::f32::consts::SQRT_2, 1.0);
        for _ in 0..300 {
            rack.step(&mono);
        }
        let db = rack.needle[0].0;
        assert!(db.abs() < 1.0, "needle at {db} dB");
        assert!(rack.correlation > 0.95, "correlation {}", rack.correlation);
        let anti = stereo(0.3, -1.0);
        for _ in 0..100 {
            rack.step(&anti);
        }
        assert!(rack.correlation < -0.95, "correlation {}", rack.correlation);
        assert!(
            rack.rgba
                .chunks_exact(4)
                .any(|p| p[0] > 0 || p[1] > 0 || p[2] > 0)
        );
    }
}
