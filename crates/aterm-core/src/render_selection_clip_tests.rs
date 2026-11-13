// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{COLOR_UNSET, PaneSelection, RenderInput, SelectionClip, SelectionRowKey};
use crate::selection::{SelectionSide, SelectionType, TextSelection};

#[test]
fn multiline_selection_is_confined_to_its_frame_rectangle() {
    let mut input = RenderInput::empty();
    input.rows = 5;
    input.cols = 14;
    input
        .selection
        .start_selection(1, 7, SelectionSide::Left, SelectionType::Simple);
    input.selection.update_selection(3, 8, SelectionSide::Right);
    input.selection.complete_selection();
    input.selection_clip = Some(SelectionClip::new(1, 4, 6, 11));

    assert!(input.selection_contains_cell(1, 7, false, false));
    assert!(input.selection_contains_cell(2, 6, false, false));
    assert!(input.selection_contains_cell(2, 10, false, false));
    assert!(input.selection_contains_cell(3, 8, false, false));

    assert!(
        !input.selection_contains_cell(2, 5, true, false),
        "wide-cell snapping may not escape through the clip's left edge"
    );
    assert!(
        !input.selection_contains_cell(2, 11, false, true),
        "a wide continuation may not escape through the clip's right edge"
    );
    assert!(
        !input.selection_contains_cell(0, 7, false, false),
        "rows above the focused pane stay unselected"
    );
    assert!(
        !input.selection_contains_cell(4, 7, false, false),
        "rows below the focused pane stay unselected"
    );
}

#[test]
fn per_pane_selections_resolve_per_cell_while_the_span_is_a_hull() {
    let pane = |lo: u16, hi: u16, clip: SelectionClip, bg: u32| {
        let mut selection = TextSelection::new();
        selection.start_selection(1, lo, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(1, hi, SelectionSide::Right);
        selection.complete_selection();
        PaneSelection {
            selection,
            clip,
            bg,
            fg: COLOR_UNSET,
            inactive: bg != COLOR_UNSET,
        }
    };

    let mut input = RenderInput::empty();
    input.rows = 3;
    input.cols = 12;
    input.selections = vec![
        pane(0, 3, SelectionClip::new(1, 2, 0, 5), COLOR_UNSET),
        pane(7, 11, SelectionClip::new(1, 2, 7, 12), 0x0011_2233),
    ];

    assert_eq!(input.selection_hit(1, 2, false, false), Some(0));
    assert_eq!(input.selection_hit(1, 9, false, false), Some(1));
    assert_eq!(
        input.selection_hit(1, 6, false, false),
        None,
        "the divider gap between the two panes belongs to neither"
    );
    assert_eq!(
        input.selection_hit(0, 2, false, false),
        None,
        "a row outside both boxes is unselected"
    );

    assert_eq!(
        input.selection_row_span(1),
        Some((0, 11)),
        "the span is the HULL over both panes — a scan window, not a key"
    );
    let key: Vec<_> = input.selection_row_keys(1).collect();
    assert_eq!(
        key,
        vec![
            SelectionRowKey {
                lo: 0,
                hi: 3,
                bg: COLOR_UNSET,
                fg: COLOR_UNSET,
                inactive: false,
            },
            SelectionRowKey {
                lo: 7,
                hi: 11,
                bg: 0x0011_2233,
                fg: COLOR_UNSET,
                inactive: true,
            },
        ],
        "the key keeps the two panes' spans and colours separate"
    );
    assert!(
        input.selection_row_keys(2).next().is_none(),
        "a row no pane reaches keys empty"
    );
}

#[test]
fn an_empty_pane_list_leaves_the_scalar_path_untouched() {
    let mut input = RenderInput::empty();
    input.rows = 2;
    input.cols = 4;
    input
        .selection
        .start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
    input.selection.update_selection(0, 2, SelectionSide::Right);
    input.selection.complete_selection();

    assert!(input.any_selection());
    assert_eq!(input.selection_hit(0, 1, false, false), Some(0));
    assert_eq!(input.selection_row_span(0), Some((1, 2)));

    input.selections = vec![PaneSelection {
        selection: TextSelection::new(),
        clip: SelectionClip::new(0, 2, 0, 4),
        bg: COLOR_UNSET,
        fg: COLOR_UNSET,
        inactive: false,
    }];
    assert!(
        !input.selection_contains_cell(0, 1, false, false),
        "the list is the authority once it is non-empty"
    );
    assert!(input.any_selection(), "…and it still counts as a selection");

    let mut list_only = RenderInput::empty();
    list_only.rows = 2;
    list_only.cols = 4;
    assert!(
        !list_only.any_selection(),
        "precondition: nothing selected at all"
    );
    let mut only = TextSelection::new();
    only.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    only.update_selection(0, 2, SelectionSide::Right);
    only.complete_selection();
    list_only.selections = vec![PaneSelection {
        selection: only,
        clip: SelectionClip::new(0, 2, 0, 4),
        bg: COLOR_UNSET,
        fg: COLOR_UNSET,
        inactive: true,
    }];
    assert!(
        !list_only.selection.has_selection(),
        "the scalar is empty — only the list carries a selection"
    );
    assert!(
        list_only.any_selection(),
        "an unfocused pane's selection must still gate the frame-wide passes"
    );
}
