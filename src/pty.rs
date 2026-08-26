use std::{
    ffi::OsString,
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use winit::event_loop::EventLoopProxy;

use crate::{event::AppEvent, terminal::GridSize};

const EVENT_QUEUE_CAPACITY: usize = 128;
const INPUT_QUEUE_CAPACITY: usize = 16;
pub const INPUT_CHUNK_SIZE: usize = 4 * 1024;
const READ_BUFFER_SIZE: usize = 8 * 1024;
const LOG_PREVIEW_BYTES: usize = 256;
pub const OUTPUT_DRAIN_BYTE_BUDGET: usize = 256 * 1024;
pub const OUTPUT_DRAIN_TIME_BUDGET: std::time::Duration = std::time::Duration::from_millis(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyDimensions {
    pub grid: GridSize,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

impl PtyDimensions {
    fn as_pty_size(self) -> PtySize {
        PtySize {
            rows: self.grid.rows.min(u16::MAX as usize) as u16,
            cols: self.grid.columns.min(u16::MAX as usize) as u16,
            pixel_width: self.pixel_width.min(u16::MAX as u32) as u16,
            pixel_height: self.pixel_height.min(u16::MAX as u32) as u16,
        }
    }
}

#[derive(Debug)]
pub enum PtyEvent {
    Output { bytes: Vec<u8>, read_at: Instant },
    EndOfFile,
    ReaderError(String),
    WriterError(String),
}

pub enum InputEnqueueError {
    Full,
    Closed,
}

#[derive(Clone)]
pub struct PtyInput {
    bytes: Arc<[u8]>,
    start: usize,
    end: usize,
}

impl PtyInput {
    pub fn new(bytes: Arc<[u8]>, start: usize, end: usize) -> Self {
        debug_assert!(start <= end && end <= bytes.len());
        debug_assert!(end - start <= INPUT_CHUNK_SIZE);
        Self { bytes, start, end }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }

    pub(crate) fn len(&self) -> usize {
        self.end - self.start
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DrainResult {
    pub budget_exhausted: bool,
}

/// A shell process connected through a pseudo-terminal.
///
/// The UI thread owns this object. Dedicated blocking reader and writer
/// threads exchange bounded byte chunks with the UI event loop.
pub struct PtySession {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    input_sender: Option<SyncSender<PtyInput>>,
    events: Receiver<PtyEvent>,
    wake_pending: Arc<AtomicBool>,
    input_wake_pending: Arc<AtomicBool>,
    event_proxy: EventLoopProxy<AppEvent>,
    reader_thread: Option<JoinHandle<()>>,
    writer_thread: Option<JoinHandle<()>>,
}

impl PtySession {
    pub fn spawn(event_proxy: EventLoopProxy<AppEvent>, dimensions: PtyDimensions) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(dimensions.as_pty_size())
            .context("could not allocate a pseudo-terminal")?;

        let reader = pair
            .master
            .try_clone_reader()
            .context("could not create a PTY output reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("could not create a PTY input writer")?;

        let shell = user_shell();
        let mut command = CommandBuilder::new(&shell);
        // Phase 5 implements the common xterm-compatible cursor, screen, mode,
        // and color sequences needed by shells and mainstream terminal UIs.
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "flash");

        let mut child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("could not start shell {:?}", shell))?;
        let process_id = child.process_id();

        let (event_sender, events) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let (input_sender, input_receiver) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        let wake_pending = Arc::new(AtomicBool::new(false));
        let reader_wake_pending = Arc::clone(&wake_pending);
        let reader_event_sender = event_sender.clone();
        let reader_event_proxy = event_proxy.clone();
        let reader_thread = match thread::Builder::new()
            .name("flash-pty-reader".to_owned())
            .spawn(move || {
                read_pty_output(
                    reader,
                    reader_event_sender,
                    reader_event_proxy,
                    reader_wake_pending,
                )
            }) {
            Ok(reader_thread) => reader_thread,
            Err(error) => {
                let _ = child.kill();
                return Err(error).context("could not start PTY reader thread");
            }
        };

        let input_wake_pending = Arc::new(AtomicBool::new(false));
        let writer_input_wake_pending = Arc::clone(&input_wake_pending);
        let writer_event_proxy = event_proxy.clone();
        let writer_event_sender = event_sender.clone();
        let writer_output_wake_pending = Arc::clone(&wake_pending);
        let writer_thread = match thread::Builder::new()
            .name("flash-pty-writer".to_owned())
            .spawn(move || {
                write_pty_input(
                    writer,
                    input_receiver,
                    writer_event_sender,
                    writer_event_proxy,
                    writer_output_wake_pending,
                    writer_input_wake_pending,
                )
            }) {
            Ok(writer_thread) => writer_thread,
            Err(error) => {
                let _ = child.kill();
                drop(pair.master);
                let _ = reader_thread.join();
                return Err(error).context("could not start PTY writer thread");
            }
        };

        tracing::info!(shell = ?shell, ?process_id, "started shell in PTY");

        Ok(Self {
            master: Some(pair.master),
            child: Some(child),
            input_sender: Some(input_sender),
            events,
            wake_pending,
            input_wake_pending,
            event_proxy,
            reader_thread: Some(reader_thread),
            writer_thread: Some(writer_thread),
        })
    }

    pub fn resize(&self, dimensions: PtyDimensions) -> Result<()> {
        self.master
            .as_ref()
            .context("PTY master is closed")?
            .resize(dimensions.as_pty_size())
            .context("could not resize PTY")
    }

    /// Enqueues one bounded input chunk without blocking the UI thread.
    pub fn try_write(&self, input: PtyInput) -> std::result::Result<(), InputEnqueueError> {
        if input.len() == 0 {
            return Ok(());
        }
        let Some(sender) = self.input_sender.as_ref() else {
            return Err(InputEnqueueError::Closed);
        };
        sender.try_send(input).map_err(|error| match error {
            TrySendError::Full(_) => InputEnqueueError::Full,
            TrySendError::Disconnected(_) => InputEnqueueError::Closed,
        })
    }

    pub fn acknowledge_input_ready(&self) {
        self.input_wake_pending.store(false, Ordering::Release);
    }

    pub fn request_output_wake(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            let _ = self.event_proxy.send_event(AppEvent::PtyActivity);
        }
    }

    /// Drains PTY events without blocking the UI, yielding after a bounded
    /// byte or time slice so a redraw can be presented under sustained output.
    ///
    /// The second receive after clearing the wake flag closes the race where a
    /// reader event arrives while the queue is being drained.
    pub fn drain_events(&self, mut handle: impl FnMut(PtyEvent)) -> DrainResult {
        let started_at = Instant::now();
        let mut output_bytes = 0;
        loop {
            while let Ok(event) = self.events.try_recv() {
                output_bytes += event.output_byte_count();
                handle(event);
                if output_bytes >= OUTPUT_DRAIN_BYTE_BUDGET
                    || started_at.elapsed() >= OUTPUT_DRAIN_TIME_BUDGET
                {
                    self.wake_pending.store(false, Ordering::Release);
                    return DrainResult {
                        budget_exhausted: true,
                    };
                }
            }

            self.wake_pending.store(false, Ordering::Release);
            match self.events.try_recv() {
                Ok(event) => handle(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                    return DrainResult::default();
                }
            }
        }
    }

    pub fn poll_child_exit(&mut self) -> Result<Option<portable_pty::ExitStatus>> {
        self.child
            .as_mut()
            .context("PTY child handle is closed")?
            .try_wait()
            .context("could not poll shell exit status")
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }

        self.input_sender.take();
        if let Some(writer_thread) = self.writer_thread.take() {
            let _ = writer_thread.join();
        }
        self.master.take();

        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
    }
}

impl PtyEvent {
    fn output_byte_count(&self) -> usize {
        match self {
            Self::Output { bytes, .. } => bytes.len(),
            Self::EndOfFile | Self::ReaderError(_) | Self::WriterError(_) => 0,
        }
    }
}

fn user_shell() -> OsString {
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| OsString::from("/bin/sh"))
}

