// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KITTY-COMMAND LISTENER: the typed-LINE state machine that decides when
//! a word the person just typed — `sit`, `kitty jump`, `good kitty` — is a
//! command to the cursor pet.
//!
//! It lives in the ENGINE (the host-boundary ruling: typed detectors belong to
//! `aterm-effects`; a host only feeds keys) and it is pure: no clock is read
//! (every feed call takes the injected `now`), nothing is drawn, nothing is
//! latched into the pet. It answers with a [`TrickEvent`] and the HOST decides
//! what an event is allowed to do (feature gates, serious mode, ignore lists).
//! A host feeds it ALWAYS, gates or no gates: line state that skipped the
//! editing done while a feature was off would call a half-typed line pure on
//! re-enable.
//!
//! # The firing law: a WORD BOUNDARY on a PURE line
//!
//! The typed-word summon detector fires on the completing LETTER. That law is
//! wrong here — `sit` would fire inside `site` — so a trick fires only when its
//! token is CLOSED (whitespace, Enter, sentence punctuation), and only while
//! the line is PURE: every token typed on it so far was pet vocabulary (a
//! trick word, a name for the pet, a filler) and no code punctuation has
//! appeared. `npm run build`, `git fetch`, `./sit`, `--sit`, `sit.txt`,
//! `sit: 3`, `sit = 3` and `play.py` are all impure before, at or right after
//! the trick word and never move the pet.
//!
//! Purity alone over-fires: half of all imperative prose STARTS with a trick
//! word (`roll back the last commit`, `hide the sidebar`, `sleep 5`). So there
//! are two tiers:
//!
//! * **Addressed** — the line also names the pet (`kitty sit`, `good kitty`).
//!   The fire is final.
//! * **Tentative** — a plain word on a line that has not named the pet. It is
//!   reported with `addressed = false`, and it is REVOKED the moment the line
//!   turns impure (`sit tight` revokes at `tight`). It is CONFIRMED by Enter on
//!   a still-pure line, or by a later name (`sit kitty`). The host settles a
//!   tentative fire slowly enough that ordinary prose revokes it first.
//!
//! An ADDRESS word (`good`, `no`, `look` — words that open too many ordinary
//! lines to ever fire alone) is HELD until the pet is named, and then fires
//! with the address word as its word: `good kitty` purrs at `kitty`, flashing
//! `good`. A name never flashes and never fires by itself.
//!
//! # One phrase, one trick
//!
//! People say `sit down`, `roll over`, `play dead`, `jump up`. The FIRST fire
//! of a phrase wins: a later trick word on the same pure line whose boundary
//! arrives within [`PHRASE_GAP`] of the previous fire is absorbed like a
//! filler — nothing fires, nothing is held. After the gap it is a new request
//! (`sit␣` … watch the cat … `jump␣` fires twice). A plain fire clears a held
//! address word, and a name that arrives while a tentative fire is live
//! confirms THAT fire and never also fires the held one: `good night kitty` is
//! one sleep, never a purr.
//!
//! # Punctuation
//!
//! * Whitespace (U+3000 included) closes a token. With no open token it is a
//!   no-op, so a leading space or a double space costs nothing.
//! * `. ! ? , …` close a token but PARK what it would have fired: `sit.` fires
//!   at the following Space or Enter, `sit!!` keeps the parked fire, and
//!   `sit.t` — a token character glued to the stop — cancels it and the line is
//!   impure (that is `sit.txt`, `v1.2`, `a,b`). The same glue law holds with
//!   nothing parked. At the very start of a line these are code (`!sit`,
//!   `.hidden`).
//! * `。！？、，` are full boundaries: fullwidth punctuation has no code meaning,
//!   so `ねこ、おすわり！` fires at `！` with no trailing space.
//! * Everything else (`: ; / \ = @ # + ~ $ % | < > * & ^ ( ) [ ] { } " \``, a
//!   joiner with no token to join) makes the line impure. `:` and `;` are code
//!   on purpose: `play: 3` is YAML.
//!
//! The lexicon's code-context rule for a scanned token (`left_suppresses` /
//! `right_suppresses`) needs no second implementation here, because on a pure
//! line it cannot trip: the character left of an open token is always the line
//! start, whitespace or a fullwidth stop, and the character that closes it is
//! one of the three kinds above.
//!
//! # No-space scripts and the IME
//!
//! A no-space-script run (kana, ideographs, Hangul, Thai) is a token like any
//! other and is looked up whole at its boundary (the vocabulary's raw-key
//! law). The END OF AN IME COMMIT IS NOT A BOUNDARY: the macOS Korean IME
//! commits one syllable per call, so `앉아` arrives as `앉` then `아`, and a
//! pinyin user who commits `跳` alone is on the way to `跳过这个测试`. Runs
//! accumulate across [`TrickListener::note_char`] and
//! [`TrickListener::note_ime`] alike and are judged only at a real boundary.
//!
//! # Typo recovery
//!
//! A direct request that goes unanswered reads as broken, so fixing a typo
//! must not kill the line:
//!
//! * The listener counts the characters typed on the line. Backspace takes
//!   them off again (a zero-width scalar goes with its base, as readline and
//!   zsh delete it), and a count of ZERO is a provably empty line: full reset
//!   to fresh-pure, whatever the line had become. Backspace at zero is a
//!   no-op, so a held key that overshoots costs nothing.
//! * ONE boundary can be crossed backwards: closing a token snapshots the
//!   line as it stood, and backspacing over the closing character restores it
//!   (`siy␣⌫⌫t␣` fires). A tentative fire made at that boundary is revoked on
//!   the way back. Crossing a SECOND boundary has no snapshot: what stands
//!   left of the caret is unknown, so the line is impure from there — but it
//!   is still COUNTED, so holding Backspace to the start recovers any line.
//! * A word-kill (Ctrl+W) on an open alphanumeric token that follows
//!   whitespace just clears the token, and on an empty line it is a no-op;
//!   any other word-kill removed text the listener cannot measure: poisoned.
//!
//! # Poison
//!
//! Tab, Esc, arrows, other chords, a paste, raw bytes from a controller, a
//! no-echo prompt: the line now holds something the listener did not see, or
//! the caret left the end of the line. POISONED means nothing fires and
//! nothing is counted until a line reset (Enter, Ctrl+C, Ctrl+U, …). A live
//! tentative fire is revoked. A no-echo prompt poisons for privacy: a
//! passphrase must not move the cat. So does a character no line editor gives
//! a column of its own (a control character, ZWJ, a variation selector, a
//! combining mark with no base): Backspace would stop matching the count.
//!
//! # Cost
//!
//! One inline push per letter — no lexicon work, no allocation, no fold. One
//! fold plus one hash probe per closed token, and only while the line is
//! still pure (an ordinary prose line stops asking after its first word). The
//! host need not even FETCH its vocabulary for the other presses: see
//! [`TrickListener::needs_lexicon`]. Everything but the fold scratch is inline
//! and sized by [`TrickLexicon::MAX_SURFACE_CHARS`]; after the scratch has
//! warmed the listener never allocates (pinned by
//! `tests/typed_tricks_alloc.rs`).

use aterm_lexicon::{
    Trick, TrickLexicon, TrickRole, is_interior_joiner, is_no_space_script, is_token_char,
};
use aterm_time::{Duration, Instant};

/// ONE PHRASE, ONE TRICK: a trick word whose boundary arrives within this long
/// of the previous fire on the same line belongs to that fire's phrase
/// (`sit down`, `roll over`) and is absorbed. Later than this it is a new
/// request. 1.5 s is a generous inter-word gap for someone typing a phrase and
/// well short of "watched the cat do it, now asks for the next thing".
pub const PHRASE_GAP: Duration = Duration::from_millis(1500);

/// The token window, in characters: exactly the longest surface the
/// vocabulary can hold, so a token that overflows it could never have matched.
const TOKEN_CHARS: usize = TrickLexicon::MAX_SURFACE_CHARS;

/// Bytes behind one inline word: every character is at most four.
const WORD_BYTES: usize = TOKEN_CHARS * 4;

// `Word::len` is a `u8`.
const _: () = assert!(WORD_BYTES <= u8::MAX as usize);

/// How many characters of the delimiter run left of the caret are remembered,
/// so Backspace inside the run (`sit!!⌫`, a double space) knows what the run
/// now ends with. A longer run is only counted: backspacing into its unseen
/// part makes the line impure until the whole run is gone again.
const GAP_WINDOW: usize = 8;

/// Dirty sessions remembered across switches (least recently left is
/// forgotten first, and a forgotten session reads as new).
const SESSION_MEMORY: usize = 8;

/// What the listener heard. At most ONE per fed press.
///
/// `word` borrows the listener's inline buffer: it is the TRICK token exactly
/// as typed (`SIT`, `goooood`) — never the name that completed an address —
/// because it is what the flash has to find on the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrickEvent<'a> {
    /// A trick word fired.
    Fire {
        /// The trick asked for.
        trick: Trick,
        /// The trick token as typed.
        word: &'a str,
        /// How many typed characters separate `word` from the caret AS IT
        /// STOOD WHEN THIS PRESS ARRIVED (its echo has not happened yet). `0`
        /// for a plain fire: the word ends at the caret. For a held fire it is
        /// what was typed since the address word (`good kitty␣`: ` kitty`, 6);
        /// for a parked one, the punctuation (`sit!!␣`: 2). For a word that
        /// arrived INSIDE a multi-character IME commit the caret still stands
        /// before the whole commit, and this counts the commit's characters
        /// that precede the word (`ねこ、おすわり！`: 3).
        back_chars: u16,
        /// `true`: FINAL — the line names the pet, or Enter submitted a pure
        /// line. `false`: TENTATIVE — a [`TrickEvent::Confirm`] or a
        /// [`TrickEvent::Revoke`] may follow.
        addressed: bool,
    },
    /// The live tentative fire is final after all: Enter on a still-pure line,
    /// or the line went on to name the pet. Carries the trick so a host need
    /// not remember what it was told.
    Confirm {
        /// The trick of the fire being confirmed.
        trick: Trick,
    },
    /// The live tentative fire is withdrawn: the line turned impure, was
    /// aborted, was poisoned, or its word was backspaced into.
    Revoke,
}

/// The answer to one fed press.
#[must_use = "an unread feed is a lost fire, confirm or revoke"]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TrickFeed<'a> {
    /// What the press meant to the pet, if anything.
    pub event: Option<TrickEvent<'a>>,
    /// Enter submitted a line that was NOTHING BUT pet vocabulary (at least
    /// one word of it, pure, unpoisoned). At a shell such a line is a command
    /// that does not exist; the host uses this to forgive the exit-127 that
    /// follows. May accompany a `Fire` or a `Confirm`. Only
    /// [`TrickListener::note_submit`] sets it.
    pub submit_pet_only: bool,
}

impl TrickFeed<'_> {
    /// Nothing happened.
    pub const NONE: TrickFeed<'static> = TrickFeed {
        event: None,
        submit_pet_only: false,
    };
}

/// A word held inline: at most [`TOKEN_CHARS`] characters, no heap. `Copy`, so
/// the boundary snapshot, the held address word and a parked fire carry their
/// word by value and nothing has to be kept in step.
#[derive(Clone, Copy)]
struct Word {
    bytes: [u8; WORD_BYTES],
    len: u8,
}

impl Word {
    const EMPTY: Word = Word {
        bytes: [0; WORD_BYTES],
        len: 0,
    };

    /// Whole characters only ever go in and come out, so the prefix is always
    /// valid UTF-8; the checked conversion keeps that a fact the compiler need
    /// not take on trust (it runs at boundaries, never per letter).
    fn as_str(&self) -> &str {
        let bytes = self.bytes.get(..usize::from(self.len)).unwrap_or(&[]);
        std::str::from_utf8(bytes).unwrap_or("")
    }

    /// `false` (and nothing written) when the character does not fit — which
    /// the caller's character count already rules out.
    fn push(&mut self, c: char) -> bool {
        let start = usize::from(self.len);
        let end = start.saturating_add(c.len_utf8());
        let Some(slot) = self.bytes.get_mut(start..end) else {
            return false;
        };
        c.encode_utf8(slot);
        // `end <= WORD_BYTES <= u8::MAX` (const-asserted above).
        self.len = u8::try_from(end).unwrap_or(u8::MAX);
        true
    }

    fn pop(&mut self) -> Option<char> {
        let c = self.as_str().chars().next_back()?;
        let width = u8::try_from(c.len_utf8()).unwrap_or(u8::MAX);
        self.len = self.len.saturating_sub(width);
        Some(c)
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

impl std::fmt::Debug for Word {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

/// How the listener reads one typed character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A token character or a no-space-script character: part of a word.
    Word,
    /// `'` `’` `-`: part of a word when one is open, code otherwise.
    Joiner,
    /// Whitespace, U+3000 included.
    Space,
    /// `. ! ? , …` — closes a token and PARKS its fire (module docs).
    Stop,
    /// `。！？、，` — a full boundary.
    WideStop,
    /// Anything else: the line is code.
    Code,
    /// A control character or a zero-width scalar that is no part of a word
    /// (ZWJ, a variation selector, a stray newline inside a commit). A line
    /// editor does not give it a column of its own, so Backspace stops
    /// matching the listener's count: the line is outside the model.
    Alien,
}

/// The lexicon's own predicates decide what a word character is, so the
/// listener segments a typed line exactly as the vocabulary was segmented at
/// load; a re-implemented look-alike would drift.
fn kind_of(c: char) -> Kind {
    if is_token_char(c) || is_no_space_script(c) {
        return Kind::Word;
    }
    if is_interior_joiner(c) {
        return Kind::Joiner;
    }
    if c.is_control() || aterm_grapheme::char_width(c) == 0 {
        return Kind::Alien;
    }
    if c.is_whitespace() {
        return Kind::Space;
    }
    match c {
        '.' | '!' | '?' | ',' | '\u{2026}' => Kind::Stop,
        '\u{3002}' | '\u{FF01}' | '\u{FF1F}' | '\u{3001}' | '\u{FF0C}' => Kind::WideStop,
        _ => Kind::Code,
    }
}

/// How a token was closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edge {
    /// Whitespace, a fullwidth stop, Enter: a verdict is delivered now.
    Open,
    /// `. ! ? , …`: a verdict is parked until the next open edge.
    Stop,
}

/// An address word waiting for the pet to be named.
#[derive(Clone, Copy, Debug)]
struct Held {
    trick: Trick,
    word: Word,
    word_chars: u32,
    /// `line_chars` as the word's last character went in: what has been typed
    /// since is `line_chars - end_at`, Backspace included, with nothing to
    /// keep in step.
    end_at: u32,
}

/// What closing a token decided — delivered at an open edge, parked at a stop.
#[derive(Clone, Copy, Debug)]
enum Verdict {
    Fire {
        trick: Trick,
        word: Word,
        word_chars: u32,
        end_at: u32,
        addressed: bool,
    },
    Confirm {
        trick: Trick,
    },
}

/// The owned twin of [`TrickEvent`]: what one step produced, before the word
/// is borrowed out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    Fire {
        trick: Trick,
        back_chars: u16,
        addressed: bool,
    },
    Confirm {
        trick: Trick,
    },
    Revoke,
}

