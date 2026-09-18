//! The Instruments engine: the sound plays drawn instruments. Six strings, one per low/mid
//! band, ring as damped modal oscillators plucked by their band's energy jumps; a drum skin,
//! a damped membrane, is struck at the centre by the beat and off-centre by mid-band onsets;
//! twelve organ pipes light up with the treble bands, an air column flickering up each one.
//! Original design, no source port.

use rand::RngExt;

use crate::engine::{Clock, CpuEngine};
use crate::geissoxide::palette::{self, Fade};
use crate::geissoxide::raster::Canvas;
use crate::milkdrop::audio::{Audio, FFT_SIZE, SAMPLES, equalize_gain};

/// Strings (60 Hz–2.5 kHz) and pipes (2.5–11 kHz), each on log-spaced bands.
const STRINGS: usize = 6;
const PIPES: usize = 12;
/// Modes kept per string.
const MODES: usize = 3;
/// Fundamental of the lowest string in Hz (visual, not audio) and damping per second.
const STRING_HZ: f32 = 2.5;
const STRING_DAMPING: f32 = 2.5;
/// Drum grid: pixels per cell, wave speed² (stable below 0.5), damping per substep, substeps.
const DRUM_SCALE: usize = 3;
const DRUM_C2: f32 = 0.35;
const DRUM_DAMPING: f32 = 0.985;
const DRUM_SUBSTEPS: usize = 4;
/// A band jumps when its energy rises by this many times its long-term average in one frame.
const ONSET: f32 = 1.5;
/// Noise floor of the long-term averages (mean magnitude per bin, full-scale sine = 1).
/// ponytail: fixed threshold; raise it for a noisy mic.
const FLOOR: f32 = 1e-2;
/// Fraction of the trail kept per frame.
const DECAY: f32 = 0.8;

/// A spectrum band measured against its own long-term average.
struct Band {
    lo: usize,
    hi: usize,
    avg: f32,
    prev: f32,
    /// Smoothed level, `0..=1`.
    level: f32,
}

impl Band {
    /// Log-spaced bands between `lo_hz` and `hi_hz`.
    fn log_spaced<const N: usize>(lo_hz: f32, hi_hz: f32, sample_rate: u32) -> [Self; N] {
        let hz_per_bin = sample_rate as f32 / FFT_SIZE as f32;
        let bin = |hz: f32| ((hz / hz_per_bin).round() as usize).clamp(1, SAMPLES - 1);
        std::array::from_fn(|k| {
            let edge = |i: usize| lo_hz * (hi_hz / lo_hz).powf(i as f32 / N as f32);
            let lo = bin(edge(k));
            Self {
                lo,
                hi: bin(edge(k + 1)).max(lo + 1),
                avg: 0.0,
                prev: 0.0,
                level: 0.0,
            }
        })
    }

    /// Updates the level from `spectrum`; returns the energy jump over the average (0 = none).
    fn measure(&mut self, spectrum: &[f32], clock: &Clock) -> f32 {
        let mean = spectrum[self.lo..self.hi].iter().sum::<f32>() / (self.hi - self.lo) as f32;
        let floor = self.avg.max(FLOOR);
        let jump = (mean - self.prev).max(0.0) / floor;
        let target = (mean / floor * 0.5).min(1.0);
        let r = clock.rate(if target > self.level { 0.5 } else { 0.85 });
        self.level = self.level * r + target * (1.0 - r);
        let r = clock.rate(0.995);
        self.avg = self.avg * r + mean * (1.0 - r);
        self.prev = mean;
        if jump > ONSET { jump.min(4.0) } else { 0.0 }
    }
}

/// A string as `MODES` damped oscillators: amplitude and velocity, in pixels.
#[derive(Clone, Copy, Default)]
struct Modes {
    a: [f32; MODES],
    v: [f32; MODES],
}

/// A running Instruments visualizer.
pub struct Instruments {
    width: usize,
    height: usize,
    audio: Audio,
    strings: [Band; STRINGS],
    pipes: [Band; PIPES],
    snare: Band,
    modes: [Modes; STRINGS],
    /// Drum membrane on a coarse grid (`side`² cells), current and previous height.
    side: usize,
    drum: Vec<f32>,
    drum_prev: Vec<f32>,
    /// Air column phase per pipe.
    air: [f32; PIPES],
    trail: Vec<u8>,
    palette: Fade,
    rng: rand::rngs::ThreadRng,
    clock: Clock,
    rgba: Vec<u8>,
}

