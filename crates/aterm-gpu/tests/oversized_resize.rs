// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// An oversized grid must not abort the process.
//
// The control-socket `resize` verb allows up to 4096 rows/cols (aterm-core's
// MAX_GRID_ROWS/COLS) but does not bound pixels. The padded framebuffer
// (`cols·cell_w + 2·pad`) sizes both the swapchain and the offscreen, and wgpu
// validates textures against `max_texture_dimension_2d`; with no uncaptured-error
// handler a violation panics and aborts. Both sites clamp the framebuffer to the
// device limit, so an extreme grid renders a clipped view.
//
// This drives the offscreen path (`render_input` → `encode_frame` →
// `offscreen_texture`; a swapchain needs a real window) with two different
// oversized grids and checks that neither aborts and both clamp to the same
// readback width, the device limit.
//
// Skips without a GPU or system font.

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, PresentCrop, WindowGpu};
use aterm_render::Theme;

use crate::common;

/// Render a `rows × cols` blank grid to the offscreen and return the readback width
/// (== the offscreen texture width == the clamped framebuffer width).
fn offscreen_width(gpu: &mut GpuRenderer, win: &mut WindowGpu, rows: usize, cols: usize) -> usize {
    let mut term = Terminal::new(rows as u16, cols as u16);
    let input = term.cell_frame(rows, cols);
    // Returns a frame whose width is clamped to the device limit.
    gpu.render_input(win, &input, None).width
}

#[test]
fn oversized_grid_clamps_offscreen_instead_of_aborting() {
    // A LARGE font so a modest column count (well within MAX_GRID_COLS == 4096, so
    // aterm-core doesn't clamp the grid) already blows past even a 32768-limit GPU.
    let Some(mut gpu) = common::gpu_or_skip(40.0, Theme::default()) else {
        return;
    };
    let (cw, _ch) = gpu.cell_size();
    assert!(cw >= 1, "cell width must be positive");

    let mut win = WindowGpu::new();

    // Two column counts whose padded widths both exceed any real
    // `max_texture_dimension_2d` (<= 32768) while staying within MAX_GRID_COLS,
    // targeting ~50k and ~90k px so they differ for any cell width.
    let rows = 2usize;
    let cols_a = (50_000usize / cw).clamp(2, 4096);
    let cols_b = (90_000usize / cw).clamp(2, 4096);
    let req_a = cols_a * cw; // approx requested framebuffer width (ignoring pad)
    let req_b = cols_b * cw;

    let w_a = offscreen_width(&mut gpu, &mut win, rows, cols_a);
    let w_b = offscreen_width(&mut gpu, &mut win, rows, cols_b);

    // Both readback widths are valid and equal, i.e. clamped to one device
    // limit.
    assert!(
        w_a >= 1 && w_b >= 1,
        "clamped widths must stay valid (>= 1)"
    );
    assert_eq!(
        w_a, w_b,
        "two oversized grids (requested {req_a}px vs {req_b}px) must both clamp the offscreen \
         to the SAME device max_texture_dimension_2d"
    );
    // And the clamp is strictly below the requested framebuffer (a real clamp, not a
    // full-size allocation) whenever the two requests actually differ.
    if cols_a != cols_b {
        assert!(
            w_a < req_a && w_a < req_b,
            "offscreen width {w_a} must be clamped below the requested framebuffers \
             ({req_a}px, {req_b}px)"
        );
    }
    eprintln!(
        "oversized grid clamped offscreen to {w_a}px (requested {req_a}px / {req_b}px, cell_w={cw}) — no abort"
    );
}

