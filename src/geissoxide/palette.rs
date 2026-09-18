//! 256-entry palettes for the 8-bit frame buffer. Port of `FX_Random_Palette`, `CrankPal`
//! and `PutPalette` from the original `video.h`.

use rand::{Rng, RngExt};

/// One RGB triple per pixel value.
pub type Palette = [[u8; 3]; 256];

/// Frames over which a new palette is blended in (`iBlendsLeftInPal`).
pub const BLEND_FRAMES: u32 = 18;

/// Percentage chance (0-10) of a coarse palette; original slider default.
const COARSE_PAL_FREQ: u32 = 3;
/// Chance out of 5 of allowing the "solar" curve 7; original slider default.
const SOLAR_PAL_FREQ: u32 = 2;
/// 8-bit gamma correction slider default (0-100).
const GAMMA: f32 = 0.0;

/// Curve `curve_id` (1-7) evaluated at `z` (`CrankPal`).
fn crank(curve_id: u32, z: u8) -> f32 {
    let x = f32::from(z);
    match curve_id {
        1 => x.sqrt() * 22.6,
        2 => x * 2.0,
        3 => x * x / 64.0,
        4 => 255.0 * (x / 256.0 * 0.5 * std::f32::consts::PI).sin(),
        5 => x * 3.5,
        6 => 1.5_f32.powf(x / 20.0) - 1.0, // really dark
        7 => x * 1.5 + 128.0 * 0.25 + 128.0 * 0.25 * (x * 0.3).sin(),
        _ => 255.0,
    }
}

/// Picks a random palette: either one of the four monotone "FX" palettes or three random curves.
pub fn random(rng: &mut impl Rng, silent: bool) -> Palette {
    let mut pal = [[0u8; 3]; 256];
    if rng.random_range(0..6) == 0 {
        // Monotone palette from the original FX: the three channel curves are permuted.
        let curves: [fn(f32) -> f32; 3] = [|a| a * a / 64.0, |a| a * 2.0, |a| a.sqrt() * 22.6];
        let order = [[0, 1, 2], [0, 2, 1], [2, 1, 0], [1, 0, 2]][rng.random_range(0..4)];
        for (n, entry) in pal.iter_mut().enumerate() {
            let a = n.min(127) as f32;
            *entry = [
                curves[order[0]](a),
                curves[order[2]](a),
                curves[order[1]](a),
            ]
            .map(|v| v as u8);
        }
        return pal;
    }
    let solar = rng.random_range(0..5) < SOLAR_PAL_FREQ;
    let max_curve = if solar { 7 } else { 6 };
    let mut curves = [0u32; 3];
    loop {
        curves = curves.map(|_| rng.random_range(1..=max_curve));
        // Disallow really dark palettes (at most one channel on curve 6).
        if curves.iter().filter(|&&c| c == 6).count() <= 1 {
            break;
        }
    }
    let (lo_band, hi_band) = if rng.random_range(0..10) < COARSE_PAL_FREQ {
        (rng.random_range(7..13), rng.random_range(17..23))
    } else {
        (-1, -1)
    };
    let gamma_factor = 1.0 + GAMMA * 0.01 + if silent { 0.3 } else { 0.0 };
    for (n, entry) in pal.iter_mut().enumerate() {
        let band = (n as i32) > lo_band && (n as i32) < hi_band;
        let boost = gamma_factor * if band { 2.0 } else { 1.0 };
        let [r, b, g] = curves.map(|c| (crank(c, n as u8) * boost).min(255.0) as u8);
        *entry = [r, g, b];
    }
    pal
}

/// A random palette whose bright end is red-dominant (`warm`) or blue-dominant (used by the
/// original engines to tell two layers apart).
pub fn random_tinted(rng: &mut impl Rng, warm: bool) -> Palette {
    for _ in 0..50 {
        let pal = random(rng, false);
        let [r, g, b] = pal[200];
        if if warm { r > g && r > b } else { b > r && b > g } {
            return pal;
        }
    }
    random(rng, false)
}

/// Black → `tint` → white over the 256 entries (phosphor screens, neon lines).
pub fn ramp(tint: [f32; 3]) -> Palette {
    std::array::from_fn(|i| {
        let t = i as f32 / 255.0;
        tint.map(|c| {
            let v = if t < 0.6 {
                c * t / 0.6
            } else {
                c + (1.0 - c) * (t - 0.6) / 0.4
            };
            (v * 255.0) as u8
        })
    })
}

/// Linear blend between two palettes, `t` in `0.0..=1.0` (`PutPalette`).
pub fn blend(from: &Palette, to: &Palette, t: f32) -> Palette {
    let mut out = [[0u8; 3]; 256];
    for ((o, a), b) in out.iter_mut().zip(from).zip(to) {
        for c in 0..3 {
            o[c] = (f32::from(a[c]) * (1.0 - t) + f32::from(b[c]) * t) as u8;
        }
    }
    out
}

/// A palette that blends toward a new one over `BLEND_FRAMES` (`iBlendsLeftInPal`).
pub struct Fade {
    current: Palette,
    from: Palette,
    to: Palette,
    left: u32,
}

impl Fade {
    /// A fade resting on `palette`.
    pub fn new(palette: Palette) -> Self {
        Self {
            current: palette,
            from: palette,
            to: palette,
            left: 0,
        }
    }

    /// Starts blending from the current palette to `palette`.
    pub fn to(&mut self, palette: Palette) {
        self.from = self.current;
        self.to = palette;
        self.left = BLEND_FRAMES;
    }

    /// Advances the blend by one frame and returns the palette to draw with.
    pub fn tick(&mut self) -> &Palette {
        if self.left > 0 {
            self.left -= 1;
            let t = 1.0 - self.left as f32 / BLEND_FRAMES as f32;
            self.current = blend(&self.from, &self.to, t);
        }
        &self.current
    }
}

/// Maps an 8-bit index buffer through `pal` into RGBA8 (alpha untouched).
pub fn apply(pal: &Palette, idx: &[u8], rgba: &mut [u8]) {
    for (px, &i) in rgba.chunks_exact_mut(4).zip(idx) {
        px[..3].copy_from_slice(&pal[usize::from(i)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_are_in_range_and_blend_hits_endpoints() {
        let mut rng = rand::rng();
        for _ in 0..50 {
            let pal = random(&mut rng, false);
            assert!(pal[200] != [0, 0, 0], "bright entries must not be black");
        }
        let a = random(&mut rng, false);
        let b = random(&mut rng, true);
        assert_eq!(blend(&a, &b, 0.0), a);
        assert_eq!(blend(&a, &b, 1.0), b);
        let mut fade = Fade::new(a);
        fade.to(b);
        for _ in 0..BLEND_FRAMES {
            fade.tick();
        }
        assert_eq!(*fade.tick(), b);
        let mut rgba = [255u8; 8];
        apply(&b, &[0, 200], &mut rgba);
        assert_eq!(&rgba[4..7], &b[200]);
        assert_eq!(rgba[7], 255);
    }
}
