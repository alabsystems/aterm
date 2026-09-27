// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 bind for `TerminalModes` (`aterm_spec::derive::terminal_modes_model`):
//! drive every `#[refines(machine = "terminal_modes", …)]` action through REAL
//! `Terminal::process_at` bytes, project the real [`TerminalModes`] onto the
//! model with [`project_modes`], and validate each step against the model (the
//! in-process interpreter always, `ty trace validate` wherever installed).
//!
//! It lives in aterm-core's own unit-test build because that is where the
//! anchors expand (`cfg_attr(any(test, feature = "spec-anchors"), …)`), so each
//! step also runs inside a [`StepEvidence`] window and the audit proves the
//! window entered the action's OWN anchored function — every site of it, for
//! the multi-site actions (the four mouse-tracking enables, the alt-screen
//! entries and exits of modes 47/1047/1049, DECSCUSR and mode 12 for both
//! cursor-shape actions, and the three encodings that replace SGR 1006).
//!
//! Every action sets its own variable to an exact value, so a handler that
//! entered its function and did nothing is a step the model rejects, not only
//! one that disturbed some other mode.
//!
//! The host's default cursor shape is set to a NON-factory style first, so the
//! cursor-shape steps and both resets are checked against the HOST's shape
//! rather than `CursorStyle::default()`.
//!
//! NEGATIVE CONTROL: the model's defect is a DECSTR that forgets the
//! synchronized-output hold (a frozen screen). From a real held state, that
//! step is rejected by the committed model and admitted by `Buggy = 1`.

use std::collections::BTreeMap;

use aterm_spec::derive::{Model, terminal_modes_model};
use aterm_spec::xref::StepEvidence;
use aterm_spec::{interp, verify};
use aterm_types::CursorStyle;

use super::{ClockReading, MouseEncoding, MouseMode, Terminal};

type State = BTreeMap<&'static str, i64>;

/// The host's configured default cursor shape for this run — deliberately not
/// `CursorStyle::default()`.
const HOST_STYLE: CursorStyle = CursorStyle::SteadyBar;

/// Project the real terminal onto `TerminalModes`' variables. `reset` is the
/// model's history variable: whether the step that produced this state was a
/// reset (it is the only variable not read off the terminal).
pub(crate) fn project_modes(term: &Terminal, reset: bool) -> State {
    let m = term.modes();
    [
        ("app_cursor_keys", m.application_cursor_keys),
        ("origin_mode", m.origin_mode),
        ("auto_wrap", m.auto_wrap),
        ("cursor_visible", m.cursor_visible),
        ("focus_reporting", m.focus_reporting),
        ("sync_output", m.synchronized_output),
        ("insert_mode", m.insert_mode),
        ("new_line_mode", m.new_line_mode),
        ("alt_screen", m.alternate_screen),
        ("bracketed_paste", m.bracketed_paste),
        ("sgr_mouse", m.mouse_encoding == MouseEncoding::Sgr),
        ("mouse_mode", m.mouse_mode != MouseMode::None),
        ("cursor_style", m.cursor_style != term.default_cursor_style),
        ("reset", reset),
    ]
    .into_iter()
    .map(|(name, fact)| (name, i64::from(fact)))
    .collect()
}

