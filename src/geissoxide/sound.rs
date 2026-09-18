//! Waveform preparation and volume/beat statistics. Ports the post-capture part of
//! `GetWaveData`, the volume tracking of `RenderDots` and the beat logic of `RenderWave`.

/// Frames of volume history (`PAST_VOL_N`).
const PAST_VOL_N: usize = 30;
/// Smallest interleaved buffer (`MINBUFSIZE`): enough for the circular waveform.
pub const MIN_BUFSIZE: usize = (314 + 50) * 2 + 20;
/// Original default wave scaling (`volscale`, tuned for a line-in at half volume).
const VOLSCALE: f32 = 0.20;

/// Sound state shared by the waveform renderers and the map scheduler.
pub struct Sound {
    width: usize,
    /// Interleaved sample count kept per frame (`BUFSIZE`).
    pub bufsize: usize,
    /// Raw samples at 16-bit scale after the level trigger (`g_SoundBuffer`).
    raw: Vec<f32>,
    /// Smoothed, scaled, centred samples in pixels (`g_fSoundBuffer`); even = left, odd = right.
    pub wave: Vec<f32>,
    last_v: f32,
    last_slope: f32,
    /// Peak-to-peak level of this frame, in 1/256 of full scale.
    pub current_vol: f32,
    avg_vol: f32,
    pub avg_vol_narrow: f32,
    past_vol: [f32; PAST_VOL_N],
    past_vol_pos: usize,
    /// A steady beat is present (`bBeatMode`).
    pub beat_mode: bool,
    /// This frame is a strong beat (`bBigBeat`); maps switch on these.
    pub big_beat: bool,
    /// Lowered while waiting for a beat so a map switch eventually happens.
    pub big_beat_threshold: f32,
    frames_since_silence: u32,
    /// No signal for a long time (`SoundEmpty`).
    pub silent: bool,
}

impl Sound {
    pub fn new(width: usize) -> Self {
        let bufsize = (width * 2).max(MIN_BUFSIZE);
        Self {
            width,
            bufsize,
            raw: vec![0.0; bufsize],
            wave: vec![0.0; bufsize + width * 4],
            last_v: 0.0,
            last_slope: 0.0,
            current_vol: 0.0,
            avg_vol: 0.0,
            avg_vol_narrow: 0.0,
            past_vol: [0.0; PAST_VOL_N],
            past_vol_pos: 0,
            beat_mode: false,
            big_beat: false,
            big_beat_threshold: 1.10,
            frames_since_silence: 0,
            silent: false,
        }
    }

    /// How many stereo frames `update` wants: the buffer plus room for the trigger search.
    pub fn frames_needed(&self) -> usize {
        self.bufsize / 2 + self.width / 2
    }

