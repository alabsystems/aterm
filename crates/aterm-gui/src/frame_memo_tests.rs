// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use aterm_core::terminal::Terminal;

use super::{FrameSlot, ScreenText, TextMemo};

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
    let ranges = FrameSlot::default();
    let mut t = Terminal::new(6, 30);
    t.process(b"\x1b[?1049h\x1b[H ls -la /tmp/work\r\n [y/n]\r\n");
    let box_a = slot.look_frame().text(&t);
    let range_a = ranges.look_frame().text_span(&t, 0, 2);
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
    let range_b = ranges.look_frame().text_span(&t, 0, 2);
    assert_eq!(range_a.body(), box_a.span(0, 2));
    assert_eq!(range_b.body(), fresh.span(0, 2));
    assert_ne!(range_a.body(), range_b.body());
    let by_seq = (seq_a, box_a.whole().to_string());
    assert!(
        by_seq.0 == t.content_seq() && by_seq.1 != fresh.whole(),
        "a memo keyed on content_seq alone serves box A for box B"
    );
}

#[test]
fn shaped_logical_reads_share_a_range_until_a_whole_read_promotes_it() {
    let mut terminal = Terminal::new(6, 32);
    terminal.process("above\r\n\r\n\r\n世界 e\u{301}   \r\n\r\n".as_bytes());
    let slot = FrameSlot::default();
    let body = "世界 e\u{301}\n\n\n";

    let span = slot.look_frame().text_span(&terminal, 3, 6);
    assert_eq!(span.body(), body);
    assert_eq!(span.rows().collect::<Vec<_>>(), ["世界 e\u{301}", "", ""]);
    let TextMemo::Range(captured) = span.memo else {
        panic!("the shaped miss should capture only the requested range");
    };
    assert_eq!(captured.first, 3);
    assert_eq!(captured.rows.ends.len(), 3);
    assert_eq!(captured.rows.text, body);
    let covered = slot.look_frame().text_span(&terminal, 4, 6);
    assert_eq!(covered.body(), "\n\n");
    let TextMemo::Range(reused) = covered.memo else {
        panic!("the covered span should reuse the range");
    };
    assert!(Arc::ptr_eq(&captured, &reused));

    let mut delayed = slot.look_frame();
    let delayed_span = delayed.text_span(&terminal, 0, 1);
    assert_eq!(delayed_span.body(), "above\n");
    let whole = slot.look_frame().text(&terminal);
    drop(delayed);
    assert_eq!(whole.span(3, 6), body);
    assert_eq!(
        whole.fnv(),
        aterm_hash::fnv1a_64(crate::control::screen_text(&terminal).as_bytes())
    );
    let span = slot.look_frame().text_span(&terminal, 3, 6);
    assert_eq!(span.body(), body);
    let TextMemo::Whole(reused) = span.memo else {
        panic!("the delayed range should leave the whole memo in place");
    };
    assert!(Arc::ptr_eq(&whole, &reused));
}
