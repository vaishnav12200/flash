use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::ModifiersState,
    window::{Window, WindowId},
};

use crate::{
    config::{Config, ShortcutAction, ShortcutMap, parse_color},
    event::AppEvent,
    input,
    pty::{self, PtyDimensions, PtyEvent, PtySession},
    renderer::{RenderError, RenderOutcome, Renderer, RendererSettings},
    terminal::{Terminal, TerminalParser},
};

const INITIAL_WINDOW_SIZE: PhysicalSize<u32> = PhysicalSize::new(960, 600);
const WINDOW_TITLE: &str = "Flash";
const FONT_SIZE_STEP: f32 = 2.0;
const MIN_FONT_SIZE: f32 = 6.0;
const MAX_FONT_SIZE: f32 = 72.0;

/// Owns the native window, renderer, PTY session, and application event lifecycle.
pub struct App {
    event_proxy: EventLoopProxy<AppEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty: Option<PtySession>,
    terminal: Terminal,
    terminal_parser: TerminalParser,
    window_size: PhysicalSize<u32>,
    scale_factor: f64,
    config: Config,
    shortcuts: ShortcutMap,
    modifiers: ModifiersState,
    clipboard: Option<arboard::Clipboard>,
    cursor_position: PhysicalPosition<f64>,
    selecting: bool,
    logical_font_size: f32,
}

impl App {
    pub fn new(event_proxy: EventLoopProxy<AppEvent>, config: Config) -> Self {
        let shortcuts = ShortcutMap::from_config(&config.keybindings)
            .expect("loaded or default configuration has valid shortcuts");
        let mut terminal = Terminal::new(24, 80);
        terminal.set_scrollback_limit(config.scrollback.lines);
        let clipboard = match arboard::Clipboard::new() {
            Ok(clipboard) => Some(clipboard),
            Err(error) => {
                tracing::warn!(%error, "Wayland clipboard is unavailable");
                None
            }
        };
        Self {
            event_proxy,
            window: None,
            renderer: None,
            pty: None,
            terminal,
            terminal_parser: TerminalParser::default(),
            window_size: INITIAL_WINDOW_SIZE,
            scale_factor: 1.0,
            logical_font_size: config.font.size,
            config,
            shortcuts,
            modifiers: ModifiersState::empty(),
            clipboard,
            cursor_position: PhysicalPosition::new(0.0, 0.0),
            selecting: false,
        }
    }

    fn window(&self) -> Option<&Arc<Window>> {
        self.window.as_ref()
    }

    fn desired_pty_dimensions(&self) -> Option<PtyDimensions> {
        let renderer = self.renderer.as_ref()?;
        let content = renderer.content_size(self.window_size);
        Some(PtyDimensions {
            grid: renderer.grid_size(self.window_size),
            pixel_width: content.width,
            pixel_height: content.height,
        })
    }

    fn synchronize_terminal_size(&mut self) {
        let Some(dimensions) = self.desired_pty_dimensions() else {
            return;
        };
        if !self.terminal.resize(dimensions.grid) {
            return;
        }

        tracing::info!(
            rows = dimensions.grid.rows,
            columns = dimensions.grid.columns,
            pixel_width = dimensions.pixel_width,
            pixel_height = dimensions.pixel_height,
            "terminal grid resized"
        );
        if let Some(pty) = self.pty.as_ref()
            && let Err(error) = pty.resize(dimensions)
        {
            tracing::error!(%error, "failed to propagate terminal size to PTY");
        }
    }

