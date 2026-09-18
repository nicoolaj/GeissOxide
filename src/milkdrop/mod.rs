//! MilkDrop 1 engine: `.milk` presets, NS-EEL scripts and a wgpu renderer. The frame
//! structure and geometry follow `milkdropfs.cpp` (as ported by butterchurn, MIT).

pub mod audio;
pub mod eel;
pub mod preset;
pub mod render;

use std::f32::consts::{PI, TAU};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow};
use rand::seq::SliceRandom;
use rust_i18n::t;
use wgpu::PrimitiveTopology as Topo;

use crate::gpu::Gpu;
use audio::{Audio, SAMPLES};
use eel::{Context, Memory, Program};
use preset::{Preset, VARS};
use render::{Blend, CompParams, Draw, Renderer, Vertex};

/// Warp mesh resolution (MilkDrop's default `nMeshSize`).
const MESH: (usize, usize) = (48, 36);
const NUM_Q: usize = 32;
const NUM_T: usize = 8;
/// Global (script-visible) inputs shared by every scope.
const GLOBALS: [&str; 10] = [
    "time", "fps", "frame", "progress", "bass", "mid", "treb", "bass_att", "mid_att", "treb_att",
];

/// Scripts and script state of a custom wave or shape.
struct Scripted {
    ctx: Context,
    per_frame: Program,
    per_point: Program,
    t_after_init: [f64; NUM_T],
}

/// A loaded preset with its compiled scripts.
struct Loaded {
    preset: Preset,
    pf: Context,
    per_frame: Program,
    pv: Context,
    per_pixel: Program,
    q_after_init: [f64; NUM_Q],
    monitor_after_init: f64,
    waves: Vec<Scripted>,
    shapes: Vec<Scripted>,
}

/// The running MilkDrop visualizer.
pub struct MilkDrop {
    renderer: Renderer,
    audio: Audio,
    playlist: Vec<PathBuf>,
    index: usize,
    pub locked: bool,
    loaded: Option<Loaded>,
    gmem: Memory,
    start: Instant,
    last_frame: Instant,
    preset_start: f64,
    duration: f64,
    frame: u64,
    fps: f32,
    aspect: (f32, f32),
}

