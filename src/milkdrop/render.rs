//! wgpu side of the MilkDrop engine: two feedback textures, one vertex format for everything
//! drawn into them, and the composite pass to the output texture.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::gpu::Gpu;

/// Vertex in clip space (`-1..1`), texture coordinates in MilkDrop space (`0..1`, v up).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

impl Vertex {
    pub fn new(pos: [f32; 2], color: [f32; 4]) -> Self {
        Self {
            pos,
            uv: [0.0; 2],
            color,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Blend {
    Replace,
    Alpha,
    Additive,
}

/// One batch of primitives; textured batches sample the previous frame.
pub struct Draw {
    pub verts: Vec<Vertex>,
    pub topology: wgpu::PrimitiveTopology,
    pub textured: bool,
    pub blend: Blend,
}

/// Uniforms of the composite pass; must match `Comp` in `shaders.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CompParams {
    pub echo_zoom: f32,
    pub echo_alpha: f32,
    pub echo_orient: f32,
    pub gamma: f32,
    pub brighten: f32,
    pub darken: f32,
    pub solarize: f32,
    pub invert: f32,
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

type PipelineKey = (wgpu::PrimitiveTopology, bool, Blend);

pub struct Renderer {
    pub width: u32,
    pub height: u32,
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    /// `[texture][sampler]` bind groups; sampler 0 wraps, sampler 1 clamps.
    binds: [[wgpu::BindGroup; 2]; 2],
    views: [wgpu::TextureView; 2],
    /// Index of the texture being drawn this frame; the other one holds the previous frame.
    target: usize,
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,
    comp_pipeline: wgpu::RenderPipeline,
    comp_uniform: wgpu::Buffer,
    comp_binds: [wgpu::BindGroup; 2],
    out_view: wgpu::TextureView,
    /// Bind group of the output texture for `Gpu::blit`.
    pub out_bind: wgpu::BindGroup,
}

impl Renderer {
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("milkdrop"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders.wgsl").into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let texture_ty = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("milkdrop"),
            entries: &[
                entry(0, texture_ty),
                entry(
                    1,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
                entry(
                    2,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("milkdrop"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let sampler = |mode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: mode,
                address_mode_v: mode,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            })
        };
        let samplers = [
            sampler(wgpu::AddressMode::Repeat),
            sampler(wgpu::AddressMode::ClampToEdge),
        ];
        let comp_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("comp"),
            size: std::mem::size_of::<CompParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let texture = |label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        };
        let textures = [texture("feedback 0"), texture("feedback 1")];
        let views = [
            textures[0].create_view(&Default::default()),
            textures[1].create_view(&Default::default()),
        ];
        let bind = |view: &wgpu::TextureView, sampler: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("milkdrop"),
                layout: &bind_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: comp_uniform.as_entire_binding(),
                    },
                ],
            })
        };
        let binds = [
            [bind(&views[0], &samplers[0]), bind(&views[0], &samplers[1])],
            [bind(&views[1], &samplers[0]), bind(&views[1], &samplers[1])],
        ];
        let comp_binds = [bind(&views[0], &samplers[1]), bind(&views[1], &samplers[1])];
        let out = texture("composite");
        let out_view = out.create_view(&Default::default());
        let out_bind = gpu.bind_texture(&out);
        let comp_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("comp"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_comp"),
                compilation_options: Default::default(),
                targets: &[Some(FORMAT.into())],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            width,
            height,
            shader,
            layout,
            binds,
            views,
            target: 0,
            pipelines: HashMap::new(),
            comp_pipeline,
            comp_uniform,
            comp_binds,
            out_view,
            out_bind,
        }
    }

    fn pipeline(&mut self, gpu: &Gpu, key: PipelineKey) -> &wgpu::RenderPipeline {
        let (topology, textured, blend) = key;
        self.pipelines.entry(key).or_insert_with(|| {
            let blend_state = match blend {
                Blend::Replace => wgpu::BlendState::REPLACE,
                Blend::Alpha => wgpu::BlendState::ALPHA_BLENDING,
                Blend::Additive => wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::SrcAlpha,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::OVER,
                },
            };
            gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("milkdrop prim"),
                layout: Some(&self.layout),
                vertex: wgpu::VertexState {
                    module: &self.shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.shader,
                    entry_point: Some(if textured { "fs_tex" } else { "fs_color" }),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: Some(blend_state),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        })
    }

    /// Draws `draws` in order into this frame's texture (cleared first), sampling the previous
    /// frame with wrapping or clamping as `wrap` says; then composites into the output texture.
    pub fn frame(&mut self, gpu: &Gpu, draws: &[Draw], wrap: bool, comp: CompParams) {
        self.target ^= 1;
        let prev = self.target ^ 1;
        // Pipelines are created lazily; make sure they exist before the pass borrows `self`.
        for d in draws {
            self.pipeline(gpu, (d.topology, d.textured, d.blend));
        }
        let buffers: Vec<wgpu::Buffer> = draws
            .iter()
            .map(|d| {
                gpu.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: bytemuck::cast_slice(&d.verts),
                        usage: wgpu::BufferUsages::VERTEX,
                    })
            })
            .collect();
        gpu.queue
            .write_buffer(&self.comp_uniform, 0, bytemuck::bytes_of(&comp));
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("milkdrop frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.views[self.target],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_bind_group(0, &self.binds[prev][usize::from(!wrap)], &[]);
            for (d, buffer) in draws.iter().zip(&buffers) {
                pass.set_pipeline(&self.pipelines[&(d.topology, d.textured, d.blend)]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..d.verts.len() as u32, 0..1);
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("milkdrop comp"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.out_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.comp_pipeline);
            pass.set_bind_group(0, &self.comp_binds[self.target], &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
    }
}
