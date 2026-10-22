// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The companions a window keeps beside its resident pet: the earned flying
//! head, its sing-along and music notes, the typed-word detectors (kitty summon,
//! tone of typing) and the typed dog. The host feeds them from its
//! committed key-press path and performs what they hand back (a Kitty Log row, a
//! notice, a sound). None reads a clock: every call takes `now`.
//!
//! The kitty-command listener ([`crate::typed_tricks`]) has its own feed cascade
//! and Serious Mode rule, so the host drives it separately.

use aterm_lexicon::{Lexicon, ScanOptions};
use aterm_time::Instant;

use crate::dog_cameo::DogCameo;
use crate::kitty_cursor::CursorCat;
use crate::kitty_sing::{KittySing, MusicNotes};
use crate::kitty_summon::{TypedHit, TypedKittySummon};
use crate::tone_infer::ToneTracker;

/// One classified key press from the host's committed key path, the only input
/// the typed detectors have. PTY output and pastes never make one, so nothing
/// on screen can steer a detector.
#[derive(Clone, Copy, Debug, Default)]
pub struct TypedPress<'ev> {
    /// A key that edits or moves beyond one glyph (Enter, Tab, Escape,
    /// Delete, navigation, a kill or nav chord, raw controller bytes): every
    /// detector's window resets.
    pub brk: bool,
    /// A plain Backspace: each window pops one glyph.
    pub backspace: bool,
    /// The bare printed glyph.
    pub typed: Option<char>,
    /// A committed IME run, fed char by char after `typed`.
    pub ime: Option<&'ev str>,
}

impl<'ev> TypedPress<'ev> {
    /// The brk → backspace → glyphs cascade every feed takes: `on_char` runs
    /// for the bare glyph and for each char of the IME run; `target` threads
    /// the feed's receiver through so no two callbacks borrow it at once.
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
    /// The compiled vocabulary and its scan options, while sparkle words
    /// are on. `None`: glyphs skip the summon detector and nothing fires;
    /// a break or a Backspace still maintains its window.
    pub words: Option<(&'a Lexicon, &'a ScanOptions<'a>)>,
    /// Tone inference may run (the host's `tone_infer_active`); the window
    /// is maintained either way.
    pub tone_active: bool,
    /// The press was a keyed Enter — the edge an armed `on=enter`
    /// celebration waits for.
    pub keyed_enter: bool,
}

/// One window's companions, reached through
/// [`crate::pipeline::EffectsPipeline::companions_mut`].
#[derive(Default)]
pub struct Companions {
    /// The earned cursor cat's decision layer ([`crate::kitty_cursor`]): stamped
    /// on the input path, ticked each frame.
    pub cursor_cat: CursorCat,
    /// Typed-"kitty" summon detector ([`crate::kitty_summon`]): recent printed
    /// keystrokes keyed to their session, plus the per-window cooldown.
    pub kitty_summon: TypedKittySummon,
    /// Tone-of-typing tracker ([`crate::tone_infer`]), whose verdict rides every
    /// trail [`crate::trail_sound::SoundEvent`] as the melody's tone.
    pub tone_tracker: ToneTracker,
    /// Sing-along detector ([`crate::kitty_sing`]): the held-key repeat run.
    /// Style gating (the rainbow kitty trail) is applied where the drive is
    /// consumed, not at the feed: the detector is O(1) state, and always
    /// feeding it keeps arming correct across a mid-hold style switch.
    pub kitty_sing: KittySing,
    /// The celebration's ♪/♫ music-note ring (bounded at
    /// [`crate::kitty_sing::MAX_NOTES`]): spawned on the shared beat clock
    /// while armed, drained through its own lifetimes on wind-down.
    pub music_notes: MusicNotes,
    /// Last riff bar index pushed to the synth — the once-per-bar latch for
    /// the celebration's riff (cleared when the drive rests).
    pub sing_riff_bar: Option<u64>,
    /// Typed-word dog cameo lifecycle ([`crate::dog_cameo`]), summoned by
    /// `kitty_summon`. Presentation state only; dogs are never recorded.
    pub dog_cameo: DogCameo,
    /// The session the live dog visit was summoned in. The pipeline is per
    /// window and a tab switch retires nothing, so this tag alone keeps a
    /// visitor summoned in tab A off tab B's glass.
    pub dog_cameo_session: Option<u64>,
}

impl Companions {
    /// Feed one classified press to the tone tracker, the sing-along (with its
    /// Enter edge) and the summon detector. Returns what completed words ask for
    /// (a Kitty Log row, a reaction, the dog). The summon detector takes glyphs
    /// only while `feed` lends it a vocabulary.
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
        // Printed glyphs extend the same-key run (`SING_ARM_REPEATS` at repeat
        // cadence arm the song); Backspace and every break release it into a
        // wind-down, never a hard cut.
        press.feed(
            &mut self.kitty_sing,
            |sing| sing.note_break(now),
            |sing| sing.note_backspace(now),
            |sing, c| sing.note_char(now, session, c),
        );
        if keyed_enter {
            self.kitty_sing.note_keyed_enter(now, session);
        }
        // `merge` folds one IME commit: a granted record outranks a
        // cooldown-suppressed one, and both family reactions are kept.
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

    /// Serious Mode's drain, the companions' half: the flying head, the dog
    /// cameo, the sing-along with its notes and riff latch, the summon
    /// detector, and the tone tracker's partial window return to rest while
    /// keeping warmed-up buffers and cooldowns intact. Idempotent: it runs
    /// every frame while Serious Mode holds.
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

    /// Glyphs extend every window, a Backspace pops one glyph, a break clears
    /// them all; with no vocabulary nothing fires but the tone window is fed.
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

        // Warm up the canine odometer and fire the Kitty Log cooldown.
        for _ in 0..DOG_SUMMON_KEYS {
            feed_line(&mut c, "a", words, now);
        }
        let first = feed_line(&mut c, " kitty", words, now);
        assert!(first.feline && first.records, "first summon records");

        // Summon a dog cameo, arm an external celebration, and start typing `kitt`.
        c.dog_cameo.on_summon(now, 42);
        c.dog_cameo_session = Some(7);
        assert!(c.dog_cameo.active(now));
        c.kitty_sing
            .arm_external(now, 7, 1, 2, CelebrateOn::Enter)
            .expect("initial arm succeeds");
        feed_line(&mut c, " kitt", words, now);

        c.drain_serious();

        // Active dog cameo, pending celebration arm, and partial typing window are gone.
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

        // Cooldowns and the canine keystroke odometer survive across Serious Mode.
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
