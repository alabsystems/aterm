// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Damage-path differential: `Renderer::render_input_cached` over a persistent `WindowCpu`
// (the shipping CPU present's entry) must be byte-identical to a fresh renderer's full
// repaint after every step of a long mutation walk (typing, cursor moves, SGR, wide CJK,
// combining marks, DECDWL, blink phase, cursor override, selection, scrollback offset,
// resize). `render_input` repaints fully into a throwaway cache, so the warm arm must not
// use it; each test pins via the `DamageOutcome` tally that the row-scoped path, the dirty
// gate and the full fallback actually ran.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_render::{DamageOutcome, Frame, Renderer, Theme, WindowCpu};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .map(|mut r| {
            // Block on lazy fallback parses so both arms rasterize the CJK step
            // identically (no provisional `.notdef`).
            r.debug_block_on_lazy_fallbacks();
            r
        })
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// The warm arm: renderer plus its persistent `WindowCpu`, and a tally of the
/// [`DamageOutcome`]s taken so each test can pin that the fast paths ran.
struct DamageRig {
    r: Renderer,
    wc: WindowCpu,
    full_frames: usize,
    gate_frames: usize,
    rows_frames: usize,
    scroll_frames: usize,
}

fn rig() -> DamageRig {
    DamageRig {
        r: renderer(),
        wc: WindowCpu::new(),
        full_frames: 0,
        gate_frames: 0,
        rows_frames: 0,
        scroll_frames: 0,
    }
}

/// Renderer-side state applied to both arms before each render: blink phase and the
/// cursor-style override are not in `RenderInput`.
#[derive(Clone, Copy)]
struct RState {
    blink_phase: bool,
    cursor_override: Option<CursorStyle>,
}

impl Default for RState {
    fn default() -> Self {
        RState {
            blink_phase: true,
            cursor_override: None,
        }
    }
}

/// Render `term` through the warm rig and a brand-new renderer under identical state and
/// assert byte equality; tally the warm arm's [`DamageOutcome`].
fn assert_identical(
    damaged: &mut DamageRig,
    rows: usize,
    cols: usize,
    term: &mut Terminal,
    st: RState,
    label: &str,
) {
    let input = term.cell_frame(rows, cols);

    damaged.r.set_cursor_blink_phase(st.blink_phase);
    damaged.r.set_cursor_style_override(st.cursor_override);
    // The view is copied into an owned `Frame` so the `wc` borrow ends before the
    // outcome tally reads it.
    let dmg: Frame = {
        let view = damaged.r.render_input_cached(&mut damaged.wc, &input);
        Frame {
            width: view.width(),
            height: view.height(),
            pixels: view.pixels().to_vec(),
        }
    };
    match damaged.wc.last_damage() {
        DamageOutcome::Full => damaged.full_frames += 1,
        DamageOutcome::GateHit => damaged.gate_frames += 1,
        DamageOutcome::Rows => damaged.rows_frames += 1,
        DamageOutcome::Scroll { .. } => damaged.scroll_frames += 1,
    }

    let mut fresh = renderer();
    fresh.set_cursor_blink_phase(st.blink_phase);
    fresh.set_cursor_style_override(st.cursor_override);
    let full: Frame = fresh.render_input(&input);

    assert_eq!(dmg.width, full.width, "width mismatch @ {label}");
    assert_eq!(dmg.height, full.height, "height mismatch @ {label}");
    assert_eq!(
        dmg.pixels.len(),
        full.pixels.len(),
        "pixel-count mismatch @ {label}"
    );

    if dmg.pixels != full.pixels {
        let mut first = None;
        for (i, (&a, &b)) in dmg.pixels.iter().zip(full.pixels.iter()).enumerate() {
            if a != b {
                let (x, y) = (i % dmg.width, i / dmg.width);
                first = Some((i, x, y, a, b));
                break;
            }
        }
        let n_diff = dmg
            .pixels
            .iter()
            .zip(full.pixels.iter())
            .filter(|(a, b)| a != b)
            .count();
        panic!(
            "DAMAGE != FULL @ {label}: {n_diff} differing pixels; first {first:?} \
             (index, x, y, damaged, full)"
        );
    }
}