/// Every step: the model action it drives and the real bytes that drive it.
/// Ordered so each toggle is seen both ways, and so the resets meet a terminal
/// with every restorable mode moved off its default.
const STEPS: &[(&str, &[u8])] = &[
    ("SetApplicationCursorKeys", b"\x1b[?1h"),
    ("ResetApplicationCursorKeys", b"\x1b[?1l"),
    ("SetOriginMode", b"\x1b[?6h"),
    ("ResetOriginMode", b"\x1b[?6l"),
    ("ResetAutoWrap", b"\x1b[?7l"),
    ("SetAutoWrap", b"\x1b[?7h"),
    ("ResetCursorVisible", b"\x1b[?25l"),
    ("SetCursorVisible", b"\x1b[?25h"),
    ("SetFocusReporting", b"\x1b[?1004h"),
    ("ResetFocusReporting", b"\x1b[?1004l"),
    ("SetSynchronizedOutput", b"\x1b[?2026h"),
    ("ResetSynchronizedOutput", b"\x1b[?2026l"),
    ("SetInsertMode", b"\x1b[4h"),
    ("ResetInsertMode", b"\x1b[4l"),
    ("SetNewLineMode", b"\x1b[20h"),
    ("ResetNewLineMode", b"\x1b[20l"),
    ("SetAlternateScreen", b"\x1b[?1049h"),
    ("ResetAlternateScreen", b"\x1b[?1049l"),
    ("SetAlternateScreen", b"\x1b[?47h"),
    ("ResetAlternateScreen", b"\x1b[?47l"),
    ("SetAlternateScreen", b"\x1b[?1047h"),
    ("ResetAlternateScreen", b"\x1b[?1047l"),
    ("SetBracketedPaste", b"\x1b[?2004h"),
    ("ResetBracketedPaste", b"\x1b[?2004l"),
    ("SetMouseMode", b"\x1b[?9h"),
    ("ResetMouseMode", b"\x1b[?9l"),
    ("SetMouseMode", b"\x1b[?1000h"),
    ("SetMouseMode", b"\x1b[?1002h"),
    ("SetMouseMode", b"\x1b[?1003h"),
    ("ResetMouseMode", b"\x1b[?1003l"),
    ("SetSgrMouseEncoding", b"\x1b[?1006h"),
    ("ResetSgrMouseEncoding", b"\x1b[?1006l"),
    // Every other encoding REPLACES SGR 1006.
    ("SetSgrMouseEncoding", b"\x1b[?1006h"),
    ("ResetSgrMouseEncoding", b"\x1b[?1005h"),
    ("SetSgrMouseEncoding", b"\x1b[?1006h"),
    ("ResetSgrMouseEncoding", b"\x1b[?1015h"),
    ("SetSgrMouseEncoding", b"\x1b[?1006h"),
    ("ResetSgrMouseEncoding", b"\x1b[?1016h"),
    // The host's shape is a steady bar: DECSCUSR and mode 12 each land on it
    // and off it.
    ("SetCursorStyle", b"\x1b[2 q"),
    ("SetCursorStyle", b"\x1b[?12h"),
    ("RestoreCursorStyle", b"\x1b[6 q"),
    ("SetCursorStyle", b"\x1b[5 q"),
    ("RestoreCursorStyle", b"\x1b[?12l"),
    ("SetCursorStyle", b"\x1b[0 q"),
    // Move every restorable mode off its default, inside the alternate screen,
    // then soft-reset: DECSTR must restore all of them and KEEP the screen.
    ("SetAlternateScreen", b"\x1b[?1049h"),
    ("SetApplicationCursorKeys", b"\x1b[?1h"),
    ("SetOriginMode", b"\x1b[?6h"),
    ("ResetAutoWrap", b"\x1b[?7l"),
    ("ResetCursorVisible", b"\x1b[?25l"),
    ("SetFocusReporting", b"\x1b[?1004h"),
    ("SetSynchronizedOutput", b"\x1b[?2026h"),
    ("SetInsertMode", b"\x1b[4h"),
    ("SetNewLineMode", b"\x1b[20h"),
    ("SetBracketedPaste", b"\x1b[?2004h"),
    ("SetMouseMode", b"\x1b[?1002h"),
    ("SetSgrMouseEncoding", b"\x1b[?1006h"),
    ("SetCursorStyle", b"\x1b[3 q"),
    ("SoftReset", b"\x1b[!p"),
    // Again for RIS, which must also leave the alternate screen.
    ("SetApplicationCursorKeys", b"\x1b[?1h"),
    ("SetOriginMode", b"\x1b[?6h"),
    ("ResetAutoWrap", b"\x1b[?7l"),
    ("ResetCursorVisible", b"\x1b[?25l"),
    ("SetFocusReporting", b"\x1b[?1004h"),
    ("SetSynchronizedOutput", b"\x1b[?2026h"),
    ("SetInsertMode", b"\x1b[4h"),
    ("SetNewLineMode", b"\x1b[20h"),
    ("SetBracketedPaste", b"\x1b[?2004h"),
    ("SetMouseMode", b"\x1b[?1000h"),
    ("SetSgrMouseEncoding", b"\x1b[?1006h"),
    ("SetCursorStyle", b"\x1b[4 q"),
    ("FullReset", b"\x1bc"),
    // A mode change after a reset closes the reset window.
    ("SetBracketedPaste", b"\x1b[?2004h"),
];