/// Fold the outcomes of one press (the characters of an IME commit; Enter's
/// closing token and Enter itself) into the one event a press may report. The
/// NEWEST wins — the pet has one slot and the host's revoke is idempotent —
/// except that a `Confirm` folds INTO the tentative `Fire` it confirms, so
/// `sit⏎` is one final fire rather than a fire the host never hears of.
fn fold(acc: Option<Outcome>, next: Option<Outcome>) -> Option<Outcome> {
    match (acc, next) {
        (acc, None) => acc,
        (
            Some(Outcome::Fire {
                trick, back_chars, ..
            }),
            Some(Outcome::Confirm { trick: confirmed }),
        ) if trick == confirmed => Some(Outcome::Fire {
            trick,
            back_chars,
            addressed: true,
        }),
        (_, next) => next,
    }
}

/// [`TrickEvent::Fire::back_chars`]. `since_end`: characters typed after the
/// word and before this press's closing character. `fed`: characters of THIS
/// press already consumed (0 for a key; the caret still stands before them).
/// So the word's end sits `since_end - fed` characters left of the caret, or,
/// when that is negative, the word lies to the caret's right: inside it the
/// distance is 0, beyond it what remains after the word's own length.
fn back_chars(since_end: u32, fed: u32, word_chars: u32) -> u16 {
    let distance = if fed <= since_end {
        since_end.saturating_sub(fed)
    } else {
        fed.saturating_sub(since_end).saturating_sub(word_chars)
    };
    u16::try_from(distance).unwrap_or(u16::MAX)
}

/// Everything a line reset clears. `Copy`: the boundary snapshot is one of
/// these by value.
#[derive(Clone, Copy, Debug)]
struct Line {
    /// The open token, RAW as typed (the vocabulary owns folding). Holds the
    /// first [`TOKEN_CHARS`] characters; `token_chars` keeps counting past
    /// them, and a token that overflowed is never looked up.
    token: Word,
    token_chars: u32,
    /// The delimiter run left of the open token (or of the caret), first
    /// [`GAP_WINDOW`] characters; `gap_chars` counts all of it.
    gap: [char; GAP_WINDOW],
    gap_chars: u32,
    /// The last delimiter typed; `None` at the start of the line.
    left: Option<char>,
    /// Characters typed on this line while unpoisoned, as Backspace will
    /// give them back (so a mark past the token window rides uncounted: see
    /// `step`). Zero is a provably empty line.
    line_chars: u32,
    /// Every token so far was pet vocabulary and no code has appeared.
    pure: bool,
    /// The line holds something the listener did not see. Only a reset lifts
    /// it.
    poisoned: bool,
    /// At least one vocabulary word has closed on this (pure) line.
    any_word: bool,
    /// The line has named the pet.
    vocative_seen: bool,
    held: Option<Held>,
    /// A verdict parked at a stop, waiting for the next open edge.
    tail: Option<Verdict>,
    /// The live tentative fire.
    tentative: Option<Trick>,
    /// When the last fire on this line was delivered (the phrase law).
    last_fire: Option<Instant>,
    /// Which trick that fire asked for (the attention law).
    last_trick: Option<Trick>,
}

impl Line {
    const FRESH: Line = Line {
        token: Word::EMPTY,
        token_chars: 0,
        gap: [' '; GAP_WINDOW],
        gap_chars: 0,
        left: None,
        line_chars: 0,
        pure: true,
        poisoned: false,
        any_word: false,
        vocative_seen: false,
        held: None,
        tail: None,
        tentative: None,
        last_fire: None,
        last_trick: None,
    };

    /// The open token can still be looked up: non-empty and inside the window.
    fn token_is_live(&self) -> bool {
        self.token_chars > 0 && self.token_chars as usize <= TOKEN_CHARS
    }
}

/// The per-window kitty-command listener, keyed to the window's front session
/// ([`rekey`](Self::rekey)). See the module docs for the law; the feed calls
/// below are the whole host surface, in the order a host's press cascade
/// consults them:
///
/// ```text
/// the tty swallows input (no echo)      note_no_echo   (Enter there: note_line_reset)
/// Enter                                 note_submit
/// raw bytes ending in CR / LF           note_line_reset
/// Ctrl+C  Ctrl+Z  Ctrl+\  Ctrl+J  Ctrl+M  Ctrl+U
///                                       note_line_reset
/// Ctrl+W  Alt+Backspace                 note_word_kill
/// Ctrl+K  forward Delete                nothing (the caret is at the end of an
///                                       unpoisoned line by construction)
/// Tab Esc arrows chords raw bytes PASTE note_break
/// Backspace                             note_backspace
/// a typed character / an IME commit     note_char / note_ime
/// ```
#[derive(Debug)]
pub struct TrickListener {
    session: Option<u64>,
    /// Sessions that were left with a dirty line, most recently left first.
    dirty: [Option<u64>; SESSION_MEMORY],
    line: Line,
    /// The line as it stood just before its last token was closed.
    snapshot: Option<Line>,
    /// The word of the last fire; what a returned [`TrickEvent::Fire`] borrows.
    word: Word,
    /// The vocabulary's fold buffer — the one heap allocation here, grown to
    /// the longest token looked up and then reused.
    scratch: String,
}

impl Default for TrickListener {
    fn default() -> Self {
        Self::new()
    }
}

