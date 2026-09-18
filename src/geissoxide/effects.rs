//! The optional overlays of `RenderFX` / `RenderDots` (original `Effects.h`, 8-bit paths):
//! chasers, bar, dotty chaser, solar particles, grid, nuclide blobs and shade bobs.

use rand::{Rng, RngExt};

use super::raster::Canvas;
use super::sound::Sound;
use super::wave::Layout;

const CHASERS: usize = 0;
const BAR: usize = 1;
const DOTS: usize = 2;
const SOLAR: usize = 3;
const GRID: usize = 4;
const NUCLIDE: usize = 5;
const SHADE: usize = 6;
const NUM_EFFECTS: usize = 7;

/// Per-mode tuning: chance (out of 1000) of each effect, solar particle budget, and how many
/// effects may run at once (`modeInfo[]` in `FX_Init`).
struct ModeInfo {
    freq: [u32; NUM_EFFECTS],
    solar_max: u32,
    min_effects: usize,
    max_effects: usize,
}

fn mode_info(mode: u8) -> ModeInfo {
    let (freq, solar_max, min_effects, max_effects) = match mode {
        1 => ([220, 150, 10, 680, 4, 170, 400], 400, 1, 2),
        2 => ([750, 500, 750, 750, 0, 0, 0], 35, 1, 5),
        3 => ([100, 100, 100, 500, 10, 0, 300], 60, 1, 2),
        4 | 16 => ([500, 100, 100, 100, 30, 0, 0], 34, 1, 2),
        5 => ([100, 350, 100, 500, 15, 180, 500], 60, 1, 2),
        6 => ([400, 120, 200, 0, 0, 0, 0], 60, 1, 2),
        7 => ([50, 200, 0, 300, 0, 600, 350], 65, 1, 2),
        8 => ([150, 150, 150, 150, 25, 0, 0], 60, 1, 2),
        9 => ([450, 200, 50, 200, 0, 100, 200], 50, 1, 2),
        10 => ([150, 20, 80, 0, 0, 80, 0], 0, 0, 2),
        11 => ([360, 200, 230, 550, 10, 330, 150], 750, 0, 4),
        12 => ([360, 200, 230, 0, 0, 330, 0], 500, 0, 2),
        13 | 14 => ([500, 0, 100, 0, 30, 0, 0], 34, 1, 2),
        15 => ([0, 0, 0, 0, 0, 200, 0], 60, 0, 1),
        _ => ([125, 150, 150, 150, 12, 125, 50], 600, 1, 3),
    };
    ModeInfo {
        freq,
        solar_max,
        min_effects,
        max_effects,
    }
}

/// The effects chosen for one map, plus their animation state.
pub struct Effects {
    active: [bool; NUM_EFFECTS],
    /// 1 or 2 chasers (`effect[CHASERS]`).
    chasers: u8,
    solar_max: u32,
    grid_dir: i32,
    chaser_offset: f32,
    /// Shade bob parameters (`micro_c*`, `micro_f*`, `micro_rad`).
    bob: [f32; 11],
    /// Dotty chaser trail: x, y, brightness of the last 20 positions.
    trail: [(i32, i32, u8); 20],
    trail_ptr: usize,
}

