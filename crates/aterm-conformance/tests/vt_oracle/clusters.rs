// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Marks and clusters in the differential VT oracle: the comparison sees them, the generator
//! reaches every place one can land, and both engines join every code point alike.

use super::caps::Caps;
use super::engines::{Astream, Aterm, Marks};
use super::generator::{self, MARK_RANGES, Reach};
use astream_term::screen::MAX_COMBINING;

/// Both engines keep the same marks of a cell: the comparison holds the first of them equal, and
/// a mark past the cap drops on both sides only while the caps are one number.
#[test]
fn the_engines_keep_as_many_marks() {
    assert_eq!(MAX_COMBINING, aterm_core::grid::CellExtra::MAX_COMBINING);
}

/// The comparison is not vacuous: a mark, its order and the cell it sits on all show, and the two
/// engines agree on the text each is compared with (a negative control for `engines.rs`).
#[test]
fn the_comparison_sees_a_cells_marks() {
    let snap = |text: &str| {
        let mut t = Aterm::new(6, 2);
        t.feed(text.as_bytes());
        t.snap()
    };
    let marked = snap("a\u{301}\u{302}b");
    for (what, other) in [
        ("no mark", "ab"),
        ("one mark fewer", "a\u{301}b"),
        ("the marks in the other order", "a\u{302}\u{301}b"),
        ("the marks on the next cell", "ab\u{301}\u{302}"),
    ] {
        assert!(marked.first_difference(&snap(other)).is_some(), "{what}");
    }
    assert_eq!(marked.row_text(0), "a\u{301}\u{302}b");
    let mut a = Astream::new(6, 2);
    a.feed("a\u{301}\u{302}b".as_bytes());
    assert!(a.snap().first_difference(&marked).is_none());
    // And on a wide glyph's lead, where the fold holds its marks and aterm its extras.
    let mut a = Astream::new(6, 2);
    a.feed("\u{4e2d}\u{301}".as_bytes());
    let wide = snap("\u{4e2d}\u{301}");
    assert!(a.snap().first_difference(&wide).is_none());
    let mut stripped = wide.clone();
    stripped.cells[0].marks = Marks::NONE;
    assert!(wide.first_difference(&stripped).is_some(), "a lead's marks");
}

/// Everything the generator draws as a mark takes no column in the fold.
#[test]
fn the_generated_marks_take_no_column() {
    for &(lo, hi) in MARK_RANGES {
        for c in (lo..=hi).filter_map(char::from_u32) {
            assert_eq!(astream_term::char_display_width(c), 0, "U+{:04X}", c as u32);
        }
    }
}

/// The fold joins every code point as aterm's write path does: a VS16 and a VS15 after it, a skin
/// tone after it, and a skin tone after a VS16 give the same cells and cursor, so
/// `is_vs16_emoji_capable` and the emoji modifier bases of both engines are one set though neither
/// is copied from the other. The last reaches the bases a VS16 has to widen before they take a
/// tone. Every emoji lies below U+20000; the ideographs above are one wide run.
#[test]
fn the_fold_joins_every_code_point_as_aterm_does() {
    const TONE: char = '\u{1f3fd}';
    let mut a = Astream::new(8, 1);
    let mut t = Aterm::new(8, 1);
    let (mut widened, mut narrowed, mut joined, mut joined_wide) = (0u32, 0u32, 0u32, 0u32);
    let mut differ = Vec::new();
    let mut bytes = String::new();
    for c in (' '..'\u{20000}').filter(|c| !c.is_control()) {
        for (prefix, suffix, count) in [
            ("", '\u{fe0f}', &mut widened),
            ("", '\u{fe0e}', &mut narrowed),
            ("", TONE, &mut joined),
            ("\u{fe0f}", TONE, &mut joined_wide),
        ] {
            bytes.clear();
            bytes.push_str("\r\x1b[2K");
            bytes.push(c);
            bytes.push_str(prefix);
            let alone = {
                a.feed(bytes.as_bytes());
                t.feed(bytes.as_bytes());
                t.snap()
            };
            bytes.clear();
            bytes.push(suffix);
            a.feed(bytes.as_bytes());
            t.feed(bytes.as_bytes());
            let (sa, st) = (a.snap(), t.snap());
            if let Some(what) = sa.first_difference(&st) {
                differ.push(format!(
                    "U+{:04X} {prefix:?} U+{:04X}: {what}",
                    c as u32, suffix as u32
                ));
            }
            let changed = if suffix == TONE {
                st.cursor == alone.cursor
            } else {
                st.cells[0].ink.half != alone.cells[0].ink.half
            };
            *count += u32::from(changed);
        }
    }
    assert!(
        differ.is_empty(),
        "{} sequences apart, first {:?}",
        differ.len(),
        &differ[..differ.len().min(10)]
    );
    // Not vacuous: each suffix changes something on what Unicode says it does: Text_Default_Emoji
    // for a VS16, the wide glyphs for a VS15, the wide Emoji_Modifier_Base for a skin tone, and
    // more of the bases than that once a VS16 has widened the text-default ones.
    assert!(
        widened > 200 && narrowed > 20_000 && joined > 100 && joined_wide > joined,
        "widened {widened}, narrowed {narrowed}, joined {joined}, joined after a VS16 {joined_wide}"
    );
}

/// The generated marks reach every place the corpus names: from a pending wrap, at column 0 of a
/// fresh row and of a row that continues the one above, at a left margin a motion put the cursor on
/// and one a print stepped onto, left of the cursor; past the cap; carried or taken by each edit
/// and by a resize.
#[test]
fn the_generated_marks_reach_every_placement() {
    let caps = Caps::probe();
    assert!(caps.combining_marks && caps.clusters, "{caps:?}");
    let mut reach = Reach::default();
    for seed in 0..5_000 {
        let program = generator::program(seed, caps);
        assert!(
            generator::run_reaching(&program, &mut reach).is_none(),
            "seed {seed:#x} diverges (random_subset_programs_agree reports it)"
        );
    }
    // About a third of what these seeds reached when the floors were set (654 from a wrap, 3,002
    // left of the cursor, 1,431 and 358 at column 0, 36 at a left margin and 131 stepped onto one,
    // 13,107 chunks with a full cell, 348 resizes, and each edit moving marks 36 to 606 times).
    let m = reach.marks;
    let mut floors = vec![
        ("from a pending wrap", m.from_wrap, 200),
        ("left of the cursor", m.left_of_cursor, 1_000),
        ("at column 0 of a fresh row", m.at_column_0, 450),
        ("at column 0 of a continued row", m.at_wrapped_column_0, 120),
        ("at a left margin", m.at_left_margin, 8),
        ("stepped onto a left margin", m.stepped_onto_margin, 40),
        ("a cell at the cap", m.at_cap, 4_000),
        ("a width change with marks up", m.resized, 110),
    ];
    for (op, got) in generator::MARK_OPS.iter().zip(m.moved_by) {
        floors.push((op.1, got, 13));
    }
    for (what, got, floor) in floors {
        assert!(got >= floor, "{what}: {got} < {floor} ({m:?})");
    }
}
