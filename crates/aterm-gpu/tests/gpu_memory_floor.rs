// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE GPU MEMORY FLOOR: what a terminal's GPU stack is allowed to hold when it
// is drawing nothing but text.
//
// Two independent eager allocations used to sit here, and both were invisible
// to every other test in the tree because nothing had ever asked the renderer
// how many bytes it was holding:
//
//   1. The SUBALLOCATOR. `GpuContext::new` requested `MemoryHints::Performance`,
//      which tells wgpu-hal to floor its block sizes at 128 MiB (device) + 64 MiB
//      (host). gpu-allocator creates a driver heap of exactly that size the FIRST
//      time each memory type is touched — during renderer construction — so a
//      terminal that sub-allocates 2.94 MB reserved 192 MB. On an INTEGRATED
//      adapter (this box: AMD Radeon 780M, DX12) every D3D12 heap is system RAM,
//      so all of it landed in the process working set. Measured: 337.8 MB working
//      set steady-state before, 97.8 MB after (see `terminal_memory_hints`).
//
//   2. The BLOOM TARGET. The half-resolution texture the comet halo is extracted
//      into was built alongside the `Offscreen`, on every window, whether or not
//      a glow ever lit — 2.05 MB at 1914x1071, 8.4 MB at 4180x2016, for a pass
//      the shipped `cursor_trail = false` default never runs.
//
// Both are lifetime-only changes: the same resources, built when something asks
// for them. This file is the standing gate that they stay that way.
//
// Gated: no GPU / no system font -> the test no-ops (returns). A backend with no
// suballocator report (GLES/WebGL2 has none) skips the reservation arm only.

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, WindowGpu};
use aterm_render::{GlowQuad, Theme, premul_rgb};

const MIB: u64 = 1024 * 1024;

/// What the suballocator may reserve for a renderer that has not drawn a frame.
///
/// Measured on this box with the shipped hint: **24 MiB** (one 16 MiB device
/// block + one 8 MiB host block), against 2.94 MB actually sub-allocated. Under
/// the old `MemoryHints::Performance` it was **192 MiB** — so this bound is what
/// catches a regression to it, with room for a backend whose first block lands
/// differently.
const MAX_RESERVED_AT_CONSTRUCT: u64 = 64 * MIB;

/// What it may reserve once a ~1080p frame has been rendered and read back —
/// the largest single allocation a terminal makes (the full-surface offscreen)
/// plus its readback staging. Measured: **32 MiB**. Under `Performance`: 256 MiB.
const MAX_RESERVED_AFTER_FRAME: u64 = 128 * MIB;

fn mib(bytes: u64) -> f64 {
    bytes as f64 / MIB as f64
}

/// A ~1080p grid on this suite's standard 18px face.
const ROWS: usize = 51;
const COLS: usize = 174;

fn scene() -> aterm_render::RenderInput {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ cargo build --release\r\n   Compiling aterm v0.62.0\r\n");
    term.cell_frame(ROWS, COLS)
}

