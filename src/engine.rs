//! Plumbing shared by the original CPU engines: the `CpuEngine` lifecycle the frame loop drives
//! and the frame clock (smoothed fps, beat gate, auto-switch timer) every engine keeps.

use std::time::Instant;

use crate::milkdrop::audio::Audio;

/// Beat when the bass jumps this much over its smoothed level.
const BEAT: f32 = 1.4;
/// Seconds between two beats.
const BEAT_HOLD: f32 = 0.25;

/// The original CPU engines: same lifecycle, rendered through `Gpu::blit_rgba`.
pub trait CpuEngine {
    /// Stereo frames `step` wants per call.
    fn frames_needed(&self) -> usize;
    /// Renders one frame from the latest interleaved stereo `pcm`; returns RGBA8 pixels.
    fn step(&mut self, pcm: &[f32]) -> &[u8];
    /// Next figure / palette right away (key binding).
    fn next(&mut self);
}

/// Frame clock: seconds since the last frame, smoothed fps, frame count, beat gate and the
/// auto-switch timer.
pub struct Clock {
    /// Seconds since the previous frame, clamped to `1/240..=0.5`.
    pub dt: f32,
    /// Smoothed frames per second.
    pub fps: f32,
    /// Frames rendered so far.
    pub frame: u64,
    last: Instant,
    since_beat: f32,
    since_switch: f32,
    duration: f32,
}

impl Clock {
    /// A clock whose `tick` asks for a switch every `duration` seconds.
    pub fn new(duration: f32) -> Self {
        Self {
            dt: 1.0 / 60.0,
            fps: 60.0,
            frame: 0,
            last: Instant::now(),
            since_beat: 0.0,
            since_switch: 0.0,
            duration,
        }
    }

    /// Advances one frame; `true` when `duration` has elapsed since the last `reset_switch`.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        self.dt = now
            .duration_since(self.last)
            .as_secs_f32()
            .clamp(1.0 / 240.0, 0.5);
        self.last = now;
        self.fps = self.fps * 0.95 + (1.0 / self.dt) * 0.05;
        self.frame += 1;
        self.since_beat += self.dt;
        self.since_switch += self.dt;
        self.since_switch >= self.duration
    }

    /// Restarts the auto-switch timer (call from `next`).
    pub fn reset_switch(&mut self) {
        self.since_switch = 0.0;
    }

    /// Per-frame factor `r`, tuned at 30 fps, adjusted to the measured fps.
    pub fn rate(&self, r: f32) -> f32 {
        r.powf(30.0 / self.fps.clamp(15.0, 144.0))
    }

    /// `true` when the bass jumps over its smoothed level, at most once per `BEAT_HOLD`.
    pub fn beat(&mut self, audio: &Audio) -> bool {
        let beat = self.since_beat >= BEAT_HOLD && audio.level[0] > BEAT * audio.att[0];
        if beat {
            self.since_beat = 0.0;
        }
        beat
    }
}
