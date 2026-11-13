// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    paint_prediction_chain, paint_prediction_ghosts, paint_prediction_overlay, paint_predictions,
    terminal_blank_cell,
};
use crate::predict::{GhostCell, Prediction};
use aterm_core::terminal::{RenderCell, Terminal};
use aterm_render::RenderInput;

fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [200, 200, 200],
        bg: [0, 0, 0],
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: aterm_core::terminal::UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
        overline_color: None,
        script: aterm_core::terminal::Script::Normal,
    }
}

fn scratch(rows: usize) -> RenderInput {
    let mut s = RenderInput::empty();
    s.cells = vec![Vec::new(); rows];
    s
}

#[test]
fn ghost_extends_row_and_dims() {
    let mut s = scratch(2);
    let preds = [Prediction::test_at(1, 3, 'x')];
    assert_eq!(
        paint_prediction_ghosts(&mut s, &preds, 10, blank()).newest,
        Some((1, 3))
    );
    assert_eq!(s.cells[1].len(), 4, "row extended to hold the ghost");
    let c = &s.cells[1][3];
    assert_eq!(c.ch, 'x');
    assert_eq!(c.fg, [100, 100, 100], "fg dimmed halfway to bg");
    assert!(!c.bold && !c.wide, "inherited SGR/geometry dropped");
}

#[test]
fn ghost_sparse_padding_uses_the_live_terminal_blank_not_last_sgr() {
    let mut term = Terminal::new(1, 5);
    term.process(b"\x1b]10;rgb:11/22/33\x07\x1b]11;rgb:44/55/66\x07\x1b[?5h");
    let live_blank = terminal_blank_cell(&term);
    assert_eq!(
        (live_blank.fg, live_blank.bg),
        ([0x44, 0x55, 0x66], [0x11, 0x22, 0x33]),
        "DECSCNM-resolved live defaults"
    );
    let mut s = scratch(1);
    let mut styled = blank();
    styled.ch = '!';
    styled.bg = [180, 20, 30];
    styled.bold = true;
    s.cells[0].push(styled);

    assert!(
        paint_prediction_ghosts(&mut s, &[Prediction::test_at(0, 3, 'x')], 5, live_blank)
            .newest
            .is_some()
    );
    assert_eq!(s.cells[0][0], styled, "materialized SGR cell is preserved");
    assert_eq!(s.cells[0][1..3], [live_blank, live_blank], "implicit tail");
    assert_eq!(s.cells[0][3].bg, live_blank.bg);
    assert!(!s.cells[0][3].bold);
}

#[test]
fn ghost_clips_to_pane_rect() {
    let mut s = scratch(2);
    let preds = [
        Prediction::test_at(0, 5, 'a'), // at cols → clipped
        Prediction::test_at(0, 9, 'b'), // past cols → clipped
        Prediction::test_at(7, 0, 'c'), // past rows → ignored
    ];
    assert_eq!(
        paint_prediction_ghosts(&mut s, &preds, 5, blank()).newest,
        None,
        "nothing painted when everything clips"
    );
    assert!(s.cells[0].is_empty(), "clipped ghosts never extend the row");
    let ok = [Prediction::test_at(0, 4, 'd')]; // last in-rect column paints
    assert_eq!(
        paint_prediction_ghosts(&mut s, &ok, 5, blank()).newest,
        Some((0, 4))
    );
    assert_eq!(s.cells[0][4].ch, 'd');
}

#[test]
fn the_cursor_goes_past_the_newest_painted_guess_never_a_refused_one() {
    let row = || {
        let mut lead = blank();
        lead.ch = '日';
        let mut continuation = blank();
        continuation.wide = true;
        vec![blank(), lead, continuation]
    };
    let mut s = scratch(1);
    s.cells[0] = row();
    let preds = [
        Prediction::test_at(0, 0, 'b'),
        Prediction::test_at(0, 1, 'c'),
    ];
    assert_eq!(
        paint_prediction_ghosts(&mut s, &preds, 5, blank()),
        super::GhostsPainted {
            newest: Some((0, 0)),
            refused: 1,
        },
        "the newest PAINTED guess, not the refused newest, which is counted"
    );
    assert_eq!(s.cells[0][0].ch, 'b');
    assert_eq!(s.cells[0][1].ch, '日');
    assert!(s.cells[0][2].wide, "the glyph keeps its right half");

    let mut s = scratch(1);
    s.cells[0] = row();
    assert_eq!(
        paint_prediction_ghosts(&mut s, &preds[1..], 5, blank()).newest,
        None,
        "every guess refused: nothing to draw the cursor past"
    );
    assert_eq!(s.cells[0], row(), "and nothing painted");
}

