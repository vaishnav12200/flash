use std::{error::Error, fmt, mem, path::PathBuf, sync::Arc};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event_loop::EventLoopProxy,
    window::Window,
};

use crate::{
    config::{CursorStyle, srgb_to_linear},
    event::AppEvent,
    font::{ATLAS_SIZE, AtlasRegion, FontError, GlyphAtlas},
    terminal::{Cell, Color, Cursor, GridSize, RenderSnapshot},
};

#[derive(Debug, Clone)]
pub struct RendererSettings {
    pub font_path: PathBuf,
    pub fallback_font_paths: Vec<PathBuf>,
    pub font_size: f32,
    pub padding_x: f32,
    pub padding_y: f32,
    pub foreground: [f32; 4],
    pub background: [f32; 4],
    pub cursor: [f32; 4],
    pub cursor_style: CursorStyle,
    pub selection_background: [f32; 4],
    pub selection_foreground: [f32; 4],
    pub ansi: [[f32; 4]; 16],
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Pod, Zeroable)]
struct GlyphInstance {
    position: [f32; 2],
    size: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    color: [f32; 4],
    style: [f32; 2],
}

#[derive(Default)]
struct RowInstances {
    backgrounds: Vec<GlyphInstance>,
    selections: Vec<GlyphInstance>,
    glyphs: Vec<GlyphInstance>,
    cursor: Vec<GlyphInstance>,
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
    atlas_texture: wgpu::Texture,
    atlas_upload: Vec<u8>,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    viewport_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instances: Vec<GlyphInstance>,
    staged_instances: Vec<GlyphInstance>,
    row_instances: Vec<RowInstances>,
    row_versions: Vec<u64>,
    cached_columns: usize,
    surface_configured: bool,
    settings: RendererSettings,
    scale_factor: f64,
    event_proxy: EventLoopProxy<AppEvent>,
}

#[derive(Debug)]
pub enum RendererInitError {
    CreateSurface(wgpu::CreateSurfaceError),
    NoAdapter,
    RequestDevice(wgpu::RequestDeviceError),
    NoSurfaceFormat,
    NoPresentMode,
    NoAlphaMode,
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
        }
    }
}

