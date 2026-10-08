// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typing click is delivered on the keypress, so its event is built by
//! the same code the frame drain uses and names the same instrument.
use super::{TrailSoundPolicy, trail_sound_event};

fn cue(col: u16) -> aterm_effects::cursor_glow::SoundCue {
    aterm_effects::cursor_glow::SoundCue {
        kind: aterm_effects::trail_sound::SoundKind::Typed,
        col,
        heat: 0.42,
        hue: 0.7,
        dir: 0,
        shifted: false,
    }
}

fn policy() -> TrailSoundPolicy {
    TrailSoundPolicy {
        voice: aterm_effects::trail_sound::SoundVoice::Style,
        gain: Some(0.4),
        tone: aterm_effects::tone::Tone::Playful,
        bed: true,
    }
}

/// Every policy axis rides the event, and the pan is normalized against the
/// terminal width exactly as the drain does it, so a key-time click lands at
/// the same stereo position as its echo.
#[test]
fn the_event_carries_the_whole_policy_and_the_drain_pan() {
    let p = policy();
    let ev = trail_sound_event(&cue(0), crate::cursor_glow::GlowStyle::Fire, 80, &p, 0.4);
    assert!((ev.pan - -1.0).abs() < 1e-6, "column 0 pans hard left");
    assert!((ev.heat - 0.42).abs() < 1e-6);
    assert!((ev.hue - 0.7).abs() < 1e-6);
    assert!((ev.gain - 0.4).abs() < 1e-6);
    assert!(ev.bed, "the bed knob rides the event");
    assert_eq!(ev.style, crate::cursor_glow::GlowStyle::Fire);
    assert!(matches!(
        ev.kind,
        aterm_effects::trail_sound::SoundGesture::Trail(
            aterm_effects::trail_sound::SoundKind::Typed
        )
    ));
    let right = trail_sound_event(&cue(79), crate::cursor_glow::GlowStyle::Fire, 80, &p, 0.4);
    assert!((right.pan - 1.0).abs() < 1e-6, "the last column pans right");
    // Past the last column clamps rather than panning outside the field.
    let past = trail_sound_event(&cue(999), crate::cursor_glow::GlowStyle::Fire, 80, &p, 0.4);
    assert!((past.pan - 1.0).abs() < 1e-6);
    // A degenerate one-column terminal never divides by zero.
    let narrow = trail_sound_event(&cue(0), crate::cursor_glow::GlowStyle::Fire, 1, &p, 0.4);
    assert!((narrow.pan - 0.0).abs() < 1e-6);
}

/// The bare Space key's key-time click carries [`SoundKind::Space`], end to
/// end from physical press to captured synth event.
#[test]
fn a_spacebar_reaches_the_audio_host_as_the_comma() {
    use crate::input::{InputEvent, Source};
    use aterm_effects::trail_sound::{SoundGesture, SoundKind};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    let mut app = crate::App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
    let wid = crate::WindowId(0);
    app.cursor_frame(
        wid,
        super::CursorFxInputs::sample_for_test(std::time::Instant::now()),
    )
    .expect("the fixture window ticks");
    let _ = app.trail_audio.take_captured_for_test();

    let _ = app.input(
        wid,
        InputEvent::Key {
            key: Key::Named(NamedKey::Space),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        },
        Source::Human,
    );

    let spoken = app.trail_audio.take_captured_for_test();
    assert!(
        spoken
            .iter()
            .any(|ev| matches!(ev.kind, SoundGesture::Trail(SoundKind::Space))),
        "a space's click is the comma, delivered at the key",
    );
    assert!(
        !spoken
            .iter()
            .any(|ev| matches!(ev.kind, SoundGesture::Trail(SoundKind::Typed))),
        "the comma REPLACES the typed click — one press, one gesture",
    );
}

