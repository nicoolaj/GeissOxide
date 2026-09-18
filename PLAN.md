# GeissOxide — audio visualizers in Rust (GeissOxide + MilkDrop)

> The Makefile rules are documented separately in `MAKEFILE.md` (§8).

## 1. Context

Goal: a cross-platform desktop audio visualizer in **pure Rust** reproducing two Winamp-era
visualizers:

- **Geiss** (Ryan Geiss, 1998-2022, BSD-3) — https://github.com/geissomatik/geiss.
  Windows-only C++/x86-asm/DirectX. Ported to Rust as a CPU engine (8-bit palette mode).
- **MilkDrop 1** (Ryan Geiss, 2001-2007, BSD-3) — re-implemented in Rust on wgpu:
  `.milk` presets, NS-EEL expression language, warp mesh, waveforms, custom waves/shapes,
  motion vectors, borders, video echo. **MilkDrop 2 HLSL shaders are out of scope** (presets with
  `warp_1=`/`comp_1=` lines are skipped by default). User decision after weighing libprojectM
  (rejected: C++ toolchain, OpenGL, no zig cross-compile).

Audio source: the sound card — **internal** (what the computer plays: BlackHole on macOS,
already installed here; PulseAudio/PipeWire *monitor* sources on Linux) or **external** (mic,
line-in, USB interface). Device selectable by name or index.

Targets: **Linux amd64** executable (built by GitHub Actions) and **macOS universal**
(arm64 + x86_64) `.app` (built locally). Delivered with a `Makefile` (§8).

Reference code read for this plan: Geiss `main.cpp` (`GenerateChunkOfNewMap`, `RenderFX`,
`GetWaveData`, `RenderWave`, `render1`), `proc_map.cpp` (warp loop), `video.h`
(`FX_Random_Palette`, `CrankPal`, `PutPalette`), `Effects.h`, geisswerks.com/geiss/secrets.html.
MilkDrop references for implementation: `WACUP/vis_milk2` (BSD-3: `vis_milk2/milkdropfs.cpp`
warp UV math + waves/shapes/borders/echo, `state.cpp` preset keys + defaults, `fft.cpp`,
`ns-eel2/*.h` function list) and `jberg/butterchurn` (MIT, readable JS port of the same math).
Preset pack: `projectM-visualizer/presets-milkdrop-original` (the pack shipped with the last
official MilkDrop; ~100 "Geiss - *" presets).

## 2. Conventions (user requirement: language conventions, best practices, DRY, KISS)

- Rust 2024 edition, `rustfmt` defaults, `cargo clippy --all-targets -- -D warnings` clean.
- Idiomatic error handling: `anyhow::Result` at the binary boundary, `?` everywhere, no
  `unwrap()`/`expect()` outside tests and provably-infallible spots (commented).
- `unsafe` forbidden crate-wide (`#![forbid(unsafe_code)]`); wgpu/cpal/winit are safe APIs.
- Doc comment on every `pub` item; module-level `//!` stating what the module ports and from where.
- **DRY**: one line/point rasterizer shared by all GeissOxide waveforms and effects; one ring buffer
  shared by both engines; one `Pcm` struct (interleaved stereo f32) as the audio contract; wgpu
  boilerplate (device/surface/blit) in one module used by both engines; constants defined once.
- **KISS**: no trait for a single implementation (engines are an `enum`), no config files, no
  plugin system, no abstractions "for later". Smallest diff that works; `ponytail:` comments mark
  deliberate ceilings.
- Tests: one focused `#[test]` per non-trivial unit (see phases); no fixtures/frameworks.
- Commit messages: conventional (`feat:`, `fix:`, `build:`…); commit only when asked.
- **i18n** (user requirement): every user-facing string (CLI help/about, `--list-devices` output,
  on-screen `H` help, errors/warnings) goes through `t!("key")` — no UI string literals in code.
  Locale auto-detected at startup, English is the fallback. Languages now: `en`, `fr`; adding one =
  adding `locales/<lang>.yml` (nothing else to touch). Code, comments, docs, commits: English.

## 3. Stack