impl Error for RendererInitError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderOutcome {
    Presented,
    Reconfigured,
    Skipped,
    TimedOut,
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
        scale_factor: f64,
        event_proxy: EventLoopProxy<AppEvent>,
        settings: RendererSettings,
        atlas: GlyphAtlas,
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
        let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Float32x4, 5 => Float32x2];
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
            atlas_texture,
            atlas_upload: Vec::new(),
            bind_group,
            pipeline,
            viewport_buffer,
            instance_buffer,
            instance_capacity,
            instances: Vec::with_capacity(instance_capacity),
            staged_instances: Vec::with_capacity(instance_capacity),
            row_instances: Vec::new(),
            row_versions: Vec::new(),
            cached_columns: 0,
            surface_configured: false,
            settings,
            scale_factor,
            event_proxy,
        };
        renderer.configure(initial_size);
        tracing::info!(format = ?renderer.config.format, present_mode = ?renderer.config.present_mode, "GPU text renderer initialized");
        Ok(renderer)
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.configure(size);
    }

    pub fn update_scale_factor(&mut self, scale_factor: f64) -> Result<(), FontError> {
        if (scale_factor - self.scale_factor).abs() < f64::EPSILON {
            return Ok(());
        }
        let atlas = GlyphAtlas::load(
            &self.settings.font_path,
            &self.settings.fallback_font_paths,
            self.settings.font_size,
            scale_factor,
            Some(self.event_proxy.clone()),
        )?;
        let scale_ratio = (scale_factor / self.scale_factor) as f32;
        self.upload_atlas(atlas);
        self.settings.padding_x *= scale_ratio;
        self.settings.padding_y *= scale_ratio;
        self.scale_factor = scale_factor;
        Ok(())
    }

    pub fn update_font_size(&mut self, font_size: f32) -> Result<(), FontError> {
        let atlas = GlyphAtlas::load(
            &self.settings.font_path,
            &self.settings.fallback_font_paths,
            font_size,
            self.scale_factor,
            Some(self.event_proxy.clone()),
        )?;
        self.upload_atlas(atlas);
        self.settings.font_size = font_size;
        Ok(())
    }

    pub fn cell_at(&self, position: PhysicalPosition<f64>, size: GridSize) -> Option<Cursor> {
        let x = position.x as f32 - self.settings.padding_x;
        let y = position.y as f32 - self.settings.padding_y;
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let column = (x / self.atlas.cell_width).floor() as usize;
        let row = (y / self.atlas.cell_height).floor() as usize;
        (row < size.rows && column < size.columns).then_some(Cursor { row, column })
    }

    fn upload_atlas(&mut self, atlas: GlyphAtlas) {
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.atlas_texture,
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
        self.atlas = atlas;
        self.row_instances.clear();
        self.row_versions.clear();
    }

    pub fn grid_size(&self, size: PhysicalSize<u32>) -> GridSize {
        grid_size_for_metrics(
            size,
            self.atlas.cell_width,
            self.atlas.cell_height,
            self.settings.padding_x,
            self.settings.padding_y,
        )
    }

    pub fn content_size(&self, size: PhysicalSize<u32>) -> PhysicalSize<u32> {
        content_size(size, self.settings.padding_x, self.settings.padding_y)
    }

    pub fn render(&mut self, snapshot: RenderSnapshot<'_>) -> Result<RenderOutcome, RenderError> {
        if !self.surface_configured {
            return Ok(RenderOutcome::Skipped);
        }
        let fallback_changed = self.atlas.drain_fallbacks() > 0;
        self.build_instances(snapshot, fallback_changed);
        if let Some(region) = self.atlas.take_dirty_region() {
            self.upload_atlas_region(region);
        }
        let surface_texture = match self.surface.get_current_texture() {
            Ok(texture) => texture,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                tracing::warn!("GPU surface became unavailable; reconfiguring it");
                self.surface.configure(&self.device, &self.config);
                return Ok(RenderOutcome::Reconfigured);
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(RenderOutcome::TimedOut),
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
                        load: wgpu::LoadOp::Clear(wgpu_color(self.settings.background)),
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
            let content =
                self.content_size(PhysicalSize::new(self.config.width, self.config.height));
            let scissor_x = self.settings.padding_x.max(0.0) as u32;
            let scissor_y = self.settings.padding_y.max(0.0) as u32;
            if content.width > 0 && content.height > 0 {
                pass.set_scissor_rect(scissor_x, scissor_y, content.width, content.height);
            }
            pass.draw(0..6, 0..self.instances.len() as u32);
        }
        self.queue.submit([encoder.finish()]);
        surface_texture.present();
        Ok(RenderOutcome::Presented)
    }

    fn build_instances(&mut self, snapshot: RenderSnapshot<'_>, force_rebuild: bool) {
        let dimensions_changed = self.row_instances.len() != snapshot.rows
            || self.row_versions.len() != snapshot.rows
            || self.cached_columns != snapshot.columns;
        if dimensions_changed {
            self.row_instances
                .resize_with(snapshot.rows, RowInstances::default);
            self.row_versions.resize(snapshot.rows, 0);
            self.cached_columns = snapshot.columns;
        }

        let mut dirty_row_count = 0;
        for row in 0..snapshot.rows {
            if force_rebuild
                || dimensions_changed
                || self.row_versions[row] != snapshot.row_versions[row]
            {
                let mut instances = mem::take(&mut self.row_instances[row]);
                self.build_row_instances(snapshot, row, &mut instances);
                self.row_instances[row] = instances;
                self.row_versions[row] = snapshot.row_versions[row];
                dirty_row_count += 1;
            }
        }
        if dirty_row_count == 0 {
            return;
        }

        self.staged_instances.clear();
        for row in &self.row_instances {
            self.staged_instances.extend_from_slice(&row.backgrounds);
        }
        for row in &self.row_instances {
            self.staged_instances.extend_from_slice(&row.selections);
        }
        for row in &self.row_instances {
            self.staged_instances.extend_from_slice(&row.glyphs);
        }
        for row in &self.row_instances {
            self.staged_instances.extend_from_slice(&row.cursor);
        }
        self.upload_changed_instances(dirty_row_count, snapshot.rows);
    }

    fn build_row_instances(
        &mut self,
        snapshot: RenderSnapshot<'_>,
        row: usize,
        instances: &mut RowInstances,
    ) {
        instances.backgrounds.clear();
        instances.selections.clear();
        instances.glyphs.clear();
        instances.cursor.clear();
        for column in 0..snapshot.columns {
            let cell = snapshot.cells[row * snapshot.columns + column];
            let (_, background) = resolve_cell_colors(
                cell,
                self.settings.foreground,
                self.settings.background,
                &self.settings.ansi,
            );
            if cell.background != Color::Default || cell.flags.inverse() {
                instances.backgrounds.push(GlyphInstance {
                    position: [
                        self.settings.padding_x + column as f32 * self.atlas.cell_width,
                        self.settings.padding_y + row as f32 * self.atlas.cell_height,
                    ],
                    size: [self.atlas.cell_width, self.atlas.cell_height],
                    uv_min: self.atlas.solid_uv_min,
                    uv_max: self.atlas.solid_uv_max,
                    color: background,
                    style: [0.0; 2],
                });
            }
        }

        if let Some(selection) = snapshot.selection {
            for column in 0..snapshot.columns {
                let cell = snapshot.cells[row * snapshot.columns + column];
                let selected = selection.contains(row, column)
                    || (cell.is_continuation()
                        && column > 0
                        && selection.contains(row, column - 1))
                    || (cell.width.columns() == 2
                        && column + 1 < snapshot.columns
                        && selection.contains(row, column + 1));
                if selected {
                    instances.selections.push(GlyphInstance {
                        position: [
                            self.settings.padding_x + column as f32 * self.atlas.cell_width,
                            self.settings.padding_y + row as f32 * self.atlas.cell_height,
                        ],
                        size: [self.atlas.cell_width, self.atlas.cell_height],
                        uv_min: self.atlas.solid_uv_min,
                        uv_max: self.atlas.solid_uv_max,
                        color: self.settings.selection_background,
                        style: [0.0; 2],
                    });
                }
            }
        }

        if snapshot.cursor_visible && snapshot.cursor.row == row {
            let mut cursor_column = snapshot.cursor.column;
            let cursor_cell = snapshot.cells[row * snapshot.columns + cursor_column];
            if cursor_cell.is_continuation() && cursor_column > 0 {
                cursor_column -= 1;
            }
            let cursor_width = snapshot.cells[row * snapshot.columns + cursor_column]
                .width
                .columns()
                .max(1) as f32;
            let cell_position = [
                self.settings.padding_x + cursor_column as f32 * self.atlas.cell_width,
                self.settings.padding_y + row as f32 * self.atlas.cell_height,
            ];
            let (position, size) = cursor_geometry(
                self.settings.cursor_style,
                cell_position,
                self.atlas.cell_width * cursor_width,
                self.atlas.cell_height,
            );
            let mut color = self.settings.cursor;
            if self.settings.cursor_style == CursorStyle::Block {
                color[3] = 0.68;
            }
            instances.cursor.push(GlyphInstance {
                position,
                size,
                uv_min: self.atlas.solid_uv_min,
                uv_max: self.atlas.solid_uv_max,
                color,
                style: [0.0; 2],
            });
        }

        for column in 0..snapshot.columns {
            let cell = snapshot.cells[row * snapshot.columns + column];
            if cell.is_continuation() {
                continue;
            }
            let foreground = resolve_glyph_foreground(
                cell,
                selection_contains_cell(snapshot, row, column),
                self.settings.foreground,
                self.settings.background,
                self.settings.selection_foreground,
                &self.settings.ansi,
            );
            let cell_width = self.atlas.cell_width;
            let cell_height = self.atlas.cell_height;
            let occupied_width = cell.width.columns() as f32 * cell_width;
            for character in cell.characters().filter(|character| {
                !matches!(
                    *character,
                    '\u{200c}' | '\u{200d}' | '\u{fe00}'..='\u{fe0f}' | '\u{e0100}'..='\u{e01ef}'
                )
            }) {
                if let Some(glyph) = self
                    .atlas
                    .glyph(character)
                    .filter(|glyph| glyph.width > 0.0 && glyph.height > 0.0)
                {
                    let centered_advance = (occupied_width - glyph.advance_width) * 0.5;
                    instances.glyphs.push(GlyphInstance {
                        position: [
                            self.settings.padding_x
                                + column as f32 * cell_width
                                + centered_advance
                                + glyph.x_offset,
                            self.settings.padding_y + row as f32 * cell_height + glyph.y_offset,
                        ],
                        size: [glyph.width, glyph.height],
                        uv_min: glyph.uv_min,
                        uv_max: glyph.uv_max,
                        color: style_color(foreground, cell.flags.dim()),
                        style: [
                            f32::from(cell.flags.bold()),
                            if cell.flags.italic() { 0.16 } else { 0.0 },
                        ],
                    });
                }
            }
            if cell.flags.underline() {
                instances.glyphs.push(GlyphInstance {
                    position: [
                        self.settings.padding_x + column as f32 * self.atlas.cell_width,
                        self.settings.padding_y + (row + 1) as f32 * cell_height - 2.0,
                    ],
                    size: [occupied_width, 1.0],
                    uv_min: self.atlas.solid_uv_min,
                    uv_max: self.atlas.solid_uv_max,
                    color: style_color(foreground, cell.flags.dim()),
                    style: [0.0; 2],
                });
            }
        }
    }

    fn upload_changed_instances(&mut self, dirty_rows: usize, total_rows: usize) {
        let reallocated = self.ensure_instance_capacity(self.staged_instances.len());
        let mut uploaded_instance_count = 0;
        let mut write_count = 0;
        if reallocated && !self.staged_instances.is_empty() {
            self.queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.staged_instances),
            );
            uploaded_instance_count = self.staged_instances.len();
            write_count = 1;
        } else if !reallocated {
            let common_len = self.instances.len().min(self.staged_instances.len());
            let mut change_run_count = 0;
            let mut index = 0;
            while index < common_len {
                if self.instances[index] == self.staged_instances[index] {
                    index += 1;
                    continue;
                }
                change_run_count += 1;
                while index < common_len && self.instances[index] != self.staged_instances[index] {
                    index += 1;
                }
            }

            if change_run_count > 8 {
                let first_changed = self
                    .instances
                    .iter()
                    .zip(&self.staged_instances)
                    .position(|(old, new)| old != new)
                    .unwrap_or(common_len);
                if first_changed < self.staged_instances.len() {
                    self.write_instance_range(first_changed, self.staged_instances.len());
                    uploaded_instance_count = self.staged_instances.len() - first_changed;
                    write_count = 1;
                }
            } else {
                index = 0;
                while index < common_len {
                    if self.instances[index] == self.staged_instances[index] {
                        index += 1;
                        continue;
                    }
                    let start = index;
                    while index < common_len
                        && self.instances[index] != self.staged_instances[index]
                    {
                        index += 1;
                    }
                    self.write_instance_range(start, index);
                    uploaded_instance_count += index - start;
                    write_count += 1;
                }
                if self.staged_instances.len() > common_len {
                    self.write_instance_range(common_len, self.staged_instances.len());
                    uploaded_instance_count += self.staged_instances.len() - common_len;
                    write_count += 1;
                }
            }
        }
        tracing::debug!(
            dirty_rows,
            total_rows,
            instance_count = self.staged_instances.len(),
            uploaded_instance_count,
            write_count,
            "renderer instance cache updated"
        );
        mem::swap(&mut self.instances, &mut self.staged_instances);
    }

    fn write_instance_range(&self, start: usize, end: usize) {
        self.queue.write_buffer(
            &self.instance_buffer,
            (start * mem::size_of::<GlyphInstance>()) as u64,
            bytemuck::cast_slice(&self.staged_instances[start..end]),
        );
    }

    fn upload_atlas_region(&mut self, region: AtlasRegion) {
        let byte_count = (region.width * region.height) as usize;
        self.atlas_upload.resize(byte_count, 0);
        for row in 0..region.height as usize {
            let source_start = (region.y as usize + row) * ATLAS_SIZE as usize + region.x as usize;
            let destination_start = row * region.width as usize;
            self.atlas_upload[destination_start..destination_start + region.width as usize]
                .copy_from_slice(
                    &self.atlas.pixels[source_start..source_start + region.width as usize],
                );
        }
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: region.x,
                    y: region.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &self.atlas_upload,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(region.width),
                rows_per_image: Some(region.height),
            },
            wgpu::Extent3d {
                width: region.width,
                height: region.height,
                depth_or_array_layers: 1,
            },
        );
        tracing::debug!(
            x = region.x,
            y = region.y,
            width = region.width,
            height = region.height,
            byte_count,
            "uploaded glyph atlas damage"
        );
    }

    fn ensure_instance_capacity(&mut self, required: usize) -> bool {
        if required <= self.instance_capacity {
            return false;
        }
        self.instance_capacity = required.next_power_of_two();
        self.instance_buffer = create_instance_buffer(&self.device, self.instance_capacity);
        true
    }

    fn configure(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            self.surface_configured = false;
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.surface_configured = true;
        let viewport = ViewportUniform {
            size: [size.width as f32, size.height as f32],
            padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.viewport_buffer, 0, bytemuck::bytes_of(&viewport));
    }
}

