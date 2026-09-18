//! `.milk` preset files (INI-like), following `CState::Import` in MilkDrop's `state.cpp`.
//! Numeric keys become the initial values of the per-frame script variables.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context as _, Result};

/// `(.milk key, script variable, default)` for every value the scripts can read or write.
pub const VARS: &[(&str, &str, f64)] = &[
    ("fDecay", "decay", 0.98),
    ("fGammaAdj", "gamma", 2.0),
    ("fVideoEchoZoom", "echo_zoom", 2.0),
    ("fVideoEchoAlpha", "echo_alpha", 0.0),
    ("nVideoEchoOrientation", "echo_orient", 0.0),
    ("bBrighten", "brighten", 0.0),
    ("bDarken", "darken", 0.0),
    ("bSolarize", "solarize", 0.0),
    ("bInvert", "invert", 0.0),
    ("nWaveMode", "wave_mode", 0.0),
    ("bAdditiveWaves", "wave_additive", 0.0),
    ("bWaveDots", "wave_usedots", 0.0),
    ("bWaveThick", "wave_thick", 0.0),
    ("bMaximizeWaveColor", "wave_brighten", 1.0),
    ("fWaveAlpha", "wave_a", 0.8),
    ("fWaveParam", "wave_mystery", 0.0),
    ("wave_r", "wave_r", 1.0),
    ("wave_g", "wave_g", 1.0),
    ("wave_b", "wave_b", 1.0),
    ("wave_x", "wave_x", 0.5),
    ("wave_y", "wave_y", 0.5),
    ("nMotionVectorsX", "mv_x", 12.0),
    ("nMotionVectorsY", "mv_y", 9.0),
    ("mv_dx", "mv_dx", 0.0),
    ("mv_dy", "mv_dy", 0.0),
    ("mv_l", "mv_l", 0.9),
    ("mv_r", "mv_r", 1.0),
    ("mv_g", "mv_g", 1.0),
    ("mv_b", "mv_b", 1.0),
    ("mv_a", "mv_a", 0.0),
    ("zoom", "zoom", 1.0),
    ("rot", "rot", 0.0),
    ("cx", "cx", 0.5),
    ("cy", "cy", 0.5),
    ("dx", "dx", 0.0),
    ("dy", "dy", 0.0),
    ("warp", "warp", 1.0),
    ("sx", "sx", 1.0),
    ("sy", "sy", 1.0),
    ("bTexWrap", "wrap", 1.0),
    ("bDarkenCenter", "darken_center", 0.0),
    ("fZoomExponent", "zoomexp", 1.0),
    ("ob_size", "ob_size", 0.01),
    ("ob_r", "ob_r", 0.0),
    ("ob_g", "ob_g", 0.0),
    ("ob_b", "ob_b", 0.0),
    ("ob_a", "ob_a", 0.0),
    ("ib_size", "ib_size", 0.01),
    ("ib_r", "ib_r", 0.25),
    ("ib_g", "ib_g", 0.25),
    ("ib_b", "ib_b", 0.25),
    ("ib_a", "ib_a", 0.0),
];

/// A custom waveform (`wavecode_N_*`, `wave_N_*`).
#[derive(Clone, Debug)]
pub struct Wave {
    pub enabled: bool,
    pub samples: usize,
    pub sep: usize,
    pub spectrum: bool,
    pub dots: bool,
    pub thick: bool,
    pub additive: bool,
    pub scaling: f64,
    pub smoothing: f64,
    pub rgba: [f64; 4],
    pub init: String,
    pub per_frame: String,
    pub per_point: String,
}

/// A custom shape (`shapecode_N_*`, `shape_N_*`).
#[derive(Clone, Debug)]
pub struct Shape {
    pub enabled: bool,
    pub sides: u32,
    pub additive: bool,
    pub thick: bool,
    pub textured: bool,
    pub num_inst: u32,
    pub x: f64,
    pub y: f64,
    pub rad: f64,
    pub ang: f64,
    pub tex_ang: f64,
    pub tex_zoom: f64,
    pub rgba: [f64; 4],
    pub rgba2: [f64; 4],
    pub border: [f64; 4],
    pub init: String,
    pub per_frame: String,
}

