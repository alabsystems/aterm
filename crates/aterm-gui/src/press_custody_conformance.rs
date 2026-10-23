// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 trace conformance for `PressCustody` (who owns the reading position and the highlight), bound to the shipping code.

#![cfg(test)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{CustodyTransition, Terminal};
use aterm_models_misc::press_custody_model;
use aterm_spec::xref::StepEvidence;

use crate::input::{InputEvent, ScrollIntent, Source};
use crate::{App, WindowId, term_lock};

/// The spec variables, in the model's declared order.
const VARS: [&str; 7] = [
    "owner",
    "offset",
    "selection",
    "prev_owner",
    "prev_offset",
    "prev_selection",
    "last_event",
];

/// The observed projection `[owner, offset, selection]` of a real `Terminal`, named by every `#[refines(project = …)]` on the `PressCustody` seams.
pub(crate) fn project_press_custody(term: &Terminal) -> [i64; 3] {
    let offset = i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX);
    [
        i64::from(offset > 0),
        offset,
        i64::from(term.text_selection().has_selection()),
    ]
}

/// One fixture's running abstract state: the observed projection plus the trace
struct Press {
    /// `(prev_owner, prev_offset, prev_selection)` — the harness's own projection of
    shadow: (i64, i64, i64),
    /// The tag of the transition the engine last recorded.
    last_event: i64,
}

impl Press {
    fn new() -> Self {
        Self {
            shadow: (0, 0, 0),
            last_event: 0,
        }
    }

    /// The full 7-variable state.
    fn state(&self, term: &Terminal) -> [i64; 7] {
        let [owner, offset, selection] = project_press_custody(term);
        [
            owner,
            offset,
            selection,
            self.shadow.0,
            self.shadow.1,
            self.shadow.2,
            self.last_event,
        ]
    }

    /// Record what the step just fired: the model writes the pre-state into the
    fn fired(&mut self, prev: [i64; 7], recorded: CustodyTransition) {
        self.shadow = (prev[0], prev[1], prev[2]);
        self.last_event = recorded.last_event();
    }
}

fn as_state(s: [i64; 7]) -> BTreeMap<&'static str, i64> {
    VARS.iter().copied().zip(s).collect()
}

/// Validate one real transition against the derived `PressCustody` spec (`Buggy` = 0).
fn validate_transition(action: &str, prev: [i64; 7], next: [i64; 7]) -> (bool, String) {
    let model = press_custody_model();
    let (refines, mut why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &as_state(prev),
        &as_state(next),
        Some(action),
        "PressCustody Tier-1 conformance",
    );
    let mut holds = true;
    for (label, observed) in [("prev", prev), ("next", next)] {
        let state = as_state(observed);
        let env = model.eval_env(&state);
        for inv in &model.invariants {
            if !model.check_invariant_in(inv, &env) {
                holds = false;
                why.push_str(&format!(
                    "\ninvariant {} VIOLATED by the {label} state {observed:?}",
                    inv.name
                ));
            }
        }
    }
    (refines && holds, why)
}

/// Clear the custody record so the step that follows must write one (claim 1): a seam
fn arm(term: &Arc<Mutex<Terminal>>) {
    let _ = term_lock(term).take_custody_transition();
}

/// Read back what the step recorded; it must be `Some(expect)`.
fn recorded(
    term: &Arc<Mutex<Terminal>>,
    expect: CustodyTransition,
    what: &str,
) -> CustodyTransition {
    let got = term_lock(term).take_custody_transition();
    assert_eq!(
        got,
        Some(expect),
        "{what}: the shipping seam must RECORD {expect:?}. The record was cleared \
         immediately before this step, so `None` means either that the seam records \
         nothing at all, or that it DECLINED because the state change it observed is \
         not the one this action describes — an output re-pin that did not move the \
         offset declines rather than claim `OutputWhileReading`. A different variant \
         means it classified the event wrongly, which for the output kinds includes \
         having destroyed a highlight it was not entitled to destroy."
    );
    expect
}

/// Drive one real step and validate it against the action the engine recorded.
fn step(
    term: &Arc<Mutex<Terminal>>,
    c: &mut Press,
    expect: CustodyTransition,
    what: &str,
    gesture: impl FnOnce(),
    validated: &mut usize,
    ev: &mut StepEvidence,
) -> [i64; 7] {
    let prev = c.state(&term_lock(term));
    arm(term);
    ev.step(expect.action(), gesture);
    let got = recorded(term, expect, what);
    c.fired(prev, got);
    let next = c.state(&term_lock(term));
    let (ok, out) = validate_transition(got.action(), prev, next);
    assert!(
        ok,
        "{what}: real {} {prev:?} -> {next:?} must conform\n--- ty ---\n{out}",
        got.action()
    );
    *validated += 1;
    next
}