| Concern | Crate | Notes |
|---|---|---|
| Window / events / fullscreen | `winit` 0.30 | pure Rust; X11 + Wayland (dlopen) on Linux, AppKit on macOS |
| GPU | `wgpu` 30 | Metal on macOS, Vulkan (GL fallback) on Linux; WGSL shaders |
| Audio capture | `cpal` 0.18 | CoreAudio / ALSA (`libasound` is the only link-time C dep on Linux) |
| FFT (MilkDrop) | `rustfft` | 512-point; GeissOxide uses its own tiny DFT (port as-is) |
| CLI | `clap` (derive) | |
| RNG | `rand` | |
| Screenshot (dev check) | `png` | `--frames N --screenshot out.png` |
| i18n | `rust-i18n` + `sys-locale` | YAML locale files embedded at compile time, `t!("key")`, fallback `en`; locale auto-detected (`sys_locale::get_locale()` works for Finder-launched `.app` where `LANG` is unset) |
| Misc | `anyhow`, `bytemuck`, `pollster`, `log` + `env_logger` | |

Not built (YAGNI): settings GUI, config files, Geiss INI presets/ratings/custom messages,
MilkDrop 2 shaders, preset blending on first release (hard cut; "snap" blend is phase 6),
native macOS Core Audio taps (BlackHole covers loopback), Windows target.

License: BSD-3 (same as Geiss/MilkDrop). Preset pack is downloaded, not vendored.

## 4. Architecture

```
geissoxide/                         single crate, binary `geissoxide`
├── Cargo.toml  Makefile  MAKEFILE.md  PLAN.md  README.md  LICENSE  deny.toml  .gitignore
├── .github/workflows/release.yml   secu + linux + macos jobs; artifacts; release on tag
├── packaging/Info.plist            CFBundleIdentifier, NSMicrophoneUsageDescription, NSHighResolutionCapable
├── locales/en.yml  locales/fr.yml  all user-facing strings (rust-i18n)
├── presets/                        downloaded by `make presets` (git-ignored)
└── src/
    ├── main.rs        CLI, winit ApplicationHandler, frame loop, keys, engine enum {GeissOxide, MilkDrop}
    ├── i18n.rs        `init()`: sys-locale → language tag (`fr-FR` → `fr`), `rust_i18n::set_locale`, fallback `en`
    ├── audio.rs       cpal device list/select, capture thread → ring buffer, gain, `--input test` synth
    ├── gpu.rs         wgpu init (surface, device, queue), resize, RGBA texture blit (fullscreen triangle), read-back for --screenshot
    ├── geissoxide/
    │   ├── mod.rs     GeissOxide::new(w,h) / step(&Pcm) → &[u8] RGBA; render1 pipeline; map switching on beats
    │   ├── map.rs     25 map modes → Vec<MapEntry{offset:u32, w:[u8;4]}>; built on a background thread
    │   ├── warp.rs    bilinear warp with decaying weights (Process_Map)
    │   ├── palette.rs CrankPal 1-7, FX monotone palettes, coarse/solar bands, 18-frame blend
    │   ├── sound.rs   level trigger, smoothing, centroid removal, volume stats, beat mode / big beat
    │   ├── raster.rs  shared `plot_max(x,y,c)` / line helpers for waves + effects (DRY)
    │   ├── wave.rs    waveforms 1-7, RenderDots, Diminish_Center
    │   └── effects.rs (phase 5) chasers, bar, dotty chaser, solar particles, nuclide, grid, shade bobs
    └── milkdrop/
        ├── mod.rs     MilkDrop::new(gpu,w,h,presets) / feed(&Pcm) / render(); playlist, timer + beat cut
        ├── eel.rs     NS-EEL: lexer, parser (precedence climbing) → bytecode; VM over f64 slots; builtins; megabuf/gmegabuf
        ├── preset.rs  .milk parser → Preset {base vars, per_frame_init/per_frame/per_pixel code, 4 waves, 4 shapes}; defaults from state.cpp
        ├── audio.rs   512-pt FFT, MilkDrop equalize/envelope, bass/mid/treb (+_att), vol, waveform/spectrum arrays for waves
        ├── render.rs  wgpu pipelines: warp pass (mesh), primitive pass (lines/tris, alpha/additive), composite pass (echo, gamma, brighten/darken/solarize/invert, darken center)
        └── shaders.wgsl
```

Frame loop (vsync):
```
events → pcm = ring.drain() * gain
GeissOxide: rgba = geissoxide.step(&pcm); gpu.blit(rgba)    (letterboxed to internal aspect)
MilkDrop: md.feed(&pcm); md.render(&gpu, surface_view)
```

Keys: `Esc` quit · `F` fullscreen · `Tab` switch engine · `Space`/`→` next preset / new map ·
`←` previous preset · `L` lock preset · `+`/`-` gain · `H` help to stderr.

