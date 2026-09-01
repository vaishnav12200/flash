#![allow(dead_code, unused_imports)]

#[path = "../src/search.rs"]
mod search;
#[path = "../src/terminal/mod.rs"]
mod terminal;

use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use search::{SearchDirection, SearchProgress, SearchState};
use terminal::{Terminal, TerminalParser};

const NORMAL_SCROLLBACK_ROWS: usize = 10_000;
const MAXIMUM_SCROLLBACK_ROWS: usize = 1_000_000;
const SEARCH_TIME_BUDGET: Duration = Duration::from_millis(2);

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

fn terminal_with_history(scrollback_rows: usize, columns: usize, line: &[u8]) -> Terminal {
    let mut terminal = Terminal::new(2, columns);
    terminal.set_scrollback_limit(scrollback_rows);
    let mut parser = TerminalParser::default();
    for _ in 0..scrollback_rows + 1 {
        parser.process(&mut terminal, line);
    }
    terminal
}

fn measure(name: &str, terminal: &Terminal, iterations: usize) {
    let mut search = SearchState::default();
    search.open();
    assert!(search.insert_text("absent-needle"));

    // Reserve reusable row extraction storage before counting hot scans.
    search.begin_search(terminal, SearchDirection::Forward);
    while search.continue_search(terminal, SEARCH_TIME_BUDGET) == SearchProgress::Pending {}
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    let started = Instant::now();
    let mut slice_count = 0;
    let mut maximum_slice = Duration::ZERO;
    for _ in 0..iterations {
        search.begin_search(terminal, SearchDirection::Forward);
        loop {
            let slice_started = Instant::now();
            let progress = black_box(search.continue_search(terminal, SEARCH_TIME_BUDGET));
            maximum_slice = maximum_slice.max(slice_started.elapsed());
            slice_count += 1;
            if progress != SearchProgress::Pending {
                break;
            }
        }
    }
    let elapsed = started.elapsed();
    let allocations = ALLOCATION_COUNT.load(Ordering::Relaxed);
    println!(
        "{name}: rows={}, iterations={iterations}, total_ms={:.3}, average_ms={:.3}, average_slices={:.1}, maximum_slice_ms={:.3}, allocations={allocations}",
        terminal.searchable_row_count(),
        elapsed.as_secs_f64() * 1_000.0,
        elapsed.as_secs_f64() * 1_000.0 / iterations as f64,
        slice_count as f64 / iterations as f64,
        maximum_slice.as_secs_f64() * 1_000.0,
    );
}

fn main() {
    let normal = terminal_with_history(
        NORMAL_SCROLLBACK_ROWS,
        80,
        b"ordinary terminal output without target\r\n",
    );
    measure("normal_scrollback_full_scan", &normal, 20);
    drop(normal);

    // A one-column row preserves the maximum configured row-count stress while
    // avoiding the multi-gigabyte cell footprint of one million 80-column rows.
    let maximum = terminal_with_history(MAXIMUM_SCROLLBACK_ROWS, 1, b"x\r\n");
    measure("maximum_scrollback_full_scan", &maximum, 5);
}
