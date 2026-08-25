use std::{error::Error, fmt, sync::Arc};

use winit::{dpi::PhysicalSize, window::Window};

const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.035,
    g: 0.04,
    b: 0.055,
    a: 1.0,
};

/// GPU resources required to clear and present the native surface.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
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
        tracing::info!(
            name = %info.name,
            backend = ?info.backend,
            device_type = ?info.device_type,
            "selected GPU adapter"
        );

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

        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
        };
        renderer.configure(initial_size);

        tracing::info!(
            format = ?renderer.config.format,
            present_mode = ?renderer.config.present_mode,
            "GPU surface initialized"
        );

        Ok(renderer)
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.configure(size);
    }

    pub fn render(&mut self) -> Result<RenderOutcome, RenderError> {
        let surface_texture = match self.surface.get_current_texture() {
            Ok(surface_texture) => surface_texture,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                tracing::warn!("GPU surface became unavailable; reconfiguring it");
                self.surface.configure(&self.device, &self.config);
                return Ok(RenderOutcome::Reconfigured);
            }
            Err(wgpu::SurfaceError::Timeout) => {
                tracing::warn!("timed out while acquiring the GPU surface texture");
                return Ok(RenderOutcome::Presented);
            }
            Err(wgpu::SurfaceError::OutOfMemory) => return Err(RenderError::OutOfMemory),
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Flash clear-screen encoder"),
            });

        {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Flash clear-screen pass"),
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
        }

        self.queue.submit([encoder.finish()]);
        surface_texture.present();
        Ok(RenderOutcome::Presented)
    }

    fn configure(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            tracing::debug!("skipping GPU surface configuration for zero-sized window");
            return;
        }

        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }
}
