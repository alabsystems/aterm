// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// REGRESSION: an oversized grid must NOT abort the process.
//
// The control-socket `resize <rows> <cols>` verb is range-checked only to 4096
// rows/cols (aterm-core's MAX_GRID_ROWS/COLS) — NOT to pixels. `apply_term_resize`
// turns that grid into a padded framebuffer of `cols·cell_w + 2·pad` px and sizes
// BOTH the swapchain surface (`resize_surface`) and the offscreen render target
// (`encode_frame` → `GpuContext::offscreen_texture`) from it. wgpu validates every
// texture/surface against `max_texture_dimension_2d` (8192 on many GPUs via
// `Limits::default()`), and because NO `on_uncaptured_error` handler is installed,
// a validation error hits wgpu's DEFAULT handler, which PANICS and aborts the whole
// process. So any grid wide enough that `cols·cell_w + 2·pad > max_texture_dimension_2d`
// used to crash aterm (a DoS reachable straight from the control socket).
//
// The fix clamps the padded framebuffer to `max_texture_dimension_2d` at BOTH sites
// (mirroring the crate's existing atlas/image texture clamps), so an extreme grid
// renders a CLIPPED view instead of crashing.
//
// This drives the OFFSCREEN path (`render_input` → `encode_frame` → `offscreen_texture`;
// the swapchain path needs a real window surface, which isn't creatable headless) with
// two DIFFERENT oversized grids and asserts:
//   * neither call aborts (reaching the assertions == no wgpu-default-handler panic), and
//   * both clamp the offscreen to the SAME readback width — the device limit. The two
//     grids request DIFFERENT framebuffer widths, so without the clamp their readbacks
//     would differ; EQUAL widths prove both were clamped to one fixed device limit.
//
// Gated: no GPU / no system font => the test no-ops (returns), like every other GPU test.

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, PresentCrop, WindowGpu};
use aterm_render::Theme;

/// Render a `rows × cols` blank grid to the offscreen and return the readback width
/// (== the offscreen texture width == the clamped framebuffer width).
fn offscreen_width(gpu: &mut GpuRenderer, win: &mut WindowGpu, rows: usize, cols: usize) -> usize {
    let mut term = Terminal::new(rows as u16, cols as u16);
    let input = term.cell_frame(rows, cols);
    // PRE-FIX: this reached `device.create_texture` with an oversized width and the
    // wgpu default uncaptured-error handler aborted the process here.
    // POST-FIX: it returns a Frame whose width is clamped to the device limit.
    gpu.render_input(win, &input, None).width
}

