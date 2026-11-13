// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::input::{InputEvent, Source};
use crate::trail_audio::TrailAudio;
use crate::{App, WindowId};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

fn key(character: char) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(character),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

fn type_line(app: &mut App, wid: WindowId, line: &str) {
    for ch in line.chars() {
        let _ = app.input(wid, key(ch), Source::Human);
    }
}

#[test]
fn a_muted_build_reports_the_gate_that_stopped_the_classifier() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(!app.trail_audio.is_live(), "the headless host is sealed");
    type_line(&mut app, wid, "why is this broken again ugh");

    let line = app.tone_status().expect("the fixture window is focused");
    assert!(line.contains("audio=inert"), "{line}");
    assert!(line.contains("active=false"), "{line}");
    assert!(line.contains("inferences=0"), "{line}");
    assert!(line.contains("tone=technical"), "{line}");
    assert!(
        line.contains("window_chars=28"),
        "the window tracks the typed text even while inference is off: {line}",
    );
}

#[test]
fn a_live_build_reports_the_classified_mood_and_that_the_model_ran() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.trail_audio = TrailAudio::capturing_for_test();
    app.config.trail_sound_volume = Some(0.65);
    type_line(&mut app, wid, "why is this broken again ugh");

    let line = app.tone_status().expect("the fixture window is focused");
    assert!(line.contains("tone=frustrated"), "{line}");
    assert!(line.contains("effective=frustrated"), "{line}");
    assert!(line.contains("audio=live"), "{line}");
    assert!(line.contains("active=true"), "{line}");
    assert!(line.contains("knob=on sounds=on volume=0.65"), "{line}");
    assert!(!line.contains("inferences=0"), "the model ran: {line}");
}

#[test]
fn a_disabled_knob_separates_the_verdict_from_what_the_synth_hears() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.trail_audio = TrailAudio::capturing_for_test();
    type_line(&mut app, wid, "why is this broken again ugh");
    app.config.tone_melody = Some(false);

    let line = app.tone_status().expect("the fixture window is focused");
    assert!(line.contains("tone=frustrated"), "{line}");
    assert!(line.contains("effective=technical"), "{line}");
    assert!(line.contains("knob=off"), "{line}");
    assert!(line.contains("active=false"), "{line}");
}

#[test]
fn a_windowless_app_refuses_rather_than_inventing_a_row() {
    let mut app = App::headless_for_test();
    app.frontmost_window = None;
    assert!(app.tone_status().is_err());
}

#[test]
fn the_status_line_never_carries_the_typed_text() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.trail_audio = TrailAudio::capturing_for_test();
    let secret = "hunter2correcthorse";
    type_line(&mut app, wid, secret);

    let line = app.tone_status().expect("the fixture window is focused");
    assert!(
        !line.contains("hunter") && !line.contains("horse"),
        "the window text must never reach the wire: {line}",
    );
    let chars: Vec<char> = secret.chars().collect();
    for window in chars.windows(4) {
        let needle: String = window.iter().collect();
        assert!(
            !line.contains(&needle),
            "no partial leak either ({needle}): {line}",
        );
    }
    assert!(
        line.contains(&format!("window_chars={}", secret.len())),
        "only the count is reported: {line}",
    );
}

fn seam_row_after_one_frame(app: &mut App) -> String {
    let wid = WindowId(0);
    let fx = crate::app_render::CursorFxInputs::sample_for_test(std::time::Instant::now());
    app.cursor_frame(wid, fx).expect("one cursor frame");
    app.tone_status().expect("the fixture window is focused")
}

fn audible_app(style: &str) -> App {
    let mut app = App::headless_for_test();
    app.trail_audio = TrailAudio::capturing_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some(style.into());
    app.config.trail_sounds = Some(true);
    app
}

#[test]
fn a_dimmed_aurora_reads_trail_off_never_engine_silent() {
    let mut app = audible_app("rainbow");
    app.config.cursor_trail_intensity = Some(0.0);
    let line = seam_row_after_one_frame(&mut app);
    assert!(line.contains("seam=closed:trail-off"), "{line}");
    assert!(line.contains("trail=off"), "{line}");
    assert!(line.contains("engine_sound=closed"), "{line}");
    assert!(!line.contains("engine-silent"), "{line}");
}

#[test]
fn a_reduce_motion_trail_is_dark_but_on_and_heard() {
    let mut app = audible_app("rainbow");
    app.system_reduce_motion = true;
    let line = seam_row_after_one_frame(&mut app);
    assert!(line.contains("seam=open"), "{line}");
    assert!(line.contains("trail=on"), "{line}");
    assert!(line.contains("engine_sound=open"), "{line}");
}

#[test]
fn a_classic_window_is_heard_and_the_verb_says_so() {
    let mut app = audible_app("classic");
    let line = seam_row_after_one_frame(&mut app);
    assert!(line.contains("seam=open"), "{line}");
    assert!(line.contains("engine_sound=open"), "{line}");
    assert!(line.contains("trail=on"), "{line}");
}
