//! The Stage engine: the mix is pulled apart into six instrument families with spectral
//! heuristics — kick, snare, hi-hats (onsets in their bands, the noisy ones told from the tonal
//! ones by spectral flatness), bass, lead (a dominant tonal peak, whose pitch is kept) and pads
//! (slow broadband energy) — and each family is an actor with its own spot on a stage: a drum
//! ring, a particle burst, sparkles, a floor wave, a bobbing light and a background wash.
//! Original design, no source port.

use rand::RngExt;

use crate::engine::{Clock, CpuEngine};
use crate::geissoxide::palette::{self, Fade};
use crate::geissoxide::raster::Canvas;
use crate::milkdrop::audio::{Audio, FFT_SIZE, SAMPLES, equalize_gain};

/// The families, in `Voice` array order.
const KICK: usize = 0;
const SNARE: usize = 1;
const HATS: usize = 2;
const BASS: usize = 3;
const LEAD: usize = 4;
const PADS: usize = 5;
const FAMILIES: usize = 6;
/// Frequency range of each family, Hz.
const RANGES: [(f32, f32); FAMILIES] = [
    (40.0, 120.0),
    (150.0, 2500.0),
    (6000.0, 16000.0),
    (40.0, 250.0),
    (250.0, 2500.0),
    (250.0, 4000.0),
];
/// Onset when the band's energy rises by this many times its long-term average in one frame.
const ONSET: f32 = 1.5;
/// Seconds between two onsets of the same family.
const HOLD: f32 = 0.08;
/// Spectral flatness above which the snare and hats bands are noise-like rather than tonal.
const NOISY: f32 = 0.35;
/// Peak-to-mean ratio above which the lead band holds a single note.
const TONAL: f32 = 4.0;
/// Band energy over its long-term average that counts as full level.
const FULL: f32 = 2.0;
/// Lowest long-term average a band is measured against (its noise floor), in mean magnitude per
/// bin where a full-scale sine reads 1; and the sum of band means below which the stage is silent.
/// ponytail: fixed thresholds; raise them for a noisy mic.
const FLOOR: f32 = 1e-3;
const SILENCE: f32 = 2e-3;
/// Fraction of the trail kept per frame.
const DECAY: f32 = 0.88;
/// Particles per snare hit.
const BURST: usize = 200;

/// One instrument family as heard this frame.
#[derive(Clone, Copy, Default)]
struct Voice {
    /// Smoothed energy relative to the family's long-term average, `0..=1`.
    level: f32,
    /// 1.0 on an onset, decaying after.
    hit: f32,
}

struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

/// A running Stage visualizer.
pub struct Stage {
    width: usize,
    height: usize,
    audio: Audio,
    /// FFT bin range of each family.
    bands: [(usize, usize); FAMILIES],
    /// Long-term average energy of each family.
    avg: [f32; FAMILIES],
    since_hit: [f32; FAMILIES],
    prev: [f32; SAMPLES],
    voices: [Voice; FAMILIES],
    /// Lead pitch within its band, `0..=1`.
    pitch: f32,
    /// Kick rings (radius) and snare particles.
    rings: Vec<f32>,
    particles: Vec<Particle>,
    /// Floor wave phase.
    phase: f32,
    trail: Vec<u8>,
    palette: Fade,
    rng: rand::rngs::ThreadRng,
    clock: Clock,
    rgba: Vec<u8>,
}

impl Stage {
    /// Creates an engine rendering at `width`×`height` for audio at `sample_rate`, changing the
    /// palette every `duration` seconds.
    pub fn new(width: usize, height: usize, sample_rate: u32, duration: f32) -> Self {
        let mut rng = rand::rng();
        let hz_per_bin = sample_rate as f32 / FFT_SIZE as f32;
        let bin = |hz: f32| ((hz / hz_per_bin).round() as usize).clamp(1, SAMPLES - 1);
        let bands = RANGES.map(|(lo, hi)| (bin(lo), bin(hi).max(bin(lo) + 1)));
        let palette = palette::random(&mut rng, false);
        Self {
            width,
            height,
            audio: Audio::new(sample_rate),
            bands,
            avg: [0.0; FAMILIES],
            since_hit: [0.0; FAMILIES],
            prev: [0.0; SAMPLES],
            voices: [Voice::default(); FAMILIES],
            pitch: 0.5,
            rings: Vec::new(),
            particles: Vec::new(),
            phase: 0.0,
            trail: vec![0; width * height],
            palette: Fade::new(palette),
            rng,
            clock: Clock::new(duration),
            rgba: vec![255; width * height * 4],
        }
    }
}

