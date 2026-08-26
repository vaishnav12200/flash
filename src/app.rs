use std::{
    collections::VecDeque,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

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
    pty::{self, InputEnqueueError, PtyDimensions, PtyEvent, PtySession},
    renderer::{RenderError, RenderOutcome, Renderer, RendererSettings},
    terminal::{Terminal, TerminalParser},
};

const INITIAL_WINDOW_SIZE: PhysicalSize<u32> = PhysicalSize::new(960, 600);
const WINDOW_TITLE: &str = "Flash";
const FONT_SIZE_STEP: f32 = 2.0;
const MIN_FONT_SIZE: f32 = 6.0;
const MAX_FONT_SIZE: f32 = 72.0;
const MAX_PENDING_INPUT_BYTES: usize = 8 * 1024 * 1024;
const INITIAL_FRAME_DEADLINE: Duration = Duration::from_millis(100);

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
    latency: LatencyTracker,
    pending_input: VecDeque<Vec<u8>>,
    pending_input_bytes: usize,
    pty_output_deferred: bool,
    window_visible: bool,
    initial_frame_deadline_reached: bool,
}

struct LatencyTracker {
    startup_started_at: Instant,
    first_output_read_at: Option<Instant>,
    oldest_unpresented_output_at: Option<Instant>,
    unpresented_output_bytes: usize,
    unpresented_output_chunks: usize,
    output_redraw_requested_at: Option<Instant>,
    first_present_complete: bool,
    first_output_present_complete: bool,
    interval_started_at: Instant,
    interval_output_bytes: usize,
    interval_present_count: usize,
    interval_max_reader_to_ui_us: u128,
    interval_max_read_to_present_us: u128,
}

impl LatencyTracker {
    fn new(startup_started_at: Instant) -> Self {
        Self {
            startup_started_at,
            first_output_read_at: None,
            oldest_unpresented_output_at: None,
            unpresented_output_bytes: 0,
            unpresented_output_chunks: 0,
            output_redraw_requested_at: None,
            first_present_complete: false,
            first_output_present_complete: false,
            interval_started_at: startup_started_at,
            interval_output_bytes: 0,
            interval_present_count: 0,
            interval_max_reader_to_ui_us: 0,
            interval_max_read_to_present_us: 0,
        }
    }
}