/// A terminal with `history` scrollback lines fed through `Terminal::process`, and the
fn engine_fixture(rows: u16, history: usize) -> Terminal {
    let mut term = Terminal::new(rows, 20);
    term.set_scrollback_line_limit(Some(history + 8));
    for i in 0..(usize::from(rows) + history) {
        term.process(format!("line{i}\r\n").as_bytes());
    }
    assert!(
        term.grid().scrollback_lines() >= history,
        "fixture must retain at least {history} scrollback line(s)"
    );
    assert_eq!(
        term.grid().display_offset(),
        0,
        "a freshly-fed fixture sits at live"
    );
    term
}

/// Arm a completed selection over live-screen rows `first..=last`, in the coordinates
fn arm_selection(term: &mut Terminal, first: i32, last: i32) {
    let sel = term.text_selection_mut();
    sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(last, 5, SelectionSide::Right);
    sel.complete_selection();
    assert!(sel.has_selection(), "the fixture selection must be alive");
}

/// `OutputDamagesTheSelectedRows`: one batch that both replaces the rows the selection
fn damaged_rows_case(validated: &mut usize, ev: &mut StepEvidence) {
    const DAMAGE: &[u8] = b"\x1b[1;1H\x1b[Knew text\x1b[6;1H\r\n";

    let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
    {
        let mut t = term_lock(&term);
        t.scroll_display(1);
        arm_selection(&mut t, 0, 1);
        assert_eq!(t.grid().display_offset(), 1, "the user is reading history");
    }
    let mut c = Press::new();
    let next = step(
        &term,
        &mut c,
        CustodyTransition::OutputDamagesTheSelectedRows,
        "output that REPLACED the selected rows while the user was reading",
        || {
            term_lock(&term).process(DAMAGE);
        },
        validated,
        ev,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [1, 2, 0],
        "the highlight over replaced text goes, the reading position does NOT"
    );

    let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
    {
        let mut t = term_lock(&term);
        arm_selection(&mut t, 0, 1);
        assert_eq!(
            t.grid().display_offset(),
            0,
            "the tail-follower owns the view"
        );
    }
    let mut c = Press::new();
    let next = step(
        &term,
        &mut c,
        CustodyTransition::OutputDamagesTheSelectedRows,
        "a status line REPLACING the rows a live highlight sits on",
        || {
            term_lock(&term).process(DAMAGE);
        },
        validated,
        ev,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [0, 0, 0],
        "the highlight over replaced text goes; at live there is no reading position \
         to ride and the view stays where it was"
    );
}

