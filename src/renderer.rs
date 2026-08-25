use std::{error::Error, fmt, mem, sync::Arc};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

use crate::{
    font::{ATLAS_SIZE, FontError, GlyphAtlas},
    terminal::{Color, RenderSnapshot},
};

const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.035,
    g: 0.04,
    b: 0.055,
    a: 1.0,
};
const DEFAULT_FOREGROUND: [f32; 4] = [0.9, 0.92, 0.96, 1.0];
const DEFAULT_BACKGROUND: [f32; 4] = [0.035, 0.04, 0.055, 1.0];
const CURSOR_COLOR: [f32; 4] = [0.55, 0.58, 0.65, 0.65];
const PADDING_X: f32 = 8.0;
const PADDING_Y: f32 = 8.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphInstance {
    position: [f32; 2],
    size: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewportUniform {
    size: [f32; 2],
    padding: [f32; 2],
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    atlas: GlyphAtlas,
    _atlas_texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    viewport_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instances: Vec<GlyphInstance>,
}

#[derive(Debug)]
pub enum RendererInitError {
    CreateSurface(wgpu::CreateSurfaceError),
    NoAdapter,
    RequestDevice(wgpu::RequestDeviceError),
    NoSurfaceFormat,
    NoPresentMode,
    NoAlphaMode,
    Font(FontError),
}

impl fmt::Display for RendererInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateSurface(error) => {
                write!(formatter, "could not create presentation surface: {error}")
            }
            Self::NoAdapter => formatter.write_str("no compatible GPU adapter was found"),
            Self::RequestDevice(error) => write!(formatter, "could not create GPU device: {error}"),
            Self::NoSurfaceFormat => {
                formatter.write_str("GPU surface exposed no supported texture formats")
            }
            Self::NoPresentMode => {
                formatter.write_str("GPU surface exposed no supported present modes")
            }
            Self::NoAlphaMode => {
                formatter.write_str("GPU surface exposed no supported alpha modes")
            }
            Self::Font(error) => write!(formatter, "could not initialize default font: {error}"),
        }
    }
}

impl Error for RendererInitError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderOutcome {
    Presented,
    Reconfigured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    OutOfMemory,
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the GPU ran out of memory")
    }
}
impl Error for RenderError {}

