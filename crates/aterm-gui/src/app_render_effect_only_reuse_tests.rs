// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The effect-only reuse gate fires on a pure animation tick over an untouched engine and
//! refuses every way the snapshot can go stale, including parser batches that mark no grid
//! damage and host selection changes.

use aterm_core::render::RenderInput;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

const ROWS: usize = 6;
const COLS: usize = 20;

/// A terminal with settled output, and a scratch freshly filled from it —
/// the state every presented frame leaves behind.
fn settled() -> (Terminal, RenderInput) {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"hello world");
    let mut scratch = RenderInput::empty();
    let _ = term.cell_frame_damage_scoped_into(&mut scratch, ROWS, COLS);
    (term, scratch)
}

fn reusable(term: &mut Terminal, scratch: &RenderInput) -> bool {
    let epoch = term.damage_epoch();
    super::effect_only_snapshot_reusable(true, scratch, term, epoch, ROWS, COLS)
}

#[test]
fn an_untouched_engine_lets_the_effect_tick_present_the_snapshot_it_already_has() {
    let (mut term, scratch) = settled();
    assert!(
        reusable(&mut term, &scratch),
        "a settled engine + a scratch it just filled is the whole point of the gate"
    );
    // An effect frame only ticks the clock and never consults the engine, so reuse must chain
    // across consecutive frames.
    assert!(
        reusable(&mut term, &scratch) && reusable(&mut term, &scratch),
        "reuse must chain across consecutive effect-only frames"
    );
}

/// Every way the snapshot can go stale, one row per clause: each row starts
/// from a settled engine the gate reuses (asserted), applies one
/// perturbation, and requires a refusal.
#[test]
fn every_way_the_snapshot_goes_stale_refuses() {
    type Perturb = fn(&mut Terminal, &mut RenderInput) -> bool;
    let rows: [(&str, Perturb); 8] = [
        (
            // A try_lock that had to wait is evidence a writer is mid-batch.
            "contention alone",
            |term, scratch| {
                let epoch = term.damage_epoch();
                super::effect_only_snapshot_reusable(false, scratch, term, epoch, ROWS, COLS)
            },
        ),
        (
            // OSC 0 retitles the window: no cell change, no grid damage, so the damage epoch
            // cannot move and only the `process_sequence` clause sees it (as it does a CUP, a
            // DECTCEM hide or an OSC 11 recolour).
            "a parser batch that marks no damage",
            |term, scratch| {
                term.process(b"\x1b]0;retitled\x07");
                assert!(
                    !term.has_damage(),
                    "negative-control precondition: this batch marks no grid damage"
                );
                reusable(term, scratch)
            },
        ),
        (
            // `text_selection_mut` is a bare accessor: a mouse drag marks no
            // damage and runs no batch, yet `cell_frame_fill` stamps the
            // selection into every snapshot and the renderer paints from it.
            "a host selection change",
            |term, scratch| {
                term.text_selection_mut().start_selection(
                    0,
                    2,
                    SelectionSide::Left,
                    SelectionType::Simple,
                );
                term.text_selection_mut()
                    .update_selection(0, 6, SelectionSide::Right);
                term.text_selection_mut().complete_selection();
                assert!(
                    !term.has_damage(),
                    "negative-control precondition: a selection marks no grid damage"
                );
                reusable(term, scratch)
            },
        ),
        (
            // Stream fade, IME preedit and prediction ghosts bump `snapshot_seq` after writing
            // cells; reusing such a scratch would tint already-tinted cells.
            "a host cell mutation since the fill",
            |term, scratch| {
                scratch.snapshot_seq = scratch.snapshot_seq.wrapping_add(1);
                reusable(term, scratch)
            },
        ),
        ("real output", |term, scratch| {
            term.process(b"x");
            reusable(term, scratch)
        }),
        (
            "a row count the snapshot was not filled at",
            |term, scratch| {
                let epoch = term.damage_epoch();
                super::effect_only_snapshot_reusable(true, scratch, term, epoch, ROWS + 1, COLS)
            },
        ),
        (
            "a column count the snapshot was not filled at",
            |term, scratch| {
                let epoch = term.damage_epoch();
                super::effect_only_snapshot_reusable(true, scratch, term, epoch, ROWS, COLS + 1)
            },
        ),
        (
            // Per-terminal `damage_epoch` values collide numerically (both count
            // from zero); the identity nonce is what cannot. A tab switch hands
            // the window a different engine with the same scratch.
            "a snapshot from another terminal",
            |_term, scratch| {
                let (mut other, _) = settled();
                reusable(&mut other, scratch)
            },
        ),
    ];
    for (label, perturb) in rows {
        let (mut term, mut scratch) = settled();
        assert!(reusable(&mut term, &scratch), "{label}: precondition");
        assert!(
            !perturb(&mut term, &mut scratch),
            "{label}: a stale snapshot must never be presented"
        );
    }
}

