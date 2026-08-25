use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

const INITIAL_WINDOW_SIZE: PhysicalSize<u32> = PhysicalSize::new(960, 600);
const WINDOW_TITLE: &str = "Flash";

/// Owns the native window lifecycle for the application.
///
/// Terminal state and rendering are intentionally not introduced until later
/// phases. Keeping this layer limited to platform events gives those systems a
/// clear integration boundary.
pub struct App {
    window: Option<Window>,
    window_size: PhysicalSize<u32>,
    scale_factor: f64,
}

impl App {
    pub fn new() -> Self {
        Self {
            window: None,
            window_size: INITIAL_WINDOW_SIZE,
            scale_factor: 1.0,
        }
    }

    fn window(&self) -> Option<&Window> {
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

        match event_loop.create_window(attributes) {
            Ok(window) => {
                self.window_size = window.inner_size();
                self.scale_factor = window.scale_factor();
                tracing::info!(
                    width = self.window_size.width,
                    height = self.window_size.height,
                    scale_factor = self.scale_factor,
                    "native window created"
                );

                window.request_redraw();
                self.window = Some(window);
            }
            Err(error) => {
                tracing::error!(%error, "failed to create native window");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window().map(Window::id) != Some(window_id) {
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
                self.window()
                    .expect("window was validated above")
                    .request_redraw();
            }
            WindowEvent::RedrawRequested => {
                tracing::trace!(
                    width = self.window_size.width,
                    height = self.window_size.height,
                    "redraw requested"
                );
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        tracing::info!("Flash is exiting");
    }
}
