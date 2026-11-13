// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::session_watch::{Subscription, Watchers};
use aterm_core::selection::{SelectionSide, SelectionType};

fn select_to(t: &mut Terminal, end_col: u16) {
    let sel = t.text_selection_mut();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, end_col, SelectionSide::Right);
    sel.complete_selection();
}

#[test]
fn a_hold_that_moves_the_frame_rings_its_frame_watchers() {
    let term = Mutex::new(Terminal::new(4, 12));
    for i in 0..12 {
        term_lock(&term).process(format!("line {i}\r\n").as_bytes());
    }
    let watchers = Arc::<Watchers>::default();
    watchers.bind_frame(&term);
    let other = Subscription::single(&watchers);
    select_to(&mut term_lock(&term), 3);
    assert!(
        !other.wait(Duration::ZERO),
        "nobody reads the screen: a selection rings nobody"
    );

    let mut screen = Subscription::new();
    let flag = screen.watch_frame(&watchers);
    flag.store(false, Ordering::Release);
    let rang = || {
        let woke = screen.wait(Duration::ZERO);
        assert_eq!(
            woke,
            flag.swap(false, Ordering::AcqRel),
            "a frame ring sets the registration's flag"
        );
        woke
    };

    let seq = term_lock(&term).content_seq();
    assert!(!rang(), "a read-only hold rings nobody");
    term_lock(&term).scroll_display(2);
    assert_ne!(term_lock(&term).grid().display_offset(), 0, "the premise");
    assert!(!rang(), "a viewport scroll moves no frame");
    select_to(&mut term_lock(&term), 5);
    assert_eq!(term_lock(&term).content_seq(), seq, "the premise");
    assert!(rang(), "a selection rings the screen readers");
    assert!(!other.wait(Duration::ZERO), "and only them");
    {
        let mut t = term_lock(&term);
        t.holder_rings();
        t.process(b"more\r\n");
    }
    assert!(!rang(), "a holder that rings itself is left to");
    term_lock(&term).process(b"more\r\n");
    assert!(rang(), "the same write rings when its holder does not");

    drop(screen);
    select_to(&mut term_lock(&term), 7);
    assert!(
        !other.wait(Duration::ZERO),
        "the screen reader gone, a selection rings nobody"
    );
}
