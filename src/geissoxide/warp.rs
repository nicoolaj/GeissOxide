//! The per-frame image warp (`Process_Map`): each destination pixel is the weighted sum of a
//! 2×2 block of the source, weights `>> 8` — so weight sums below 256 fade the picture.

use super::map::MapEntry;

/// Warps `src` into `dst` through `map`; `width` is the row stride.
pub fn process(src: &[u8], dst: &mut [u8], map: &[MapEntry], width: usize) {
    for (d, e) in dst.iter_mut().zip(map) {
        let o = e.offset as usize;
        let sum = u32::from(e.w[0]) * u32::from(src[o])
            + u32::from(e.w[1]) * u32::from(src[o + 1])
            + u32::from(e.w[2]) * u32::from(src[o + width])
            + u32::from(e.w[3]) * u32::from(src[o + width + 1]);
        *d = (sum >> 8) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_map_fades_and_offset_map_shifts() {
        let w = 8;
        // Rows 0..6 map to themselves; the last two rows stay black (as the hidden bands do).
        let identity: Vec<MapEntry> = (0..w * 8)
            .map(|i| {
                if i < w * 6 {
                    MapEntry {
                        offset: i as u32,
                        w: [255, 0, 0, 0],
                    }
                } else {
                    MapEntry::default()
                }
            })
            .collect();
        let src = vec![200u8; w * 8];
        let mut dst = vec![0u8; w * 8];
        process(&src, &mut dst, &identity, w);
        assert_eq!(dst[10], ((200u32 * 255) >> 8) as u8);

        let mut shifted = identity.clone();
        shifted[10].offset = 11;
        let mut src = vec![0u8; w * 8];
        src[11] = 255;
        process(&src, &mut dst, &shifted, w);
        assert_eq!(dst[10], 254);
        assert_eq!(dst[9], 0);
    }
}
