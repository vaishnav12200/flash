#![allow(dead_code, unused_imports)]

#[path = "../src/terminal/mod.rs"]
mod terminal;

use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

use terminal::{Terminal, TerminalParser};

const DATA_SIZE: usize = 8 * 1024 * 1024;
const ITERATIONS: usize = 5;
const PTY_CHUNK_SIZE: usize = 8 * 1024;

struct CountingAllocator;

static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: this allocator forwards the unchanged layout to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: this allocator forwards the unchanged layout to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: this allocator forwards the original allocation and layout to System.
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: this allocator forwards the original allocation and layout to System.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn workload(line: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(DATA_SIZE + line.len());
    while bytes.len() < DATA_SIZE {
        bytes.extend_from_slice(line);
    }
    bytes.truncate(DATA_SIZE);
    bytes
}

fn measure(name: &str, bytes: &[u8], scrollback_lines: usize) {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    let started = Instant::now();
    let mut checksum = 0_u32;

    for _ in 0..ITERATIONS {
        let mut terminal = Terminal::new(24, 80);
        terminal.set_scrollback_limit(scrollback_lines);
        let mut parser = TerminalParser::default();
        for chunk in bytes.chunks(PTY_CHUNK_SIZE) {
            parser.process(&mut terminal, black_box(chunk));
        }
        checksum ^= terminal
            .render_snapshot()
            .cells
            .iter()
            .fold(0_u32, |sum, cell| sum.wrapping_add(cell.character as u32));
    }

    let elapsed = started.elapsed();
    let mib = (bytes.len() * ITERATIONS) as f64 / (1024.0 * 1024.0);
    let allocations = ALLOCATION_COUNT.load(Ordering::Relaxed);
    println!(
        "{name}: {mib:.1} MiB in {:.3}s = {:.1} MiB/s, {allocations} allocations = {:.1} alloc/MiB (checksum={checksum})",
        elapsed.as_secs_f64(),
        mib / elapsed.as_secs_f64(),
        allocations as f64 / mib,
    );
}

fn main() {
    let plain = workload(b"0123456789 abcdefghijklmnopqrstuvwxyz\r\n");
    measure("plain_ascii", &plain, 10_000);
    measure("plain_ascii_no_history", &plain, 0);
    measure(
        "styled_unicode",
        &workload("\x1b[32mUnicode: café 日本語 ✓ 🚀\x1b[0m\r\n".as_bytes()),
        10_000,
    );
    measure(
        "parser_control_only",
        &workload(b"\x1b[0m\x1b[1;1H\x1b[39m\x1b[49m"),
        0,
    );
}
