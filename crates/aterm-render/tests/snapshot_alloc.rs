// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An owned CPU capture transfers the completed framebuffer out of its temporary
//! window cache. Count full-frame allocations on the shipping snapshot path;
//! a byte-parity check alone would let its old extra clone come back unnoticed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu, embedded_font};

static FRAME_BYTES: AtomicUsize = AtomicUsize::new(0);
static FRAME_ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct CountingAlloc;

fn note_alloc(size: usize) {
    if size != 0 && size == FRAME_BYTES.load(Ordering::Relaxed) {
        FRAME_ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: all operations forward the original arguments to System; the only
// additional work is allocation-free atomic bookkeeping.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        // SAFETY: forward the caller's valid layout unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        // SAFETY: forward the caller's valid layout unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_alloc(new_size);
        // SAFETY: the pointer and layout came from this System-backed allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the pointer and layout came from this System-backed allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn count_frames<T>(bytes: usize, f: impl FnOnce() -> T) -> (T, usize) {
    FRAME_ALLOCS.store(0, Ordering::Relaxed);
    FRAME_BYTES.store(bytes, Ordering::Relaxed);
    let result = f();
    FRAME_BYTES.store(0, Ordering::Relaxed);
    (result, FRAME_ALLOCS.load(Ordering::Relaxed))
}

#[test]
fn owned_snapshots_transfer_pixels_without_a_full_frame_clone() {
    let mut renderer = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("bundled monospace font");
    renderer.set_pad(5);
    let (rows, cols) = (32usize, 100usize);
    let mut terminal = Terminal::new(rows as u16, cols as u16);
    terminal.process(b"\x1b[?25l");
    for row in 0..rows {
        terminal.process(
            format!(
                "\x1b[{};1H\x1b[3{}mrow {row}: capture text",
                row + 1,
                row % 6 + 1
            )
            .as_bytes(),
        );
    }
    let mut input = terminal.cell_frame(rows, cols);
    input.grid_top_row = 1;
    input.grid_bot_row = rows - 1;
    let mut cache = WindowCpu::new();

    for frac in [0, 3, -3] {
        input.scroll_frac_px = frac;
        let view = renderer.render_input_cached(&mut cache, &input);
        let dims = (view.width(), view.height());
        let expected = view.pixels().to_vec();
        let bytes = expected.len() * std::mem::size_of::<u32>();
        // The plain copy is a negative control for the counter, and is the
        // exact extra allocation the old snapshot path made after rasterizing.
        let (copy, copies) = count_frames(bytes, || expected.clone());
        assert_eq!(copies, 1, "the counter must observe a full-frame clone");
        assert_eq!(copy, expected);
        drop(copy);

        let (owned, allocations) = count_frames(bytes, || renderer.render_input(&input));
        assert_eq!((owned.width, owned.height), dims);
        assert_eq!(owned.pixels, expected, "presented pixels at frac={frac}");
        assert_eq!(
            allocations,
            if frac == 0 { 1 } else { 2 },
            "capture must allocate only its raster and, for fractional scroll, \
             its translated present buffer; never a third ownership copy"
        );
        eprintln!(
            "snapshot frac={frac}: {allocations} full-frame allocations, {bytes} bytes per frame"
        );
    }
}