/// One parsed preset.
#[derive(Clone, Debug)]
pub struct Preset {
    pub name: String,
    /// Initial value of every script variable in `VARS`, in the same order.
    pub vars: Vec<f64>,
    pub wave_scale: f64,
    pub wave_smoothing: f64,
    pub mod_wave_alpha_by_volume: bool,
    pub mod_wave_alpha_start: f64,
    pub mod_wave_alpha_end: f64,
    pub warp_anim_speed: f64,
    pub warp_scale: f64,
    pub per_frame_init: String,
    pub per_frame: String,
    pub per_pixel: String,
    pub waves: Vec<Wave>,
    pub shapes: Vec<Shape>,
    /// The preset carries MilkDrop 2 pixel shaders (which this engine cannot run).
    pub has_shaders: bool,
}

impl Preset {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read(path).with_context(|| path.display().to_string())?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::parse(&name, &String::from_utf8_lossy(&text))
    }

    /// Parses the text of a `.milk` file.
    pub fn parse(name: &str, text: &str) -> Result<Self> {
        let map: HashMap<String, &str> = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim_end_matches('\r')))
            .collect();
        let num = |key: &str, default: f64| -> f64 {
            map.get(&key.to_ascii_lowercase())
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(default)
        };
        let flag = |key: &str, default: bool| num(key, f64::from(default)) != 0.0;
        let code = |prefix: &str| -> String {
            let prefix = prefix.to_ascii_lowercase();
            let mut lines: Vec<(u32, &str)> = map
                .iter()
                .filter_map(|(k, v)| {
                    k.strip_prefix(&prefix)
                        .and_then(|n| n.parse().ok())
                        .map(|n| (n, *v))
                })
                .collect();
            lines.sort_unstable();
            // Like MilkDrop: drop `//` / `\\` comments, then concatenate the lines with nothing
            // in between (some presets split a token across two lines).
            lines
                .iter()
                .map(|(_, l)| {
                    let l = l.strip_prefix('`').unwrap_or(l);
                    let end = l.find("//").or_else(|| l.find("\\\\")).unwrap_or(l.len());
                    &l[..end]
                })
                .collect::<String>()
        };
        let waves = (0..4)
            .map(|i| {
                let k = |s: &str| format!("wavecode_{i}_{s}");
                Wave {
                    enabled: flag(&k("enabled"), false),
                    samples: num(&k("samples"), 512.0) as usize,
                    sep: num(&k("sep"), 0.0) as usize,
                    spectrum: flag(&k("bSpectrum"), false),
                    dots: flag(&k("bUseDots"), false),
                    thick: flag(&k("bDrawThick"), false),
                    additive: flag(&k("bAdditive"), false),
                    scaling: num(&k("scaling"), 1.0),
                    smoothing: num(&k("smoothing"), 0.5),
                    rgba: [
                        num(&k("r"), 1.0),
                        num(&k("g"), 1.0),
                        num(&k("b"), 1.0),
                        num(&k("a"), 1.0),
                    ],
                    init: code(&format!("wave_{i}_init")),
                    per_frame: code(&format!("wave_{i}_per_frame")),
                    per_point: code(&format!("wave_{i}_per_point")),
                }
            })
            .collect();
        let shapes = (0..4)
            .map(|i| {
                let k = |s: &str| format!("shapecode_{i}_{s}");
                Shape {
                    enabled: flag(&k("enabled"), false),
                    sides: num(&k("sides"), 4.0) as u32,
                    additive: flag(&k("additive"), false),
                    thick: flag(&k("thickOutline"), false),
                    textured: flag(&k("textured"), false),
                    num_inst: num(&k("num_inst"), 1.0).max(1.0) as u32,
                    x: num(&k("x"), 0.5),
                    y: num(&k("y"), 0.5),
                    rad: num(&k("rad"), 0.1),
                    ang: num(&k("ang"), 0.0),
                    tex_ang: num(&k("tex_ang"), 0.0),
                    tex_zoom: num(&k("tex_zoom"), 1.0),
                    rgba: [
                        num(&k("r"), 1.0),
                        num(&k("g"), 0.0),
                        num(&k("b"), 0.0),
                        num(&k("a"), 1.0),
                    ],
                    rgba2: [
                        num(&k("r2"), 0.0),
                        num(&k("g2"), 1.0),
                        num(&k("b2"), 0.0),
                        num(&k("a2"), 0.0),
                    ],
                    border: [
                        num(&k("border_r"), 1.0),
                        num(&k("border_g"), 1.0),
                        num(&k("border_b"), 1.0),
                        num(&k("border_a"), 0.1),
                    ],
                    init: code(&format!("shape_{i}_init")),
                    per_frame: code(&format!("shape_{i}_per_frame")),
                }
            })
            .collect();
        let mut vars: Vec<f64> = VARS
            .iter()
            .map(|&(key, _, default)| num(key, default))
            .collect();
        // Old presets switch motion vectors on with a flag instead of an alpha.
        if flag("bMotionVectorsOn", false) && !map.contains_key("mv_a") {
            vars[VARS.iter().position(|v| v.1 == "mv_a").unwrap_or(0)] = 1.0;
        }
        Ok(Self {
            name: name.to_owned(),
            vars,
            wave_scale: num("fWaveScale", 1.0),
            wave_smoothing: num("fWaveSmoothing", 0.75),
            mod_wave_alpha_by_volume: flag("bModWaveAlphaByVolume", false),
            mod_wave_alpha_start: num("fModWaveAlphaStart", 0.75),
            mod_wave_alpha_end: num("fModWaveAlphaEnd", 0.95),
            warp_anim_speed: num("fWarpAnimSpeed", 1.0),
            warp_scale: num("fWarpScale", 1.0),
            per_frame_init: code("per_frame_init_"),
            per_frame: code("per_frame_"),
            per_pixel: code("per_pixel_"),
            waves,
            shapes,
            has_shaders: map.contains_key("warp_1") || map.contains_key("comp_1"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[preset00]\r\nfRating=4.0\r\nfDecay=0.96\r\nnWaveMode=7\r\nzoom=1.02\r\n\
        wavecode_1_enabled=1\r\nwavecode_1_samples=64\r\nshapecode_2_enabled=1\r\nshapecode_2_sides=6\r\n\
        per_frame_init_1=q1=0.5;\r\nper_frame_1=`zoom = zoom + 0.01*bass;\r\nper_frame_2=rot = rot + 0.1;\r\n\
        per_pixel_1=zoom = zoom + rad*0.1;\r\nwave_1_per_point1=x = sample;\r\n";

    #[test]
    fn parses_values_code_and_shader_flag() {
        let p = Preset::parse("sample", SAMPLE).unwrap();
        let var = |name: &str| p.vars[VARS.iter().position(|v| v.1 == name).unwrap()];
        assert_eq!(var("decay"), 0.96);
        assert_eq!(var("wave_mode"), 7.0);
        assert_eq!(var("zoom"), 1.02);
        assert_eq!(var("cx"), 0.5); // default
        assert_eq!(p.per_frame, "zoom = zoom + 0.01*bass;rot = rot + 0.1;");
        assert_eq!(p.per_frame_init, "q1=0.5;");
        assert_eq!(p.per_pixel, "zoom = zoom + rad*0.1;");
        assert!(
            p.waves[1].enabled && p.waves[1].samples == 64 && p.waves[1].per_point == "x = sample;"
        );
        assert!(!p.waves[0].enabled && p.shapes[2].enabled && p.shapes[2].sides == 6);
        assert!(!p.has_shaders);
        assert!(
            Preset::parse("s", "warp_1=`shader_body{}")
                .unwrap()
                .has_shaders
        );
    }
}