pub(crate) fn content_size(
    size: PhysicalSize<u32>,
    padding_x: f32,
    padding_y: f32,
) -> PhysicalSize<u32> {
    PhysicalSize::new(
        size.width.saturating_sub((padding_x * 2.0) as u32),
        size.height.saturating_sub((padding_y * 2.0) as u32),
    )
}

pub(crate) fn grid_size_for_metrics(
    size: PhysicalSize<u32>,
    cell_width: f32,
    cell_height: f32,
    padding_x: f32,
    padding_y: f32,
) -> GridSize {
    let content = content_size(size, padding_x, padding_y);
    GridSize {
        rows: ((content.height as f32 / cell_height).floor() as usize).max(1),
        columns: ((content.width as f32 / cell_width).floor() as usize).max(1),
    }
}

fn resolve_color(color: Color, default: [f32; 4], ansi: &[[f32; 4]; 16]) -> [f32; 4] {
    match color {
        Color::Default => default,
        Color::Rgb(red, green, blue) => rgba(red, green, blue),
        Color::Indexed(index) if index < 16 => ansi[index as usize],
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

fn resolve_cell_colors(
    cell: Cell,
    default_foreground: [f32; 4],
    default_background: [f32; 4],
    ansi: &[[f32; 4]; 16],
) -> ([f32; 4], [f32; 4]) {
    let foreground = resolve_color(cell.foreground, default_foreground, ansi);
    let background = resolve_color(cell.background, default_background, ansi);
    if cell.flags.inverse() {
        (background, foreground)
    } else {
        (foreground, background)
    }
}

fn resolve_glyph_foreground(
    cell: Cell,
    selected: bool,
    default_foreground: [f32; 4],
    default_background: [f32; 4],
    selection_foreground: [f32; 4],
    ansi: &[[f32; 4]; 16],
) -> [f32; 4] {
    if selected {
        selection_foreground
    } else {
        resolve_cell_colors(cell, default_foreground, default_background, ansi).0
    }
}

fn wgpu_color(color: [f32; 4]) -> wgpu::Color {
    wgpu::Color {
        r: color[0] as f64,
        g: color[1] as f64,
        b: color[2] as f64,
        a: color[3] as f64,
    }
}

fn style_color(mut color: [f32; 4], dim: bool) -> [f32; 4] {
    if dim {
        color[0] *= 0.66;
        color[1] *= 0.66;
        color[2] *= 0.66;
    }
    color
}

fn rgba(red: u8, green: u8, blue: u8) -> [f32; 4] {
    [
        srgb_to_linear(red),
        srgb_to_linear(green),
        srgb_to_linear(blue),
        1.0,
    ]
}

fn selection_contains_cell(snapshot: RenderSnapshot<'_>, row: usize, column: usize) -> bool {
    let Some(selection) = snapshot.selection else {
        return false;
    };
    let cell = snapshot.cells[row * snapshot.columns + column];
    selection.contains(row, column)
        || (cell.is_continuation() && column > 0 && selection.contains(row, column - 1))
        || (cell.width.columns() == 2
            && column + 1 < snapshot.columns
            && selection.contains(row, column + 1))
}

fn cursor_geometry(
    style: CursorStyle,
    position: [f32; 2],
    width: f32,
    height: f32,
) -> ([f32; 2], [f32; 2]) {
    match style {
        CursorStyle::Block => (position, [width, height]),
        CursorStyle::Beam => {
            let thickness = (width * 0.16).clamp(2.0, 3.0).min(width);
            (position, [thickness, height])
        }
        CursorStyle::Underline => {
            let thickness = (height * 0.1).clamp(2.0, 3.0).min(height);
            (
                [position[0], position[1] + height - thickness],
                [width, thickness],
            )
        }
    }
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
    use super::{
        cursor_geometry, grid_size_for_metrics, resolve_cell_colors, resolve_color,
        resolve_glyph_foreground, rgba,
    };
    use crate::config::{Config, CursorStyle};
    use crate::terminal::{Color, GridSize, Terminal, TerminalParser};
    use winit::dpi::PhysicalSize;

    #[test]
    fn calculates_grid_after_removing_window_padding() {
        assert_eq!(
            grid_size_for_metrics(PhysicalSize::new(816, 456), 10.0, 20.0, 8.0, 8.0),
            GridSize {
                rows: 22,
                columns: 80
            }
        );
        assert_eq!(
            grid_size_for_metrics(PhysicalSize::new(1, 1), 10.0, 20.0, 8.0, 8.0),
            GridSize {
                rows: 1,
                columns: 1
            }
        );
    }

    #[test]
    fn resolves_default_truecolor_and_color_cube_entries() {
        const DEFAULT: [f32; 4] = [0.9, 0.92, 0.96, 1.0];
        let ansi = Config::default().visual_colors().unwrap().ansi;
        assert_eq!(resolve_color(Color::Default, DEFAULT, &ansi), DEFAULT);
        assert_eq!(
            resolve_color(Color::Rgb(1, 2, 3), DEFAULT, &ansi),
            rgba(1, 2, 3)
        );
        assert_eq!(
            resolve_color(Color::Indexed(16), DEFAULT, &ansi),
            rgba(0, 0, 0)
        );
        assert_eq!(
            resolve_color(Color::Indexed(231), DEFAULT, &ansi),
            rgba(255, 255, 255)
        );
    }

    #[test]
    fn ansi_indices_resolve_from_the_flash_palette() {
        let colors = Config::default().visual_colors().unwrap();
        assert_eq!(
            resolve_color(Color::Indexed(6), colors.foreground, &colors.ansi),
            colors.ansi[6]
        );
        assert_ne!(colors.ansi[6], colors.ansi[14]);
    }

    #[test]
    fn inverse_cells_swap_resolved_default_color_roles() {
        const FOREGROUND: [f32; 4] = [0.9, 0.8, 0.7, 1.0];
        const BACKGROUND: [f32; 4] = [0.1, 0.2, 0.3, 1.0];
        let mut terminal = Terminal::new(1, 1);
        TerminalParser::default().process(&mut terminal, b"\x1b[7mX");
        let ansi = Config::default().visual_colors().unwrap().ansi;
        assert_eq!(
            resolve_cell_colors(
                terminal.render_snapshot().cells[0],
                FOREGROUND,
                BACKGROUND,
                &ansi
            ),
            (BACKGROUND, FOREGROUND)
        );
    }

    #[test]
    fn cursor_styles_have_balanced_cell_relative_geometry() {
        assert_eq!(
            cursor_geometry(CursorStyle::Block, [14.0, 12.0], 22.0, 24.0),
            ([14.0, 12.0], [22.0, 24.0])
        );
        assert_eq!(
            cursor_geometry(CursorStyle::Beam, [14.0, 12.0], 11.0, 24.0),
            ([14.0, 12.0], [2.0, 24.0])
        );
        assert_eq!(
            cursor_geometry(CursorStyle::Underline, [14.0, 12.0], 22.0, 24.0),
            ([14.0, 33.6], [22.0, 2.4])
        );
    }

    #[test]
    fn selection_foreground_overrides_ansi_without_mutating_the_cell() {
        let colors = Config::default().visual_colors().unwrap();
        let mut terminal = Terminal::new(1, 1);
        TerminalParser::default().process(&mut terminal, b"\x1b[31mX");
        let cell = terminal.render_snapshot().cells[0];
        assert_eq!(cell.foreground, Color::Indexed(1));
        assert_eq!(
            resolve_glyph_foreground(
                cell,
                true,
                colors.foreground,
                colors.background,
                colors.selection_foreground,
                &colors.ansi,
            ),
            colors.selection_foreground
        );
        assert_eq!(cell.foreground, Color::Indexed(1));
    }
}