impl MilkDrop {
    /// Scans `presets_dir` for `.milk` files (shuffled) and loads the first one.
    pub fn new(
        gpu: &Gpu,
        width: u32,
        height: u32,
        sample_rate: u32,
        presets_dir: &Path,
        allow_shaders: bool,
        duration: f64,
    ) -> Result<Self> {
        let mut rng = rand::rng();
        let mut playlist: Vec<PathBuf> = std::fs::read_dir(presets_dir)
            .with_context(|| presets_dir.display().to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("milk"))
            })
            .filter(|p| allow_shaders || Preset::load(p).is_ok_and(|p| !p.has_shaders))
            .collect();
        if playlist.is_empty() {
            return Err(anyhow!(t!(
                "milkdrop.no_presets",
                dir = presets_dir.display()
            )));
        }
        playlist.shuffle(&mut rng);
        let (w, h) = (width as f32, height as f32);
        let mut md = Self {
            renderer: Renderer::new(gpu, width, height),
            audio: Audio::new(sample_rate),
            playlist,
            index: 0,
            locked: false,
            loaded: None,
            gmem: Memory::new(),
            start: Instant::now(),
            last_frame: Instant::now(),
            preset_start: 0.0,
            duration,
            frame: 0,
            fps: 60.0,
            aspect: (
                if h > w { w / h } else { 1.0 },
                if w > h { h / w } else { 1.0 },
            ),
        };
        md.load_current();
        Ok(md)
    }

    /// Stereo frames `render` wants per call.
    pub fn frames_needed(&self) -> usize {
        audio::FFT_SIZE
    }

    pub fn next_preset(&mut self) {
        self.index = (self.index + 1) % self.playlist.len();
        self.load_current();
    }

    pub fn prev_preset(&mut self) {
        self.index = (self.index + self.playlist.len() - 1) % self.playlist.len();
        self.load_current();
    }

    /// Loads `playlist[index]`, skipping (and dropping) presets that fail to parse or compile.
    fn load_current(&mut self) {
        while !self.playlist.is_empty() {
            let path = self.playlist[self.index].clone();
            match Preset::load(&path).and_then(|p| self.compile(p)) {
                Ok(loaded) => {
                    eprintln!("{}", t!("milkdrop.loaded", name = loaded.preset.name));
                    self.loaded = Some(loaded);
                    self.preset_start = self.start.elapsed().as_secs_f64();
                    return;
                }
                Err(e) => {
                    log::warn!(
                        "{}",
                        t!(
                            "milkdrop.preset_error",
                            name = path.display(),
                            error = format!("{e:#}")
                        )
                    );
                    self.playlist.remove(self.index);
                    if !self.playlist.is_empty() {
                        self.index %= self.playlist.len();
                    }
                }
            }
        }
        self.loaded = None;
    }

    fn compile(&mut self, preset: Preset) -> Result<Loaded> {
        let mut pf = Context::default();
        for (&(_, name, _), &value) in VARS.iter().zip(&preset.vars) {
            pf.set(name, value);
        }
        self.set_globals(&mut pf);
        let per_frame_init = compile_or_warn(&preset.name, &preset.per_frame_init, &mut pf);
        let per_frame = compile_or_warn(&preset.name, &preset.per_frame, &mut pf);
        per_frame_init.run(&mut pf, &mut self.gmem);
        let q_after_init = std::array::from_fn(|i| pf.get(&format!("q{}", i + 1)));
        let monitor_after_init = pf.get("monitor");
        let mut pv = Context::default();
        let per_pixel = compile_or_warn(&preset.name, &preset.per_pixel, &mut pv);

        let mut scripted = |init: &str, frame: &str, point: &str| -> Result<Scripted> {
            let mut ctx = Context::default();
            self.set_globals(&mut ctx);
            for (i, q) in q_after_init.iter().enumerate() {
                ctx.set(&format!("q{}", i + 1), *q);
            }
            let init = compile_or_warn(&preset.name, init, &mut ctx);
            let per_frame = compile_or_warn(&preset.name, frame, &mut ctx);
            let per_point = compile_or_warn(&preset.name, point, &mut ctx);
            init.run(&mut ctx, &mut self.gmem);
            let t_after_init = std::array::from_fn(|i| ctx.get(&format!("t{}", i + 1)));
            Ok(Scripted {
                ctx,
                per_frame,
                per_point,
                t_after_init,
            })
        };
        let waves = preset
            .waves
            .iter()
            .map(|w| scripted(&w.init, &w.per_frame, &w.per_point))
            .collect::<Result<Vec<_>>>()?;
        let shapes = preset
            .shapes
            .iter()
            .map(|s| scripted(&s.init, &s.per_frame, ""))
            .collect::<Result<Vec<_>>>()?;
        Ok(Loaded {
            preset,
            pf,
            per_frame,
            pv,
            per_pixel,
            q_after_init,
            monitor_after_init,
            waves,
            shapes,
        })
    }

    /// Writes the per-frame inputs every scope can read.
    fn set_globals(&self, ctx: &mut Context) {
        let time = self.start.elapsed().as_secs_f64();
        let progress = ((time - self.preset_start) / self.duration).clamp(0.0, 1.0);
        let a = &self.audio;
        let values = [
            time,
            f64::from(self.fps),
            self.frame as f64,
            progress,
            f64::from(a.level[0]),
            f64::from(a.level[1]),
            f64::from(a.level[2]),
            f64::from(a.att[0]),
            f64::from(a.att[1]),
            f64::from(a.att[2]),
        ];
        for (name, value) in GLOBALS.iter().zip(values) {
            ctx.set(name, value);
        }
        ctx.set("meshx", MESH.0 as f64);
        ctx.set("meshy", MESH.1 as f64);
        ctx.set("pixelsx", f64::from(self.renderer.width));
        ctx.set("pixelsy", f64::from(self.renderer.height));
        ctx.set("aspectx", f64::from(1.0 / self.aspect.0));
        ctx.set("aspecty", f64::from(1.0 / self.aspect.1));
    }

    /// Feeds the latest audio and renders one frame into `view`.
    pub fn render(&mut self, gpu: &mut Gpu, pcm: &[f32], view: &wgpu::TextureView) {
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_frame)
            .as_secs_f32()
            .clamp(1.0 / 240.0, 0.5);
        self.last_frame = now;
        self.fps = self.fps * 0.95 + 0.05 / dt;
        self.frame += 1;
        self.audio.update(pcm, self.fps, self.frame);
        let time = self.start.elapsed().as_secs_f64();
        if !self.locked && time - self.preset_start >= self.duration && self.playlist.len() > 1 {
            self.next_preset();
        }
        let Some(mut loaded) = self.loaded.take() else {
            return;
        };

        // Per-frame script: base values, q's from the init code, then the preset's own code.
        let pf = &mut loaded.pf;
        for (&(_, name, _), &value) in VARS.iter().zip(&loaded.preset.vars) {
            pf.set(name, value);
        }
        for (i, q) in loaded.q_after_init.iter().enumerate() {
            pf.set(&format!("q{}", i + 1), *q);
        }
        pf.set("monitor", loaded.monitor_after_init);
        self.set_globals(pf);
        loaded.per_frame.run(pf, &mut self.gmem);
        let gamma = pf.get("gamma").clamp(0.0, 8.0);
        let echo_zoom = pf.get("echo_zoom").clamp(0.001, 1000.0);
        let q: [f64; NUM_Q] = std::array::from_fn(|i| pf.get(&format!("q{}", i + 1)));

        let (w, h) = (self.renderer.width as f32, self.renderer.height as f32);
        let uvs = self.warp_uvs(&mut loaded, time as f32);
        let mut draws = vec![self.warp_mesh(&uvs, loaded.pf.get("decay") as f32)];
        draws.extend(Self::motion_vectors(&loaded.pf, &uvs, w));
        draws.extend(self.custom_shapes(&mut loaded, &q, w, h));
        draws.extend(self.custom_waves(&mut loaded, &q, w, h));
        draws.extend(self.basic_wave(&loaded, time as f32, w, h));
        let v = |name: &str| loaded.pf.get(name) as f32;
        if loaded.pf.get("darken_center") != 0.0 {
            draws.push(self.darken_center());
        }
        draws.extend(border(
            v("ob_size"),
            0.0,
            [v("ob_r"), v("ob_g"), v("ob_b"), v("ob_a")],
        ));
        draws.extend(border(
            v("ib_size"),
            v("ob_size"),
            [v("ib_r"), v("ib_g"), v("ib_b"), v("ib_a")],
        ));

        let comp = CompParams {
            echo_zoom: echo_zoom as f32,
            echo_alpha: v("echo_alpha"),
            echo_orient: v("echo_orient"),
            gamma: gamma as f32,
            brighten: v("brighten"),
            darken: v("darken"),
            solarize: v("solarize"),
            invert: v("invert"),
        };
        self.renderer
            .frame(gpu, &draws, loaded.pf.get("wrap") > 0.5, comp);
        gpu.blit(
            &self.renderer.out_bind,
            (self.renderer.width, self.renderer.height),
            view,
        );
        self.loaded = Some(loaded);
    }

    /// Runs the per-vertex script over the mesh and returns the source UV of every vertex.
    fn warp_uvs(&mut self, loaded: &mut Loaded, time: f32) -> Vec<[f32; 2]> {
        let (gx, gy) = MESH;
        let (aspectx, aspecty) = self.aspect;
        let pf = &loaded.pf;
        let names = [
            "zoom", "zoomexp", "rot", "warp", "cx", "cy", "dx", "dy", "sx", "sy",
        ];
        let frame_vals: [f64; 10] = std::array::from_fn(|i| pf.get(names[i]));
        let warp_time = time * loaded.preset.warp_anim_speed as f32;
        let warp_scale_inv = 1.0 / loaded.preset.warp_scale as f32;
        let f = [
            11.68 + 4.0 * (warp_time * 1.413 + 10.0).cos(),
            8.77 + 3.0 * (warp_time * 1.113 + 7.0).cos(),
            10.54 + 3.0 * (warp_time * 1.233 + 3.0).cos(),
            11.49 + 4.0 * (warp_time * 0.933 + 5.0).cos(),
        ];
        let run_vertex = !loaded.per_pixel.is_empty();
        let pv = &mut loaded.pv;
        let slots: Vec<usize> = names.iter().map(|n| pv.slot(n)).collect();
        let (sx_slot, sy_slot, rad_slot, ang_slot) =
            (pv.slot("x"), pv.slot("y"), pv.slot("rad"), pv.slot("ang"));
        if run_vertex {
            self.set_globals(pv);
            for i in 1..=NUM_Q {
                pv.set(&format!("q{i}"), pf.get(&format!("q{i}")));
            }
        }
        let mut uvs = Vec::with_capacity((gx + 1) * (gy + 1));
        for iy in 0..=gy {
            for ix in 0..=gx {
                let x = ix as f32 / gx as f32 * 2.0 - 1.0;
                let y = iy as f32 / gy as f32 * 2.0 - 1.0;
                let rad = (x * x * aspectx * aspectx + y * y * aspecty * aspecty).sqrt();
                let mut vals = frame_vals;
                if run_vertex {
                    let ang = if iy == gy / 2 && ix == gx / 2 {
                        0.0
                    } else {
                        (y * aspecty).atan2(x * aspectx)
                    };
                    pv.vars[sx_slot] = f64::from(x * 0.5 * aspectx + 0.5);
                    pv.vars[sy_slot] = f64::from(y * -0.5 * aspecty + 0.5);
                    pv.vars[rad_slot] = f64::from(rad);
                    pv.vars[ang_slot] = f64::from(ang);
                    for (slot, val) in slots.iter().zip(&vals) {
                        pv.vars[*slot] = *val;
                    }
                    loaded.per_pixel.run(pv, &mut self.gmem);
                    for (slot, val) in slots.iter().zip(vals.iter_mut()) {
                        *val = pv.vars[*slot];
                    }
                }
                let [zoom, zoomexp, rot, warp, cx, cy, dx, dy, sx, sy] = vals.map(|v| v as f32);
                let zoom2 = zoom.powf(zoomexp.powf(rad * 2.0 - 1.0));
                let zoom2_inv = 1.0 / zoom2;
                let mut u = x * 0.5 * aspectx * zoom2_inv + 0.5;
                let mut vv = -y * 0.5 * aspecty * zoom2_inv + 0.5;
                u = (u - cx) / sx + cx;
                vv = (vv - cy) / sy + cy;
                if warp != 0.0 {
                    u += warp
                        * 0.0035
                        * (warp_time * 0.333 + warp_scale_inv * (x * f[0] - y * f[3])).sin();
                    vv += warp
                        * 0.0035
                        * (warp_time * 0.375 - warp_scale_inv * (x * f[2] + y * f[1])).cos();
                    u += warp
                        * 0.0035
                        * (warp_time * 0.753 - warp_scale_inv * (x * f[1] - y * f[2])).cos();
                    vv += warp
                        * 0.0035
                        * (warp_time * 0.825 + warp_scale_inv * (x * f[0] + y * f[3])).sin();
                }
                let (u2, v2) = (u - cx, vv - cy);
                let (cos_rot, sin_rot) = (rot.cos(), rot.sin());
                u = u2 * cos_rot - v2 * sin_rot + cx - dx;
                vv = u2 * sin_rot + v2 * cos_rot + cy - dy;
                u = (u - 0.5) / aspectx + 0.5;
                vv = (vv - 0.5) / aspecty + 0.5;
                uvs.push([finite(u), finite(vv)]);
            }
        }
        uvs
    }

    /// The warped copy of the previous frame, faded by `decay`.
    fn warp_mesh(&self, uvs: &[[f32; 2]], decay: f32) -> Draw {
        let (gx, gy) = MESH;
        let color = [decay, decay, decay, 1.0];
        let vertex = |ix: usize, iy: usize| Vertex {
            pos: [
                ix as f32 / gx as f32 * 2.0 - 1.0,
                -(iy as f32 / gy as f32 * 2.0 - 1.0),
            ],
            uv: uvs[iy * (gx + 1) + ix],
            color,
        };
        let mut verts = Vec::with_capacity(gx * gy * 6);
        for iy in 0..gy {
            for ix in 0..gx {
                let (a, b, c, d) = (
                    vertex(ix, iy),
                    vertex(ix, iy + 1),
                    vertex(ix + 1, iy + 1),
                    vertex(ix + 1, iy),
                );
                verts.extend([a, b, d, b, c, d]);
            }
        }
        Draw {
            verts,
            topology: Topo::TriangleList,
            textured: true,
            blend: Blend::Replace,
        }
    }

    fn motion_vectors(pf: &Context, uvs: &[[f32; 2]], tex_w: f32) -> Option<Draw> {
        let alpha = pf.get("mv_a") as f32;
        let (mv_x, mv_y) = (pf.get("mv_x") as f32, pf.get("mv_y") as f32);
        let (mut nx, mut ny) = (mv_x.floor() as i32, mv_y.floor() as i32);
        if alpha <= 0.001 || nx <= 0 || ny <= 0 {
            return None;
        }
        let (mut dx, mut dy) = (mv_x - nx as f32, mv_y - ny as f32);
        if nx > 64 {
            nx = 64;
            dx = 0.0;
        }
        if ny > 48 {
            ny = 48;
            dy = 0.0;
        }
        let (dx2, dy2, len_mult) = (
            pf.get("mv_dx") as f32,
            pf.get("mv_dy") as f32,
            pf.get("mv_l") as f32,
        );
        let min_len = 1.0 / tex_w;
        let color = [
            pf.get("mv_r") as f32,
            pf.get("mv_g") as f32,
            pf.get("mv_b") as f32,
            alpha,
        ];
        let mut verts = Vec::new();
        for j in 0..ny {
            let fy = (j as f32 + 0.25) / (ny as f32 + dy + 0.25 - 1.0) - dy2;
            if !(0.0001..0.9999).contains(&fy) {
                continue;
            }
            for i in 0..nx {
                let fx = (i as f32 + 0.25) / (nx as f32 + dx + 0.25 - 1.0) + dx2;
                if !(0.0001..0.9999).contains(&fx) {
                    continue;
                }
                let [fx2, fy2] = motion_dir(uvs, fx, fy);
                let (mut dxi, mut dyi) = ((fx2 - fx) * len_mult, (fy2 - fy) * len_mult);
                let dist = (dxi * dxi + dyi * dyi).sqrt();
                if dist < min_len && dist > 0.000_000_01 {
                    dxi *= min_len / dist;
                    dyi *= min_len / dist;
                } else if dist <= 0.000_000_01 {
                    dxi = min_len;
                    dyi = min_len;
                }
                verts.push(Vertex::new([2.0 * fx - 1.0, 2.0 * fy - 1.0], color));
                verts.push(Vertex::new(
                    [2.0 * (fx + dxi) - 1.0, 2.0 * (fy + dyi) - 1.0],
                    color,
                ));
            }
        }
        Some(Draw {
            verts,
            topology: Topo::LineList,
            textured: false,
            blend: Blend::Alpha,
        })
    }

    fn custom_shapes(
        &mut self,
        loaded: &mut Loaded,
        q: &[f64; NUM_Q],
        w: f32,
        h: f32,
    ) -> Vec<Draw> {
        let mut draws = Vec::new();
        let (_, aspecty) = self.aspect;
        for (shape, scripts) in loaded.preset.shapes.iter().zip(&mut loaded.shapes) {
            if !shape.enabled {
                continue;
            }
            let ctx = &mut scripts.ctx;
            for inst in 0..shape.num_inst.clamp(1, 1024) {
                self.set_globals(ctx);
                for (i, qv) in q.iter().enumerate() {
                    ctx.set(&format!("q{}", i + 1), *qv);
                }
                for (i, tv) in scripts.t_after_init.iter().enumerate() {
                    ctx.set(&format!("t{}", i + 1), *tv);
                }
                let base = [
                    ("instance", f64::from(inst)),
                    ("x", shape.x),
                    ("y", shape.y),
                    ("rad", shape.rad),
                    ("ang", shape.ang),
                    ("sides", f64::from(shape.sides)),
                    ("r", shape.rgba[0]),
                    ("g", shape.rgba[1]),
                    ("b", shape.rgba[2]),
                    ("a", shape.rgba[3]),
                    ("r2", shape.rgba2[0]),
                    ("g2", shape.rgba2[1]),
                    ("b2", shape.rgba2[2]),
                    ("a2", shape.rgba2[3]),
                    ("border_r", shape.border[0]),
                    ("border_g", shape.border[1]),
                    ("border_b", shape.border[2]),
                    ("border_a", shape.border[3]),
                    ("thickoutline", f64::from(shape.thick)),
                    ("textured", f64::from(shape.textured)),
                    ("tex_zoom", shape.tex_zoom),
                    ("tex_ang", shape.tex_ang),
                    ("additive", f64::from(shape.additive)),
                    ("num_inst", f64::from(shape.num_inst)),
                ];
                for (name, value) in base {
                    ctx.set(name, value);
                }
                scripts.per_frame.run(ctx, &mut self.gmem);
                let g = |name: &str| ctx.get(name) as f32;
                let sides = (g("sides") as i32).clamp(3, 100) as usize;
                let (x, y, rad, ang) =
                    (g("x") * 2.0 - 1.0, g("y") * -2.0 + 1.0, g("rad"), g("ang"));
                let textured = g("textured").abs() >= 1.0;
                let (tex_zoom, tex_ang) = (g("tex_zoom"), g("tex_ang"));
                let blend = if g("additive").abs() >= 1.0 {
                    Blend::Additive
                } else {
                    Blend::Alpha
                };
                let center = Vertex {
                    pos: [x, y],
                    uv: [0.5, 0.5],
                    color: [g("r"), g("g"), g("b"), g("a")],
                };
                let outer_color = [g("r2"), g("g2"), g("b2"), g("a2")];
                let ring: Vec<Vertex> = (0..=sides)
                    .map(|k| {
                        let p = k as f32 / sides as f32 * TAU;
                        let a = p + ang + PI * 0.25;
                        let ta = p + tex_ang + PI * 0.25;
                        Vertex {
                            pos: [x + rad * a.cos() * aspecty, y + rad * a.sin()],
                            uv: [
                                0.5 + 0.5 * ta.cos() / tex_zoom * aspecty,
                                0.5 + 0.5 * ta.sin() / tex_zoom,
                            ],
                            color: outer_color,
                        }
                    })
                    .collect();
                let mut fan = Vec::with_capacity(sides * 3);
                for k in 0..sides {
                    fan.extend([center, ring[k], ring[k + 1]]);
                }
                draws.push(Draw {
                    verts: fan,
                    topology: Topo::TriangleList,
                    textured,
                    blend,
                });
                let border = [g("border_r"), g("border_g"), g("border_b"), g("border_a")];
                if border[3] > 0.0 {
                    let outline: Vec<Vertex> =
                        ring.iter().map(|v| Vertex::new(v.pos, border)).collect();
                    let d = Draw {
                        verts: outline,
                        topology: Topo::LineStrip,
                        textured: false,
                        blend,
                    };
                    draws.extend(thick_copies(d, g("thickoutline").abs() >= 1.0, w, h));
                }
            }
        }
        draws
    }

    fn custom_waves(&mut self, loaded: &mut Loaded, q: &[f64; NUM_Q], w: f32, h: f32) -> Vec<Draw> {
        let mut draws = Vec::new();
        let wave_scale = loaded.preset.wave_scale as f32;
        for (wave, scripts) in loaded.preset.waves.iter().zip(&mut loaded.waves) {
            if !wave.enabled {
                continue;
            }
            let ctx = &mut scripts.ctx;
            self.set_globals(ctx);
            for (i, qv) in q.iter().enumerate() {
                ctx.set(&format!("q{}", i + 1), *qv);
            }
            for (i, tv) in scripts.t_after_init.iter().enumerate() {
                ctx.set(&format!("t{}", i + 1), *tv);
            }
            let base = [
                ("samples", wave.samples as f64),
                ("sep", wave.sep as f64),
                ("scaling", wave.scaling),
                ("spectrum", f64::from(wave.spectrum)),
                ("smoothing", wave.smoothing),
                ("r", wave.rgba[0]),
                ("g", wave.rgba[1]),
                ("b", wave.rgba[2]),
                ("a", wave.rgba[3]),
            ];
            for (name, value) in base {
                ctx.set(name, value);
            }
            scripts.per_frame.run(ctx, &mut self.gmem);
            let g = |name: &str| ctx.get(name) as f32;
            let sep = g("sep").floor().max(0.0) as usize;
            let samples = (g("samples").floor() as usize)
                .min(SAMPLES)
                .saturating_sub(sep);
            if samples < 2 && !(wave.dots && samples >= 1) {
                continue;
            }
            let spectrum = g("spectrum") != 0.0;
            let scale = if spectrum { 0.15 } else { 0.004 } * g("scaling") * wave_scale;
            let (left, right) = if spectrum {
                (&self.audio.freq[0], &self.audio.freq[1])
            } else {
                (&self.audio.time[0], &self.audio.time[1])
            };
            let (j0, j1) = if spectrum {
                (0, 0)
            } else {
                (
                    (SAMPLES - samples) / 2 - sep / 2,
                    (SAMPLES - samples) / 2 + sep / 2,
                )
            };
            let t = if spectrum {
                (SAMPLES - sep) as f32 / samples as f32
            } else {
                1.0
            };
            let mix1 = (g("smoothing") * 0.98).powf(0.5);
            let mix2 = 1.0 - mix1;
            let mut pts = [vec![0.0f32; samples], vec![0.0f32; samples]];
            pts[0][0] = left[j0];
            pts[1][0] = right[j1];
            for j in 1..samples {
                pts[0][j] = left[((j as f32 * t) as usize + j0).min(SAMPLES - 1)] * mix2
                    + pts[0][j - 1] * mix1;
                pts[1][j] = right[((j as f32 * t) as usize + j1).min(SAMPLES - 1)] * mix2
                    + pts[1][j - 1] * mix1;
            }
            for j in (0..samples.saturating_sub(1)).rev() {
                pts[0][j] = pts[0][j] * mix2 + pts[0][j + 1] * mix1;
                pts[1][j] = pts[1][j] * mix2 + pts[1][j + 1] * mix1;
            }
            let frame_color = [g("r"), g("g"), g("b"), g("a")];
            let slots: Vec<usize> = ["sample", "value1", "value2", "x", "y", "r", "g", "b", "a"]
                .iter()
                .map(|n| ctx.slot(n))
                .collect();
            let (inv_ax, inv_ay) = (1.0 / self.aspect.0, 1.0 / self.aspect.1);
            let mut verts = Vec::with_capacity(samples);
            for (j, (&l, &r)) in pts[0].iter().zip(&pts[1]).enumerate() {
                let (v1, v2) = (l * scale, r * scale);
                let values = [
                    j as f32 / (samples - 1).max(1) as f32,
                    v1,
                    v2,
                    0.5 + v1,
                    0.5 + v2,
                    frame_color[0],
                    frame_color[1],
                    frame_color[2],
                    frame_color[3],
                ];
                for (slot, val) in slots.iter().zip(values) {
                    ctx.vars[*slot] = f64::from(val);
                }
                scripts.per_point.run(ctx, &mut self.gmem);
                let out: Vec<f32> = slots[3..].iter().map(|s| ctx.vars[*s] as f32).collect();
                verts.push(Vertex::new(
                    [
                        (out[0] * 2.0 - 1.0) * inv_ax,
                        (out[1] * -2.0 + 1.0) * inv_ay,
                    ],
                    [out[2], out[3], out[4], out[5]],
                ));
            }
            let blend = if wave.additive {
                Blend::Additive
            } else {
                Blend::Alpha
            };
            let d = if wave.dots {
                Draw {
                    verts,
                    topology: Topo::PointList,
                    textured: false,
                    blend,
                }
            } else {
                Draw {
                    verts: smooth_wave(&verts),
                    topology: Topo::LineStrip,
                    textured: false,
                    blend,
                }
            };
            draws.extend(thick_copies(d, wave.thick || wave.dots, w, h));
        }
        draws
    }

    /// The preset's main waveform, modes 0-7 (`DrawWave`).
    fn basic_wave(&self, loaded: &Loaded, time: f32, w: f32, h: f32) -> Vec<Draw> {
        let g = |name: &str| loaded.pf.get(name) as f32;
        let mut alpha = g("wave_a");
        let vol = (self.audio.level[0] + self.audio.level[1] + self.audio.level[2]) / 3.0;
        if vol <= -0.01 || alpha <= 0.001 {
            return Vec::new();
        }
        let preset = &loaded.preset;
        let process = |samples: &[f32; SAMPLES]| -> Vec<f32> {
            let scale = preset.wave_scale as f32 / 128.0;
            let smooth = preset.wave_smoothing as f32;
            let smooth2 = scale * (1.0 - smooth);
            let mut out = Vec::with_capacity(SAMPLES);
            out.push(samples[0] * scale);
            for i in 1..SAMPLES {
                let prev = out[i - 1];
                out.push(samples[i] * smooth2 + prev * smooth);
            }
            out
        };
        let (wave_l, wave_r) = (process(&self.audio.time[0]), process(&self.audio.time[1]));
        let mode = (g("wave_mode").floor() as i32).rem_euclid(8);
        let (pos_x, pos_y) = (g("wave_x") * 2.0 - 1.0, g("wave_y") * 2.0 - 1.0);
        let (aspectx, aspecty) = self.aspect;
        let mut mystery = g("wave_mystery");
        if matches!(mode, 0 | 1 | 4) && !(-1.0..=1.0).contains(&mystery) {
            mystery = mystery * 0.5 + 0.5;
            mystery -= mystery.floor();
            mystery = mystery.abs() * 2.0 - 1.0;
        }
        let tex_alpha = if w < 1024.0 {
            0.09
        } else if w < 2048.0 {
            0.11
        } else {
            0.13
        };
        let n = SAMPLES;
        let mut pts: Vec<[f32; 2]> = Vec::new();
        let mut pts2: Vec<[f32; 2]> = Vec::new();
        match mode {
            0 => {
                let num = n / 2 + 1;
                let inv = 1.0 / (num - 1) as f32;
                let off = (n - num) / 2;
                for i in 0..num - 1 {
                    let mut rad = 0.5 + 0.4 * wave_r[i + off] + mystery;
                    let ang = i as f32 * inv * TAU + time * 0.2;
                    if i < num / 10 {
                        let mut mix = i as f32 / (num as f32 * 0.1);
                        mix = 0.5 - 0.5 * (mix * PI).cos();
                        let rad2 = 0.5 + 0.4 * wave_r[(i + num + off).min(n - 1)] + mystery;
                        rad = (1.0 - mix) * rad2 + rad * mix;
                    }
                    pts.push([
                        rad * ang.cos() * aspecty + pos_x,
                        rad * ang.sin() * aspectx + pos_y,
                    ]);
                }
                pts.push(pts[0]);
            }
            1 => {
                alpha *= 1.25;
                for i in 0..n / 2 {
                    let rad = 0.53 + 0.43 * wave_r[i] + mystery;
                    let ang = wave_l[i + 32] * 0.5 * PI + time * 2.3;
                    pts.push([
                        rad * ang.cos() * aspecty + pos_x,
                        rad * ang.sin() * aspectx + pos_y,
                    ]);
                }
            }
            2 | 3 => {
                alpha *= if mode == 2 {
                    tex_alpha
                } else {
                    tex_alpha * 1.65 * 1.3 * self.audio.level[2] * self.audio.level[2]
                };
                for i in 0..n {
                    pts.push([
                        wave_r[i] * aspecty + pos_x,
                        wave_l[(i + 32) % n] * aspectx + pos_y,
                    ]);
                }
            }
            4 => {
                let num = n.min((w / 3.0) as usize);
                let inv = 1.0 / num as f32;
                let off = (n - num) / 2;
                let w1 = 0.45 + 0.5 * (mystery * 0.5 + 0.5);
                let w2 = 1.0 - w1;
                for i in 0..num {
                    let mut x =
                        2.0 * i as f32 * inv + (pos_x - 1.0) + wave_r[(i + 25 + off) % n] * 0.44;
                    let mut y = wave_l[i + off] * 0.47 + pos_y;
                    if i > 1 {
                        x = x * w2 + w1 * (pts[i - 1][0] * 2.0 - pts[i - 2][0]);
                        y = y * w2 + w1 * (pts[i - 1][1] * 2.0 - pts[i - 2][1]);
                    }
                    pts.push([x, y]);
                }
            }
            5 => {
                alpha *= tex_alpha;
                let (cos_rot, sin_rot) = ((time * 0.3).cos(), (time * 0.3).sin());
                for i in 0..n {
                    let ioff = (i + 32) % n;
                    let x0 = wave_r[i] * wave_l[ioff] + wave_l[i] * wave_r[ioff];
                    let y0 = wave_r[i] * wave_r[i] - wave_l[ioff] * wave_l[ioff];
                    pts.push([
                        (x0 * cos_rot - y0 * sin_rot) * (aspecty + pos_x),
                        (x0 * sin_rot + y0 * cos_rot) * (aspectx + pos_y),
                    ]);
                }
            }
            _ => {
                let num = (n / 2).min((w / 3.0) as usize);
                let off = (n - num) / 2;
                let ang = PI * 0.5 * mystery;
                let (mut dx, mut dy) = (ang.cos(), ang.sin());
                let mut edge_x = [
                    pos_x * (ang + PI * 0.5).cos() - dx * 3.0,
                    pos_x * (ang + PI * 0.5).cos() + dx * 3.0,
                ];
                let mut edge_y = [
                    pos_x * (ang + PI * 0.5).sin() - dy * 3.0,
                    pos_x * (ang + PI * 0.5).sin() + dy * 3.0,
                ];
                for i in 0..2 {
                    for j in 0..4 {
                        let (a, b) = if j < 2 { (edge_x, i) } else { (edge_y, i) };
                        let limit = if j % 2 == 0 { 1.1 } else { -1.1 };
                        let clip = if j % 2 == 0 {
                            a[b] > limit
                        } else {
                            a[b] < limit
                        };
                        if clip {
                            let t = (limit - a[1 - b]) / (a[b] - a[1 - b]);
                            let (dxi, dyi) = (edge_x[i] - edge_x[1 - i], edge_y[i] - edge_y[1 - i]);
                            edge_x[i] = edge_x[1 - i] + dxi * t;
                            edge_y[i] = edge_y[1 - i] + dyi * t;
                        }
                    }
                }
                dx = (edge_x[1] - edge_x[0]) / num as f32;
                dy = (edge_y[1] - edge_y[0]) / num as f32;
                let ang2 = dy.atan2(dx);
                let (perp_dx, perp_dy) = ((ang2 + PI * 0.5).cos(), (ang2 + PI * 0.5).sin());
                let sep = if mode == 7 {
                    (pos_y * 0.5 + 0.5).powi(2)
                } else {
                    0.0
                };
                for i in 0..num {
                    let s = wave_l[i + off];
                    pts.push([
                        edge_x[0] + dx * i as f32 + perp_dx * (0.25 * s + sep),
                        edge_y[0] + dy * i as f32 + perp_dy * (0.25 * s + sep),
                    ]);
                }
                if mode == 7 {
                    for i in 0..num {
                        let s = wave_r[i + off];
                        pts2.push([
                            edge_x[0] + dx * i as f32 + perp_dx * (0.25 * s - sep),
                            edge_y[0] + dy * i as f32 + perp_dy * (0.25 * s - sep),
                        ]);
                    }
                }
            }
        }
        if preset.mod_wave_alpha_by_volume {
            let diff = (preset.mod_wave_alpha_end - preset.mod_wave_alpha_start) as f32;
            alpha *= (vol - preset.mod_wave_alpha_start as f32) / diff;
        }
        let alpha = alpha.clamp(0.0, 1.0);
        let mut rgb = [g("wave_r"), g("wave_g"), g("wave_b")].map(|c| c.clamp(0.0, 1.0));
        if g("wave_brighten") != 0.0 {
            let max = rgb.iter().copied().fold(0.0, f32::max);
            if max > 0.01 {
                rgb = rgb.map(|c| c / max);
            }
        }
        let color = [rgb[0], rgb[1], rgb[2], alpha];
        let blend = if g("wave_additive") != 0.0 {
            Blend::Additive
        } else {
            Blend::Alpha
        };
        let dots = g("wave_usedots") != 0.0;
        let thick = g("wave_thick") != 0.0 || dots;
        let mut draws = Vec::new();
        for p in [pts, pts2] {
            if p.is_empty() {
                continue;
            }
            let verts: Vec<Vertex> = p
                .iter()
                .map(|&[x, y]| Vertex::new([x, -y], color))
                .collect();
            let d = if dots {
                Draw {
                    verts,
                    topology: Topo::PointList,
                    textured: false,
                    blend,
                }
            } else {
                Draw {
                    verts: smooth_wave(&verts),
                    topology: Topo::LineStrip,
                    textured: false,
                    blend,
                }
            };
            draws.extend(thick_copies(d, thick, w, h));
        }
        draws
    }

    /// A faint black diamond at the centre (`bDarkenCenter`).
    fn darken_center(&self) -> Draw {
        let half = 0.05;
        let ay = self.aspect.1;
        let c = Vertex::new([0.0, 0.0], [0.0, 0.0, 0.0, 3.0 / 32.0]);
        let edge = |x: f32, y: f32| Vertex::new([x, y], [0.0; 4]);
        let ring = [
            edge(-half * ay, 0.0),
            edge(0.0, -half),
            edge(half * ay, 0.0),
            edge(0.0, half),
            edge(-half * ay, 0.0),
        ];
        let mut verts = Vec::with_capacity(12);
        for k in 0..4 {
            verts.extend([c, ring[k], ring[k + 1]]);
        }
        Draw {
            verts,
            topology: Topo::TriangleList,
            textured: false,
            blend: Blend::Alpha,
        }
    }
}