CLI: `geissoxide [--engine geissoxide|milkdrop] [--device NAME|INDEX] [--list-devices] [--presets DIR]
[--res WxH] [--fullscreen] [--gain F] [--preset-duration S] [--allow-shader-presets]
[--input device|test] [--frames N --screenshot out.png]`.

## 5. Host prerequisites

macOS (this machine, verified): rustc 1.97, targets `aarch64-apple-darwin` + `x86_64-apple-darwin`,
`lipo`, libclang (CLT, for `coreaudio-sys` bindgen), `cargo-audit`, `cargo-deny`, `gh`, BlackHole 16ch.
Nothing to install.
Linux CI runner (ubuntu-22.04 → glibc 2.35 baseline): `libasound2-dev`. Runtime: `libasound.so.2`,
Vulkan or GL driver.

## 6. Implementation phases (each ends with a runnable check)

### Phase 0 — Scaffold
1. `git init`; `cargo init --name geissoxide`; write `PLAN.md`, `MAKEFILE.md`, `Makefile`, `deny.toml`,
   `.gitignore`, `LICENSE`, `packaging/Info.plist`, `.github/workflows/release.yml`.
2. `gpu.rs` + `main.rs`: winit window 1280×720 + wgpu surface; blit a test gradient; `F` fullscreen; resize.
3. `i18n.rs` + `locales/{en,fr}.yml`: `i18n::init()` runs before clap parsing so `--help` is localized
   (`#[arg(help = t!(...))]`). Test: every key of `fr.yml` exists in `en.yml`; `LANG=fr_FR.UTF-8 geissoxide --help`
   prints French, `LANG=de_DE` falls back to English.
   → check: window renders on arm64; `cargo build --target x86_64-apple-darwin` succeeds; `make secu` green.

### Phase 1 — Audio (`audio.rs`)
- `--list-devices`: `cpal::default_host().input_devices()` → `index: name`.
- Capture thread: `build_input_stream` on the chosen device (default input), converts any sample
  format to f32 interleaved stereo at the device rate (resample not needed: both engines take
  the rate as a parameter). Ring buffer `Arc<Mutex<VecDeque<f32>>>` capped at 1 s; drained per frame.
- `--input test`: kick every 500 ms + sine sweep, so rendering is checkable without a device.
- README notes: macOS loopback = BlackHole + Multi-Output Device; Linux loopback =
  `--device pulse` with `PULSE_SOURCE=<sink>.monitor` or `pactl set-default-source`.
→ check: `--list-devices` shows the MacBook mic and BlackHole; RMS debug log moves with sound.

### Phase 2 — GeissOxide engine v1 (`src/geissoxide/*`), port order with one test each
1. `palette.rs` — `CrankPal` curves 1-7, random palette (solar/coarse knobs), FX monotone palettes
   0-3, `blend(a,b,t)` over 18 frames. Test: all entries ≤ 255, blend endpoints equal inputs.
2. `map.rs` — `MapParams` (mode 1-25, centre, scale/turn 1-2, f1-f4, damping, weightsum, mode-6
   charges) + `generate(&MapParams, w, h) -> Vec<MapEntry>`: exact port of the 25 modes, rotation-
   dither modes, custom-vector modes 6/10/12, damping mix, x-wrap, offset clamp `[2W, W(H-3)-1]`,
   `weightsum_res_adjusted` table; run on `std::thread`, delivered via `mpsc`. Test: every mode at
   320×240 → all offsets in bounds, Σw ∈ (0,256).
3. `warp.rs` — `dst[i] = (w0·s[o] + w1·s[o+1] + w2·s[o+W] + w3·s[o+W+1]) >> 8`. Test: identity map
   with Σw = 255 dims a flat image by 1/256; a +1-offset map shifts a lone bright pixel by one.
4. `sound.rs` — `GetWaveData` post-capture part: latest `max(3W, MINBUFSIZE)` samples at i16 scale,
   level trigger (height + slope match), 0.8/0.2 smoothing, `volscale`, per-channel centroid removal;
   `current_vol`/`avg_vol`/`avg_vol_narrow`/`past_vol[]`, `bBeatMode`, `bBigBeat` + threshold decay.
   Test: a sine gives a stable trigger index across frames and zero-mean output.
5. `raster.rs` + `wave.rs` — waveforms 1-7 (max-blend plots), `RenderDots` (treble hits),
   `Diminish_Center`, brightness `base` from volume (saver formula). Test: plots stay inside
   `[FX_YCUT_HIDE, H-FX_YCUT_HIDE)`.