impl TrickListener {
    /// A listener on a fresh, pure line, bound to no session yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            session: None,
            dirty: [None; SESSION_MEMORY],
            line: Line::FRESH,
            snapshot: None,
            word: Word::EMPTY,
            scratch: String::new(),
        }
    }

    // ---- the lexicon seam ----
    //
    // The vocabulary is consulted ONLY when a token closes on a still-pure
    // line, so a host that keeps it behind a shared pointer need not touch the
    // pointer for a plain letter — or for any press at all once an ordinary
    // line has gone impure. Each feed call that can close a token takes
    // `Option<&TrickLexicon>` and has a predicate saying whether THIS press
    // needs it; a host passes `None` whenever the predicate says `false`.
    // Passing `None` when it said `true` fails CLOSED: the token is judged
    // unknown and the line goes impure.

    /// Whether [`note_char`](Self::note_char) with `c` would consult the
    /// vocabulary: `c` closes a token, a token that can be looked up is open,
    /// and the line is still pure and unpoisoned.
    #[must_use]
    pub fn needs_lexicon(&self, c: char) -> bool {
        self.judging()
            && self.line.token_is_live()
            && matches!(kind_of(c), Kind::Space | Kind::Stop | Kind::WideStop)
    }

    /// [`needs_lexicon`](Self::needs_lexicon) for an IME commit: some
    /// character of `text` closes a token that is open now or opens inside
    /// `text`, before code punctuation in `text` makes the line impure. It may
    /// say `true` for a commit that turns out not to ask (a token that
    /// overflows inside it, a glued stop); it never says `false` for one that
    /// does.
    #[must_use]
    pub fn ime_needs_lexicon(&self, text: &str) -> bool {
        if !self.judging() {
            return false;
        }
        let mut open = self.line.token_is_live();
        for c in text.chars() {
            match kind_of(c) {
                Kind::Word => open = true,
                Kind::Joiner if open => {}
                Kind::Space | Kind::Stop | Kind::WideStop if open => return true,
                Kind::Space | Kind::Stop | Kind::WideStop => {}
                Kind::Joiner | Kind::Code | Kind::Alien => return false,
            }
        }
        false
    }

    /// [`needs_lexicon`](Self::needs_lexicon) for Enter.
    #[must_use]
    pub fn submit_needs_lexicon(&self) -> bool {
        self.judging() && self.line.token_is_live()
    }

    /// The line can still fire: pure and unpoisoned.
    fn judging(&self) -> bool {
        self.line.pure && !self.line.poisoned
    }

    // ---- observers (tests, the conformance bind) ----

    /// Every token so far was pet vocabulary and no code has appeared.
    #[must_use]
    pub fn is_pure(&self) -> bool {
        self.line.pure
    }

    /// The line holds something the listener did not see; nothing fires until
    /// a reset.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.line.poisoned
    }

    /// The live tentative fire, if any.
    #[must_use]
    pub fn tentative(&self) -> Option<Trick> {
        self.line.tentative
    }

    /// Characters typed on this line while unpoisoned.
    #[must_use]
    pub fn line_chars(&self) -> u32 {
        self.line.line_chars
    }

    // ---- sessions ----

    /// Bind the listener to `session`, the window's front session. Call it
    /// before feeding a press; the same session again is one compare.
    ///
    /// On a switch the OLD session is remembered as dirty when its line held
    /// typed characters or was poisoned. A session never seen (or left clean)
    /// starts fresh and pure; one that was left dirty comes back POISONED —
    /// returning to a tab with a half-typed line is not a fresh line, and the
    /// listener no longer knows what is on it. A live tentative fire is left
    /// to the host's own expiry: the switch says nothing about the line.
    pub fn rekey(&mut self, session: u64) {
        if self.session == Some(session) {
            return;
        }
        if let Some(old) = self.session {
            let dirty = self.line.line_chars > 0 || self.line.poisoned;
            self.forget(old);
            if dirty {
                // Most recently left first; the least recently left falls off.
                self.dirty.rotate_right(1);
                if let Some(front) = self.dirty.first_mut() {
                    *front = Some(old);
                }
            }
        }
        self.session = Some(session);
        let was_dirty = self.forget(session);
        self.line = Line::FRESH;
        self.line.poisoned = was_dirty;
        self.snapshot = None;
    }

    /// Drop `session` from the dirty memory; whether it was there.
    fn forget(&mut self, session: u64) -> bool {
        let Some(at) = self.dirty.iter().position(|s| *s == Some(session)) else {
            return false;
        };
        if let Some(rest) = self.dirty.get_mut(at..) {
            // Close the hole, keeping the order; the freed slot ends up last.
            rest.rotate_left(1);
        }
        if let Some(last) = self.dirty.last_mut() {
            *last = None;
        }
        true
    }

    // ---- feed calls ----

    /// One typed character (the shifted glyph; Space arrives here too).
    /// `lexicon` may be `None` unless [`needs_lexicon`](Self::needs_lexicon)
    /// says otherwise.
    pub fn note_char(
        &mut self,
        now: Instant,
        c: char,
        lexicon: Option<&TrickLexicon>,
    ) -> TrickFeed<'_> {
        let out = self.step(now, c, 0, lexicon);
        self.feed(out, false)
    }

    /// One IME commit. Its END is NOT a boundary (module docs): the characters
    /// are fed exactly as [`note_char`](Self::note_char) would feed them, and a
    /// run left open stays open for the next commit. `lexicon` may be `None`
    /// unless [`ime_needs_lexicon`](Self::ime_needs_lexicon) says otherwise.
    pub fn note_ime(
        &mut self,
        now: Instant,
        text: &str,
        lexicon: Option<&TrickLexicon>,
    ) -> TrickFeed<'_> {
        let mut out = None;
        let mut fed = 0u32;
        for c in text.chars() {
            out = fold(out, self.step(now, c, fed, lexicon));
            fed = fed.saturating_add(1);
        }
        self.feed(out, false)
    }

    /// Enter: close the open token (or deliver a parked verdict), confirm a
    /// live tentative fire if the line is still pure, report
    /// [`TrickFeed::submit_pet_only`], and start a fresh line. `lexicon` may
    /// be `None` unless [`submit_needs_lexicon`](Self::submit_needs_lexicon)
    /// says otherwise.
    pub fn note_submit(&mut self, now: Instant, lexicon: Option<&TrickLexicon>) -> TrickFeed<'_> {
        let mut out = None;
        let mut pet_only = false;
        if !self.line.poisoned {
            out = if self.line.token_chars > 0 {
                self.close_token(now, Edge::Open, 0, lexicon)
            } else {
                self.deliver_tail(now, 0)
            };
            if self.line.pure {
                // THE SUBMIT IS THE CONFIRMATION: a line that was nothing but
                // pet vocabulary, sent. (Folded into this press's own fire,
                // `sit⏎` reports one final `Fire`.)
                let confirm = self
                    .line
                    .tentative
                    .take()
                    .map(|trick| Outcome::Confirm { trick });
                out = fold(out, confirm);
                pet_only = self.line.any_word;
            }
        }
        self.line = Line::FRESH;
        self.snapshot = None;
        self.feed(out, pet_only)
    }

    /// The line was abandoned or emptied WITHOUT being judged: Ctrl+C, Ctrl+Z,
    /// Ctrl+\, Ctrl+J, Ctrl+M, Ctrl+U, raw controller bytes ending in CR / LF
    /// (`sit` typed, then a `send 'xyz\n'` — the line that ran was `sitxyz`,
    /// so the open `sit` must not be evaluated), and Enter at a no-echo
    /// prompt. Fresh pure line; a live tentative fire is revoked.
    pub fn note_line_reset(&mut self) -> TrickFeed<'_> {
        let out = self.fresh_line();
        self.feed(out, false)
    }

    /// A backward word-kill (Ctrl+W, Alt+Backspace, Ctrl+Backspace).
    ///
    /// Every word-kill there is agrees about ONE case: an open token made only
    /// of letters and digits, standing after whitespace or at the start of the
    /// line, is removed whole and nothing else is. That case clears the token
    /// (`sot`, Ctrl+W, `sit␣` fires). Anywhere else the editors disagree about
    /// how much went (`night-night`, `foo_bar`, `ねこ、おすわり`, an empty token
    /// with the previous word behind it), so the line is poisoned.
    ///
    /// On a provably EMPTY line there is nothing to kill in any editor: a
    /// no-op, exactly like Backspace at zero. Ctrl+W pressed once too often
    /// while clearing a line must not cost the next `sit␣`.
    pub fn note_word_kill(&mut self) -> TrickFeed<'_> {
        if self.line.poisoned || self.line.line_chars == 0 {
            return TrickFeed::NONE;
        }
        let after_space = self.line.left.is_none_or(char::is_whitespace);
        let plain_token = self.line.token_is_live()
            && self.line.token.as_str().chars().all(char::is_alphanumeric);
        let out = if after_space && plain_token {
            self.line.line_chars = self.line.line_chars.saturating_sub(self.line.token_chars);
            self.line.token.clear();
            self.line.token_chars = 0;
            if self.line.line_chars == 0 {
                self.fresh_line()
            } else {
                None
            }
        } else {
            self.poison()
        };
        self.feed(out, false)
    }

    /// Tab, Esc, arrows, Home / End / PgUp / PgDn, any other chord, raw
    /// controller bytes, a PASTE: poisoned until a reset.
    pub fn note_break(&mut self) -> TrickFeed<'_> {
        let out = self.poison();
        self.feed(out, false)
    }

    /// A press at a prompt whose tty swallows input (a password). Poisoned,
    /// for privacy: the sixteen tricks look different, so a passphrase that
    /// starts with `sleep ` would tell a bystander its first word. A host
    /// should answer ENTER at such a prompt with
    /// [`note_line_reset`](Self::note_line_reset) instead: the secret's line
    /// is over, and the prompt that follows starts clean.
    pub fn note_no_echo(&mut self) -> TrickFeed<'_> {
        let out = self.poison();
        self.feed(out, false)
    }

    /// A plain Backspace. See "Typo recovery" in the module docs.
    ///
    /// THE COUNT IS NEVER LOST, even when the content is. Backspacing past
    /// what the listener remembers (a second boundary, a delimiter run longer
    /// than its window) makes the line IMPURE — what stands left of the caret
    /// was never snapshotted, so nothing on it may fire — but each further
    /// press still takes exactly one character off the count, while a line
    /// editor takes AT LEAST one per press. There the count can only err
    /// high, so zero still proves the line empty, and a held Backspace
    /// recovers any line.
    pub fn note_backspace(&mut self) -> TrickFeed<'_> {
        // Poisoned: the count stopped meaning anything. Zero: nothing to take
        // (a held key overshooting an empty line).
        if self.line.poisoned || self.line.line_chars == 0 {
            return TrickFeed::NONE;
        }
        let mut out = None;
        if self.line.token_chars > 0 {
            let popped = self.pop_cluster();
            self.line.line_chars = self.line.line_chars.saturating_sub(popped);
        } else if self.line.gap_chars == 1 && self.snapshot.is_some() {
            // The run's FIRST character goes: the caret is back at the end of
            // the token that run closed, and the line is exactly the snapshot.
            if let Some(before) = self.snapshot.take() {
                // A tentative fire delivered at this boundary is being
                // un-typed (`sit␣⌫` is on its way to `site`).
                let fired_here = self.line.last_fire != before.last_fire;
                if fired_here && self.line.tentative.is_some() {
                    out = Some(Outcome::Revoke);
                }
                self.line = before;
            }
        } else {
            let run = self.line.gap_chars as usize;
            if (2..=GAP_WINDOW).contains(&run) {
                // Inside a remembered delimiter run (`sit!!⌫`, a doubled
                // space): the run now ends with the character before.
                self.line.left = self.line.gap.get(run.saturating_sub(2)).copied();
            } else {
                // A SECOND boundary crossed backwards, or a run longer than
                // the window: the content left of the caret is unknown.
                out = self.go_impure();
            }
            self.line.gap_chars = self.line.gap_chars.saturating_sub(1);
            self.line.line_chars = self.line.line_chars.saturating_sub(1);
        }
        if self.line.line_chars == 0 {
            out = fold(out, self.fresh_line());
        }
        self.feed(out, false)
    }

    // ---- the machine ----

    /// One character of the line.
    fn step(
        &mut self,
        now: Instant,
        c: char,
        fed: u32,
        lexicon: Option<&TrickLexicon>,
    ) -> Option<Outcome> {
        if self.line.poisoned {
            return None;
        }
        let kind = kind_of(c);
        if kind == Kind::Word
            && self.line.token_chars as usize >= TOKEN_CHARS
            && aterm_grapheme::char_width(c) == 0
        {
            // PAST THE WINDOW A MARK RIDES WITH ITS BASE, UNCOUNTED. It is
            // not stored, so Backspace could never see that it belongs to the
            // character before it; counted, every such cluster would leave
            // the count one too high per mark, and a Thai sentence (no
            // spaces, marks everywhere) held-Backspaced to its start would
            // read as a line that still has text on it. Uncounted, one
            // Backspace is one count again: exact, like the stored part. (A
            // mark riding on the window's LAST character is thereby left out
            // of the lookup too; only a surface exactly as long as the window
            // could tell the difference.)
            return None;
        }
        // Judged BEFORE the step: closing a token empties it.
        let joins = kind == Kind::Joiner && self.line.token_chars > 0;
        let out = match kind {
            Kind::Word => self.push_token_char(c),
            Kind::Joiner if joins => self.push_token_char(c),
            Kind::Space | Kind::WideStop => self.edge(now, Edge::Open, fed, lexicon),
            Kind::Stop => self.edge(now, Edge::Stop, fed, lexicon),
            // A joiner with no token to join is a flag or a quote. The open
            // token is dropped UNJUDGED: `sit:` is a key, not a word.
            Kind::Joiner | Kind::Code => {
                self.end_token();
                self.go_impure()
            }
            Kind::Alien => self.poison(),
        };
        if self.line.poisoned {
            // An alien (or a mark with no base): the count means nothing now.
            return out;
        }
        if kind != Kind::Word && !joins {
            // A delimiter: it extends the run left of the caret.
            if let Some(slot) = self.line.gap.get_mut(self.line.gap_chars as usize) {
                *slot = c;
            }
            self.line.gap_chars = self.line.gap_chars.saturating_add(1);
            self.line.left = Some(c);
        }
        // Counted AFTER the step: a fire measures what was typed before the
        // character that delivered it.
        self.line.line_chars = self.line.line_chars.saturating_add(1);
        out
    }

    /// A word character (or an interior joiner) goes onto the open token.
    fn push_token_char(&mut self, c: char) -> Option<Outcome> {
        let mut out = None;
        if self.line.token_chars == 0 {
            if aterm_grapheme::char_width(c) == 0 {
                // A combining mark with no base IN THIS TOKEN attaches to the
                // delimiter before it, and Backspace would take both: outside
                // the model, like any other alien.
                return self.poison();
            }
            // THE GLUE LAW: a token that starts right after `. ! ? , …` with
            // no whitespace between is `sit.txt`, `v1.2`, `a,b` — code. It
            // also cancels whatever that stop had parked.
            let glued = self.line.tail.is_some()
                || self.line.left.is_some_and(|l| kind_of(l) == Kind::Stop);
            if glued {
                out = self.go_impure();
            }
        }
        if (self.line.token_chars as usize) < TOKEN_CHARS {
            self.line.token.push(c);
        }
        // Past the window the characters are only counted: the token can no
        // longer match anything, and Backspace still has to find its start.
        self.line.token_chars = self.line.token_chars.saturating_add(1);
        out
    }

    /// Whitespace, a stop or a fullwidth stop arrived.
    fn edge(
        &mut self,
        now: Instant,
        edge: Edge,
        fed: u32,
        lexicon: Option<&TrickLexicon>,
    ) -> Option<Outcome> {
        if self.line.token_chars > 0 {
            return self.close_token(now, edge, fed, lexicon);
        }
        match edge {
            Edge::Open => self.deliver_tail(now, fed),
            Edge::Stop => {
                let run_continues = self.line.left.is_some_and(|l| kind_of(l) == Kind::Stop);
                if run_continues || self.line.any_word || !self.line.pure {
                    // `sit!!`, `sit...`: the parked verdict stays parked. After
                    // a word and a space it is French spacing (`assis !`).
                    None
                } else {
                    // Before any word it is code: `!sit`, `.hidden`, `..`.
                    self.go_impure()
                }
            }
        }
    }

    /// Snapshot the line, then take the open token off it. The snapshot is the
    /// ONE level of undo Backspace has (module docs).
    fn end_token(&mut self) -> (Word, u32) {
        let token = (self.line.token, self.line.token_chars);
        if self.line.token_chars > 0 {
            self.snapshot = Some(self.line);
            self.line.token.clear();
            self.line.token_chars = 0;
            // The run that closes this token starts here.
            self.line.gap_chars = 0;
        }
        token
    }

    /// THE BOUNDARY: the only place the vocabulary is consulted.
    fn close_token(
        &mut self,
        now: Instant,
        edge: Edge,
        fed: u32,
        lexicon: Option<&TrickLexicon>,
    ) -> Option<Outcome> {
        let end_at = self.line.line_chars;
        let (word, word_chars) = self.end_token();
        if !self.line.pure {
            // An impure line asks nothing: no fold, no probe.
            return None;
        }
        // An overflowed token is longer than any surface; a missing lexicon
        // (a host that ignored `needs_lexicon`) fails closed the same way.
        let role = if word_chars as usize > TOKEN_CHARS {
            None
        } else {
            lexicon.and_then(|lexicon| lexicon.classify(word.as_str(), &mut self.scratch))
        };
        let verdict = match role {
            Some(TrickRole::Trick {
                trick,
                needs_vocative,
            }) => {
                self.line.any_word = true;
                let in_phrase = self
                    .line
                    .last_fire
                    .is_some_and(|at| now.saturating_duration_since(at) <= PHRASE_GAP);
                // LOOK NEVER WINS A PHRASE. `hey kitty sit`, `pspsps kitty
                // jump`, `come here kitty roll`: the greeting or the call is
                // the attention-getter and the verb after it is the request,
                // so a Look fire yields to any other trick word inside its own
                // phrase instead of absorbing it. The other way round still
                // absorbs (`sit hey`): the request came first.
                let attention_only =
                    self.line.last_trick == Some(Trick::Look) && trick != Trick::Look;
                if in_phrase && !attention_only {
                    // ONE PHRASE, ONE TRICK: `down` of `sit down`. Absorbed
                    // like a filler — not even held.
                    None
                } else if !needs_vocative || self.line.vocative_seen {
                    Some(Verdict::Fire {
                        trick,
                        word,
                        word_chars,
                        end_at,
                        addressed: self.line.vocative_seen,
                    })
                } else {
                    // The newest address word is the one the name will fire.
                    self.line.held = Some(Held {
                        trick,
                        word,
                        word_chars,
                        end_at,
                    });
                    None
                }
            }
            Some(TrickRole::Vocative) => {
                self.line.any_word = true;
                self.line.vocative_seen = true;
                // The name answers ONE thing. A live tentative fire comes
                // first and the held word is dropped, never also fired:
                // `good night kitty` is one sleep.
                let held = self.line.held.take();
                match (self.line.tentative, held) {
                    (Some(trick), _) => Some(Verdict::Confirm { trick }),
                    (None, Some(held)) => Some(Verdict::Fire {
                        trick: held.trick,
                        word: held.word,
                        word_chars: held.word_chars,
                        end_at: held.end_at,
                        addressed: true,
                    }),
                    (None, None) => None,
                }
            }
            Some(TrickRole::Filler) => {
                self.line.any_word = true;
                None
            }
            None => return self.go_impure(),
        };
        match (verdict, edge) {
            (None, _) => None,
            (Some(verdict), Edge::Open) => self.deliver(now, verdict, fed),
            (Some(verdict), Edge::Stop) => {
                // `sit.` may still turn out to be `sit.txt`, and
                // `play cat.mp3` must not confirm at `cat.`.
                self.line.tail = Some(verdict);
                None
            }
        }
    }

    /// An open edge with no open token: whatever a stop parked is due.
    fn deliver_tail(&mut self, now: Instant, fed: u32) -> Option<Outcome> {
        let verdict = self.line.tail.take()?;
        self.deliver(now, verdict, fed)
    }

    fn deliver(&mut self, now: Instant, verdict: Verdict, fed: u32) -> Option<Outcome> {
        match verdict {
            Verdict::Fire {
                trick,
                word,
                word_chars,
                end_at,
                addressed,
            } => {
                let since_end = self.line.line_chars.saturating_sub(end_at);
                self.word = word;
                // A plain fire clears a held address word (`hey sit`); a held
                // fire has already taken it.
                self.line.held = None;
                self.line.last_fire = Some(now);
                self.line.last_trick = Some(trick);
                if !addressed {
                    self.line.tentative = Some(trick);
                }
                Some(Outcome::Fire {
                    trick,
                    back_chars: back_chars(since_end, fed, word_chars),
                    addressed,
                })
            }
            Verdict::Confirm { trick } => {
                self.line.tentative = None;
                Some(Outcome::Confirm { trick })
            }
        }
    }

    /// The line is not pet talk. Nothing on it fires from here on, and a live
    /// tentative fire is withdrawn.
    fn go_impure(&mut self) -> Option<Outcome> {
        self.line.pure = false;
        self.line.held = None;
        self.line.tail = None;
        self.line.tentative.take().map(|_| Outcome::Revoke)
    }

    fn poison(&mut self) -> Option<Outcome> {
        self.line.poisoned = true;
        self.line.held = None;
        self.line.tail = None;
        self.snapshot = None;
        self.line.tentative.take().map(|_| Outcome::Revoke)
    }

    fn fresh_line(&mut self) -> Option<Outcome> {
        let revoked = self.line.tentative.is_some();
        self.line = Line::FRESH;
        self.snapshot = None;
        revoked.then_some(Outcome::Revoke)
    }

    /// Take one CLUSTER off the open token, as a line editor's Backspace does:
    /// trailing zero-width scalars (combining marks, conjoining jamo) go with
    /// their base. Returns how many characters went. Past the token window
    /// the characters were never stored, so they go one at a time — which is
    /// still one CLUSTER at a time, because a mark past the window was never
    /// counted (see `step`).
    ///
    /// Exact for readline, zsh and vim, which delete a base with its marks.
    /// An editor that deletes one scalar per press leaves marks behind that
    /// the listener believes gone; the worst that follows is a pet word heard
    /// on a line that still shows a stray mark.
    fn pop_cluster(&mut self) -> u32 {
        if self.line.token_chars as usize > TOKEN_CHARS {
            self.line.token_chars = self.line.token_chars.saturating_sub(1);
            return 1;
        }
        let mut popped = 0u32;
        while let Some(c) = self.line.token.pop() {
            popped = popped.saturating_add(1);
            self.line.token_chars = self.line.token_chars.saturating_sub(1);
            if aterm_grapheme::char_width(c) != 0 {
                break;
            }
        }
        popped
    }

    fn feed(&self, out: Option<Outcome>, submit_pet_only: bool) -> TrickFeed<'_> {
        let event = out.map(|out| match out {
            Outcome::Fire {
                trick,
                back_chars,
                addressed,
            } => TrickEvent::Fire {
                trick,
                word: self.word.as_str(),
                back_chars,
                addressed,
            },
            Outcome::Confirm { trick } => TrickEvent::Confirm { trick },
            Outcome::Revoke => TrickEvent::Revoke,
        });
        TrickFeed {
            event,
            submit_pet_only,
        }
    }

    /// Bytes of heap the listener holds (the fold scratch — everything else is
    /// inline). Steady-state typing must not move it.
    #[cfg(test)]
    fn heap_capacity(&self) -> usize {
        self.scratch.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small hermetic vocabulary, so these tests pin the LISTENER and not
    /// the shipped word lists (which a data pass may re-sort between `words`
    /// and `address` at any time; the tests that must hold against the
    /// shipped English say so in their names). Roles follow the design's data
    /// rulings: iconic verbs are plain, praise / scolding / everyday openers
    /// are address words, fillers are interjections and glue only — `the`,
    /// `it`, `to`, `for`, `on` are NOT fillers. `night` is plain here because
    /// the phrase law's own example (`good night kitty`) is written that way.
    /// The last `look` word is exactly `TOKEN_CHARS` characters long.
    const VOCAB: &str = r#"
[[trick]]
id      = "sit"
lang    = "en"
words   = ["sit", "stay"]
address = ["wait"]

[[trick]]
id      = "down"
lang    = "en"
words   = ["down", "loaf"]
address = ["rest"]

[[trick]]
id      = "sleep"
lang    = "en"
words   = ["sleep", "nap", "night"]
address = ["bed"]

[[trick]]
id      = "stretch"
lang    = "en"
words   = ["stretch", "wake"]
address = ["rise"]

[[trick]]
id      = "jump"
lang    = "en"
words   = ["jump", "hop"]
address = ["up"]

[[trick]]
id      = "play"
lang    = "en"
words   = ["play", "zoomies"]
address = ["run", "fetch"]

[[trick]]
id      = "roll"
lang    = "en"
words   = ["roll"]
address = ["over", "dead"]

[[trick]]
id      = "purr"
lang    = "en"
words   = ["purr"]
address = ["good", "love"]

[[trick]]
id      = "look"
lang    = "en"
words   = ["pspsps", "pspspspspspspspspspspspspspspsps"]
address = ["look", "hey", "come", "here"]

[[trick]]
id      = "hide"
lang    = "en"
words   = ["hide"]
address = ["away"]

[[trick]]
id      = "scold"
lang    = "en"
words   = ["badkitty"]
address = ["no", "bad", "stop"]

[[trick]]
id      = "treat"
lang    = "en"
words   = ["treat", "snack"]
address = ["nice", "yay"]

[[vocative]]
lang  = "en"
words = ["kitty", "cat", "kitten", "dog", "puppy", "boy", "girl", "buddy"]

[[filler]]
lang  = "en"
words = ["please", "pls", "ok", "okay", "now", "oh", "aw", "my", "little", "a", "you", "and", "so", "such", "very", "whos"]

[[trick]]
id    = "sit"
lang  = "ko"
words = ["앉아"]
cjk   = true

[[vocative]]
lang  = "ko"
words = ["고양이"]
cjk   = true

[[trick]]
id    = "sit"
lang  = "th"
words = ["นั่ง"]
cjk   = true

[[trick]]
id    = "sit"
lang  = "ja"
words = ["おすわり"]
cjk   = true

[[vocative]]
lang  = "ja"
words = ["ねこ"]
cjk   = true

[[trick]]
id    = "sit"
lang  = "zh"
words = ["坐", "坐下"]
cjk   = true

[[trick]]
id    = "jump"
lang  = "zh"
words = ["跳"]
cjk   = true

[[vocative]]
lang  = "zh"
words = ["猫咪"]
cjk   = true
"#;

    /// One keystroke of the rig's clock: a brisk typist. Five of them (a
    /// four-letter word and its space) stay well inside [`PHRASE_GAP`].
    const KEY_GAP: Duration = Duration::from_millis(80);

    /// What the rig saw, owned (an event borrows the listener).
    #[derive(Clone, PartialEq, Eq, Debug)]
    enum Seen {
        Fire {
            trick: Trick,
            word: String,
            back_chars: u16,
            addressed: bool,
        },
        Confirm(Trick),
        Revoke,
        /// `submit_pet_only` — recorded AFTER the same press's event.
        PetOnly,
    }

    fn fire(trick: Trick, word: &str, back_chars: u16, addressed: bool) -> Seen {
        Seen::Fire {
            trick,
            word: word.to_string(),
            back_chars,
            addressed,
        }
    }

    fn record(seen: &mut Vec<Seen>, feed: TrickFeed<'_>) {
        match feed.event {
            Some(TrickEvent::Fire {
                trick,
                word,
                back_chars,
                addressed,
            }) => seen.push(fire(trick, word, back_chars, addressed)),
            Some(TrickEvent::Confirm { trick }) => seen.push(Seen::Confirm(trick)),
            Some(TrickEvent::Revoke) => seen.push(Seen::Revoke),
            None => {}
        }
        if feed.submit_pet_only {
            seen.push(Seen::PetOnly);
        }
    }

    /// A listener, a vocabulary, an injected clock and a log. It feeds the
    /// way a host must: the vocabulary is handed over ONLY when the matching
    /// `needs_lexicon` predicate asks — so every test here also proves the
    /// predicates never under-ask (an under-ask fails closed and the fire the
    /// test expects goes missing).
    struct Rig {
        listener: TrickListener,
        lexicon: TrickLexicon,
        now: Instant,
        seen: Vec<Seen>,
    }

    impl Rig {
        fn with(lexicon: TrickLexicon) -> Rig {
            assert!(lexicon.conflicts().is_empty(), "{:?}", lexicon.conflicts());
            Rig {
                listener: TrickListener::new(),
                lexicon,
                now: Instant::now(),
                seen: Vec::new(),
            }
        }

        fn new() -> Rig {
            Rig::with(TrickLexicon::from_source(VOCAB, &["en"]).expect("the fixture parses"))
        }

        fn shipped_english() -> Rig {
            Rig::with(TrickLexicon::shared_en().clone())
        }

        fn key(&mut self, c: char) {
            self.now += KEY_GAP;
            let lexicon = self.listener.needs_lexicon(c).then_some(&self.lexicon);
            let feed = self.listener.note_char(self.now, c, lexicon);
            record(&mut self.seen, feed);
        }

        fn ime(&mut self, text: &str) {
            self.now += KEY_GAP;
            let lexicon = self
                .listener
                .ime_needs_lexicon(text)
                .then_some(&self.lexicon);
            let feed = self.listener.note_ime(self.now, text, lexicon);
            record(&mut self.seen, feed);
        }

        fn enter(&mut self) {
            self.now += KEY_GAP;
            let lexicon = self
                .listener
                .submit_needs_lexicon()
                .then_some(&self.lexicon);
            let feed = self.listener.note_submit(self.now, lexicon);
            record(&mut self.seen, feed);
        }

        fn backspace(&mut self) {
            self.now += KEY_GAP;
            let feed = self.listener.note_backspace();
            record(&mut self.seen, feed);
        }

        fn word_kill(&mut self) {
            let feed = self.listener.note_word_kill();
            record(&mut self.seen, feed);
        }

        fn line_reset(&mut self) {
            let feed = self.listener.note_line_reset();
            record(&mut self.seen, feed);
        }

        fn brk(&mut self) {
            let feed = self.listener.note_break();
            record(&mut self.seen, feed);
        }

        fn no_echo(&mut self) {
            let feed = self.listener.note_no_echo();
            record(&mut self.seen, feed);
        }

        fn wait(&mut self, pause: Duration) {
            self.now += pause;
        }

        /// A typed script: `␣` is the Space key, `⏎` Enter, `⌫` Backspace,
        /// anything else a typed character.
        fn run(&mut self, script: &str) -> &mut Rig {
            for c in script.chars() {
                match c {
                    '␣' => self.key(' '),
                    '⏎' => self.enter(),
                    '⌫' => self.backspace(),
                    c => self.key(c),
                }
            }
            self
        }

        fn fires(&self) -> Vec<Trick> {
            self.seen
                .iter()
                .filter_map(|seen| match seen {
                    Seen::Fire { trick, .. } => Some(*trick),
                    Seen::Confirm(_) | Seen::Revoke | Seen::PetOnly => None,
                })
                .collect()
        }

        /// The line's verdict as the PET would live it: some fire ended
        /// final (addressed, confirmed, or tentative and never revoked).
        fn something_stuck(&self) -> bool {
            let mut stuck = false;
            let mut tentative = false;
            for seen in &self.seen {
                match seen {
                    Seen::Fire { addressed, .. } => {
                        stuck |= *addressed;
                        tentative = !*addressed;
                    }
                    Seen::Confirm(_) => {
                        stuck = true;
                        tentative = false;
                    }
                    Seen::Revoke => tentative = false,
                    Seen::PetOnly => {}
                }
            }
            stuck || tentative
        }
    }

    // ---- the firing law ----

    #[test]
    fn a_plain_word_fires_tentatively_at_the_space_and_not_a_letter_sooner() {
        let mut rig = Rig::new();
        rig.run("sit");
        assert!(rig.seen.is_empty(), "a completing LETTER fires nothing");
        rig.run("␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        assert_eq!(rig.listener.tentative(), Some(Trick::Sit));
    }

    #[test]
    fn prose_after_a_tentative_fire_revokes_it() {
        let mut rig = Rig::new();
        rig.run("sit␣tight");
        assert_eq!(rig.seen.len(), 1, "still tentative while `tight` is open");
        rig.run("␣");
        assert_eq!(
            rig.seen,
            [fire(Trick::Sit, "sit", 0, false), Seen::Revoke],
            "`sit tight while I check` must not seat the cat"
        );
        assert!(!rig.listener.is_pure());
        rig.run("while␣I␣check⏎");
        assert_eq!(rig.seen.len(), 2, "an impure line says nothing more");
    }

    #[test]
    fn a_named_pet_makes_the_fire_final() {
        let mut rig = Rig::new();
        rig.run("kitty␣");
        assert!(rig.seen.is_empty(), "a name fires nothing by itself");
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, true)]);
        assert_eq!(rig.listener.tentative(), None);
    }

    #[test]
    fn an_address_word_is_held_until_the_pet_is_named_and_fires_as_itself() {
        let mut rig = Rig::new();
        rig.run("good␣");
        assert!(rig.seen.is_empty(), "`good catch` must not move the cat");
        rig.run("kitty␣");
        // The word is the TRICK token, never the name; it sits ` kitty` — six
        // characters — left of the caret the closing space arrived at.
        assert_eq!(rig.seen, [fire(Trick::Purr, "good", 6, true)]);
    }

    #[test]
    fn an_address_word_that_is_never_named_never_fires() {
        for line in ["good⏎", "no⏎", "stop␣⏎", "look␣here⏎", "good␣catch⏎"] {
            let mut rig = Rig::new();
            rig.run(line);
            assert!(rig.fires().is_empty(), "{line:?}: {:?}", rig.seen);
        }
    }

    #[test]
    fn an_address_word_after_the_name_fires_at_once() {
        let mut rig = Rig::new();
        rig.run("kitty␣come␣");
        assert_eq!(rig.seen, [fire(Trick::Look, "come", 0, true)]);
    }

    #[test]
    fn fillers_keep_the_line_pure_and_fire_nothing() {
        let mut rig = Rig::new();
        rig.run("please␣sit␣now␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        assert!(rig.listener.is_pure());
        let mut rig = Rig::new();
        rig.run("whos␣a␣good␣boy␣");
        assert_eq!(rig.seen, [fire(Trick::Purr, "good", 4, true)]);
    }

    #[test]
    fn a_name_confirms_a_tentative_fire() {
        let mut rig = Rig::new();
        rig.run("sit␣kitty␣");
        assert_eq!(
            rig.seen,
            [fire(Trick::Sit, "sit", 0, false), Seen::Confirm(Trick::Sit)]
        );
        assert_eq!(rig.listener.tentative(), None);
        // Confirmed is final: prose afterwards revokes nothing.
        rig.run("you␣rascal␣");
        assert_eq!(rig.seen.len(), 2);
    }

    #[test]
    fn enter_confirms_a_tentative_fire_on_a_still_pure_line() {
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.wait(Duration::from_secs(5));
        rig.run("⏎");
        assert_eq!(
            rig.seen,
            [
                fire(Trick::Sit, "sit", 0, false),
                Seen::Confirm(Trick::Sit),
                Seen::PetOnly
            ]
        );
    }

    #[test]
    fn enter_on_the_word_itself_is_one_final_fire() {
        let mut rig = Rig::new();
        rig.run("sit⏎");
        assert_eq!(
            rig.seen,
            [fire(Trick::Sit, "sit", 0, true), Seen::PetOnly],
            "the submit IS the confirmation: one event, not a fire the host never hears of"
        );
        assert_eq!(rig.listener.line_chars(), 0, "Enter starts a fresh line");
    }

    #[test]
    fn capitals_fold_and_the_word_is_reported_as_typed() {
        for word in ["SIT", "Sit", "sIt"] {
            let mut rig = Rig::new();
            rig.run(word).run("␣");
            assert_eq!(rig.seen, [fire(Trick::Sit, word, 0, false)]);
        }
    }

    #[test]
    fn a_drawn_out_word_fires_and_flashes_as_it_was_typed() {
        let mut rig = Rig::new();
        rig.run("goooood␣kitty␣");
        assert_eq!(rig.seen, [fire(Trick::Purr, "goooood", 6, true)]);
    }

    #[test]
    fn whole_words_only() {
        for line in ["site␣", "sitting␣", "sits␣", "sleepy␣", "runtime␣", "asit␣"] {
            let mut rig = Rig::new();
            rig.run(line);
            assert!(rig.seen.is_empty(), "{line:?}: {:?}", rig.seen);
            assert!(!rig.listener.is_pure(), "{line:?} is not pet talk");
        }
    }

    #[test]
    fn leading_and_doubled_spaces_cost_nothing() {
        let mut rig = Rig::new();
        rig.run("␣sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        let mut rig = Rig::new();
        rig.run("kitty␣␣␣sit␣␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, true)]);
    }

    // ---- code never fires ----

    #[test]
    fn shell_and_code_lines_never_fire() {
        for line in [
            "npm run build",
            "git fetch",
            "./sit",
            "--sit",
            "-sit",
            "sit.txt",
            "sit: 3",
            "sit:3",
            "sit=3",
            "sit;",
            "play.py",
            "sleep.sh",
            "$sit",
            "~/sit",
            "sit/",
            "sit()",
            "'sit'",
            "\"sit\"",
            "`sit`",
            "#sit",
            "@sit",
            "!sit",
            ".sit",
            "cat sleep.txt",
            "kitty play.py",
            "FOO=1 sleep 5",
            "time sleep 1",
            "echo sit",
            "x = sit",
            "sit_down",
            "sit2",
            "sit-",
            "sit'",
        ] {
            let mut rig = Rig::new();
            rig.run(line).run("␣").run("⏎");
            assert!(rig.seen.is_empty(), "{line:?}: {:?}", rig.seen);
        }
    }

    #[test]
    fn code_after_a_plain_word_revokes_it() {
        // At `sit␣` these lines ARE `sit␣`: the tentative tier is what makes
        // that affordable.
        for line in [
            "sit = 3",
            "play = 3",
            "sleep 5",
            "play song.wav",
            "hide = False",
        ] {
            let mut rig = Rig::new();
            rig.run(line).run("⏎");
            assert_eq!(rig.seen.len(), 2, "{line:?}: {:?}", rig.seen);
            assert_eq!(rig.seen.last(), Some(&Seen::Revoke), "{line:?}");
        }
    }

    #[test]
    fn every_code_punctuation_mark_makes_the_line_impure() {
        for mark in ":;/\\=@#+~$%|<>*&^_()[]{}\"`".chars() {
            // After a word and a space: revokes.
            let mut rig = Rig::new();
            rig.run("sit␣");
            rig.key(mark);
            if mark == '_' {
                // `_` is a token character: it opens a token that is not a
                // word, which is judged at ITS boundary.
                rig.run("␣");
            }
            assert_eq!(rig.seen.last(), Some(&Seen::Revoke), "{mark:?}");
            // Glued to the word: the word is dropped unjudged.
            let mut rig = Rig::new();
            rig.run("sit");
            rig.key(mark);
            rig.run("␣⏎");
            assert!(rig.seen.is_empty(), "{mark:?}: {:?}", rig.seen);
        }
    }

    #[test]
    fn the_lexicons_code_context_set_is_code_here_too() {
        // The screen scanner suppresses a word next to these (`cat=1`,
        // `/api/cat`, `$cat`); the listener must draw the code / prose line in
        // the same place. Its own rule is wider — everything that is not a
        // word, a joiner, whitespace or a sentence stop — and this pins that
        // it stays a SUPERSET of the lexicon's set, which is the oracle.
        let mut checked = 0;
        for c in (0..=0x2FFFu32).filter_map(char::from_u32) {
            if aterm_lexicon::is_code_adjacent_punct(c) {
                assert_eq!(kind_of(c), Kind::Code, "{c:?}");
                checked += 1;
            }
        }
        assert!(checked >= 16, "the oracle went vacuous: {checked}");
        // The scanner's other two context rules are structural here: a
        // leading `-` has no token to join, and `.` + a letter is glue.
        assert_eq!(kind_of('-'), Kind::Joiner);
        assert_eq!(kind_of('.'), Kind::Stop);
        // `:` and `;` are code, not sentence punctuation: `play: 3` is YAML.
        assert_eq!(kind_of(':'), Kind::Code);
        assert_eq!(kind_of(';'), Kind::Code);
    }

    // ---- sentence punctuation ----

    #[test]
    fn a_full_stop_parks_the_fire_until_the_next_space() {
        let mut rig = Rig::new();
        rig.run("sit.");
        assert!(rig.seen.is_empty(), "`sit.` may still become `sit.txt`");
        rig.run("␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 1, false)]);
    }

    #[test]
    fn repeated_sentence_punctuation_keeps_the_parked_fire() {
        let mut rig = Rig::new();
        rig.run("sit!!␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 2, false)]);
        let mut rig = Rig::new();
        rig.run("sit...␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 3, false)]);
        let mut rig = Rig::new();
        rig.run("good␣kitty!!␣");
        assert_eq!(rig.seen, [fire(Trick::Purr, "good", 8, true)]);
    }

    #[test]
    fn a_token_character_glued_to_the_stop_cancels_the_parked_fire() {
        let mut rig = Rig::new();
        rig.run("sit.t");
        assert!(rig.seen.is_empty());
        assert!(
            !rig.listener.is_pure(),
            "`sit.t` is on its way to `sit.txt`"
        );
        rig.run("xt␣⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
    }

    #[test]
    fn the_glue_law_holds_with_nothing_parked() {
        // `cat.` parks nothing (a name fires nothing), and `cat.sit` is still
        // a path, not an address.
        let mut rig = Rig::new();
        rig.run("cat.sit␣⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
        let mut rig = Rig::new();
        rig.run("kitty,sit␣⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
        // With the space it is a sentence.
        let mut rig = Rig::new();
        rig.run("kitty,␣sit!⏎");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 1, true), Seen::PetOnly]);
    }

    #[test]
    fn enter_delivers_a_parked_fire_as_final() {
        let mut rig = Rig::new();
        rig.run("sit.⏎");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 1, true), Seen::PetOnly]);
    }

    #[test]
    fn a_name_before_a_stop_parks_its_confirmation_too() {
        // `play cat.mp3`: the confirmation at `cat.` would be FINAL, and the
        // line is a sox command.
        let mut rig = Rig::new();
        rig.run("play␣cat.");
        assert_eq!(rig.seen, [fire(Trick::Play, "play", 0, false)]);
        rig.run("mp3⏎");
        assert_eq!(
            rig.seen,
            [fire(Trick::Play, "play", 0, false), Seen::Revoke],
            "never confirmed"
        );
        // …and the sentence still confirms.
        let mut rig = Rig::new();
        rig.run("play␣kitty.␣");
        assert_eq!(
            rig.seen,
            [
                fire(Trick::Play, "play", 0, false),
                Seen::Confirm(Trick::Play)
            ]
        );
    }

    #[test]
    fn sentence_punctuation_before_any_word_is_code() {
        for line in ["!sit␣", ".␣sit␣", "?sit␣", "␣␣!␣sit␣", "...␣sit␣"] {
            let mut rig = Rig::new();
            rig.run(line).run("⏎");
            assert!(rig.seen.is_empty(), "{line:?}: {:?}", rig.seen);
        }
    }

    #[test]
    fn a_spaced_stop_after_a_word_is_french_spacing_not_code() {
        let mut rig = Rig::new();
        rig.run("sit␣!␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        assert!(rig.listener.is_pure());
        // …but a word glued to THAT stop is still glue.
        rig.run("!stay␣");
        assert_eq!(rig.seen.last(), Some(&Seen::Revoke));
    }

    // ---- one phrase, one trick ----

    #[test]
    fn sit_down_fires_exactly_one_sit() {
        let mut rig = Rig::new();
        rig.run("sit␣down␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        assert!(rig.listener.is_pure(), "absorbed like a filler");
        rig.run("⏎");
        assert_eq!(
            rig.seen,
            [
                fire(Trick::Sit, "sit", 0, false),
                Seen::Confirm(Trick::Sit),
                Seen::PetOnly
            ]
        );
    }

    #[test]
    fn two_word_phrases_fire_once_each() {
        for (line, trick, word) in [
            ("play␣dead␣", Trick::Play, "play"),
            ("roll␣over␣", Trick::Roll, "roll"),
            ("jump␣up␣", Trick::Jump, "jump"),
            ("wake␣up␣", Trick::Stretch, "wake"),
            ("sit␣stay␣", Trick::Sit, "sit"),
        ] {
            let mut rig = Rig::new();
            rig.run(line);
            assert_eq!(rig.seen, [fire(trick, word, 0, false)], "{line:?}");
            // The absorbed word is not HELD either: a name now confirms the
            // first fire and fires nothing else.
            rig.run("kitty␣");
            assert_eq!(rig.seen.last(), Some(&Seen::Confirm(trick)), "{line:?}");
            assert_eq!(rig.fires(), [trick], "{line:?}");
        }
    }

    #[test]
    fn good_night_kitty_is_one_sleep_and_never_a_purr() {
        let mut rig = Rig::new();
        rig.run("good␣night␣kitty␣");
        assert_eq!(
            rig.seen,
            [
                // A plain fire CLEARS the held `good`…
                fire(Trick::Sleep, "night", 0, false),
                // …so the name only confirms; it never also fires a purr.
                Seen::Confirm(Trick::Sleep),
            ]
        );
    }

    #[test]
    fn a_greeting_or_a_call_never_wins_the_phrase() {
        // `hey kitty sit`: the name fires the held greeting (Look, addressed),
        // and the verb that follows inside the phrase gap is the request —
        // it fires instead of being absorbed.
        let mut rig = Rig::new();
        rig.run("hey␣kitty␣sit␣");
        assert_eq!(
            rig.seen,
            [
                fire(Trick::Look, "hey", 6, true),
                fire(Trick::Sit, "sit", 0, true)
            ]
        );
        // `pspsps kitty jump`: a plain call fires tentatively, the name
        // confirms it, and the verb after it still fires, addressed.
        let mut rig = Rig::new();
        rig.run("pspsps␣kitty␣jump␣");
        assert_eq!(
            rig.seen,
            [
                fire(Trick::Look, "pspsps", 0, false),
                Seen::Confirm(Trick::Look),
                fire(Trick::Jump, "jump", 0, true)
            ]
        );
        // A second Look inside the phrase IS absorbed (`hey kitty look`), and
        // the verb-first order still absorbs the greeting (`sit hey`, pinned
        // beside this test): only a LOOK fire yields.
        let mut rig = Rig::new();
        rig.run("hey␣kitty␣look␣");
        assert_eq!(rig.fires(), [Trick::Look]);
        let mut rig = Rig::new();
        rig.run("sit␣down␣");
        assert_eq!(
            rig.fires(),
            [Trick::Sit],
            "a non-Look fire still absorbs its phrase"
        );
    }

    #[test]
    fn a_plain_fire_clears_the_held_address_word() {
        let mut rig = Rig::new();
        rig.run("hey␣sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        rig.run("kitty␣");
        assert_eq!(rig.fires(), [Trick::Sit], "`hey` was dropped, not fired");
    }

    #[test]
    fn a_name_confirms_the_tentative_fire_and_drops_a_later_held_word() {
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.wait(Duration::from_secs(2));
        // Past the phrase gap `good` is a new request — and is held.
        rig.run("good␣kitty␣");
        assert_eq!(
            rig.seen,
            [fire(Trick::Sit, "sit", 0, false), Seen::Confirm(Trick::Sit)],
            "one name answers one thing"
        );
    }

    #[test]
    fn a_trick_word_after_the_phrase_gap_is_a_new_request() {
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.wait(Duration::from_secs(2));
        rig.run("jump␣");
        assert_eq!(
            rig.seen,
            [
                fire(Trick::Sit, "sit", 0, false),
                fire(Trick::Jump, "jump", 0, false)
            ]
        );
        assert_eq!(rig.listener.tentative(), Some(Trick::Jump));
    }

    #[test]
    fn the_phrase_gap_is_measured_boundary_to_boundary_and_is_inclusive() {
        let lexicon = TrickLexicon::from_source(VOCAB, &["en"]).expect("the fixture parses");
        for (gap, fires) in [(PHRASE_GAP, 1), (PHRASE_GAP + Duration::from_millis(1), 2)] {
            let mut listener = TrickListener::new();
            let t0 = Instant::now();
            let mut fired = 0;
            for c in "sit".chars() {
                let _ = listener.note_char(t0, c, None);
            }
            fired += usize::from(listener.note_char(t0, ' ', Some(&lexicon)).event.is_some());
            for c in "jump".chars() {
                let _ = listener.note_char(t0, c, None);
            }
            fired += usize::from(
                listener
                    .note_char(t0 + gap, ' ', Some(&lexicon))
                    .event
                    .is_some(),
            );
            assert_eq!(fired, fires, "{gap:?}");
        }
    }

    #[test]
    fn a_new_line_is_a_new_phrase() {
        let mut rig = Rig::new();
        rig.run("sit⏎jump⏎");
        assert_eq!(rig.fires(), [Trick::Sit, Trick::Jump]);
    }

    // ---- typo recovery ----

    #[test]
    fn backspacing_a_typo_to_the_start_of_the_line_recovers() {
        let mut rig = Rig::new();
        rig.run("sot⌫⌫⌫sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

    #[test]
    fn backspacing_over_one_boundary_restores_the_token_behind_it() {
        let mut rig = Rig::new();
        rig.run("siy␣");
        assert!(!rig.listener.is_pure());
        rig.run("⌫⌫t␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        // Mid-line too, with the flags of the moment restored.
        let mut rig = Rig::new();
        rig.run("kitty␣siy␣⌫⌫t␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, true)]);
    }

    #[test]
    fn a_held_backspace_that_overshoots_the_line_costs_nothing() {
        let mut rig = Rig::new();
        rig.run("ls␣-la␣sot");
        for _ in 0..40 {
            rig.backspace();
        }
        assert_eq!(rig.listener.line_chars(), 0);
        assert!(rig.listener.is_pure() && !rig.listener.is_poisoned());
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

    #[test]
    fn a_word_kill_on_an_open_token_recovers() {
        let mut rig = Rig::new();
        rig.run("sot");
        rig.word_kill();
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        let mut rig = Rig::new();
        rig.run("kitty␣sot");
        rig.word_kill();
        assert_eq!(rig.listener.line_chars(), 6);
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, true)]);
    }

    #[test]
    fn a_word_kill_the_editors_disagree_about_poisons() {
        // No open token: the previous word went, and how much of the run with
        // it depends on the editor.
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.word_kill();
        assert!(rig.listener.is_poisoned());
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false), Seen::Revoke]);
        // A joiner inside: Ctrl+W takes the token, Alt+Backspace half of it.
        let mut rig = Rig::new();
        rig.run("night-night");
        rig.word_kill();
        assert!(rig.listener.is_poisoned());
        // A token that does not follow whitespace: Ctrl+W takes `ねこ、` too.
        let mut rig = Rig::new();
        rig.ime("ねこ、おすわり");
        rig.word_kill();
        assert!(rig.listener.is_poisoned());
    }

    #[test]
    fn backspacing_into_a_tentatively_fired_word_revokes_it() {
        let mut rig = Rig::new();
        rig.run("sit␣⌫");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false), Seen::Revoke]);
        assert!(rig.listener.is_pure());
        // `site`: the prefix must not have moved the cat.
        rig.run("e␣");
        assert_eq!(rig.seen.len(), 2);
        assert!(!rig.listener.is_pure());
        // Retyping the space instead fires again: the request stands.
        let mut rig = Rig::new();
        rig.run("sit␣⌫␣");
        assert_eq!(
            rig.seen,
            [
                fire(Trick::Sit, "sit", 0, false),
                Seen::Revoke,
                fire(Trick::Sit, "sit", 0, false)
            ]
        );
    }

    #[test]
    fn backspace_inside_a_delimiter_run_knows_what_the_run_now_ends_with() {
        // `sit!!⌫` is `sit!`: the fire is still parked, and a letter is glue.
        let mut rig = Rig::new();
        rig.run("sit!!⌫␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 1, false)]);
        let mut rig = Rig::new();
        rig.run("sit.␣⌫t");
        assert_eq!(
            rig.seen,
            [fire(Trick::Sit, "sit", 1, false), Seen::Revoke],
            "the line reads `sit.t`"
        );
        // A doubled space taken back is still a space.
        let mut rig = Rig::new();
        rig.run("kitty␣␣⌫sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, true)]);
    }

    #[test]
    fn a_second_boundary_crossed_backwards_leaves_the_line_impure_but_counted() {
        let mut rig = Rig::new();
        rig.run("kitty␣sit␣⌫⌫⌫⌫");
        assert!(rig.listener.is_pure(), "one boundary, then the token");
        rig.run("⌫");
        assert!(!rig.listener.is_pure(), "`kitty` was never snapshotted");
        assert!(!rig.listener.is_poisoned());
        assert_eq!(rig.listener.line_chars(), 5);
        rig.run("␣sit␣");
        assert_eq!(rig.fires(), [Trick::Sit], "nothing new fires on it");
        // …but the count was kept, so backspacing to the start proves the
        // line empty.
        rig.run("⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫");
        assert_eq!(rig.listener.line_chars(), 0);
        rig.run("sit␣");
        assert_eq!(rig.fires(), [Trick::Sit, Trick::Sit]);
    }

    #[test]
    fn a_delimiter_run_longer_than_the_window_still_gives_the_token_back() {
        let mut rig = Rig::new();
        let run = GAP_WINDOW + 4;
        rig.run("siy");
        for _ in 0..run {
            rig.run("␣");
        }
        for _ in 0..run {
            rig.run("⌫");
        }
        assert!(
            rig.listener.is_pure(),
            "the line is exactly the snapshot again"
        );
        rig.run("⌫t␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

    #[test]
    fn backspace_takes_a_zero_width_mark_with_its_base() {
        let mut rig = Rig::new();
        // น + ั + ่ : one cluster, three scalars.
        rig.run("นั่");
        assert_eq!(rig.listener.line_chars(), 3);
        rig.backspace();
        assert_eq!(
            rig.listener.line_chars(),
            0,
            "as readline and zsh delete it"
        );
        rig.run("นั่ง␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "นั่ง", 0, false)]);
    }

    #[test]
    fn a_long_marked_run_is_backspaced_cluster_for_cluster_past_the_window_too() {
        // Thai has no spaces: a sentence is ONE run, far longer than the
        // window, with marks all the way along. A line editor takes one
        // CLUSTER per press, so as many presses as clusters must bring the
        // count to zero — a count that erred high here would leave the
        // emptied line impure, and the request typed next unanswered.
        let clusters = TOKEN_CHARS;
        let mut rig = Rig::new();
        for _ in 0..clusters {
            rig.run("นั่");
        }
        assert!(rig.listener.line.token_chars as usize > TOKEN_CHARS);
        for _ in 0..clusters {
            assert_ne!(rig.listener.line_chars(), 0);
            rig.backspace();
        }
        assert_eq!(rig.listener.line_chars(), 0, "one press, one cluster");
        rig.run("นั่ง␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "นั่ง", 0, false)]);
    }

    #[test]
    fn a_word_kill_on_an_empty_line_is_a_no_op() {
        let mut rig = Rig::new();
        rig.word_kill();
        assert!(!rig.listener.is_poisoned(), "nothing was there to kill");
        // `sot`, Ctrl+W — and Ctrl+W once more, as people do.
        rig.run("sot");
        rig.word_kill();
        rig.word_kill();
        assert!(!rig.listener.is_poisoned());
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

    // ---- poison and resets ----

    #[test]
    fn escape_tab_and_arrows_poison_the_line_until_a_reset() {
        let mut rig = Rig::new();
        rig.brk();
        rig.run("sit␣");
        assert!(rig.seen.is_empty(), "the caret may be anywhere");
        rig.run("⌫⌫⌫⌫⌫⌫⌫⌫");
        rig.run("sit␣");
        assert!(
            rig.seen.is_empty(),
            "backspacing proves nothing once poisoned"
        );
        rig.run("⏎sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        // `si<Tab>t `
        let mut rig = Rig::new();
        rig.run("si");
        rig.brk();
        rig.run("t␣⏎");
        assert!(rig.seen.is_empty());
    }

    #[test]
    fn a_break_revokes_a_live_tentative_fire() {
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.brk();
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false), Seen::Revoke]);
        rig.brk();
        assert_eq!(rig.seen.len(), 2, "once");
    }

    #[test]
    fn ctrl_u_and_ctrl_c_start_a_fresh_pure_line() {
        let mut rig = Rig::new();
        rig.run("ls␣-la");
        rig.line_reset();
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        // …and abandon a tentative fire.
        rig.line_reset();
        assert_eq!(rig.seen.last(), Some(&Seen::Revoke));
        // …and lift a poison.
        rig.brk();
        rig.line_reset();
        rig.run("kitty␣jump␣");
        assert_eq!(rig.seen.last(), Some(&fire(Trick::Jump, "jump", 0, true)));
    }

    #[test]
    fn a_character_with_no_column_of_its_own_poisons() {
        // ZWJ after a word: a line editor's Backspace would take the `t` with
        // it, and the listener's one-for-one count would restore a `sit` that
        // is no longer on the line.
        for alien in ['\u{200d}', '\u{fe0f}', '\u{0}', '\n', '\u{7f}'] {
            let mut rig = Rig::new();
            rig.run("sit");
            rig.key(alien);
            assert!(rig.listener.is_poisoned(), "{alien:?}");
            rig.run("⌫␣⏎");
            assert!(rig.seen.is_empty(), "{alien:?}: {:?}", rig.seen);
        }
        // A combining mark with no base in the token hangs on the space.
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.key('\u{301}');
        assert!(rig.listener.is_poisoned());
        assert_eq!(rig.seen.last(), Some(&Seen::Revoke));
        // Inside a token it is part of the word, and of its cluster: ONE
        // Backspace takes the `e` and its accent.
        let mut rig = Rig::new();
        rig.run("site\u{301}⌫␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

    #[test]
    fn a_paste_poisons() {
        // The host answers a paste with `note_break`: after pasting `echo `
        // the listener would otherwise still call the line empty and pure.
        let mut rig = Rig::new();
        rig.brk();
        rig.run("sleep␣⏎");
        assert!(rig.seen.is_empty());
    }

    #[test]
    fn a_no_echo_prompt_poisons_and_says_nothing() {
        let mut rig = Rig::new();
        rig.no_echo();
        rig.run("sleep␣tight␣");
        assert!(rig.seen.is_empty(), "a passphrase must not move the cat");
        // Enter there ends the secret's line; the next prompt is clean.
        rig.line_reset();
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        // A tentative fire that was live when the prompt went dark is taken
        // back.
        rig.no_echo();
        assert_eq!(rig.seen.last(), Some(&Seen::Revoke));
    }

    #[test]
    fn raw_bytes_ending_in_a_newline_reset_without_judging() {
        // A person types `sit`; an agent then sends `xyz\n`. The line that
        // ran was `sitxyz`.
        let mut rig = Rig::new();
        rig.run("sit");
        rig.line_reset();
        assert!(rig.seen.is_empty(), "the open token is NOT evaluated");
        assert_eq!(rig.listener.line_chars(), 0);
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

    // ---- no-space scripts and the IME ----

    #[test]
    fn hangul_committed_one_syllable_at_a_time_fires_at_the_boundary() {
        let mut rig = Rig::new();
        rig.ime("앉");
        rig.ime("아");
        assert!(rig.seen.is_empty(), "the end of a commit is not a boundary");
        rig.run("␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "앉아", 0, false)]);
        let mut rig = Rig::new();
        rig.ime("앉");
        rig.ime("아");
        rig.run("⏎");
        assert_eq!(rig.seen, [fire(Trick::Sit, "앉아", 0, true), Seen::PetOnly]);
        // The name, then the word: addressed.
        let mut rig = Rig::new();
        for syllable in ["고", "양", "이"] {
            rig.ime(syllable);
        }
        rig.run("␣");
        rig.ime("앉");
        rig.ime("아");
        rig.run("␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "앉아", 0, true)]);
    }

    #[test]
    fn thai_typed_through_a_layout_fires() {
        // Thai arrives as typed characters, not commits, and none of them is
        // a token character.
        let mut rig = Rig::new();
        rig.run("นั่ง");
        assert!(rig.seen.is_empty());
        rig.run("␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "นั่ง", 0, false)]);
    }

    #[test]
    fn a_lone_committed_ideograph_on_the_way_to_a_longer_word_never_fires() {
        // `跳` committed alone, then `过`: the run is `跳过` ("skip").
        let mut rig = Rig::new();
        rig.ime("跳");
        assert!(rig.seen.is_empty());
        rig.ime("过");
        rig.ime("这个测试");
        rig.run("⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
    }

    #[test]
    fn a_japanese_sentence_committed_whole_fires_addressed_at_the_fullwidth_stop() {
        let mut rig = Rig::new();
        rig.ime("ねこ、おすわり！");
        // Three characters of the commit precede the word, and the caret
        // still stood before the whole commit.
        assert_eq!(rig.seen, [fire(Trick::Sit, "おすわり", 3, true)]);
    }

    #[test]
    fn a_lone_ideograph_needs_the_pet_named() {
        let mut rig = Rig::new();
        rig.ime("坐");
        rig.run("␣");
        assert!(rig.seen.is_empty(), "`坐` is a syllable of someone's prose");
        rig.ime("猫咪");
        rig.run("␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "坐", 3, true)]);
        // The two-character word is an ordinary plain word.
        let mut rig = Rig::new();
        rig.ime("坐下");
        rig.key('\u{3000}');
        assert_eq!(rig.seen, [fire(Trick::Sit, "坐下", 0, false)]);
    }

    #[test]
    fn a_fullwidth_stop_is_a_full_boundary_for_a_spaced_token_too() {
        let mut rig = Rig::new();
        rig.run("sit！");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        rig.run("kitty");
        rig.key('。');
        assert_eq!(rig.seen.last(), Some(&Seen::Confirm(Trick::Sit)));
    }

    #[test]
    fn a_narrow_stop_parks_a_no_space_run_like_any_other_token() {
        let mut rig = Rig::new();
        rig.ime("おすわり");
        rig.run("!");
        assert!(rig.seen.is_empty());
        rig.run("⏎");
        assert_eq!(
            rig.seen,
            [fire(Trick::Sit, "おすわり", 1, true), Seen::PetOnly]
        );
    }

    #[test]
    fn a_mixed_script_run_is_no_word() {
        let mut rig = Rig::new();
        rig.run("sit");
        rig.ime("坐");
        rig.run("␣⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
    }

    #[test]
    fn one_commit_reports_one_event() {
        // Dictation and text replacement commit whole phrases.
        let mut rig = Rig::new();
        rig.ime("sit ");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        let mut rig = Rig::new();
        rig.ime("sit kitty ");
        assert_eq!(
            rig.seen,
            [fire(Trick::Sit, "sit", 0, true)],
            "the confirmation folds into the fire it confirms"
        );
        let mut rig = Rig::new();
        rig.ime("sit tight ");
        assert_eq!(
            rig.seen,
            [Seen::Revoke],
            "the newest outcome; a no-op for the host"
        );
        assert_eq!(rig.listener.tentative(), None);
        let mut rig = Rig::new();
        rig.ime("good kitty ");
        assert_eq!(rig.seen, [fire(Trick::Purr, "good", 0, true)]);
    }

    // ---- sessions ----

    #[test]
    fn a_session_left_with_a_half_typed_line_comes_back_poisoned() {
        let mut rig = Rig::new();
        rig.listener.rekey(1);
        rig.run("si");
        rig.listener.rekey(2);
        rig.run("sit␣");
        assert_eq!(
            rig.seen,
            [fire(Trick::Sit, "sit", 0, false)],
            "a new session is fresh"
        );
        rig.listener.rekey(1);
        assert!(rig.listener.is_poisoned());
        rig.run("t␣");
        assert_eq!(
            rig.seen.len(),
            1,
            "letters typed in two visits are not a word"
        );
        rig.run("⏎sit␣");
        assert_eq!(
            rig.fires(),
            [Trick::Sit, Trick::Sit],
            "Enter proves the line clean"
        );
    }

    #[test]
    fn a_session_left_clean_comes_back_fresh() {
        let mut rig = Rig::new();
        rig.listener.rekey(1);
        rig.run("ls⏎");
        rig.listener.rekey(2);
        rig.listener.rekey(1);
        assert!(!rig.listener.is_poisoned());
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        // Rekeying to the session already bound changes nothing.
        rig.listener.rekey(1);
        assert_eq!(rig.listener.tentative(), Some(Trick::Sit));
    }

    #[test]
    fn a_poisoned_session_is_remembered_as_dirty() {
        let mut rig = Rig::new();
        rig.listener.rekey(1);
        rig.brk();
        rig.listener.rekey(2);
        rig.listener.rekey(1);
        assert!(rig.listener.is_poisoned());
        // Coming back does not launder it: left again untouched, it is
        // still dirty…
        rig.listener.rekey(2);
        rig.listener.rekey(1);
        assert!(rig.listener.is_poisoned());
        // …until a reset THERE proves its line clean.
        rig.run("⏎");
        rig.listener.rekey(2);
        rig.listener.rekey(1);
        assert!(!rig.listener.is_poisoned());
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

    #[test]
    fn the_dirty_memory_is_bounded_and_forgets_the_least_recently_left() {
        let mut rig = Rig::new();
        let sessions = SESSION_MEMORY as u64 + 1;
        for session in 0..sessions {
            rig.listener.rekey(session);
            rig.run("x");
        }
        rig.listener.rekey(u64::MAX);
        // The most recent SESSION_MEMORY are remembered…
        for session in (1..sessions).rev() {
            rig.listener.rekey(session);
            assert!(rig.listener.is_poisoned(), "session {session}");
            rig.listener.rekey(u64::MAX);
        }
        // …and the oldest reads as new.
        rig.listener.rekey(0);
        assert!(!rig.listener.is_poisoned());
    }

    // ---- the token window ----

    #[test]
    fn a_word_exactly_as_long_as_the_window_still_fires() {
        let word = "ps".repeat(TOKEN_CHARS / 2);
        assert_eq!(word.chars().count(), TOKEN_CHARS);
        let mut rig = Rig::new();
        rig.run(&word).run("␣");
        assert_eq!(rig.seen, [fire(Trick::Look, &word, 0, false)]);
    }

    #[test]
    fn a_token_longer_than_the_window_spoils_its_line_and_nothing_else() {
        let long = "ps".repeat(TOKEN_CHARS / 2) + "p";
        let mut rig = Rig::new();
        rig.run(&long).run("␣");
        assert!(rig.seen.is_empty());
        assert!(!rig.listener.is_pure(), "an unknown token, like any other");
        assert!(!rig.listener.is_poisoned(), "but only ITS line");
        // The count kept going past the window, so Backspace still finds the
        // start of the line…
        for _ in 0..=long.chars().count() {
            rig.backspace();
        }
        assert_eq!(rig.listener.line_chars(), 0);
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        // …and trimming it back INTO the window makes it a word again.
        let mut rig = Rig::new();
        rig.run(&long).run("⌫␣");
        assert_eq!(rig.fires(), [Trick::Look]);
    }

    // ---- SubmitPetOnly ----

    #[test]
    fn submit_pet_only_means_the_whole_submitted_line_was_pet_vocabulary() {
        for (line, pet_only) in [
            ("sit", true),
            ("kitty", true),
            ("please", true),
            ("good", true),
            ("good kitty", true),
            ("sit down kitty!", true),
            ("", false),
            ("   ", false),
            ("sit tight", false),
            ("ls", false),
            ("sit.txt", false),
            ("sit;", false),
            ("!sit", false),
        ] {
            let mut rig = Rig::new();
            rig.run(line).run("⏎");
            assert_eq!(rig.seen.contains(&Seen::PetOnly), pet_only, "{line:?}");
        }
        // A poisoned line is never vouched for.
        let mut rig = Rig::new();
        rig.run("sit");
        rig.brk();
        rig.run("⏎");
        assert!(!rig.seen.contains(&Seen::PetOnly));
        let mut rig = Rig::new();
        rig.no_echo();
        rig.run("sit⏎");
        assert!(rig.seen.is_empty());
    }

    // ---- the lexicon seam ----

    #[test]
    fn the_vocabulary_is_wanted_only_when_a_token_closes_on_a_pure_line() {
        let mut rig = Rig::new();
        assert!(!rig.listener.needs_lexicon(' '), "no open token");
        assert!(!rig.listener.submit_needs_lexicon());
        rig.run("si");
        assert!(!rig.listener.needs_lexicon('t'), "a letter is one push");
        assert!(!rig.listener.needs_lexicon('-'), "a joiner joins");
        assert!(
            !rig.listener.needs_lexicon(':'),
            "code drops the token unjudged"
        );
        for closer in [' ', '\u{3000}', '.', '!', '?', ',', '…', '。', '！'] {
            assert!(rig.listener.needs_lexicon(closer), "{closer:?}");
        }
        assert!(rig.listener.submit_needs_lexicon());
        assert!(rig.listener.ime_needs_lexicon("t "));
        assert!(!rig.listener.ime_needs_lexicon("t"));
        assert!(
            !rig.listener.ime_needs_lexicon("t: "),
            "impure before it closes"
        );
        // An ordinary line stops asking after its first word.
        rig.run("x␣");
        assert!(!rig.listener.is_pure());
        rig.run("sit");
        assert!(!rig.listener.needs_lexicon(' '));
        assert!(!rig.listener.submit_needs_lexicon());
        assert!(!rig.listener.ime_needs_lexicon("sit "));
        // So does a poisoned one.
        let mut rig = Rig::new();
        rig.run("sit");
        rig.brk();
        assert!(!rig.listener.needs_lexicon(' '));
        // A commit that opens AND closes a token asks.
        let rig = Rig::new();
        assert!(rig.listener.ime_needs_lexicon("ねこ、"));
        assert!(!rig.listener.ime_needs_lexicon("ねこ"));
        assert!(!rig.listener.ime_needs_lexicon("  "));
    }

    #[test]
    fn a_missing_vocabulary_at_a_boundary_fails_closed() {
        let mut listener = TrickListener::new();
        let now = Instant::now();
        for c in "sit".chars() {
            let _ = listener.note_char(now, c, None);
        }
        assert_eq!(listener.note_char(now, ' ', None), TrickFeed::NONE);
        assert!(!listener.is_pure(), "unknown, therefore not pet talk");
        // The EMPTY vocabulary (a host before its compile lands) is the same.
        let mut listener = TrickListener::default();
        let empty = TrickLexicon::default();
        for c in "sit".chars() {
            let _ = listener.note_char(now, c, Some(&empty));
        }
        assert_eq!(listener.note_submit(now, Some(&empty)), TrickFeed::NONE);
    }

    #[test]
    fn back_chars_measures_from_the_caret_the_press_arrived_at() {
        // (since_end, fed, word_chars) -> back_chars
        for (since_end, fed, word_chars, expected) in [
            (0, 0, 3, 0), // `sit␣`
            (6, 0, 4, 6), // `good kitty␣`
            (2, 0, 3, 2), // `sit!!␣`
            (0, 7, 4, 3), // `ねこ、おすわり！` in one commit
            (0, 3, 3, 0), // `sit ` in one commit: the caret is at the word's start
            (0, 1, 2, 0), // `앉`, then `아 ` in one commit: the caret is inside it
            (6, 6, 4, 0), // ` kitty` + space in one commit after `good`
            (u32::MAX, 0, 1, u16::MAX),
        ] {
            assert_eq!(
                back_chars(since_end, fed, word_chars),
                expected,
                "({since_end}, {fed}, {word_chars})"
            );
        }
    }

    // ---- the shipped English ----

    fn shipped_trick(word: &str) -> Trick {
        match TrickLexicon::shared_en().classify(word, &mut String::new()) {
            Some(TrickRole::Trick { trick, .. }) => trick,
            other => panic!("{word:?} is not a shipped trick word: {other:?}"),
        }
    }

    #[test]
    fn the_shipped_english_answers_the_owners_examples() {
        let mut rig = Rig::shipped_english();
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        let mut rig = Rig::shipped_english();
        rig.run("kitty␣jump␣");
        assert_eq!(rig.seen, [fire(Trick::Jump, "jump", 0, true)]);
        let mut rig = Rig::shipped_english();
        rig.run("good␣kitty␣");
        assert_eq!(rig.seen, [fire(shipped_trick("good"), "good", 6, true)]);
        let mut rig = Rig::shipped_english();
        rig.run("sit␣tight␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false), Seen::Revoke]);
    }

    #[test]
    fn the_shipped_english_says_each_two_word_phrase_once() {
        for line in ["sit␣down␣", "roll␣over␣", "jump␣up␣", "wake␣up␣"] {
            let first = line.split('␣').next().expect("a first word");
            let mut rig = Rig::shipped_english();
            rig.run(line);
            assert_eq!(
                rig.fires(),
                [shipped_trick(first)],
                "{line:?}: {:?}",
                rig.seen
            );
            assert!(rig.listener.is_pure(), "{line:?}");
        }
    }

    #[test]
    fn the_shipped_english_hears_good_night_kitty_as_one_final_sleep() {
        // Whichever list `night` sits in: plain, it fires and the name
        // confirms; an address word, it replaces the held `good` and the name
        // fires it. Either way one sleep, final, and never the praise.
        let mut rig = Rig::shipped_english();
        rig.run("good␣night␣kitty␣");
        assert_eq!(rig.fires(), [Trick::Sleep], "{:?}", rig.seen);
        assert!(rig.something_stuck());
        assert_eq!(rig.listener.tentative(), None, "final");
    }

    // ---- ordinary lines ----

    /// Ordinary prose, shell and REPL lines, every false-fire example of the
    /// misfire critique among them. Each is typed character by character and
    /// submitted.
    const ORDINARY_LINES: &[&str] = &[
        // Imperative prose that STARTS with a plain trick word.
        "roll back the last commit",
        "jump to line 40",
        "hide the sidebar on mobile",
        "treat warnings as errors",
        "play the animation once",
        "stretch the image to fit",
        "stay on this branch",
        "sleep on it",
        "sit tight while I check",
        "roll back flaky retry",
        // Fillers, then a trick word, then prose.
        "please hide the debug logs",
        "ok roll it back",
        "now play it",
        // Shell.
        "play song.wav",
        "sleep 0.5 && curl -s localhost:8080",
        "sleep 5",
        "time sleep 1",
        "FOO=1 sleep 5",
        "cat sleep.txt",
        "kitty play.py",
        "sleep.sh",
        "play cat.mp3",
        "hide cat.png",
        "sleep kitty.sh",
        "npm run build",
        "git fetch",
        "cargo run",
        "docker compose up",
        "echo sit",
        "ls -la",
        "cd ..",
        "./sit",
        "--help",
        "--sit",
        "sit.txt",
        "play.py",
        "git commit -m \"roll back flaky retry\"",
        "for f in *.rs; do echo $f; done",
        " sit tight",
        // REPL, editor and config lines.
        "play = 3",
        "roll = random.randint(1,6)",
        "hide = False",
        "sit = 3",
        "sit: 3",
        "play: 3",
        "sleep: 5",
        "hide: bool",
        "let play = 3;",
        "x = sit(3)",
        "print(\"sit\")",
        "sit_down()",
        "SELECT sleep(5);",
        "hide = {}",
        // A name inside ordinary prose (`cat` is a name AND cat(1)).
        "look at the cat output",
        "run the cat command",
        "wait for cat to finish",
        // Chat to a coding assistant.
        "/slash",
        "@file",
        "!bash-mode",
        "good catch",
        "nice work on the parser",
        "no that is wrong",
        "stop the server",
        "look at this file",
        "come on",
        "here is the log",
        "hello world",
        "hey claude",
        "good morning team",
        "please fix the failing test",
        "thanks, that works",
        // Address words alone: held, never fired.
        "wait",
        "stop",
        "no",
        "good",
    ];

    #[test]
    fn nothing_on_an_ordinary_line_ends_confirmed_or_unrevoked() {
        for line in ORDINARY_LINES {
            let mut rig = Rig::new();
            rig.run(line).run("⏎");
            assert!(!rig.something_stuck(), "{line:?}: {:?}", rig.seen);
            assert_eq!(rig.listener.tentative(), None, "{line:?}");
        }
    }

    /// The same lines against the SHIPPED English. Which of them survive
    /// depends on the word lists (a filler list that holds `it` lets
    /// `now play it` through; one that holds `for` makes `wait for cat` an
    /// address), so what is pinned here is the listener's half of the
    /// bargain, which no data pass can change: a fire becomes final ONLY
    /// while every word typed so far is shipped pet vocabulary.
    #[test]
    fn under_the_shipped_english_a_fire_is_final_only_on_an_all_vocabulary_prefix() {
        let shipped = TrickLexicon::shared_en();
        let mut scratch = String::new();
        for line in ORDINARY_LINES {
            let mut rig = Rig::shipped_english();
            let mut typed = String::new();
            let mut final_at: Option<String> = None;
            for c in line.chars() {
                rig.key(c);
                let is_final = rig.seen.iter().any(|seen| {
                    matches!(
                        seen,
                        Seen::Confirm(_)
                            | Seen::Fire {
                                addressed: true,
                                ..
                            }
                    )
                });
                if is_final && final_at.is_none() {
                    // Final at the character just typed: judge what stood
                    // before it.
                    final_at = Some(typed.clone());
                }
                typed.push(c);
            }
            rig.enter();
            if final_at.is_none() && rig.something_stuck() {
                final_at = Some(typed);
            }
            let Some(prefix) = final_at else {
                continue;
            };
            for piece in prefix.split_whitespace() {
                let word = piece.trim_end_matches(['.', '!', '?', ',']);
                assert!(
                    shipped.classify(word, &mut scratch).is_some(),
                    "{line:?} went final after {prefix:?}, yet {piece:?} is not pet vocabulary: {:?}",
                    rig.seen
                );
            }
        }
    }

    // ---- robustness ----

    /// xorshift64*: a deterministic stream, no dependency.
    struct Dice(u64);

    impl Dice {
        fn roll(&mut self, sides: usize) -> usize {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let mixed = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
            (mixed >> 33) as usize % sides.max(1)
        }
    }

    /// Letters that spell vocabulary, every kind of punctuation, no-space
    /// scripts, zero-width marks, a four-byte letter and a fold that expands.
    const ALPHABET: &[char] = &[
        's', 'i', 't', 'k', 'y', 'g', 'o', 'd', 'n', 'a', 'p', 'l', 'u', 'm', 'j', 'S', 'T', ' ',
        ' ', ' ', ' ', '\u{3000}', '.', '!', '?', ',', '…', '。', '！', '、', ':', ';', '/', '-',
        '\'', '_', '=', '$', '坐', '下', '跳', '猫', '咪', '앉', '아', 'น', 'ั', '่', 'ง', '𝐚', 'ß',
        '\u{301}', '\u{200d}', '\n', '\t', '\u{0}',
    ];

    fn random_walk(rig: &mut Rig, dice: &mut Dice, steps: usize) {
        for _ in 0..steps {
            // A character, a commit and Enter can only take purity AWAY, and
            // what can give it back (Backspace, the resets, a session switch)
            // never fires. So whatever a step fires or confirms, the line
            // was pure and unpoisoned when the step began.
            let before = rig.seen.len();
            let judging = rig.listener.is_pure() && !rig.listener.is_poisoned();
            match dice.roll(24) {
                0 => rig.backspace(),
                1 => rig.enter(),
                2 => rig.line_reset(),
                3 => rig.word_kill(),
                4 => rig.brk(),
                5 => rig.no_echo(),
                6 => rig.listener.rekey(dice.roll(12) as u64),
                7 => rig.wait(Duration::from_millis(dice.roll(2500) as u64)),
                8 | 9 => {
                    let len = dice.roll(6);
                    let text: String = (0..len)
                        .map(|_| ALPHABET[dice.roll(ALPHABET.len())])
                        .collect();
                    rig.ime(&text);
                }
                10 => {
                    // A real word now and then, so fires actually happen.
                    let words = ["sit", "kitty", "good", "jump", "please", "down"];
                    rig.run(words[dice.roll(words.len())]).run("␣");
                }
                _ => rig.key(ALPHABET[dice.roll(ALPHABET.len())]),
            }
            let fired = rig.seen[before..]
                .iter()
                .any(|seen| matches!(seen, Seen::Fire { .. } | Seen::Confirm(_)));
            assert!(
                !fired || judging,
                "fired on an impure or poisoned line: {:?}",
                &rig.seen[before..]
            );
            if rig.listener.is_poisoned() {
                assert_eq!(rig.listener.tentative(), None, "poison revokes");
            }
            if !rig.listener.is_pure() {
                assert_eq!(rig.listener.tentative(), None, "impurity revokes");
            }
        }
    }

    #[test]
    fn arbitrary_input_never_panics_and_never_fires_on_a_dead_line() {
        for seed in 1..=8u64 {
            let mut rig = Rig::new();
            let mut dice = Dice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            random_walk(&mut rig, &mut dice, 6_000);
            assert!(!rig.fires().is_empty(), "seed {seed}: the walk must fire");
            rig.line_reset();
            assert_eq!(rig.listener.tentative(), None);
            assert_eq!(rig.listener.line_chars(), 0);
        }
        // Every ordinary line, character by character, under both tables.
        for line in ORDINARY_LINES {
            for mut rig in [Rig::new(), Rig::shipped_english()] {
                rig.run(line).run("⌫⌫⌫").run(line).run("⏎");
            }
        }
    }

    // ---- the oracle: what is REALLY on the line ----

    /// A line editor the size of a test. It knows nothing of tokens, purity,
    /// snapshots or counts — only TEXT — so it can say whether what the
    /// listener believes about the line is still true. (The walk above asks
    /// the listener's own flags, and a wrong snapshot restore would pass it.)
    /// Backspace takes one cluster, as readline and zsh do; Ctrl+W is
    /// readline's: back over whitespace, then over the word.
    #[derive(Default)]
    struct Editor {
        line: Vec<char>,
    }

    impl Editor {
        fn backspace(&mut self) {
            while let Some(c) = self.line.pop() {
                if aterm_grapheme::char_width(c) != 0 {
                    break;
                }
            }
        }

        fn word_kill(&mut self) {
            while self.line.last().is_some_and(|c| c.is_whitespace()) {
                self.line.pop();
            }
            while self.line.last().is_some_and(|c| !c.is_whitespace()) {
                self.line.pop();
            }
        }

        /// The words of the line as a READER splits it: at whitespace and
        /// fullwidth stops, each word allowed a tail of sentence punctuation.
        /// Wider than the listener on purpose (`. sit` reads as pet talk
        /// here): the oracle judges what may never happen, not what must.
        fn roles(&self, lexicon: &TrickLexicon) -> Vec<Option<TrickRole>> {
            let text: String = self.line.iter().collect();
            let mut scratch = String::new();
            text.split(|c: char| matches!(kind_of(c), Kind::Space | Kind::WideStop))
                .map(|piece| piece.trim_end_matches(|c: char| kind_of(c) == Kind::Stop))
                .filter(|word| !word.is_empty())
                .map(|word| lexicon.classify(word, &mut scratch))
                .collect()
        }

        /// `line[start..end]` is a WHOLE word: nothing word-like touches it.
        fn is_whole_word(&self, start: usize, end: usize) -> bool {
            let line = &self.line;
            let bounded_left =
                start == 0 || matches!(kind_of(line[start - 1]), Kind::Space | Kind::WideStop);
            let bounded_right = line
                .get(end)
                .is_none_or(|c| !matches!(kind_of(*c), Kind::Word | Kind::Joiner));
            bounded_left && bounded_right
        }

        /// The fired word stands where the event says it does — `back_chars`
        /// left of the caret at the end of the line — as a whole word.
        fn assert_word_at(&self, word: &str, back_chars: u16) {
            let word: Vec<char> = word.chars().collect();
            let line = &self.line;
            let end = line.len().checked_sub(usize::from(back_chars));
            let start = end.and_then(|end| end.checked_sub(word.len()));
            let (Some(start), Some(end)) = (start, end) else {
                panic!("{word:?} back {back_chars} does not fit on {line:?}");
            };
            assert_eq!(line[start..end], word[..], "back {back_chars} on {line:?}");
            assert!(self.is_whole_word(start, end), "{word:?} on {line:?}");
        }

        /// The same for a word heard INSIDE a commit. The caret still stood
        /// before the whole commit when the press arrived, so the word may lie
        /// on either side of it — or around it — `back_chars` away.
        fn assert_word_near(&self, caret: usize, word: &str, back_chars: u16) {
            let word: Vec<char> = word.chars().collect();
            assert!(!word.is_empty());
            let line = &self.line;
            let away = usize::from(back_chars);
            let found = line.windows(word.len()).enumerate().any(|(start, at)| {
                let end = start + word.len();
                let placed = if end <= caret {
                    caret - end == away
                } else if start >= caret {
                    start - caret == away
                } else {
                    away == 0
                };
                at == &word[..] && placed && self.is_whole_word(start, end)
            });
            assert!(
                found,
                "{word:?} is not {away} from caret {caret} on {line:?}"
            );
        }
    }

    /// The events of one CLOSING press — a typed character, or Enter
    /// (`closer = None`) — judged against the text that stood on the line
    /// when the press arrived.
    fn judge(rig: &Rig, before: usize, editor: &Editor, closer: Option<char>, judging: bool) {
        let heard = &rig.seen[before..];
        if heard.iter().all(|seen| *seen == Seen::Revoke) {
            // Taking a fire back needs no licence.
            return;
        }
        let line = &editor.line;
        assert!(judging, "{heard:?} on a dead line {line:?}");
        // PREFIX NEVER FIRES: only a boundary speaks.
        assert!(
            closer.is_none_or(|c| matches!(kind_of(c), Kind::Space | Kind::WideStop)),
            "{heard:?} at {closer:?} on {line:?}"
        );
        // NOTHING BUT PET TALK stood on the line.
        let roles = editor.roles(&rig.lexicon);
        assert!(
            !roles.is_empty() && roles.iter().all(Option::is_some),
            "{heard:?} on {line:?}"
        );
        let named = roles.contains(&Some(TrickRole::Vocative));
        let submitted = closer.is_none();
        for seen in heard {
            match seen {
                Seen::Fire {
                    word,
                    back_chars,
                    addressed,
                    ..
                } => {
                    editor.assert_word_at(word, *back_chars);
                    // Final exactly when the line names the pet, or was sent.
                    assert_eq!(*addressed, named || submitted, "{heard:?} on {line:?}");
                }
                Seen::Confirm(_) => assert!(named || submitted, "{heard:?} on {line:?}"),
                Seen::PetOnly => assert!(submitted, "{heard:?} on {line:?}"),
                Seen::Revoke => {}
            }
        }
    }

    /// An editing key fires nothing, ever: all it may say is `Revoke`.
    fn assert_only_takes_back(rig: &Rig, before: usize) {
        let heard = &rig.seen[before..];
        assert!(heard.iter().all(|seen| *seen == Seen::Revoke), "{heard:?}");
    }

    /// The event of an IME commit (one, by the feed's type), judged against
    /// the text before it (`editor`) and the commit. Inside a commit purity
    /// can only be LOST, so whatever it fired was licensed at some boundary
    /// inside it: the text left of that boundary was nothing but pet talk.
    fn judge_commit(rig: &Rig, before: usize, editor: &Editor, commit: &[char], judging: bool) {
        let heard = &rig.seen[before..];
        if heard.iter().all(|seen| *seen == Seen::Revoke) {
            return;
        }
        let line = &editor.line;
        assert!(judging, "{heard:?} on a dead line {line:?} + {commit:?}");
        let caret = line.len();
        let mut after = Editor { line: line.clone() };
        let mut licensed = false;
        for c in commit {
            if matches!(kind_of(*c), Kind::Space | Kind::WideStop) {
                let roles = after.roles(&rig.lexicon);
                licensed |= !roles.is_empty() && roles.iter().all(Option::is_some);
            }
            after.line.push(*c);
        }
        assert!(licensed, "{heard:?} on {line:?} + {commit:?}");
        if let Some(Seen::Fire {
            word, back_chars, ..
        }) = heard.first()
        {
            after.assert_word_near(caret, word, *back_chars);
        }
    }

    /// What the listener believes, held against the text.
    fn assert_beliefs_hold(rig: &Rig, editor: &Editor) {
        let listener = &rig.listener;
        if listener.is_poisoned() {
            // Poison claims nothing, and only a reset — which empties the
            // editor too — lifts it.
            return;
        }
        let line = &editor.line;
        // ZERO PROVES THE LINE EMPTY, whatever else went wrong on it.
        if listener.line_chars() == 0 {
            assert!(line.is_empty(), "count 0 on {line:?}");
        }
        if !listener.is_pure() {
            return;
        }
        // A PURE line is a KNOWN line: empty exactly when the count says so,
        // and the open token is the word the text really ends with (plus, on
        // a full window, the marks that ride uncounted on its last
        // character).
        assert_eq!(listener.line_chars() == 0, line.is_empty(), "{line:?}");
        let word_start = line
            .iter()
            .rposition(|c| !matches!(kind_of(*c), Kind::Word | Kind::Joiner))
            .map_or(0, |at| at + 1);
        let real: String = line[word_start..].iter().collect();
        let held = listener.line.token.as_str();
        if listener.line.token_chars as usize > TOKEN_CHARS {
            assert!(real.starts_with(held), "{held:?} against {line:?}");
        } else {
            let riding = real.strip_prefix(held);
            assert!(
                riding
                    .is_some_and(|marks| marks.chars().all(|c| aterm_grapheme::char_width(c) == 0)),
                "{held:?} against {line:?}"
            );
        }
    }

    #[test]
    fn every_fire_and_every_empty_line_agrees_with_a_simulated_line_editor() {
        // Words that fire, hold, name, fill, go wrong and come from other
        // scripts; closers of every kind.
        let words = [
            "sit",
            "sit",
            "kitty",
            "good",
            "jump",
            "down",
            "please",
            "cat",
            "siy",
            "ls",
            "坐下",
            "猫咪",
            "นั่ง",
            "goooood",
            "night-night",
        ];
        let closers = [' ', ' ', ' ', '.', '!', ',', '\u{3000}', '、', '！', ';'];
        let (mut fires, mut fires_after_an_edit, mut emptied) = (0usize, 0usize, 0usize);
        let mut commit_fires = 0usize;
        for seed in 1..=12u64 {
            let mut rig = Rig::new();
            let mut editor = Editor::default();
            let mut dice = Dice(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
            // The line has been backspaced or word-killed since it began.
            let mut edited = false;
            for _ in 0..4_000 {
                let before = rig.seen.len();
                let judging = rig.listener.is_pure() && !rig.listener.is_poisoned();
                let mut typed: Vec<char> = Vec::new();
                match dice.roll(32) {
                    0..=7 => {
                        let had_text = !editor.line.is_empty();
                        rig.backspace();
                        editor.backspace();
                        assert_only_takes_back(&rig, before);
                        edited = true;
                        emptied += usize::from(had_text && editor.line.is_empty());
                    }
                    8 => {
                        rig.word_kill();
                        editor.word_kill();
                        assert_only_takes_back(&rig, before);
                        edited = true;
                    }
                    9 | 10 => {
                        rig.enter();
                        judge(&rig, before, &editor, None, judging);
                        editor.line.clear();
                        edited = false;
                    }
                    11 => {
                        rig.line_reset();
                        editor.line.clear();
                        assert_only_takes_back(&rig, before);
                        edited = false;
                    }
                    12 => {
                        // Tab, an arrow: the editor's text is anyone's guess
                        // now — and the listener, poisoned, claims nothing
                        // until a reset empties both.
                        rig.brk();
                        assert_only_takes_back(&rig, before);
                    }
                    13 => rig.wait(Duration::from_millis(dice.roll(3000) as u64)),
                    14 | 15 => {
                        // An IME commit, dictation, a text replacement: a
                        // few words and characters in ONE press.
                        let mut commit: Vec<char> = Vec::new();
                        for _ in 0..=dice.roll(3) {
                            if dice.roll(4) == 0 {
                                commit.push(ALPHABET[dice.roll(ALPHABET.len())]);
                            } else {
                                commit.extend(words[dice.roll(words.len())].chars());
                                commit.push(closers[dice.roll(closers.len())]);
                            }
                        }
                        let text: String = commit.iter().collect();
                        rig.ime(&text);
                        judge_commit(&rig, before, &editor, &commit, judging);
                        let fired = rig.seen[before..]
                            .iter()
                            .any(|seen| matches!(seen, Seen::Fire { .. }));
                        commit_fires += usize::from(fired);
                        editor.line.extend(commit);
                    }
                    16..=25 => {
                        typed.extend(words[dice.roll(words.len())].chars());
                        typed.push(closers[dice.roll(closers.len())]);
                    }
                    _ => typed.push(ALPHABET[dice.roll(ALPHABET.len())]),
                }
                for c in typed {
                    let before = rig.seen.len();
                    let judging = rig.listener.is_pure() && !rig.listener.is_poisoned();
                    rig.key(c);
                    judge(&rig, before, &editor, Some(c), judging);
                    let fired = rig.seen[before..]
                        .iter()
                        .any(|seen| matches!(seen, Seen::Fire { .. }));
                    fires += usize::from(fired);
                    fires_after_an_edit += usize::from(fired && edited);
                    editor.line.push(c);
                    assert_beliefs_hold(&rig, &editor);
                }
                assert_beliefs_hold(&rig, &editor);
            }
        }
        // The walk went where the laws bite: fires, fires on a line that had
        // been backspaced or word-killed into shape, fires inside a commit,
        // lines emptied by hand.
        assert!(
            fires >= 200 && fires_after_an_edit >= 20 && commit_fires >= 20 && emptied >= 20,
            "{fires} fires, {fires_after_an_edit} after an edit, {commit_fires} in a commit, \
             {emptied} lines emptied"
        );
    }

    #[test]
    fn the_heap_the_listener_holds_is_stable_once_warm() {
        let mut rig = Rig::new();
        assert_eq!(
            rig.listener.heap_capacity(),
            0,
            "nothing until a token closes"
        );
        // Warm-up: the widest token the window can hold — TOKEN_CHARS
        // four-byte letters — sizes the fold scratch once and for all.
        let widest = "𝐚".repeat(TOKEN_CHARS);
        rig.run(&widest).run("␣⏎");
        let warm = rig.listener.heap_capacity();
        assert!(warm >= widest.len());
        let mut dice = Dice(0xC0FF_EE00_D15E_A5E5);
        random_walk(&mut rig, &mut dice, 6_000);
        for line in ORDINARY_LINES {
            rig.run(line).run("⏎");
        }
        rig.ime("ねこ、おすわり！");
        rig.run(&"ß".repeat(TOKEN_CHARS)).run("␣⏎");
        assert_eq!(
            rig.listener.heap_capacity(),
            warm,
            "steady state allocates nothing"
        );
    }
}
