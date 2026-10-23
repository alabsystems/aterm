// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Allocation gate for the CPU fx-layer rasterizer.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRamps, FxRect, FxSlot, FxSpace, FxSprite,
    FxTexels, PxRect, RenderInput,
};
use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu, embedded_font};

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
        // SAFETY: `ptr` and `layout` came from this pass-through allocator; `new_size` is
        // forwarded unchanged.
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

/// A card-like set of layers over every slot and both blends.
fn layers(cw: usize, ch: usize, dy: i32) -> Vec<FxLayer> {
    let (cw, ch) = (cw as i32, ch as i32);
    let kitty: Vec<u8> = (0..8 * 6)
        .map(|i| [0u8, 1, 2, 3][(i * 7 + i / 8) % 4])
        .collect();
    let kitty = Arc::new(FxArt::new(8, 6, FxTexels::Index8, kitty).expect("sized"));
    let star = Arc::new(
        FxArt::new(
            2,
            2,
            FxTexels::Rgba8Premul,
            vec![
                0x30, 0x20, 0x10, 0, 0, 0, 0, 0, 0x40, 0x40, 0x40, 0x80, 0x10, 0, 0x20, 0,
            ],
        )
        .expect("sized"),
    );
    let straight = Arc::new(
        FxArt::new(
            2,
            1,
            FxTexels::Rgba8Straight,
            vec![200, 120, 40, 255, 30, 90, 220, 140],
        )
        .expect("sized"),
    );
    let mut under = FxLayer::new(
        1,
        Arc::new(FxContent::new(
            vec![
                FxItem::Rect(FxRect {
                    x: cw,
                    y: ch / 2,
                    w: (10 * cw) as u16,
                    h: (3 * ch) as u16,
                    ink: 0,
                }),
                FxItem::Ramps(FxRamps {
                    x: 2 * cw,
                    y: ch,
                    w: (6 * cw) as u16,
                    h: 2,
                    ink: 0,
                }),
            ],
            vec![
                FxInk::add(0x0030_2010),
                FxInk::add(0x0010_2030),
                FxInk::add(0x0008_0808),
                FxInk::add(0x0020_0020),
            ],
        )),
        FxSlot::Under,
        FxSpace::Grid,
        FxBlend::Exact,
    );
    under.offset.1 = dy;
    let glow = FxLayer::new(
        2,
        Arc::new(FxContent::new(
            vec![
                FxItem::Sprite(FxSprite {
                    x: 4 * cw,
                    y: ch,
                    dw: 8 * 3,
                    dh: (2 * ch) as u16,
                    flip_x: true,
                    art: kitty,
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                }),
                FxItem::Sprite(FxSprite {
                    x: 9 * cw,
                    y: ch + 3,
                    dw: 6,
                    dh: (ch + 2) as u16,
                    flip_x: false,
                    art: star,
                    ink: 0,
                    clip: PxRect::new(9 * cw, 0, 9 * cw + 5, 1000),
                }),
            ],
            vec![
                FxInk::over(0x0040_2000, 0x60),
                FxInk::add(0x0020_4060),
                FxInk::over(0x0011_2233, 255),
            ],
        )),
        FxSlot::Glow,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let over = FxLayer::new(
        3,
        Arc::new(FxContent::new(
            vec![FxItem::Rect(FxRect {
                x: 0,
                y: ch,
                w: (5 * cw) as u16,
                h: (3 * ch) as u16,
                ink: 0,
            })],
            vec![FxInk::over(0x0020_1010, 0x50); 2],
        )),
        FxSlot::Over,
        FxSpace::Grid,
        FxBlend::Exact,
    );
    let linear = |id, slot| {
        FxLayer::new(
            id,
            Arc::new(FxContent::new(
                vec![FxItem::Sprite(FxSprite {
                    x: 6 * cw,
                    y: 2 * ch,
                    dw: 7,
                    dh: (2 * ch) as u16,
                    flip_x: false,
                    art: straight.clone(),
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                })],
                vec![FxInk::over(0x00E0_F0FF, 200)],
            )),
            slot,
            FxSpace::Grid,
            FxBlend::Linear,
        )
    };
    vec![
        under,
        glow,
        over,
        linear(4, FxSlot::Under),
        linear(5, FxSlot::Over),
    ]
}

/// Two text frames differing on one row, each carrying `fx` and `occluders`.
fn frames(
    t: &mut Terminal,
    rows: usize,
    cols: usize,
    fx: [&[FxLayer]; 2],
    occluders: &[PxRect],
) -> [RenderInput; 2] {
    let mut out = [0, 1].map(|k| {
        t.process(format!("\x1b[3;1Hframe {k} changes row two").as_bytes());
        t.cell_frame(rows, cols)
    });
    for (input, layers) in out.iter_mut().zip(fx) {
        input.fx_layers = layers.to_vec();
        input.fx_occluders = occluders.to_vec();
    }
    out
}

/// Render `inputs` alternately, warm, then count the allocations of 32 more
/// frames and check each took the damage path `want`.
fn steady_allocs(
    renderer: &mut Renderer,
    window: &mut WindowCpu,
    inputs: &[RenderInput; 2],
    want: DamageOutcome,
) -> u64 {
    for input in inputs.iter().cycle().take(6) {
        let _ = renderer.render_input_cached(window, input);
    }
    let (checksum, allocs) = counted(|| {
        let mut checksum = 0u32;
        for input in inputs.iter().cycle().take(32) {
            let view = renderer.render_input_cached(window, input);
            let pixels = view.pixels();
            checksum ^= pixels[pixels.len() / 2];
            black_box(checksum);
            assert_eq!(window.last_damage(), want, "the frame took another path");
        }
        checksum
    });
    black_box(checksum);
    allocs
}

#[test]
fn the_fx_path_allocates_nothing_once_warm() {
    let mut renderer = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("bundled monospace font builds");
    renderer.set_pad(4);
    let (cw, ch) = renderer.cell_size();
    let (rows, cols) = (6usize, 16usize);
    let mut t = Terminal::new(rows as u16, cols as u16);
    t.process(b"\x1b[?25l");
    for r in 0..rows {
        t.process(format!("\x1b[{};1Hrow {r} of text", r + 1).as_bytes());
    }
    let occluders = [
        PxRect::at(0, (2 * ch) as i32, 40, 6),
        PxRect::at((7 * cw) as i32, 0, 9, (4 * ch) as u32),
    ];
    let mut window = WindowCpu::new();

    let bare = frames(&mut t, rows, cols, [&[], &[]], &[]);
    let none = steady_allocs(&mut renderer, &mut window, &bare, DamageOutcome::Rows);
    assert_eq!(none, 0, "frames without fx must allocate nothing");

    let settled = layers(cw, ch, 0);
    let held = frames(&mut t, rows, cols, [&settled, &settled], &occluders);
    let at_rest = steady_allocs(&mut renderer, &mut window, &held, DamageOutcome::Rows);
    assert_eq!(
        at_rest, 0,
        "settled layers sliced on Rows frames must allocate nothing"
    );

    let (a, b) = (layers(cw, ch, 0), layers(cw, ch, 3));
    let moving = frames(&mut t, rows, cols, [&a, &b], &occluders);
    let in_motion = steady_allocs(&mut renderer, &mut window, &moving, DamageOutcome::Rows);
    assert_eq!(
        in_motion, 0,
        "changing layers must allocate nothing once warm"
    );
}