/// Compiles one script section; like MilkDrop, a broken section is dropped, not the preset.
fn compile_or_warn(preset: &str, src: &str, ctx: &mut Context) -> Program {
    Program::compile(src, ctx).unwrap_or_else(|e| {
        log::warn!(
            "{}",
            t!(
                "milkdrop.preset_error",
                name = preset,
                error = format!("{e:#}")
            )
        );
        Program::default()
    })
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.5 }
}

/// Where the point `(fx, fy)` (MilkDrop space) came from, by bilinear lookup in the warp UVs.
fn motion_dir(uvs: &[[f32; 2]], fx: f32, fy: f32) -> [f32; 2] {
    let (gx, gy) = MESH;
    let (px, py) = (fx * gx as f32, fy * gy as f32);
    let (x0, y0) = (
        (px.floor() as usize).min(gx - 1),
        (py.floor() as usize).min(gy - 1),
    );
    let (dx, dy) = (px - x0 as f32, py - y0 as f32);
    let at = |x: usize, y: usize| uvs[y * (gx + 1) + x];
    let out: [f32; 2] = std::array::from_fn(|k| {
        at(x0, y0)[k] * (1.0 - dx) * (1.0 - dy)
            + at(x0 + 1, y0)[k] * dx * (1.0 - dy)
            + at(x0, y0 + 1)[k] * (1.0 - dx) * dy
            + at(x0 + 1, y0 + 1)[k] * dx * dy
    });
    [out[0], 1.0 - out[1]]
}