/// The in-place and unattributed takes:
fn in_place_and_unattributed_takes(validated: &mut usize, ev: &mut StepEvidence) {
    let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
    {
        let mut t = term_lock(&term);
        t.scroll_display(1);
        arm_selection(&mut t, 0, 1);
    }
    let mut c = Press::new();
    let next = step(
        &term,
        &mut c,
        CustodyTransition::OutputDamagesTheSelectedRowsInPlace,
        "an in-place rewrite (EL, no line feed) over the selected rows while reading",
        || {
            term_lock(&term).process(b"\x1b[1;1H\x1b[Knew text");
        },
        validated,
        ev,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [1, 1, 0],
        "the highlight over rewritten text goes; the reader does not move"
    );

    let floor = |read_top: bool| {
        let mut t = Terminal::new(4, 20);
        t.set_scrollback_line_limit(Some(2));
        for i in 0..8 {
            t.process(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(t.grid().scrollback_lines(), 2, "a two-line history floor");
        if read_top {
            t.scroll_display(2);
            assert_eq!(t.grid().display_offset(), 2, "parked at the top of history");
        }
        let sel = t.text_selection_mut();
        sel.start_selection(-2, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(-2, 5, SelectionSide::Right);
        sel.complete_selection();
        assert!(sel.has_selection(), "the oldest retained row is selected");
        Arc::new(Mutex::new(t))
    };
    for (read_top, expect, bytes, projected, what) in [
        (
            false,
            CustodyTransition::OutputTookTheSelectionUnattributed,
            &b"a\r\nb\r\nc\r\n"[..],
            [0, 0, 0],
            "output at live evicting the selected row off the history floor",
        ),
        (
            true,
            CustodyTransition::OutputTookTheSelectionUnattributedInPlace,
            &b"a\r\n"[..],
            [1, 2, 0],
            "output evicting the selected row under a reader parked at the floor",
        ),
    ] {
        let term = floor(read_top);
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            expect,
            what,
            || {
                term_lock(&term).process(bytes);
            },
            validated,
            ev,
        );
        assert_eq!([next[0], next[1], next[2]], projected, "{what}");
    }
}

/// Back toward live without typing. A one-row scroll down that stops short of the tail
fn toward_live_seams(validated: &mut usize, ev: &mut StepEvidence) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.copy_on_select = false;
    let term = app
        .front_terminal(wid)
        .expect("headless_for_test seeds one window with one terminal")
        .term
        .clone();
    {
        let mut t = term_lock(&term);
        let rows = t.grid().rows();
        for i in 0..(u32::from(rows) + 4) {
            t.process(format!("row{i}\r\n").as_bytes());
        }
    }
    let mut c = Press::new();
    let scroll = |app: &mut App, intent: ScrollIntent| {
        app.input(wid, InputEvent::ScrollView(intent), Source::Human);
    };
    for _ in 0..2 {
        step(
            &term,
            &mut c,
            CustodyTransition::UserScroll,
            "one-row scroll back into history",
            || scroll(&mut app, ScrollIntent::By(1)),
            validated,
            ev,
        );
    }
    step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "Select All, so the trip down has a highlight to keep",
        || app.select_all(),
        validated,
        ev,
    );
    let next = step(
        &term,
        &mut c,
        CustodyTransition::UserScrollTowardLive,
        "one row back down, stopping short of the tail",
        || scroll(&mut app, ScrollIntent::By(-1)),
        validated,
        ev,
    );
    assert_eq!(
        [next[0], next[1], next[2]],
        [1, 1, 1],
        "still the user's view"
    );
    for (what, gesture) in [
        (
            "End (ScrollIntent::Bottom)",
            Box::new(|app: &mut App| scroll(app, ScrollIntent::Bottom)) as Box<dyn Fn(&mut App)>,
        ),
        (
            "a page down that lands at the tail",
            Box::new(|app: &mut App| scroll(app, ScrollIntent::Down)),
        ),
        (
            "the ⌘-V / IME snap",
            Box::new(|app: &mut App| app.snap_to_bottom(wid)),
        ),
    ] {
        if c.state(&term_lock(&term))[1] == 0 {
            step(
                &term,
                &mut c,
                CustodyTransition::UserScroll,
                "one-row scroll back into history",
                || scroll(&mut app, ScrollIntent::By(1)),
                validated,
                ev,
            );
        }
        let next = step(
            &term,
            &mut c,
            CustodyTransition::SnapToLive,
            what,
            || gesture(&mut app),
            validated,
            ev,
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [0, 0, 1],
            "{what}: at live, tail-owned, and the highlight kept"
        );
    }
}

