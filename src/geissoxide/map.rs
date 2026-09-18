//! Warp maps: for every pixel, where to sample the previous frame and with which bilinear
//! weights. Port of the 25 "modes" of `GenerateChunkOfNewMap` (original `main.cpp`).

use rand::{Rng, RngExt};

/// Number of warp modes; modes are numbered 1..=NUM_MODES like the original.
pub const NUM_MODES: u8 = 25;

/// Base weight sum; anything below 256 makes the image fade a little every frame.
const WEIGHTSUM: u32 = 253;

/// One destination pixel: top-left source offset and the four `>>8` bilinear weights
/// (top-left, top-right, bottom-left, bottom-right).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapEntry {
    pub offset: u32,
    pub w: [u8; 4],
}

/// A point charge of the mode-6 vortex field.
#[derive(Clone, Copy, Debug, Default)]
struct Charge {
    x: f32,
    y: f32,
    i: f32,
    j: f32,
    kind: u8,
}

/// Everything a map depends on, chosen once per map (`MapParams::random`).
#[derive(Clone, Debug)]
pub struct MapParams {
    pub mode: u8,
    pub width: usize,
    pub height: usize,
    /// Rows hidden at the top and bottom (`FX_YCUT`).
    pub y_cut: usize,
    /// Centre of the effect (`gXC`, `gYC`).
    pub cx: i32,
    pub cy: i32,
    scale1: f32,
    scale2: f32,
    turn1: f32,
    turn2: f32,
    f1: f32,
    f2: f32,
    f3: f32,
    charges: [Charge; 10],
    /// Blend between "stay put" and "follow the map" (`new_damping_temp`).
    damping: f32,
    weightsum: u32,
    seed: u64,
    nuclide: bool,
}

/// Modes whose motion is halved (`mode_motion_dampened`), indexed by mode.
const DAMPENED: [bool; 26] = [
    true, true, true, false, true, true, false, true, true, true, true, true, true, true, true,
    true, true, false, false, false, false, false, false, false, false, false,
];
/// Modes alternating two rotations in a checkerboard (`rotation_dither`).
const ROTATION_DITHER: [bool; 26] = {
    let mut t = [false; 26];
    t[1] = true;
    t[9] = true;
    t[11] = true;
    t
};

