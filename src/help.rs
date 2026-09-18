//! The on-screen help panel: the `app.help_keys` text rasterized with an 8×8 pixel font on a
//! smoky (translucent black) background, composited by `Gpu::overlay_rgba`.

use font8x8::{BASIC_FONTS, LATIN_FONTS, UnicodeFonts};

/// Padding around the text, in glyph cells.
const PAD: u32 = 1;
/// Line pitch in pixels: 8 glyph rows plus a gap.
const LINE: u32 = 10;
/// Panel background: 80 % black keeps white text readable over any engine output.
const BG: [u8; 4] = [0, 0, 0, 204];
const FG: [u8; 4] = [255, 255, 255, 255];
const SHADOW: [u8; 4] = [0, 0, 0, 255];

/// Rasterizes `text` (one line per `|`- or newline-separated entry) with glyphs scaled `scale`×.
/// Returns `(width, height, rgba)`.
pub fn panel(text: &str, scale: u32) -> (u32, u32, Vec<u8>) {
    let scale = scale.max(1);
    let lines: Vec<&str> = text.split(['|', '\n']).map(str::trim).collect();
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u32;
    let (w, h) = (
        (cols + 2 * PAD) * 8 * scale,
        (lines.len() as u32 * LINE + 2 * PAD * 8) * scale,
    );
    let mut rgba = BG.repeat((w * h) as usize);
    let mut block = |x: u32, y: u32, c: [u8; 4]| {
        for dy in 0..scale {
            for dx in 0..scale {
                let (px, py) = (x + dx, y + dy);
                if px < w && py < h {
                    let i = ((py * w + px) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(&c);
                }
            }
        }
    };
    for (row, line) in lines.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            let glyph = BASIC_FONTS
                .get(ch)
                .or_else(|| LATIN_FONTS.get(ch))
                .unwrap_or([0; 8]);
            let (x0, y0) = ((PAD + col as u32) * 8, PAD * 8 + row as u32 * LINE);
            for (y, bits) in glyph.iter().enumerate() {
                for x in (0..8).filter(|x| bits & (1 << x) != 0) {
                    let (px, py) = ((x0 + x) * scale, (y0 + y as u32) * scale);
                    block(px + scale, py + scale, SHADOW);
                    block(px, py, FG);
                }
            }
        }
    }
    (w, h, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_has_background_and_glyphs() {
        let (w, h, rgba) = panel("A | Bé", 2);
        assert_eq!((w, h), ((2 + 2) * 16, (2 * 10 + 16) * 2));
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert_eq!(&rgba[..4], &BG);
        assert!(rgba.chunks(4).any(|p| p == FG));
    }
}
