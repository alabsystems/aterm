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
        event_type: KeyEventType::Press,
    }
}

/// A headless App whose audio host is live but capturing: `is_live()` gates
/// both the classifier (`tone_infer_active`) and the key-time cue
/// (`keystroke_click_audible`), so a sealed host would short-circuit them.
fn app_with_capturing_audio() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    // Enable the trail explicitly: its default is platform-split
    // (`DEFAULT_DECORATIVE_EFFECTS`), and the audio seam must behave the same
    // everywhere once on.
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
    // The verdict lands at the next frame's drain (P06: never in the key
    // handler) — exactly when the first cue that could carry it is stamped.
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

/// EVERY drain seam resolves its tone through the ONE helper. Single-pane,
/// composed, and split-pane rendering now share one cursor-effects tick and
/// therefore one drain policy; a second construction here is a new seam and
/// must be pinned explicitly. A behavioural test can only reach the route
/// its fixture happens to drive, so the source-level census keeps that
/// centralization honest.
///
/// It refuses two specific regressions: a policy literal that stops taking
/// the tracker's answer (a hardcoded `Tone::Technical`, the silent revert),
/// and one that reads `current()` directly, which would keep stamping a
/// stale mood after the knob was switched off.
#[test]
fn every_trail_sound_policy_takes_its_tone_from_the_tracker() {
    // Scan the CODE only: this module's own prose mentions both spellings,
    // and a test that matches itself proves nothing.
    let source = include_str!("app_render.rs");
    let code = source
        .split("/// THE TONE-MELODY SEAM")
        .next()
        .expect("this module's doc block terminates the scanned region");
    let policies: Vec<&str> = code
        .match_indices("TrailSoundPolicy {")
        .filter(|(at, _)| !code[..*at].ends_with("struct "))
        .filter_map(|(at, _)| code[at..].split('}').next())
        .collect();
    assert_eq!(
        policies.len(),
        1,
        "the three render routes must keep one shared drain seam; a new one must be wired here too",
    );
    for policy in &policies {
        assert!(
            policy.contains("tone,"),
            "a policy must take the resolved `tone` binding, not an inline \
             literal: {policy:?}",
        );
    }
    // Every `tone` binding in this file comes from the shared helper.
    let bindings: Vec<&str> = code
        .split("let tone = ")
        .skip(1)
        .filter_map(|tail| tail.split(';').next())
        .collect();
    assert_eq!(bindings.len(), policies.len(), "one binding per drain seam");
    for binding in &bindings {
        assert!(
            binding.contains("tone_tracker") && binding.contains(".effective("),
            "a drain seam must resolve its tone through \
             `ToneTracker::effective` (knob folded in one place): {binding:?}",
        );
        assert!(
            !binding.contains(".current()"),
            "`current()` ignores the knob — a stale verdict would survive a \
             toggle-off: {binding:?}",
        );
    }
    // …AND THE SECOND SEAM, which does not live in this file. The KEY-TIME
    // typing click reaches the synth from `app_input` without a drain, so a
    // scan of this file alone would report full coverage while the hottest
    // audio path stamped whatever it liked. Same two refusals, same helper.
    let key_seam = include_str!("app_input.rs");
    let key_policies: Vec<&str> = key_seam
        .match_indices("TrailSoundPolicy {")
        .filter_map(|(at, _)| key_seam[at..].split('}').next())
        .collect();
    assert_eq!(
        key_policies.len(),
        1,
        "expected exactly the deferred key-time delivery's policy in \
         `app_input` (the typed/space click and the shift lift both \
         resolve through it, after the PTY write)",
    );
    for policy in &key_policies {
        assert!(
            policy.contains("tone_tracker") && policy.contains(".effective("),
            "the key-time click must resolve its tone through the SAME \
             helper the drains use: {policy:?}",
        );
        assert!(
            !policy.contains(".current()"),
            "`current()` ignores the knob: {policy:?}",
        );
    }
}
