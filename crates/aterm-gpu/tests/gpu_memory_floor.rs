// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GPU memory floor: what a terminal's GPU stack may hold while drawing only
// text.
//
//   1. Suballocator: `GpuContext::new` uses `terminal_memory_hints`, not
//      `MemoryHints::Performance`, whose 128 MiB device + 64 MiB host block
//      floors are reserved on first touch (system RAM on integrated adapters)
//      for a few MB of actual allocations.
//   2. Bloom target: the half-resolution texture is built only when a frame
//      carries a live glow, not with every `Offscreen`.
//
// No GPU / no system font: no-op. A backend with no suballocator report
// (GLES/WebGL2) skips the reservation arm only.

use aterm_core::terminal::Terminal;
use aterm_gpu::WindowGpu;
use aterm_render::{GlowQuad, Theme, premul_rgb};

use crate::common;

const MIB: u64 = 1024 * 1024;

/// What the suballocator may reserve before the first frame. The shipped hint
/// reserves about 24 MiB (one 16 MiB device block + one 8 MiB host block);
/// `Performance` would reserve 192 MiB.
const MAX_RESERVED_AT_CONSTRUCT: u64 = 64 * MIB;

/// What it may reserve after a ~1080p frame is rendered and read back (the
/// full-surface offscreen plus readback staging): about 32 MiB with the
/// shipped hint, 256 MiB under `Performance`.
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
    let Some(mut gpu) = common::wgpu_oracle_or_skip(18.0, Theme::default()) else {
        return;
    };
    let (adapter, backend) = gpu.adapter();
    let Some((used, reserved, blocks)) = gpu.allocator_reserved() else {
        // A skip-by-return reports `ok`, so it must be legitimate:
        // `allocator_reserved` returns `None` only by compile-time cfg (no
        // `wgpu_arm`; Metal's armed resources are exact-size mints). On a
        // build with the arm, `None` means the report regressed and the gate
        // would guard nothing.
        assert!(
            !cfg!(wgpu_arm) || backend.eq_ignore_ascii_case("metal"),
            "{backend} produced no suballocator report, and it is not the one \
             backend known to have none. Metal mints shared-storage exact-size \
             resources and exposes no report surface; every other backend on a \
             `wgpu_arm` build must produce one, so a `None` here is the report \
             regressing and this gate would go on reporting `ok` while guarding \
             nothing"
        );
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

/// The half-res bloom target is built on demand: absent until a frame carries
/// a live glow, present and used on the frame that does.
#[test]
fn bloom_target_is_built_on_demand_not_with_the_offscreen() {
    let Some(mut gpu) = common::wgpu_oracle_or_skip(18.0, Theme::default()) else {
        return;
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
        color2: premul_rgb(0x0050_FA7B, 255),
        alpha2: 0,
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