/// Doubles the vertex count of a line strip with a Catmull-Rom-like midpoint (`SmoothWave`).
fn smooth_wave(verts: &[Vertex]) -> Vec<Vertex> {
    let n = verts.len();
    if n < 2 {
        return verts.to_vec();
    }
    let (c1, c2, c3, c4) = (-0.15, 1.15, 1.15, -0.15);
    let inv = 1.0 / (c1 + c2 + c3 + c4);
    let mut out = Vec::with_capacity(n * 2);
    let mut below = 0;
    for i in 0..n - 1 {
        let above = (i + 1).min(n - 1);
        let above2 = (i + 2).min(n - 1);
        out.push(verts[i]);
        let mut mid = verts[i];
        for k in 0..2 {
            mid.pos[k] = (c1 * verts[below].pos[k]
                + c2 * verts[i].pos[k]
                + c3 * verts[above].pos[k]
                + c4 * verts[above2].pos[k])
                * inv;
        }
        out.push(mid);
        below = i;
    }
    out.push(verts[n - 1]);
    out
}

/// Four pixel-offset copies when `thick`, else the draw itself (how MilkDrop fakes line width).
fn thick_copies(d: Draw, thick: bool, w: f32, h: f32) -> Vec<Draw> {
    if !thick {
        return vec![d];
    }
    let offsets = [
        [0.0, 0.0],
        [2.0 / w, 0.0],
        [0.0, 2.0 / h],
        [2.0 / w, 2.0 / h],
    ];
    offsets
        .iter()
        .map(|[ox, oy]| Draw {
            verts: d
                .verts
                .iter()
                .map(|v| Vertex {
                    pos: [v.pos[0] + ox, v.pos[1] + oy],
                    ..*v
                })
                .collect(),
            topology: d.topology,
            textured: d.textured,
            blend: d.blend,
        })
        .collect()
}