6. `mod.rs` — `render1` order, uniform random mode pick (ratings skipped), per-mode waveform rules,
   `frames_til_auto_switch` scaled by fps, map swap on big beat, palette re-roll on swap, LUT → RGBA.
   Internal res default 800×450 (`--res`); `rmult = 640/W` keeps the original tuning.
→ check: `make run` shows warping palettes reacting to audio; `--input test --frames 300 --screenshot`
  gives a recognisable GeissOxide frame (inspected); warp ≤ 2 ms at 800×450 (logged once); `cargo test` green.

### Phase 3 — NS-EEL + preset parser (`milkdrop/eel.rs`, `preset.rs`)
- `eel.rs`: tokens (numbers incl. `1.`/`.5`, idents case-insensitive, `$PI/$E/$PHI`, `//` comments);
  grammar: `;`-separated statements, `=` and MD2 compound assigns, `?:`, `||/&&`, `==/!=/</>/<=/>=`,
  `+ -`, `* / %`, unary `- !`, `^` (pow), `& |` (int bitwise), calls. Compile to a flat bytecode
  `Vec<Op>`; VM with `f64` slots; variable table built at compile time (name → slot). Builtins:
  sin cos tan asin acos atan atan2 sqr sqrt invsqrt pow exp log log10 abs min max sign rand int
  floor ceil sigmoid band bor bnot equal above below if loop while exec2 exec3 assign
  megabuf gmegabuf (sparse `HashMap<u32,f64>`; gmegabuf shared across a preset's programs).
  Test: precedence (`1+2*3^2`), `if(above(x,1),…)`, assignment returns value, megabuf round-trip,
  short-circuit `&&`, `%` on non-integers, `rand(n)` range.
- `preset.rs`: INI-style `.milk` → `Preset { vars: base values for all per-frame vars (defaults from
  `state.cpp`), per_frame_init, per_frame, per_pixel programs, waves[4] {enabled, samples, sep,
  spectrum, dots, thick, additive, scaling, smoothing, rgba, init/per_frame/per_point}, shapes[4]
  {enabled, sides, additive, thick, textured, num_inst, x,y,rad,ang, rgba, rgba2, border, init/per_frame},
  has_shaders }`. Lines are joined in `_N` order. Test: parse a bundled "Geiss - *" preset and
  check a few numeric keys + program line counts; a preset with `warp_1=` sets `has_shaders`.

### Phase 4 — MilkDrop renderer (`milkdrop/render.rs`, `audio.rs`, `mod.rs`)
- `audio.rs`: 512-sample window per channel → `rustfft` → MilkDrop equalize + envelope; `imm/avg/
  long_avg` per band with fps-adjusted rates; `bass/mid/treb`, `*_att`, `vol`; smoothed waveform and
  spectrum arrays (576/512) for waves. Test: a 60 Hz sine → bass dominates; 8 kHz → treb dominates.
- `render.rs` (wgpu): two RGBA16F feedback textures (ping-pong, size = window or `--res`);
  **warp pass**: mesh `meshx×meshy` (default 48×36) — per-vertex `x,y,rad,ang` → run per_pixel program
  → UV via the `milkdropfs.cpp` formula (zoom/zoomexp/rot/warp/cx/cy/dx/dy/sx/sy, animated warp
  terms) → vertex buffer; fragment samples previous frame × `decay`, wrap vs clamp; **primitive pass**
  into the same texture: waveform modes 0-7, custom waves (per_point program per sample), custom
  shapes (n-gon fan + border), motion vectors, outer/inner borders; alpha or additive blend,
  thick = 4 offset passes like the original; **composite pass** to the surface: video echo
  (`echo_zoom/alpha/orient`), gamma, brighten/darken/solarize/invert, darken center.
- `mod.rs`: per_frame_init once per preset, per_frame each frame with `time fps frame progress
  bass… q1-q32 meshx meshy pixelsx pixelsy aspectx aspecty`; playlist (dir scan, shuffle, skip
  `has_shaders` unless `--allow-shader-presets`), preset duration timer + hard cut on beat
  (`bass_att`-based like `milkdrop.cpp`), keys next/prev/lock.
- `make presets` downloads `presets-milkdrop-original` into `presets/`.
→ check: `make run ARGS="--engine milkdrop"` plays the original pack; `--frames 120 --screenshot`
  PNGs of 3 named presets inspected against known looks; `cargo test` green.

### Phase 5 — GeissOxide extra effects (`geissoxide/effects.rs`)
`Two_Chasers`, `Solid_Line` (bar), `One_Dotty_Chaser`, `Drop_Solar_Particles`, `Nuclide`, `Grid`,
`ShadeBobs` with the per-mode `effect_freq` table; same random gating as the original.

### Phase 6 — Packaging, CI, docs
- `make darwin`: both targets → `lipo` → `dist/GeissOxide.app` (`Contents/MacOS/geissoxide`, `Info.plist`,
  `Resources/presets`), `codesign -s - --force --deep`. Check: `lipo -info` = `x86_64 arm64`;
  `open dist/GeissOxide.app` runs and prompts for the microphone.
- `.github/workflows/release.yml`: jobs `secu` (fmt, clippy, audit, deny, test), `linux`
  (ubuntu-22.04, `apt install libasound2-dev`, `cargo build --release`, artifact `geissoxide-linux-amd64`),
  `macos` (universal .app zip); on `v*` tags → GitHub Release. Triggered by `workflow_dispatch`
  so `make linux` on macOS can run `gh workflow run` + `gh run watch` + `gh run download` into `dist/`.
- README (English): build, run, device selection, loopback setup per OS, keys, presets.
- Later (not now): preset "snap" blending, Core Audio process taps.

## 7. Verification (end to end)
1. `make` → secu green + release binary.
2. `make run ARGS="--list-devices"` → mic + BlackHole listed.
3. `make run` (GeissOxide) and `make run ARGS="--engine milkdrop"` react to music routed through
   BlackHole (Multi-Output Device); `Tab` switches engines live.
4. Headless: `target/release/geissoxide --input test --frames 300 --screenshot /tmp/g.png` for both
   engines; PNGs inspected.
5. `make darwin` → universal `.app` launches. `make linux` → `dist/geissoxide-linux-amd64` from CI
   (`file` → ELF x86-64). `make dist` → both.
6. `cargo test` → unit tests of phases 2-4.

## 8. Makefile (spec; documented in `MAKEFILE.md`)

Variables: `NAME=geissoxide`, `VERSION` (from Cargo.toml), `BUNDLE_ID?=org.geissoxide.visualizer`,
`DIST=dist`, `ARGS` (forwarded by `run`), `UNAME_S`. `.DEFAULT_GOAL := all`.

| Target | Does |
|---|---|
| `all` (`make`) | `secu` then `cargo build --release` for the current OS/arch |
| `run` | `all` then `./target/release/geissoxide $(ARGS)` |
| `secu` (alias `sécu`) | `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo audit`, `cargo deny check`, `cargo test` |
| `darwin` | `secu`, build arm64 + x86_64, `lipo`, assemble + ad-hoc sign `dist/GeissOxide.app` (macOS host only) |
| `linux` | `secu`; on Linux: native `cargo build --release` → `dist/geissoxide-linux-amd64`; on macOS: run the GitHub Actions workflow via `gh` and download the artifact into `dist/` |
| `dist` | `darwin` + `linux` |
| `presets` | download the original MilkDrop preset pack into `presets/` |
| `help` | list targets (from `##` comments) |
| `clean` | `cargo clean` |
| `mrproper` | `clean` + `rm -rf dist presets` ("mrpropoer" in the request → `mrproper`) |

`MAKEFILE.md` (English) documents: prerequisites per OS, each target with what it runs and
produces, variables, the CI hand-off used by `make linux` on macOS, and troubleshooting
(cargo-audit/deny missing, `gh auth`, microphone permission).

## 9. Risks
- MilkDrop fidelity: formulas must be taken from `milkdropfs.cpp`/butterchurn, not memory; MD2
  shader presets are skipped by default.
- cpal on Linux lists ALSA PCMs only; loopback relies on the Pulse/PipeWire ALSA plugin + `PULSE_SOURCE` (documented).
- `make linux` from macOS requires the repo on GitHub and `gh auth login`.


## 10. Upcoming evolutions (user: "évolutions à venir" — not in this plan)
- More languages: add `locales/<lang>.yml` only.
- Preset "snap" blending between MilkDrop presets; MilkDrop 2 shader presets (HLSL → WGSL translator).
- Native macOS Core Audio process taps (loopback without BlackHole, macOS ≥ 14.2).
- Geiss 32-bit colour mode and remaining effects if phase 5 is cut short.
