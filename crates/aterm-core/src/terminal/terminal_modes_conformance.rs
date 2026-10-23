// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Conformance tests for `TerminalModes` ([`aterm_models_core::terminal_modes_model`]).

use std::collections::BTreeMap;

use aterm_models_core::terminal_modes_model;
use aterm_spec::derive::Model;
use aterm_spec::xref::StepEvidence;
use aterm_spec::{interp, verify};
use aterm_types::CursorStyle;

use super::{ClockReading, MouseEncoding, MouseMode, Terminal};

type State = BTreeMap<&'static str, i64>;

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
    ("SetSgrMouseEncoding", b"\x1b[?1006h"),
    ("ResetSgrMouseEncoding", b"\x1b[?1005h"),
    ("SetSgrMouseEncoding", b"\x1b[?1006h"),
    ("ResetSgrMouseEncoding", b"\x1b[?1015h"),
    ("SetSgrMouseEncoding", b"\x1b[?1006h"),
    ("ResetSgrMouseEncoding", b"\x1b[?1016h"),
    ("SetCursorStyle", b"\x1b[2 q"),
    ("SetCursorStyle", b"\x1b[?12h"),
    ("RestoreCursorStyle", b"\x1b[6 q"),
    ("SetCursorStyle", b"\x1b[5 q"),
    ("RestoreCursorStyle", b"\x1b[?12l"),
    ("SetCursorStyle", b"\x1b[0 q"),
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
    ("SetBracketedPaste", b"\x1b[?2004h"),
];

fn fixture() -> (Terminal, ClockReading) {
    let mut term = Terminal::new(24, 80);
    term.set_default_cursor_style(HOST_STYLE);
    let clock = ClockReading {
        monotonic: aterm_time::Instant::now(), // captured once, reused so deltas are zero
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
