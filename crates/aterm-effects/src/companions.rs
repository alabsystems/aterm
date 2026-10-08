// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE COMPANIONS A WINDOW KEEPS BESIDE ITS RESIDENT PET (host-boundary
//! Phase 5): the earned flying head, its sing-along and music notes, the
//! typed-word detectors (the kitty summon, the tone of typing), the typed dog
//! and Robi. Each is an engine the window's
//! [`crate::pipeline::EffectsPipeline`] owns; the host feeds them from its
//! committed key-press path and performs what they hand back (a Kitty Log
//! row, a notice, a sound). None reads a clock: every call takes `now`.
//!
//! The kitty-command listener ([`crate::typed_tricks`]) is not here: it is
//! the bookkeeping of the typed line, with its own feed cascade and its own
//! Serious Mode rule, and the host drives it beside these.

use aterm_lexicon::{Lexicon, ScanOptions};
use aterm_time::Instant;

use crate::dog_cameo::DogCameo;
use crate::kitty_cursor::CursorCat;
use crate::kitty_sing::{KittySing, MusicNotes};
use crate::kitty_summon::{TypedHit, TypedKittySummon};
use crate::robi::RobiShow;
use crate::tone_infer::ToneTracker;

/// ONE CLASSIFIED KEY PRESS, as the host's committed key path read it — the
/// only input the typed detectors have. PTY output and pastes never make
/// one, so nothing on screen can steer a detector.
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

/// One window's companions. Plain data: every field is an engine with its
/// own laws, and the window reaches it through
/// [`crate::pipeline::EffectsPipeline::companions_mut`].
#[derive(Default)]
pub struct Companions {
    /// The rare cursor cat's decision layer: forward-momentum + a sustain
    /// clock + a fade/exit state machine. It is EARNED — appears only after
    /// sustained fast forward typing AND a rarity roll, then fades out
    /// completely (sometimes with a star wink or heart meow). Stamped on the
    /// input path, ticked each frame.
    pub cursor_cat: CursorCat,
    /// TYPED-"kitty" summon detector ([`crate::kitty_summon`]): a bounded
    /// rolling window of this window's recent printed keystrokes (keyed to
    /// the session they were typed into) plus the per-window summon
    /// cooldown. Fed only on the committed key-press path — it never sees
    /// screen content, so `cat`ing a file full of "kitty" cannot fire it.
    pub kitty_summon: TypedKittySummon,
    /// TONE-of-typing tracker ([`crate::tone_infer`]): the bounded typed-line
    /// window + throttled classifier cache whose verdict rides every trail
    /// [`crate::trail_sound::SoundEvent`] as the melody's tone. Fed on the
    /// same committed key-press path as `kitty_summon`, classifying only
    /// while the host says tone inference is active.
    pub tone_tracker: ToneTracker,
    /// SING-ALONG detector ([`crate::kitty_sing`]): the held-key repeat run,
    /// fed on the same committed key-press path as `kitty_summon` (typed
    /// provenance only — PTY output and pastes can never arm the
    /// celebration). Style gating (the rainbow kitty trail) is applied where
    /// the drive is CONSUMED, not at the feed: the detector is O(1) scalar
    /// state, and feeding it always keeps arming correct across a mid-hold
    /// style switch.
    pub kitty_sing: KittySing,
    /// The celebration's ♪/♫ music-note ring (bounded at
    /// [`crate::kitty_sing::MAX_NOTES`]): spawned on the shared beat clock
    /// while armed, drained through its own lifetimes on wind-down.
    pub music_notes: MusicNotes,
    /// Last riff bar index pushed to the synth — the once-per-bar latch for
    /// the celebration's riff (cleared when the drive rests).
    pub sing_riff_bar: Option<u64>,
    /// TYPED-word DOG cameo lifecycle ([`crate::dog_cameo`]): the envelope +
    /// rolled breed for the canine summon `kitty_summon` fires once its
    /// typed-a-lot gate opens. Pure presentation state — dogs are never
    /// recorded anywhere.
    pub dog_cameo: DogCameo,
    /// The session the live dog visit was summoned in — the dog's pane/tab
    /// scope: a visitor summoned in tab A must never present on tab B's
    /// glass. The pipeline is per WINDOW and a tab switch retires nothing,
    /// so this tag is the only thing that keeps it off the wrong grid.
    pub dog_cameo_session: Option<u64>,
    /// ROBI THE HELPER ROBOT ([`crate::robi`]): the resident's birth instant +
    /// tip roll; everything else derives per frame. Once born he is on the
    /// glass at all times (per window — he walks whatever tab fronts);
    /// `stop` only for config/motion/serious hygiene.
    pub robi_show: RobiShow,
    /// Lifetime (re)birth counter — the seed fold, so name-summons rotate
    /// through different tips.
    pub robi_shows: u64,
    /// Rolling lowercase tail of recent printed keystrokes (8 chars), the
    /// literal `robi`/`robot` summon matcher — no lexicon class, he answers
    /// to his name. Fed on the committed key-press path only, like the cats.
    pub robi_tail: String,
}

impl Companions {
    /// THE TYPED DETECTORS' FEED: one classified press into the tone of
    /// typing, the sing-along (with its Enter edge) and the summon detector,
    /// each by the same cascade ([`TypedPress::feed`]). Returns what the
    /// words the press completed ask for — a Kitty Log row, a reaction, the
    /// dog — for the host to perform. The tone window is kept whatever the
    /// gates (only its classifier is gated); the summon detector takes
    /// glyphs only while `feed` lends it a vocabulary.
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
        // PRINTED characters extend the same-key run (16 at repeat cadence
        // arm the song); Backspace and every break release it — a graceful
        // wind-down, never a hard cut. Style gating is the consumer's.
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

    /// ROBI'S NAME: the rolling lowercase 8-char tail of a printed press —
    /// no lexicon class, he answers to `robi` or `robot`. A match POKES him:
    /// the tail clears (holding the last key cannot re-poke) and his show
    /// restarts at the greeting. Returns whether he was poked. The host
    /// gates this (his config, motion and Serious Mode) as it gates his draw.
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

    /// SERIOUS MODE'S DRAIN, the companions' half: the flying head, the
    /// sing-along with its notes and riff latch, and the summon detector
    /// return to rest. Idempotent — the drain runs on every frame while
    /// Serious Mode holds.
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

    /// One press reaches the tone window, the sing-along and the summon
    /// detector by the same cascade: glyphs extend every window, a
    /// Backspace pops one glyph, a break clears them all; with no
    /// vocabulary nothing fires and the tone window is still fed.
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
