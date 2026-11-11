// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use super::*;
use crate::kitty_sing::CelebrateOn;
use crate::kitty_summon::DOG_SUMMON_KEYS;

const fn glyph(c: char) -> TypedPress<'static> {
    TypedPress {
        typed: Some(c),
        ..TypedPress::NONE
    }
}

fn feed_line(
    c: &mut Companions,
    line: &str,
    words: Option<(&Lexicon, &ScanOptions<'_>)>,
    now: Instant,
) -> TypedHit {
    line.chars().fold(TypedHit::NONE, |hit, ch| {
        hit.merge(c.note_typed_press(
            glyph(ch),
            PressFeed {
                session: 7,
                now,
                words,
                tone_active: false,
                keyed_enter: false,
            },
        ))
    })
}

#[test]
fn one_press_feeds_every_typed_detector_by_the_same_cascade() {
    let now = Instant::now();
    let lexicon = Lexicon::builtin().clone();
    let opts = ScanOptions::default();
    let words = Some((&lexicon, &opts));
    let mut c = Companions::default();
    assert!(feed_line(&mut c, "kitty", words, now).feline);
    assert_eq!(c.tone_tracker.window_chars(), 5);

    let backspace = TypedPress {
        backspace: true,
        ..TypedPress::default()
    };
    let feed = PressFeed {
        session: 7,
        now,
        words,
        tone_active: false,
        keyed_enter: false,
    };
    assert!(!feed_line(&mut c, "kittx", words, now).feline);
    c.note_typed_press(backspace, feed);
    assert_eq!(
        c.tone_tracker.window_chars(),
        9,
        "a Backspace pops one glyph"
    );
    assert!(
        feed_line(&mut c, "y", words, now).feline,
        "the popped window completes `kitty` at the retyped glyph"
    );

    feed_line(&mut c, "kitt", words, now);
    let brk = TypedPress {
        brk: true,
        ..TypedPress::default()
    };
    c.note_typed_press(brk, feed);
    assert_eq!(c.tone_tracker.window_chars(), 0, "a break clears the tone");
    assert!(
        !feed_line(&mut c, "y", words, now).feline,
        "a break clears the summon window"
    );

    let silent = feed_line(&mut c, " kitty", None, now);
    assert!(
        !silent.feline && !silent.records,
        "no vocabulary, no reaction"
    );
    assert_eq!(c.tone_tracker.window_chars(), 7, "the tone window is kept");
    assert!(feed_line(&mut c, " kitty", words, now).feline);
}

#[test]
fn drain_serious_clears_dog_cameo_and_live_runs_while_keeping_cooldowns_and_odometer() {
    let now = Instant::now();
    let lexicon = Lexicon::builtin().clone();
    let opts = ScanOptions::default();
    let words = Some((&lexicon, &opts));
    let mut c = Companions::default();

    for _ in 0..DOG_SUMMON_KEYS {
        feed_line(&mut c, "a", words, now);
    }
    let first = feed_line(&mut c, " kitty", words, now);
    assert!(first.feline && first.records, "first summon records");

    c.dog_cameo.on_summon(now, 42);
    c.dog_cameo_session = Some(7);
    assert!(c.dog_cameo.active(now));
    c.kitty_sing
        .arm_external(now, 7, 1, 2, CelebrateOn::Enter)
        .expect("initial arm succeeds");
    feed_line(&mut c, " kitt", words, now);

    c.drain_serious();

    assert!(!c.dog_cameo.active(now));
    assert_eq!(c.dog_cameo_session, None);
    assert_eq!(c.kitty_sing.armed_external(), None);
    assert_eq!(
        c.tone_tracker.window_chars(),
        0,
        "drain_serious clears the tone tracker's partial typing window"
    );
    assert!(
        !feed_line(&mut c, "y", words, now + Duration::from_secs(1)).feline,
        "partial word before drain_serious does not complete after unmute"
    );

    assert!(
        !c.kitty_sing
            .cooldown_remaining(now + Duration::from_secs(1))
            .is_zero(),
        "celebration arm cooldown survives drain_serious"
    );
    let second = feed_line(&mut c, " kitty", words, now + Duration::from_secs(1));
    assert!(
        second.feline && !second.records,
        "Kitty Log 30s cooldown survives drain_serious"
    );
    assert!(
        feed_line(&mut c, " puppy", words, now + Duration::from_secs(1)).canine,
        "canine keystroke odometer survives drain_serious"
    );
}