impl App {
    pub fn new(
        event_proxy: EventLoopProxy<AppEvent>,
        config: Config,
        startup_started_at: Instant,
    ) -> Self {
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
            latency: LatencyTracker::new(startup_started_at),
            pending_input: VecDeque::new(),
            pending_input_bytes: 0,
            pty_output_deferred: false,
            window_visible: false,
            initial_frame_deadline_reached: false,
        }
    }

    fn window(&self) -> Option<&Arc<Window>> {
        self.window.as_ref()
    }

    fn initial_redraw_allowed(&self) -> bool {
        self.window_visible
            || self.latency.first_output_read_at.is_some()
            || self.initial_frame_deadline_reached
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
        let mut writer_failed = false;

        let (pty, terminal, parser) = (&self.pty, &mut self.terminal, &mut self.terminal_parser);
        if let Some(pty) = pty.as_ref() {
            let drain_result = pty.drain_events(|event| match event {
                PtyEvent::Output { bytes, read_at } => {
                    let process_started_at = Instant::now();
                    if self.latency.first_output_read_at.is_none() {
                        self.latency.first_output_read_at = Some(read_at);
                        tracing::info!(
                            startup_to_pty_read_us = read_at
                                .duration_since(self.latency.startup_started_at)
                                .as_micros(),
                            "latency.first_pty_output"
                        );
                    }
                    self.latency.oldest_unpresented_output_at = Some(
                        self.latency
                            .oldest_unpresented_output_at
                            .map_or(read_at, |oldest| oldest.min(read_at)),
                    );
                    self.latency.unpresented_output_bytes += bytes.len();
                    self.latency.unpresented_output_chunks += 1;
                    parser.process(terminal, &bytes);
                    let reader_to_ui_us = process_started_at.duration_since(read_at).as_micros();
                    self.latency.interval_max_reader_to_ui_us = self
                        .latency
                        .interval_max_reader_to_ui_us
                        .max(reader_to_ui_us);
                    tracing::debug!(
                        byte_count = bytes.len(),
                        reader_to_ui_us,
                        parse_us = process_started_at.elapsed().as_micros(),
                        "latency.pty_output_processed"
                    );
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
                PtyEvent::WriterError(error) => {
                    tracing::error!(%error, "PTY writer failed");
                    writer_failed = true;
                }
            });
            self.pty_output_deferred |= drain_result.budget_exhausted;
        }

        if received_output {
            self.latency.output_redraw_requested_at = Some(Instant::now());
            self.window()
                .expect("window exists while processing PTY events")
                .request_redraw();
        }
        if writer_failed {
            self.pending_input.clear();
            self.pending_input_bytes = 0;
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
        if bytes.is_empty() {
            return;
        }
        if self.pending_input_bytes.saturating_add(bytes.len()) > MAX_PENDING_INPUT_BYTES {
            tracing::error!(
                byte_count = bytes.len(),
                pending_byte_count = self.pending_input_bytes,
                limit = MAX_PENDING_INPUT_BYTES,
                "PTY input rejected because the bounded pending-input limit was reached"
            );
            return;
        }
        let chunk_count = bytes.len().div_ceil(pty::INPUT_CHUNK_SIZE);
        append_input_chunks(&mut self.pending_input, bytes);
        self.pending_input_bytes += bytes.len();
        tracing::debug!(
            byte_count = bytes.len(),
            chunk_count,
            pending_byte_count = self.pending_input_bytes,
            "latency.pty_input_enqueued"
        );
        self.pump_pty_input();
    }

    fn pump_pty_input(&mut self) {
        let Some(pty) = self.pty.as_ref() else {
            return;
        };
        while let Some(chunk) = self.pending_input.pop_front() {
            let chunk_len = chunk.len();
            match pty.try_write(chunk) {
                Ok(()) => self.pending_input_bytes -= chunk_len,
                Err(InputEnqueueError::Full(chunk)) => {
                    self.pending_input.push_front(chunk);
                    break;
                }
                Err(InputEnqueueError::Closed) => {
                    tracing::error!("could not forward input because the PTY writer is closed");
                    self.pending_input.clear();
                    self.pending_input_bytes = 0;
                    break;
                }
            }
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
            .with_visible(false)
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
        tracing::info!(
            startup_to_window_us = self.latency.startup_started_at.elapsed().as_micros(),
            "latency.window_created"
        );

        let provisional_dimensions = PtyDimensions {
            grid: self.terminal.size(),
            pixel_width: self.window_size.width,
            pixel_height: self.window_size.height,
        };
        match PtySession::spawn(self.event_proxy.clone(), provisional_dimensions) {
            Ok(pty) => self.pty = Some(pty),
            Err(error) => {
                tracing::error!(%error, "failed to initialize PTY session");
                event_loop.exit();
                return;
            }
        }
        tracing::info!(
            startup_to_pty_spawn_us = self.latency.startup_started_at.elapsed().as_micros(),
            "latency.pty_spawned"
        );

        let deadline_proxy = self.event_proxy.clone();
        thread::spawn(move || {
            thread::sleep(INITIAL_FRAME_DEADLINE);
            let _ = deadline_proxy.send_event(AppEvent::InitialFrameDeadline);
        });

        let renderer_settings = RendererSettings {
            font_path: self.config.font.path.clone(),
            fallback_font_paths: self.config.font.fallback.clone(),
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
            self.event_proxy.clone(),
            renderer_settings,
        )) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(error) => {
                tracing::error!(%error, "failed to initialize GPU renderer");
                event_loop.exit();
                return;
            }
        }
        tracing::info!(
            startup_to_renderer_us = self.latency.startup_started_at.elapsed().as_micros(),
            "latency.renderer_ready"
        );

        self.synchronize_terminal_size();
        self.window = Some(window);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::PtyActivity => self.drain_pty_events(event_loop),
            AppEvent::PtyInputReady => {
                if let Some(pty) = self.pty.as_ref() {
                    pty.acknowledge_input_ready();
                }
                self.pump_pty_input();
            }
            AppEvent::FontFallbackReady => {
                if let Some(window) = self.window() {
                    window.request_redraw();
                }
            }
            AppEvent::InitialFrameDeadline => {
                self.drain_pty_events(event_loop);
                self.initial_frame_deadline_reached = true;
                if !self.window_visible
                    && let Some(window) = self.window()
                {
                    window.request_redraw();
                }
            }
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

                if size.width > 0 && size.height > 0 && self.initial_redraw_allowed() {
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

                if self.initial_redraw_allowed() {
                    self.window()
                        .expect("window was validated above")
                        .request_redraw();
                }
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
                if !self.initial_redraw_allowed() {
                    return;
                }
                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };

                let render_started_at = Instant::now();
                match renderer.render(self.terminal.render_snapshot()) {
                    Ok(RenderOutcome::Presented) => {
                        let presented_at = Instant::now();
                        if !self.latency.first_present_complete {
                            tracing::info!(
                                startup_to_first_present_us = presented_at
                                    .duration_since(self.latency.startup_started_at)
                                    .as_micros(),
                                first_frame_had_pty_output =
                                    self.latency.first_output_read_at.is_some(),
                                render_us =
                                    presented_at.duration_since(render_started_at).as_micros(),
                                "latency.first_present"
                            );
                            self.latency.first_present_complete = true;
                        }
                        if let Some(read_at) = self.latency.oldest_unpresented_output_at.take() {
                            let read_to_present_us =
                                presented_at.duration_since(read_at).as_micros();
                            if !self.latency.first_output_present_complete {
                                tracing::info!(
                                    byte_count = self.latency.unpresented_output_bytes,
                                    chunk_count = self.latency.unpresented_output_chunks,
                                    pty_read_to_first_output_present_us = read_to_present_us,
                                    startup_to_first_output_present_us = presented_at
                                        .duration_since(self.latency.startup_started_at)
                                        .as_micros(),
                                    "latency.first_output_present"
                                );
                                self.latency.first_output_present_complete = true;
                            }
                            tracing::debug!(
                                byte_count = self.latency.unpresented_output_bytes,
                                chunk_count = self.latency.unpresented_output_chunks,
                                pty_read_to_present_us = read_to_present_us,
                                redraw_to_present_us =
                                    self.latency.output_redraw_requested_at.map(|requested_at| {
                                        presented_at.duration_since(requested_at).as_micros()
                                    }),
                                render_us =
                                    presented_at.duration_since(render_started_at).as_micros(),
                                "latency.pty_output_presented"
                            );
                            self.latency.interval_output_bytes +=
                                self.latency.unpresented_output_bytes;
                            self.latency.interval_present_count += 1;
                            self.latency.interval_max_read_to_present_us = self
                                .latency
                                .interval_max_read_to_present_us
                                .max(read_to_present_us);
                            self.latency.unpresented_output_bytes = 0;
                            self.latency.unpresented_output_chunks = 0;
                            self.latency.output_redraw_requested_at = None;
                        }
                        if self.latency.interval_started_at.elapsed() >= Duration::from_secs(1) {
                            tracing::info!(
                                interval_ms =
                                    self.latency.interval_started_at.elapsed().as_millis(),
                                byte_count = self.latency.interval_output_bytes,
                                present_count = self.latency.interval_present_count,
                                max_reader_to_ui_us = self.latency.interval_max_reader_to_ui_us,
                                max_read_to_present_us =
                                    self.latency.interval_max_read_to_present_us,
                                "latency.pty_present_interval"
                            );
                            self.latency.interval_started_at = presented_at;
                            self.latency.interval_output_bytes = 0;
                            self.latency.interval_present_count = 0;
                            self.latency.interval_max_reader_to_ui_us = 0;
                            self.latency.interval_max_read_to_present_us = 0;
                        }
                        if !self.window_visible {
                            self.window()
                                .expect("window exists while presenting")
                                .set_visible(true);
                            self.window_visible = true;
                        }
                        if self.pty_output_deferred {
                            self.pty_output_deferred = false;
                            if let Some(pty) = self.pty.as_ref() {
                                pty.request_output_wake();
                            }
                        }
                    }
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

fn append_input_chunks(queue: &mut VecDeque<Vec<u8>>, bytes: &[u8]) {
    queue.extend(bytes.chunks(pty::INPUT_CHUNK_SIZE).map(<[u8]>::to_vec));
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::mpsc, time::Instant};

    use super::append_input_chunks;
    use crate::pty::INPUT_CHUNK_SIZE;

    #[test]
    fn large_input_hits_bounded_channel_backpressure_without_blocking() {
        let bytes = vec![b'x'; 8 * 1024 * 1024];
        let started_at = Instant::now();
        let mut pending = VecDeque::new();
        append_input_chunks(&mut pending, &bytes);

        let (sender, _receiver) = mpsc::sync_channel(16);
        let mut accepted = 0;
        while let Some(chunk) = pending.pop_front() {
            match sender.try_send(chunk) {
                Ok(()) => accepted += 1,
                Err(mpsc::TrySendError::Full(chunk)) => {
                    pending.push_front(chunk);
                    break;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => unreachable!(),
            }
        }

        eprintln!(
            "8 MiB input enqueue returned in {:.3} ms",
            started_at.elapsed().as_secs_f64() * 1_000.0
        );
        assert_eq!(accepted, 16);
        assert_eq!(pending.len(), bytes.len() / INPUT_CHUNK_SIZE - accepted);
        assert!(pending.iter().all(|chunk| chunk.len() <= INPUT_CHUNK_SIZE));
    }
}