impl Renderer {
    pub async fn new(
        window: Arc<Window>,
        initial_size: PhysicalSize<u32>,
    ) -> Result<Self, RendererInitError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let surface = instance
            .create_surface(window)
            .map_err(RendererInitError::CreateSurface)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(RendererInitError::NoAdapter)?;
        let info = adapter.get_info();
        tracing::info!(name = %info.name, backend = ?info.backend, device_type = ?info.device_type, "selected GPU adapter");

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("Flash GPU device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(RendererInitError::RequestDevice)?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| capabilities.formats.first().copied())
            .ok_or(RendererInitError::NoSurfaceFormat)?;
        let present_mode = capabilities
            .present_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::PresentMode::Fifo)
            .or_else(|| capabilities.present_modes.first().copied())
            .ok_or(RendererInitError::NoPresentMode)?;
        let alpha_mode = capabilities
            .alpha_modes
            .first()
            .copied()
            .ok_or(RendererInitError::NoAlphaMode)?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: initial_size.width,
            height: initial_size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        let atlas = GlyphAtlas::load_default().map_err(RendererInitError::Font)?;
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Flash ASCII glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas.pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE),
                rows_per_image: Some(ATLAS_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
        );
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Flash glyph sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let viewport = ViewportUniform {
            size: [
                initial_size.width.max(1) as f32,
                initial_size.height.max(1) as f32,
            ],
            padding: [0.0; 2],
        };
        let viewport_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Flash viewport uniform"),
            contents: bytemuck::bytes_of(&viewport),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Flash glyph bind group layout"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Flash glyph bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: viewport_buffer.as_entire_binding(),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Flash glyph shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("renderer.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Flash glyph pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Flash glyph pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vertex_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: mem::size_of::<GlyphInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fragment_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        let instance_capacity = 2048;
        let instance_buffer = create_instance_buffer(&device, instance_capacity);

        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
            atlas,
            _atlas_texture: atlas_texture,
            bind_group,
            pipeline,
            viewport_buffer,
            instance_buffer,
            instance_capacity,
            instances: Vec::with_capacity(instance_capacity),
        };
        renderer.configure(initial_size);
        tracing::info!(format = ?renderer.config.format, present_mode = ?renderer.config.present_mode, "GPU text renderer initialized");
        Ok(renderer)
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.configure(size);
    }

    pub fn render(&mut self, snapshot: RenderSnapshot<'_>) -> Result<RenderOutcome, RenderError> {
        self.build_instances(snapshot);
        self.ensure_instance_capacity();
        if !self.instances.is_empty() {
            self.queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        }
        let surface_texture = match self.surface.get_current_texture() {
            Ok(texture) => texture,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                tracing::warn!("GPU surface became unavailable; reconfiguring it");
                self.surface.configure(&self.device, &self.config);
                return Ok(RenderOutcome::Reconfigured);
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(RenderOutcome::Presented),
            Err(wgpu::SurfaceError::OutOfMemory) => return Err(RenderError::OutOfMemory),
        };
        let view = surface_texture.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Flash frame encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Flash terminal pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
            pass.draw(0..6, 0..self.instances.len() as u32);
        }
        self.queue.submit([encoder.finish()]);
        surface_texture.present();
        Ok(RenderOutcome::Presented)
    }

    fn build_instances(&mut self, snapshot: RenderSnapshot<'_>) {
        self.instances.clear();

        for row in 0..snapshot.rows {
            for column in 0..snapshot.columns {
                let cell = snapshot.cells[row * snapshot.columns + column];
                let background = if cell.flags.inverse() {
                    cell.foreground
                } else {
                    cell.background
                };
                if background != Color::Default {
                    self.instances.push(GlyphInstance {
                        position: [
                            PADDING_X + column as f32 * self.atlas.cell_width,
                            PADDING_Y + row as f32 * self.atlas.cell_height,
                        ],
                        size: [self.atlas.cell_width, self.atlas.cell_height],
                        uv_min: self.atlas.solid_uv_min,
                        uv_max: self.atlas.solid_uv_max,
                        color: resolve_color(background, DEFAULT_BACKGROUND),
                    });
                }
            }
        }

        if snapshot.cursor_visible {
            self.instances.push(GlyphInstance {
                position: [
                    PADDING_X + snapshot.cursor.column as f32 * self.atlas.cell_width,
                    PADDING_Y + snapshot.cursor.row as f32 * self.atlas.cell_height,
                ],
                size: [self.atlas.cell_width, self.atlas.cell_height],
                uv_min: self.atlas.solid_uv_min,
                uv_max: self.atlas.solid_uv_max,
                color: CURSOR_COLOR,
            });
        }

        for row in 0..snapshot.rows {
            for column in 0..snapshot.columns {
                let cell = snapshot.cells[row * snapshot.columns + column];
                let foreground = if cell.flags.inverse() {
                    cell.background
                } else {
                    cell.foreground
                };
                if let Some(glyph) = self
                    .atlas
                    .glyph(cell.character)
                    .filter(|glyph| glyph.width > 0.0 && glyph.height > 0.0)
                {
                    self.instances.push(GlyphInstance {
                        position: [
                            PADDING_X + column as f32 * self.atlas.cell_width + glyph.x_offset,
                            PADDING_Y + row as f32 * self.atlas.cell_height + glyph.y_offset,
                        ],
                        size: [glyph.width, glyph.height],
                        uv_min: glyph.uv_min,
                        uv_max: glyph.uv_max,
                        color: resolve_color(foreground, DEFAULT_FOREGROUND),
                    });
                }
                if cell.flags.underline() {
                    self.instances.push(GlyphInstance {
                        position: [
                            PADDING_X + column as f32 * self.atlas.cell_width,
                            PADDING_Y + (row + 1) as f32 * self.atlas.cell_height - 2.0,
                        ],
                        size: [self.atlas.cell_width, 1.0],
                        uv_min: self.atlas.solid_uv_min,
                        uv_max: self.atlas.solid_uv_max,
                        color: resolve_color(foreground, DEFAULT_FOREGROUND),
                    });
                }
            }
        }
    }

    fn ensure_instance_capacity(&mut self) {
        if self.instances.len() <= self.instance_capacity {
            return;
        }
        self.instance_capacity = self.instances.len().next_power_of_two();
        self.instance_buffer = create_instance_buffer(&self.device, self.instance_capacity);
    }

    fn configure(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        let viewport = ViewportUniform {
            size: [size.width as f32, size.height as f32],
            padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.viewport_buffer, 0, bytemuck::bytes_of(&viewport));
    }
}

fn resolve_color(color: Color, default: [f32; 4]) -> [f32; 4] {
    match color {
        Color::Default => default,
        Color::Rgb(red, green, blue) => rgba(red, green, blue),
        Color::Indexed(index) if index < 16 => {
            const ANSI: [[u8; 3]; 16] = [
                [0, 0, 0],
                [205, 49, 49],
                [13, 188, 121],
                [229, 229, 16],
                [36, 114, 200],
                [188, 63, 188],
                [17, 168, 205],
                [229, 229, 229],
                [102, 102, 102],
                [241, 76, 76],
                [35, 209, 139],
                [245, 245, 67],
                [59, 142, 234],
                [214, 112, 214],
                [41, 184, 219],
                [255, 255, 255],
            ];
            let [red, green, blue] = ANSI[index as usize];
            rgba(red, green, blue)
        }
        Color::Indexed(index) if index < 232 => {
            let index = index - 16;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            rgba(
                component(index / 36),
                component(index / 6 % 6),
                component(index % 6),
            )
        }
        Color::Indexed(index) => {
            let value = 8 + (index - 232) * 10;
            rgba(value, value, value)
        }
    }
}

fn rgba(red: u8, green: u8, blue: u8) -> [f32; 4] {
    [
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
        1.0,
    ]
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Flash glyph instance buffer"),
        size: (capacity * mem::size_of::<GlyphInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_FOREGROUND, resolve_color, rgba};
    use crate::terminal::Color;

    #[test]
    fn resolves_default_truecolor_and_color_cube_entries() {
        assert_eq!(
            resolve_color(Color::Default, DEFAULT_FOREGROUND),
            DEFAULT_FOREGROUND
        );
        assert_eq!(
            resolve_color(Color::Rgb(1, 2, 3), DEFAULT_FOREGROUND),
            rgba(1, 2, 3)
        );
        assert_eq!(
            resolve_color(Color::Indexed(16), DEFAULT_FOREGROUND),
            rgba(0, 0, 0)
        );
        assert_eq!(
            resolve_color(Color::Indexed(231), DEFAULT_FOREGROUND),
            rgba(255, 255, 255)
        );
    }
}