/// The suballocator must not floor itself at a streaming engine's block sizes.
#[test]
fn suballocator_reserves_a_terminal_sized_floor() {
    let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
        // THE FLIP: this file audits the WGPU-HAL suballocator floors — the
        // WGPU ORACLE arm, asked for by name post-flip.
        Ok(mut g) => {
            #[cfg(target_os = "macos")]
            g.disarm_metal_for_oracle();
            g
        }
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let (adapter, backend) = gpu.adapter();
    let Some((used, reserved, blocks)) = gpu.allocator_reserved() else {
        eprintln!("SKIP: {backend} exposes no suballocator report");
        return;
    };
    eprintln!(
        "GPU floor [{adapter} / {backend}] at construct: used={:.2} MiB reserved={:.2} MiB blocks={:?}",
        mib(used),
        mib(reserved),
        blocks.iter().map(|b| mib(*b)).collect::<Vec<_>>()
    );
    // Non-vacuity: the renderer really did sub-allocate (glyph atlas, uniforms),
    // so a zero-reservation pass cannot be an unbuilt device.
    assert!(
        used > 0,
        "a constructed renderer must have sub-allocated something"
    );
    assert!(
        reserved <= MAX_RESERVED_AT_CONSTRUCT,
        "a renderer that has drawn NOTHING reserved {:.2} MiB of driver heap for \
         {:.2} MiB of resources (cap {:.2} MiB). The suballocator block floor has \
         regressed — see `aterm_gpu::terminal_memory_hints`.",
        mib(reserved),
        mib(used),
        mib(MAX_RESERVED_AT_CONSTRUCT),
    );

    let mut win = WindowGpu::new();
    let frame = gpu.render_input(&mut win, &scene(), None);
    let (fw, fh) = (frame.width, frame.height);
    drop(frame);
    let (used2, reserved2, blocks2) = gpu
        .allocator_reserved()
        .expect("the report was available a moment ago");
    eprintln!(
        "GPU floor after a {fw}x{fh} frame: used={:.2} MiB reserved={:.2} MiB blocks={:?}",
        mib(used2),
        mib(reserved2),
        blocks2.iter().map(|b| mib(*b)).collect::<Vec<_>>()
    );
    assert!(
        used2 > used,
        "rendering a frame must sub-allocate more than construction did \
         (used {:.2} -> {:.2} MiB)",
        mib(used),
        mib(used2)
    );
    assert!(
        reserved2 <= MAX_RESERVED_AFTER_FRAME,
        "after one {fw}x{fh} frame the suballocator reserved {:.2} MiB for \
         {:.2} MiB of resources (cap {:.2} MiB)",
        mib(reserved2),
        mib(used2),
        mib(MAX_RESERVED_AFTER_FRAME),
    );
}

/// The half-res bloom target is DEMAND-BUILT: absent until a frame carries a
/// live glow, present on the frame that does, and actually used by it.
#[test]
fn bloom_target_is_built_on_demand_not_with_the_offscreen() {
    let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
        // THE FLIP: this file audits the WGPU-HAL suballocator floors — the
        // WGPU ORACLE arm, asked for by name post-flip.
        Ok(mut g) => {
            #[cfg(target_os = "macos")]
            g.disarm_metal_for_oracle();
            g
        }
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    // The whole point is the SHIPPED configuration: bloom enabled, nothing
    // drawing through it.
    assert!(
        gpu.bloom_enabled(),
        "this gate is about the default, batteries-included bloom"
    );
    let mut win = WindowGpu::new();
    let mut input = scene();
    input.cursor_visible = false;

    let plain = gpu.render_input(&mut win, &input, None);
    let plain_pixels = plain.pixels.clone();
    let (w, h) = (plain.width, plain.height);
    drop(plain);
    assert!(
        !win.bloom_target_resident(),
        "a glow-free frame must not allocate the {}x{} half-res bloom texture \
         (~{:.2} MiB) — the shipped cursor_trail = false default never draws it",
        w / 2,
        h / 2,
        mib((w / 2) as u64 * (h / 2) as u64 * 4),
    );

    // Now light a glow: a premultiplied additive quad on row 1, the same shape
    // the LUMEN aurora emits.
    let (cw, ch) = gpu.cell_size();
    input.cursor_glow_add.push(GlowQuad {
        row: 1,
        x: (4 * cw) as u16,
        y: ch as u16,
        w: (6 * cw) as u16,
        h: ch as u16,
        color: premul_rgb(0x0050_FA7B, 255),
        // ADDITIVE light (see `GlowQuad::alpha`).
        alpha: 0,
    });
    let lit = gpu.render_input(&mut win, &input, None);
    let lit_pixels = lit.pixels.clone();
    drop(lit);

    assert!(
        win.bloom_target_resident(),
        "the first frame with a live glow must build the bloom target"
    );
    // Non-vacuity for the demand rule: the lit frame really did paint through
    // the bloom path, so residency is not being asserted about a dead pass.
    assert_ne!(
        plain_pixels, lit_pixels,
        "the glow frame must actually paint (glow + its bloom halo)"
    );
    // And the halo must reach BEYOND the quad: the composite is what needs the
    // half-res target, so a pixel outside the quad's own rows must have changed.
    let above = (ch - 1) * w + 6 * cw;
    assert_ne!(
        plain_pixels[above], lit_pixels[above],
        "the bloom halo must spread past the glow quad's own band \
         (that spread is the only thing the half-res target exists for)"
    );
}
