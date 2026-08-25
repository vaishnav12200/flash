use std::{
    ffi::OsString,
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use winit::event_loop::EventLoopProxy;

use crate::event::AppEvent;

const EVENT_QUEUE_CAPACITY: usize = 128;
const READ_BUFFER_SIZE: usize = 8 * 1024;
const LOG_PREVIEW_BYTES: usize = 256;

#[derive(Debug)]
pub enum PtyEvent {
    Output(Vec<u8>),
    EndOfFile,
    ReaderError(String),
}

/// A shell process connected through a pseudo-terminal.
///
/// The UI thread owns this object. A single reader thread owns only a cloned
/// read handle and forwards opaque output byte chunks to the UI thread.
pub struct PtySession {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    writer: Option<Box<dyn Write + Send>>,
    events: Receiver<PtyEvent>,
    wake_pending: Arc<AtomicBool>,
    reader_thread: Option<JoinHandle<()>>,
}

impl PtySession {
    pub fn spawn(event_proxy: EventLoopProxy<AppEvent>) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
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
        let wake_pending = Arc::new(AtomicBool::new(false));
        let reader_wake_pending = Arc::clone(&wake_pending);
        let reader_thread = match thread::Builder::new()
            .name("flash-pty-reader".to_owned())
            .spawn(move || read_pty_output(reader, event_sender, event_proxy, reader_wake_pending))
        {
            Ok(reader_thread) => reader_thread,
            Err(error) => {
                let _ = child.kill();
                return Err(error).context("could not start PTY reader thread");
            }
        };

        tracing::info!(shell = ?shell, ?process_id, "started shell in PTY");

        Ok(Self {
            master: Some(pair.master),
            child: Some(child),
            writer: Some(writer),
            events,
            wake_pending,
            reader_thread: Some(reader_thread),
        })
    }

    /// Sends already-encoded terminal input to the shell.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }

        let writer = self.writer.as_mut().context("PTY input writer is closed")?;
        writer
            .write_all(bytes)
            .context("could not write keyboard input to PTY")?;
        writer.flush().context("could not flush PTY input")
    }

    /// Drains every PTY event currently available without blocking the UI.
    ///
    /// The second receive after clearing the wake flag closes the race where a
    /// reader event arrives while the queue is being drained.
    pub fn drain_events(&self, mut handle: impl FnMut(PtyEvent)) {
        loop {
            while let Ok(event) = self.events.try_recv() {
                handle(event);
            }

            self.wake_pending.store(false, Ordering::Release);
            match self.events.try_recv() {
                Ok(event) => handle(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
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

        self.writer.take();
        self.master.take();

        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
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
                    PtyEvent::Output(bytes),
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
    use super::{escaped_preview, hex_preview};

    #[test]
    fn escapes_control_bytes_without_losing_printable_text() {
        assert_eq!(escaped_preview(b"ok\r\n\x1b[31m"), "ok\\r\\n\\x1B[31m");
    }

    #[test]
    fn renders_a_space_separated_hex_preview() {
        assert_eq!(hex_preview(b"A\n\x1b"), "41 0A 1B");
    }
}