/// A terminal with the host's non-factory default shape installed, and the
/// fixed batch clock every step runs under (so no mode-2026 timeout can fire
/// between steps while `ty` validates the previous one).
fn fixture() -> (Terminal, ClockReading) {
    let mut term = Terminal::new(24, 80);
    term.set_default_cursor_style(HOST_STYLE);
    let clock = ClockReading {
        monotonic: aterm_time::Instant::now(), // CLOCK-EXEMPT: captured once, reused so deltas are zero
        wall_ms: Some(0),
    };
    (term, clock)
}

fn conforms(model: &Model, prev: &State, next: &State, action: &str) -> (bool, String) {
    verify::validate_transition_tiered(
        model,
        &[],
        prev,
        next,
        Some(action),
        "terminal_modes conformance",
    )
}

#[test]
fn real_mode_handlers_conform_to_terminal_modes_model() {
    let model = terminal_modes_model();
    let (mut term, clock) = fixture();
    let mut state = project_modes(&term, false);
    assert_eq!(
        state,
        model.init_state(),
        "a fresh terminal is the model's Init"
    );

    let mut ev = StepEvidence::new("terminal_modes");
    for &(action, bytes) in STEPS {
        ev.step(action, || term.process_at(bytes, clock));
        let next = project_modes(&term, matches!(action, "SoftReset" | "FullReset"));
        let (ok, why) = conforms(&model, &state, &next, action);
        assert!(
            ok,
            "{:?} is not the model's {action}: {state:?} -> {next:?}\n{why}",
            String::from_utf8_lossy(bytes)
        );
        assert!(
            model.check_invariant("ResetRestoresDefaults", &next),
            "{action} left the terminal unusable: {next:?}"
        );
        state = next;
    }

    let audit = ev.audit();
    assert!(
        audit.is_complete(),
        "terminal_modes step evidence incomplete: {audit:?}"
    );
    assert!(
        audit.dark_sites.is_empty(),
        "anchor sites no window of their action entered: {:?}",
        audit.dark_sites
    );
    let driven: std::collections::BTreeSet<&str> = STEPS.iter().map(|(a, _)| *a).collect();
    let modelled: std::collections::BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(driven, modelled, "every model action was driven");
}

/// The modelled defect from a REAL held state: DECSTR that keeps the
/// synchronized-output hold.
#[test]
fn a_soft_reset_that_keeps_the_sync_hold_is_the_buggy_step() {
    let model = terminal_modes_model();
    let (mut term, clock) = fixture();
    term.process_at(b"\x1b[?2026h", clock);
    let held = project_modes(&term, false);
    assert_eq!(held["sync_output"], 1, "the hold is real");

    term.process_at(b"\x1b[!p", clock);
    let real = project_modes(&term, true);
    let mut stuck = real.clone();
    stuck.insert("sync_output", 1);

    assert!(conforms(&model, &held, &real, "SoftReset").0);
    assert!(
        !conforms(&model, &held, &stuck, "SoftReset").0,
        "the committed model must reject a soft reset that keeps the hold"
    );
    assert!(
        interp::with_buggy(&model, 1)
            .successors("SoftReset", &held)
            .contains(&stuck),
        "Buggy = 1 admits exactly this stuck hold"
    );
    assert!(!model.check_invariant("ResetRestoresDefaults", &stuck));
}
