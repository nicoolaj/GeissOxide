//! The GeissOxide engine: an 8-bit frame buffer warped through a precomputed map every frame,
//! with the sound wave drawn on top and a random palette. Follows `render1()` of the original.

mod effects;
mod map;
pub mod palette;
pub mod raster;
mod sound;
mod warp;
mod wave;

use std::sync::mpsc;
use std::time::Instant;

use rand::{Rng, RngExt};

use effects::Effects;
use map::{MapEntry, MapParams, NUM_MODES};
use palette::Palette;
use raster::Canvas;
use sound::Sound;
use wave::Layout;

/// Number of waveform styles (`NUM_WAVES`).
const NUM_WAVES: u32 = 5;
/// Frames a map stays before the next one may replace it, at 30 fps (`frames_til_auto_switch`).
const FRAMES_TIL_AUTO_SWITCH: f32 = 400.0;
/// How often each mode is picked, index = mode (`default_modeprefs`).
const MODE_WEIGHTS: [u32; 26] = [
    0, 3, 3, 3, 3, 5, 5, 5, 5, 5, 3, 3, 1, 3, 3, 2, 3, 4, 3, 3, 3, 3, 3, 3, 3, 3,
];
/// Per-mode dimming of the warp centre (`center_dwindle`), index = mode.
const CENTER_DWINDLE: [f32; 26] = [
    1.0, 1.0, 1.0, 0.99, 0.98, 0.99, 1.0, 0.985, 0.96, 0.985, 1.0, 1.0, 0.915, 0.98, 0.98, 1.0,
    0.98, 1.0, 1.0, 1.0, 0.98, 0.98, 0.98, 0.98, 1.0, 1.0,
];

/// A running GeissOxide visualizer.
pub struct GeissOxide {
    width: usize,
    height: usize,
    y_cut: usize,
    vs1: Vec<u8>,
    vs2: Vec<u8>,
    map: Vec<MapEntry>,
    params: MapParams,
    waveform: u8,
    pending: Option<mpsc::Receiver<(MapParams, Vec<MapEntry>)>>,
    next: Option<(MapParams, Vec<MapEntry>)>,
    palette: Palette,
    palette_from: Palette,
    palette_to: Palette,
    blends_left: u32,
    sound: Sound,
    effects: Effects,
    rng: rand::rngs::ThreadRng,
    floatframe: f32,
    intframe: u64,
    frames_this_mode: f32,
    frames_til_auto_switch: f32,
    fps: f32,
    last_frame: Instant,
    rgba: Vec<u8>,
}

impl GeissOxide {
    /// Creates an engine rendering at `width`×`height` (internal resolution).
    pub fn new(width: usize, height: usize) -> Self {
        let mut rng = rand::rng();
        // The original hides `max(4, (65 - 0.65 * size%) / 200 * height)` rows; size% is 100 here.
        let y_cut = 4;
        let params = MapParams::random(
            Self::pick_mode(&mut rng),
            width,
            height,
            y_cut,
            60.0,
            false,
            &mut rng,
        );
        let map = map::generate(&params);
        let palette = palette::random(&mut rng, false);
        let effects = Effects::pick(params.mode, false, &mut rng);
        let mut engine = Self {
            width,
            height,
            y_cut,
            vs1: vec![0; width * height],
            vs2: vec![0; width * height],
            waveform: Self::pick_waveform(params.mode, &effects, &mut rng),
            map,
            params,
            pending: None,
            next: None,
            palette,
            palette_from: palette,
            palette_to: palette,
            blends_left: 0,
            sound: Sound::new(width),
            effects,
            rng,
            floatframe: 0.0,
            intframe: 0,
            frames_this_mode: 0.0,
            frames_til_auto_switch: FRAMES_TIL_AUTO_SWITCH * 2.0,
            fps: 60.0,
            last_frame: Instant::now(),
            rgba: vec![255; width * height * 4],
        };
        engine.start_next_map();
        engine
    }

    /// Stereo frames `step` wants per call.
    pub fn frames_needed(&self) -> usize {
        self.sound.frames_needed()
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
        self.frames_this_mode += 1.0;
        self.floatframe += 1.6 * (47.0 / self.fps).min(1.0);
        self.intframe += 1;

        let layout = Layout {
            cx: self.params.cx,
            cy: self.params.cy,
            y_cut: self.y_cut as i32,
            mode: self.params.mode,
        };
        let mut canvas = Canvas {
            buf: &mut self.vs1,
            width: self.width,
            height: self.height,
        };
        self.effects.before_warp(
            &mut canvas,
            &layout,
            &self.sound,
            self.floatframe,
            self.intframe,
            self.fps,
            &mut self.rng,
        );
        wave::diminish_center(
            &mut canvas,
            &layout,
            CENTER_DWINDLE[usize::from(self.params.mode)],
        );

        warp::process(&self.vs1, &mut self.vs2, &self.map, self.width);

        self.sound
            .update(pcm, self.fps, self.frames_til_auto_switch);
        let base = self.sound.base();
        let mut canvas = Canvas {
            buf: &mut self.vs2,
            width: self.width,
            height: self.height,
        };
        self.effects
            .after_warp(&mut canvas, &layout, &self.sound, &mut self.rng);
        wave::render(&mut canvas, &mut self.sound, self.waveform, &layout, base);
        std::mem::swap(&mut self.vs1, &mut self.vs2);

        self.schedule_map();
        self.blend_palette();
        // Only the rows between the hidden bands are shown, as in the original.
        let visible = self.width * self.y_cut..self.width * (self.height - self.y_cut);
        for (n, (px, &i)) in self.rgba.chunks_exact_mut(4).zip(&self.vs1).enumerate() {
            let rgb = if visible.contains(&n) {
                self.palette[usize::from(i)]
            } else {
                [0; 3]
            };
            px[..3].copy_from_slice(&rgb);
        }
        &self.rgba
    }