impl MapParams {
    /// Random parameters for `mode` at `width`×`height`, tuned for `fps` (motion is scaled so
    /// the map moves at the same speed as the original did at 30 fps). `nuclide` is whether the
    /// nuclide effect is currently running (mode 5 changes shape with it, as in the original).
    pub fn random(
        mode: u8,
        width: usize,
        height: usize,
        y_cut: usize,
        fps: f32,
        nuclide: bool,
        rng: &mut impl Rng,
    ) -> Self {
        let r = |rng: &mut dyn rand::Rng, lo: f32, range: f32| {
            lo + range * rng.random_range(0..1000) as f32 * 0.001
        };
        let protective = if width > 640 {
            640.0 / width as f32
        } else {
            1.0
        };
        let (mut scale1, mut scale2, mut turn1, mut turn2) = (1.0, 1.0, 0.0, 0.0);
        let mut f3 = 0.0;
        let mut charges = [Charge::default(); 10];
        let mut weightsum = WEIGHTSUM as f32;
        match mode {
            1 => {
                scale1 = 0.985 - 0.12 * (rng.random_range(0..1000) as f32 * 0.001).powi(2);
                scale2 = scale1;
                turn1 = r(rng, 0.01, 0.01);
                turn2 = turn1;
                if scale1 > 0.97 && rng.random_range(0..3) == 1 {
                    turn1 = -turn1;
                }
            }
            2 => {
                scale1 = r(rng, 1.0, 0.02);
                turn1 = r(rng, 0.02, 0.07);
            }
            3 => {
                scale1 = r(rng, 0.85, 0.1);
                scale2 = scale1;
                turn1 = r(rng, 0.01, 0.015);
                turn2 = turn1;
            }
            4 | 13 | 14 | 16 => {
                turn1 = r(rng, 0.007, 0.02);
                turn2 = turn1;
            }
            5 => {
                turn1 = r(rng, 0.01, 0.03);
                turn2 = turn1;
            }
            6 => {
                for c in &mut charges {
                    let d = rng.random_range(0..628) as f32 * 0.01;
                    let f = 1.0 + rng.random_range(0..80) as f32 * 0.01;
                    *c = Charge {
                        x: rng.random_range(0..width * 10) as f32 * 0.1,
                        y: y_cut as f32
                            + rng.random_range(0..(height - y_cut * 2) * 10) as f32 * 0.1,
                        i: d.cos() * f,
                        j: d.sin() * f,
                        kind: rng.random_range(0..3),
                    };
                }
            }
            7 => {
                turn1 = r(rng, 0.01, 0.01);
                turn2 = turn1;
            }
            8 => {
                turn1 = r(rng, 0.0, 0.05);
                turn2 = turn1;
            }
            9 => {
                scale1 = (r(rng, 0.8, 0.25) - 1.0) * protective + 1.0;
                scale2 = scale1;
                turn1 = r(rng, 0.01, 0.03);
                turn2 = turn1;
            }
            11 => {
                scale1 = r(rng, 1.008, 0.008);
                scale2 = scale1;
                turn1 = r(rng, 0.12, 0.06);
                turn2 = turn1;
                turn1 *= -0.6;
                turn2 *= 0.1;
                scale1 *= 0.99;
                scale2 *= 1.01;
            }
            12 => weightsum *= 0.98,
            15 => {
                turn1 = r(rng, 0.0, 0.04) + r(rng, 0.0, 0.045);
                turn2 = turn1;
            }
            _ => {
                turn1 = r(rng, 0.007, 0.02);
                turn2 = turn1;
            }
        }
        if rng.random_range(0..2) == 1 {
            turn1 = -turn1;
            turn2 = -turn2;
        }
        // f1..f3: per-mode shape parameters.
        let (f1, f2) = match mode {
            5 => (
                r(rng, 0.05, 0.05) + r(rng, 0.0, 0.07),
                0.99 - r(rng, 0.0, 0.01) - r(rng, 0.0, 0.02),
            ),
            7 => (r(rng, 0.92, 0.01), r(rng, 0.0006, 0.0005)),
            8 => (r(rng, 0.0, 1.0).powi(4) * 8.0 + 1.5, 0.0),
            9 => (r(rng, 0.98, 0.01), r(rng, 0.0009, 0.0012)),
            13 => (r(rng, 0.92, 0.16), 0.0),
            15 => {
                f3 = r(rng, 0.05, 0.05);
                (rng.random_range(2..7) as f32, r(rng, 0.92, 0.06)) // petals, scale
            }
            _ => (r(rng, 0.92, 0.05), r(rng, 0.0009, 0.0012)),
        };
        turn1 *= 0.6;
        turn2 *= 0.6;

        let pixels = width * height;
        let res_adjust = match pixels {
            p if p <= 320 * 240 => 250.0,
            p if p <= 400 * 300 => 251.0,
            p if p <= 512 * 384 => 252.0,
            p if p <= 800 * 600 => 253.0,
            p if p <= 1280 * 960 => 254.0,
            _ => 255.0,
        };
        let mut damping: f32 = if DAMPENED[usize::from(mode)] {
            0.5
        } else {
            1.0
        };
        if (10.0..=120.0).contains(&fps) {
            damping *= 30.0 / fps;
        }
        Self {
            mode,
            width,
            height,
            y_cut,
            cx: width as i32 / 2 - 1 + rng.random_range(-30..30),
            cy: height as i32 / 2 - 1 + rng.random_range(-15..15),
            scale1,
            scale2,
            turn1,
            turn2,
            f1,
            f2,
            f3,
            charges,
            damping,
            weightsum: (weightsum * res_adjust / 256.0) as u32,
            seed: rng.random(),
            nuclide,
        }
    }
}

