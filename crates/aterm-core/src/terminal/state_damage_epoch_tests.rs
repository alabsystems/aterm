// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::Terminal;

#[test]
fn damage_epoch_advances_on_write_not_on_noop() {
    let mut term = Terminal::new(4, 10);

    assert!(term.has_damage(), "fresh terminal must start damaged");
    let e0 = term.damage_epoch();
    assert_eq!(e0, 1, "first observed epoch is 1");
    assert_eq!(term.damage_epoch(), e0, "epoch is stable within a session");

    term.take_damage();
    assert!(!term.has_damage(), "take_damage clears grid damage");
    assert_eq!(term.damage_epoch(), e0, "no new damage => epoch unchanged");

    term.process(b"hello");
    assert!(term.has_damage(), "a write must damage the grid");
    let e1 = term.damage_epoch();
    assert_eq!(e1, e0 + 1, "a write advances the epoch by one");
    assert_eq!(term.damage_epoch(), e1, "still one session until cleared");

    term.take_damage();
    assert_eq!(term.damage_epoch(), e1, "cleared => epoch holds");

    term.process(b"");
    assert!(!term.has_damage(), "an empty write damages nothing");
    assert_eq!(
        term.damage_epoch(),
        e1,
        "no-op write must NOT advance the epoch"
    );

    term.process(b"world");
    let e2 = term.damage_epoch();
    assert_eq!(e2, e1 + 1, "the next write advances the epoch again");
    assert!(e2 > e1, "epoch is monotonic");
}

#[test]
fn content_seq_advances_on_content_not_on_viewport_scroll() {
    let mut term = Terminal::new(3, 10);

    let g0 = term.content_seq();
    assert!(g0 > 0, "content_gen is initialized nonzero");

    term.process(b"hello");
    let g1 = term.content_seq();
    assert!(g1 > g0, "a cell write advances content_gen");

    term.process(b"");
    assert_eq!(
        term.content_seq(),
        g1,
        "a no-op process must NOT advance content_gen"
    );

    term.process(b"\x1b[K");
    let g2 = term.content_seq();
    assert!(g2 > g1, "a line erase advances content_gen");

    for _ in 0..10 {
        term.process(b"line\r\n");
    }
    let g3 = term.content_seq();
    assert!(g3 > g2, "a content scroll (scroll_up) advances content_gen");

    let epoch_before = term.damage_epoch();
    term.take_damage();
    term.grid_mut().scroll_display(2);
    assert!(
        term.has_damage(),
        "a viewport scroll still damages the grid (for the renderer)"
    );
    assert!(
        term.damage_epoch() > epoch_before,
        "a viewport scroll DOES advance damage_epoch"
    );
    assert_eq!(
        term.content_seq(),
        g3,
        "a pure viewport scroll must NOT advance content_gen"
    );

    term.grid_mut().scroll_display(0);
    assert_eq!(
        term.content_seq(),
        g3,
        "a no-op viewport scroll must NOT advance content_gen"
    );

    term.process(b"x");
    let g4 = term.content_seq();
    assert!(g4 > g3, "content_gen is monotonic across further writes");
}
