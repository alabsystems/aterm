// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use aterm_types::text_shaping::AmbiguousWidth;

use super::{SearchKey, SearchWindow};
use crate::search::max_cached_for_retained;
use crate::terminal::Terminal;

fn key() -> SearchKey {
    let mut t = Terminal::new(4, 40);
    for i in 0..30 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    SearchKey::capture(&t, 1_000)
}

/// `k` with the fields `f` changes.
fn moved(mut k: SearchKey, f: impl FnOnce(&mut SearchKey)) -> SearchKey {
    f(&mut k);
    k
}

/// Each verdict compares exactly the fields it names: every field moved alone flips
/// the verdicts that compare it and leaves the others standing.
#[test]
fn each_verdict_compares_every_field_it_names() {
    let k = key();
    assert!(k.same_frame(&k) && k.same_generation(&k) && k.refreshable_from(&k));
    assert_eq!(k.ambiguous, AmbiguousWidth::Single, "precondition");
    for (field, frame) in [
        ("alt", moved(k, |k| k.alt = !k.alt)),
        ("renumber_epoch", moved(k, |k| k.renumber_epoch += 1)),
        ("row_revision", moved(k, |k| k.row_revision += 1)),
        ("cols", moved(k, |k| k.cols -= 1)),
        (
            "ambiguous",
            moved(k, |k| k.ambiguous = AmbiguousWidth::Double),
        ),
    ] {
        assert!(!k.same_frame(&frame), "{field}");
        assert!(!k.same_generation(&frame), "{field}");
        assert!(!frame.refreshable_from(&k), "{field}");
    }
    let output = moved(k, |k| k.content_seq += 1);
    assert!(k.same_frame(&output));
    assert!(!k.same_generation(&output));
    assert!(output.refreshable_from(&k));
    let deeper = moved(k, |k| k.max_lines += 1);
    assert!(k.same_frame(&deeper));
    assert!(!k.same_generation(&deeper));
    assert!(!deeper.refreshable_from(&k));
    let advanced = moved(k, |k| {
        k.oldest += 3;
        k.content_seq += 1;
    });
    assert!(advanced.refreshable_from(&k));
    assert!(
        !k.refreshable_from(&advanced),
        "a retreat of the retained base"
    );
    let shorter = moved(k, |k| k.scrollback -= 1);
    assert!(
        !shorter.refreshable_from(&k),
        "a retreat of the indexed end"
    );
}

/// A Kitty `CSI +T` unscroll slides every retained row's key while `content_seq`
/// stands: only the epoch tells the two keys apart. NEGATIVE CONTROL: every other
/// field a generation compares is equal across it.
#[test]
fn an_unscroll_is_another_frame_at_the_same_content_seq() {
    let sb = aterm_scrollback::Scrollback::new(8, 64, 8_000_000);
    let mut t = Terminal::with_scrollback(6, 40, 8, sb);
    for i in 0..40 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    let before = SearchKey::capture(&t, 1_000);
    t.process(b"\x1b[2+T");
    let after = SearchKey::capture(&t, 1_000);
    assert_eq!(
        moved(after, |k| k.renumber_epoch = before.renumber_epoch),
        moved(before, |k| (k.oldest, k.scrollback) =
            (after.oldest, after.scrollback)),
        "the unscroll moves no generation field but the epoch"
    );
    assert_ne!(before.renumber_epoch, after.renumber_epoch);
    assert!(!before.same_frame(&after));
    assert!(!before.same_generation(&after));
}

/// `capture` reads the terminal's own accessors in one hold and floors the depth cap
/// at the visible screen; `window` is the newest `max_lines` rows of the retained
/// history and screen.
#[test]
fn capture_floors_the_depth_and_the_window_is_the_newest_rows() {
    let mut t = Terminal::new(4, 40);
    for i in 0..30 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    let whole = SearchKey::capture(&t, 1_000);
    assert_eq!(whole.cols, t.grid().cols());
    assert_eq!(whole.oldest, t.grid().oldest_absolute_row());
    assert_eq!(whole.scrollback, t.grid().scrollback_lines());
    assert_eq!(whole.ambiguous, t.ambiguous_width());
    let base = usize::try_from(whole.oldest).unwrap();
    let total = whole.scrollback + 4;
    assert_eq!(
        whole.window(),
        SearchWindow {
            visible_base: base + whole.scrollback,
            retained_base: base,
            retained_total: total,
            skipped_prefix: 0,
            indexed_end: base + total,
        }
    );
    assert_eq!(whole.window().searched_lines(), None);

    let floor = max_cached_for_retained(4);
    let tiny = SearchKey::capture(&t, 1);
    assert_eq!(
        tiny.max_lines, floor,
        "the screen stays searchable under any cap"
    );
    assert!(total > floor, "precondition: history past the floor");
    let window = tiny.window();
    assert_eq!(window.retained_total, floor);
    assert_eq!(window.skipped_prefix, total - floor);
    assert_eq!(window.retained_base, base + total - floor);
    assert_eq!(window.indexed_end, base + total);
    assert_eq!(window.searched_lines(), Some(floor));
}
