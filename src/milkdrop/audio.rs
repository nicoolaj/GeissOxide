//! MilkDrop's sound analysis: 1024-sample FFT with its equaliser curve, bass/mid/treb levels
//! relative to a long-term average (`bass`, `bass_att`, …) and the 512-sample waveforms the
//! wave renderers read. Follows `fft.cpp` / `AnalyzeNewSound` (as ported by butterchurn).

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

/// Samples per channel fed to the FFT.
pub const FFT_SIZE: usize = 1024;
/// Waveform / spectrum samples exposed to presets.
pub const SAMPLES: usize = 512;

/// `N` log-spaced bin ranges over bins `1..=SAMPLES/2` (up to ~11 kHz), each at least one bin
/// wide, for the original engines.
pub fn log_bands<const N: usize>() -> [(usize, usize); N] {
    let mut bands = [(0, 0); N];
    let mut lo = 1;
    for (k, band) in bands.iter_mut().enumerate() {
        let hi = ((SAMPLES / 2) as f32)
            .powf((k + 1) as f32 / N as f32)
            .round() as usize;
        let hi = hi.max(lo + 1);
        *band = (lo, hi);
        lo = hi;
    }
    bands
}

/// Per-frame audio features.
pub struct Audio {
    fft: Arc<dyn Fft<f32>>,
    equalize: [f32; SAMPLES],
    /// `[bass, mid, treb]` bin ranges.
    bands: [(usize, usize); 3],
    imm: [f32; 3],
    avg: [f32; 3],
    long_avg: [f32; 3],
    /// Instantaneous level of each band relative to its long-term average.
    pub level: [f32; 3],
    /// Smoothed ("attenuated") level of each band.
    pub att: [f32; 3],
    /// Left/right waveform, 512 samples at ±128 scale.
    pub time: [[f32; SAMPLES]; 2],
    /// Left/right spectrum, 512 bins.
    pub freq: [[f32; SAMPLES]; 2],
    scratch: Vec<Complex<f32>>,
}

impl Audio {
    pub fn new(sample_rate: u32) -> Self {
        let bucket_hz = sample_rate as f32 / FFT_SIZE as f32;
        let bin = |hz: f32| {
            ((hz / bucket_hz).round() as isize - 1).clamp(0, SAMPLES as isize - 1) as usize
        };
        let (bass_low, bass_high, mid_high, treb_high) =
            (bin(20.0), bin(320.0), bin(2800.0), bin(11025.0));
        let mut equalize = [0.0; SAMPLES];
        for (i, e) in equalize.iter_mut().enumerate() {
            *e = -0.02 * ((SAMPLES - i) as f32 / SAMPLES as f32).ln();
        }
        Self {
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            equalize,
            bands: [
                (bass_low, bass_high),
                (bass_high, mid_high),
                (mid_high, treb_high),
            ],
            imm: [0.0; 3],
            avg: [1.0; 3],
            long_avg: [1.0; 3],
            level: [1.0; 3],
            att: [1.0; 3],
            time: [[0.0; SAMPLES]; 2],
            freq: [[0.0; SAMPLES]; 2],
            scratch: vec![Complex::default(); FFT_SIZE],
        }
    }

    /// Analyses the latest `FFT_SIZE` interleaved stereo frames.
    pub fn update(&mut self, pcm: &[f32], fps: f32, frame: u64) {
        let sample = |i: usize, ch: usize| (pcm[i * 2 + ch] * 128.0).clamp(-128.0, 127.0);
        for ch in 0..2 {
            for (j, t) in self.time[ch].iter_mut().enumerate() {
                // Undersample by two, averaging each sample with its predecessor.
                let i = j * 2;
                let prev = sample(i.saturating_sub(1), ch);
                *t = 0.5 * (sample(i, ch) + prev);
            }
            self.freq[ch] = self.spectrum(|i| sample(i, ch));
        }
        let mono = self.spectrum(|i| 0.5 * (sample(i, 0) + sample(i, 1)));

        let fps = if fps.is_finite() {
            fps.clamp(15.0, 144.0)
        } else {
            15.0
        };
        let rate = |r: f32| r.powf(30.0 / fps);
        for i in 0..3 {
            let (lo, hi) = self.bands[i];
            self.imm[i] = mono[lo..hi].iter().sum();
            let r = rate(if self.imm[i] > self.avg[i] { 0.2 } else { 0.5 });
            self.avg[i] = self.avg[i] * r + self.imm[i] * (1.0 - r);
            let r = rate(if frame < 50 { 0.9 } else { 0.992 });
            self.long_avg[i] = self.long_avg[i] * r + self.imm[i] * (1.0 - r);
            if self.long_avg[i] < 0.001 {
                self.level[i] = 1.0;
                self.att[i] = 1.0;
            } else {
                self.level[i] = self.imm[i] / self.long_avg[i];
                self.att[i] = self.avg[i] / self.long_avg[i];
            }
        }
    }

    fn spectrum(&mut self, sample: impl Fn(usize) -> f32) -> [f32; SAMPLES] {
        for (i, c) in self.scratch.iter_mut().enumerate() {
            *c = Complex::new(sample(i), 0.0);
        }
        self.fft.process(&mut self.scratch);
        let mut out = [0.0; SAMPLES];
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.equalize[i] * self.scratch[i].norm();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32) -> Vec<f32> {
        (0..FFT_SIZE)
            .flat_map(|i| [0.5 * (i as f32 * hz * std::f32::consts::TAU / 44_100.0).sin(); 2])
            .collect()
    }

    #[test]
    fn levels_react_to_the_tone_frequency() {
        let mut a = Audio::new(44_100);
        for _ in 0..200 {
            a.update(&tone(60.0), 60.0, 100);
        }
        a.update(&tone(8000.0), 60.0, 100);
        assert!(a.level[2] > 3.0 && a.level[0] < 0.5, "treb {:?}", a.level);
        for _ in 0..200 {
            a.update(&tone(8000.0), 60.0, 100);
        }
        a.update(&tone(60.0), 60.0, 100);
        assert!(a.level[0] > 3.0 && a.level[2] < 0.5, "bass {:?}", a.level);
        assert!(a.time[0].iter().all(|v| (-128.0..=127.0).contains(v)));
    }
}