/// A frontend crop is in logical raw-frame coordinates. For an oversized grid
/// the resident offscreen is only the clamped top prefix, so a valid logical
/// crop is normalized to that prefix rather than rejected.
#[test]
fn oversized_tall_grid_normalizes_cropped_virtual_present() {
    let Some(mut gpu) = common::gpu_or_skip(18.0, Theme::default()) else {
        return;
    };
    let (rows, cols) = (2048usize, 4usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let input = term.cell_frame(rows, cols);
    let (logical_w, logical_h) = gpu.frame_size(rows, cols);
    let logical_h = u32::try_from(logical_h).expect("fixture fits u32");

    let mut win = WindowGpu::new();
    let resident = gpu.render_input(&mut win, &input, None);
    let resident_h = u32::try_from(resident.height).expect("GPU extent fits u32");
    let resident_w = u32::try_from(resident.width).expect("GPU extent fits u32");
    if resident_h >= logical_h {
        eprintln!("SKIP: logical height {logical_h}px did not exceed this device's resident limit");
        return;
    }

    // Three rows removed above and two below is a valid asymmetric logical crop
    // whose end lies below the resident prefix; the GPU trims its height to
    // `resident_h - 3`.
    let valid = PresentCrop {
        source_y: 3,
        height: logical_h - 5,
    };
    assert!(
        gpu.present_virtual_cropped_with_effects_transport(
            &mut win,
            &input,
            false,
            None,
            None,
            valid,
            0,
            (resident_w, resident_h - valid.source_y),
        ),
        "a valid logical crop must survive resident-source normalization"
    );

    // Logically malformed intervals remain strict failures; normalization must
    // never turn them into apparently valid resident crops.
    for malformed in [
        PresentCrop {
            source_y: 0,
            height: 0,
        },
        PresentCrop {
            source_y: logical_h - 1,
            height: 2,
        },
        // Valid logically, but wholly below the resident prefix: empty on this
        // device, therefore also a graceful failure.
        PresentCrop {
            source_y: resident_h + 1,
            height: 1,
        },
    ] {
        assert!(
            !gpu.present_virtual_cropped_with_effects_transport(
                &mut win,
                &input,
                false,
                None,
                None,
                malformed,
                0,
                (resident_w, resident_h),
            ),
            "malformed/empty crop {malformed:?} must be rejected"
        );
    }

    eprintln!(
        "cropped virtual present normalized logical {logical_w}x{logical_h} to resident {}x{}",
        resident.width, resident.height
    );
}

// Instance streams are bounded by the clamped framebuffer: `encode_frame` skips
// cells whose pixel origin is off-screen (byte-identical, since they were
// clipped anyway), so a dense MAX_GRID grid cannot request a vertex buffer past
// `max_buffer_size`, which would abort.
//
// This renders a wide, short, width-clamped grid with every cell filled and
// checks the total instance count stays well below the grid cell count.
#[test]
fn densely_filled_oversized_grid_bounds_instances_no_abort() {
    let Some(mut gpu) = common::gpu_or_skip(40.0, Theme::default()) else {
        return;
    };
    let (cw, _ch) = gpu.cell_size();
    let mut win = WindowGpu::new();

    // 4096 cols of a large cell is ~98k px, past any real limit, so the width
    // clamps and the column loop-clip engages. Few rows keep the readback small.
    let rows = 8usize;
    let cols = 4096usize;
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Fill EVERY cell with a non-space glyph (autowrap tiles the whole grid) so the
    // GlyphInstance stream is maximal — the stream that overflowed pre-fix.
    term.process(&vec![b'X'; rows * cols]);
    let input = term.cell_frame(rows, cols);

    let frame = gpu.render_input(&mut win, &input, None);
    let total = gpu.last_instances();

    // Reaching here == no abort (pre-fix the glyph/bg streams' `create_buffer` could
    // exceed max_buffer_size and abort).
    assert!(total >= 1, "a densely-filled grid must emit some instances");
    // Streams are bounded by the on-screen area: unbounded code would build
    // about two instances per cell.
    assert!(
        total < rows * cols,
        "instance streams must be bounded by the clamped framebuffer, not the grid: \
         got {total} instances for a {rows}x{cols} grid (cell_w={cw})"
    );
    // The clamped offscreen was still produced (width clamped below the requested px).
    assert!(
        frame.width >= 1 && frame.width < cols * cw,
        "offscreen width {} must be clamped below the requested {}px",
        frame.width,
        cols * cw
    );
    eprintln!(
        "densely-filled {rows}x{cols} grid: {total} instances (bounded vs {} cells), \
         offscreen {}x{} — no abort",
        rows * cols,
        frame.width,
        frame.height
    );
}

// The scissored dirty-row band clamps both its top and bottom to the clamped
// framebuffer height. A change confined to a row below the clamp (the prompt on
// the bottom row of a tall grid) would otherwise produce a scissor outside the
// render target, which wgpu rejects with an abort; clamped, it is a zero-height
// in-bounds rect.
//
// This presents a tall clamped grid, then changes only the bottom row (cursor
// hidden) through the real scissored path; the scissor counter shows the
// dirty-row path ran.
#[test]
fn oversized_tall_grid_bottom_row_change_scissors_without_abort() {
    let Some(mut gpu) = common::gpu_or_skip(18.0, Theme::default()) else {
        return;
    };
    let (_cw, ch) = gpu.cell_size();
    let mut win = WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    // 2048 rows exceeds every current `max_texture_dimension_2d`, so the height
    // clamps and bottom rows lie beyond it. Narrow keeps the readback cheap.
    let (rows, cols) = (2048usize, 4usize);
    let mut term = Terminal::new(rows as u16, cols as u16);

    // Frame 1: first paint (full repaint) primes the present-path prior frame.
    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false; // keep the (row-0) cursor from dirtying an on-screen row
    let frame = gpu.present_input_readback(&mut win, &input);
    assert!(
        frame.height < rows * ch,
        "grid height {}px must have been clamped (got {}px) for this regression to bite",
        rows * ch,
        frame.height
    );

    // Frame 2: change ONLY the BOTTOM row — its pixel origin is far below the clamp.
    // PRE-FIX: the scissor origin exceeded the attachment ⇒ validation abort HERE.
    term.process(format!("\x1b[{rows};1HZ").as_bytes());
    let mut input2 = term.cell_frame(rows, cols);
    input2.cursor_visible = false;
    let scissor_before = gpu.scissor_taken();
    let frame2 = gpu.present_input_readback(&mut win, &input2);

    // Reaching here == no abort. The change was reusable (same dims/offset), so the
    // SCISSOR path — the one that built the out-of-bounds rect — must have run.
    assert!(
        gpu.scissor_taken() > scissor_before,
        "a bottom-row-only change on a reusable frame must take the scissor path"
    );
    // The clipped view is unchanged (the changed row is off-screen).
    assert_eq!(
        (frame.width, frame.height),
        (frame2.width, frame2.height),
        "clamped offscreen dims must be stable across presents"
    );
    assert!(
        frame.pixels == frame2.pixels,
        "an off-screen-row change must leave the clamped view byte-identical"
    );
    eprintln!(
        "tall {rows}x{cols} grid clamped to {}x{}: bottom-row change scissored without abort",
        frame2.width, frame2.height
    );
}

// The same scissor clamp through the readback present path
// (`present_input_readback` → `encode_present_frame`): frame 1 paints the bottom
// row (full), frame 2 changes only that row, far below the clamped height, so the
// band's `y0` collapses to a degenerate rect instead of an invalid one.
#[test]
fn oversized_tall_grid_below_fold_scissor_clamps_no_abort() {
    let Some(mut gpu) = common::gpu_or_skip(40.0, Theme::default()) else {
        return;
    };
    let (cw, ch) = gpu.cell_size();
    assert!(cw >= 1 && ch >= 1, "cell size must be positive");
    let mut win = WindowGpu::new();

    // rows*ch exceeds any real `max_texture_dimension_2d`, so the height clamps
    // and bottom rows fall below the fold; within MAX_GRID_ROWS so aterm-core
    // does not clamp the grid.
    let rows = 2048usize;
    let cols = 4usize;
    let mut term = Terminal::new(rows as u16, cols as u16);

    // Frame 1 (FULL, first frame): paint the BOTTOM row so the only content lives
    // below the fold and the cursor stays there (no above-fold dirty row in frame 2).
    term.process(format!("\x1b[{rows};1Ha").as_bytes());
    let in1 = term.cell_frame(rows, cols);
    let frame1 = gpu.present_input_readback(&mut win, &in1);
    assert_eq!(gpu.full_repaints(), 1, "first frame must be a full repaint");
    assert_eq!(gpu.scissor_taken(), 0);

    // The height must have clamped for a below-fold dirty row to exist; skip on
    // a GPU with a larger limit.
    let requested_h = rows * ch;
    if frame1.height >= requested_h {
        eprintln!(
            "SKIP: offscreen height {} not clamped below requested {requested_h}px \
             (max_texture_dimension_2d too large to exercise the below-fold scissor)",
            frame1.height
        );
        return;
    }

    // Frame 2: change only the bottom row; its `y0` is far past the clamped `h`
    // and clamps to it.
    term.process(b"b");
    let in2 = term.cell_frame(rows, cols);
    let scissor_before = gpu.scissor_taken();
    let frame2 = gpu.present_input_readback(&mut win, &in2);

    // Reaching here == the InvalidScissorRect abort is fixed.
    assert!(
        gpu.scissor_taken() > scissor_before,
        "a single below-fold row change must take the scissor path (exercising the clamp)"
    );
    assert!(
        frame2.width >= 1 && frame2.height >= 1,
        "the clamped present must still produce a valid frame"
    );
    eprintln!(
        "tall {rows}x{cols} grid: bottom-row change scissored with offscreen clamped to \
         {}x{} (requested height {requested_h}px, cell {cw}x{ch}) — no abort",
        frame2.width, frame2.height
    );
}

// `GpuContext::offscreen_texture` is public, so it bounds its own extent:
// `clamp_tex_dim` floors to 1 and caps at `max_texture_dimension_2d`. Two
// different oversized widths landing on one width shows the cap is the device
// limit.
#[test]
fn offscreen_texture_clamps_zero_and_oversized_dims() {
    let ctx = match aterm_gpu::GpuContext::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: no GPU available: {e}");
            return;
        }
    };
    // Reaching each assert == `create_texture` did not abort.
    let dims = |w, h| {
        let t = ctx.offscreen_texture(w, h);
        (t.width(), t.height())
    };
    assert_eq!(dims(0, 4), (1, 4), "width 0 clamps up to 1");
    assert_eq!(dims(6, 0), (6, 1), "height 0 clamps up to 1");
    assert_eq!(dims(0, 0), (1, 1), "0x0 clamps to 1x1");
    let (a, h) = dims(40_000, 4);
    let (b, _) = dims(80_000, 4);
    assert_eq!(h, 4, "an in-range height passes through");
    assert!(
        a < 40_000,
        "width {a} must be clamped below the 40000px request"
    );
    assert_eq!(a, b, "oversized widths must clamp to the SAME device limit");
}
