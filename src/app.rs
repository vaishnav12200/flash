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
    event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy},
    keyboard::ModifiersState,
    window::{Theme, Window, WindowId},
};

use crate::{
    config::{Config, ShortcutAction, ShortcutMap},
    event::AppEvent,
    font::GlyphAtlas,
    input,
    pty::{self, InputEnqueueError, PtyDimensions, PtyEvent, PtyInput, PtySession},
    renderer::{RenderError, RenderOutcome, Renderer, RendererSettings},
    search::{SearchDirection, SearchInputOutcome, SearchMatch, SearchState},
    terminal::{MouseTracking, Terminal, TerminalParser},
};

const INITIAL_WINDOW_SIZE: PhysicalSize<u32> = PhysicalSize::new(960, 600);
const WINDOW_TITLE: &str = "Flash";
const FONT_SIZE_STEP: f32 = 2.0;
const MIN_FONT_SIZE: f32 = 6.0;
const MAX_FONT_SIZE: f32 = 72.0;
const MAX_PENDING_INPUT_BYTES: usize = 8 * 1024 * 1024;
const INITIAL_FRAME_DEADLINE: Duration = Duration::from_millis(100);
const LATENCY_BUCKET_UPPER_US: [u64; 19] = [
    250,
    500,
    750,
    1_000,
    1_500,
    2_000,
    3_000,
    4_000,
    6_000,
    8_000,
    12_000,
    16_000,
    33_000,
    100_000,
    500_000,
    1_000_000,
    5_000_000,
    10_000_000,
    u64::MAX,
];

/// Owns the native window, renderer, PTY session, and application event lifecycle.
pub struct App {
    event_proxy: EventLoopProxy<AppEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty: Option<PtySession>,
    pty_dimensions: Option<PtyDimensions>,
    terminal: Terminal,
    terminal_parser: TerminalParser,
    window_size: PhysicalSize<u32>,
    scale_factor: f64,
    config: Config,
    shortcuts: ShortcutMap,
    search: SearchState,
    modifiers: ModifiersState,
    clipboard: Option<arboard::Clipboard>,
    cursor_position: PhysicalPosition<f64>,
    selecting: bool,
    reported_mouse_button: Option<u8>,
    logical_font_size: f32,
    latency: LatencyTracker,
    pending_input: VecDeque<PendingInput>,
    pending_input_bytes: usize,
    pty_output_batch: Vec<u8>,
    pty_output_deferred: bool,
    window_visible: bool,
    initial_frame_deadline_reached: bool,
    input_latency_probe_pending: bool,
    surface_timeout_retries: u8,
    title_version: u64,
    cursor_blink_visible: bool,
    cursor_blink_deadline: Option<Instant>,
    window_focused: bool,
}

struct PendingInput {
    bytes: Arc<[u8]>,
    offset: usize,
}

impl PendingInput {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Arc::from(bytes),
            offset: 0,
        }
    }

    fn remaining_len(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn next_chunk(&self) -> PtyInput {
        let end = (self.offset + pty::INPUT_CHUNK_SIZE).min(self.bytes.len());
        PtyInput::new(Arc::clone(&self.bytes), self.offset, end)
    }
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
    pending_key_to_present_at: Option<Instant>,
    render_latency: LatencyHistogram,
    output_latency: LatencyHistogram,
    key_latency: LatencyHistogram,
}

#[derive(Default)]
struct LatencyHistogram {
    buckets: [u64; LATENCY_BUCKET_UPPER_US.len()],
    count: u64,
}

impl LatencyHistogram {
    fn record(&mut self, latency_us: u128) {
        let latency_us = latency_us.min(u128::from(u64::MAX)) as u64;
        let index = LATENCY_BUCKET_UPPER_US
            .iter()
            .position(|upper| latency_us <= *upper)
            .unwrap_or(LATENCY_BUCKET_UPPER_US.len() - 1);
        self.buckets[index] += 1;
        self.count += 1;
    }

    fn percentile_upper_us(&self, percentile: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = self.count.saturating_mul(percentile).div_ceil(100);
        let mut cumulative = 0;
        for (count, upper) in self.buckets.iter().zip(LATENCY_BUCKET_UPPER_US) {
            cumulative += count;
            if cumulative >= target {
                return upper;
            }
        }
        u64::MAX
    }

