// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::{
    Annotation, MarksState, OutputBlock, ShellIntegrationState, TerminalMark, TransientState,
};
use super::apply_absolute_row_update;

#[test]
fn splice_preserves_exclusive_end_boundaries_and_remaps_all_anchor_families() {
    let mut shell = ShellIntegrationState::new();
    let mut before = OutputBlock::new(0, 2, 0);
    before.end_row = Some(3);
    shell.output_blocks.push_back(before);
    let mut at = OutputBlock::new(1, 3, 0);
    at.end_row = Some(3);
    shell.output_blocks.push_back(at);

    let mut marks = MarksState::new();
    marks.marks.push_back(TerminalMark::new(0, 3, 0));
    marks
        .annotations
        .push_back(Annotation::new(0, 3, 0, "footer".into()));

    let mut revision = 0;
    let mut transient = TransientState::new();
    transient.osc_events.set_polled(true);
    transient.osc_events.push_mark('A', 3, 0);
    transient.osc_events.push(133, "D;exit=0".into());
    transient.osc_events.push(7, "/tmp".into());
    apply_absolute_row_update(
        aterm_grid::AbsoluteRowUpdate::Splice { at: 3, inserted: 1 },
        &mut shell,
        &mut marks,
        &mut transient,
        &mut revision,
    );

    assert_eq!(shell.output_blocks[0].prompt_start_row, 2);
    assert_eq!(shell.output_blocks[0].end_row, Some(3));
    assert_eq!(shell.output_blocks[1].prompt_start_row, 4);
    assert_eq!(shell.output_blocks[1].end_row, Some(4));
    assert_eq!(marks.marks[0].row, 4);
    assert_eq!(marks.annotations[0].row, 4);
    assert_eq!(
        transient.osc_events.snapshot(),
        vec![
            (133, "A;row=4;col=0".into()),
            (133, "D;exit=0".into()),
            (7, "/tmp".into()),
        ]
    );
    assert_eq!(
        transient.osc_events.bytes(),
        "A;row=4;col=0D;exit=0/tmp".len()
    );
    assert_eq!(revision, 1);
}

#[test]
fn invalidation_drops_only_coordinate_bearing_shell_events() {
    let mut shell = ShellIntegrationState::new();
    let mut marks = MarksState::new();
    let mut transient = TransientState::new();
    transient.osc_events.set_polled(true);
    for (command, col) in [('A', 0), ('B', 1), ('C', 2)] {
        transient.osc_events.push_mark(command, 3, col);
    }
    for event in [
        (133, "D;exit=0".into()),
        (7, "/tmp".into()),
        (52, "clipboard".into()),
    ] {
        transient.osc_events.push(event.0, event.1);
    }
    let mut revision = 0;

    apply_absolute_row_update(
        aterm_grid::AbsoluteRowUpdate::Invalidate,
        &mut shell,
        &mut marks,
        &mut transient,
        &mut revision,
    );

    assert_eq!(
        transient.osc_events.snapshot(),
        vec![
            (133, "D;exit=0".into()),
            (7, "/tmp".into()),
            (52, "clipboard".into()),
        ]
    );
    assert_eq!(transient.osc_events.bytes(), "D;exit=0/tmpclipboard".len());
    assert_eq!(revision, 1);
}
