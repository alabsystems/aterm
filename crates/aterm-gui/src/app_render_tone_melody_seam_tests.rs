// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{SoundEvent, SoundGesture};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

use super::CursorFxInputs;
use crate::input::{InputEvent, Source};
use crate::trail_audio::TrailAudio;
use crate::{App, WindowId};

fn key(character: char) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(character),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

fn app_with_capturing_audio() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.trail_audio = TrailAudio::capturing_for_test();
    assert!(app.trail_audio.is_live(), "the capturing host reports live");
    (app, WindowId(0))
}

fn tick(app: &mut App, wid: WindowId) {
    app.cursor_frame(wid, CursorFxInputs::sample_for_test(Instant::now()))
        .expect("the fixture window ticks");
}

fn type_line(app: &mut App, wid: WindowId, line: &str) {
    for ch in line.chars() {
        let _ = app.input(wid, key(ch), Source::Human);
    }
}

fn trail_tones(events: &[SoundEvent]) -> Vec<Tone> {
    events
        .iter()
        .filter(|ev| matches!(ev.kind, SoundGesture::Trail(_)))
        .map(|ev| ev.tone)
        .collect()
}

#[test]
fn a_typed_mood_reaches_the_audio_host_on_every_trail_event() {
    let (mut app, wid) = app_with_capturing_audio();
    assert!(
        app.config.tone_melody_or_default()
            && app.config.trail_sounds_or_default()
            && app.config.trail_sound_volume() > 0.0,
        "the shipped defaults are what this test claims to exercise",
    );
    tick(&mut app, wid);
    let _ = app.trail_audio.take_captured_for_test();

    type_line(&mut app, wid, "why is this broken again ugh");
    tick(&mut app, wid);
    assert_eq!(
        app.windows[&wid].fx.companions().tone_tracker.current(),
        Tone::Frustrated,
        "the real input path must have fed the classifier",
    );
    assert!(
        app.windows[&wid].fx.companions().tone_tracker.inferences > 0,
        "the classifier must actually have run under the live-audio gate",
    );

    tick(&mut app, wid);
    let _ = app.trail_audio.take_captured_for_test();
    type_line(&mut app, wid, " so broken");
    assert_eq!(
        app.windows[&wid].fx.companions().tone_tracker.current(),
        Tone::Frustrated,
        "precondition: the register has not moved under the second line",
    );
    tick(&mut app, wid);
    let tones = trail_tones(&app.trail_audio.take_captured_for_test());
    assert!(
        !tones.is_empty(),
        "typing with sound live must produce trail cues to carry a mood",
    );
    assert!(
        tones.iter().all(|&t| t == Tone::Frustrated),
        "every event must carry the typed mood, got {tones:?}",
    );
}

#[test]
fn a_disabled_knob_stamps_the_neutral_identity_over_a_cached_verdict() {
    let (mut app, wid) = app_with_capturing_audio();
    tick(&mut app, wid);
    type_line(&mut app, wid, "why is this broken again ugh");
    tick(&mut app, wid); // the verdict lands at the frame drain (P06)
    assert_eq!(
        app.windows[&wid].fx.companions().tone_tracker.current(),
        Tone::Frustrated
    );

    app.config.tone_melody = Some(false);
    let _ = app.trail_audio.take_captured_for_test();
    type_line(&mut app, wid, " and again");
    tick(&mut app, wid);

    let tones = trail_tones(&app.trail_audio.take_captured_for_test());
    assert!(!tones.is_empty(), "the sound path is still live");
    assert!(
        tones.iter().all(|&t| t == Tone::Technical),
        "a disabled knob must stamp the identity, got {tones:?}",
    );
    assert_eq!(
        app.windows[&wid].fx.companions().tone_tracker.current(),
        Tone::Frustrated,
        "the cached verdict is masked at the seam, not erased",
    );
}