impl Instruments {
    /// Creates an engine rendering at `width`×`height` for audio at `sample_rate`, changing the
    /// palette every `duration` seconds.
    pub fn new(width: usize, height: usize, sample_rate: u32, duration: f32) -> Self {
        let mut rng = rand::rng();
        let palette = palette::random(&mut rng, false);
        let side = (2.0 * Self::drum_radius(height) / DRUM_SCALE as f32).ceil() as usize + 1;
        let [snare] = Band::log_spaced::<1>(150.0, 2500.0, sample_rate);
        Self {
            width,
            height,
            audio: Audio::new(sample_rate),
            strings: Band::log_spaced(60.0, 2500.0, sample_rate),
            pipes: Band::log_spaced(2500.0, 11000.0, sample_rate),
            snare,
            modes: [Modes::default(); STRINGS],
            side,
            drum: vec![0.0; side * side],
            drum_prev: vec![0.0; side * side],
            air: [0.0; PIPES],
            trail: vec![0; width * height],
            palette: Fade::new(palette),
            rng,
            clock: Clock::new(duration),
            rgba: vec![255; width * height * 4],
        }
    }

    fn drum_radius(height: usize) -> f32 {
        0.15 * height as f32
    }

    fn drum_centre(&self) -> (f32, f32) {
        (0.25 * self.width as f32, 0.8 * self.height as f32)
    }
}

impl CpuEngine for Instruments {
    fn frames_needed(&self) -> usize {
        FFT_SIZE
    }

    fn step(&mut self, pcm: &[f32]) -> &[u8] {
        if self.clock.tick() {
            self.next();
        }
        self.audio.update(pcm, self.clock.fps, self.clock.frame);
        // Mono magnitudes without MilkDrop's equaliser, scaled so a full-scale sine reads 1.
        let mut spectrum = [0.0f32; SAMPLES];
        for (i, s) in spectrum.iter_mut().enumerate() {
            *s = 0.5 * (self.audio.freq[0][i] + self.audio.freq[1][i])
                / (equalize_gain(i) * FFT_SIZE as f32 * 64.0);
        }
        let h = self.height as f32;
        for (band, modes) in self.strings.iter_mut().zip(&mut self.modes) {
            let jump = band.measure(&spectrum, &self.clock);
            for (k, v) in modes.v.iter_mut().enumerate() {
                *v += 0.15 * h * jump / (k + 1) as f32;
            }
        }
        for band in &mut self.pipes {
            band.measure(&spectrum, &self.clock);
        }
        let snare = self.snare.measure(&spectrum, &self.clock);
        if self.clock.beat(&self.audio) {
            let s = self.side as f32 * 0.5;
            let strength = (self.audio.level[0] - self.audio.att[0]).clamp(0.5, 3.0);
            self.strike(s, s, strength);
        }
        if snare > 0.0 {
            let r = self.side as f32 * 0.3;
            let angle = self.rng.random_range(0.0..std::f32::consts::TAU);
            let s = self.side as f32 * 0.5;
            self.strike(s + r * angle.cos(), s + r * angle.sin(), 0.4 * snare);
        }
        self.vibrate();
        self.draw();
        &self.rgba
    }

    /// New palette and a knock on the drum.
    fn next(&mut self) {
        self.clock.reset_switch();
        self.palette.to(palette::random(&mut self.rng, false));
        let s = self.side as f32 * 0.5;
        self.strike(s, s, 2.0);
    }
}

impl Instruments {
    /// Presses a Gaussian dimple of depth `weight` into the membrane at grid `(x, y)`.
    fn strike(&mut self, x: f32, y: f32, weight: f32) {
        let radius = 2.0;
        let (n, r) = (self.side as i32, (2.0 * radius) as i32);
        for dy in -r..=r {
            for dx in -r..=r {
                let (px, py) = (x as i32 + dx, y as i32 + dy);
                if px >= 0 && px < n && py >= 0 && py < n {
                    let d2 = (dx * dx + dy * dy) as f32;
                    self.drum[(py * n + px) as usize] -= weight * (-d2 / (radius * radius)).exp();
                }
            }
        }
    }

    /// Integrates the strings (semi-implicit Euler over the real frame time) and the membrane
    /// (leap-frog, clamped to zero outside the rim).
    fn vibrate(&mut self) {
        // Substeps of at most 1/240 s keep the stiffest mode (ω ≈ 100 rad/s) stable.
        let steps = (self.clock.dt * 240.0).ceil() as usize;
        let dt = self.clock.dt / steps as f32;
        for (k, modes) in self.modes.iter_mut().enumerate() {
            let f0 = STRING_HZ * (1.0 + 0.25 * k as f32);
            for m in 0..MODES {
                let omega = std::f32::consts::TAU * f0 * (m + 1) as f32;
                for _ in 0..steps {
                    modes.v[m] -= (omega * omega * modes.a[m] + STRING_DAMPING * modes.v[m]) * dt;
                    modes.a[m] += modes.v[m] * dt;
                }
            }
        }
        let n = self.side;
        let c = (n as f32 - 1.0) * 0.5;
        let inside = |x: usize, y: usize| (x as f32 - c).hypot(y as f32 - c) < c;
        for _ in 0..DRUM_SUBSTEPS {
            let mut next = std::mem::take(&mut self.drum_prev);
            for y in 1..n - 1 {
                for x in 1..n - 1 {
                    let i = y * n + x;
                    next[i] = if inside(x, y) {
                        let lap = self.drum[i - n]
                            + self.drum[i + n]
                            + self.drum[i - 1]
                            + self.drum[i + 1]
                            - 4.0 * self.drum[i];
                        (2.0 * self.drum[i] - next[i] + DRUM_C2 * lap) * DRUM_DAMPING
                    } else {
                        0.0
                    };
                }
            }
            self.drum_prev = std::mem::replace(&mut self.drum, next);
        }
    }