/// The GUI half: one real `App` and terminal, nine validated transitions through the
fn gui_gesture_chain(validated: &mut usize, ev: &mut StepEvidence) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.copy_on_select = false;
    let term = app
        .front_terminal(wid)
        .expect("headless_for_test seeds one window with one terminal")
        .term
        .clone();

    {
        let mut t = term_lock(&term);
        let rows = t.grid().rows();
        for i in 0..(u32::from(rows) + 4) {
            t.process(format!("row{i}\r\n").as_bytes());
        }
        assert!(
            t.grid().scrollback_lines() >= 4,
            "the fixture needs history to scroll into"
        );
        assert_eq!(t.grid().display_offset(), 0, "…and starts at live");
    }
    let mut c = Press::new();

    let init = press_custody_model().init_state();
    let model_init: [i64; 7] = VARS.map(|v| init[v]);
    assert_eq!(
        c.state(&term_lock(&term)),
        model_init,
        "a live, unselected terminal must project to PressCustody's Init"
    );
    assert_eq!(model_init, [0; 7], "sanity: Init is the all-zero state");

    step(
        &term,
        &mut c,
        CustodyTransition::UserScroll,
        "one-row scroll back into history",
        || {
            app.input(
                wid,
                InputEvent::ScrollView(ScrollIntent::By(1)),
                Source::Human,
            );
        },
        validated,
        ev,
    );

    let drag_from = |app: &mut App, from: (u16, u16)| {
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_cell = from;
        }
        app.begin_selection(wid, SelectionType::Simple);
    };
    step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "left press starting a drag",
        || drag_from(&mut app, (1, 0)),
        validated,
        ev,
    );

    step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "left release completing the drag",
        || {
            app.drag_selection(wid, 2, 5);
            let _ = app.finish_selection(wid, true);
        },
        validated,
        ev,
    );

    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let shift_left = || Key::Named(NamedKey::ShiftLeft);
    for (key, mods, event_type, expect, what) in [
        (
            shift_left(),
            Modifiers::SHIFT,
            KeyEventType::Release,
            CustodyTransition::ReleaseEvent,
            "a Shift key-UP (release AND inert modifier at once)",
        ),
        (
            shift_left(),
            Modifiers::SHIFT,
            KeyEventType::Repeat,
            CustodyTransition::RepeatPress,
            "a held Shift auto-repeating (repeat AND inert modifier at once)",
        ),
        (
            shift_left(),
            Modifiers::SHIFT,
            KeyEventType::Press,
            CustodyTransition::InertPress,
            "a bare Shift keydown — the first half of Cmd-C, classified by \
             `is_modifier_or_lock_key` and by nothing the harness said",
        ),
        (
            Key::Character('a'),
            Modifiers::empty(),
            KeyEventType::Press,
            CustodyTransition::TypingPress,
            "a byte-producing keydown — the ONE handover",
        ),
    ] {
        let before = c.state(&term_lock(&term));
        let after = step(
            &term,
            &mut c,
            expect,
            what,
            || {
                app.input(
                    wid,
                    InputEvent::Key {
                        key,
                        mods,
                        base_layout: None,
                        layout_text: None,
                        event_type,
                    },
                    Source::Human,
                );
            },
            validated,
            ev,
        );
        if expect == CustodyTransition::TypingPress {
            assert_eq!(
                [after[0], after[1], after[2]],
                [0, 0, 0],
                "typing lands at live, tail-owned, unselected"
            );
        } else {
            assert_eq!(
                [after[0], after[1], after[2]],
                [before[0], before[1], before[2]],
                "{what}: an inert press may move neither the viewport nor the highlight"
            );
            assert_eq!(
                [before[0], before[1], before[2]],
                [1, 1, 1],
                "{what}: it must be driven with something TO take — a reading \
                 position and a live highlight — or `inert` is free"
            );
        }
    }

    step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "left press with no drag",
        || drag_from(&mut app, (2, 0)),
        validated,
        ev,
    );
    let after_clear = step(
        &term,
        &mut c,
        CustodyTransition::UserClear,
        "left release inside the press cell",
        || {
            let _ = app.finish_selection(wid, true);
        },
        validated,
        ev,
    );
    assert_eq!(after_clear[2], 0, "a click without a drag deselects");
}

/// Seams that bring a selection into existence with no drag around them: Select All (a
fn silent_select_seams(validated: &mut usize, ev: &mut StepEvidence) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.copy_on_select = false;
    let term = app
        .front_terminal(wid)
        .expect("headless_for_test seeds one window with one terminal")
        .term
        .clone();
    {
        let mut t = term_lock(&term);
        let rows = t.grid().rows();
        for i in 0..(u32::from(rows) + 4) {
            t.process(format!("row{i}\r\n").as_bytes());
        }
        assert!(
            !t.text_selection().has_selection(),
            "the fixture starts with nothing selected"
        );
    }
    let mut c = Press::new();

    let after_all = step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "Select All from the menu, with no mouse-down before it",
        || app.select_all(),
        validated,
        ev,
    );
    assert_eq!(
        after_all[2], 1,
        "Select All really did bring a selection into existence"
    );

    let after_word = step(
        &term,
        &mut c,
        CustodyTransition::UserSelect,
        "the double-click PRESS that word-selects, before any release",
        || app.select_word_click(wid, 0, 1),
        validated,
        ev,
    );
    assert_eq!(
        after_word[2], 1,
        "the word-click press leaves a completed selection behind"
    );
}

