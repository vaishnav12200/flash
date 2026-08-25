use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

use crate::{
    event::AppEvent,
    pty::{self, PtyEvent, PtySession},
    renderer::{RenderError, RenderOutcome, Renderer},
};

const INITIAL_WINDOW_SIZE: PhysicalSize<u32> = PhysicalSize::new(960, 600);
const WINDOW_TITLE: &str = "Flash";

/// Owns the native window, renderer, PTY session, and application event lifecycle.
pub struct App {
    event_proxy: EventLoopProxy<AppEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty: Option<PtySession>,
    window_size: PhysicalSize<u32>,
    scale_factor: f64,
}

impl App {
    pub fn new(event_proxy: EventLoopProxy<AppEvent>) -> Self {
        Self {
            event_proxy,
            window: None,
            renderer: None,
            pty: None,
            window_size: INITIAL_WINDOW_SIZE,
            scale_factor: 1.0,
        }
    }

    fn window(&self) -> Option<&Arc<Window>> {
        self.window.as_ref()
    }

    fn drain_pty_events(&mut self, event_loop: &ActiveEventLoop) {
        let mut received_output = false;
        let mut reader_closed = false;

        if let Some(pty) = self.pty.as_ref() {
            pty.drain_events(|event| match event {
                PtyEvent::Output(bytes) => {
                    pty::log_output(&bytes);
                    received_output = true;
                }
                PtyEvent::EndOfFile => {
                    tracing::info!("PTY output stream closed");
                    reader_closed = true;
                }
                PtyEvent::ReaderError(error) => {
                    tracing::error!(%error, "PTY reader failed");
                    reader_closed = true;
                }
            });
        }

        if received_output {
            self.window()
                .expect("window exists while processing PTY events")
                .request_redraw();
        }

        let Some(pty) = self.pty.as_mut() else {
            return;
        };

        match pty.poll_child_exit() {
            Ok(Some(status)) => {
                tracing::info!(%status, "shell exited");
                event_loop.exit();
            }
            Ok(None) if reader_closed => {
                tracing::info!("PTY session ended");
                event_loop.exit();
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, "could not observe shell lifecycle");
                event_loop.exit();
            }
        }
    }

    fn forward_keyboard_input(&mut self, event: KeyEvent) {
        let Some(bytes) = terminal_input(&event) else {
            return;
        };
        let Some(pty) = self.pty.as_mut() else {
            return;
        };

        if let Err(error) = pty.write(bytes) {
            tracing::error!(%error, "could not forward keyboard input to PTY");
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
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

        match PtySession::spawn(self.event_proxy.clone()) {
            Ok(pty) => self.pty = Some(pty),
            Err(error) => {
                tracing::error!(%error, "failed to initialize PTY session");
                event_loop.exit();
                return;
            }
        }

        window.request_redraw();
        self.window = Some(window);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::PtyActivity => self.drain_pty_events(event_loop),
        }
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
            WindowEvent::KeyboardInput { event, .. } => self.forward_keyboard_input(event),
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
        self.pty.take();
        tracing::info!("Flash is exiting");
    }
}

fn terminal_input(event: &KeyEvent) -> Option<&[u8]> {
    terminal_input_bytes(event.state, &event.logical_key, event.text.as_deref())
}

fn terminal_input_bytes<'a>(
    state: ElementState,
    logical_key: &Key,
    text: Option<&'a str>,
) -> Option<&'a [u8]> {
    if state != ElementState::Pressed {
        return None;
    }

    match logical_key {
        Key::Named(NamedKey::Enter) => Some(b"\r"),
        Key::Named(NamedKey::Backspace) => Some(b"\x7f"),
        Key::Named(NamedKey::Tab) => Some(b"\t"),
        _ => text.map(str::as_bytes),
    }
}

#[cfg(test)]
mod tests {
    use winit::{
        event::ElementState,
        keyboard::{Key, NamedKey},
    };

    use super::terminal_input_bytes;

    #[test]
    fn encodes_text_and_basic_editing_keys() {
        assert_eq!(
            terminal_input_bytes(
                ElementState::Pressed,
                &Key::Character("a".into()),
                Some("a")
            ),
            Some(&b"a"[..])
        );
        assert_eq!(
            terminal_input_bytes(ElementState::Pressed, &Key::Named(NamedKey::Enter), None),
            Some(&b"\r"[..])
        );
        assert_eq!(
            terminal_input_bytes(
                ElementState::Pressed,
                &Key::Named(NamedKey::Backspace),
                None
            ),
            Some(&b"\x7f"[..])
        );
    }
}