/// A frame around the screen: `size` is the border width (0..1), `inset` the width already
/// taken by an outer border.
fn border(size: f32, inset: f32, color: [f32; 4]) -> Option<Draw> {
    if size <= 0.0 || color[3] <= 0.0 {
        return None;
    }
    let (prev, width) = (inset / 2.0 * 2.0, (size / 2.0 + inset / 2.0) * 2.0);
    let quad = |x0: f32, y0: f32, x1: f32, y1: f32| {
        let v = |x, y| Vertex::new([x, y], color);
        [
            v(x0, y0),
            v(x1, y0),
            v(x1, y1),
            v(x0, y0),
            v(x1, y1),
            v(x0, y1),
        ]
    };
    let mut verts = Vec::with_capacity(24);
    verts.extend(quad(-1.0 + prev, -1.0 + width, -1.0 + width, 1.0 - width));
    verts.extend(quad(1.0 - width, -1.0 + width, 1.0 - prev, 1.0 - width));
    verts.extend(quad(-1.0 + prev, 1.0 - width, 1.0 - prev, 1.0 - prev));
    verts.extend(quad(-1.0 + prev, -1.0 + prev, 1.0 - prev, -1.0 + width));
    Some(Draw {
        verts,
        topology: Topo::TriangleList,
        textured: false,
        blend: Blend::Alpha,
    })
}
