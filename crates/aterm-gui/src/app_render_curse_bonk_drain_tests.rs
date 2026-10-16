// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sound policy's laws are pinned in `aterm_effects::sound_policy`;
//! these drive them through this host's config and window.
use std::time::Instant;

use aterm_core::terminal::Terminal;
use aterm_effects::trail_sound::{SoundGesture, SoundVoice, WordGesture};
use aterm_effects::word_decorations::{EffectGeom, WordDecorations};
use aterm_lexicon::Lexicon;

use super::{bonk_sound_gain, drain_curse_bonk_cues};

/// A window inside [`crate::RESIZE_SOUND_QUIET`] of its last reflow mutes the sound seams (a
/// TUI's resize repaint storm is not typing); a never-resized window is never quiet.
#[test]
fn resize_quiets_the_sound_seams_and_expires() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let now = std::time::Instant::now();
    assert!(
        !app.windows.get(&wid).unwrap().resize_sound_quiet(now),
        "a never-resized window must not be quiet"
    );
    app.windows.get_mut(&wid).unwrap().last_resize_at = Some(now);
    assert!(
        app.windows.get(&wid).unwrap().resize_sound_quiet(now),
        "a just-reflowed window must be quiet"
    );
    assert!(
        !app.windows
            .get(&wid)
            .unwrap()
            .resize_sound_quiet(now + crate::RESIZE_SOUND_QUIET),
        "the quiet window must expire"
    );
}

/// Over a real typed-curse tick, an enabled drain emits one `Words(Bonk)` at the user volume; a
/// muted drain drops the same cue and still empties the engine's queue, so nothing backlogs.
#[test]
fn drain_maps_typed_cues_to_namespaced_bonk_and_never_backlogs() {
    let cfg = crate::app_config::Config::default()
        .sparkle_deco_config()
        .expect("defaults enable sparkle words");
    let lex = Lexicon::with_languages(&["en"]);
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 4,
        cols: 32,
    };
    let t0 = Instant::now();
    let style = aterm_effects::cursor_glow::GlowStyle::RainbowKitty;
    let tick = |wd: &mut WordDecorations, now: Instant| {
        let (mut o, mut i, mut f, mut n) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        wd.tick(
            now, &cfg, geom, None, None, true, &mut o, &mut i, &mut f, &mut n,
        );
    };
    let typed_curse = |wd: &mut WordDecorations, now: Instant| {
        let mut term = Terminal::new(4, 32);
        term.process(b"fuck");
        // Typed means a committed key, not just a caret one past the token (a scrolling build
        // log leaves it there too); the engine's typed witness demands both.
        wd.note_typed_edit(now, None);
        wd.rescan(&term, 4, 32, &lex, &cfg, 1, now);
        tick(wd, now);
    };

    // Enabled: one cue ⇒ one namespaced bonk at the user volume.
    let mut wd = WordDecorations::default();
    typed_curse(&mut wd, t0);
    let mut events = Vec::new();
    let drained = drain_curse_bonk_cues(
        &mut wd,
        style,
        SoundVoice::Style,
        geom.cols,
        Some(0.4),
        false,
        |ev| events.push(ev),
    );
    assert_eq!(drained.emitted, 1);
    assert_eq!(drained.wince_hits, 1);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].kind,
        SoundGesture::Words(WordGesture::Bonk),
        "the drain must speak the namespaced sparkle-words gesture"
    );
    assert_eq!(events[0].gain, 0.4);
    assert_eq!(
        events[0].style, style,
        "the clash register keys off the trail style"
    );

    // Muted (gain None): the same cue is dropped and the queue emptied, so nothing replays.
    let mut wd2 = WordDecorations::default();
    typed_curse(&mut wd2, t0);
    let drained = drain_curse_bonk_cues(
        &mut wd2,
        style,
        SoundVoice::Style,
        geom.cols,
        None,
        false,
        |_| panic!("a muted drain must not emit"),
    );
    assert_eq!(drained.emitted, 0);
    assert_eq!(
        drained.wince_hits, 1,
        "visual reaction is sound-independent"
    );
    let drained = drain_curse_bonk_cues(
        &mut wd2,
        style,
        SoundVoice::Style,
        geom.cols,
        Some(0.4),
        false,
        |_| panic!("the muted frame's cue must not backlog into the unmute"),
    );
    assert_eq!(drained, aterm_effects::sound_policy::CurseDrain::default());
}

/// Flipping only `trail_sounds` (the "Music effects" setting) silences a real typed curse and
/// leaves the wince unchanged, through the real [`bonk_sound_gain`] and [`drain_curse_bonk_cues`].
#[test]
fn music_effects_off_drops_a_real_typed_curse_but_keeps_the_wince() {
    let cfg = crate::app_config::Config::default()
        .sparkle_deco_config()
        .expect("defaults enable sparkle words");
    let lex = Lexicon::with_languages(&["en"]);
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 4,
        cols: 32,
    };
    let t0 = Instant::now();
    let style = aterm_effects::cursor_glow::GlowStyle::RainbowKitty;
    let typed_curse = |now: Instant| {
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(4, 32);
        term.process(b"fuck");
        wd.note_typed_edit(now, None);
        wd.rescan(&term, 4, 32, &lex, &cfg, 1, now);
        let (mut o, mut i, mut f, mut n) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        wd.tick(
            now, &cfg, geom, None, None, true, &mut o, &mut i, &mut f, &mut n,
        );
        wd
    };
    // The whole policy vector, with only the master differing between the
    // two runs: focused, knob on, full motion, audible volume.
    let drain_with_master = |master: bool, emit: &mut dyn FnMut(_)| {
        let mut wd = typed_curse(t0);
        drain_curse_bonk_cues(
            &mut wd,
            style,
            SoundVoice::Style,
            geom.cols,
            bonk_sound_gain(true, master, true, false, 0.4),
            false,
            emit,
        )
    };

    let mut heard = Vec::new();
    let audible = drain_with_master(true, &mut |ev| heard.push(ev));
    assert_eq!(
        audible.emitted, 1,
        "PRECONDITION: with Music effects ON this posture really bonks",
    );
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].kind, SoundGesture::Words(WordGesture::Bonk));

    let muted = drain_with_master(false, &mut |_| {
        panic!("Music effects OFF must not push a single bonk event")
    });
    assert_eq!(
        muted.emitted, 0,
        "the master switch the Sound menu names must reach the bonk",
    );
    assert_eq!(
        muted.wince_hits, audible.wince_hits,
        "and it is a SOUND gate only: the on-screen wince is unchanged",
    );
}
