//! GeissOxide, MilkDrop & Chladni audio visualizers — window, CLI and frame loop.

#![forbid(unsafe_code)]

mod audio;
mod chladni;
mod geissoxide;
mod gpu;
mod i18n;
mod milkdrop;

rust_i18n::i18n!("locales", fallback = "en");

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use rust_i18n::t;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

/// Which visualizer runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum EngineKind {
    #[value(name = "geissoxide")]
    GeissOxide,
    Milkdrop,
    Chladni,
}

/// Where audio comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum InputKind {
    Device,
    Test,
}

/// Command line options (help text is localized at runtime).
#[derive(Parser, Debug)]
#[command(version, about = t!("app.about").to_string())]
struct Cli {
    #[arg(long, value_enum, default_value_t = EngineKind::GeissOxide, help = t!("cli.engine").to_string())]
    engine: EngineKind,
    #[arg(long, help = t!("cli.device").to_string())]
    device: Option<String>,
    #[arg(long, help = t!("cli.list_devices").to_string())]
    list_devices: bool,
    #[arg(long, default_value = "presets", help = t!("cli.presets").to_string())]
    presets: PathBuf,
    #[arg(long, default_value = "800x450", value_parser = parse_res, help = t!("cli.res").to_string())]
    res: (u32, u32),
    #[arg(long, help = t!("cli.fullscreen").to_string())]
    fullscreen: bool,
    #[arg(long, default_value_t = 1.0, help = t!("cli.gain").to_string())]
    gain: f32,
    #[arg(long, default_value_t = 20.0, help = t!("cli.preset_duration").to_string())]
    preset_duration: f32,
    #[arg(long, help = t!("cli.allow_shader_presets").to_string())]
    allow_shader_presets: bool,
    #[arg(long, value_enum, default_value_t = InputKind::Device, help = t!("cli.input").to_string())]
    input: InputKind,
    #[arg(long, help = t!("cli.frames").to_string())]
    frames: Option<u64>,
    #[arg(long, help = t!("cli.screenshot").to_string())]
    screenshot: Option<PathBuf>,
}

