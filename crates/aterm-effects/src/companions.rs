// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The companions a window keeps beside its resident pet: the earned flying
//! head, its sing-along and music notes, the typed-word detectors (kitty summon,
//! tone of typing), the typed dog and Robi. The host feeds them from its
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
use crate::robi::RobiShow;
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
    /// Robi the helper robot ([`crate::robi`]): the resident's birth instant and
    /// tip roll; everything else derives per frame. Once born he stays on the
    /// glass (per window, on whichever tab fronts); `stop` is only for
    /// config/motion/Serious Mode hygiene.
    pub robi_show: RobiShow,
    /// Lifetime (re)birth counter — the seed fold, so name-summons rotate
    /// through different tips.
    pub robi_shows: u64,
    /// Rolling lowercase 8-char tail of recent printed keystrokes, matched
    /// literally against `robi`/`robot` (no lexicon class).
    pub robi_tail: String,
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

    /// Robi's name: printed glyphs extend a rolling lowercase 8-char tail, and
    /// a tail ending in `robi` or `robot` pokes him: the tail clears (holding
    /// the last key cannot re-poke) and his show restarts at the greeting.
    /// Returns whether he was poked. The host gates this (config, motion,
    /// Serious Mode) as it gates his draw.
    pub fn note_robi_press(&mut self, press: TypedPress<'_>, now: Instant) -> bool {
        if press.brk || press.backspace {
            return false;
        }
        let mut typed = false;
        for c in press.chars() {
            typed = true;
            for lc in c.to_lowercase() {
                self.robi_tail.push(lc);
            }
            while self.robi_tail.chars().count() > 8 {
                let first_len = self.robi_tail.chars().next().map_or(0, char::len_utf8);
                self.robi_tail.drain(..first_len);
            }
        }
        if !typed || !(self.robi_tail.ends_with("robi") || self.robi_tail.ends_with("robot")) {
            return false;
        }
        self.robi_tail.clear();
        self.robi_shows += 1;
        self.robi_show.start(now, self.robi_shows);
        true
    }

    /// Serious Mode's drain, the companions' half: the flying head, the
    /// sing-along with its notes and riff latch, and the summon detector return
    /// to rest. Idempotent: it runs every frame while Serious Mode holds.
    pub fn drain_serious(&mut self) {
        self.cursor_cat = CursorCat::default();
        self.kitty_sing = KittySing::default();
        self.music_notes = MusicNotes::default();
        self.sing_riff_bar = None;
        self.kitty_summon = TypedKittySummon::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Robi answers to his name, in any case and through an IME commit;
    /// the poke clears the tail, so holding the last key cannot poke again;
    /// a break or a Backspace never pokes.
    #[test]
    fn robi_answers_to_his_name_once_per_spelling() {
        let now = Instant::now();
        let mut c = Companions::default();
        for ch in "ROBO".chars() {
            assert!(!c.note_robi_press(glyph(ch), now));
        }
        assert!(c.note_robi_press(glyph('T'), now), "`robot` pokes him");
        assert_eq!(c.robi_shows, 1);
        assert!(!c.note_robi_press(glyph('t'), now), "the tail cleared");
        let ime = TypedPress {
            ime: Some("xrobi"),
            ..TypedPress::default()
        };
        assert!(c.note_robi_press(ime, now), "an IME commit spells it too");
        for ch in "rob".chars() {
            c.note_robi_press(glyph(ch), now);
        }
        let brk = TypedPress {
            brk: true,
            typed: Some('i'),
            ..TypedPress::default()
        };
        assert!(!c.note_robi_press(brk, now), "a break never pokes");
        assert_eq!(c.robi_shows, 2);
    }
}