    fn clear(&mut self) {
        self.buckets.fill(0);
        self.count = 0;
    }
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
            pending_key_to_present_at: None,
            render_latency: LatencyHistogram::default(),
            output_latency: LatencyHistogram::default(),
            key_latency: LatencyHistogram::default(),
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
            pty_dimensions: None,
            terminal,
            terminal_parser: TerminalParser::default(),
            window_size: INITIAL_WINDOW_SIZE,
            scale_factor: 1.0,
            logical_font_size: config.font.size,
            config,
            shortcuts,
            search: SearchState::default(),
            modifiers: ModifiersState::empty(),
            clipboard,
            cursor_position: PhysicalPosition::new(0.0, 0.0),
            selecting: false,
            reported_mouse_button: None,
            latency: LatencyTracker::new(startup_started_at),
            pending_input: VecDeque::new(),
            pending_input_bytes: 0,
            pty_output_batch: Vec::with_capacity(pty::OUTPUT_DRAIN_BYTE_BUDGET),
            pty_output_deferred: false,
            window_visible: false,
            initial_frame_deadline_reached: false,
            input_latency_probe_pending: std::env::var_os("FLASH_INPUT_LATENCY_PROBE")
                .is_some_and(|value| value == "1"),
            surface_timeout_retries: 0,
            title_version: 0,
            cursor_blink_visible: true,
            cursor_blink_deadline: None,
            window_focused: true,
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

    fn restart_cursor_blink(&mut self) {
        let needs_redraw = !self.cursor_blink_visible;
        self.cursor_blink_visible = true;
        self.cursor_blink_deadline = (self.config.cursor.blink && self.window_focused)
            .then(|| Instant::now() + Duration::from_millis(self.config.cursor.blink_interval));
        if needs_redraw && let Some(window) = self.window() {
            window.request_redraw();
        }
    }

    fn desired_pty_dimensions(&self) -> Option<PtyDimensions> {
        if self.window_size.width == 0 || self.window_size.height == 0 {
            return None;
        }
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
        let grid_changed = self.terminal.resize(dimensions.grid);
        if grid_changed {
            self.refresh_search_after_terminal_change();
        }
        if !grid_changed && self.pty_dimensions == Some(dimensions) {
            return;
        }

        if grid_changed {
            tracing::info!(
                rows = dimensions.grid.rows,
                columns = dimensions.grid.columns,
                pixel_width = dimensions.pixel_width,
                pixel_height = dimensions.pixel_height,
                "terminal grid resized"
            );
        }
        if let Some(pty) = self.pty.as_ref() {
            match pty.resize(dimensions) {
                Ok(()) => self.pty_dimensions = Some(dimensions),
                Err(error) => {
                    tracing::error!(%error, "failed to propagate terminal size to PTY");
                }
            }
        }
    }

