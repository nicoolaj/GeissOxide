//! wgpu boilerplate shared by both engines: surface, device, RGBA blit, screenshot read-back.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use rust_i18n::t;
use winit::window::Window;

const BLIT_SHADER: &str = r#"
struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    // One triangle covering the viewport; uv flipped so texture row 0 is at the top.
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i >> 1u) * 4 - 1);
    return VsOut(vec4(x, y, 0.0, 1.0), vec2((x + 1.0) * 0.5, 1.0 - (y + 1.0) * 0.5));
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
"#;

/// GPU context bound to the application window.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    blit_layout: wgpu::BindGroupLayout,
    blit_pipeline: wgpu::RenderPipeline,
    /// Same shader, alpha-blended over what is already drawn.
    overlay_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    blit_texture: Option<(wgpu::Texture, wgpu::BindGroup)>,
    overlay_texture: Option<(wgpu::Texture, wgpu::BindGroup)>,
}

impl Gpu {
    /// Creates the surface, device and blit pipeline for `window`.
    pub fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window)?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|_| anyhow!(t!("gpu.no_adapter")))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .context("surface not supported by adapter")?;
        config.usage |= wgpu::TextureUsages::COPY_SRC;
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = pipeline(&device, &pipeline_layout, &shader, config.format, None);
        let overlay_pipeline = pipeline(
            &device,
            &pipeline_layout,
            &shader,
            config.format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            device,
            queue,
            surface,
            config,
            blit_layout,
            blit_pipeline,
            overlay_pipeline,
            sampler,
            blit_texture: None,
            overlay_texture: None,
        })
    }

    /// Reconfigures the surface after a window resize.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    /// Acquires the next swap-chain texture; `None` means "skip this frame".
    pub fn acquire(&mut self) -> Option<wgpu::SurfaceTexture> {
        use wgpu::CurrentSurfaceTexture as C;
        match self.surface.get_current_texture() {
            C::Success(frame) => Some(frame),
            C::Suboptimal(frame) => {
                self.surface.configure(&self.device, &self.config);
                Some(frame)
            }
            C::Outdated | C::Lost => {
                self.surface.configure(&self.device, &self.config);
                None
            }
            C::Timeout | C::Occluded | C::Validation => None,
        }
    }

    /// Presents an acquired frame.
    pub fn present(&self, frame: wgpu::SurfaceTexture) {
        self.queue.present(frame);
    }

    /// Uploads an RGBA8 image and draws it centred on `view`, letterboxed to keep its aspect.
    pub fn blit_rgba(&mut self, width: u32, height: u32, rgba: &[u8], view: &wgpu::TextureView) {
        let taken = self.blit_texture.take();
        let slot = self.upload_rgba(taken, width, height, rgba);
        self.blit(&slot.1, (width, height), view);
        self.blit_texture = Some(slot);
    }

    /// Uploads an RGBA8 image and alpha-blends it, unscaled and centred, over `view`.
    pub fn overlay_rgba(&mut self, width: u32, height: u32, rgba: &[u8], view: &wgpu::TextureView) {
        let taken = self.overlay_texture.take();
        let slot = self.upload_rgba(taken, width, height, rgba);
        let (ow, oh) = (self.config.width as f32, self.config.height as f32);
        let (w, h) = ((width as f32).min(ow), (height as f32).min(oh));
        let rect = ((ow - w) * 0.5, (oh - h) * 0.5, w, h);
        self.draw(
            &self.overlay_pipeline,
            &slot.1,
            rect,
            wgpu::LoadOp::Load,
            view,
        );
        self.overlay_texture = Some(slot);
    }

    /// Writes `rgba` into `slot`, recreated when its size differs.
    fn upload_rgba(
        &self,
        slot: Option<(wgpu::Texture, wgpu::BindGroup)>,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> (wgpu::Texture, wgpu::BindGroup) {
        let (texture, bind_group) = match slot {
            Some(s) if s.0.width() == width && s.0.height() == height => s,
            _ => {
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("blit source"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let bind_group = self.bind_texture(&texture);
                (texture, bind_group)
            }
        };
        self.queue.write_texture(
            texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            texture.size(),
        );
        (texture, bind_group)
    }

    /// Draws a bound texture of size `size` centred on `view`, letterboxed to keep its aspect.
    pub fn blit(&self, bind_group: &wgpu::BindGroup, size: (u32, u32), view: &wgpu::TextureView) {
        let rect = letterbox(self.config.width, self.config.height, size.0, size.1);
        let load = wgpu::LoadOp::Clear(wgpu::Color::BLACK);
        self.draw(&self.blit_pipeline, bind_group, rect, load, view);
    }

    /// One full-quad draw of `bind_group` into the viewport `rect` of `view`.
    fn draw(
        &self,
        pipeline: &wgpu::RenderPipeline,
        bind_group: &wgpu::BindGroup,
        rect: (f32, f32, f32, f32),
        load: wgpu::LoadOp<wgpu::Color>,
        view: &wgpu::TextureView,
    ) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let (x, y, w, h) = rect;
            pass.set_viewport(x, y, w, h, 0.0, 1.0);
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Binds a texture for the blit pipeline.
    pub fn bind_texture(&self, texture: &wgpu::Texture) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &texture.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Reads a texture back as tightly packed RGBA8 (or BGRA8, matching its format).
    pub fn read_texture(&self, texture: &wgpu::Texture) -> Result<Vec<u8>> {
        let (width, height) = (texture.width(), texture.height());
        let padded_row = (width * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read-back"),
            size: u64::from(padded_row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let data = slice.get_mapped_range()?;
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for row in data.chunks(padded_row as usize) {
            out.extend_from_slice(&row[..(width * 4) as usize]);
        }
        Ok(out)
    }

    /// Whether the surface format stores blue first (needed to write correct PNGs).
    pub fn surface_is_bgra(&self) -> bool {
        matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        )
    }
}

/// The blit pipeline, opaque or blended with `blend`.
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blit"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Largest rectangle of aspect `iw:ih` centred inside `ow×oh`, as `(x, y, w, h)`.
fn letterbox(ow: u32, oh: u32, iw: u32, ih: u32) -> (f32, f32, f32, f32) {
    let (ow, oh, iw, ih) = (ow as f32, oh as f32, iw as f32, ih as f32);
    let scale = (ow / iw).min(oh / ih);
    let (w, h) = (iw * scale, ih * scale);
    ((ow - w) * 0.5, (oh - h) * 0.5, w, h)
}

/// The application icon (Linux window icon; macOS uses the bundle's `.icns`).
pub fn window_icon() -> Result<winit::window::Icon> {
    let bytes: &[u8] = include_bytes!("../packaging/icon.iconset/icon_128x128.png");
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf)?;
    Ok(winit::window::Icon::from_rgba(
        buf[..info.buffer_size()].to_vec(),
        info.width,
        info.height,
    )?)
}

/// Writes an RGBA8 (or BGRA8 when `bgra`) image as PNG.
pub fn save_png(
    path: &std::path::Path,
    width: u32,
    height: u32,
    pixels: &[u8],
    bgra: bool,
) -> Result<()> {
    let mut rgba = pixels.to_vec();
    if bgra {
        rgba.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
    }
    let mut encoder = png::Encoder::new(std::fs::File::create(path)?, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::letterbox;

    #[test]
    fn letterbox_keeps_aspect_and_centres() {
        let close = |a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)| {
            let d = |x: f32, y: f32| (x - y).abs() < 1e-2;
            assert!(
                d(a.0, b.0) && d(a.1, b.1) && d(a.2, b.2) && d(a.3, b.3),
                "{a:?} != {b:?}"
            );
        };
        close(letterbox(1920, 1080, 800, 450), (0.0, 0.0, 1920.0, 1080.0));
        close(
            letterbox(1000, 1000, 800, 450),
            (0.0, 218.75, 1000.0, 562.5),
        );
        close(letterbox(1000, 100, 800, 450), (411.11, 0.0, 177.78, 100.0));
    }
}