/// The wheel, driven through both of `input_wheel`'s motion routes. The recorder lives
fn wheel_seams(validated: &mut usize, ev: &mut StepEvidence) {
    for (focused, what) in [
        (
            false,
            "a wheel notch on an unfocused window (the INSTANT motion arm)",
        ),
        (
            true,
            "a wheel notch settled out of its GLIDE (the eased motion arm)",
        ),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.copy_on_select = false;
        let term = app
            .front_terminal(wid)
            .expect("headless_for_test seeds one window with one terminal")
            .term
            .clone();
        {
            let mut t = term_lock(&term);
            let rows = t.grid().rows();
            for i in 0..(u32::from(rows) + 4) {
                t.process(format!("row{i}\r\n").as_bytes());
            }
            assert!(
                t.grid().scrollback_lines() >= 4,
                "the fixture needs history to scroll into"
            );
            assert_eq!(t.grid().display_offset(), 0, "…and starts at live");
        }
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.focused = focused;
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::UserScroll,
            what,
            || {
                app.input(
                    wid,
                    InputEvent::Wheel {
                        dir: aterm_types::mouse::WheelDir::Up,
                        lines: 1,
                        detent: false,
                        row: 0,
                        col: 0,
                        mods: 0,
                        px_off: crate::input::PixelOffset::CELL_ORIGIN,
                    },
                    Source::Human,
                );
                app.settle_scroll_motion_at_target(wid, std::time::Instant::now());
            },
            validated,
            ev,
        );
        assert_eq!(
            [next[0], next[1]],
            [1, 1],
            "{what}: the wheel really moved the engine's viewport, not just the record"
        );
    }
}

/// The output half: real `Terminal::process` batches, each on its own fixture so the
fn output_batches(validated: &mut usize, ev: &mut StepEvidence) {
    {
        let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
        {
            let mut t = term_lock(&term);
            arm_selection(&mut t, 0, 1);
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::OutputAtLive,
            "one line of output while the tail-follower owns the viewport",
            || {
                term_lock(&term).process(b"tail\r\n");
            },
            validated,
            ev,
        );
        assert!(
            term_lock(&term).text_selection().has_selection(),
            "a status bar repainting rows it never selected may not destroy the \
             highlight — the shipping defect, failing under its own name"
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [0, 0, 1],
            "output at live leaves the view at live AND spares an undamaged selection"
        );
    }

    {
        let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
        {
            let mut t = term_lock(&term);
            t.scroll_display(1);
            arm_selection(&mut t, 0, 1);
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::OutputWhileReading,
            "one line of output while the USER owns the viewport",
            || {
                term_lock(&term).process(b"more\r\n");
            },
            validated,
            ev,
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [1, 2, 1],
            "the offset RISES with the arriving line, ownership stays with the user, \
             AND a highlight the batch never touched survives"
        );
    }

    {
        let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
        {
            let mut t = term_lock(&term);
            t.scroll_display(1);
            arm_selection(&mut t, 0, 1);
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::OutputInvalidatesTheCoordinateSpace,
            "ED 3 erasing the scrollback the offset and the anchors named",
            || {
                term_lock(&term).process(b"\x1b[3J");
            },
            validated,
            ev,
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [0, 0, 0],
            "the space the offset named is gone, so the viewport goes back and the \
             selection cannot outlive it"
        );
    }

    {
        let term = Arc::new(Mutex::new(engine_fixture(6, 4)));
        {
            let mut t = term_lock(&term);
            t.scroll_display(1);
            arm_selection(&mut t, 0, 1);
        }
        let mut c = Press::new();
        let next = step(
            &term,
            &mut c,
            CustodyTransition::OutputInvalidatesTheCoordinateSpace,
            "RIS destroying the coordinate space",
            || {
                term_lock(&term).process(b"\x1bc");
            },
            validated,
            ev,
        );
        assert_eq!(
            [next[0], next[1], next[2]],
            [0, 0, 0],
            "same law, other route"
        );
    }
}