fn read_pty_output(
    mut reader: Box<dyn Read + Send>,
    event_sender: SyncSender<PtyEvent>,
    event_proxy: EventLoopProxy<AppEvent>,
    wake_pending: Arc<AtomicBool>,
) {
    let mut buffer = [0_u8; READ_BUFFER_SIZE];

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                send_event(
                    PtyEvent::EndOfFile,
                    &event_sender,
                    &event_proxy,
                    &wake_pending,
                );
                return;
            }
            Ok(byte_count) => {
                let bytes = buffer[..byte_count].to_vec();
                if !send_event(
                    PtyEvent::Output {
                        bytes,
                        read_at: Instant::now(),
                    },
                    &event_sender,
                    &event_proxy,
                    &wake_pending,
                ) {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                send_event(
                    PtyEvent::ReaderError(error.to_string()),
                    &event_sender,
                    &event_proxy,
                    &wake_pending,
                );
                return;
            }
        }
    }
}

fn write_pty_input(
    mut writer: Box<dyn Write + Send>,
    input_receiver: Receiver<PtyInput>,
    event_sender: SyncSender<PtyEvent>,
    event_proxy: EventLoopProxy<AppEvent>,
    output_wake_pending: Arc<AtomicBool>,
    input_wake_pending: Arc<AtomicBool>,
) {
    while let Ok(input) = input_receiver.recv() {
        let started_at = Instant::now();
        let result = writer
            .write_all(input.as_bytes())
            .and_then(|()| writer.flush());
        tracing::debug!(
            byte_count = input.len(),
            write_us = started_at.elapsed().as_micros(),
            "latency.pty_input_write"
        );
        if let Err(error) = result {
            send_event(
                PtyEvent::WriterError(error.to_string()),
                &event_sender,
                &event_proxy,
                &output_wake_pending,
            );
            return;
        }
        if !input_wake_pending.swap(true, Ordering::AcqRel)
            && event_proxy.send_event(AppEvent::PtyInputReady).is_err()
        {
            return;
        }
    }
}

