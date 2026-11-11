// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `new` is the literal a test host wrote, and each setter moves only its own field: the
/// constructor every host and harness builds a frame's input through (§15.1 P0.1).
#[test]
fn a_host_frame_input_built_by_new_is_the_literal() {
    let now = Instant::now();
    let geometry = FrameGeom {
        rows: 20,
        cols: 100,
        cell_w: 15,
        cell_h: 28,
        origin_px: (3, 4),
    };
    let plain = HostFrameInput {
        now,
        visibility: Visibility::Focused,
        reduced_motion: false,
        serious: false,
        shed_envelope: 1.0,
        shed_active: false,
        pointer_px: None,
        capture: CaptureMode::Present,
        geometry,
    };
    assert_eq!(
        format!("{:?}", HostFrameInput::new(now, geometry)),
        format!("{plain:?}")
    );
    let every = HostFrameInput {
        now,
        visibility: Visibility::VisibleUnfocused,
        reduced_motion: true,
        serious: true,
        shed_envelope: 0.25,
        shed_active: true,
        pointer_px: Some((5.0, 6.0)),
        capture: CaptureMode::StaticCapture,
        geometry,
    };
    let built = HostFrameInput::new(now, geometry)
        .with_visibility(Visibility::VisibleUnfocused)
        .with_reduced_motion(true)
        .with_serious(true)
        .with_shed_envelope(0.25)
        .with_shed_active(true)
        .with_pointer_px(Some((5.0, 6.0)))
        .with_capture(CaptureMode::StaticCapture);
    assert_eq!(format!("{built:?}"), format!("{every:?}"));
    assert_ne!(
        format!("{plain:?}"),
        format!("{every:?}"),
        "control: the setters moved the input off its defaults"
    );
}

/// The reader against a real emulator: fresh state, OSC 133 `C` as `shell_executing`,
/// `D;<code>` as `cmd_done` with the completion seq.
#[test]
fn the_facts_reader_reports_what_the_emulator_said() {
    let mut term = Terminal::new(5, 20);
    let fresh = TerminalFacts::read(&term, 7, false);
    assert_eq!(fresh.session, 7);
    assert_eq!(fresh.caret, Some((0, 0)));
    assert!(fresh.cursor_visible && fresh.live_viewport);
    assert_eq!(fresh.display_offset, 0);
    assert!(!fresh.shell_executing && !fresh.alt_screen && !fresh.scrolled);
    assert_eq!(fresh.cmd_done, None);

    term.process(b"\x1b]133;A\x07\x1b]133;B\x07ls\r\n\x1b]133;C\x07");
    let executing = TerminalFacts::read(&term, 7, true);
    assert!(executing.shell_executing, "OSC 133 C is the Execute phase");
    assert!(
        executing.scrolled,
        "the host's scroll decision rides through"
    );
    assert!(
        executing.content_seq > fresh.content_seq,
        "output moved the content clock"
    );

    term.process(b"out\r\n\x1b]133;D;3\x07");
    let done = TerminalFacts::read(&term, 7, false);
    assert!(!done.shell_executing);
    let (seq, code, _dur) = done.cmd_done.expect("a completed command");
    assert_eq!(code, 3, "the exit code the shell reported");
    assert_eq!(seq, term.completed_command_seq());
    assert!(seq >= 1, "the completion seq counts once per D");
}

/// DECTCEM hides the caret without withdrawing it; scrolling into history withdraws it.
#[test]
fn a_hidden_cursor_keeps_its_caret_and_history_withdraws_it() {
    let mut term = Terminal::new(5, 20);
    term.process(b"\x1b[3;7H\x1b[?25l");
    let hidden = TerminalFacts::read(&term, 1, false);
    assert_eq!(hidden.caret, Some((2, 6)), "a hidden caret still travels");
    assert!(!hidden.cursor_visible, "…and is reported unpainted");
    assert!(hidden.live_viewport, "hidden is not scrolled");

    for _ in 0..12 {
        term.process(b"line\r\n");
    }
    term.scroll_display(2);
    let history = TerminalFacts::read(&term, 1, false);
    assert!(!history.live_viewport);
    assert_eq!(history.caret, None, "history withdraws the caret");

    term.scroll_display(-2);
    term.process(b"\x1b[?25h\x1b[?1049h");
    let alt = TerminalFacts::read(&term, 1, false);
    assert!(alt.alt_screen);
    assert!(alt.caret.is_some() && alt.cursor_visible);
}

