// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Counting-allocator tests for CPU snapshot framebuffer transfers, zoomed inline image raster budgets, and decode caching.

use aterm_bench::allocator::{Observed, Observer};
use std::alloc::System;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu, embedded_font};

static FRAME_BYTES: AtomicUsize = AtomicUsize::new(0);
static FRAME_ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// The largest single allocation while [`TRACK_LARGEST`] is set.
static LARGEST: AtomicUsize = AtomicUsize::new(0);
static TRACK_LARGEST: AtomicBool = AtomicBool::new(false);
/// How many allocations of at least [`BIG_BYTES`] were made while it is non-zero.
static BIG_BYTES: AtomicUsize = AtomicUsize::new(0);
static BIG_ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// The counters are process-wide: each test holds this while it reads them.
static COUNTERS: Mutex<()> = Mutex::new(());

/// Successfully retained requested bytes and their logical high-water mark, rather than the
/// allocator's internal transient buffers or resident physical pages.
struct CountingAlloc {
    live: AtomicUsize,
    peak: AtomicUsize,
}

impl CountingAlloc {
    const fn new() -> Self {
        Self {
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }
}

fn note_alloc(size: usize) {
    if size != 0 && size == FRAME_BYTES.load(Ordering::Relaxed) {
        FRAME_ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
    if TRACK_LARGEST.load(Ordering::Relaxed) {
        LARGEST.fetch_max(size, Ordering::Relaxed);
    }
    let big = BIG_BYTES.load(Ordering::Relaxed);
    if big != 0 && size >= big {
        BIG_ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: the callbacks use only nonallocating, nonpanicking atomic arithmetic.
unsafe impl Observer for CountingAlloc {
    fn requested(&self, size: usize) {
        note_alloc(size);
    }

    fn retained(&self, old: usize, new: usize) {
        if new > old {
            let growth = new - old;
            let live = self
                .live
                .fetch_add(growth, Ordering::Relaxed)
                .wrapping_add(growth);
            self.peak.fetch_max(live, Ordering::Relaxed);
        } else {
            self.live.fetch_sub(old - new, Ordering::Relaxed);
        }
    }
}

#[global_allocator]
static GLOBAL: Observed<System, CountingAlloc> = Observed::new(System, CountingAlloc::new());

#[test]
fn retained_peak_preserves_in_place_realloc_and_refused_operations() {
    use std::alloc::{GlobalAlloc, Layout};

    let _counters = COUNTERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // This allocator owns 512 bytes throughout each block's lifetime and accepts a smaller logical
    // layout. Every supported reallocation is therefore deterministically in place.
    struct InPlace;
    const BACKING: Layout = Layout::new::<[u64; 64]>();
    // SAFETY: successful requests fit BACKING's size/alignment. Realloc preserves the live block
    // unchanged; null failures leave it owned by the caller. Deallocation uses its backing layout.
    unsafe impl GlobalAlloc for InPlace {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if layout.size() > BACKING.size() || layout.align() > BACKING.align() {
                return std::ptr::null_mut();
            }
            // SAFETY: BACKING is a nonzero valid layout, at least as large/aligned as requested.
            unsafe { System.alloc(BACKING) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if layout.size() > BACKING.size() || layout.align() > BACKING.align() {
                return std::ptr::null_mut();
            }
            // SAFETY: BACKING is a nonzero valid layout, at least as large/aligned as requested.
            unsafe { System.alloc_zeroed(BACKING) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, _layout: Layout, size: usize) -> *mut u8 {
            if size == 128 || size > BACKING.size() {
                std::ptr::null_mut()
            } else {
                ptr
            }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            // SAFETY: each live block came from System with BACKING and is freed only once.
            unsafe { System.dealloc(ptr, BACKING) };
        }
    }

    let allocator = Observed::new(InPlace, CountingAlloc::new());
    let small = Layout::from_size_align(64, BACKING.align()).unwrap();
    let grown = Layout::from_size_align(256, BACKING.align()).unwrap();
    let shrunk = Layout::from_size_align(32, BACKING.align()).unwrap();
    let too_large = Layout::from_size_align(1_024, BACKING.align()).unwrap();
    // SAFETY: all accesses stay inside the current logical layout, successful reallocs preserve
    // ownership, refused ones retain it, and the final logical block is deallocated exactly once.
    unsafe {
        assert!(allocator.alloc(too_large).is_null());
        assert!(allocator.alloc_zeroed(too_large).is_null());
        assert_eq!(allocator.observer().live.load(Ordering::Relaxed), 0);
        assert_eq!(allocator.observer().peak.load(Ordering::Relaxed), 0);
        let ptr = allocator.alloc_zeroed(small);
        assert!(!ptr.is_null());
        assert!(std::slice::from_raw_parts(ptr, 64).iter().all(|b| *b == 0));
        ptr.write_bytes(0xa5, 32);
        assert_eq!(allocator.realloc(ptr, small, 256), ptr);
        assert_eq!(allocator.observer().live.load(Ordering::Relaxed), 256);
        assert_eq!(allocator.observer().peak.load(Ordering::Relaxed), 256);
        assert!(allocator.realloc(ptr, grown, 128).is_null());
        assert!(allocator.realloc(ptr, grown, 1_024).is_null());
        assert_eq!(allocator.observer().live.load(Ordering::Relaxed), 256);
        assert_eq!(allocator.observer().peak.load(Ordering::Relaxed), 256);
        assert_eq!(allocator.realloc(ptr, grown, 32), ptr);
        assert!(
            std::slice::from_raw_parts(ptr, 32)
                .iter()
                .all(|b| *b == 0xa5)
        );
        assert_eq!(allocator.observer().live.load(Ordering::Relaxed), 32);
        assert_eq!(allocator.observer().peak.load(Ordering::Relaxed), 256);
        allocator.dealloc(ptr, shrunk);
        assert_eq!(allocator.observer().live.load(Ordering::Relaxed), 0);
        assert_eq!(allocator.observer().peak.load(Ordering::Relaxed), 256);
    }
}

fn count_frames<T>(bytes: usize, f: impl FnOnce() -> T) -> (T, usize) {
    FRAME_ALLOCS.store(0, Ordering::Relaxed);
    FRAME_BYTES.store(bytes, Ordering::Relaxed);
    let result = f();
    FRAME_BYTES.store(0, Ordering::Relaxed);
    (result, FRAME_ALLOCS.load(Ordering::Relaxed))
}

/// How many allocations of at least `bytes` `f` makes.
fn count_big<T>(bytes: usize, f: impl FnOnce() -> T) -> (T, usize) {
    BIG_ALLOCS.store(0, Ordering::Relaxed);
    BIG_BYTES.store(bytes, Ordering::Relaxed);
    let result = f();
    BIG_BYTES.store(0, Ordering::Relaxed);
    (result, BIG_ALLOCS.load(Ordering::Relaxed))
}

/// A solid red sixel `w` x `h` px, `h` a multiple of six.
fn red_sixel(w: usize, h: usize) -> Vec<u8> {
    let mut sixel = format!("\x1bP0;1;0q\"1;1;{w};{h}#1;2;100;0;0#1");
    for band in 0..h / 6 {
        if band > 0 {
            sixel.push('-');
        }
        sixel.push_str(&format!("!{w}~"));
    }
    sixel.push_str("\x1b\\");
    sixel.into_bytes()
}

/// Returns `(result, largest_single_request, peak_retained_delta)` for `f`.
fn largest_alloc<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    LARGEST.store(0, Ordering::Relaxed);
    let before = GLOBAL.observer().live.load(Ordering::Relaxed);
    GLOBAL.observer().peak.store(before, Ordering::Relaxed);
    TRACK_LARGEST.store(true, Ordering::Relaxed);
    let result = f();
    TRACK_LARGEST.store(false, Ordering::Relaxed);
    let peak = GLOBAL
        .observer()
        .peak
        .load(Ordering::Relaxed)
        .saturating_sub(before);
    (result, LARGEST.load(Ordering::Relaxed), peak)
}

#[test]
fn owned_snapshots_transfer_pixels_without_a_full_frame_clone() {
    let _counters = COUNTERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let (copy, copies) = count_frames(bytes, || expected.clone());
        assert_eq!(copies, 1, "the counter must observe a full-frame clone");
        assert_eq!(copy, expected);
        drop(copy);

        let (owned, allocations) = count_frames(bytes, || renderer.render_input(&input));
        assert_eq!((owned.width, owned.height), dims);
        assert_eq!(owned.pixels, expected, "presented pixels at frac={frac}");
        assert_eq!(
            allocations, 1,
            "capture must allocate only its raster: a fractional scroll's \
             present is built straight from it in one pass, never a second copy"
        );
        eprintln!(
            "snapshot frac={frac}: {allocations} full-frame allocations, {bytes} bytes per frame"
        );
    }
}

/// Renders `placement` zoomed to the 200 px font ceiling scrolled `rows_down` rows and returns `(frame, cell_px, largest_alloc, peak_live, renderer)`.
fn render_zoomed(
    placement: impl FnOnce(&mut Terminal, (usize, usize)),
    rows_down: i32,
) -> (aterm_render::Frame, (usize, usize), usize, usize, Renderer) {
    let mut renderer = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("bundled monospace font");
    let (cw, ch) = renderer.cell_size();
    let (cols, rows) = (64usize, 44usize);
    let mut terminal = Terminal::new(rows as u16, cols as u16);
    terminal.set_cell_pixel_size(cw as u16, ch as u16);
    placement(&mut terminal, (cw, ch));

    renderer.set_px(200.0);
    let (zw, zh) = renderer.cell_size();
    let (zcols, zrows) = ((cols * cw / zw).max(1), (rows * ch / zh).max(1));
    terminal.resize(zrows as u16, zcols as u16);
    terminal.set_cell_pixel_size(zw as u16, zh as u16);
    terminal.scroll_display(i32::MAX);
    terminal.scroll_display(-rows_down);
    let input = terminal.cell_frame(zrows, zcols);
    assert!(
        input.images.iter().any(|row| !row.is_empty()),
        "the picture is on screen"
    );
    let (frame, largest, peak) = largest_alloc(|| renderer.render_input(&input));
    (frame, (zw, zh), largest, peak, renderer)
}

/// Maximum live bytes allowed while rendering a zoomed placement (`MAX_PLACEMENT_RASTER_PX` RGBA plus frame buffers).
fn zoomed_frame_budget(frame: &aterm_render::Frame) -> usize {
    aterm_render::MAX_PLACEMENT_RASTER_PX * 4 + 8 * frame.width * frame.height * 4
}

/// A placement zoomed to the 200 px ceiling decodes within `MAX_PLACEMENT_RASTER_PX` and renders its pixels.
#[test]
fn a_zoomed_placement_decodes_within_the_raster_budget() {
    let _counters = COUNTERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (frame, (zw, zh), largest, peak, renderer) = render_zoomed(
        |terminal, (cw, ch)| terminal.process(&red_sixel(64 * cw, (40 * ch) / 6 * 6)),
        0,
    );
    let budget = aterm_render::MAX_PLACEMENT_RASTER_PX * 4;
    assert!(
        largest <= budget,
        "a 64x40-cell placement zoomed to {zw}x{zh}-px cells allocated {largest} bytes \
         at once; its raster budget is {budget}"
    );
    assert!(
        peak <= zoomed_frame_budget(&frame),
        "drawing it held {peak} bytes at once"
    );
    let at = |x: usize, y: usize| frame.pixels[y * frame.width + x] & 0x00FF_FFFF;
    let (x, y) = (renderer.pad() + 10, renderer.grid_top() + 10);
    assert_eq!(
        at(x, y) >> 16,
        0xFF,
        "the picture is drawn, red: {:06x}",
        at(x, y)
    );
}

/// A cell-fitted placement zoomed to the 200 px ceiling resamples row by row within the raster budget.
#[test]
fn a_zoomed_fit_placement_decodes_within_the_raster_budget() {
    let _counters = COUNTERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let place = |terminal: &mut Terminal, _: (usize, usize)| {
        let rgba: Vec<u8> = [255u8, 10, 10, 255].repeat(64 * 40);
        let payload = aterm_codec::base64::encode(&rgba).expect("base64");
        terminal.process(
            format!("\x1b[1;1H\x1b_Ga=T,f=32,s=64,v=40,c=64,r=40;{payload}\x1b\\").as_bytes(),
        );
    };
    let (frame, (zw, zh), largest, peak, renderer) = render_zoomed(place, 20);
    let budget = aterm_render::MAX_PLACEMENT_RASTER_PX * 4;
    assert!(
        largest <= budget,
        "a 64x40-cell fitted placement zoomed to {zw}x{zh}-px cells allocated {largest} \
         bytes at once; its raster budget is {budget}"
    );
    assert!(
        peak <= zoomed_frame_budget(&frame),
        "drawing it held {peak} bytes at once; the raster is {budget}"
    );
    let at = |x: usize, y: usize| frame.pixels[y * frame.width + x] & 0x00FF_FFFF;
    let (x, y) = (renderer.pad() + 10, renderer.grid_top() + 10);
    assert_eq!(
        at(x, y) >> 16,
        0xFF,
        "the picture is drawn, red: {:06x}",
        at(x, y)
    );
}

/// Two simultaneously visible placements exceeding the decode cache byte budget are each decoded once across repaints.
#[test]
fn two_placements_past_the_byte_budget_are_decoded_once() {
    let _counters = COUNTERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut renderer = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("bundled monospace font");
    let (cw, ch) = renderer.cell_size();
    let (cols, rows) = (64usize, 84usize);
    let mut terminal = Terminal::new(rows as u16, cols as u16);
    terminal.set_cell_pixel_size(cw as u16, ch as u16);
    let sixel = red_sixel(cols * cw, (40 * ch) / 6 * 6);
    terminal.process(&sixel);
    terminal.process(&sixel);

    renderer.set_px(200.0);
    let (zw, zh) = renderer.cell_size();
    let (zcols, zrows) = ((cols * cw / zw).max(1), (rows * ch / zh).max(1));
    terminal.resize(zrows as u16, zcols as u16);
    terminal.set_cell_pixel_size(zw as u16, zh as u16);
    let mut input = terminal.cell_frame(zrows, zcols);
    let shown = |input: &aterm_render::RenderInput| {
        let mut ids: Vec<usize> = input
            .images
            .iter()
            .flatten()
            .map(|(_, image)| std::sync::Arc::as_ptr(&image.image) as usize)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };
    for _ in 0..rows {
        if shown(&input) == 2 {
            break;
        }
        terminal.scroll_display(1);
        input = terminal.cell_frame(zrows, zcols);
    }
    assert_eq!(shown(&input), 2, "both pictures are on screen");

    let big = 32 << 20;
    let mut cache = WindowCpu::new();
    let ((), first) = count_big(big, || {
        let _ = renderer.render_input_cached(&mut cache, &input);
    });
    assert!(
        first >= 2,
        "the first frame decodes both pictures ({first})"
    );
    for frame in 0..2 {
        cache.invalidate();
        let ((), again) = count_big(big, || {
            let _ = renderer.render_input_cached(&mut cache, &input);
        });
        assert_eq!(
            again, 0,
            "repaint {frame} decoded a picture again ({again} allocations of 32 MiB+)"
        );
    }
}

/// Layered placements whose full rasters exceed `MAX_IMAGE_WORKING_SET_BYTES` are scaled to fit the working set and decoded once.
#[test]
fn layered_placements_past_the_working_set_are_decoded_once() {
    let _counters = COUNTERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut renderer = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("bundled monospace font");
    let (cw, ch) = renderer.cell_size();
    let (cols, rows) = (64usize, 44usize);
    let mut terminal = Terminal::new(rows as u16, cols as u16);
    terminal.set_cell_pixel_size(cw as u16, ch as u16);
    let layers = 3;
    for z in 0..layers {
        let rgba: Vec<u8> = [z as u8 * 80, 10, 10, 255].repeat(8 * 6);
        let payload = aterm_codec::base64::encode(&rgba).expect("base64");
        terminal.process(
            format!("\x1b[1;1H\x1b_Ga=T,f=32,s=8,v=6,c=64,r=40,z={z},C=1;{payload}\x1b\\")
                .as_bytes(),
        );
    }
    renderer.set_px(200.0);
    let (zw, zh) = renderer.cell_size();
    let (zcols, zrows) = ((cols * cw / zw).max(1), (rows * ch / zh).max(1));
    terminal.resize(zrows as u16, zcols as u16);
    terminal.set_cell_pixel_size(zw as u16, zh as u16);
    terminal.scroll_display(i32::MAX);
    let input = terminal.cell_frame(zrows, zcols);
    let layered = input.images.iter().flatten().count();
    assert!(
        layered >= 2 * layers,
        "the pictures are layered on the visible cells ({layered} cell layers)"
    );
    let (w, h) = aterm_render::placement_raster_size(64 * zw, 40 * zh);
    assert!(
        layers * w * h * 4 > aterm_render::MAX_IMAGE_WORKING_SET_BYTES,
        "the full rasters pass the working set together"
    );

    let big = 48 << 20;
    let mut cache = WindowCpu::new();
    let (((), first), _, peak) = largest_alloc(|| {
        count_big(big, || {
            let _ = renderer.render_input_cached(&mut cache, &input);
        })
    });
    assert_eq!(first, layers, "the first repaint decodes each picture once");
    assert!(
        peak <= aterm_render::MAX_IMAGE_WORKING_SET_BYTES + (16 << 20),
        "the frame held {peak} bytes at once; its working set is {}",
        aterm_render::MAX_IMAGE_WORKING_SET_BYTES
    );
    for frame in 0..2 {
        cache.invalidate();
        let ((), again) = count_big(big, || {
            let _ = renderer.render_input_cached(&mut cache, &input);
        });
        assert_eq!(
            again, 0,
            "repaint {frame} decoded a picture again ({again} allocations of 48 MiB+)"
        );
    }
}

/// A visible contact sheet exceeding the decode cache's 64-entry cap keeps all on-screen tiles cached across repaints.
#[test]
fn a_contact_sheet_past_the_entry_cap_is_decoded_once() {
    let _counters = COUNTERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut renderer = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("bundled monospace font");
    let (cw, ch) = renderer.cell_size();
    let (side, tile_cols, tile_rows) = (9usize, 10usize, 5usize);
    let (cols, rows) = (side * tile_cols, side * tile_rows);
    let mut terminal = Terminal::new(rows as u16, cols as u16);
    terminal.set_cell_pixel_size(cw as u16, ch as u16);
    for i in 0..side * side {
        let (r, c) = (i / side * tile_rows + 1, i % side * tile_cols + 1);
        let rgba: Vec<u8> = [i as u8, 100, 200, 255].repeat(8 * 6);
        let payload = aterm_codec::base64::encode(&rgba).expect("base64");
        terminal.process(
            format!(
                "\x1b[{r};{c}H\x1b_Ga=T,f=32,s=8,v=6,c={tile_cols},r={tile_rows},C=1;{payload}\x1b\\"
            )
            .as_bytes(),
        );
    }
    let input = terminal.cell_frame(rows, cols);
    let mut shown: Vec<usize> = input
        .images
        .iter()
        .flatten()
        .map(|(_, image)| std::sync::Arc::as_ptr(&image.image) as usize)
        .collect();
    shown.sort_unstable();
    shown.dedup();
    assert_eq!(shown.len(), side * side, "every thumbnail is on screen");

    let raster = tile_cols * cw * tile_rows * ch * 4;
    let mut cache = WindowCpu::new();
    let ((), first) = count_frames(raster, || {
        let _ = renderer.render_input_cached(&mut cache, &input);
    });
    assert!(
        first >= side * side,
        "the first frame decodes every tile ({first})"
    );
    for frame in 0..2 {
        cache.invalidate();
        let ((), again) = count_frames(raster, || {
            let _ = renderer.render_input_cached(&mut cache, &input);
        });
        assert_eq!(again, 0, "repaint {frame} decoded {again} thumbnails again");
    }
}