    fn drain_pty_events(&mut self, event_loop: &ActiveEventLoop) {
        let mut received_output = false;
        let mut reader_closed = false;

        let (pty, terminal, parser) = (&self.pty, &mut self.terminal, &mut self.terminal_parser);
        if let Some(pty) = pty.as_ref() {
            pty.drain_events(|event| match event {
                PtyEvent::Output(bytes) => {
                    parser.process(terminal, &bytes);
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

    fn write_pty(&mut self, bytes: &[u8]) {
        let Some(pty) = self.pty.as_mut() else {
            return;
        };
        if let Err(error) = pty.write(bytes) {
            tracing::error!(%error, "could not forward input to PTY");
        }
    }

    fn forward_keyboard_input(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        if let Some(action) = self.shortcuts.action(&event.logical_key, self.modifiers) {
            self.handle_shortcut(action);
            return;
        }
        let Some(bytes) = input::encode_key(&event, self.modifiers) else {
            return;
        };
        self.terminal.scroll_to_bottom();
        self.write_pty(&bytes);
    }

    fn handle_shortcut(&mut self, action: ShortcutAction) {
        match action {
            ShortcutAction::Copy => self.copy_selection(),
            ShortcutAction::Paste => self.paste_clipboard(),
            ShortcutAction::IncreaseFont => {
                self.change_font_size((self.logical_font_size + FONT_SIZE_STEP).min(MAX_FONT_SIZE))
            }
            ShortcutAction::DecreaseFont => {
                self.change_font_size((self.logical_font_size - FONT_SIZE_STEP).max(MIN_FONT_SIZE))
            }
            ShortcutAction::ResetFont => self.change_font_size(self.config.font.size),
            ShortcutAction::ScrollPageUp => self.terminal.scroll_page_up(),
            ShortcutAction::ScrollPageDown => self.terminal.scroll_page_down(),
            ShortcutAction::ScrollToBottom => self.terminal.scroll_to_bottom(),
        }
        if let Some(window) = self.window() {
            window.request_redraw();
        }
    }

    fn copy_selection(&mut self) {
        let Some(text) = self
            .terminal
            .selected_text()
            .filter(|text| !text.is_empty())
        else {
            return;
        };
        let Some(clipboard) = self.clipboard.as_mut() else {
            tracing::warn!("cannot copy because the Wayland clipboard is unavailable");
            return;
        };
        if let Err(error) = clipboard.set_text(text) {
            tracing::error!(%error, "could not copy terminal selection");
        }
    }

    fn paste_clipboard(&mut self) {
        let Some(clipboard) = self.clipboard.as_mut() else {
            tracing::warn!("cannot paste because the Wayland clipboard is unavailable");
            return;
        };
        let text = match clipboard.get_text() {
            Ok(text) => text,
            Err(error) => {
                tracing::error!(%error, "could not read text from the Wayland clipboard");
                return;
            }
        };
        self.terminal.scroll_to_bottom();
        let bytes = input::encode_paste(&text, self.terminal.bracketed_paste());
        self.write_pty(&bytes);
    }

    fn change_font_size(&mut self, font_size: f32) {
        if (font_size - self.logical_font_size).abs() < f32::EPSILON {
            return;
        }
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        if let Err(error) = renderer.update_font_size(font_size) {
            tracing::error!(%error, font_size, "could not rebuild the font atlas");
            return;
        }
        self.logical_font_size = font_size;
        self.synchronize_terminal_size();
        tracing::info!(font_size, "terminal font size changed");
    }

    fn pointer_cell(&self) -> Option<crate::terminal::Cursor> {
        self.renderer
            .as_ref()?
            .cell_at(self.cursor_position, self.terminal.size())
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

        let renderer_settings = RendererSettings {
            font_path: self.config.font.path.clone(),
            font_size: self.logical_font_size,
            padding_x: self.config.window.padding_x,
            padding_y: self.config.window.padding_y,
            foreground: parse_color(&self.config.window.foreground)
                .expect("loaded configuration has a valid foreground color"),
            background: parse_color(&self.config.window.background)
                .expect("loaded configuration has a valid background color"),
        };
        match pollster::block_on(Renderer::new(
            Arc::clone(&window),
            self.window_size,
            self.scale_factor,
            renderer_settings,
        )) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(error) => {
                tracing::error!(%error, "failed to initialize GPU renderer");
                event_loop.exit();
                return;
            }
        }

        self.synchronize_terminal_size();
        let dimensions = self
            .desired_pty_dimensions()
            .expect("renderer was initialized above");
        match PtySession::spawn(self.event_proxy.clone(), dimensions) {
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
                self.synchronize_terminal_size();

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
                    if let Err(error) = renderer.update_scale_factor(scale_factor) {
                        tracing::error!(%error, "failed to rebuild scaled font atlas");
                        event_loop.exit();
                        return;
                    }
                    renderer.resize(self.window_size);
                }
                self.synchronize_terminal_size();

                self.window()
                    .expect("window was validated above")
                    .request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => self.forward_keyboard_input(event),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = position;
                if self.selecting
                    && let Some(cell) = self.pointer_cell()
                {
                    self.terminal.update_selection(cell);
                    self.window()
                        .expect("window was validated above")
                        .request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    if let Some(cell) = self.pointer_cell() {
                        self.terminal.begin_selection(cell);
                        self.selecting = true;
                    } else {
                        self.terminal.clear_selection();
                        self.selecting = false;
                    }
                    self.window()
                        .expect("window was validated above")
                        .request_redraw();
                }
                ElementState::Released => self.selecting = false,
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, vertical) => (vertical * 3.0).round() as isize,
                    MouseScrollDelta::PixelDelta(position) => position.y.signum() as isize * 3,
                };
                if lines != 0 {
                    self.terminal.scroll_viewport(lines);
                    self.window()
                        .expect("window was validated above")
                        .request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };

                match renderer.render(self.terminal.render_snapshot()) {
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
