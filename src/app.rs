use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

use crate::renderer::{RenderError, RenderOutcome, Renderer};

const INITIAL_WINDOW_SIZE: PhysicalSize<u32> = PhysicalSize::new(960, 600);
const WINDOW_TITLE: &str = "Flash";

/// Owns the native window, renderer, and application event lifecycle.
pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    window_size: PhysicalSize<u32>,
    scale_factor: f64,
}

impl App {
    pub fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            window_size: INITIAL_WINDOW_SIZE,
            scale_factor: 1.0,
        }
    }

    fn window(&self) -> Option<&Arc<Window>> {
        self.window.as_ref()
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title(WINDOW_TITLE)
            .with_inner_size(INITIAL_WINDOW_SIZE)
            .with_resizable(true);

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                tracing::error!(%error, "failed to create native window");
                event_loop.exit();
                return;
            }
        };

        self.window_size = window.inner_size();
        self.scale_factor = window.scale_factor();
        tracing::info!(
            width = self.window_size.width,
            height = self.window_size.height,
            scale_factor = self.scale_factor,
            "native window created"
        );

        match pollster::block_on(Renderer::new(Arc::clone(&window), self.window_size)) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(error) => {
                tracing::error!(%error, "failed to initialize GPU renderer");
                event_loop.exit();
                return;
            }
        }

        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window().map(|window| window.id()) != Some(window_id) {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("close requested");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.window_size = size;
                tracing::debug!(width = size.width, height = size.height, "window resized");

                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                }

                if size.width > 0 && size.height > 0 {
                    self.window()
                        .expect("window was validated above")
                        .request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale_factor = scale_factor;
                self.window_size = self
                    .window()
                    .expect("window was validated above")
                    .inner_size();
                tracing::debug!(
                    width = self.window_size.width,
                    height = self.window_size.height,
                    scale_factor,
                    "window scale factor changed"
                );

                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(self.window_size);
                }

                self.window()
                    .expect("window was validated above")
                    .request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };

                match renderer.render() {
                    Ok(RenderOutcome::Presented) => {}
                    Ok(RenderOutcome::Reconfigured) => self
                        .window()
                        .expect("window was validated above")
                        .request_redraw(),
                    Err(RenderError::OutOfMemory) => {
                        tracing::error!("GPU ran out of memory; exiting");
                        event_loop.exit();
                    }
                }
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        tracing::info!("Flash is exiting");
    }
}