/// A reader parked in history gets reuse for effect ticks (blinks, pill
/// fades, glide band frames) like one at the live bottom; the wheel step
/// that moves the viewport is refused, the following refill re-arms reuse,
/// and a scratch whose offset differs from the engine's is refused.
#[test]
fn a_viewport_parked_in_history_reuses_like_the_live_bottom() {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    for i in 0..16 {
        term.process(format!("line {i}\r\n").as_bytes());
    }
    term.scroll_display(2);
    let mut scratch = RenderInput::empty();
    let _ = term.cell_frame_damage_scoped_into(&mut scratch, ROWS, COLS);
    assert_eq!(
        scratch.display_offset, 2,
        "precondition: the scratch was filled while parked in history"
    );
    assert!(
        reusable(&mut term, &scratch) && reusable(&mut term, &scratch),
        "an untouched engine parked in history is the blink/pill frame this gate is for, \
         and reuse must chain"
    );
    term.scroll_display(1);
    assert!(
        !reusable(&mut term, &scratch),
        "a wheel step damages the grid; the strip must be extracted"
    );
    let _ = term.cell_frame_damage_scoped_into(&mut scratch, ROWS, COLS);
    assert!(
        reusable(&mut term, &scratch),
        "the refill that follows the step re-arms the gate at the new offset"
    );
    scratch.display_offset += 1;
    assert!(
        !reusable(&mut term, &scratch),
        "a scratch from one offset over an engine at another is a different viewport"
    );
}

/// The shipping redraw path over a quiet scrollback viewport whose DEC cursor
/// is in the incoming row, with the strip off so effect-only reuse is eligible.
fn app_with_apron_cursor() -> crate::App {
    use crate::scripted_redraw_fixture::{feed, scripted_app};
    let mut app = scripted_app();
    let wid = crate::WindowId(0);
    let rows = usize::from(app.windows[&wid].rows);
    assert_eq!(app.tab_strip_rows, 0, "exercise the effect-only reuse lane");
    for row in 0..rows + 4 {
        feed(&app, format!("row {row}\r\n").as_bytes());
    }
    feed(&app, b"caret");
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    crate::term_lock(&terminal).scroll_display(1);
    app.redraw_window(wid);
    let input = &app.windows[&wid].input_scratch;
    assert!(
        input.apron_row.present,
        "the incoming row was actually filled"
    );
    assert_eq!(input.apron_row.cursor_col, Some(5));
    app
}

#[test]
fn ordinary_app_frames_keep_reusing_the_unchanged_apron_cursor() {
    let mut app = app_with_apron_cursor();
    let wid = crate::WindowId(0);
    let generation = app.windows[&wid].input_scratch.extract_gen;
    for _ in 0..3 {
        app.redraw_window(wid);
        let input = &app.windows[&wid].input_scratch;
        assert_eq!(input.extract_gen, generation, "ordinary reuse still chains");
        assert_eq!(input.apron_row.cursor_col, Some(5));
    }
}

#[test]
fn closing_a_real_modal_restores_the_idle_apron_cursor() {
    let mut app = app_with_apron_cursor();
    let wid = crate::WindowId(0);
    app.palette_enter();
    app.redraw_window(wid);
    assert!(
        app.windows[&wid].settings_card.is_some(),
        "actual modal painted"
    );
    assert_eq!(app.windows[&wid].input_scratch.apron_row.cursor_col, None);

    app.palette_exit();
    app.redraw_window(wid);
    let input = &app.windows[&wid].input_scratch;
    assert!(
        app.windows[&wid].settings_card.is_none(),
        "actual modal closed"
    );
    assert_eq!(
        input.apron_row.cursor_col,
        Some(5),
        "closing a modal must recover the idle engine's incoming-row caret"
    );
}

#[test]
fn leaving_real_copy_mode_restores_the_idle_apron_cursor() {
    let mut app = app_with_apron_cursor();
    let wid = crate::WindowId(0);
    app.toggle_vi_mode(wid);
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut term = crate::term_lock(&terminal);
        // Move visibly inside the viewport rather than leave the copy caret
        // on the engine cursor's same numeric row after the scroll offset.
        term.vi_motion(aterm_core::ViMotion::Up, aterm_core::ViBoundary::Grid);
        term.vi_motion(aterm_core::ViMotion::Up, aterm_core::ViBoundary::Grid);
    }
    app.redraw_window(wid);
    assert!(
        app.windows[&wid].input_scratch.cursor_visible,
        "the copy-mode caret painted"
    );
    assert_eq!(app.windows[&wid].input_scratch.apron_row.cursor_col, None);

    app.toggle_vi_mode(wid);
    app.redraw_window(wid);
    assert!(
        !crate::term_lock(&terminal).vi_is_active(),
        "actual copy mode closed"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.apron_row.cursor_col,
        Some(5),
        "leaving copy mode must recover the idle engine's incoming-row caret"
    );
}