    /// Consumes the latest interleaved stereo `pcm` (`-1.0..=1.0`) for one video frame.
    pub fn update(&mut self, pcm: &[f32], fps: f32, frames_til_auto_switch: f32) {
        let w = self.width;
        // Level trigger: find a phase matching last frame's so the wave doesn't jitter.
        let half = w / 2;
        let mut shift = 0;
        let mut found = false;
        let mut i = 8;
        while i < half {
            let old_v = pcm[i + half - 8] * 32767.0;
            let v = pcm[i + half] * 32767.0;
            if (v - self.last_v).abs() <= 256.0 && self.last_slope * (v - old_v) >= 0.0 {
                self.last_v = v;
                self.last_slope = v - old_v;
                shift = i;
                found = true;
                break;
            }
            i += 2;
        }
        if !found {
            let old_v = pcm[half] * 32767.0;
            let v = pcm[half + 8] * 32767.0;
            self.last_v = v;
            self.last_slope = v - old_v;
        }
        for (r, s) in self.raw.iter_mut().zip(&pcm[shift..]) {
            *r = s * 32767.0;
        }

        // Smoothing (shows bass more than treble), then scale to pixels.
        let billy = VOLSCALE / (64.0 * (640.0 / w as f32)) * 4.0;
        for i in 0..self.bufsize - 2 {
            self.wave[i] = (0.8 * self.raw[i] + 0.2 * self.raw[i + 2]) * billy;
        }
        // Centre each channel on zero.
        for ch in 0..2 {
            let center: f32 = (ch..self.bufsize)
                .step_by(8)
                .map(|i| self.wave[i])
                .sum::<f32>()
                / (w as f32 * 0.125);
            for i in (ch..self.bufsize).step_by(2) {
                self.wave[i] -= center;
            }
        }

        // Volume statistics (from `RenderDots`).
        let (mut low, mut high) = (self.raw[0], self.raw[0]);
        let mut i = self.bufsize as isize - 4;
        while i > 0 {
            low = low.min(self.raw[i as usize]);
            high = high.max(self.raw[i as usize]);
            i -= 4;
        }
        let vol = (high - low) / 256.0;
        self.current_vol = vol;
        let rate = |r: f32| adjust_rate_to_fps(r, 30.0, fps);
        self.avg_vol_narrow = self.avg_vol_narrow * rate(0.30) + vol * (1.0 - rate(0.30));
        self.avg_vol = self.avg_vol * rate(0.85) + vol * (1.0 - rate(0.85));
        self.past_vol_pos = (self.past_vol_pos + 1) % PAST_VOL_N;
        self.past_vol[self.past_vol_pos] = self.avg_vol_narrow;

        // Beat mode: the smoothed volume history is evenly bumpy when a good beat is on.
        let avg_uniform = self.past_vol.iter().sum::<f32>() / PAST_VOL_N as f32;
        let mut beat_strength: f32 = 0.0;
        for i in 1..PAST_VOL_N {
            beat_strength +=
                ((self.past_vol[i] - self.past_vol[i - 1]).abs() - avg_uniform * 0.15).max(0.0);
        }
        beat_strength = if avg_uniform < 10.0 {
            0.0
        } else {
            beat_strength / avg_uniform * 10.0
        };
        if beat_strength > 90.0 + 19.0 {
            self.beat_mode = true;
        } else if beat_strength < 90.0 - 19.0 {
            self.beat_mode = false;
        }
        let max_vol = self.past_vol[..PAST_VOL_N / 3]
            .iter()
            .copied()
            .fold(0.0, f32::max);
        self.big_beat = self.avg_vol_narrow > max_vol * self.big_beat_threshold;

        // Silence detection.
        if self.base() == 0 {
            self.frames_since_silence += 1;
            if self.frames_since_silence as f32 > frames_til_auto_switch * 2.0 {
                self.silent = true;
            }
        } else {
            self.frames_since_silence = 0;
            self.silent = false;
        }
    }

    /// Brightness of the waveform this frame, 0..=155 (`base` in `RenderWave`, saver build).
    pub fn base(&self) -> u8 {
        ((self.current_vol * 6.0 - self.avg_vol * 3.5) * 10.0 - 40.0).clamp(0.0, 155.0) as u8
    }
}

/// Converts a per-frame decay rate tuned at `fps1` to the equivalent at `actual_fps`.
pub fn adjust_rate_to_fps(rate_at_fps1: f32, fps1: f32, actual_fps: f32) -> f32 {
    rate_at_fps1.powf(fps1).powf(1.0 / actual_fps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_input_is_centred_and_triggers_stably() {
        let w = 320;
        let mut s = Sound::new(w);
        let n = s.frames_needed();
        let sine = |phase: usize| -> Vec<f32> {
            (0..n)
                .flat_map(|i| {
                    let v = 0.05 * ((i + phase) as f32 * 0.1).sin();
                    [v, v]
                })
                .collect()
        };
        s.update(&sine(0), 60.0, 400.0);
        let first_v = s.last_v;
        s.update(&sine(37), 60.0, 400.0);
        assert!(
            (s.last_v - first_v).abs() <= 256.0,
            "trigger drifted: {first_v} → {}",
            s.last_v
        );
        let mean: f32 =
            (0..s.bufsize).step_by(2).map(|i| s.wave[i]).sum::<f32>() / (s.bufsize / 2) as f32;
        assert!(mean.abs() < 1.0, "left channel mean {mean}");
        assert!(
            s.current_vol > 10.0 && s.base() == 155,
            "vol {} base {}",
            s.current_vol,
            s.base()
        );
        assert!((adjust_rate_to_fps(0.9, 30.0, 60.0) - 0.9487).abs() < 1e-3);
    }
}
