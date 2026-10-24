// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Allocation gate for the companions' sprite layers on the CPU compositor.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aterm_core::render::{FxArt, FxTexels};
use aterm_core::terminal::Terminal;
use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers};
use aterm_render::{Renderer, Theme, WindowCpu, embedded_font};

struct CountingAlloc;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

#[inline]
fn note_alloc() {
    if ACTIVE.load(Ordering::Relaxed) {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc();
        // SAFETY: forward the allocator's valid layout unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_alloc();
        // SAFETY: forward the allocator's valid layout unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_alloc();
        // SAFETY: `ptr` and `layout` came from this pass-through allocator; the
        // requested size is forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` came from the System allocator above.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn counted<R>(f: impl FnOnce() -> R) -> (R, u64) {
    ALLOCS.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    let result = f();
    ACTIVE.store(false, Ordering::Relaxed);
    (result, ALLOCS.load(Ordering::Relaxed))
}

fn art(width: u16, height: u16) -> Arc<FxArt> {
    let mut rgba = Vec::with_capacity(usize::from(width) * usize::from(height) * 4);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[
                (17 * x + 11 * y) as u8,
                (31 * x + 7 * y) as u8,
                (5 * x + 29 * y) as u8,
                180 + ((x + y) % 76) as u8,
            ]);
        }
    }
    Arc::new(FxArt::new(width, height, FxTexels::Rgba8Straight, rgba).expect("a sized art"))
}

/// `art` stretched to `w × h` at `(x, 3)`: one held art a pose, as the bakers
/// hold theirs (an art builds its row runs once, on its first draw).
fn sprite(x: i32, w: u16, h: u16, art: &Arc<FxArt>, flip_x: bool) -> FreeSprite {
    FreeSprite {
        w,
        h,
        tint: 0x00E8_F5FF,
        alpha: 217,
        flip_x,
        z: FreeZ::UnderText,
        ..FreeSprite::whole(x, 3, Arc::clone(art))
    }
}

#[test]
fn active_free_sprites_do_not_allocate_per_frame() {
    let mut renderer = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("bundled monospace font builds");
    renderer.set_pad(4);
    let (_, cell_h) = renderer.cell_size();
    let (rows, cols) = (4usize, 16usize);
    let mut terminal = Terminal::new(rows as u16, cols as u16);
    terminal.process(b"\x1b[?25l");
    let mut layers = SpriteLayers::new(1);
    let mut make = |free: FreeSprite| {
        let mut input = terminal.cell_frame(rows, cols);
        layers.publish(&[free], &mut input.fx_layers);
        input
    };

    let (pose, small, wide) = (art(22, (cell_h + 5) as u16), art(8, 8), art(13, 9));
    let dormant = make(sprite(2, 0, cell_h as u16, &small, false));
    // A sprite shown once, at rest since: the window's fx scratch has grown to
    // one layer on both the full and the damaged path (a first frame with any
    // layer sizes it), and keeps it.
    let warm = make(sprite(5, 22, (cell_h + 5) as u16, &pose, false));
    let one_to_one = make(sprite(2, 22, (cell_h + 5) as u16, &pose, true));
    let mut window = WindowCpu::new();
    for input in [&dormant, &warm, &dormant, &warm] {
        let _ = renderer.render_input_cached(&mut window, input);
    }
    let dormant_pixels = renderer
        .render_input_cached(&mut window, &dormant)
        .pixels()
        .to_vec();
    let _ = renderer.render_input_cached(&mut window, &dormant);

    let (one_to_one_changed, first_active_allocs) = counted(|| {
        let view = renderer.render_input_cached(&mut window, &one_to_one);
        let pixels = view.pixels();
        black_box(pixels[0]);
        pixels != dormant_pixels.as_slice()
    });
    assert!(
        one_to_one_changed,
        "the 1:1 sprite must really stamp pixels"
    );
    assert_eq!(
        first_active_allocs, 0,
        "a 1:1 pet/mote/note waking from rest must not allocate"
    );

    let scaled_a = make(sprite(-7, 39, (2 * cell_h + 3) as u16, &wide, false));
    let scaled_b = make(sprite(9, 37, (2 * cell_h + 1) as u16, &wide, true));
    let _ = renderer.render_input_cached(&mut window, &scaled_a);
    let _ = renderer.render_input_cached(&mut window, &scaled_b);

    let (checksum, steady_allocs) = counted(|| {
        let mut checksum = 0u32;
        for input in [&scaled_a, &scaled_b].into_iter().cycle().take(32) {
            let view = renderer.render_input_cached(&mut window, input);
            let pixels = view.pixels();
            checksum ^= pixels[pixels.len() / 2];
            black_box(checksum);
        }
        checksum
    });
    black_box(checksum);
    assert_eq!(
        steady_allocs, 0,
        "warm scaled/flip/clipped sprite-layer motion must allocate zero times"
    );
}
