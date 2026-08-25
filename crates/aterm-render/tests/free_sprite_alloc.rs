// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Allocation gate for the CPU free-sprite compositor. This is its own test
// binary because the process-global counting allocator must not instrument
// unrelated renderer tests.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ, SceneAtlas};
use aterm_core::terminal::Terminal;
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

fn atlas() -> Arc<SceneAtlas> {
    let (width, height) = (64u32, 64u32);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
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
    Arc::new(SceneAtlas {
        width,
        height,
        rgba,
        version: 1,
    })
}

fn sprite(x: i32, w: u16, h: u16, aw: u16, ah: u16, flip_x: bool) -> FreeSprite {
    FreeSprite {
        x,
        y: 3,
        w,
        h,
        ax: 0,
        ay: 0,
        aw,
        ah,
        tint: 0x00E8_F5FF,
        alpha: 217,
        flip_x,
        z: FreeZ::UnderText,
        sampler: FreeSampler::Nearest,
    }
}

/// The pet/mote/note regime is 1:1 and must allocate nothing even on its first
/// real stamp; scaled/flip sprites may grow the column map once, then must reuse
/// it across motion frames. Before the resident scratch fix, each counted frame
/// allocated and dropped a fresh `Vec<u32>` in `stamp_free_sprites`.
#[test]
fn active_free_sprites_do_not_allocate_per_frame() {
    let mut renderer = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("bundled monospace font builds");
    renderer.set_pad(4);
    let (_, cell_h) = renderer.cell_size();
    let (rows, cols) = (4usize, 16usize);
    let mut terminal = Terminal::new(rows as u16, cols as u16);
    terminal.process(b"\x1b[?25l");
    let atlas = atlas();

    let mut make = |free: FreeSprite| {
        let mut input = terminal.cell_frame(rows, cols);
        input.free_atlas = Some(Arc::clone(&atlas));
        input.free_sprites = vec![free];
        input
    };

    // Prime every frame/cache container while keeping the x-map untouched: the
    // zero-width sprite occupies the cached `free_sprites` Vec but stamps no texel.
    let dormant = make(sprite(2, 0, cell_h as u16, 8, 8, false));
    let one_to_one = make(sprite(
        2,
        22,
        (cell_h + 5) as u16,
        22,
        (cell_h + 5) as u16,
        true,
    ));
    let mut window = WindowCpu::new();
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
        "a first active 1:1 pet/mote/note stamp must not allocate an x-map"
    );

    // Exercise the general sampler too: the first scaled frame is allowed to
    // grow the resident x-map; every later scaled, flipped, clipped move reuses it.
    let scaled_a = make(sprite(-7, 39, (2 * cell_h + 3) as u16, 13, 9, false));
    let scaled_b = make(sprite(9, 37, (2 * cell_h + 1) as u16, 13, 9, true));
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
        "warm scaled/flip/clipped free-sprite motion must allocate zero times"
    );
}
