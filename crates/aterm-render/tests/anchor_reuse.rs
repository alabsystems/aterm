// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Damage precheck reuse arm tests across streaming, flood, selection, reflow, image, and fractional scroll frames.

use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, DirtyDecision, Renderer, Theme, WindowCpu, compute_dirty_rows};

fn renderer() -> Renderer {
    Renderer::from_system(16.0, Theme::default())
        .map(|mut r| {
            r.debug_block_on_lazy_fallbacks();
            r
        })
        .expect("no font: build with aterm-render's default embedded-font feature")
}

fn render_both(
    warm: &mut Renderer,
    wc: &mut WindowCpu,
    term: &mut Terminal,
    rows: usize,
    cols: usize,
    label: &str,
) -> DamageOutcome {
    let input = term.cell_frame(rows, cols);
    let (pixels, w, h) = {
        let view = warm.render_input_cached(wc, &input);
        (view.pixels().to_vec(), view.width(), view.height())
    };
    let outcome = wc.last_damage();
    let mut fresh = renderer();
    let full = fresh.render_input(&input);
    assert_eq!((w, h), (full.width, full.height), "dims @ {label}");
    assert_eq!(pixels, full.pixels, "warm != full repaint @ {label}");
    outcome
}

#[test]
fn streaming_while_scrolled_gate_hits_on_the_anchor() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (6usize, 32usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..60 {
        term.process(format!("history line {i}\r\n").as_bytes());
    }
    term.scroll_display(20);
    render_both(
        &mut warm,
        &mut wc,
        &mut term,
        rows,
        cols,
        "scrolled warm-up",
    );
    let offset_before = term.grid().display_offset();

    for burst in 0..3 {
        term.process(format!("streamed {burst}\r\n").as_bytes());
        assert!(
            term.grid().display_offset() > offset_before + burst,
            "precondition: the viewport re-pins while reading history"
        );
        let outcome = render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("repin[{burst}]"),
        );
        assert_eq!(
            outcome,
            DamageOutcome::GateHit,
            "an anchor-stable, content-identical repin frame must be zero work"
        );
    }
}

#[test]
fn uniform_bottom_flood_gate_hits_on_equal_offsets() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (6usize, 32usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for _ in 0..30 {
        term.process(b"flood line\r\n");
    }
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "flood warm-up");
    let base_before = term.cell_frame(rows, cols).base_y;

    for burst in 0..3i64 {
        term.process(b"flood line\r\n");
        let frame = term.cell_frame(rows, cols);
        assert_eq!(
            frame.display_offset, 0,
            "a bottom-pinned flood never scrolls"
        );
        assert!(
            frame.base_y > base_before + burst,
            "precondition: each flood line advances base_y"
        );
        let outcome = render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("flood[{burst}]"),
        );
        assert_eq!(
            outcome,
            DamageOutcome::GateHit,
            "a uniform bottom-flood frame is pixel-identical and must be zero work"
        );
    }
}

#[test]
fn selection_and_cursor_survive_anchor_reuse() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (6usize, 32usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..40 {
        term.process(format!("selectable {i}\r\n").as_bytes());
    }
    term.scroll_display(10);
    {
        use aterm_core::selection::{SelectionSide, SelectionType};
        let sel = term.text_selection_mut();
        sel.start_selection(-8, 2, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(-8, 9, SelectionSide::Right);
        sel.complete_selection();
    }
    render_both(
        &mut warm,
        &mut wc,
        &mut term,
        rows,
        cols,
        "selection warm-up",
    );

    for burst in 0..3 {
        term.process(format!("sel stream {burst}\r\n").as_bytes());
        let outcome = render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("selection repin[{burst}]"),
        );
        assert_ne!(
            outcome,
            DamageOutcome::Full,
            "anchor-equal selection frames ride the row-diff path"
        );
    }

    term.process(b"\x1b[2;3H");
    let outcome = render_both(&mut warm, &mut wc, &mut term, rows, cols, "cursor move");
    assert_ne!(outcome, DamageOutcome::Full, "cursor move is row damage");
}

#[test]
fn reflow_forces_full_and_stays_byte_exact() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (6usize, 32usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..30 {
        term.process(format!("a wrapping reflow probe line {i}\r\n").as_bytes());
    }
    term.scroll_display(8);
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "pre-reflow");
    term.resize(rows as u16, 20);
    let outcome = render_both(&mut warm, &mut wc, &mut term, rows, 20, "post-reflow");
    assert_eq!(
        outcome,
        DamageOutcome::Full,
        "a rewrap can never reuse rows across the resize"
    );
}

#[test]
fn image_lane_change_dirties_its_row_under_anchor_shift() {
    let (rows, cols) = (4usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..20 {
        term.process(format!("img {i}\r\n").as_bytes());
    }
    term.scroll_display(5);
    let prev = term.cell_frame(rows, cols);

    let mut next = prev.clone();
    next.display_offset += 1;
    next.base_y += 1;
    next.images[2].push((
        1, // column
        aterm_core::grid::extra::ImageRef {
            image: std::sync::Arc::new(aterm_core::grid::extra::ImageData {
                bytes: vec![0u8; 4],
                format: aterm_core::grid::extra::ImageFormat::RawRgba8 {
                    width: 1,
                    height: 1,
                },
                cols: 1,
                rows: 1,
                z_index: 0,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: aterm_core::grid::extra::ImageScaling::Fit,
                source_rect: None,
            }),
            cell_row: 0,
            cell_col: 0,
            kitty: None,
        },
    ));

    let mut dirty = Vec::new();
    let decision = compute_dirty_rows(
        &prev,
        &next,
        true,
        None,
        true,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    );
    match decision {
        DirtyDecision::FullRepaint => panic!("anchor-equal frames must not full-repaint"),
        DirtyDecision::Rows(_) => {
            assert!(dirty[2], "the image row repaints");
            assert!(!dirty[1], "unchanged rows stay clean");
        }
    }

    let mut skewed = prev.clone();
    skewed.display_offset += 1; // base_y unchanged → anchor moved
    let decision = compute_dirty_rows(
        &prev,
        &skewed,
        true,
        None,
        true,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    );
    assert!(
        matches!(decision, DirtyDecision::FullRepaint),
        "a real viewport move still takes the full path"
    );
}

#[test]
fn fractional_present_over_anchor_reuse_is_byte_exact() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (6usize, 32usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..50 {
        term.process(format!("frac reuse {i}\r\n").as_bytes());
    }
    term.scroll_display(12);
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "frac warm-up");
    term.process(b"frac stream\r\n"); // repin: anchor holds

    let mut input = term.cell_frame(rows, cols);
    input.scroll_frac_px = -5;
    input.grid_top_row = 0;
    input.grid_bot_row = rows;
    let warm_pixels = {
        let view = warm.render_input_cached(&mut wc, &input);
        view.pixels().to_vec()
    };
    assert_ne!(
        wc.last_damage(),
        DamageOutcome::Full,
        "the frac present must not defeat the anchor reuse"
    );
    let mut fresh = renderer();
    let mut fresh_wc = WindowCpu::new();
    let fresh_pixels = {
        let view = fresh.render_input_cached(&mut fresh_wc, &input);
        view.pixels().to_vec()
    };
    assert_eq!(
        warm_pixels, fresh_pixels,
        "translated present over a reused cache is byte-exact"
    );
}
