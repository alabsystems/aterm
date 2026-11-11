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

/// Classified key press from the committed key path.
#[derive(Clone, Copy, Debug)]
pub struct TypedPress<'ev> {
    /// Key that edits or moves beyond one glyph, resetting detector windows.
    pub brk: bool,
    /// Plain Backspace, popping one glyph from each window.
    pub backspace: bool,
    /// The bare printed glyph.
    pub typed: Option<char>,
    /// Committed IME text, fed after `typed`.
    pub ime: Option<&'ev str>,
}

impl Default for TypedPress<'_> {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

impl<'ev> TypedPress<'ev> {
    /// An empty key press with no break, backspace, bare glyph, or IME commit.
    pub const NONE: Self = Self {
        brk: false,
        backspace: false,
        typed: None,
        ime: None,
    };

    /// Apply the `brk` -> `backspace` -> `chars` cascade to `target`.
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
            if let Some(c) = self.typed {
                on_char(target, c);
            }
            if let Some(ime) = self.ime {
                for c in ime.chars() {
                    on_char(target, c);
                }
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

/// Context for [`Companions::note_typed_press`].
#[derive(Clone, Copy)]
pub struct PressFeed<'a> {
    /// The session the press went to.
    pub session: u64,
    pub now: Instant,
    /// Compiled vocabulary and scan options when sparkle words are enabled.
    pub words: Option<(&'a Lexicon, &'a ScanOptions<'a>)>,
    /// Tone inference may run (the host's `tone_infer_active`).
    pub tone_active: bool,
    /// Whether the press was a keyed Enter (for `on=enter` celebrations).
    pub keyed_enter: bool,
}

/// Per-window companion detectors and cameo state.
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
    /// Session in which the active dog cameo was summoned.
    pub dog_cameo_session: Option<u64>,
}

impl Default for Companions {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Companions {
    /// Construct a fresh per-window companion state with all detectors at rest.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            cursor_cat: CursorCat::new(),
            kitty_summon: TypedKittySummon::new(),
            tone_tracker: ToneTracker::new(),
            kitty_sing: KittySing::new(),
            music_notes: MusicNotes::new(),
            sing_riff_bar: None,
            dog_cameo: DogCameo::new(),
            dog_cameo_session: None,
        }
    }

    /// Feed a classified key press to the tone, sing-along, and summon detectors.
    #[inline]
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
            ToneTracker::note_break,
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
        let mut fired = TypedHit::NONE;
        if let Some((lexicon, opts)) = words {
            press.feed(
                &mut self.kitty_summon,
                TypedKittySummon::note_break,
                |summon| summon.note_backspace(session),
                |summon, c| {
                    fired = fired.merge(summon.note_char(now, session, c, lexicon, opts));
                },
            );
        } else if press.brk {
            self.kitty_summon.note_break();
        } else if press.backspace {
            self.kitty_summon.note_backspace(session);
        }
        fired
    }

    /// Reset transient companion state for Serious Mode.
    #[inline]
    pub fn drain_serious(&mut self) {
        self.cursor_cat = CursorCat::new();
        self.dog_cameo = DogCameo::new();
        self.dog_cameo_session = None;
        self.kitty_sing.drain_serious();
        self.music_notes.clear();
        self.sing_riff_bar = None;
        self.kitty_summon.drain_serious();
        self.tone_tracker.drain_serious();
    }
}

#[cfg(test)]
#[path = "companions_tests.rs"]
mod tests;