impl Effects {
    /// Rolls the effects for `mode` (`GenerateChunkOfNewMap` apply section + `Clip_Num_Effects`).
    pub fn pick(mode: u8, sound_active: bool, rng: &mut impl Rng) -> Self {
        let info = mode_info(mode);
        let mut active = [false; NUM_EFFECTS];
        for (i, on) in active.iter_mut().enumerate() {
            let thresh = if sound_active {
                info.freq[i] * 7 / 10
            } else {
                info.freq[i]
            };
            *on = rng.random_range(0..1000) < thresh;
        }
        let count = |a: &[bool]| a.iter().filter(|&&b| b).count();
        if !sound_active {
            while count(&active) < info.min_effects {
                let j = rng.random_range(0..NUM_EFFECTS);
                if !active[j] && rng.random_range(0..1000) < info.freq[j] {
                    active[j] = true;
                }
            }
        }
        while count(&active) > info.max_effects {
            let j = rng.random_range(0..NUM_EFFECTS);
            if active[j] && info.freq[j] < 1000 {
                active[j] = false;
            }
        }
        if active[GRID] {
            active[BAR] = false;
        }
        let r = |rng: &mut dyn Rng, lo: f32, range: f32| {
            lo + range * rng.random_range(0..1000) as f32 * 0.001
        };
        let bob = [
            r(rng, 0.08, 0.09),
            r(rng, 0.08, 0.09),
            r(rng, 0.08, 0.09),
            r(rng, 0.1, 0.05),
            r(rng, 0.1, 0.05),
            r(rng, 0.1, 0.05),
            r(rng, 0.1, 0.05),
            r(rng, 2.0, 2.8),
            r(rng, 2.0, 2.8),
            r(rng, 2.0, 2.8),
            r(rng, 2.0, 2.8),
        ];
        Self {
            active,
            chasers: 1 + rng.random_range(0..2),
            solar_max: info.solar_max,
            grid_dir: rng.random_range(0..2) * 2 - 1,
            chaser_offset: rng.random_range(0..40_000) as f32,
            bob,
            trail: [(1, 1, 0); 20],
            trail_ptr: 0,
        }
    }

    pub fn nuclide(&self) -> bool {
        self.active[NUCLIDE]
    }

    /// The nuclide effect prefers no waveform (`waveform = 0` 4 times out of 7).
    pub fn hides_wave(&self, rng: &mut impl Rng) -> bool {
        self.active[NUCLIDE] && rng.random_range(0..7) > 2
    }

    /// Sun burst when mode 1 starts (`Drop_Solar_Particles(500)` half of the time).
    pub fn initial_burst(
        &self,
        mode: u8,
        canvas: &mut Canvas,
        layout: &Layout,
        rng: &mut impl Rng,
    ) {
        if mode == 1 && rng.random_range(0..2) == 0 {
            solar_particles(canvas, layout, 500, rng);
        }
    }