/// A first-landing bare shift cues [`SoundKind::Shift`] at the key with no
/// echo, credit or repeat; auto-repeat and release stay silent, and the next
/// typed click still speaks.
#[test]
fn a_bare_shift_reaches_the_audio_host_as_the_lift() {
    use crate::input::{InputEvent, Source};
    use aterm_effects::trail_sound::{SoundGesture, SoundKind};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    let mut app = crate::App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
    let wid = crate::WindowId(0);
    app.cursor_frame(
        wid,
        super::CursorFxInputs::sample_for_test(std::time::Instant::now()),
    )
    .expect("the fixture window ticks");
    let _ = app.trail_audio.take_captured_for_test();

    let press = |app: &mut crate::App, event_type| {
        let _ = app.input(
            wid,
            InputEvent::Key {
                key: Key::Named(NamedKey::ShiftLeft),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type,
            },
            Source::Human,
        );
    };
    press(&mut app, KeyEventType::Press);
    let spoken = app.trail_audio.take_captured_for_test();
    assert!(
        spoken
            .iter()
            .any(|ev| matches!(ev.kind, SoundGesture::Trail(SoundKind::Shift))),
        "the first landing of a bare shift lifts",
    );
    // Repeat and release stay silent.
    press(&mut app, KeyEventType::Repeat);
    press(&mut app, KeyEventType::Release);
    assert!(
        app.trail_audio.take_captured_for_test().is_empty(),
        "a held shift's auto-repeat and its key-up cue nothing",
    );
    // The capital right behind it still clicks: the lift banked no
    // echo-muting credit.
    let _ = app.input(
        wid,
        InputEvent::Key {
            key: Key::Character('a'),
            mods: Modifiers::SHIFT,
            base_layout: None,
            event_type: KeyEventType::Press,
        },
        Source::Human,
    );
    assert!(
        app.trail_audio
            .take_captured_for_test()
            .iter()
            .any(|ev| matches!(ev.kind, SoundGesture::Trail(SoundKind::Typed))),
        "the capital the lift announced must still click",
    );
}

/// The keyed seam's `Typed` gesture's `shifted` flag agrees with
/// `typed_class_for`, which asks
/// the glyph: `A` with no SHIFT bit (Caps Lock) is a shifted `Typed`; `a`
/// and `7` are not; `a` under SHIFT is. (The visual flag still reads the
/// modifier, and Caps Lock mints no Shift cue.)
///
/// Also pins that a keypress reaches the audio host without a render tick:
/// nothing here ticks between a key and its capture (the one tick precedes
/// the first key). Keep it that way.
#[test]
fn a_caps_lock_capital_is_a_shifted_click() {
    use crate::input::{InputEvent, Source};
    use aterm_effects::trail_sound::{SoundGesture, SoundKind};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    let mut app = crate::App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
    let wid = crate::WindowId(0);
    app.cursor_frame(
        wid,
        super::CursorFxInputs::sample_for_test(std::time::Instant::now()),
    )
    .expect("the fixture window ticks");
    let _ = app.trail_audio.take_captured_for_test();

    let mut shifted_of = |ch: char, mods: Modifiers| -> bool {
        let _ = app.input(
            wid,
            InputEvent::Key {
                key: Key::Character(ch),
                mods,
                base_layout: None,
                event_type: KeyEventType::Press,
            },
            Source::Human,
        );
        let spoken = app.trail_audio.take_captured_for_test();
        let typed = spoken
            .iter()
            .find(|ev| matches!(ev.kind, SoundGesture::Trail(SoundKind::Typed)))
            .unwrap_or_else(|| panic!("`{ch}` ({mods:?}) must click at the key"));
        assert!(
            !spoken
                .iter()
                .any(|ev| matches!(ev.kind, SoundGesture::Trail(SoundKind::Shift))),
            "`{ch}` ({mods:?}): a glyph key never mints the bare Shift's pickup"
        );
        typed.shifted
    };
    assert!(
        shifted_of('A', Modifiers::empty()),
        "a Caps-Lock capital is a shifted click — the glyph is asked, not the modifier"
    );
    assert!(
        !shifted_of('a', Modifiers::empty()),
        "a lowercase letter is not shifted"
    );
    assert!(
        !shifted_of('7', Modifiers::empty()),
        "a digit is not shifted"
    );
    assert!(
        shifted_of('a', Modifiers::SHIFT),
        "`a` under Shift is the capital it always was"
    );
    // Every way a shifted glyph can reach the seam: a shifted mark (`?` is
    // `/` under Shift; `Key::Character` holds the base), a capital under
    // Shift and Caps Lock, and the literal-text path, which carries no
    // modifier bits (owner, 2026-09-19).
    assert!(
        shifted_of('/', Modifiers::SHIFT),
        "`?` — `/` under Shift — is a shifted click"
    );
    assert!(
        shifted_of('9', Modifiers::SHIFT),
        "`(` — `9` under Shift — is a shifted click"
    );
    assert!(
        shifted_of('a', Modifiers::SHIFT | Modifiers::CAPS_LOCK),
        "Shift over Caps Lock is still a shifted click"
    );
    let mut text_shifted_of = |text: &str| -> bool {
        let _ = app.input(wid, InputEvent::Text(text.to_string()), Source::Human);
        app.trail_audio
            .take_captured_for_test()
            .iter()
            .find(|ev| matches!(ev.kind, SoundGesture::Trail(SoundKind::Typed)))
            .unwrap_or_else(|| panic!("committed {text:?} must click at the key"))
            .shifted
    };
    assert!(
        text_shifted_of("A"),
        "one committed capital is a capital, whatever path carried it"
    );
    assert!(!text_shifted_of("a"), "a committed lowercase glyph is not");
    assert!(
        !text_shifted_of("Abc"),
        "a committed RUN is a phrase, not a key: it stays unshifted"
    );
}