/// The `PressCustody` Tier-1 body, run by `spec_xref_gate` with its execution evidence.
pub(crate) fn run_conformance() -> StepEvidence {
    let mut validated = 0usize;
    let mut ev = StepEvidence::new("PressCustody");

    gui_gesture_chain(&mut validated, &mut ev);
    wheel_seams(&mut validated, &mut ev);
    silent_select_seams(&mut validated, &mut ev);
    toward_live_seams(&mut validated, &mut ev);
    let gui = validated;
    output_batches(&mut validated, &mut ev);
    damaged_rows_case(&mut validated, &mut ev);
    in_place_and_unattributed_takes(&mut validated, &mut ev);

    let read_low = [1, 1, 1, 0, 0, 0, 0];
    let read_high = [1, 1, 1, 1, 1, 1, 0];

    let rejected: &[(&str, [i64; 7], [i64; 7], &str)] = &[
        (
            "InertPress",
            read_low,
            [0, 0, 0, 1, 1, 1, 3],
            "a bare modifier expresses no intent and may disturb nothing",
        ),
        (
            "RepeatPress",
            read_low,
            [0, 0, 0, 1, 1, 1, 2],
            "the press already took custody; a tick of the same hold cannot take it twice",
        ),
        (
            "ReleaseEvent",
            read_low,
            [0, 0, 0, 1, 1, 1, 4],
            "a key-up is not typing",
        ),
        (
            "OutputWhileReading",
            read_low,
            [0, 0, 1, 1, 1, 1, 5],
            "output may never take the reading position",
        ),
        (
            "OutputAtLive",
            [0, 0, 1, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 1, 5],
            "a status bar repainting rows it never selected may not destroy the highlight",
        ),
        (
            "OutputDamagesTheSelectedRows",
            read_low,
            [0, 0, 0, 1, 1, 1, 6],
            "damaging the selected rows is not a licence to move the view",
        ),
        (
            "OutputInvalidatesTheCoordinateSpace",
            read_low,
            [0, 0, 1, 1, 1, 1, 7],
            "a selection may not outlive the space that gives its rows meaning",
        ),
        (
            "TypingPress",
            read_low,
            [1, 1, 0, 1, 1, 1, 1],
            "typing means take me to the prompt, so it lands at live or it is not typing",
        ),
        (
            "OutputDamagesTheSelectedRowsInPlace",
            read_low,
            [0, 0, 0, 1, 1, 1, 6],
            "an in-place rewrite is not a licence to move the view",
        ),
        (
            "OutputTookTheSelectionUnattributed",
            read_low,
            [0, 0, 0, 1, 1, 1, 6],
            "a fail-closed take is not a licence to move the view",
        ),
        (
            "OutputTookTheSelectionUnattributedInPlace",
            read_low,
            [0, 0, 0, 1, 1, 1, 6],
            "a fail-closed take in place is not a licence to move the view",
        ),
        (
            "SnapToLive",
            read_low,
            [0, 0, 0, 1, 1, 1, 8],
            "End or a ⌘-V snap brings the view home and leaves the highlight alone",
        ),
        (
            "UserScrollTowardLive",
            [1, 2, 1, 0, 0, 0, 0],
            [1, 1, 0, 1, 2, 1, 8],
            "a scroll down is not a deselect",
        ),
        (
            "UserScroll",
            [1, 2, 0, 0, 0, 0, 0],
            [1, 2, 0, 1, 2, 0, 0],
            "UserScroll is guarded `offset <= MaxOffset - 1` and cannot fire at the cap",
        ),
        (
            "UserScroll",
            [0, 0, 0, 0, 0, 0, 0],
            [0, 1, 0, 0, 0, 0, 0],
            "an offset above the tail means the USER owns the viewport",
        ),
        (
            "UserClear",
            read_high,
            [0, 0, 0, 1, 1, 1, 0],
            "a deselecting click is not a scroll",
        ),
        (
            "InertPress",
            [1, 7, 0, 0, 0, 0, 0],
            [1, 7, 0, 1, 7, 0, 3],
            "a projection that left `0..=MaxOffset` must be REJECTED, not clamped",
        ),
    ];
    for (action, prev, next, why) in rejected {
        let (ok, out) = validate_transition(action, *prev, *next);
        assert!(
            !ok,
            "NEGATIVE CONTROL ({action}) {prev:?} -> {next:?} MUST be rejected — {why}\
             \n--- ty ---\n{out}"
        );
    }

    eprintln!(
        "PressCustody Tier-1 conformance: {validated} real transitions strictly validated \
         against the derived spec ({gui} through the real App scroll/wheel/gesture/press \
         seams — every press class delivered as a real key event, and the wheel driven on \
         both of `input_wheel`'s motion routes — and {} through real `Terminal::process` \
         batches), every one of them named by the ENGINE's own custody record rather than \
         by the harness, and {} negative controls (inert press disturbs, repeat disturbs, \
         release disturbs, output snaps, output takes an undamaged highlight, damaging \
         output snaps — in place and unattributed too — a return to live that deselects, \
         invalidation leaves a dangling selection, typing without a snap, \
         scroll past the cap, a scroll that does not take ownership, a deselect that \
         scrolls, a projection that left `0..=MaxOffset`) all rejected — the last of those \
         rejectable only by the invariant evaluation this module performs on top of the \
         refinement check.",
        validated - gui,
        rejected.len()
    );

    ev
}