/// A hidden cursor on an unfinished frame is no motion caret, also after the terminal's
/// timeout force-clears the mode.
#[test]
fn a_hidden_cursor_on_an_unfinished_frame_is_no_caret_for_the_motion_engines() {
    use aterm_core::terminal::ClockReading;
    let mut term = Terminal::new(10, 40);
    let t0 = std::time::Instant::now();
    let at = |ms: u64| ClockReading {
        monotonic: t0 + std::time::Duration::from_millis(ms),
        wall_ms: None,
    };
    term.process_at(b"\x1b[8;5H", at(0));
    let read = |term: &Terminal| {
        let f = TerminalFacts::read(term, 1, false);
        assert_eq!(f.frame_unfinished, term.sync_frame_unfinished());
        (
            f.caret,
            motion_caret(f.caret, f.cursor_visible, hidden_caret_withheld(term)),
        )
    };
    assert_eq!(read(&term), (Some((7, 4)), Some((7, 4))), "a shown caret");

    term.process_at(b"\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[2Btranscript", at(10));
    let (facts, motion) = read(&term);
    assert_eq!(facts, Some((2, 10)), "the pet still reads the cursor");
    assert_eq!(motion, None, "the write position is no caret");

    term.process_at(b"\r\x1b[3Bmore", at(1_200));
    assert!(
        !term.modes().synchronized_output(),
        "control: the mode level reads closed"
    );
    let (facts, motion) = read(&term);
    assert_eq!(facts, Some((5, 4)), "the pet still reads the cursor");
    assert_eq!(
        motion, None,
        "a timed-out frame's write position is no caret"
    );

    term.process_at(b"\x1b[?25h", at(1_210));
    assert_eq!(read(&term), (Some((5, 4)), Some((5, 4))));

    term.process_at(b"\x1b[?25l\x1b[8;1Hprompt\x1b[8;5H\x1b[?2026l", at(1_220));
    assert_eq!(read(&term), (Some((7, 4)), Some((7, 4))));

    term.process_at(
        b"\x1b[9;1Hrow\x1b[?2026h\x1b[9;2H\x1b[?2026l\x1b[?2026h",
        at(1_230),
    );
    assert!(term.modes().synchronized_output());
    assert_eq!(read(&term), (Some((8, 1)), Some((8, 1))));

    assert_eq!(motion_caret(None, true, false), None);
}

/// Claude Code's mid-line echo torn before its parking CUP: the print run
/// ends at the reprinted tail's end, a write position, so no anchor is
/// handed over.
#[test]
fn an_unfinished_frames_print_anchor_is_no_echo_for_the_motion_engines() {
    let mut term = Terminal::new(10, 40);
    term.process(b"\x1b[8;1H> the tail.\x1b[8;6H");
    let finished = motion_print_anchor(&term);
    assert!(
        finished.0.is_some(),
        "a finished frame hands its anchor over"
    );
    assert_eq!(finished, (term.print_anchor(), term.print_anchor_glyph()));

    term.process(b"\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[6C\x1b[7B the tail.");
    assert!(term.sync_frame_unfinished());
    assert!(
        term.print_anchor().is_some(),
        "control: the run ended somewhere"
    );
    assert_eq!(motion_print_anchor(&term), (None, None), "a write position");

    term.process(b"\x1b[10;1H\x1b[8;7H\x1b[?25h\x1b[?2026l");
    let after = motion_print_anchor(&term);
    assert_eq!(after, (term.print_anchor(), term.print_anchor_glyph()));
    assert!(
        after
            .0
            .is_some_and(|(_, _, seq)| finished.0.is_some_and(|(_, _, s0)| seq > s0))
    );

    term.process(b"\x1b[?2026h");
    assert_eq!(motion_print_anchor(&term), after);

    term.process(b"\x1b[9;1H$ ls");
    assert!(term.sync_frame_unfinished() && term.cursor_visible());
    let drawn = motion_print_anchor(&term);
    assert_eq!(drawn, (term.print_anchor(), term.print_anchor_glyph()));
    assert!(drawn.0.is_some_and(|(row, _, _)| row == 8));
}

/// Claude Code's question dialog parks its hidden cursor at column 0 of the blank bottom row:
/// no caret there.
#[test]
fn a_hidden_cursor_on_a_blank_row_is_no_caret_for_the_motion_engines() {
    let mut term = Terminal::new(12, 40);
    term.process(b"\x1b[?25l\x1b[5;1H\xe2\x80\xba 3. Type something.");
    term.process(b"\x1b[7;1H  4. Chat about this\x1b[5;1H");
    let read = |term: &Terminal| {
        let f = TerminalFacts::read(term, 1, false);
        (
            f.caret,
            motion_caret(f.caret, f.cursor_visible, hidden_caret_withheld(term)),
        )
    };
    assert_eq!(
        read(&term),
        (Some((4, 0)), Some((4, 0))),
        "the option's row"
    );

    term.process(b"\x1b[12;1H");
    let (facts, motion) = read(&term);
    assert_eq!(facts, Some((11, 0)), "the pet still reads the cursor");
    assert_eq!(motion, None, "a hidden cursor on a blank row is no caret");

    term.process(b"\x1b[?25h");
    assert_eq!(read(&term), (Some((11, 0)), Some((11, 0))));

    term.process(b"\x1b[?25l\x1b[12;1H\x1b[2m    \x1b[38;5;244m    \x1b[0m\x1b[12;1H");
    assert_eq!(read(&term).1, None, "invisible styling is no mark");
    term.process(b"\x1b[12;1H\x1b[73m    \x1b[74m    \x1b[0m\x1b[12;1H");
    assert_eq!(
        read(&term).1,
        None,
        "a super- or subscripted space is no overline"
    );
    term.process(b"\x1b[12;1H\x1b[53m \x1b[0m\x1b[12;1H");
    assert_eq!(
        read(&term),
        (Some((11, 0)), Some((11, 0))),
        "an overline is a mark"
    );
    term.process(b"\x1b[12;1H\x1b[2K");

    term.process(b"\x1b[?25l\x1b[12;1H\x1b[7m \x1b[27m\x1b[12;1H");
    assert_eq!(read(&term), (Some((11, 0)), Some((11, 0))));
}