impl CpuEngine for Stage {
    fn frames_needed(&self) -> usize {
        FFT_SIZE
    }

    fn step(&mut self, pcm: &[f32]) -> &[u8] {
        if self.clock.tick() {
            self.next();
        }
        self.audio.update(pcm, self.clock.fps, self.clock.frame);
        self.detect();
        self.draw();
        &self.rgba
    }

    /// New palette.
    fn next(&mut self) {
        self.clock.reset_switch();
        self.palette.to(palette::random(&mut self.rng, false));
    }
}

impl Stage {
    /// Energy, onset and character of each family's band, from the mono spectrum and the
    /// previous frame's.
    fn detect(&mut self) {
        // Mono magnitudes without MilkDrop's equaliser, scaled so a full-scale sine reads 1.
        let mut spectrum = [0.0f32; SAMPLES];
        for (i, s) in spectrum.iter_mut().enumerate() {
            *s = 0.5 * (self.audio.freq[0][i] + self.audio.freq[1][i])
                / (equalize_gain(i) * FFT_SIZE as f32 * 64.0);
        }
        let mut energy = [0.0f32; FAMILIES];
        let mut onset = [false; FAMILIES];
        let mut noisy = [false; FAMILIES];
        let mut total = 0.0;
        let r = self.clock.rate(0.995);
        for (k, &(lo, hi)) in self.bands.iter().enumerate() {
            let band = &spectrum[lo..hi];
            let n = (hi - lo) as f32;
            let mean = band.iter().sum::<f32>() / n;
            total += mean;
            let flux = band
                .iter()
                .zip(&self.prev[lo..hi])
                .map(|(s, p)| (s - p).max(0.0))
                .sum::<f32>()
                / n;
            let flatness =
                (band.iter().map(|s| (s + 1e-6).ln()).sum::<f32>() / n).exp() / (mean + 1e-6);
            energy[k] = mean / self.avg[k].max(FLOOR);
            self.since_hit[k] += self.clock.dt;
            onset[k] = flux / self.avg[k].max(FLOOR) > ONSET && self.since_hit[k] >= HOLD;
            noisy[k] = flatness > NOISY;
            self.avg[k] = self.avg[k] * r + mean * (1.0 - r);
        }
        let (lo, hi) = self.bands[LEAD];
        let (peak_bin, peak) =
            spectrum[lo..hi]
                .iter()
                .enumerate()
                .fold(
                    (lo, 0.0f32),
                    |best, (i, &s)| {
                        if s > best.1 { (lo + i, s) } else { best }
                    },
                );
        let lead_mean = spectrum[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
        let tonal = peak > TONAL * lead_mean;
        if tonal {
            let pitch =
                ((peak_bin as f32 / lo as f32).ln() / (hi as f32 / lo as f32).ln()).clamp(0.0, 1.0);
            self.pitch += (pitch - self.pitch) * 0.3;
        }
        let silent = total < SILENCE;
        self.prev = spectrum;

        for k in 0..FAMILIES {
            let hit = !silent
                && match k {
                    KICK => onset[k],
                    SNARE | HATS => onset[k] && noisy[k],
                    _ => false,
                };
            if hit {
                self.since_hit[k] = 0.0;
                self.voices[k].hit = 1.0;
            } else {
                self.voices[k].hit *= self.clock.rate(0.6);
            }
            let mut target = (energy[k] / FULL).min(1.0);
            if silent || (k == LEAD && !tonal) {
                target = 0.0;
            }
            let v = &mut self.voices[k];
            let r = self.clock.rate(match (k, target > v.level) {
                (PADS, true) => 0.9,
                (PADS, false) => 0.97,
                (_, true) => 0.5,
                (_, false) => 0.85,
            });
            v.level = v.level * r + target * (1.0 - r);
        }
    }

    /// Fades the trail, draws every actor at its spot and maps the palette.
    fn draw(&mut self) {
        let (w, h) = (self.width as f32, self.height as f32);
        let v = self.voices;
        for (y, row) in self.trail.chunks_exact_mut(self.width).enumerate() {
            // Pads: a wash over the back of the stage, fading toward the front.
            let wash = (v[PADS].level * 45.0 * (1.0 - y as f32 / (0.5 * h)).max(0.0)) as u8;
            for t in row.iter_mut() {
                *t = ((f32::from(*t) * DECAY) as u8).max(wash);
            }
        }
        let mut canvas = Canvas {
            buf: &mut self.trail,
            width: self.width,
            height: self.height,
        };

        // Kick: a glowing drum and a ring per hit.
        let drum = (0.5 * w, 0.35 * h);
        canvas.disc(
            drum,
            6.0 + 14.0 * v[KICK].level,
            (60.0 + 160.0 * v[KICK].level) as u8,
        );
        if v[KICK].hit > 0.99 {
            self.rings.push(0.0);
        }
        let reach = 0.3 * h;
        for r in &mut self.rings {
            canvas.circle(drum, *r, (255.0 * (1.0 - *r / reach)) as u8);
            *r += 0.06 * h * 30.0 / self.clock.fps.clamp(15.0, 144.0);
        }
        self.rings.retain(|&r| r < reach);

        // Snare: a burst of particles falling under gravity.
        let snare = (0.64 * w, 0.4 * h);
        if v[SNARE].hit > 0.99 {
            for _ in 0..BURST {
                let angle = self.rng.random_range(0.0..std::f32::consts::TAU);
                let speed = self.rng.random_range(1.0..6.0) * h / 450.0;
                self.particles.push(Particle {
                    x: snare.0,
                    y: snare.1,
                    vx: speed * angle.cos(),
                    vy: speed * angle.sin(),
                });
            }
        }
        for p in &mut self.particles {
            p.vy += 0.15 * h / 450.0;
            p.x += p.vx;
            p.y += p.vy;
            canvas.add(p.x as i32, p.y as i32, 90, 255);
        }
        self.particles.retain(|p| p.y < 0.92 * h);

        // Hats: sparkles top-right.
        let sparkles = (60.0 * v[HATS].hit) as usize;
        for _ in 0..sparkles {
            let x = self.rng.random_range(0.7 * w..0.95 * w);
            let y = self.rng.random_range(0.1 * h..0.3 * h);
            canvas.plot_max(x as i32, y as i32, 220);
        }

        // Bass: a wave rolling along the floor, bottom-left.
        let bass = v[BASS].level;
        self.phase += 0.1 + 0.4 * bass;
        let amp = 0.08 * h * bass;
        let mut last = None;
        for i in 0..=40 {
            let x = 0.05 * w + 0.4 * w * i as f32 / 40.0;
            let y = 0.85 * h + amp * (i as f32 * 0.5 + self.phase).sin();
            if let Some(prev) = last {
                canvas.line(prev, (x, y), (60.0 + 190.0 * bass) as u8);
            }
            last = Some((x, y));
        }

        // Lead: a light front-centre, higher for higher notes, leaving a trail.
        let lead = v[LEAD].level;
        let spot = (0.5 * w, 0.78 * h - 0.3 * h * self.pitch);
        canvas.disc(spot, 4.0 + 10.0 * lead, (255.0 * lead) as u8);

        palette::apply(self.palette.tick(), &self.trail, &mut self.rgba);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(f: impl Fn(usize) -> f32) -> Vec<f32> {
        (0..FFT_SIZE).flat_map(|i| [f(i); 2]).collect()
    }

    #[test]
    fn a_kick_hits_the_drum_and_a_note_lights_the_lead() {
        let silence = frames(|_| 0.0);
        let kick = frames(|i| {
            let t = i as f32 / 44_100.0;
            0.8 * (-t * 40.0).exp() * (t * 60.0 * std::f32::consts::TAU).sin()
        });
        let note = frames(|i| 0.5 * (i as f32 * 1000.0 * std::f32::consts::TAU / 44_100.0).sin());
        let mut stage = Stage::new(160, 90, 44_100, 1e9);
        for _ in 0..30 {
            stage.step(&silence);
        }
        stage.step(&kick);
        assert!(
            stage.voices[KICK].hit > 0.5,
            "kick {:?}",
            stage.voices[KICK].hit
        );
        assert_eq!(stage.voices[HATS].hit, 0.0);
        for _ in 0..30 {
            stage.step(&note);
        }
        assert!(
            stage.voices[LEAD].level > 0.5,
            "lead {}",
            stage.voices[LEAD].level
        );
        assert!(
            stage.voices[KICK].hit < 0.05,
            "kick {}",
            stage.voices[KICK].hit
        );
        assert!(
            stage
                .rgba
                .chunks_exact(4)
                .any(|p| p[0] > 0 || p[1] > 0 || p[2] > 0)
        );
    }
}
