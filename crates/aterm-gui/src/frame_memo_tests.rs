// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;

use super::{FrameSlot, ScreenText};

#[test]
fn screen_text_slices_are_the_rows_and_the_fence_hash() {
    let mut t = Terminal::new(5, 16);
    t.process("世界 e\u{301}   \r\n\r\ntrail   \r\n\x1b[7mx\x1b[0m".as_bytes());
    let text = ScreenText::of(&t);
    let rows = t.rows() as usize;
    assert_eq!(text.row_count(), rows);
    let mut joined = String::new();
    for r in 0..rows {
        assert_eq!(text.row(r), crate::control::visible_row(&t, r), "row {r}");
        joined.push_str(text.row(r));
        joined.push('\n');
    }
    assert_eq!(text.whole(), joined);
    assert_eq!(text.rows().count(), rows);
    for first in 0..=rows {
        for last in 0..=rows {
            assert_eq!(
                text.rows_hash(first, last),
                crate::control::rows_hash(&t, first, last),
                "rows {first}..={last}"
            );
        }
        assert_eq!(text.span(first, first), "");
    }
    assert_eq!(
        text.span(1, 3),
        format!("{}\n{}\n", text.row(1), text.row(2))
    );
}

#[test]
fn a_re_entered_alt_screen_at_the_same_seq_is_not_served_the_old_text() {
    let slot = FrameSlot::default();
    let mut t = Terminal::new(6, 30);
    t.process(b"\x1b[?1049h\x1b[H ls -la /tmp/work\r\n [y/n]\r\n");
    let box_a = slot.look_frame().text(&t);
    let seq_a = t.content_seq();
    t.process(b"\x1b[?1049l\x1b[?1049h\x1b[H rm -rf /tmp/work\r\n [y/n]\r\n");
    assert_eq!(
        t.content_seq(),
        seq_a,
        "the premise: both boxes read one seq"
    );
    let fresh = ScreenText::of(&t);
    assert_ne!(
        fresh.whole(),
        box_a.whole(),
        "the premise: the boxes differ"
    );
    assert_eq!(slot.look_frame().text(&t).whole(), fresh.whole());
    assert!(fresh.whole().contains("rm -rf"));
    let by_seq = (seq_a, box_a.whole().to_string());
    assert!(
        by_seq.0 == t.content_seq() && by_seq.1 != fresh.whole(),
        "a memo keyed on content_seq alone serves box A for box B"
    );
}