/// Builds the map: one entry per pixel; rows outside `[y_cut, height - y_cut)` get zero weights.
pub fn generate(p: &MapParams) -> Vec<MapEntry> {
    use rand::SeedableRng;
    let mut rng = rand::rngs::SmallRng::seed_from_u64(p.seed);
    let (w, h) = (p.width, p.height);
    let (fw, fh) = (w as f32, h as f32);
    let rmult = 640.0 / fw;
    let protective = if w > 640 { 640.0 / fw } else { 1.0 };
    let inv_fw = 2.0 / fw;
    let half_fw = if p.mode <= 16 { 1.0 } else { 0.5 * fw };
    let (cos1, sin1, cos2, sin2) = (p.turn1.cos(), p.turn1.sin(), p.turn2.cos(), p.turn2.sin());
    let min_offset = (w * 2) as u32;
    let max_offset = (w * (h - 3) - 1) as u32;
    let mut map = vec![MapEntry::default(); w * h];

    for y in p.y_cut..h - p.y_cut {
        for x in 0..w {
            let (xf, yf) = (x as f32, y as f32);
            let mut dx = xf - p.cx as f32;
            let mut dy = yf - p.cy as f32;
            let mut scale1 = p.scale1;
            let (mut cos1, mut sin1) = (cos1, sin1);
            let rad = (dx * dx + dy * dy).sqrt();
            if p.mode <= 16 {
                let r = rad * rmult;
                scale1 = match p.mode {
                    3 => 0.95 - (dy * (480.0 / fh)) * 0.0005,
                    4 => 0.9 + r * 0.0025 * 0.14,
                    5 => {
                        let r = rad * (1.0 / 200.0) * rmult;
                        let r = if p.nuclide { r * 1.7 } else { r.sqrt() };
                        (p.f2 - p.f1 * r - 1.0) * protective + 1.0
                    }
                    7 => {
                        let r = rad * p.f2 * rmult;
                        (p.f1 - r - 1.0) * protective
                            + 1.0
                            + rng.random_range(0..100) as f32 * 0.0005
                    }
                    8 => 0.85 + 0.1 * (r.sqrt() * p.f1).sin(),
                    9 => (p.f1 - rad * p.f2 * rmult - 1.0) * protective + 1.0,
                    13 => (1.04 - r * r.sqrt() * 0.00025 * 0.14 - 1.0) * p.f1 + 1.0,
                    14 => {
                        0.9 + 0.2
                            * (dy * 12.0 / (fh + rng.random_range(0..1024) as f32 / 1024.0)).cos()
                    }
                    15 => p.f2 + p.f3 * (dy.atan2(dx) * p.f1).sin(),
                    16 => (1.05 - r * r * 0.00025 * 0.09).max(-1.5),
                    _ => scale1,
                };
            } else {
                dx *= inv_fw;
                dy *= inv_fw;
                let r = (dx * dx + dy * dy).sqrt();
                scale1 = match p.mode {
                    17 => 0.97 - dy * dy * 0.40,
                    18 => 0.97 - dx * dx * 0.40,
                    19 => 1.04 - 0.25 * r,
                    20 => 1.15 - (dy + 1.4).sqrt() * 0.20,
                    21 => {
                        0.95 - (dx.abs() * 10.0).floor() * 0.03 - (dy.abs() * 10.0).floor() * 0.03
                    }
                    22 => 0.95 - (r * 10.0).floor() * 0.04,
                    23 => 0.95 - ((r * 20.0) as i32 % 4) as f32 * 0.12,
                    24 => {
                        cos1 = 0.05_f32.cos();
                        sin1 = 0.05_f32.sin();
                        0.96
                    }
                    _ => 3.0 / (3.0 + r), // 25: 1/r zoom
                };
            }

            let (mut nx, mut ny) = match p.mode {
                6 => {
                    let (mut tx, mut ty, mut f) = (0.0, 0.0, 0.0);
                    for c in &p.charges[..5] {
                        let (ex, ey) = (c.x - xf, c.y - yf);
                        let xxyy = ex * ex + ey * ey;
                        let d = 1.0 / (xxyy + 0.1);
                        f += d;
                        match c.kind {
                            0 => {
                                tx += c.i * d;
                                ty += c.j * d;
                            }
                            kind => {
                                let z = 1.0 / (xxyy.sqrt() + 0.01);
                                let d = d * 2.0;
                                let sign = if kind == 1 { 1.0 } else { -1.0 };
                                tx += d * (-ey) * z * sign;
                                ty += d * ex * z * sign;
                            }
                        }
                    }
                    if f > 0.000_001 {
                        let f = 1.9 / f;
                        (xf + tx * f - 0.1, yf + ty * f + 0.6)
                    } else {
                        (xf - 0.1, yf + 0.6)
                    }
                }
                10 => (dx * (1.03 + 0.03 * (yf / fh)) + p.cx as f32, yf * 1.04),
                12 => {
                    let nx = if dx < -0.5 {
                        -(-dx).sqrt() + p.cx as f32 + 0.9
                    } else if dx > 0.5 {
                        dx.sqrt() + p.cx as f32 - 0.9
                    } else {
                        p.cx as f32
                    };
                    (nx, dy + p.cy as f32)
                }
                _ => {
                    let (c, s, scale) =
                        if ROTATION_DITHER[usize::from(p.mode)] && (x % 2) != (y % 2) {
                            (cos2, sin2, p.scale2)
                        } else {
                            (cos1, sin1, scale1)
                        };
                    let rx = dx * c - dy * s;
                    let ry = dx * s + dy * c;
                    (
                        rx * scale * half_fw + p.cx as f32,
                        ry * scale * half_fw + p.cy as f32,
                    )
                }
            };

            nx = xf * (1.0 - p.damping) + nx * p.damping;
            ny = yf * (1.0 - p.damping) + ny * p.damping;
            nx = nx.rem_euclid(fw - 1.0);
            ny = ny.clamp(0.0, fh - 1.0);

            let (a, b) = (nx as i32, ny as i32);
            let offset = (b * w as i32 + a).clamp(min_offset as i32, max_offset as i32) as u32;
            let (fx, fy) = (nx - a as f32, ny - b as f32);
            let ws = p.weightsum as f32;
            map[y * w + x] = MapEntry {
                offset,
                w: [
                    ((1.0 - fx) * (1.0 - fy) * ws) as u8,
                    (fx * (1.0 - fy) * ws) as u8,
                    ((1.0 - fx) * fy * ws) as u8,
                    (fx * fy * ws) as u8,
                ],
            };
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_yields_in_bounds_offsets_and_decaying_weights() {
        let (w, h) = (320, 240);
        let mut rng = rand::rng();
        for mode in 1..=NUM_MODES {
            let params = MapParams::random(mode, w, h, 4, 60.0, mode % 2 == 0, &mut rng);
            let map = generate(&params);
            assert_eq!(map.len(), w * h);
            for (i, e) in map.iter().enumerate() {
                let y = i / w;
                let sum: u32 = e.w.iter().map(|&v| u32::from(v)).sum();
                if y < 4 || y >= h - 4 {
                    assert_eq!(sum, 0, "mode {mode}: hidden row {y} must be black");
                    continue;
                }
                assert!(
                    sum > 0 && sum < 256,
                    "mode {mode} pixel {i}: weight sum {sum}"
                );
                assert!(
                    (e.offset as usize) + w + 1 < w * h,
                    "mode {mode} pixel {i}: offset {}",
                    e.offset
                );
            }
        }
    }
}
