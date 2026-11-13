// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{DefaultBgSpan, PaneSelection, RenderInput, SelectionClip};

#[test]
fn lookup_uses_owning_pane_and_falls_back_for_gaps_or_malformed_spans() {
    let mut input = RenderInput::empty();
    input.rows = 2;
    input.cols = 10;
    input.default_bg = 0x0001_0203;
    input.default_bg_spans = vec![
        vec![
            DefaultBgSpan::new(0, 4, 0x0011_2233),
            DefaultBgSpan::new(5, 10, 0x0044_5566),
        ],
        vec![DefaultBgSpan::new(2, 99, 0x0077_8899)],
    ];

    assert_eq!(input.default_bg_at(0, 0), 0x0011_2233);
    assert_eq!(input.default_bg_at(0, 3), 0x0011_2233);
    assert_eq!(
        input.default_bg_at(0, 4),
        input.default_bg,
        "divider gap inherits the frame scalar"
    );
    assert_eq!(input.default_bg_at(0, 5), 0x0044_5566);
    assert_eq!(input.default_bg_at(0, 9), 0x0044_5566);
    assert_eq!(
        input.default_bg_at(1, 2),
        input.default_bg,
        "out-of-bounds producer spans fail closed to the scalar"
    );
    assert_eq!(
        input.default_bg_at(99, 0),
        input.default_bg,
        "missing rows retain the historical scalar path"
    );
}

#[test]
fn spans_are_render_content_and_survive_both_clone_paths() {
    let mut source = RenderInput::empty();
    source.rows = 1;
    source.cols = 8;
    source.selection_bg = 0x0001_0203;
    source.selection_fg = 0x0004_0506;
    source.selection_clip = Some(SelectionClip::new(0, 1, 4, 8));
    source.selections = vec![PaneSelection {
        selection: crate::selection::TextSelection::new(),
        clip: SelectionClip::new(0, 1, 4, 8),
        bg: 0x0001_0203,
        fg: 0x0004_0506,
        inactive: true,
    }];
    source.default_bg_spans = vec![vec![
        DefaultBgSpan::new(0, 4, 0x0011_2233),
        DefaultBgSpan::new(4, 8, 0x0044_5566),
    ]];

    let cloned = source.clone();
    assert_eq!(cloned.default_bg_spans, source.default_bg_spans);
    assert_eq!(cloned.selection_bg, source.selection_bg);
    assert_eq!(cloned.selection_fg, source.selection_fg);
    assert_eq!(cloned.selection_clip, source.selection_clip);
    assert_eq!(cloned.selections, source.selections);
    assert_eq!(cloned, source);

    let mut reused = RenderInput::empty();
    reused.default_bg_spans = vec![vec![DefaultBgSpan::new(0, 99, 0x00aa_bbcc)]];
    reused.selections = vec![PaneSelection {
        selection: crate::selection::TextSelection::new(),
        clip: SelectionClip::new(9, 9, 9, 9),
        bg: 0,
        fg: 0,
        inactive: false,
    }];
    reused.clone_from(&source);
    assert_eq!(reused.default_bg_spans, source.default_bg_spans);
    assert_eq!(reused.selection_bg, source.selection_bg);
    assert_eq!(reused.selection_fg, source.selection_fg);
    assert_eq!(reused.selection_clip, source.selection_clip);
    assert_eq!(reused.selections, source.selections);
    assert_eq!(reused, source);

    let mut changed = source.clone();
    changed.default_bg_spans[0][0].default_bg ^= 0x0001_0101;
    assert_ne!(
        changed, source,
        "pane-local default provenance changes rendered pixels"
    );

    let mut changed = source.clone();
    changed.selection_bg ^= 0x0001_0101;
    assert_ne!(changed, source, "live selection bg changes rendered pixels");

    let mut changed = source.clone();
    changed.selection_fg ^= 0x0001_0101;
    assert_ne!(changed, source, "live selection fg changes rendered pixels");

    let mut changed = source.clone();
    changed.selection_clip = Some(SelectionClip::new(0, 1, 5, 8));
    assert_ne!(
        changed, source,
        "split selection bounds change rendered pixels"
    );

    let mut changed = source.clone();
    changed.selections[0].clip = SelectionClip::new(0, 1, 5, 8);
    assert_ne!(changed, source, "a pane's selection box changes pixels");

    let mut changed = source.clone();
    changed.selections[0].bg ^= 0x0001_0101;
    assert_ne!(changed, source, "a pane's live selection bg changes pixels");

    let mut changed = source.clone();
    changed.selections[0].fg ^= 0x0001_0101;
    assert_ne!(changed, source, "a pane's live selection fg changes pixels");

    let mut changed = source.clone();
    changed.selections[0].inactive = false;
    assert_ne!(
        changed, source,
        "a pane's focus selects a different band colour"
    );

    let mut changed = source.clone();
    changed.selections.clear();
    assert_ne!(
        changed, source,
        "dropping the list hands the frame back to the scalar authority"
    );
}
