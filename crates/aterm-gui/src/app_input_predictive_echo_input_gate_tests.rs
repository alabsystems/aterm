// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use crate::input::{InputEvent, Source};
use crate::{App, WindowId, term_lock};
use aterm_predict::PredictMode;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

fn printable(ch: char) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(ch),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

#[test]
fn codex_report_event_types_never_arms_the_expiry_erase() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();

    let model = aterm_models_core::predictive_echo_visibility_model();
    let mut model_state = model.init_state();

    app.config.predictive_echo = Some("adaptive".to_string());
    let now = Instant::now();
    let first_confirmed = now + Duration::from_millis(60);
    let second_confirmed = first_confirmed + Duration::from_millis(60);
    {
        let predictor = &mut app.windows.get_mut(&wid).expect("window").predictor;
        predictor.set_mode(PredictMode::Adaptive);
        assert!(predictor.predict_char('a', (0, 0), 0, 80, now));
        predictor.reconcile(Some((0, 1)), 0, false, first_confirmed, |row, col| {
            ((row, col) == (0, 0)).then_some('a')
        });
        assert!(predictor.predict_char('b', (0, 1), 0, 80, first_confirmed));
        predictor.reconcile(Some((0, 2)), 0, false, second_confirmed, |row, col| {
            ((row, col) == (0, 1)).then_some('b')
        });
        assert!(predictor.predict_char('c', (0, 2), 0, 80, second_confirmed));
        assert!(predictor.is_displaying(second_confirmed));
    }
    assert!(model.fire("ConfirmSlow", &mut model_state));
    assert!(model.fire("Key", &mut model_state));
    assert_eq!(model_state["pending"], 1);
    assert_eq!(model_state["visible"], 1);

    assert!(
        app.windows[&wid].predictor.next_deadline().is_some(),
        "control: the pre-Codex visible guess armed the 250 ms self-heal"
    );

    term_lock(&term).process(b"\x1b[>7u");
    assert!(model.fire("EnterComposer", &mut model_state));
    crate::app_input::pin_input_clock(Some(second_confirmed));
    let _ = app.input(wid, printable('c'), Source::Human);
    assert!(model.fire("Key", &mut model_state));
    let predictor = &app.windows[&wid].predictor;
    assert!(
        predictor.idle(),
        "the app-owned composer flushes prior guesses"
    );
    assert_eq!(model_state["pending"], 0);
    assert_eq!(model_state["visible"], 0);
    assert!(
        predictor.next_deadline().is_none(),
        "Codex input must not arm a delayed erase"
    );
}