#[test]
fn oversized_grid_clamps_offscreen_instead_of_aborting() {
    // A LARGE font so a modest column count (well within MAX_GRID_COLS == 4096, so
    // aterm-core doesn't clamp the grid) already blows past even a 32768-limit GPU.
    let mut gpu = match GpuRenderer::new(40.0, Theme::default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let (cw, _ch) = gpu.cell_size();
    assert!(cw >= 1, "cell width must be positive");

    let mut win = WindowGpu::new();

    // Two DIFFERENT column counts whose padded framebuffers BOTH exceed any real
    // `max_texture_dimension_2d` (<= 32768 on current GPUs), while staying within
    // MAX_GRID_COLS (4096) so the GRID itself isn't clamped by aterm-core. Targeting
    // ~50k and ~90k px keeps them distinct for any plausible cell width.
    let rows = 2usize;
    let cols_a = (50_000usize / cw).clamp(2, 4096);
    let cols_b = (90_000usize / cw).clamp(2, 4096);
    let req_a = cols_a * cw; // approx requested framebuffer width (ignoring pad)
    let req_b = cols_b * cw;

    let w_a = offscreen_width(&mut gpu, &mut win, rows, cols_a);
    let w_b = offscreen_width(&mut gpu, &mut win, rows, cols_b);

    // Reaching here already proves the DoS is fixed: pre-fix, render_input aborted the
    // process inside device.create_texture. Both readback widths must be valid and, for
    // two DIFFERENT oversized requests, EQUAL — i.e. both clamped to the one device limit.
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

/// A frontend crop is expressed in LOGICAL raw-frame coordinates. For an
/// oversized grid, the resident offscreen is only the device-clamped top
/// prefix; presentation must normalize a valid logical crop to that prefix
/// instead of rejecting the historically supported clipped grid.
#[test]
fn oversized_tall_grid_normalizes_cropped_virtual_present() {
    let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
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

    // Three rows removed above and two below is a valid odd/asymmetric crop in
    // the LOGICAL source. Its logical end lies far below the resident prefix,
    // so the GPU must trim its height to `resident_h - 3` before the shader.
    let valid = PresentCrop {
        source_y: 3,
        height: logical_h - 5,
    };
    assert!(
        gpu.present_virtual_cropped(
            &mut win,
            &input,
            false,
            None,
            None,
            valid,
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
            !gpu.present_virtual_cropped(
                &mut win,
                &input,
                false,
                None,
                None,
                malformed,
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

// `GpuContext::clear_to_frame` (the proof-of-life probe) is a SECOND crash site: it
// clamps the offscreen TEXTURE via `offscreen_texture`, but the follow-up
// `read_back` (copy_texture_to_buffer) used the ORIGINAL unclamped width/height, so
// an oversized request asked wgpu to copy an extent LARGER than the clamped source
// texture — a validation error that the (missing) uncaptured-error handler turns into
// a process abort. The fix clamps width/height ONCE at the top of `clear_to_frame` so
// BOTH the texture and the copy extent use the bounded dims.
#[test]
fn oversized_clear_to_frame_clamps_instead_of_aborting() {
    let ctx = match aterm_gpu::GpuContext::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: no GPU available: {e}");
            return;
        }
    };
    // Height kept tiny (small readback buffer); width absurdly oversized — far past any
    // real `max_texture_dimension_2d`. Two DIFFERENT oversized widths must clamp to the
    // SAME device limit.
    let a = ctx.clear_to_frame(40_000, 4, 0x00_ff_00);
    let b = ctx.clear_to_frame(80_000, 4, 0x00_ff_00);

    // Reaching here == no abort (pre-fix `read_back` aborted on the copy-larger-than-
    // texture validation error).
    assert!(a.width >= 1, "clamped width must stay valid (>= 1)");
    assert_eq!(a.height, 4, "height (within the limit) must be preserved");
    assert_eq!(
        a.width, b.width,
        "two oversized clear_to_frame requests must clamp width to the SAME device limit"
    );
    assert!(
        a.width < 40_000,
        "clear_to_frame width {} must be clamped below the 40000px request",
        a.width
    );
    // The readback genuinely copied the CLAMPED extent (texture and copy agree): the
    // returned pixel buffer is exactly width*height, which pre-fix would have required
    // copying past the source texture.
    assert_eq!(
        a.pixels.len(),
        a.width * a.height,
        "readback pixel count must match the clamped dims"
    );
    eprintln!(
        "oversized clear_to_frame clamped width to {}px (requested 40000/80000) — no abort",
        a.width
    );
}

// A THIRD, distinct DoS class the texture/surface clamp merely RELOCATED: the per-frame
// instance/vertex streams. `encode_frame` used to iterate the FULL input.rows×input.cols
// and push one BgInstance (12 B) per cell + one GlyphInstance (36 B) per non-blank cell,
// with NO regard to the clamped framebuffer. A densely-filled MAX_GRID 4096×4096 grid
// therefore built ~16.7M GlyphInstances → ~576 MB → next_power_of_two → 1 GiB, far past
// the 256 MiB `max_buffer_size`, so `VertexBuffer::alloc`'s `create_buffer` failed
// validation and the (uninstalled) uncaptured-error handler aborted the process.
//
// The fix bounds the encode loops to the clamped framebuffer (cells whose pixel origin is
// off-screen are skipped — byte-identical, since they were scissored/clipped anyway), so
// the streams are sized by the on-screen cell count, not the grid.
//
// This renders an oversized (width-clamped) grid with EVERY cell filled by a non-space
// glyph (maximal glyph stream) and asserts the total instance count is bounded well below
// the grid cell count. Reaching the assertions == the encode's `create_buffer` did not
// abort. It is deliberately WIDE-and-SHORT so the offscreen readback stays cheap while the
// width clamp (and thus the column loop-clip) is fully exercised. Gated like the others.
#[test]
fn densely_filled_oversized_grid_bounds_instances_no_abort() {
    let mut gpu = match GpuRenderer::new(40.0, Theme::default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let (cw, _ch) = gpu.cell_size();
    let mut win = WindowGpu::new();

    // Oversized in WIDTH: 4096 cols (MAX_GRID_COLS) × a large cell is ~98k px, far past
    // any real max_texture_dimension_2d, so the width clamps and the per-cell column
    // loop-clip engages. Few rows keeps the clamped offscreen (and its readback) small.
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
    // The loop-clip engaged: the streams are bounded by the on-screen area, NOT the grid.
    // Pre-fix `total` was ~2·rows·cols (a bg + a glyph instance per cell) > rows·cols, so
    // this bound FAILS on the unbounded code and PASSES once off-screen cells are skipped.
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

// A FOURTH crash site, on the PRESENT path: the scissored dirty-row repaint's band
// used the RAW dirty-row index for its scissor origin (`y0 = pad + first_dirty * ch`),
// clamping only the band's BOTTOM to the clamped framebuffer height. On an oversized
// (height-clamped) grid — reachable from the control socket, e.g. `resize 2048 4` —
// a change confined to a row BELOW the clamp (the shell prompt lives on the bottom
// row) produced a scissor rect whose y origin lay outside the render target; wgpu
// validates scissor rects against the attachment and, with no uncaptured-error
// handler installed, its default handler PANICS and aborts the process. The fix
// clamps the band's TOP as well, saturating to a zero-height in-bounds rect (the
// changed row is off-screen, so nothing visible changes).
//
// This presents a tall clamped grid, then changes ONLY the bottom row (cursor hidden
// so no on-screen row is marked dirty) and presents again through the REAL scissored
// present path. Reaching the assertions == no abort; the scissor counter proves the
// dirty-row path (not the full-repaint fallback) was exercised.
#[test]
fn oversized_tall_grid_bottom_row_change_scissors_without_abort() {
    let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let (_cw, ch) = gpu.cell_size();
    let mut win = WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    // TALL: 2048 rows × any real cell height blows past every current
    // `max_texture_dimension_2d` (<= 32768), so the offscreen HEIGHT clamps and the
    // bottom rows' pixel origins land beyond it. NARROW keeps the readback cheap.
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

// A FOURTH oversized crash site: the SCISSORED present path. `encode_present_frame`
// clamps the offscreen to `h = clamp_fb_dim(rows*ch + 2*pad)`, but when it builds the
// dirty-row scissor band it clamped only the BOTTOM edge (`y1.min(h)`), not the TOP
// edge `y0 = pad + first_dirty*ch`. On a grid tall enough to clamp, a dirty row whose
// pixel origin lies BELOW the clamped fold gives `y0 > h`, so the scissor rect's `y`
// exceeds `extent.height`; wgpu rejects it (InvalidScissorRect) and — with no
// uncaptured-error handler — the default handler aborts the process. The fix clamps
// BOTH edges to `h`, so an out-of-view band collapses to a degenerate 0-height rect
// (draws nothing) instead of an invalid one.
//
// This drives the readback present path (`present_input_readback` → `encode_present_frame`,
// headless-safe) with a TALL grid: frame 1 paints the bottom row (FULL, first frame);
// frame 2 changes only that bottom row, so the sole dirty row lies far below the fold and
// the scissor band's `y0` is ~rows*ch, thousands of px past the clamped `h`. Reaching the
// post-present assertions == no InvalidScissorRect abort. Gated like the others.
#[test]
fn oversized_tall_grid_below_fold_scissor_clamps_no_abort() {
    let mut gpu = match GpuRenderer::new(40.0, Theme::default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let (cw, ch) = gpu.cell_size();
    assert!(cw >= 1 && ch >= 1, "cell size must be positive");
    let mut win = WindowGpu::new();

    // Tall enough that rows*ch (~100k px at 40pt) exceeds any real
    // max_texture_dimension_2d (<= 32768 on current GPUs), so the offscreen HEIGHT
    // clamps and the bottom rows fall below the fold. Narrow so the readback stays
    // cheap. Within MAX_GRID_ROWS (4096) so aterm-core doesn't clamp the grid.
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

    // The offscreen HEIGHT must have clamped below the requested framebuffer — that
    // is the precondition for a below-fold dirty row. If some GPU has an enormous
    // limit that didn't clamp, the scissor bug can't manifest; skip cleanly.
    let requested_h = rows * ch;
    if frame1.height >= requested_h {
        eprintln!(
            "SKIP: offscreen height {} not clamped below requested {requested_h}px \
             (max_texture_dimension_2d too large to exercise the below-fold scissor)",
            frame1.height
        );
        return;
    }

    // Frame 2: change ONLY the bottom row ('a' -> 'ab'); cursor stays on it. The sole
    // dirty row is far below the fold, so `y0 = pad + (rows-1)*ch` is thousands of px
    // past the clamped `h`. PRE-FIX: the unclamped scissor rect aborts the process
    // here. POST-FIX: `y0` clamps to `h`, the band is degenerate, no abort.
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

// FIX A: `clamp_tex_dim` had no LOWER floor (unlike `clamp_fb_dim`), so a public call
// with a ZERO dimension — `GpuContext::clear_to_frame(0, h, ..)` or `(w, 0, ..)` — reached
// `create_texture` with a zero extent. A 0-dimension texture is ALWAYS invalid, so wgpu's
// validation fired and (with no uncaptured-error handler) the default handler aborted the
// process — the same crash class as the oversized cases. The `.max(1)` floor now clamps a
// zero UP to a valid 1×N (or N×1) texture. Reaching the asserts == no abort.
#[test]
fn zero_dim_clear_to_frame_clamps_to_valid_no_abort() {
    let ctx = match aterm_gpu::GpuContext::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: no GPU available: {e}");
            return;
        }
    };
    // Width 0 → clamps up to 1; the in-range height passes through unchanged.
    let a = ctx.clear_to_frame(0, 4, 0x00_00_ff);
    assert_eq!(a.width, 1, "width 0 must clamp up to a valid 1");
    assert_eq!(a.height, 4, "an in-range height passes through");
    assert_eq!(
        a.pixels.len(),
        a.width * a.height,
        "readback pixel count must match the clamped dims"
    );
    // Height 0 → clamps up to 1.
    let b = ctx.clear_to_frame(6, 0, 0x00_00_ff);
    assert_eq!(b.height, 1, "height 0 must clamp up to a valid 1");
    assert_eq!(b.width, 6, "an in-range width passes through");
    // Both zero → 1×1.
    let c = ctx.clear_to_frame(0, 0, 0x00_00_ff);
    assert!(
        c.width >= 1 && c.height >= 1,
        "a 0×0 request must clamp to a valid ≥1 texture"
    );
    eprintln!("clear_to_frame(0,4)/(6,0)/(0,0) clamped to valid dims — no abort");
}