    /// Forces a new map right away (key binding).
    pub fn next_map(&mut self) {
        self.frames_this_mode = self.frames_til_auto_switch;
        self.sound.beat_mode = false;
    }

    fn pick_mode(rng: &mut impl Rng) -> u8 {
        let total: u32 = MODE_WEIGHTS.iter().sum();
        let mut a = rng.random_range(0..total);
        for (mode, &w) in MODE_WEIGHTS.iter().enumerate().skip(1) {
            if a < w {
                return mode as u8;
            }
            a -= w;
        }
        NUM_MODES
    }

    /// Random waveform, de-emphasising the last one and avoiding bad mode/wave pairs; some
    /// modes and the nuclide effect override the choice, as in the original.
    fn pick_waveform(mode: u8, effects: &Effects, rng: &mut impl Rng) -> u8 {
        let w = loop {
            let w = (rng.random_range(0..NUM_WAVES * 3 - 1) / 3 + 1) as u8;
            if !matches!((mode, w), (6, 5) | (12, 4) | (14, 3) | (14, 4)) {
                break w;
            }
        };
        match mode {
            _ if effects.hides_wave(rng) => 0,
            10 => 1,
            15 if rng.random_range(0..5) == 0 => 5,
            _ => w,
        }
    }

    /// Generates the next map on a background thread.
    fn start_next_map(&mut self) {
        let params = MapParams::random(
            Self::pick_mode(&mut self.rng),
            self.width,
            self.height,
            self.y_cut,
            self.fps,
            self.effects.nuclide(),
            &mut self.rng,
        );
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let map = map::generate(&params);
            let _ = tx.send((params, map));
        });
        self.pending = Some(rx);
        self.sound.big_beat_threshold = 1.10;
    }

    /// Applies the next map once it is ready, the current one has been shown long enough and
    /// (when a beat is on) a big beat hits — waiting less and less patiently.
    fn schedule_map(&mut self) {
        if let Some(rx) = &self.pending
            && let Ok(ready) = rx.try_recv()
        {
            self.next = Some(ready);
            self.pending = None;
        }
        if self.next.is_none() || self.frames_this_mode < self.frames_til_auto_switch {
            return;
        }
        if self.sound.beat_mode && !self.sound.big_beat {
            self.sound.big_beat_threshold -= 0.2 / self.frames_til_auto_switch;
            return;
        }
        let Some((params, map)) = self.next.take() else {
            return;
        };
        self.effects = Effects::pick(params.mode, !self.sound.silent, &mut self.rng);
        self.waveform = Self::pick_waveform(params.mode, &self.effects, &mut self.rng);
        self.params = params;
        self.map = map;
        self.frames_this_mode = 0.0;
        self.frames_til_auto_switch = if (10.0..120.0).contains(&self.fps) {
            FRAMES_TIL_AUTO_SWITCH * self.fps / 30.0
        } else {
            FRAMES_TIL_AUTO_SWITCH
        };
        // Clear the hidden bands so the new mode starts clean.
        let band = self.width * self.y_cut;
        for vs in [&mut self.vs1, &mut self.vs2] {
            vs[..band].fill(0);
            let len = vs.len();
            vs[len - band..].fill(0);
        }
        let layout = Layout {
            cx: self.params.cx,
            cy: self.params.cy,
            y_cut: self.y_cut as i32,
            mode: self.params.mode,
        };
        let mut canvas = Canvas {
            buf: &mut self.vs1,
            width: self.width,
            height: self.height,
        };
        self.effects
            .initial_burst(self.params.mode, &mut canvas, &layout, &mut self.rng);
        self.palette_from = self.palette;
        self.palette_to = palette::random(&mut self.rng, self.sound.silent);
        self.blends_left = palette::BLEND_FRAMES;
        self.start_next_map();
    }

    fn blend_palette(&mut self) {
        if self.blends_left > 0 {
            self.blends_left -= 1;
            let t = 1.0 - self.blends_left as f32 / palette::BLEND_FRAMES as f32;
            self.palette = palette::blend(&self.palette_from, &self.palette_to, t);
        }
    }
}