/// The presets directory as given, or, when it does not exist, the copy shipped next to the
/// executable (`Contents/Resources/presets` in the macOS bundle, `presets/` beside it on Linux).
fn resolve_presets(given: &Path) -> PathBuf {
    if given.is_dir() {
        return given.to_path_buf();
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    exe_dir
        .into_iter()
        .flat_map(|dir| [dir.join("../Resources").join(given), dir.join(given)])
        .find(|p| p.is_dir())
        .unwrap_or_else(|| given.to_path_buf())
}

fn window_title(device: &str) -> String {
    format!("GeissOxide \u{2014} {device}")
}

fn parse_res(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got {s}"))?;
    let parse = |v: &str| v.parse::<u32>().map_err(|e| e.to_string());
    Ok((parse(w)?, parse(h)?))
}

fn main() -> Result<()> {
    i18n::init();
    env_logger::init();
    let cli = Cli::parse();
    if cli.list_devices {
        return audio::print_devices();
    }
    let device = match cli.device.clone() {
        Some(d) => Some(d),
        // Launched from the Finder (no terminal): ask with a native dialog.
        #[cfg(target_os = "macos")]
        None if cli.input == InputKind::Device && !std::io::stdin().is_terminal() => {
            match audio::pick_device()? {
                Some(d) => Some(d),
                None => return Ok(()),
            }
        }
        None => None,
    };
    let capture = match cli.input {
        InputKind::Test => audio::Capture::test(cli.gain),
        InputKind::Device => audio::Capture::open(device.as_deref(), cli.gain)?,
    };
    let geissoxide = geissoxide::GeissOxide::new(cli.res.0 as usize, cli.res.1 as usize);
    let event_loop = EventLoop::new()?;
    let engine = cli.engine;
    let mut app = App {
        cli,
        capture,
        engine,
        geissoxide,
        milkdrop: None,
        chladni: None,
        window: None,
        gpu: None,
        frame: 0,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}

/// winit application state.
struct App {
    cli: Cli,
    capture: audio::Capture,
    engine: EngineKind,
    geissoxide: geissoxide::GeissOxide,
    milkdrop: Option<milkdrop::MilkDrop>,
    chladni: Option<chladni::Chladni>,
    window: Option<Arc<Window>>,
    gpu: Option<gpu::Gpu>,
    frame: u64,
    error: Option<anyhow::Error>,
}

impl App {
    fn render(&mut self) -> Result<()> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };
        let (w, h) = self.cli.res;
        let Some(frame) = gpu.acquire() else {
            return Ok(());
        };
        let view = frame.texture.create_view(&Default::default());
        match self.engine {
            EngineKind::GeissOxide => {
                let pcm = self.capture.latest(self.geissoxide.frames_needed());
                let rgba = self.geissoxide.step(&pcm).to_vec();
                gpu.blit_rgba(w, h, &rgba, &view);
            }
            EngineKind::Milkdrop => {
                let md = match self.milkdrop.as_mut() {
                    Some(md) => md,
                    None => self.milkdrop.insert(milkdrop::MilkDrop::new(
                        gpu,
                        w,
                        h,
                        self.capture.rate,
                        &resolve_presets(&self.cli.presets),
                        self.cli.allow_shader_presets,
                        f64::from(self.cli.preset_duration),
                    )?),
                };
                let pcm = self.capture.latest(md.frames_needed());
                md.render(gpu, &pcm, &view);
            }
            EngineKind::Chladni => {
                let ch = self.chladni.get_or_insert_with(|| {
                    chladni::Chladni::new(
                        w as usize,
                        h as usize,
                        self.capture.rate,
                        self.cli.preset_duration,
                    )
                });
                let pcm = self.capture.latest(ch.frames_needed());
                let rgba = ch.step(&pcm).to_vec();
                gpu.blit_rgba(w, h, &rgba, &view);
            }
        }
        self.frame += 1;
        if self.cli.frames.is_some_and(|n| self.frame >= n) {
            if let Some(path) = &self.cli.screenshot {
                let pixels = gpu.read_texture(&frame.texture)?;
                gpu::save_png(
                    path,
                    gpu.config.width,
                    gpu.config.height,
                    &pixels,
                    gpu.surface_is_bgra(),
                )?;
                println!("{}", t!("gpu.screenshot_saved", path = path.display()));
            }
        }
        gpu.present(frame);
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(window_title(&self.capture.name))
            .with_window_icon(gpu::window_icon().ok())
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720))
            .with_fullscreen(self.cli.fullscreen.then_some(Fullscreen::Borderless(None)));
        match event_loop
            .create_window(attrs)
            .map_err(anyhow::Error::from)
            .and_then(|w| {
                let window = Arc::new(w);
                gpu::Gpu::new(window.clone()).map(|g| (window, g))
            }) {
            Ok((window, gpu)) => {
                self.window = Some(window);
                self.gpu = Some(gpu);
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.key(event_loop, logical_key),
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.render() {
                    self.error = Some(e);
                    event_loop.exit();
                }
                if self.cli.frames.is_some_and(|n| self.frame >= n) {
                    event_loop.exit();
                } else if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl App {
    fn key(&mut self, event_loop: &ActiveEventLoop, key: Key) {
        let Some(window) = &self.window else { return };
        match key {
            Key::Named(NamedKey::Escape) => event_loop.exit(),
            Key::Character(c) if c.eq_ignore_ascii_case("f") => {
                let next = window
                    .fullscreen()
                    .is_none()
                    .then_some(Fullscreen::Borderless(None));
                window.set_fullscreen(next);
            }
            Key::Named(NamedKey::Tab) => {
                self.engine = match self.engine {
                    EngineKind::GeissOxide => EngineKind::Milkdrop,
                    EngineKind::Milkdrop => EngineKind::Chladni,
                    EngineKind::Chladni => EngineKind::GeissOxide,
                };
                eprintln!(
                    "{}",
                    t!("engine.switched", name = format!("{:?}", self.engine))
                );
            }
            Key::Named(NamedKey::Space | NamedKey::ArrowRight) => {
                match (self.engine, self.milkdrop.as_mut(), self.chladni.as_mut()) {
                    (EngineKind::Milkdrop, Some(md), _) => md.next_preset(),
                    (EngineKind::Chladni, _, Some(ch)) => ch.next(),
                    _ => self.geissoxide.next_map(),
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if let (EngineKind::Milkdrop, Some(md)) = (self.engine, self.milkdrop.as_mut()) {
                    md.prev_preset();
                }
            }
            Key::Character(c) if c.eq_ignore_ascii_case("l") => {
                if let Some(md) = self.milkdrop.as_mut() {
                    md.locked = !md.locked;
                    eprintln!("{}", t!("milkdrop.locked", state = md.locked));
                }
            }
            Key::Character(c) if c.eq_ignore_ascii_case("d") => {
                match audio::next_device(&self.capture.name)
                    .and_then(|sel| audio::Capture::open(Some(&sel), self.cli.gain))
                {
                    Ok(capture) => {
                        self.capture = capture;
                        // Both hold the sample rate; rebuilt on next frame.
                        self.milkdrop = None;
                        self.chladni = None;
                        audio::remember(&self.capture.name);
                        window.set_title(&window_title(&self.capture.name));
                    }
                    Err(e) => eprintln!("{}", t!("audio.switch_error", error = e)),
                }
            }
            Key::Character(c) if c.eq_ignore_ascii_case("h") => {
                eprintln!("{}", t!("app.help_keys"))
            }
            _ => {}
        }
    }
}