    /// Draws the effects into the frame that is about to be warped (`RenderFX`).
    #[allow(clippy::too_many_arguments)]
    pub fn before_warp(
        &mut self,
        canvas: &mut Canvas,
        layout: &Layout,
        sound: &Sound,
        floatframe: f32,
        intframe: u64,
        fps: f32,
        rng: &mut impl Rng,
    ) {
        let time_scale = if (10.0..120.0).contains(&fps) {
            30.0 / fps
        } else {
            1.0
        };
        let s = canvas.width as f32 / 640.0;
        let (cx, cy) = (layout.cx as f32, layout.cy as f32);
        if self.active[SHADE] {
            let b = &self.bob;
            let mut a = layout.cx
                + (b[7] * (floatframe * b[3]).cos() + b[9] * (floatframe * b[4]).cos()) as i32;
            let mut y = layout.cy
                + (b[8] * (floatframe * b[5]).cos() + b[10] * (floatframe * b[6]).cos()) as i32;
            for _ in 0..4 {
                y += rng.random_range(-2..=2);
                a += rng.random_range(-2..=2);
                if y > layout.y_cut && y < canvas.height as i32 - 1 - layout.y_cut {
                    canvas.add(a, y, 2, 250);
                    for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                        canvas.add(a + dx, y + dy, 1, 250);
                    }
                }
            }
        }
        if self.active[CHASERS] {
            let mut t = (floatframe + self.chaser_offset) * time_scale;
            let k2 = (20.0 * s) as i32;
            for _ in 0..k2 {
                t += 0.08 * time_scale * 20.0 / k2 as f32;
                for pass in 0..self.chasers {
                    let (a, b) = if pass == 0 {
                        (
                            cx + s * 74.0 * (t * 0.1102 + 10.0).cos()
                                + s * 65.0 * (t * 0.1312 + 20.0).cos(),
                            cy + s * 54.0 * (t * 0.1204 + 40.0).cos()
                                + s * 55.0 * (t * 0.1715 + 30.0).cos(),
                        )
                    } else {
                        (
                            cx + s * 64.0 * (t * 0.1213 + 33.0).cos()
                                + s * 55.0 * (t * 0.1408 + 15.0).cos(),
                            cy + s * 52.0 * (t * 0.1304 + 12.0).cos()
                                + s * 51.0 * (t * 0.1103 + 21.0).cos(),
                        )
                    };
                    if let Some(p) = canvas.get_mut(a as i32, b as i32) {
                        *p = (255.0 - (255.0 - f32::from(*p)) * 0.6) as u8;
                    }
                }
            }
        }
        if self.active[BAR] {
            let frame = (floatframe + self.chaser_offset * 0.6) * 0.55 / (0.08 * 20.0);
            let x1 = cx
                + s * 16.0 * (frame * 0.1102 + 10.0).cos()
                + s * 15.0 * (frame * 0.1312 + 20.0).cos();
            let y1 = cy
                + s * 15.0 * (frame * 0.1204 + 40.0).cos()
                + s * 10.0 * (frame * 0.1715 + 30.0).cos();
            let x2 = cx
                + s * 14.0 * (frame * 0.1213 + 33.0).cos()
                + s * 13.0 * (frame * 0.1408 + 15.0).cos();
            let y2 = cy
                + s * 13.0 * (frame * 0.1304 + 12.0).cos()
                + s * 11.0 * (frame * 0.1103 + 21.0).cos();
            let k2 = (s * 50.0) as i32;
            for k in 0..k2 {
                let f = k as f32 / k2 as f32;
                canvas.add(
                    (x1 * f + x2 * (1.0 - f)) as i32,
                    (y1 * f + y2 * (1.0 - f)) as i32,
                    16,
                    223,
                );
            }
        }
        if self.active[DOTS] {
            let t = floatframe * time_scale;
            let a = layout.cx
                + (s * 64.0 * (t * 0.0613 + 33.0).cos() + s * 55.0 * (t * 0.0708 + 15.0).cos())
                    as i32;
            let b = layout.cy
                + (s * 52.0 * (t * 0.0704 + 12.0).cos() + s * 51.0 * (t * 0.0503 + 21.0).cos())
                    as i32;
            if b > layout.y_cut && b < canvas.height as i32 - 1 - layout.y_cut {
                self.trail_ptr = (self.trail_ptr + 1) % 20;
                self.trail[self.trail_ptr] =
                    (a, b, (127.0 + 126.0 * (t * 0.0613 + 33.0).sin()) as u8);
                let fat = canvas.width >= 1050;
                for dot in &mut self.trail {
                    canvas.set(dot.0, dot.1, dot.2);
                    if fat {
                        canvas.set(dot.0 + 1, dot.1, dot.2);
                        canvas.set(dot.0, dot.1 + 1, dot.2);
                        canvas.set(dot.0 + 1, dot.1 + 1, dot.2);
                    }
                    dot.0 += 1;
                }
            }
        }
        if self.active[NUCLIDE] && sound.silent && rng.random_range(0..12) == 0 {
            let r = 3 + rng.random_range(0..8);
            let rad = 34 + rng.random_range(0..8);
            nuclide_blobs(canvas, layout, r, rad, rng);
        }
        if self.active[GRID] {
            grid(canvas, layout, intframe, fps, self.grid_dir);
        }
        if self.active[SOLAR] {
            let f = intframe as f32;
            let m = self.solar_max as f32;
            let fx =
                3.0 + m * 1.6 + m * 0.43 * (f * 0.05).sin() + m * 0.43 * (f * 0.038 + 1.0).sin();
            solar_particles(canvas, layout, (fx * 0.05) as u32, rng);
        }
    }

    /// Sound-triggered nuclide blobs, drawn after the warp (`RenderDots`).
    pub fn after_warp(
        &self,
        canvas: &mut Canvas,
        layout: &Layout,
        sound: &Sound,
        rng: &mut impl Rng,
    ) {
        if !self.active[NUCLIDE] || sound.silent || sound.current_vol <= sound.avg_vol_narrow * 1.1
        {
            return;
        }
        let r =
            (3.0 + 40.0 * (sound.current_vol / sound.avg_vol_narrow - 1.1)).clamp(1.0, 10.0) as i32;
        let mut rad = 34 + rng.random_range(0..8);
        if canvas.width > 1024 {
            rad = rad * canvas.width as i32 / 1024;
        }
        nuclide_blobs(canvas, layout, r, rad, rng);
    }
}

