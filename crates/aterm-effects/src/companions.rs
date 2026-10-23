// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window companion state for typed-word detectors, sing-along notes, and cameos.

use aterm_lexicon::{Lexicon, ScanOptions};
use aterm_time::Instant;

use crate::dog_cameo::DogCameo;
use crate::kitty_cursor::CursorCat;
use crate::kitty_sing::{KittySing, MusicNotes};
use crate::kitty_summon::{TypedHit, TypedKittySummon};
use crate::tone_infer::ToneTracker;

/// One classified key press from the host's committed key path, the only input the typed detectors
/// have.
#[derive(Clone, Copy, Debug, Default)]
pub struct TypedPress<'ev> {
    /// A key that edits or moves beyond one glyph (Enter, Tab, Escape, Delete, navigation, a kill
    /// or nav chord, raw controller bytes): every detector's window resets.
    pub brk: bool,
    /// A plain Backspace: each window pops one glyph.
    pub backspace: bool,
    /// The bare printed glyph.
    pub typed: Option<char>,
    /// A committed IME run, fed char by char after `typed`.
    pub ime: Option<&'ev str>,
}

impl<'ev> TypedPress<'ev> {
    /// The brk → backspace → glyphs cascade every feed takes.
    #[inline]
    pub fn feed<T>(
        self,
        target: &mut T,
        on_break: impl FnOnce(&mut T),
        on_backspace: impl FnOnce(&mut T),
        mut on_char: impl FnMut(&mut T, char),
    ) {
        if self.brk {
            on_break(target);
        } else if self.backspace {
            on_backspace(target);
        } else {
            for c in self.chars() {
                on_char(target, c);
            }
        }
    }

    /// The glyphs this press types — the bare glyph, then the IME run.
    #[inline]
    pub fn chars(self) -> impl Iterator<Item = char> + 'ev {
        self.typed
            .into_iter()
            .chain(self.ime.into_iter().flat_map(str::chars))
    }
}

/// What the host lends one press's feed.
#[derive(Clone, Copy)]
pub struct PressFeed<'a> {
    /// The session the press went to.
    pub session: u64,
    pub now: Instant,
    /// The compiled vocabulary and its scan options, while sparkle words are on.
    pub words: Option<(&'a Lexicon, &'a ScanOptions<'a>)>,
    /// Tone inference may run (the host's `tone_infer_active`).
    pub tone_active: bool,
    /// The press was a keyed Enter — the edge an armed `on=enter` celebration waits for.
    pub keyed_enter: bool,
}

/// One window's companions, reached through [`crate::pipeline::EffectsPipeline::companions_mut`].
#[derive(Default)]
pub struct Companions {
    /// The earned cursor cat's decision layer ([`crate::kitty_cursor`]).
    pub cursor_cat: CursorCat,
    /// Typed-"kitty" summon detector ([`crate::kitty_summon`]).
    pub kitty_summon: TypedKittySummon,
    /// Tone-of-typing tracker ([`crate::tone_infer`]), whose verdict rides every trail
    /// [`crate::trail_sound::SoundEvent`] as the melody's tone.
    pub tone_tracker: ToneTracker,
    /// Sing-along detector ([`crate::kitty_sing`]): the held-key repeat run.
    pub kitty_sing: KittySing,
    /// The celebration's ♪/♫ music-note ring (bounded at [`crate::kitty_sing::MAX_NOTES`]).
    pub music_notes: MusicNotes,
    /// Last riff bar index pushed to the synth.
    pub sing_riff_bar: Option<u64>,
    /// Typed-word dog cameo lifecycle ([`crate::dog_cameo`]), summoned by `kitty_summon`.
    pub dog_cameo: DogCameo,
    /// The session the live dog visit was summoned in.
    pub dog_cameo_session: Option<u64>,
}

impl Companions {
    /// Feed one classified press to the tone tracker, the sing-along (with its Enter edge) and the
    /// summon detector.
    pub fn note_typed_press(&mut self, press: TypedPress<'_>, feed: PressFeed<'_>) -> TypedHit {
        let PressFeed {
            session,
            now,
            words,
            tone_active,
            keyed_enter,
        } = feed;
        press.feed(
            &mut self.tone_tracker,
            |tone| tone.note_break(),
            |tone| tone.note_backspace(session),
            |tone, c| tone.note_char(now, session, c, tone_active),
        );
        press.feed(
            &mut self.kitty_sing,
            |sing| sing.note_break(now),
            |sing| sing.note_backspace(now),
            |sing, c| sing.note_char(now, session, c),
        );
        if keyed_enter {
            self.kitty_sing.note_keyed_enter(now, session);
        }
        let mut fired = TypedHit::default();
        press.feed(
            &mut self.kitty_summon,
            |kitty| kitty.note_break(),
            |kitty| kitty.note_backspace(session),
            |kitty, c| {
                if let Some((lexicon, opts)) = words {
                    fired = fired.merge(kitty.note_char(now, session, c, lexicon, opts));
                }
            },
        );
        fired
    }

    /// Serious Mode's drain, the companions' half.
    #[inline]
    pub fn drain_serious(&mut self) {
        self.cursor_cat = CursorCat::default();
        self.dog_cameo = DogCameo::default();
        self.dog_cameo_session = None;
        self.kitty_sing.drain_serious();
        self.music_notes.clear();
        self.sing_riff_bar = None;
        self.kitty_summon.drain_serious();
        self.tone_tracker.drain_serious();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::kitty_sing::CelebrateOn;
    use crate::kitty_summon::DOG_SUMMON_KEYS;

    fn glyph(c: char) -> TypedPress<'static> {
        TypedPress {
            typed: Some(c),
            ..TypedPress::default()
        }
    }

    fn feed_line(
        c: &mut Companions,
        line: &str,
        words: Option<(&Lexicon, &ScanOptions<'_>)>,
        now: Instant,
    ) -> TypedHit {
        line.chars().fold(TypedHit::default(), |hit, ch| {
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
}
