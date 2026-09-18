//! The sound-reactive drawing: waveforms 1-6 of `RenderWave` and `Diminish_Center`.

use super::raster::Canvas;
use super::sound::Sound;

/// Number of stereo samples blended at the seam of the circular waveform (`WAVE_5_BLEND_RANGE`).
const WAVE_5_BLEND_RANGE: usize = 50;

/// Geometry shared by the waveforms.
pub struct Layout {
    pub cx: i32,
    pub cy: i32,
    /// Hidden rows at top and bottom (`FX_YCUT_HIDE`).
    pub y_cut: i32,
    pub mode: u8,
}

/// Draws waveform `waveform` (0 = none, 1..=5) with brightness `c`.
pub fn render(canvas: &mut Canvas, sound: &mut Sound, waveform: u8, layout: &Layout, c: u8) {
    let (w, h) = (canvas.width as i32, canvas.height as i32);
    let (lo, hi) = (layout.y_cut, h - layout.y_cut);
    // At high resolutions the wave is stretched by interpolating extra samples.
    let passes = match waveform {
        1 | 2 if w >= 1920 => 2,
        1 | 2 if w > 1024 => 1,
        _ if w >= 1440 => 1,
        _ => 0,
    };
    for _ in 0..passes {
        let src = sound.wave.clone();
        let (mut sl, mut sr) = (src[0], src[1]);
        for i in (0..(w as usize) * 2).step_by(2) {
            let tl = src[i + 2] * 1.14;
            let tr = src[i + 3] * 1.14;
            let p = &mut sound.wave[i * 2..i * 2 + 4];
            p.copy_from_slice(&[sl, sr, 0.5 * (sl + tl), 0.5 * (sr + tr)]);
            sl = tl;
            sr = tr;
        }
    }
    let wave = &sound.wave;
    let sample = |i: usize| wave[i & !1];
    // Original smoothing: z = prev*0.9 + new*0.1.
    let smooth = |prev: f32, next: f32| prev * 0.9 + next * 0.1;
    match waveform {
        1 => {
            let (mut start, mut end, mut y_center) = (0, w, layout.cy);
            if layout.mode == 10 {
                y_center = ((h - layout.y_cut) + h / 2) / 2;
                start += if w >= 640 { 15 } else { 10 };
                end -= if w >= 640 { 15 } else { 10 };
            }
            let mut z = sample(start as usize) + y_center as f32;
            for i in start..end {
                z = smooth(z, sample(i as usize) + y_center as f32);
                let y = z as i32;
                if y >= lo && y < hi {
                    canvas.plot_max(i, y, c);
                }
            }
        }
        2 => {
            let (h1, h2) = (
                layout.cy as f32 - h as f32 * 0.12,
                layout.cy as f32 + h as f32 * 0.12,
            );
            let mut zl = wave[0] * 0.7 + h1;
            let mut zr = wave[1] * 0.7 + h2;
            for i in 0..w {
                zl = smooth(zl, sample(i as usize) * 0.7 + h1);
                zr = smooth(zr, wave[(i as usize & !1) + 1] * 0.7 + h2);
                for y in [zl as i32, zr as i32] {
                    if y > lo && y < hi {
                        canvas.plot_max(i, y, c);
                    }
                }
            }
        }
        3 => {
            let mut z = sample(lo as usize) + layout.cx as f32;
            for i in lo..hi {
                z = smooth(z, sample(i as usize) + layout.cx as f32);
                canvas.plot_max(z as i32, i, c);
            }
        }
        4 => {
            let mut zl = sample(lo as usize) * 0.9;
            let mut zr = wave[(lo as usize & !1) + 1] * 0.9;
            for i in lo..hi {
                zl = smooth(zl, sample(i as usize) * 0.9);
                zr = smooth(zr, wave[(i as usize & !1) + 1] * 0.9);
                canvas.plot_max(zl as i32 + i, i, c);
                canvas.plot_max(zr as i32 + i + (w - h), i, c);
            }
        }
        5 => {
            // Circle: blend the seam so the ring closes.
            let range_inv = 1.0 / WAVE_5_BLEND_RANGE as f32;
            for i in 0..WAVE_5_BLEND_RANGE {
                let amt = i as f32 * range_inv;
                let j = i & !1;
                let k = (i + 314) & !1;
                sound.wave[j] = sound.wave[j] * amt + (1.0 - amt) * sound.wave[k];
            }
            let wave = &sound.wave;
            let base_rad = if w == 320 {
                40.0
            } else {
                w as f32 / 640.0 * 60.0
            };
            let mut rad = base_rad + wave[0] * 0.7;
            for i in 0..314 {
                rad = rad * 0.5 + 0.5 * (base_rad + wave[i & !1] * 0.7);
                if rad >= 5.0 {
                    let a = i as f32 * 0.02;
                    let (px, py) = (
                        layout.cx as f32 + rad * a.cos(),
                        layout.cy as f32 + rad * a.sin(),
                    );
                    if (py as i32) >= lo && (py as i32) < hi {
                        canvas.plot_max(px as i32, py as i32, c);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Keeps the warp centre from saturating (`Diminish_Center`); `factor` is `center_dwindle`.
pub fn diminish_center(canvas: &mut Canvas, layout: &Layout, factor: f32) {
    if factor >= 0.999 {
        return;
    }
    if layout.mode == 12 {
        for y in layout.y_cut..canvas.height as i32 - layout.y_cut {
            for x in layout.cx - 1..=layout.cx + 1 {
                canvas.dim(x, y, factor);
            }
        }
    } else {
        for (dx, dy) in [(0, 0), (-1, 0), (1, 0), (0, 1), (0, -1)] {
            canvas.dim(layout.cx + dx, layout.cy + dy, factor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveforms_only_touch_visible_rows() {
        let (w, h) = (320usize, 240usize);
        let mut sound = Sound::new(w);
        let pcm: Vec<f32> = (0..sound.frames_needed() * 2)
            .map(|i| ((i / 2) as f32 * 0.3).sin())
            .collect();
        sound.update(&pcm, 60.0, 400.0);
        let layout = Layout {
            cx: 160,
            cy: 120,
            y_cut: 12,
            mode: 1,
        };
        for waveform in 1..=5 {
            let mut buf = vec![0u8; w * h];
            let mut canvas = Canvas {
                buf: &mut buf,
                width: w,
                height: h,
            };
            render(&mut canvas, &mut sound, waveform, &layout, 150);
            let lit: Vec<usize> = buf
                .iter()
                .enumerate()
                .filter(|&(_, &v)| v > 0)
                .map(|(i, _)| i / w)
                .collect();
            assert!(!lit.is_empty(), "waveform {waveform} drew nothing");
            assert!(
                lit.iter().all(|&y| (12..228).contains(&y)),
                "waveform {waveform} drew in hidden rows"
            );
        }
    }
}