    fn drain_pty_events(&mut self, event_loop: &ActiveEventLoop) {
        let mut received_output = false;
        let mut reader_closed = false;
        let mut writer_failed = false;
        self.pty_output_batch.clear();

        let pty = &self.pty;
        if let Some(pty) = pty.as_ref() {
            let drain_result = pty.drain_events(|event| match event {
                PtyEvent::Output { bytes, read_at } => {
                    let received_at = Instant::now();
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
                    let reader_to_ui_us = received_at.duration_since(read_at).as_micros();
                    self.latency.interval_max_reader_to_ui_us = self
                        .latency
                        .interval_max_reader_to_ui_us
                        .max(reader_to_ui_us);
                    self.pty_output_batch.extend_from_slice(&bytes);
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
            let process_started_at = Instant::now();
            self.terminal_parser
                .process(&mut self.terminal, &self.pty_output_batch);
            self.refresh_search_after_terminal_change();
            self.restart_cursor_blink();
            if self.terminal.mouse_tracking() == MouseTracking::None {
                self.reported_mouse_button = None;
            } else {
                self.selecting = false;
            }
            if self.title_version != self.terminal.title_version() {
                let title = self.terminal.title();
                self.window()
                    .expect("window exists while processing PTY output")
                    .set_title(if title.is_empty() {
                        WINDOW_TITLE
                    } else {
                        title
                    });
                self.title_version = self.terminal.title_version();
            }
            tracing::debug!(
                byte_count = self.pty_output_batch.len(),
                parse_us = process_started_at.elapsed().as_micros(),
                "latency.pty_output_batch_processed"
            );
            pty::log_output(&self.pty_output_batch);
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

    fn write_pty(&mut self, bytes: Vec<u8>) {
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
        let byte_count = bytes.len();
        let chunk_count = byte_count.div_ceil(pty::INPUT_CHUNK_SIZE);
        self.pending_input.push_back(PendingInput::new(bytes));
        self.pending_input_bytes += byte_count;
        tracing::debug!(
            byte_count,
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
        while let Some(pending) = self.pending_input.front_mut() {
            let chunk = pending.next_chunk();
            let chunk_len = chunk.len();
            match pty.try_write(chunk) {
                Ok(()) => {
                    pending.offset += chunk_len;
                    self.pending_input_bytes -= chunk_len;
                    if pending.remaining_len() == 0 {
                        self.pending_input.pop_front();
                    }
                }
                Err(InputEnqueueError::Full) => break,
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
        let key_received_at = Instant::now();
        if event.state != ElementState::Pressed {
            return;
        }
        self.restart_cursor_blink();
        let shortcut = self.shortcuts.action(&event.logical_key, self.modifiers);
        if shortcut == Some(ShortcutAction::Search) {
            self.handle_shortcut(ShortcutAction::Search);
            return;
        }
        if self.search.is_active() {
            let outcome =
                self.search
                    .handle_key(&event.logical_key, event.text.as_deref(), self.modifiers);
            let found = match outcome {
                SearchInputOutcome::QueryChanged => self
                    .search
                    .find_next(&self.terminal, SearchDirection::Forward),
                SearchInputOutcome::Navigate(direction) => {
                    self.search.find_next(&self.terminal, direction)
                }
                SearchInputOutcome::Ignored
                | SearchInputOutcome::Consumed
                | SearchInputOutcome::Closed => None,
            };
            if let Some(found) = found {
                self.reveal_search_match(found);
            }
            if outcome.needs_redraw()
                && let Some(window) = self.window()
            {
                window.request_redraw();
            }
            return;
        }
        if let Some(action) = shortcut {
            self.handle_shortcut(action);
            return;
        }
        let Some(bytes) = input::encode_key(
            &event,
            self.modifiers,
            self.terminal.application_cursor_keys(),
            self.terminal.application_keypad(),
        ) else {
            return;
        };
        self.latency
            .pending_key_to_present_at
            .get_or_insert(key_received_at);
        self.terminal.scroll_to_bottom();
        self.write_pty(bytes);
    }

    fn handle_shortcut(&mut self, action: ShortcutAction) {
        let needs_redraw = match action {
            ShortcutAction::Search => {
                let opened = self.search.open();
                if opened {
                    if let Some(found) = self
                        .search
                        .find_next(&self.terminal, SearchDirection::Forward)
                    {
                        self.reveal_search_match(found);
                    }
                }
                opened
            }
            ShortcutAction::Copy => {
                self.copy_selection();
                true
            }
            ShortcutAction::Paste => {
                self.paste_clipboard();
                true
            }
            ShortcutAction::IncreaseFont => {
                self.change_font_size((self.logical_font_size + FONT_SIZE_STEP).min(MAX_FONT_SIZE));
                true
            }
            ShortcutAction::DecreaseFont => {
                self.change_font_size((self.logical_font_size - FONT_SIZE_STEP).max(MIN_FONT_SIZE));
                true
            }
            ShortcutAction::ResetFont => {
                self.change_font_size(self.config.font.size);
                true
            }
            ShortcutAction::ScrollPageUp => {
                self.terminal.scroll_page_up();
                true
            }
            ShortcutAction::ScrollPageDown => {
                self.terminal.scroll_page_down();
                true
            }
            ShortcutAction::ScrollToBottom => {
                self.terminal.scroll_to_bottom();
                true
            }
        };
        if needs_redraw && let Some(window) = self.window() {
            window.request_redraw();
        }
    }

    fn reveal_search_match(&mut self, search_match: SearchMatch) {
        self.terminal.reveal_search_row(search_match.row);
    }

    fn refresh_search_after_terminal_change(&mut self) {
        if !self.search.is_active() {
            return;
        }
        if let Some(found) = self.search.refresh_current(&self.terminal) {
            self.reveal_search_match(found);
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
        self.write_pty(bytes);
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

    fn mouse_reporting_active(&self) -> bool {
        should_report_mouse(self.terminal.mouse_tracking(), self.modifiers.shift_key())
    }

    fn report_mouse(&mut self, kind: input::MouseEventKind) {
        let Some(cell) = self.pointer_cell() else {
            return;
        };
        if let Some(bytes) = input::encode_mouse(
            kind,
            cell.row,
            cell.column,
            self.modifiers,
            self.terminal.sgr_mouse(),
        ) {
            self.write_pty(bytes);
        }
    }

    fn mouse_button_code(button: MouseButton) -> Option<u8> {
        match button {
            MouseButton::Left => Some(0),
            MouseButton::Middle => Some(1),
            MouseButton::Right => Some(2),
            _ => None,
        }
    }
}

fn should_report_mouse(mouse_tracking: MouseTracking, shift_override: bool) -> bool {
    mouse_tracking != MouseTracking::None && !shift_override
}

fn advance_cursor_blink(
    visible: bool,
    deadline: Instant,
    now: Instant,
    interval: Duration,
) -> (bool, Instant, bool) {
    if now < deadline {
        (visible, deadline, false)
    } else {
        (!visible, now + interval, true)
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
            .with_resizable(true)
            .with_theme(Some(Theme::Dark));

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

        let visual_colors = self
            .config
            .visual_colors()
            .expect("loaded configuration has valid visual colors");
        let physical_scale = self.scale_factor as f32;
        let mut selection_background = visual_colors.selection_background;
        selection_background[3] = 0.72;
        let renderer_settings = RendererSettings {
            font_path: self.config.font.path.clone(),
            fallback_font_paths: self.config.font.fallback.clone(),
            font_size: self.logical_font_size,
            padding_x: self.config.window.padding_x * physical_scale,
            padding_y: self.config.window.padding_y * physical_scale,
            foreground: visual_colors.foreground,
            background: visual_colors.background,
            cursor: visual_colors.cursor,
            cursor_style: self.config.cursor.style,
            selection_background,
            selection_foreground: visual_colors.selection_foreground,
            ansi: visual_colors.ansi,
        };
        let atlas = match GlyphAtlas::load(
            &renderer_settings.font_path,
            &renderer_settings.fallback_font_paths,
            renderer_settings.font_size,
            self.scale_factor,
            Some(self.event_proxy.clone()),
        ) {
            Ok(atlas) => atlas,
            Err(error) => {
                tracing::error!(%error, "failed to initialize font atlas");
                event_loop.exit();
                return;
            }
        };
        let content = crate::renderer::content_size(
            self.window_size,
            renderer_settings.padding_x,
            renderer_settings.padding_y,
        );
        let grid = crate::renderer::grid_size_for_metrics(
            self.window_size,
            atlas.cell_width,
            atlas.cell_height,
            renderer_settings.padding_x,
            renderer_settings.padding_y,
        );
        self.terminal.resize(grid);
        tracing::info!(
            rows = grid.rows,
            columns = grid.columns,
            pixel_width = content.width,
            pixel_height = content.height,
            "initial terminal grid measured"
        );
        let dimensions = PtyDimensions {
            grid,
            pixel_width: content.width,
            pixel_height: content.height,
        };
        match PtySession::spawn(self.event_proxy.clone(), dimensions) {
            Ok(pty) => {
                self.pty = Some(pty);
                self.pty_dimensions = Some(dimensions);
            }
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

        match pollster::block_on(Renderer::new(
            Arc::clone(&window),
            self.window_size,
            self.scale_factor,
            self.event_proxy.clone(),
            renderer_settings,
            atlas,
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
        self.restart_cursor_blink();
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
            AppEvent::RedrawRetry => {
                if self.window_size.width > 0
                    && self.window_size.height > 0
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
            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                if focused {
                    self.restart_cursor_blink();
                } else {
                    let needs_redraw = !self.cursor_blink_visible;
                    self.cursor_blink_visible = true;
                    self.cursor_blink_deadline = None;
                    if needs_redraw && let Some(window) = self.window() {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.forward_keyboard_input(event),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = position;
                let motion_button = match self.terminal.mouse_tracking() {
                    MouseTracking::AnyMotion if self.mouse_reporting_active() => {
                        Some(self.reported_mouse_button.unwrap_or(3))
                    }
                    MouseTracking::ButtonMotion if self.mouse_reporting_active() => {
                        self.reported_mouse_button
                    }
                    _ => None,
                };
                if let Some(button) = motion_button {
                    self.report_mouse(input::MouseEventKind::Motion(button));
                } else if self.selecting
                    && let Some(cell) = self.pointer_cell()
                {
                    self.terminal.update_selection(cell);
                    self.window()
                        .expect("window was validated above")
                        .request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button_code = Self::mouse_button_code(button);
                if self.mouse_reporting_active() {
                    if let Some(button) = button_code {
                        match state {
                            ElementState::Pressed => {
                                self.reported_mouse_button = Some(button);
                                self.report_mouse(input::MouseEventKind::Press(button));
                            }
                            ElementState::Released => {
                                self.report_mouse(input::MouseEventKind::Release(button));
                                if self.reported_mouse_button == Some(button) {
                                    self.reported_mouse_button = None;
                                }
                            }
                        }
                    }
                    return;
                }

                if state == ElementState::Released {
                    self.reported_mouse_button = None;
                }

                if button != MouseButton::Left {
                    return;
                }
                match state {
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
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.mouse_reporting_active() {
                    let kind = match delta {
                        MouseScrollDelta::LineDelta(_, vertical) if vertical > 0.0 => {
                            Some(input::MouseEventKind::WheelUp)
                        }
                        MouseScrollDelta::LineDelta(_, vertical) if vertical < 0.0 => {
                            Some(input::MouseEventKind::WheelDown)
                        }
                        MouseScrollDelta::PixelDelta(position) if position.y > 0.0 => {
                            Some(input::MouseEventKind::WheelUp)
                        }
                        MouseScrollDelta::PixelDelta(position) if position.y < 0.0 => {
                            Some(input::MouseEventKind::WheelDown)
                        }
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        self.report_mouse(kind);
                    }
                    return;
                }
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
                let mut snapshot = self.terminal.render_snapshot();
                if self.config.cursor.blink && !self.cursor_blink_visible {
                    snapshot.cursor_visible = false;
                }
                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };

                let render_started_at = Instant::now();
                match renderer.render(snapshot) {
                    Ok(RenderOutcome::Presented) => {
                        self.surface_timeout_retries = 0;
                        let presented_at = Instant::now();
                        let render_us = presented_at.duration_since(render_started_at).as_micros();
                        self.latency.render_latency.record(render_us);
                        if !self.latency.first_present_complete {
                            tracing::info!(
                                startup_to_first_present_us = presented_at
                                    .duration_since(self.latency.startup_started_at)
                                    .as_micros(),
                                first_frame_had_pty_output =
                                    self.latency.first_output_read_at.is_some(),
                                render_us,
                                "latency.first_present"
                            );
                            self.latency.first_present_complete = true;
                        }
                        if let Some(read_at) = self.latency.oldest_unpresented_output_at.take() {
                            let read_to_present_us =
                                presented_at.duration_since(read_at).as_micros();
                            self.latency.output_latency.record(read_to_present_us);
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
                                render_us,
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
                            if let Some(key_at) = self.latency.pending_key_to_present_at.take() {
                                let key_to_present_us =
                                    presented_at.duration_since(key_at).as_micros();
                                self.latency.key_latency.record(key_to_present_us);
                                tracing::info!(key_to_present_us, "latency.key_to_present");
                            }
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
                                render_p50_upper_us =
                                    self.latency.render_latency.percentile_upper_us(50),
                                render_p95_upper_us =
                                    self.latency.render_latency.percentile_upper_us(95),
                                render_p99_upper_us =
                                    self.latency.render_latency.percentile_upper_us(99),
                                output_p95_upper_us =
                                    self.latency.output_latency.percentile_upper_us(95),
                                key_p95_upper_us = self.latency.key_latency.percentile_upper_us(95),
                                "latency.pty_present_interval"
                            );
                            self.latency.interval_started_at = presented_at;
                            self.latency.interval_output_bytes = 0;
                            self.latency.interval_present_count = 0;
                            self.latency.interval_max_reader_to_ui_us = 0;
                            self.latency.interval_max_read_to_present_us = 0;
                            self.latency.render_latency.clear();
                            self.latency.output_latency.clear();
                            self.latency.key_latency.clear();
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
                        if self.input_latency_probe_pending {
                            self.input_latency_probe_pending = false;
                            self.latency.pending_key_to_present_at = Some(Instant::now());
                            tracing::info!("latency.input_probe_started");
                            self.write_pty(vec![b'x']);
                        }
                    }
                    Ok(RenderOutcome::Reconfigured) => self
                        .window()
                        .expect("window was validated above")
                        .request_redraw(),
                    Ok(RenderOutcome::Skipped) => {}
                    Ok(RenderOutcome::TimedOut) => {
                        if self.surface_timeout_retries < 3 {
                            self.surface_timeout_retries += 1;
                            let retry_proxy = self.event_proxy.clone();
                            thread::spawn(move || {
                                thread::sleep(Duration::from_millis(16));
                                let _ = retry_proxy.send_event(AppEvent::RedrawRetry);
                            });
                        } else {
                            tracing::warn!(
                                "GPU surface timed out repeatedly; waiting for the next event"
                            );
                        }
                    }
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let terminal_cursor_visible = self.terminal.render_snapshot().cursor_visible;
        if !self.config.cursor.blink || !self.window_focused || !terminal_cursor_visible {
            self.cursor_blink_visible = true;
            self.cursor_blink_deadline = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }

        let interval = Duration::from_millis(self.config.cursor.blink_interval);
        let now = Instant::now();
        let deadline = self.cursor_blink_deadline.unwrap_or(now + interval);
        let (visible, deadline, changed) =
            advance_cursor_blink(self.cursor_blink_visible, deadline, now, interval);
        self.cursor_blink_visible = visible;
        if changed && let Some(window) = self.window() {
            window.request_redraw();
        }
        self.cursor_blink_deadline = Some(deadline);
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use super::{LatencyHistogram, PendingInput, advance_cursor_blink, should_report_mouse};
    use crate::pty::INPUT_CHUNK_SIZE;
    use crate::terminal::MouseTracking;

    #[test]
    fn large_input_hits_bounded_channel_backpressure_without_blocking() {
        let bytes = vec![b'x'; 8 * 1024 * 1024];
        let started_at = Instant::now();
        let mut pending = PendingInput::new(bytes);
        for _ in 0..16 {
            let chunk = pending.next_chunk();
            assert_eq!(Arc::strong_count(&pending.bytes), 2);
            pending.offset += chunk.len();
        }

        eprintln!(
            "8 MiB input enqueue returned in {:.3} ms",
            started_at.elapsed().as_secs_f64() * 1_000.0
        );
        assert_eq!(pending.offset, 16 * INPUT_CHUNK_SIZE);
        assert_eq!(pending.remaining_len(), 8 * 1024 * 1024 - pending.offset);
    }

    #[test]
    fn latency_histogram_reports_bounded_percentiles_without_allocating_samples() {
        let mut histogram = LatencyHistogram::default();
        for latency in [100, 400, 900, 1_400, 7_500, 40_000] {
            histogram.record(latency);
        }
        assert_eq!(histogram.percentile_upper_us(50), 1_000);
        assert_eq!(histogram.percentile_upper_us(95), 100_000);
        histogram.clear();
        assert_eq!(histogram.percentile_upper_us(95), 0);
    }

    #[test]
    fn latency_histogram_reports_slow_startup_without_leaking_overflow_sentinel() {
        let mut histogram = LatencyHistogram::default();
        histogram.record(503_900);
        assert_eq!(histogram.percentile_upper_us(95), 1_000_000);
    }

    #[test]
    fn mouse_reporting_route_is_disabled_for_normal_shell_and_shift_override() {
        assert!(!should_report_mouse(MouseTracking::None, false));
        assert!(should_report_mouse(MouseTracking::Button, false));
        assert!(should_report_mouse(MouseTracking::ButtonMotion, false));
        assert!(should_report_mouse(MouseTracking::AnyMotion, false));
        assert!(!should_report_mouse(MouseTracking::Button, true));
    }

    #[test]
    fn cursor_blink_changes_only_at_scheduled_transitions() {
        let now = Instant::now();
        let interval = std::time::Duration::from_millis(600);
        let deadline = now + interval;
        assert_eq!(
            advance_cursor_blink(true, deadline, now, interval),
            (true, deadline, false)
        );
        assert_eq!(
            advance_cursor_blink(true, deadline, deadline, interval),
            (false, deadline + interval, true)
        );
    }
}