/// A ring of `3..8` soft blobs of radius `r` around the centre.
fn nuclide_blobs(canvas: &mut Canvas, layout: &Layout, r: i32, rad: i32, rng: &mut impl Rng) {
    let nodes = 3 + rng.random_range(0..5);
    let phase = rng.random_range(0..1000) as f32;
    for n in 0..nodes {
        let a = n as f32 / nodes as f32 * std::f32::consts::TAU + phase;
        let (cx, cy) = (
            layout.cx + (rad as f32 * a.cos()) as i32,
            layout.cy + (rad as f32 * a.sin()) as i32,
        );
        if cy - 10 <= layout.y_cut || cy + 10 >= canvas.height as i32 - 1 - layout.y_cut {
            continue;
        }
        for y in -10..10 {
            for x in -10..10 {
                let val = (r as f32 - ((x * x + y * y) as f32).sqrt()) * 25.0;
                if val > 0.0 {
                    canvas.add(cx + x, cy + y, val as u8, 255);
                }
            }
        }
    }
}

/// Sparkles near the centre (`Drop_Solar_Particles`, 8-bit: four times `n`).
fn solar_particles(canvas: &mut Canvas, layout: &Layout, n: u32, rng: &mut impl Rng) {
    for _ in 0..n * 4 {
        let (mut x, mut y, mut dist) = (0, 0, 100.0);
        while dist >= 35.0 {
            y = rng.random_range(0..72) - 36;
            x = rng.random_range(0..96) - 48;
            dist = ((x * x + y * y) as f32).sqrt();
        }
        let (px, py) = (layout.cx + x, layout.cy + y);
        if py <= layout.y_cut || py >= canvas.height as i32 - 1 - layout.y_cut {
            continue;
        }
        let i0 = 2 + rng.random_range(0..2) + ((35.0 - dist) / 9.0) as i32;
        let (i1, i2) = (i0 - 1, i0 - 2);
        if canvas
            .get_mut(px, py)
            .is_some_and(|p| i32::from(*p) < 207 - i0)
        {
            canvas.add(px, py, i0 as u8, 255);
            for (dx, dy, v) in [
                (1, 0, i1),
                (-1, 0, i1),
                (0, 1, i1),
                (0, -1, i1),
                (-1, -1, i2),
                (1, -1, i2),
                (-1, 1, i2),
                (1, 1, i2),
            ] {
                canvas.add(px + dx, py + dy, v.max(0) as u8, 255);
            }
        }
    }
}

/// A slowly pulsing grid of dots that scrolls sideways (`Grid`).
fn grid(canvas: &mut Canvas, layout: &Layout, intframe: u64, fps: f32, dir: i32) {
    let inc = (canvas.width / 30) as i32;
    let f = intframe as f32 * 30.0 / fps;
    let s = (65.0
        + 45.0 * (f * 0.06033).sin()
        + 35.0 * (f * 0.04710 + 1.0).cos()
        + 25.0 * (f * 0.00523 - 1.0).cos())
    .max(0.0) as u8;
    let shift = (intframe as i32 % inc) * -dir;
    let fat = canvas.width >= 1700;
    let mut y = layout.y_cut;
    while y < canvas.height as i32 - layout.y_cut {
        let mut x = 0;
        while x < canvas.width as i32 {
            canvas.plot_max(x + shift, y, s);
            if fat {
                canvas.plot_max(x + shift + 1, y, s);
                canvas.plot_max(x + shift, y + 1, s);
                canvas.plot_max(x + shift + 1, y + 1, s);
            }
            x += inc;
        }
        y += inc;
    }
}
