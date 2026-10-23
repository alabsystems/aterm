// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The sound policy's laws are pinned in `aterm_effects::sound_policy`;
//! these drive them through this host's config and window.

use std::time::Instant;

use aterm_core::terminal::Terminal;
use aterm_effects::trail_sound::{SoundGesture, SoundVoice, WordGesture};
use aterm_effects::word_decorations::{EffectGeom, WordDecorations};
use aterm_lexicon::Lexicon;

use super::drain_curse_bonk_cues;

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
        wd.note_typed_edit(now, None);
        wd.rescan(&term, 4, 32, &lex, &cfg, 1, now);
        tick(wd, now);
    };

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