#[test]
fn a_frame_that_refused_an_older_guess_shows_no_key_through_the_newest() {
    let mut predictor = crate::predict::Predictor::new(crate::predict::PredictMode::Always);
    let now = std::time::Instant::now();
    for (col, ch) in (0u16..).zip(['a', 'b', 'c', 'd']) {
        assert!(predictor.predict_char(ch, (0, col), 0, 10, now));
    }
    let newest = predictor.shown_through();
    assert!(newest.is_some(), "fixture: Always shows all four");
    let wide = || {
        let mut lead = blank();
        lead.ch = '日';
        let mut continuation = blank();
        continuation.wide = true;
        vec![blank(), lead, continuation, blank()]
    };
    let mut s = scratch(1);
    s.cells[0] = wide();
    let painted = paint_prediction_overlay(&mut predictor, &mut s, now, 10, blank());
    assert_eq!(painted.cursor, Some((0, 4)), "one past the newest painted");
    assert_eq!(
        (s.cells[0][0].ch, s.cells[0][1].ch, s.cells[0][3].ch),
        ('a', '日', 'd'),
        "fixture: `b` and `c` refused, `a` and `d` painted"
    );
    assert_eq!(
        painted.shown_through, None,
        "the keys of `b` and `c` are not on glass"
    );
    let mut s = scratch(1);
    s.cells[0] = vec![blank(); 4];
    let painted = paint_prediction_overlay(&mut predictor, &mut s, now, 10, blank());
    assert_eq!(painted.shown_through, newest, "every guess painted");
    assert_eq!(
        painted.closes_input_through(),
        None,
        "`always` shows them with no proof: their keys wait for their echoes"
    );
}

fn wide_row() -> Vec<RenderCell> {
    let cell = |ch: char| RenderCell { ch, ..blank() };
    let mut continuation = blank();
    continuation.wide = true;
    vec![cell('a'), cell('b'), cell('日'), continuation]
}

#[test]
fn a_state_chain_is_painted_all_or_none() {
    let mut s = scratch(1);
    s.cells[0] = wide_row();
    let refused = [
        Prediction::test_at(0, 1, 'x'),
        Prediction::test_at(0, 3, 'y'),
    ];
    assert_eq!(
        paint_prediction_chain(&mut s, &refused, (0, 1), 6, blank()),
        None
    );
    assert_eq!(s.cells[0], wide_row(), "nothing painted");
    assert_eq!(
        paint_prediction_chain(&mut s, &[], (0, 3), 6, blank()),
        None
    );
    assert_eq!(
        paint_prediction_chain(&mut s, &[], (0, 1), 6, blank()),
        Some((0, 1)),
        "a pure motion: its cursor, no cell"
    );
    assert_eq!(s.cells[0], wide_row());
    let erase = [Prediction::test_at(0, 1, ' ')];
    assert_eq!(
        paint_predictions(&mut s, &erase, Some((0, 1)), 6, blank()),
        super::OverlayPaint {
            cursor: Some((0, 1)),
            whole: true,
        },
        "the chain's cursor, on its vacated cell, the chain painted whole"
    );
    assert_eq!(s.cells[0][1].ch, ' ', "the vacated cell painted blank");
    let mut s = scratch(1);
    s.cells[0] = wide_row();
    assert_eq!(
        paint_predictions(&mut s, &erase, None, 6, blank()),
        super::OverlayPaint {
            cursor: Some((0, 2)),
            whole: true,
        },
        "the glyph lane: one past the newest painted guess, every guess painted"
    );
}

#[test]
fn a_shift_is_painted_as_the_text_it_moves() {
    let cell = |ch: char| RenderCell { ch, ..blank() };
    let mut bold = cell('c');
    bold.bold = true;
    bold.fg = [200, 0, 0];
    let mut s = scratch(1);
    s.cells[0] = vec![cell('a'), cell('b'), bold, cell('d')];
    let shift = [
        Prediction::test_ghost(0, 1, 'x', GhostCell::Typed),
        Prediction::test_ghost(0, 2, 'b', GhostCell::Moved { from: 1 }),
        Prediction::test_ghost(0, 3, 'c', GhostCell::Moved { from: 2 }),
        Prediction::test_ghost(0, 4, 'd', GhostCell::Moved { from: 3 }),
    ];
    assert_eq!(
        paint_predictions(&mut s, &shift, Some((0, 2)), 6, blank()).cursor,
        Some((0, 2)),
        "the chain's cursor"
    );
    let text: String = s.cells[0].iter().map(|c| c.ch).collect();
    assert_eq!(text, "axbcd");
    assert_eq!(s.cells[0][3], bold, "`c` moved, in its own style");
    assert!(
        s.cells[0][1].fg != blank().fg && !s.cells[0][1].bold,
        "the typed glyph: a ghost"
    );

    let mut s = scratch(1);
    s.cells[0] = wide_row();
    let refused = [Prediction::test_ghost(
        0,
        1,
        '日',
        GhostCell::Moved { from: 2 },
    )];
    assert_eq!(
        paint_predictions(&mut s, &refused, Some((0, 1)), 6, blank()),
        super::OverlayPaint {
            cursor: None,
            whole: false,
        }
    );
    assert_eq!(s.cells[0], wide_row(), "nothing painted");
}