fn send_event(
    event: PtyEvent,
    event_sender: &SyncSender<PtyEvent>,
    event_proxy: &EventLoopProxy<AppEvent>,
    wake_pending: &AtomicBool,
) -> bool {
    if event_sender.send(event).is_err() {
        return false;
    }

    if !wake_pending.swap(true, Ordering::AcqRel)
        && event_proxy.send_event(AppEvent::PtyActivity).is_err()
    {
        return false;
    }

    true
}

pub fn log_output(bytes: &[u8]) {
    tracing::debug!(
        byte_count = bytes.len(),
        escaped = %escaped_preview(bytes),
        hex = %hex_preview(bytes),
        "PTY output received"
    );
}

fn escaped_preview(bytes: &[u8]) -> String {
    let mut output = String::new();
    for &byte in bytes.iter().take(LOG_PREVIEW_BYTES) {
        match byte {
            b'\\' => output.push_str("\\\\"),
            b'\n' => output.push_str("\\n"),
            b'\r' => output.push_str("\\r"),
            b'\t' => output.push_str("\\t"),
            0x20..=0x7e => output.push(byte.into()),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\x{byte:02X}");
            }
        }
    }

    append_truncation_marker(&mut output, bytes.len());
    output
}

fn hex_preview(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    for (index, byte) in bytes.iter().take(LOG_PREVIEW_BYTES).enumerate() {
        if index > 0 {
            output.push(' ');
        }
        let _ = write!(output, "{byte:02X}");
    }

    append_truncation_marker(&mut output, bytes.len());
    output
}

fn append_truncation_marker(output: &mut String, byte_count: usize) {
    if byte_count > LOG_PREVIEW_BYTES {
        use std::fmt::Write as _;
        let _ = write!(
            output,
            " … ({} bytes omitted)",
            byte_count - LOG_PREVIEW_BYTES
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{PtyDimensions, escaped_preview, hex_preview};
    use crate::terminal::GridSize;

    #[test]
    fn escapes_control_bytes_without_losing_printable_text() {
        assert_eq!(escaped_preview(b"ok\r\n\x1b[31m"), "ok\\r\\n\\x1B[31m");
    }

    #[test]
    fn renders_a_space_separated_hex_preview() {
        assert_eq!(hex_preview(b"A\n\x1b"), "41 0A 1B");
    }

    #[test]
    fn pty_dimensions_saturate_to_kernel_field_widths() {
        let size = PtyDimensions {
            grid: GridSize {
                rows: usize::MAX,
                columns: 80,
            },
            pixel_width: u32::MAX,
            pixel_height: 600,
        }
        .as_pty_size();
        assert_eq!(size.rows, u16::MAX);
        assert_eq!(size.cols, 80);
        assert_eq!(size.pixel_width, u16::MAX);
        assert_eq!(size.pixel_height, 600);
    }
}
