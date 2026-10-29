// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that grids exceeding `max_texture_dimension_2d` clamp the offscreen framebuffer without aborting.

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, PresentCrop, WindowGpu};
use aterm_render::Theme;

use crate::common;

/// Render a `rows × cols` blank grid and return the clamped offscreen width.
fn offscreen_width(gpu: &mut GpuRenderer, win: &mut WindowGpu, rows: usize, cols: usize) -> usize {
    let mut term = Terminal::new(rows as u16, cols as u16);
    let input = term.cell_frame(rows, cols);
    gpu.render_input(win, &input, None).width
}

#[test]
fn oversized_grid_clamps_offscreen_instead_of_aborting() {
    let mut gpu = common::gpu(40.0, Theme::default());
    let (cw, _ch) = gpu.cell_size();
    assert!(cw >= 1, "cell width must be positive");
    let max = gpu.max_texture_dim() as usize;

    let rows = 2usize;
    let cols_a = (max / cw).saturating_add(1).min(4096);
    let cols_b = cols_a.saturating_add(256).min(4096);
    let (req_a, _) = gpu.frame_size(rows, cols_a);
    let (req_b, _) = gpu.frame_size(rows, cols_b);
    if req_a <= max || cols_a == cols_b {
        aterm_spec::host::could_not_run(format!(
            "device texture limit {max}px at cell_w={cw}px leaves no two distinct oversized \
             grids under MAX_GRID_COLS=4096 (cols_a={cols_a}, cols_b={cols_b}, req_a={req_a}px)"
        ));
    }

    let mut win = WindowGpu::new();
    let w_a = offscreen_width(&mut gpu, &mut win, rows, cols_a);
    let w_b = offscreen_width(&mut gpu, &mut win, rows, cols_b);

    assert_eq!(
        w_a, max,
        "oversized grid A (requested {req_a}px) must clamp offscreen width to \
         max_texture_dimension_2d ({max}px)"
    );
    assert_eq!(
        w_b, max,
        "oversized grid B (requested {req_b}px) must clamp offscreen width to \
         max_texture_dimension_2d ({max}px)"
    );
    assert!(
        w_a < req_a && req_a < req_b,
        "distinct oversized grids ({req_a}px < {req_b}px) must both strictly exceed \
         clamped width {w_a}px"
    );
    eprintln!(
        "oversized grid clamped offscreen to {w_a}px (requested {req_a}px / {req_b}px, cell_w={cw}) — no abort"
    );
}

/// Logical raw-frame crops are normalized against the clamped resident offscreen prefix.
#[test]
fn oversized_tall_grid_normalizes_cropped_virtual_present() {
    let mut gpu = common::gpu(18.0, Theme::default());
    let (rows, cols) = (2048usize, 4usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let input = term.cell_frame(rows, cols);
    let (logical_w, logical_h) = gpu.frame_size(rows, cols);
    let logical_h = u32::try_from(logical_h).expect("fixture fits u32");

    let mut win = WindowGpu::new();
    let resident = gpu.render_input(&mut win, &input, None);
    let resident_h = u32::try_from(resident.height).expect("GPU extent fits u32");
    let resident_w = u32::try_from(resident.width).expect("GPU extent fits u32");
    let max = gpu.max_texture_dim();
    if logical_h <= max {
        aterm_spec::host::could_not_run(format!(
            "this device's texture limit {max}px holds the {logical_h}px frame"
        ));
    }
    assert_eq!(
        resident_h, max,
        "the resident offscreen clamps to the device limit"
    );

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

    for malformed in [
        PresentCrop {
            source_y: 0,
            height: 0,
        },
        PresentCrop {
            source_y: logical_h - 1,
            height: 2,
        },
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

#[test]
fn densely_filled_oversized_grid_bounds_instances_no_abort() {
    let mut gpu = common::gpu(40.0, Theme::default());
    let (cw, _ch) = gpu.cell_size();
    let mut win = WindowGpu::new();

    let rows = 8usize;
    let cols = 4096usize;
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(&vec![b'X'; rows * cols]);
    let input = term.cell_frame(rows, cols);

    let frame = gpu.render_input(&mut win, &input, None);
    let total = gpu.last_instances();

    assert!(total >= 1, "a densely-filled grid must emit some instances");
    assert!(
        total < rows * cols,
        "instance streams must be bounded by the clamped framebuffer, not the grid: \
         got {total} instances for a {rows}x{cols} grid (cell_w={cw})"
    );
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

#[test]
fn oversized_tall_grid_bottom_row_change_scissors_without_abort() {
    let mut gpu = common::gpu(18.0, Theme::default());
    let (_cw, ch) = gpu.cell_size();
    let mut win = WindowGpu::new();
    gpu.set_cursor_blink_phase(true);
    gpu.set_cursor_style_override(None);

    let (rows, cols) = (2048usize, 4usize);
    let mut term = Terminal::new(rows as u16, cols as u16);

    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false; // keep the (row-0) cursor from dirtying an on-screen row
    let frame = gpu.present_input_readback(&mut win, &input);
    assert!(
        frame.height < rows * ch,
        "grid height {}px must have been clamped (got {}px) for this regression to bite",
        rows * ch,
        frame.height
    );

    term.process(format!("\x1b[{rows};1HZ").as_bytes());
    let mut input2 = term.cell_frame(rows, cols);
    input2.cursor_visible = false;
    let scissor_before = gpu.scissor_taken();
    let frame2 = gpu.present_input_readback(&mut win, &input2);

    assert!(
        gpu.scissor_taken() > scissor_before,
        "a bottom-row-only change on a reusable frame must take the scissor path"
    );
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