/// Three echo-born cues drained in one frame reach the host with three
/// distinct, strictly increasing, non-zero `at_ms`, not one frame-level
/// stamp. The cues are queued on the engine's backlog between frames, so
/// this tests the frame drain's stamp, not the keyed seam's.
#[test]
fn the_drain_stamps_each_echo_cue_at_its_own_instant() {
    use aterm_effects::trail_sound::{SoundGesture, SoundKind};
    use std::time::{Duration, Instant};

    let mut app = crate::App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.trail_sounds = Some(true);
    app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
    let wid = crate::WindowId(0);
    app.windows.get_mut(&wid).expect("window").focused = true;
    let t0 = Instant::now();
    // One drawing tick arms the engine's silence law (`sound_live`).
    app.cursor_frame(wid, super::CursorFxInputs::sample_for_test(t0))
        .expect("the fixture window ticks");
    let _ = app.trail_audio.take_captured_with_meta_for_test();

    {
        let glow = &mut app
            .windows
            .get_mut(&wid)
            .expect("window")
            .fx
            .cursor_mut()
            .glow;
        for i in 0..3u64 {
            assert!(
                glow.cue_keystroke(t0 + Duration::from_millis(1 + i)),
                "the backlog takes cue {i}"
            );
        }
    }
    app.cursor_frame(
        wid,
        super::CursorFxInputs::sample_for_test(t0 + Duration::from_millis(16)),
    )
    .expect("the fixture window ticks");

    let spoken = app.trail_audio.take_captured_with_meta_for_test();
    let stamps: Vec<u32> = spoken
        .iter()
        .filter(|(ev, _)| matches!(ev.kind, SoundGesture::Trail(SoundKind::Typed)))
        .map(|(_, meta)| meta.at_ms)
        .collect();
    assert_eq!(
        stamps.len(),
        3,
        "three cues drained in one frame: {stamps:?}"
    );
    assert!(
        stamps.iter().all(|&at| at != 0),
        "an echo-born cue is stamped, never left to the block clock: {stamps:?}"
    );
    assert!(
        stamps.windows(2).all(|w| w[1] > w[0]),
        "three cues in one frame carry three distinct, increasing stamps: {stamps:?}"
    );
    // And an echo-born cue still carries no glyph: class 0, rank 0.
    for (_, meta) in &spoken {
        assert_eq!((meta.glyph_class, meta.rank), (0, 0));
    }
}

/// The key seam's [`crate::App::glow_style`] and the frame drain's
/// `glow_config().style` agree, so a keystroke sounds the same instrument
/// whether the click rides the key or the echo.
#[test]
fn glow_style_matches_glow_config() {
    let app = crate::App::headless_for_test();
    assert_eq!(
        app.glow_style(),
        app.glow_config().style,
        "the key-time click must name the frame drain's instrument"
    );
}
