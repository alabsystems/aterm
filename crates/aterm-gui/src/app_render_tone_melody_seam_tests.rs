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

/// A headless App whose audio host is live but capturing: `is_live()` gates
/// both the classifier (`tone_infer_active`) and the key-time cue
/// (`keystroke_click_audible`), so a sealed host would short-circuit them.
fn app_with_capturing_audio() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    // The trail's default is platform-split (`DEFAULT_DECORATIVE_EFFECTS`): turn it on.
    app.config.cursor_trail = Some(true);
    app.trail_audio = TrailAudio::capturing_for_test();
    assert!(app.trail_audio.is_live(), "the capturing host reports live");
    (app, WindowId(0))
}

/// One frame of the real single-pane cursor-effects tick — the seam that
/// resolves the tone and drains the glow's cues into the audio host.
fn tick(app: &mut App, wid: WindowId) {
    app.cursor_frame(wid, CursorFxInputs::sample_for_test(Instant::now()))
        .expect("the fixture window ticks");
}

fn type_line(app: &mut App, wid: WindowId, line: &str) {
    for ch in line.chars() {
        let _ = app.input(wid, key(ch), Source::Human);
    }
}

/// Trail gestures only: the bonk is tone-blind by design.
fn trail_tones(events: &[SoundEvent]) -> Vec<Tone> {
    events
        .iter()
        .filter(|ev| matches!(ev.kind, SoundGesture::Trail(_)))
        .map(|ev| ev.tone)
        .collect()
}

/// A frustrated line typed through the real input path reaches the audio
/// host as sound events whose `tone` is Frustrated. The first tick arms the
/// glow's key-cue seam (`cursor_glow::sound_live` needs a drawing tick).
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

    // The line comes from the committed training distribution, so a miss is
    // a broken seam, not a model change.
    type_line(&mut app, wid, "why is this broken again ugh");
    // The verdict lands at the next frame's drain (P06: never in the key handler).
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

    // Each key-time click carries the mood the classifier held at that key,
    // so the verdict ramps (Technical, Excited, Frustrated) as the line
    // grows. Discard the ramp-up and pin that every click after the verdict
    // carries it.
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

/// With `tone_melody = false` events carry the neutral identity even though
/// the tracker still holds a verdict, so a disabled knob sounds like no tone.
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
    // Clear clicks already delivered before the toggle: they left with the
    // mood live at their keypress, and the knob governs only later events.
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