/// One end-to-end differential walk.
#[test]
fn damage_path_is_byte_identical_to_full_repaint() {
    let mut dmg = rig();

    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let st = RState::default();

    // The first frame takes the full path and warms the cache.
    assert_identical(&mut dmg, rows, cols, &mut term, st, "blank");

    // Typing: each keystroke is a 1-cell change on one row.
    for (i, ch) in "hello world".bytes().enumerate() {
        term.process(&[ch]);
        assert_identical(&mut dmg, rows, cols, &mut term, st, &format!("type[{i}]"));
    }

    term.process(b"\x08\x08X"); // two BS, overwrite the 'l' of "world"
    assert_identical(&mut dmg, rows, cols, &mut term, st, "backspace+overwrite");

    term.process(b"\r\nsecond line");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "second row");

    // Cursor moves without a content change.
    term.process(b"\x1b[1;1H"); // home
    assert_identical(&mut dmg, rows, cols, &mut term, st, "cursor home");
    term.process(b"\x1b[3;5H"); // row 3 col 5
    assert_identical(&mut dmg, rows, cols, &mut term, st, "cursor move r3c5");

    term.process(b"\x1b[31;42mRED-ON-GREEN\x1b[0m");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "sgr colour");

    term.process(b"\x1b[1;3;4mBI\x1b[0m\x1b[9mS\x1b[0m\x1b[53mO\x1b[0m");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "decorations");

    term.process(b"\x1b[5;1H");
    term.process("日本語".as_bytes());
    assert_identical(&mut dmg, rows, cols, &mut term, st, "wide cjk");

    term.process(b"\x1b[5;10H");
    term.process("e\u{0301}".as_bytes());
    assert_identical(&mut dmg, rows, cols, &mut term, st, "combining é");

    term.process(b"\x1b[6;1H");
    term.process(b"\x1b#6DOUBLEWIDE");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "decdwl");

    // Blink phase changes no content and matters only for a Blinking* cursor.
    term.process(b"\x1b[1 q"); // DECSCUSR 1 = blinking block
    term.process(b"\x1b[1;1H");
    let st_off = RState {
        blink_phase: false,
        ..st
    };
    assert_identical(&mut dmg, rows, cols, &mut term, st_off, "blink off");
    let st_on = RState {
        blink_phase: true,
        ..st
    };
    assert_identical(&mut dmg, rows, cols, &mut term, st_on, "blink on");
    // Toggle again from the warm cache to exercise the gate's phase tracking.
    assert_identical(&mut dmg, rows, cols, &mut term, st_off, "blink off 2");

    // No change at all: the dirty-gate fast return.
    assert_identical(&mut dmg, rows, cols, &mut term, st_on, "no-op gate");
    assert_identical(&mut dmg, rows, cols, &mut term, st_on, "no-op gate 2");

    // Cursor-style override (the frontend forces HollowBlock while unfocused).
    let st_hollow = RState {
        cursor_override: Some(CursorStyle::HollowBlock),
        ..st_on
    };
    assert_identical(
        &mut dmg,
        rows,
        cols,
        &mut term,
        st_hollow,
        "override hollow",
    );
    assert_identical(&mut dmg, rows, cols, &mut term, st_on, "override cleared");

    // Steady block over a glyph: the block cut-out path.
    term.process(b"\x1b[2 q\x1b[1;1H"); // steady block at home, over the 'h'
    assert_identical(
        &mut dmg,
        rows,
        cols,
        &mut term,
        st,
        "steady block over glyph",
    );

    term.process(b"\x1b[?25l"); // hide
    assert_identical(&mut dmg, rows, cols, &mut term, st, "cursor hidden");
    term.process(b"\x1b[?25h"); // show
    assert_identical(&mut dmg, rows, cols, &mut term, st, "cursor shown");

    // Selection is frame-global: full fallback.
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 2, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    assert_identical(&mut dmg, rows, cols, &mut term, st, "selection set");
    {
        let sel = term.text_selection_mut();
        sel.extend_selection(1, 4, SelectionSide::Right); // extend onto row 1
    }
    assert_identical(&mut dmg, rows, cols, &mut term, st, "selection extend");
    {
        let sel = term.text_selection_mut();
        sel.clear();
    }
    assert_identical(&mut dmg, rows, cols, &mut term, st, "selection clear");

    // A display-offset change is frame-global: full fallback. Generate history first.
    for i in 0..20 {
        term.process(format!("\r\nscrollback row {i}").as_bytes());
    }
    assert_identical(&mut dmg, rows, cols, &mut term, st, "after scroll content");
    term.scroll_display(5); // scroll up into history
    assert_identical(&mut dmg, rows, cols, &mut term, st, "display_offset=5");
    term.scroll_display(-2);
    assert_identical(&mut dmg, rows, cols, &mut term, st, "display_offset=3");
    term.scroll_to_bottom();
    assert_identical(&mut dmg, rows, cols, &mut term, st, "display_offset=0");

    term.process(b"Z");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "1-cell after scroll");

    // Resize changes dims: full fallback and cache rebuild.
    let (rows2, cols2) = (8usize, 30usize);
    term.resize(rows2 as u16, cols2 as u16);
    term.process(b"after resize");
    assert_identical(&mut dmg, rows2, cols2, &mut term, st, "after resize");
    // A 1-cell change at the new size takes the rebuilt damage path.
    term.process(b"!");
    assert_identical(&mut dmg, rows2, cols2, &mut term, st, "1-cell after resize");

    term.resize(rows as u16, cols as u16);
    assert_identical(&mut dmg, rows, cols, &mut term, st, "shrink resize");
    term.process(b"Q");
    assert_identical(&mut dmg, rows, cols, &mut term, st, "1-cell after shrink");

    // Non-vacuity: the walk exercised every damage outcome class.
    assert!(
        dmg.rows_frames > 0,
        "warm arm never took the row-scoped damage path — the differential is vacuous"
    );
    assert!(
        dmg.gate_frames > 0,
        "warm arm never dirty-gate-hit (the no-op steps must gate) — the differential is vacuous"
    );
    assert!(
        dmg.full_frames > 0,
        "warm arm never full-repainted (first frame + resizes must) — the fallback lost coverage"
    );
    eprintln!(
        "damage outcomes over the walk: rows {} | gate {} | full {} | scroll {}",
        dmg.rows_frames, dmg.gate_frames, dmg.full_frames, dmg.scroll_frames
    );
}

/// The single-cell warm path: byte-identity at every keystroke once the cache is warm.
#[test]
fn warm_single_cell_typing_is_identical() {
    let mut dmg = rig();
    let (rows, cols) = (4usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let st = RState::default();

    assert_identical(&mut dmg, rows, cols, &mut term, st, "warm:blank");
    let text = "The quick brown fox jumps over the lazy dog 0123";
    let mut typed = 0usize;
    for (i, ch) in text.bytes().enumerate() {
        if i >= cols {
            break;
        }
        term.process(&[ch]);
        typed += 1;
        assert_identical(
            &mut dmg,
            rows,
            cols,
            &mut term,
            st,
            &format!("warm:type[{i}]"),
        );
    }

    // Only the priming frame repaints fully; every keystroke takes the row-scoped path.
    assert_eq!(
        dmg.full_frames, 1,
        "warm typing: only the cache-priming first frame may full-repaint"
    );
    assert_eq!(
        dmg.gate_frames, 0,
        "warm typing: a keystroke frame dirty-gate-hit — its glyph never reached the diff"
    );
    assert_eq!(
        dmg.rows_frames, typed,
        "warm typing: every keystroke must take the row-scoped damage path"
    );
}
