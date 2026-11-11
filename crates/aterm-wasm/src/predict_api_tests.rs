// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use crate::WebTerminal;

fn terminal() -> WebTerminal {
    const BACK_MS: u64 = 10_000;
    const _: () = assert!(
        BACK_MS > aterm_predict::compute_glitch_window_ms(Some(u32::MAX), u8::MAX),
        "the pin is further back than any glitch window"
    );
    let past = aterm_time::Instant::now()
        .checked_sub(aterm_time::Duration::from_millis(BACK_MS))
        .expect("the monotonic clock is more than ten seconds old");
    let mut term = crate::system_font_terminal(4, 20, 16.0);
    term.pred_row_scratch.set_clock(Some(past));
    term
}

#[test]
fn off_by_default_and_unknown_modes_fail_safe() {
    let mut t = terminal();
    assert!(!t.predict_char('a'), "default off ⇒ inert");
    t.set_predictive_echo("nonsense");
    assert!(!t.predict_char('a'), "unknown mode parses to off");
    assert!(t.predict_next_deadline_ms().is_none(), "nothing armed");
}

#[test]
fn always_tracks_paints_and_retires_through_the_export() {
    let mut t = terminal();
    t.set_predictive_echo("always");
    assert!(t.predict_char('a'));
    assert_eq!(t.predict_overlay(), vec![0, 0, u32::from('a')]);
    assert!(t.predict_next_deadline_ms().is_some(), "glitch timer armed");
    t.process(b"a");
    t.predict_reconcile();
    assert!(t.predict_overlay().is_empty());
    assert!(t.predict_next_deadline_ms().is_none());
}

#[test]
fn an_edit_key_is_armed_and_its_cursor_handed_over() {
    let mut t = terminal();
    t.set_predictive_echo("always");
    for ch in ['a', 'b'] {
        assert!(t.predict_char(ch));
        t.process(ch.to_string().as_bytes());
        t.predict_reconcile();
    }
    assert_eq!(t.predict_edit_key(0), 0, "an unknown code is ignored");
    assert_eq!(t.predict_edit_key(1), 1, "`←` armed on the proven line");
    assert!(t.predict_overlay().is_empty(), "a motion paints no cell");
    assert_eq!(t.predict_overlay_cursor(), vec![0, 1]);
    t.process(b"\x08");
    t.predict_reconcile();
    assert!(t.predict_overlay().is_empty());
    assert!(t.predict_overlay_cursor().is_empty(), "its echo retired it");
    assert_eq!(t.predict_edit_key(2), 1, "`→` back to the end");
    t.process(b"\x1b[C");
    t.predict_reconcile();
    assert_eq!(t.predict_edit_key(3), 1, "an erase at the end of the line");
    assert_eq!(t.predict_overlay(), vec![0, 1, u32::from(' ')]);
    assert_eq!(t.predict_overlay_cursor(), vec![0, 1]);
    assert!(
        !t.predict_backspace(),
        "a second erase arms; it cancels nothing"
    );
    t.set_predictive_echo("off");
    assert!(t.predict_overlay_cursor().is_empty());
    assert_eq!(t.predict_edit_key(1), 0);
    assert!(t.predict_next_deadline_ms().is_none());
}

#[test]
fn a_shift_s_moved_text_comes_out_as_triples() {
    let mut t = terminal();
    t.set_predictive_echo("always");
    for ch in ['a', 'b', 'c'] {
        assert!(t.predict_char(ch));
        t.process(ch.to_string().as_bytes());
        t.predict_reconcile();
    }
    for _ in 0..2 {
        assert_eq!(t.predict_edit_key(1), 1, "`←` armed");
        t.process(b"\x08");
        t.predict_reconcile();
    }
    assert!(t.predict_char('x'), "a shift, armed");
    assert_eq!(
        t.predict_overlay(),
        vec![
            0,
            1,
            u32::from('x'),
            0,
            2,
            u32::from('b'),
            0,
            3,
            u32::from('c')
        ]
    );
    assert_eq!(t.predict_overlay_cursor(), vec![0, 2]);
    t.process(b"xbc\x08\x08");
    t.predict_reconcile();
    assert!(t.predict_overlay().is_empty(), "its echo retired it");
    assert!(t.predict_overlay_cursor().is_empty());
}

#[test]
fn adaptive_tracks_but_never_shows_an_unconfirmed_line() {
    let mut t = terminal();
    t.set_predictive_echo("adaptive");
    assert!(t.predict_char('a'), "tracked, awaiting the echo");
    assert!(
        t.predict_overlay().is_empty(),
        "no echo confirmed on this line yet ⇒ nothing painted (password safety)"
    );
}

#[test]
fn adaptive_confirmed_echo_retires_the_prediction() {
    let mut t = terminal();
    t.set_predictive_echo("adaptive");
    assert!(t.predict_char('a'));
    assert!(t.predict_next_deadline_ms().is_some());
    t.process(b"a");
    t.predict_reconcile();
    assert!(
        t.predict_next_deadline_ms().is_none(),
        "confirmed echo retires the tracked prediction"
    );
}

