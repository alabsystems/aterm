// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DECALN rewrites every visible cell and homes the cursor. It did both without the bookkeeping
//! a home or ED 2 does for the resize hand-back, which went on owing back the history lines the
//! previous resize had revealed on the screen, though the pattern had overwritten them: the next
//! width change planned to hand back rows the screen no longer had and sliced past the end of
//! its rows (a panic in `Terminal::resize`, met by a generated bulk-versus-`write_char` sweep,
//! 2026-11-06). DECALN now forgets what the hand-back owed, as ED 2 does.

use aterm_core::terminal::Terminal;

#[test]
fn a_resize_after_decaln_rewraps_the_pattern() {
    // The minimised input: a scroll region from the top, enough text to scroll some into
    // history, a shrink, DECALN, then a grow that narrows the screen.
    let mut t = Terminal::new(10, 8);
    t.process(b"\x1b[1;6r4444444>FfH9JJJb3($BOdddg*Z\"xxx,Jsfiw>:+q-4RTTTTT`DDDDDDD");
    t.resize(5, 17);
    t.process(b"\x1b#8");
    t.resize(12, 9);

    // Five rows of 17 'E's, each rewrapped into a row of nine and a continuation of eight,
    // with nothing of the earlier text back on the screen.
    let rows: Vec<(String, bool)> = (0..12u16)
        .map(|r| {
            let text = t.row_text(usize::from(r)).unwrap_or_default();
            (text, t.grid().row(r).expect("a visible row").is_wrapped())
        })
        .collect();
    let want: Vec<(String, bool)> = (0..12)
        .map(|r| match r {
            0..10 if r % 2 == 0 => ("E".repeat(9), false),
            0..10 => ("E".repeat(8), true),
            _ => (String::new(), false),
        })
        .collect();
    assert_eq!(rows, want);
    assert_eq!((t.grid().cursor_row(), t.grid().cursor_col()), (0, 0));
}
