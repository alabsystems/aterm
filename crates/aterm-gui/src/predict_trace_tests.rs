// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::predict::{KeySample, PredictMode, Predictor};

fn scratch_with(text: &str) -> RenderInput {
    let mut s = RenderInput::empty();
    let blank = aterm_core::terminal::RenderCell::default();
    s.cells = vec![
        text.chars()
            .map(|ch| aterm_core::terminal::RenderCell { ch, ..blank })
            .collect(),
    ];
    s
}

/// A press names the op it armed and a frame the op of its newest visible
/// key, its rows as the engine had them and as drawn, and both cursors;
/// the text is percent-encoded, so a line splits on spaces.
#[test]
fn a_press_names_its_op_and_a_frame_its_newest_key_and_both_rows() {
    let now = Instant::now();
    let mut p = Predictor::new(PredictMode::Always);
    let sample = KeySample {
        cursor: (0, 2),
        grid: (10, 1),
        wrap_pending: Some(false),
        ..KeySample::default()
    };
    let before = p.newest_op();
    assert!(p.predict_glyph('x', &sample, now));
    let line = press_line(&p, "glyph x", p.armed_since(before), now);
    assert!(line.contains(" key=glyph%20x armed=0 pending=1"), "{line}");
    let again = press_line(&p, "unmodelled", None, now);
    assert!(again.contains(" armed=- pending=1"), "{again}");

    let mut s = scratch_with("$ ");
    s.cursor_row = 0;
    s.cursor_col = 2;
    let (cells, chain) = p.overlay_frame(now);
    let cells = cells.to_vec();
    let under = Under(Under::rows(&s, &cells, chain));
    for c in &cells {
        let pad = s.cells[0][0];
        s.cells[0].resize(usize::from(c.col) + 1, pad);
        s.cells[0][usize::from(c.col)].ch = c.ch;
    }
    let line = frame_line(&p, &under, &s, (0, 2), (0, 3), now);
    assert!(
        line.contains(" mode=always engaged=0 visible=1 newest=0 cursor=0,2 shown=0,3"),
        "{line}"
    );
    assert!(line.ends_with(" row=0 under=$ over=$%20x"), "{line}");
    assert_eq!(encode("a%b é"), "a%25b%20%C3%A9");
}