#[test]
fn codex_report_event_types_suppresses_arm_reconcile_and_display() {
    let mut t = terminal();
    t.set_predictive_echo("always");

    assert!(t.predict_char('a'));
    t.process(b"\x1b[>7u");
    t.predict_reconcile();
    assert!(t.predict_overlay().is_empty());
    assert!(t.predict_next_deadline_ms().is_none());

    t.process(b"\x1b[=0u");
    assert!(t.predict_char('b'));
    t.process(b"\x1b[>7u");
    assert!(t.predict_overlay().is_empty());
    assert!(t.predict_next_deadline_ms().is_none());

    assert!(!t.predict_char('c'));
    assert!(!t.predict_backspace());
    assert!(t.predict_next_deadline_ms().is_none());
}

#[test]
fn line_submit_ends_the_epoch_no_same_row_leak() {
    let mut t = terminal();
    t.set_predictive_echo("adaptive");
    assert!(t.predict_char('a'));
    t.process(b"a");
    t.predict_reconcile();
    assert!(t.predict_char('b'));
    assert!(
        t.predict_next_deadline_ms().is_some(),
        "the next guess is tracked before submit"
    );
    t.predict_line_submit();
    assert!(
        t.predict_overlay().is_empty(),
        "a fast link paints nothing, before or after the submit"
    );
    assert!(
        !t.predict_char('s'),
        "the password keystroke behind a submitted line in flight is declined…"
    );
    assert!(
        t.predict_overlay().is_empty(),
        "…and never displayed on the unconfirmed new line (no leak)"
    );
}

#[test]
fn only_a_line_terminator_carries_the_guesses_in_flight() {
    let mut t = terminal();
    t.set_predictive_echo("always");
    assert!(t.predict_char('a'));
    t.predict_line_submit();
    assert_eq!(
        t.predict_overlay(),
        vec![0, 0, u32::from('a')],
        "Enter keeps the tail on glass"
    );
    t.predict_unmodelled_input();
    assert!(
        t.predict_overlay().is_empty(),
        "a key that may have erased the tail retires it"
    );
    assert!(t.predict_next_deadline_ms().is_none(), "nothing left armed");
}

#[test]
fn divergence_and_alt_screen_flush() {
    let mut t = terminal();
    t.set_predictive_echo("always");
    t.predict_char('a');
    t.process(b"*");
    t.predict_reconcile();
    assert!(t.predict_overlay().is_empty(), "divergence flushes the set");
    t.predict_char('x');
    t.process(b"\x1b[?1049h");
    t.predict_reconcile();
    assert!(t.predict_overlay().is_empty(), "alt screen refuses guesses");
    assert!(t.predict_next_deadline_ms().is_none());
}

#[test]
fn backspace_cancels_and_disable_is_fully_inert() {
    let mut t = terminal();
    t.set_predictive_echo("always");
    t.predict_char('a');
    t.predict_char('b');
    assert!(t.predict_backspace());
    assert_eq!(t.predict_overlay(), vec![0, 0, u32::from('a')]);
    t.set_predictive_echo("off");
    assert!(t.predict_overlay().is_empty());
    assert!(t.predict_next_deadline_ms().is_none());
}

#[test]
fn scrolled_back_viewport_paints_no_ghosts() {
    let mut t = terminal();
    for i in 0..12 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    t.set_predictive_echo("always");
    assert!(t.predict_char('a'));
    t.scroll_lines(2);
    assert!(
        t.predict_overlay().is_empty(),
        "guesses are active-grid coords — never painted over scrollback"
    );
    t.scroll_to_bottom();
    assert_eq!(
        t.predict_overlay().len(),
        3,
        "back at bottom, ghost returns"
    );
}

#[test]
fn a_run_s_first_press_hands_the_predictor_its_row() {
    let mut t = terminal();
    t.set_predictive_echo("always");
    t.process(b"$ \x1b[16G<r>\x1b[1;3H");
    assert!(t.predict_char('a'));
    t.process(b"a");
    t.predict_reconcile();
    assert!(t.predict_overlay().is_empty(), "retired by its echo");
    let events = t.predict.events_for_test();
    assert_eq!(
        events.decorated_proofs, 1,
        "proved over the right prompt: {events:?}"
    );
    assert_eq!(events.tail_refusals, 0, "{events:?}");
}

#[test]
fn a_press_while_scrolled_back_hands_no_row() {
    let mut t = terminal();
    for _ in 0..5 {
        t.process(b"$ \x1b[16G<r>\r\n");
    }
    t.process(b"$ \x1b[16G<r>\x1b[3G");
    t.set_predictive_echo("always");
    t.scroll_lines(1);
    assert!(t.predict_char('a'));
    t.scroll_to_bottom();
    t.process(b"a");
    t.predict_reconcile();
    assert!(t.predict_overlay().is_empty(), "retired by its echo");
    let events = t.predict.events_for_test();
    assert_eq!(
        events.decorated_proofs, 0,
        "no history row compared: {events:?}"
    );
    assert_eq!(events.tail_refusals, 1, "{events:?}");
}

#[test]
fn resize_drops_inflight_guesses() {
    let mut t = terminal();
    t.set_predictive_echo("always");
    t.predict_char('a');
    t.resize(10, 40).expect("admitted geometry");
    assert!(t.predict_overlay().is_empty(), "stale-coords guess dropped");
    assert!(t.predict_next_deadline_ms().is_none());
}