    /// Fades the trail, draws the pipes, the strings and the drum, and maps the palette.
    fn draw(&mut self) {
        let (w, h) = (self.width as f32, self.height as f32);
        let (cx, cy) = self.drum_centre();
        for t in &mut self.trail {
            *t = (f32::from(*t) * DECAY) as u8;
        }
        let mut canvas = Canvas {
            buf: &mut self.trail,
            width: self.width,
            height: self.height,
        };

        // Pipes: an outline each, filled to the band level with a rising air column.
        let step = 0.8 * w / PIPES as f32;
        let (top, foot) = (0.05 * h, 0.35 * h);
        for (k, band) in self.pipes.iter().enumerate() {
            let x0 = 0.1 * w + step * (k as f32 + 0.25);
            let x1 = x0 + 0.5 * step;
            canvas.line((x0, top), (x0, foot), 40);
            canvas.line((x1, top), (x1, foot), 40);
            canvas.line((x0, foot), (x1, foot), 40);
            self.air[k] += 0.2 + 0.6 * band.level;
            let lit = foot - (foot - top) * band.level;
            for y in (lit as i32)..(foot as i32) {
                let flicker = (y as f32 * 0.15 + self.air[k]).sin();
                let c = (40.0 + 150.0 * band.level + 60.0 * flicker).clamp(0.0, 255.0) as u8;
                canvas.line((x0 + 1.0, y as f32), (x1 - 1.0, y as f32), c);
            }
        }

        // Strings: `y = y0 + Σ a_k sin(kπs)`, brighter the faster they move.
        let (x0, x1) = (0.1 * w, 0.9 * w);
        for (k, modes) in self.modes.iter().enumerate() {
            let y0 = 0.42 * h + 0.045 * h * k as f32;
            let speed = modes.v.iter().map(|v| v.abs()).sum::<f32>() / (0.5 * h);
            let c = (60.0 + 195.0 * speed.min(1.0)) as u8;
            let mut last = None;
            for i in 0..=64 {
                let s = i as f32 / 64.0;
                let y = y0
                    + modes
                        .a
                        .iter()
                        .enumerate()
                        .map(|(m, a)| a * ((m + 1) as f32 * std::f32::consts::PI * s).sin())
                        .sum::<f32>();
                let p = (x0 + (x1 - x0) * s, y);
                if let Some(prev) = last {
                    canvas.line(prev, p, c);
                }
                last = Some(p);
            }
        }

        // Drum: membrane height inside the rim, the rim itself as a circle.
        let radius = Self::drum_radius(self.height);
        let n = self.side;
        let r = radius as i32;
        for dy in -r..=r {
            for dx in -r..=r {
                if ((dx * dx + dy * dy) as f32) >= radius * radius {
                    continue;
                }
                let gx = ((dx as f32 + radius) / DRUM_SCALE as f32) as usize;
                let gy = ((dy as f32 + radius) / DRUM_SCALE as f32) as usize;
                let u = self.drum[gy.min(n - 1) * n + gx.min(n - 1)];
                let c = (100.0 + 120.0 * (0.5 * u).clamp(-0.8, 1.0)) as u8;
                canvas.plot_max(cx as i32 + dx, cy as i32 + dy, c);
            }
        }
        canvas.circle((cx, cy), radius, 100);
        canvas.circle((cx, cy), radius + 1.0, 100);

        palette::apply(self.palette.tick(), &self.trail, &mut self.rgba);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_low_tone_plucks_the_low_string_and_silence_lets_it_ring_down() {
        let tone: Vec<f32> = (0..FFT_SIZE)
            .flat_map(|i| [0.5 * (i as f32 * 100.0 * std::f32::consts::TAU / 44_100.0).sin(); 2])
            .collect();
        let silence = vec![0.0; FFT_SIZE * 2];
        let mut engine = Instruments::new(160, 90, 44_100, 1e9);
        let energy = |m: &Modes| m.a.iter().map(|a| a * a).sum::<f32>();
        for _ in 0..30 {
            engine.step(&tone);
        }
        let low = energy(&engine.modes[0]);
        let high = energy(&engine.modes[STRINGS - 1]);
        assert!(low > 0.0 && high < 1e-3 * low, "low {low} high {high}");
        let mut peak = low;
        for _ in 0..600 {
            engine.step(&silence);
            peak = peak.max(energy(&engine.modes[0]));
        }
        let rest = energy(&engine.modes[0]);
        assert!(
            rest < 0.1 * peak,
            "string should ring down: {peak} -> {rest}"
        );
        assert!(engine.drum.iter().all(|v| v.is_finite()));
    }
}
