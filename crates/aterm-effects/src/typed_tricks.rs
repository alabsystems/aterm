// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kitty-command listener: the typed-line state machine that decides when a
//! word just typed (`sit`, `kitty jump`, `good kitty`) is a command to the pet.
//!
//! It is pure: every feed call takes the injected `now`, nothing is drawn and
//! nothing is latched into the pet. It answers with a [`TrickEvent`]; the host
//! decides what an event may do (feature gates, serious mode, ignore lists). A
//! host feeds it always, gates or no gates, so re-enabling a feature never
//! finds a half-typed line it missed.
//!
//! # Firing: a word boundary on a pure line
//!
//! A trick fires only when its token is closed (whitespace, Enter, sentence
//! punctuation), so `sit` never fires inside `site`, and only while the line is
//! pure: every token so far was pet vocabulary (trick word, pet name, filler)
//! and no code punctuation appeared. `npm run build`, `./sit`, `--sit`,
//! `sit.txt`, `sit: 3` and `play.py` never move the pet.
//!
//! Imperative prose often starts with a trick word (`roll back the commit`,
//! `sleep 5`), so there are two tiers:
//!
//! * **Addressed**: the line also names the pet (`kitty sit`). The fire is final.
//! * **Tentative**: a plain word on a line without the name, reported with
//!   `addressed = false`. It is revoked when the line turns impure (`sit tight`)
//!   and confirmed by Enter on a still-pure line or a later name (`sit kitty`).
//!
//! An address word (`good`, `no`, `look`: too common to fire alone) is held
//! until the pet is named, then fires as the word: `good kitty` purrs at
//! `kitty`, flashing `good`. A name alone never fires or flashes.
//!
//! # One phrase, one trick
//!
//! The first fire of a phrase wins: a later trick word on the same pure line
//! within [`PHRASE_GAP`] of the previous fire is absorbed like a filler (`sit
//! down`, `roll over`). After the gap it is a new request. A plain fire clears
//! a held address word, and a name arriving while a tentative fire is live
//! confirms that fire only: `good night kitty` is one sleep, never a purr.
//!
//! # Punctuation
//!
//! * Whitespace (U+3000 included) closes a token; with no open token it is a
//!   no-op.
//! * `. ! ? , …` close a token but park its fire until the next Space or Enter
//!   (`sit.`, `sit!!`). A token character glued to the stop (`sit.t`, `v1.2`,
//!   `a,b`) cancels it and makes the line impure. At line start they are code
//!   (`!sit`, `.hidden`).
//! * `。！？、，` are full boundaries: `ねこ、おすわり！` fires at `！`.
//! * Everything else (`: ; / \ = @ # + ~ $ % | < > * & ^ ( ) [ ] { } " \``, a
//!   joiner with nothing to join) makes the line impure; `play: 3` is YAML.
//!
//! The lexicon's code-context rule (`left_suppresses` / `right_suppresses`)
//! cannot trip on a pure line, so it is not reimplemented here.
//!
//! # No-space scripts and the IME
//!
//! A kana, ideograph, Hangul or Thai run is one token, looked up whole at its
//! boundary. The end of an IME commit is not a boundary (the macOS Korean IME
//! commits `앉아` as `앉` then `아`), so runs accumulate across
//! [`TrickListener::note_char`] and [`TrickListener::note_ime`] alike.
//!
//! # A name glued to a command
//!
//! Someone who writes without spaces types the name and the command as one run
//! (`猫咪坐下`, `ねこおすわり`, `แมวนั่ง`). A token that is no vocabulary word
//! gets one more reading: a name and a trick word, in either order. It is a
//! guess (`猫咪洗澡，需要注意什么` opens with "kitty, wash" and is a question
//! about bathing a cat; `猫咪、狗狗等，…` lists pets), so it says nothing at its
//! own boundary. It waits, unflashed: Enter on a still-pure line sends it, and
//! a later name vouches for it, both final; a word that is not pet vocabulary
//! drops it unheard, and a plain fire after it is the newer request. The name
//! inside the run neither names the line nor fires a held word, and a name
//! before the run vouches for nothing after it. It is narrow:
//!
//! * every character is of a script written without spaces (ideographs, kana,
//!   Thai, Lao, Khmer). Latin, every other spaced script and a mixed run never
//!   split, and neither does Hangul: Korean writes its spaces;
//! * exactly a name and a trick word, whole, the name at least two characters
//!   (`猫爪` is a claw), neither half one the vocabulary marks `unglued` (a
//!   thing, an everyday-word name: `猫咪玩具` is a cat toy);
//! * every cut that reads it asks for the same trick (`わるいこ|ねこ`,
//!   `わるい|こねこ`); cuts that disagree fire nothing.
//!
//! The fired word is the whole run, because the flash finds whole words only;
//! [`TrickEvent::Fire::commands`] carries the command half beside it.
//!
//! # Typo recovery
//!
//! * The listener counts characters typed on the line; Backspace takes them off
//!   (a zero-width scalar goes with its base, as in readline and zsh). A count
//!   of zero is a provably empty line and resets it to fresh and pure.
//!   Backspace at zero is a no-op.
//! * One boundary can be crossed backwards: closing a token snapshots the line
//!   and backspacing over the closer restores it (`siy␣⌫⌫t␣` fires), revoking
//!   a tentative fire made there. Past a second boundary the line is impure but
//!   still counted, so holding Backspace to the start recovers any line.
//! * Ctrl+W on an open alphanumeric token after whitespace clears the token; on
//!   an empty line it is a no-op; any other word-kill poisons the line.
//!
//! # Poison
//!
//! Tab, Esc, arrows, other chords, a paste, controller bytes or a no-echo
//! prompt leave text the listener did not see (or move the caret off the end).
//! A poisoned line fires and counts nothing until a line reset (Enter, Ctrl+C,
//! Ctrl+U, …), and a live tentative fire is revoked. A no-echo prompt poisons
//! for privacy. So does a character without its own column (a control
//! character, ZWJ, a variation selector, a baseless combining mark).
//!
//! # Cost
//!
//! One inline push per letter; one fold and one hash probe per closed token,
//! only while the line is pure, and for a no-space token that misses, one probe
//! per cut and a second where the head is a word.
//! [`TrickListener::needs_lexicon`] tells the host when it need not fetch the
//! vocabulary at all. State is inline, sized by
//! [`TrickLexicon::MAX_SURFACE_CHARS`]; after the fold scratch warms up the
//! listener never allocates (`tests/typed_tricks_alloc.rs`).

use aterm_lexicon::{
    Trick, TrickLexicon, TrickRole, is_interior_joiner, is_no_space_script, is_token_char,
};
use aterm_time::{Duration, Instant};

/// A trick word within this long of the previous fire on the same line belongs
/// to that fire's phrase (`sit down`) and is absorbed; later it is a new request.
pub const PHRASE_GAP: Duration = Duration::from_millis(1500);

/// The token window, in characters: exactly the longest surface the
/// vocabulary can hold, so a token that overflows it could never have matched.
const TOKEN_CHARS: usize = TrickLexicon::MAX_SURFACE_CHARS;

/// Bytes behind one inline word: every character is at most four.
const WORD_BYTES: usize = TOKEN_CHARS * 4;

// `Word::len` is a `u8`.
const _: () = assert!(WORD_BYTES <= u8::MAX as usize);

/// Characters of the delimiter run left of the caret that are remembered, so
/// Backspace inside the run knows what it ends with. Backspacing into the
/// unseen part of a longer run makes the line impure until the run is gone.
const GAP_WINDOW: usize = 8;

/// Dirty sessions remembered across switches (least recently left is
/// forgotten first, and a forgotten session reads as new).
const SESSION_MEMORY: usize = 8;

/// What the listener heard; at most one per fed press.
///
/// `word` borrows the listener's inline buffer: the trick token exactly as typed
/// (`SIT`, `goooood`, a glued `猫咪坐下` whole), never the name that completed
/// an address, because the flash has to find it on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrickEvent<'a> {
    /// A trick word fired.
    Fire {
        /// The trick asked for.
        trick: Trick,
        /// The trick token as typed.
        word: &'a str,
        /// The command inside `word`, for a host's ignore list: `word` itself,
        /// or the command half of a glued run (`坐下` of `猫咪坐下`).
        commands: Commands<'a>,
        /// Typed characters between `word` and the caret as it stood when this
        /// press arrived (before its echo). `0` for a plain fire; for a held
        /// fire, what was typed since the address word (`good kitty␣`: 6); for
        /// a parked one, the punctuation (`sit!!␣`: 2). Inside a multi-character
        /// IME commit, the commit characters before the word
        /// (`ねこ、おすわり！`: 3).
        back_chars: u16,
        /// `true`: final (the line names the pet, or Enter submitted a pure
        /// line). `false`: tentative; a [`TrickEvent::Confirm`] or
        /// [`TrickEvent::Revoke`] may follow.
        addressed: bool,
    },
    /// The live tentative fire is final: Enter on a still-pure line, or the
    /// line named the pet. Carries the trick so the host need not remember it.
    Confirm {
        /// The trick of the fire being confirmed.
        trick: Trick,
    },
    /// The live tentative fire is withdrawn: the line turned impure, was
    /// aborted, was poisoned, or its word was backspaced into.
    Revoke,
}

/// The command surfaces inside a fired word, borrowed from it: the word itself,
/// or for a glued run the half beside the name at each cut that read it
/// (`わるいこねこ`: `わるいこ` and `わるい`). Walking them allocates nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Commands<'a> {
    word: &'a str,
    cuts: Cuts,
}

impl<'a> Commands<'a> {
    /// Each command surface as typed: at least one, never empty, never the
    /// name.
    pub fn iter(self) -> impl Iterator<Item = &'a str> {
        let Commands {
            word,
            cuts: Cuts { mut at, name_first },
        } = self;
        std::iter::from_fn(move || {
            if at == 0 {
                return None;
            }
            let cut = usize::try_from(at.trailing_zeros()).unwrap_or(usize::MAX);
            at &= at - 1;
            let half = if name_first {
                word.get(cut..)
            } else {
                word.get(..cut)
            };
            // Every cut is a character boundary of this word; the whole word
            // is the safe answer were one not.
            Some(half.unwrap_or(word))
        })
    }
}

/// Where a fired word's command lies in it ([`Commands`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Cuts {
    /// Bit `b`: the word splits into a name and its command at byte `b`.
    at: u128,
    /// The name comes first, so the command is what follows a cut.
    name_first: bool,
}

impl Cuts {
    /// Not glued: an empty name at byte 0, the whole word the command.
    const WHOLE: Cuts = Cuts {
        at: 1,
        name_first: true,
    };
}

// A cut is a byte offset inside the inline word, one bit apiece.
const _: () = assert!(WORD_BYTES <= u128::BITS as usize);

/// The answer to one fed press.
#[must_use = "an unread feed is a lost fire, confirm or revoke"]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TrickFeed<'a> {
    /// What the press meant to the pet, if anything.
    pub event: Option<TrickEvent<'a>>,
    /// Enter submitted a line of nothing but pet vocabulary (at least one word,
    /// pure, unpoisoned). The host uses it to forgive the shell's exit 127. May
    /// accompany a `Fire` or `Confirm`; only [`TrickListener::note_submit`]
    /// sets it.
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
/// snapshots, held address words and parked fires carry it by value.
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

    /// Only whole characters go in and out, so the prefix is valid UTF-8; the
    /// checked conversion runs at boundaries only.
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
    /// A control character or a zero-width scalar outside a word (ZWJ, a
    /// variation selector, a newline inside a commit). It gets no column, so
    /// Backspace would stop matching the count.
    Alien,
}

/// Uses the lexicon's own predicates, so a typed line segments exactly as the
/// vocabulary was segmented at load.
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

/// A character of a script written without spaces between words: the
/// vocabulary's no-space set minus Hangul, which is there only to be looked up
/// raw. Korean writes its spaces.
fn is_unspaced_script(c: char) -> bool {
    is_no_space_script(c) && !matches!(c as u32, 0x1100..=0x11FF | 0xAC00..=0xD7AF)
}

/// The shortest name a glued run may hold: a lone ideograph is a syllable of
/// prose far more often than a name (`猫爪` is a claw).
const GLUED_NAME_CHARS: usize = 2;

/// THE GLUED READING of a token that is no vocabulary word (module docs): the
/// trick it asks for and every cut that reads it so. `None` for a spaced or
/// mixed run, for no cut, and for cuts that disagree.
///
/// Both halves are borrowed from `token` and looked up raw, so nothing is
/// allocated: one probe per cut, a second where the head is a word.
fn glued(lexicon: &TrickLexicon, token: &str, scratch: &mut String) -> Option<(Trick, Cuts)> {
    if !token.chars().all(is_unspaced_script) {
        return None;
    }
    let chars = token.chars().count();
    let mut found: Option<(Trick, Cuts)> = None;
    for (head_chars, (at, _)) in token.char_indices().enumerate().skip(1) {
        let (head, tail) = token.split_at_checked(at)?;
        let head_role = lexicon.classify_glued_half(head, scratch);
        let name_first = match head_role {
            Some(TrickRole::Vocative) if head_chars >= GLUED_NAME_CHARS => true,
            Some(TrickRole::Trick { .. })
                if chars.saturating_sub(head_chars) >= GLUED_NAME_CHARS =>
            {
                false
            }
            Some(_) | None => continue,
        };
        let trick = match (head_role, lexicon.classify_glued_half(tail, scratch)) {
            (Some(TrickRole::Vocative), Some(TrickRole::Trick { trick, .. }))
            | (Some(TrickRole::Trick { trick, .. }), Some(TrickRole::Vocative)) => trick,
            _ => continue,
        };
        // `at` < `WORD_BYTES`, const-asserted to fit.
        let cut = 1u128.checked_shl(u32::try_from(at).ok()?)?;
        match found.as_mut() {
            None => {
                found = Some((
                    trick,
                    Cuts {
                        at: cut,
                        name_first,
                    },
                ));
            }
            // Two cuts asking for the same thing are one request…
            Some((first, cuts)) if *first == trick && cuts.name_first == name_first => {
                cuts.at |= cut;
            }
            // …and two that disagree are neither of them what was said.
            Some(_) => return None,
        }
    }
    found
}

/// How a token was closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edge {
    /// Whitespace, a fullwidth stop, Enter: a verdict is delivered now.
    Open,
    /// `. ! ? , …`: a verdict is parked until the next open edge.
    Stop,
}

/// What closing a token decided — delivered at an open edge, parked at a stop.
#[derive(Clone, Copy, Debug)]
enum Verdict {
    Fire {
        trick: Trick,
        word: Word,
        cuts: Cuts,
        word_chars: u32,
        /// `line_chars` when the word's last character went in; characters typed
        /// since are `line_chars - end_at`.
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

/// Fold the outcomes of one press (an IME commit's characters; Enter's closing
/// token and Enter) into the one event a press reports. The newest wins, except
/// that a `Confirm` folds into the tentative `Fire` it confirms, so `sit⏎` is
/// one final fire.
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
/// word and before this press's closer. `fed`: characters of this press already
/// consumed (the caret still stands before them). The word ends
/// `since_end - fed` left of the caret; when negative the word lies right of
/// the caret: distance 0 inside it, else what remains past its length.
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
    /// The open token, raw as typed. Holds the first [`TOKEN_CHARS`] characters;
    /// `token_chars` keeps counting, and an overflowed token is never looked up.
    token: Word,
    token_chars: u32,
    /// The delimiter run left of the open token (or of the caret), first
    /// [`GAP_WINDOW`] characters; `gap_chars` counts all of it.
    gap: [char; GAP_WINDOW],
    gap_chars: u32,
    /// The last delimiter typed; `None` at the start of the line.
    left: Option<char>,
    /// Characters typed on this line while unpoisoned, as Backspace gives them
    /// back (marks past the token window are uncounted; see `step`). Zero is a
    /// provably empty line only while the line is unpoisoned: poison stops the
    /// count, so text typed after it is never counted.
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
    /// An address word waiting for the pet to be named: the addressed fire
    /// the name will deliver.
    held: Option<Verdict>,
    /// A verdict parked at a stop, waiting for the next open edge.
    tail: Option<Verdict>,
    /// A glued run's fire, waiting for a name or Enter (module docs).
    guess: Option<Verdict>,
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
        guess: None,
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
/// ([`rekey`](Self::rekey)). The feed calls are the whole host surface, in the
/// order a host's press cascade consults them:
///
/// ```text
/// the tty swallows input (no echo)      note_no_echo   (Enter there: note_line_reset)
/// Enter                                 note_submit
/// raw bytes ending in CR / LF           note_line_reset
/// Ctrl+C  Ctrl+J  Ctrl+M  Ctrl+U        note_line_reset
/// Ctrl+Z  Ctrl+\                        note_job_chord
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
    /// Where that word's command lies in it.
    cuts: Cuts,
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
            cuts: Cuts::WHOLE,
            scratch: String::new(),
        }
    }

    // ---- the lexicon seam ----
    //
    // The vocabulary is consulted only when a token closes on a pure line. Each
    // feed that can close a token takes `Option<&TrickLexicon>` plus a predicate
    // saying whether this press needs it; the host passes `None` when it says
    // `false`. `None` when it said `true` fails closed: the line goes impure.

    /// Whether [`note_char`](Self::note_char) with `c` would consult the
    /// vocabulary: `c` closes a lookup-able open token on a pure, unpoisoned line.
    #[must_use]
    pub fn needs_lexicon(&self, c: char) -> bool {
        self.judging()
            && self.line.token_is_live()
            && matches!(kind_of(c), Kind::Space | Kind::Stop | Kind::WideStop)
    }

    /// [`needs_lexicon`](Self::needs_lexicon) for an IME commit. May say `true`
    /// for a commit that turns out not to ask; never `false` for one that does.
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
    #[cfg(test)]
    pub fn tentative(&self) -> Option<Trick> {
        self.line.tentative
    }

    /// Characters typed on this line while unpoisoned. Zero proves the line
    /// empty only when it is not [poisoned](Self::is_poisoned).
    #[must_use]
    pub fn line_chars(&self) -> u32 {
        self.line.line_chars
    }

    // ---- sessions ----

    /// Bind the listener to `session`, the window's front session; the same
    /// session again is one compare.
    ///
    /// A session left with typed characters or poisoned is remembered as dirty
    /// and comes back poisoned; an unseen or clean one starts fresh and pure. A
    /// live tentative fire is left to the host's own expiry.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "RekeyClean",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "RekeyDirty",
            project = "conformance_trick_listener::project"
        )
    )]
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

    /// One IME commit. Its end is not a boundary: the characters are fed as
    /// [`note_char`](Self::note_char) would, and an open run stays open. `lexicon`
    /// may be `None` unless [`ime_needs_lexicon`](Self::ime_needs_lexicon) says
    /// otherwise.
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

    /// Enter: close the open token (or deliver a parked fire), confirm a live
    /// tentative fire on a still-pure line, report
    /// [`TrickFeed::submit_pet_only`], and start a fresh line. `lexicon` may be
    /// `None` unless [`submit_needs_lexicon`](Self::submit_needs_lexicon) says
    /// otherwise.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "SubmitQuiet",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "SubmitFinal",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "SubmitConfirm",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "SubmitRevoke",
            project = "conformance_trick_listener::project"
        )
    )]
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
                // Submitting a pet-only line sends a waiting guess, the newest
                // request, or confirms; folded into this press's own fire,
                // `sit⏎` reports one final `Fire`.
                let answer = match self.line.guess.take() {
                    Some(guess) => self.deliver(now, guess, 0),
                    None => self
                        .line
                        .tentative
                        .take()
                        .map(|trick| Outcome::Confirm { trick }),
                };
                out = fold(out, answer);
                pet_only = self.line.any_word;
            }
        }
        self.line = Line::FRESH;
        self.snapshot = None;
        self.feed(out, pet_only)
    }

    /// The line was abandoned without being judged: Ctrl+C, Ctrl+J, Ctrl+M,
    /// Ctrl+U, controller bytes ending in CR / LF (the line that ran may be
    /// `sitxyz`), Enter at a no-echo prompt. Starts a fresh pure line and
    /// revokes a live tentative fire.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "Reset",
            project = "conformance_trick_listener::project"
        )
    )]
    pub fn note_line_reset(&mut self) -> TrickFeed<'_> {
        let out = self.fresh_line();
        self.feed(out, false)
    }

    /// Ctrl+Z or Ctrl+\: at a prompt every shell keeps its line through them,
    /// and under a program the tty flushed it, so only a provably empty line
    /// (unpoisoned, nothing counted) is reset; any other is a break. A poisoned
    /// line's count stopped at the poison: Esc then `sot`, a line recalled with
    /// ↑ and a paste leave text the shell keeps, where `sit␣` would fire
    /// inside `sotsit␣`.
    pub fn note_job_chord(&mut self) -> TrickFeed<'_> {
        if self.line.line_chars == 0 && !self.line.poisoned {
            self.note_line_reset()
        } else {
            self.note_break()
        }
    }

    /// A backward word-kill (Ctrl+W, Alt+Backspace, Ctrl+Backspace).
    ///
    /// Every editor removes an open letters-and-digits token after whitespace
    /// or at line start whole, so that case clears the token (`sot`, Ctrl+W,
    /// `sit␣` fires). Elsewhere editors disagree (`night-night`, `foo_bar`), so
    /// the line is poisoned. On a provably empty line it is a no-op.
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

    /// A press at a prompt whose tty swallows input (a password): poisons the
    /// line, so a passphrase cannot move the pet. The host answers Enter there
    /// with [`note_line_reset`](Self::note_line_reset) instead.
    pub fn note_no_echo(&mut self) -> TrickFeed<'_> {
        let out = self.poison();
        self.feed(out, false)
    }

    /// A plain Backspace (see "Typo recovery" in the module docs).
    ///
    /// The count is never lost: backspacing past what was remembered makes the
    /// line impure, but each press still takes one off the count while an
    /// editor takes at least one. The count can only err high, so zero still
    /// proves the line empty.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EraseSome",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EraseToken",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EraseBlind",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EraseLast",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "Restore",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "RestoreRevoke",
            project = "conformance_trick_listener::project"
        )
    )]
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
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "Ignored",
            project = "conformance_trick_listener::project"
        )
    )]
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
            // Past the window a mark rides with its base, uncounted: it is not
            // stored, so Backspace could not know it belongs to the previous
            // character, and counting it would leave the count high (a Thai
            // line backspaced to its start would look non-empty). A mark on the
            // window's last character is thereby left out of the lookup too.
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
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "Letter",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "LetterGlued",
            project = "conformance_trick_listener::project"
        )
    )]
    fn push_token_char(&mut self, c: char) -> Option<Outcome> {
        let mut out = None;
        if self.line.token_chars == 0 {
            if aterm_grapheme::char_width(c) == 0 {
                // A combining mark with no base in this token attaches to the
                // delimiter, and Backspace would take both: alien.
                return self.poison();
            }
            // Glue law: a token right after `. ! ? , …` with no whitespace is
            // code (`sit.txt`, `v1.2`, `a,b`), and cancels a parked fire.
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
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "MarkQuiet",
            project = "conformance_trick_listener::project"
        )
    )]
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
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EdgeQuiet",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EdgeName",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "MarkName",
            project = "conformance_trick_listener::project"
        )
    )]
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
        let role = match lexicon {
            Some(lexicon) if word_chars as usize <= TOKEN_CHARS => {
                match lexicon.classify(word.as_str(), &mut self.scratch) {
                    Some(role) => Some((role, Cuts::WHOLE)),
                    // No word, unless a name and a command written as one run
                    // (module docs): the name is right there, so the command
                    // needs no other.
                    None => {
                        glued(lexicon, word.as_str(), &mut self.scratch).map(|(trick, cuts)| {
                            let needs_vocative = false;
                            let role = TrickRole::Trick {
                                trick,
                                needs_vocative,
                            };
                            (role, cuts)
                        })
                    }
                }
            }
            _ => None,
        };
        let verdict = match role {
            Some((
                TrickRole::Trick {
                    trick,
                    needs_vocative,
                },
                cuts,
            )) => {
                self.line.any_word = true;
                let in_phrase = self
                    .line
                    .last_fire
                    .is_some_and(|at| now.saturating_duration_since(at) <= PHRASE_GAP);
                // A Look fire never wins a phrase: in `hey kitty sit` the verb
                // after the greeting is the request, so it fires instead of
                // being absorbed. `sit hey` still absorbs the greeting.
                let attention_only =
                    self.line.last_trick == Some(Trick::Look) && trick != Trick::Look;
                if in_phrase && !attention_only {
                    // One phrase, one trick: `down` of `sit down` is absorbed.
                    None
                } else if cuts != Cuts::WHOLE {
                    // A glued run is a guess: it waits for a name or Enter.
                    self.line.guess = Some(Verdict::Fire {
                        trick,
                        word,
                        cuts,
                        word_chars,
                        end_at,
                        addressed: true,
                    });
                    None
                } else if !needs_vocative || self.line.vocative_seen {
                    Some(Verdict::Fire {
                        trick,
                        word,
                        cuts,
                        word_chars,
                        end_at,
                        addressed: self.line.vocative_seen,
                    })
                } else {
                    // The newest address word is the one the name will fire.
                    self.line.held = Some(Verdict::Fire {
                        trick,
                        word,
                        cuts,
                        word_chars,
                        end_at,
                        addressed: true,
                    });
                    None
                }
            }
            Some((TrickRole::Vocative, _)) => {
                self.line.any_word = true;
                self.line.vocative_seen = true;
                // The name confirms a live tentative fire and drops the held
                // word and the guess: `good night kitty` is one sleep.
                // Otherwise it vouches for the guess, else fires the held word.
                let held = self.line.held.take();
                let guess = self.line.guess.take();
                match self.line.tentative {
                    Some(trick) => Some(Verdict::Confirm { trick }),
                    None => guess.or(held),
                }
            }
            Some((TrickRole::Filler, _)) => {
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
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EdgeConfirm",
            project = "conformance_trick_listener::project"
        )
    )]
    fn deliver_tail(&mut self, now: Instant, fed: u32) -> Option<Outcome> {
        let verdict = self.line.tail.take()?;
        self.deliver(now, verdict, fed)
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EdgeFire",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EdgeNameFires",
            project = "conformance_trick_listener::project"
        )
    )]
    fn deliver(&mut self, now: Instant, verdict: Verdict, fed: u32) -> Option<Outcome> {
        match verdict {
            Verdict::Fire {
                trick,
                word,
                cuts,
                word_chars,
                end_at,
                addressed,
            } => {
                let since_end = self.line.line_chars.saturating_sub(end_at);
                self.word = word;
                self.cuts = cuts;
                // A plain fire clears a held address word (`hey sit`) and a
                // waiting guess; a held fire has already taken them.
                self.line.held = None;
                self.line.guess = None;
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
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "MarkImpure",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "EdgeUnknown",
            project = "conformance_trick_listener::project"
        )
    )]
    fn go_impure(&mut self) -> Option<Outcome> {
        self.line.pure = false;
        self.line.held = None;
        self.line.tail = None;
        self.line.guess = None;
        self.line.tentative.take().map(|_| Outcome::Revoke)
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "TrickListenerPurity",
            action = "Lose",
            project = "conformance_trick_listener::project"
        )
    )]
    fn poison(&mut self) -> Option<Outcome> {
        self.line.poisoned = true;
        self.line.held = None;
        self.line.tail = None;
        self.line.guess = None;
        self.snapshot = None;
        self.line.tentative.take().map(|_| Outcome::Revoke)
    }

    fn fresh_line(&mut self) -> Option<Outcome> {
        let revoked = self.line.tentative.is_some();
        self.line = Line::FRESH;
        self.snapshot = None;
        revoked.then_some(Outcome::Revoke)
    }

    /// Take one cluster off the open token, as an editor's Backspace does:
    /// trailing zero-width scalars go with their base. Returns how many
    /// characters went. Past the token window characters go one at a time,
    /// which is still one cluster, since marks there were never counted.
    ///
    /// Exact for readline, zsh and vim. An editor that deletes one scalar per
    /// press can leave a stray mark the listener believes gone.
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
                commands: Commands {
                    word: self.word.as_str(),
                    cuts: self.cuts,
                },
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

    /// A small hermetic vocabulary, so these tests pin the listener rather than
    /// the shipped word lists (tests against shipped English say so in their
    /// names). Iconic verbs are plain; praise, scolding and openers are address
    /// words; fillers are interjections and glue only (`the`, `it`, `to` are
    /// not). `night` is plain. The last `look` word is exactly `TOKEN_CHARS`
    /// characters long. The no-space rows carry what the glued reading needs:
    /// full names, a one-character name, an address word, a filler, and an
    /// `unglued` thing and everyday-word name.
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

[[vocative]]
lang  = "th"
words = ["แมว"]
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
id      = "sit"
lang    = "zh"
words   = ["坐", "坐下"]
address = ["等"]
cjk     = true

[[trick]]
id    = "jump"
lang  = "zh"
words = ["跳"]
cjk   = true

[[trick]]
id      = "purr"
lang    = "zh"
address = ["真棒"]
cjk     = true

[[trick]]
id      = "play"
lang    = "zh"
words   = ["玩耍", "玩具"]
cjk     = true
unglued = ["玩具"]

[[vocative]]
lang    = "zh"
words   = ["猫咪", "猫", "宝宝", "狗狗"]
cjk     = true
unglued = ["宝宝"]

[[filler]]
lang  = "zh"
words = ["请"]
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

    /// Logs the event, and a fire's command surfaces into `commands`: each a
    /// non-empty prefix or suffix of the fired word, in every test.
    fn record(seen: &mut Vec<Seen>, commands: &mut Vec<Vec<String>>, feed: TrickFeed<'_>) {
        match feed.event {
            Some(TrickEvent::Fire {
                trick,
                word,
                commands: fired,
                back_chars,
                addressed,
            }) => {
                let fired: Vec<String> = fired.iter().map(str::to_owned).collect();
                assert!(!fired.is_empty(), "{word:?}: no command");
                for command in &fired {
                    assert!(
                        !command.is_empty()
                            && (word.starts_with(command) || word.ends_with(command)),
                        "{word:?}: command {command:?}"
                    );
                }
                commands.push(fired);
                seen.push(fire(trick, word, back_chars, addressed));
            }
            Some(TrickEvent::Confirm { trick }) => seen.push(Seen::Confirm(trick)),
            Some(TrickEvent::Revoke) => seen.push(Seen::Revoke),
            None => {}
        }
        if feed.submit_pet_only {
            seen.push(Seen::PetOnly);
        }
    }

    /// A listener, a vocabulary, an injected clock and a log. It hands over the
    /// vocabulary only when the matching `needs_lexicon` predicate asks, so every
    /// test also proves the predicates never under-ask.
    struct Rig {
        listener: TrickListener,
        lexicon: TrickLexicon,
        now: Instant,
        seen: Vec<Seen>,
        /// Each fire's command surfaces, in fire order.
        commands: Vec<Vec<String>>,
    }

    impl Rig {
        fn with(lexicon: TrickLexicon) -> Rig {
            assert!(lexicon.conflicts().is_empty(), "{:?}", lexicon.conflicts());
            Rig {
                listener: TrickListener::new(),
                lexicon,
                now: Instant::now(),
                seen: Vec::new(),
                commands: Vec::new(),
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
            record(&mut self.seen, &mut self.commands, feed);
        }

        fn ime(&mut self, text: &str) {
            self.now += KEY_GAP;
            let lexicon = self
                .listener
                .ime_needs_lexicon(text)
                .then_some(&self.lexicon);
            let feed = self.listener.note_ime(self.now, text, lexicon);
            record(&mut self.seen, &mut self.commands, feed);
        }

        fn enter(&mut self) {
            self.now += KEY_GAP;
            let lexicon = self
                .listener
                .submit_needs_lexicon()
                .then_some(&self.lexicon);
            let feed = self.listener.note_submit(self.now, lexicon);
            record(&mut self.seen, &mut self.commands, feed);
        }

        fn backspace(&mut self) {
            self.now += KEY_GAP;
            let feed = self.listener.note_backspace();
            record(&mut self.seen, &mut self.commands, feed);
        }

        fn word_kill(&mut self) {
            let feed = self.listener.note_word_kill();
            record(&mut self.seen, &mut self.commands, feed);
        }

        fn line_reset(&mut self) {
            let feed = self.listener.note_line_reset();
            record(&mut self.seen, &mut self.commands, feed);
        }

        fn brk(&mut self) {
            let feed = self.listener.note_break();
            record(&mut self.seen, &mut self.commands, feed);
        }

        fn no_echo(&mut self) {
            let feed = self.listener.note_no_echo();
            record(&mut self.seen, &mut self.commands, feed);
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
        // The listener's impure set must stay a superset of the lexicon's
        // code-context set (`cat=1`, `/api/cat`, `$cat`), which is the oracle.
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
        // and the verb inside the phrase gap fires instead of being absorbed.
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
        // A second Look inside the phrase is absorbed (`hey kitty look`); only
        // a Look fire yields.
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
        // A Thai sentence is one run far longer than the window, with marks
        // throughout; one Backspace per cluster must bring the count to zero.
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
        // An alien after a word: an editor's Backspace would take the `t` with
        // it, so the one-for-one count must not restore `sit`.
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

    // ---- a name glued to a command ----

    #[test]
    fn a_name_glued_to_its_command_waits_for_enter_either_way_round() {
        // (run, trick, command): a plain word, an address word, a lone
        // ideograph after a full name — each vouched for by the name.
        for (run, trick, command) in [
            ("猫咪坐下", Trick::Sit, "坐下"),
            ("坐下猫咪", Trick::Sit, "坐下"),
            ("猫咪真棒", Trick::Purr, "真棒"),
            ("真棒猫咪", Trick::Purr, "真棒"),
            ("猫咪跳", Trick::Jump, "跳"),
        ] {
            let mut rig = Rig::new();
            rig.ime(run);
            rig.run("␣");
            assert!(rig.seen.is_empty(), "{run}: a guess says nothing yet");
            rig.run("⏎");
            // The whole run is the word: the flash finds whole words only.
            assert_eq!(
                rig.seen,
                [fire(trick, run, 1, true), Seen::PetOnly],
                "{run}"
            );
            assert_eq!(rig.commands, [[command]], "{run}");
        }
    }

    #[test]
    fn every_unspaced_script_glues_by_key_or_by_commit() {
        // Japanese committed whole, Thai typed key by key with its marks,
        // Chinese committed word by word and closed by an ideographic space.
        let mut rig = Rig::new();
        rig.ime("ねこおすわり");
        rig.run("␣⏎");
        assert_eq!(rig.seen[0], fire(Trick::Sit, "ねこおすわり", 1, true));
        let mut rig = Rig::new();
        rig.run("แมวนั่ง␣⏎");
        assert_eq!(rig.seen[0], fire(Trick::Sit, "แมวนั่ง", 1, true));
        assert_eq!(rig.commands, [["นั่ง"]]);
        let mut rig = Rig::new();
        rig.ime("猫咪");
        rig.ime("坐下");
        rig.key('\u{3000}');
        rig.run("⏎");
        assert_eq!(rig.seen[0], fire(Trick::Sit, "猫咪坐下", 1, true));
    }

    #[test]
    fn a_glued_run_waits_for_the_name_or_enter_and_prose_drops_it_unheard() {
        // A question about bathing a cat whose first clause reads as a call,
        // and a list of pets on a line that named one: prose, never a flash.
        for line in ["猫咪坐下，需要注意什么", "猫咪、狗狗等，都需要定期驱虫"]
        {
            let mut rig = Rig::new();
            rig.ime(line);
            rig.run("⏎");
            assert!(rig.seen.is_empty(), "{line}: {:?}", rig.seen);
        }
        let mut rig = Rig::new();
        rig.ime("猫咪坐下");
        rig.run("⏎");
        assert_eq!(
            rig.seen,
            [fire(Trick::Sit, "猫咪坐下", 0, true), Seen::PetOnly]
        );
        // A name after it vouches for it, at once and final.
        let mut rig = Rig::new();
        rig.ime("猫咪坐下");
        rig.run("␣kitty␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "猫咪坐下", 6, true)]);
        // A plain fire after it is the newer request, and a name confirming
        // an earlier tentative fire takes the guess with it.
        let mut rig = Rig::new();
        rig.ime("猫咪坐下");
        rig.run("␣jump␣⏎");
        assert_eq!(rig.fires(), [Trick::Jump]);
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.wait(PHRASE_GAP);
        rig.ime("猫咪坐下");
        rig.run("␣kitty␣⏎");
        assert_eq!(rig.fires(), [Trick::Sit]);
    }

    #[test]
    fn the_name_inside_a_run_names_nothing_and_a_named_line_still_waits() {
        // A held address word is not fired by the glued name; Enter sends the
        // run's command.
        let mut rig = Rig::new();
        rig.run("good␣");
        rig.ime("猫咪坐下");
        rig.run("␣⏎");
        assert_eq!(rig.fires(), [Trick::Sit]);
        // The phrase law still reads the command.
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.ime("坐下猫咪");
        rig.run("␣⏎");
        assert_eq!(rig.fires(), [Trick::Sit]);
        assert_eq!(rig.seen[0], fire(Trick::Sit, "sit", 0, false));
        // A name earlier on the line vouches for nothing after it.
        let mut rig = Rig::new();
        rig.run("kitty␣");
        rig.ime("坐下猫咪");
        rig.run("␣tight␣⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
    }

    #[test]
    fn a_script_that_writes_its_spaces_never_splits() {
        // Latin, Hangul (`고양이간식` is cat treats) and mixed runs: each is one
        // unknown word, though spaced every pair here is a call.
        for line in [
            "kittysit␣",
            "sitkitty␣",
            "고양이앉아␣",
            "앉아고양이␣",
            "猫咪sit␣",
            "kitty坐下␣",
        ] {
            let mut rig = Rig::new();
            rig.run(line);
            assert!(rig.seen.is_empty(), "{line:?}: {:?}", rig.seen);
            assert!(!rig.listener.is_pure(), "{line:?}");
        }
    }

    #[test]
    fn a_one_character_name_is_no_half_of_a_run() {
        // `猫爪` is a claw and `猫玩具` a cat toy: a lone ideograph glued to a
        // word heads a compound. Spaced, the same two words are a call.
        for run in ["猫坐下", "坐下猫", "猫跳"] {
            let mut rig = Rig::new();
            rig.ime(run);
            rig.run("␣⏎");
            assert!(rig.seen.is_empty(), "{run}: {:?}", rig.seen);
        }
        let mut rig = Rig::new();
        rig.ime("猫");
        rig.run("␣");
        rig.ime("坐下");
        rig.run("␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "坐下", 0, true)]);
    }

    #[test]
    fn an_unglued_surface_is_no_half_of_a_run() {
        // `猫咪玩具` is a cat toy and `宝宝坐下` a sentence about a baby.
        for run in ["猫咪玩具", "玩具猫咪", "宝宝坐下", "坐下宝宝"] {
            let mut rig = Rig::new();
            rig.ime(run);
            rig.run("␣⏎");
            assert!(rig.seen.is_empty(), "{run}: {:?}", rig.seen);
        }
        // The verb of the same row still glues.
        let mut rig = Rig::new();
        rig.ime("猫咪玩耍");
        rig.run("␣⏎");
        assert_eq!(rig.fires(), [Trick::Play]);
    }

    #[test]
    fn a_run_that_is_not_exactly_a_name_and_a_command_is_prose() {
        for run in [
            "坐下跳",
            "猫咪很可爱",
            "猫咪坐下来",
            "很猫咪坐下",
            "猫咪猫咪",
            "请坐下",
            "请猫咪坐下",
        ] {
            let mut rig = Rig::new();
            rig.ime(run);
            rig.run("␣");
            rig.ime("坐下");
            rig.run("␣⏎");
            assert!(rig.seen.is_empty(), "{run}: {:?}", rig.seen);
        }
    }

    #[test]
    fn cuts_that_agree_are_one_request_and_cuts_that_disagree_fire_nothing() {
        // `わるいこ|ねこ` and `わるい|こねこ` are both "bad kitty"; `ねこ|ねんね`
        // (sleep) and `ねこね|んね` (jump) are two requests.
        let lexicon = TrickLexicon::from_source(
            r#"
[[vocative]]
lang  = "ja"
words = ["ねこ", "こねこ", "ねこね"]
cjk   = true

[[trick]]
id      = "scold"
lang    = "ja"
words   = ["わるいこ"]
address = ["わるい"]
cjk     = true

[[trick]]
id    = "sleep"
lang  = "ja"
words = ["ねんね"]
cjk   = true

[[trick]]
id    = "jump"
lang  = "ja"
words = ["んね"]
cjk   = true
"#,
            &["all"],
        )
        .expect("the fixture parses");
        let mut rig = Rig::with(lexicon.clone());
        rig.ime("わるいこねこ");
        rig.run("␣⏎");
        assert_eq!(rig.fires(), [Trick::Scold]);
        // Each cut's command, so an ignore list holding either holds it back.
        let mut commands = rig.commands.concat();
        commands.sort();
        assert_eq!(commands, ["わるい", "わるいこ"]);
        let mut rig = Rig::with(lexicon);
        rig.ime("ねこねんね");
        rig.run("␣⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
    }

    #[test]
    fn a_plain_fire_carries_its_word_as_its_command() {
        // Held, parked, capitalised, drawn out: exactly as typed.
        for (script, command) in [
            ("SIT!␣", "SIT"),
            ("good␣kitty␣", "good"),
            ("goooood␣kitty.⏎", "goooood"),
        ] {
            let mut rig = Rig::new();
            rig.run(script);
            assert_eq!(rig.commands, [[command]], "{script:?}: {:?}", rig.seen);
        }
    }

    #[test]
    fn the_shipped_vocabulary_hears_glued_calls_and_not_compound_nouns() {
        for run in ["猫咪坐下", "坐下猫咪", "ねこおすわり", "น้องแมวนั่ง"]
        {
            let mut rig = Rig::shipped_english();
            rig.ime(run);
            rig.run("␣⏎");
            assert_eq!(rig.seen[0], fire(Trick::Sit, run, 1, true), "{run}");
        }
        // Things, everyday-word names, and command words that glued spell
        // another word (`喂猫咪` feed the kitty, `แมวมอง` a talent scout,
        // `猫咪等` cats and the like, `ネコ好き` a cat lover).
        for run in [
            "猫咪玩具",
            "ねこじゃらし",
            "ขนมแมว",
            "宝宝睡觉",
            "หนูไป",
            "ลูกบอล",
            "喂猫咪",
            "แมวมอง",
            "猫咪等",
            "ネコ好き",
            "ねこ大好き",
        ] {
            let mut rig = Rig::shipped_english();
            rig.ime(run);
            rig.run("␣⏎");
            assert!(rig.seen.is_empty(), "{run}: {:?}", rig.seen);
        }
        // Spaced, `喂 猫咪` is hey kitty.
        let mut rig = Rig::shipped_english();
        rig.ime("喂");
        rig.run("␣");
        rig.ime("猫咪");
        rig.run("␣");
        assert_eq!(rig.fires(), [Trick::Look]);
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
        // Whether `night` is plain or an address word, `good night kitty` is
        // one final sleep, never the praise.
        let mut rig = Rig::shipped_english();
        rig.run("good␣night␣kitty␣");
        assert_eq!(rig.fires(), [Trick::Sleep], "{:?}", rig.seen);
        assert!(rig.something_stuck());
        assert_eq!(rig.listener.tentative(), None, "final");
    }

    // ---- ordinary lines ----

    /// Ordinary prose, shell and REPL lines, each typed character by character
    /// and submitted.
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
        // Unspaced prose and code: a clause that opens like a call, a compound
        // noun, a baby, a glued run in a command or a filename.
        "猫咪坐下，需要注意什么",
        "猫咪、狗狗等，都需要定期驱虫",
        "猫咪、狗狗洗澡，需要注意什么",
        "猫咪玩具 推荐一下",
        "宝宝坐下了",
        "npm run 猫咪坐下",
        "猫咪坐下.txt",
        "./坐下猫咪",
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

    /// The same lines against the shipped English. Which survive depends on the
    /// word lists; what is pinned is the listener's half: a fire becomes final
    /// only while every word typed so far is shipped pet vocabulary.
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
            // Characters, commits and Enter can only remove purity, and what
            // restores it never fires, so any fire or confirm began on a pure,
            // unpoisoned line.
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

    /// A test-sized line editor that knows only text, so it can check what the
    /// listener believes about the line. Backspace takes one cluster (readline,
    /// zsh); Ctrl+W is readline's: back over whitespace, then the word.
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

        /// The line's words as a reader splits them: at whitespace and fullwidth
        /// stops, each allowed trailing sentence punctuation. Deliberately wider
        /// than the listener: it judges what may never happen.
        fn roles(&self, lexicon: &TrickLexicon) -> Vec<Option<TrickRole>> {
            let text: String = self.line.iter().collect();
            let mut scratch = String::new();
            text.split(|c: char| matches!(kind_of(c), Kind::Space | Kind::WideStop))
                .map(|piece| piece.trim_end_matches(|c: char| kind_of(c) == Kind::Stop))
                .filter(|word| !word.is_empty())
                .map(|word| read(lexicon, word, &mut scratch))
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

        /// The same for a word heard inside a commit: the caret stood before the
        /// whole commit, so the word may lie on either side of it.
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

    /// One word as a reader takes it. A run that is no word reads as its
    /// command when ANY cut makes it a name of two or more characters and a
    /// trick word, neither `unglued`, all in a script written without spaces —
    /// not as a name, since it names nothing. The guards are stated again here,
    /// not borrowed from the listener, so weakening one there is caught here.
    fn read(lexicon: &TrickLexicon, word: &str, scratch: &mut String) -> Option<TrickRole> {
        if let Some(role) = lexicon.classify(word, scratch) {
            return Some(role);
        }
        let chars: Vec<char> = word.chars().collect();
        let unspaced = chars.iter().all(|c| {
            matches!(*c as u32,
                0x3040..=0x30FF | 0x31F0..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
                | 0xF900..=0xFAFF | 0x0E00..=0x0EFF | 0x1780..=0x17FF | 0x2_0000..=0x2_A6DF)
        });
        let trick = (1..chars.len()).find_map(|at| {
            let head: String = chars[..at].iter().collect();
            let tail: String = chars[at..].iter().collect();
            let halves = (
                lexicon.classify_glued_half(&head, scratch)?,
                lexicon.classify_glued_half(&tail, scratch)?,
            );
            match halves {
                (TrickRole::Vocative, TrickRole::Trick { trick, .. }) if at >= 2 => Some(trick),
                (TrickRole::Trick { trick, .. }, TrickRole::Vocative) if chars.len() - at >= 2 => {
                    Some(trick)
                }
                _ => None,
            }
        });
        trick.filter(|_| unspaced).map(|trick| TrickRole::Trick {
            trick,
            needs_vocative: false,
        })
    }

    /// The events of one closing press (a character, or Enter when
    /// `closer = None`), judged against the line as it stood.
    fn judge(rig: &Rig, before: usize, editor: &Editor, closer: Option<char>, judging: bool) {
        let heard = &rig.seen[before..];
        if heard.iter().all(|seen| *seen == Seen::Revoke) {
            // Taking a fire back needs no licence.
            return;
        }
        let line = &editor.line;
        assert!(judging, "{heard:?} on a dead line {line:?}");
        // A prefix never fires: only a boundary speaks.
        assert!(
            closer.is_none_or(|c| matches!(kind_of(c), Kind::Space | Kind::WideStop)),
            "{heard:?} at {closer:?} on {line:?}"
        );
        // Nothing but pet talk stood on the line.
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
                    // A glued run never speaks at its own boundary: a later
                    // name or Enter sends it.
                    let glued = rig.lexicon.classify(word, &mut String::new()).is_none();
                    assert!(
                        !glued || submitted || *back_chars > 0,
                        "{heard:?} on {line:?}"
                    );
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

    /// The event of an IME commit, judged against the text before it and the
    /// commit: whatever it fired was licensed at a boundary inside it, with
    /// only pet talk to its left.
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
        // Zero proves the line empty, whatever else went wrong.
        if listener.line_chars() == 0 {
            assert!(line.is_empty(), "count 0 on {line:?}");
        }
        if !listener.is_pure() {
            return;
        }
        // A pure line is known: empty exactly when the count says so, and the
        // open token is the word the text ends with (plus, on a full window,
        // the uncounted marks on its last character).
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
            "猫咪坐下",
            "坐下猫咪",
            // Splits the listener's guards forbid: a spaced script, Hangul, a
            // one-character name, a thing, an everyday name.
            "kittysit",
            "고양이앉아",
            "猫坐下",
            "猫咪玩具",
            "宝宝坐下",
        ];
        let closers = [' ', ' ', ' ', '.', '!', ',', '\u{3000}', '、', '！', ';'];
        let (mut fires, mut fires_after_an_edit, mut emptied) = (0usize, 0usize, 0usize);
        let (mut commit_fires, mut glued_fires) = (0usize, 0usize);
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
                        // Tab or arrow: the editor's text is unknown and the
                        // poisoned listener claims nothing until a reset.
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
            glued_fires += rig
                .seen
                .iter()
                .filter(|seen| matches!(seen, Seen::Fire { word, .. } if word.contains("猫咪")))
                .count();
        }
        // The walk reached fires, fires on backspaced or word-killed lines,
        // fires inside a commit, glued fires, and lines emptied by hand.
        assert!(
            fires >= 200
                && fires_after_an_edit >= 20
                && commit_fires >= 20
                && glued_fires >= 20
                && emptied >= 20,
            "{fires} fires, {fires_after_an_edit} after an edit, {commit_fires} in a commit, \
             {glued_fires} glued, {emptied} lines emptied"
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
        // The glued reading: a hit either way round, and a miss that walks
        // every cut of a full window.
        rig.ime("猫咪坐下！坐下猫咪！");
        rig.ime(&"猫".repeat(TOKEN_CHARS));
        rig.run("␣⏎");
        rig.run(&"ß".repeat(TOKEN_CHARS)).run("␣⏎");
        assert_eq!(
            rig.listener.heap_capacity(),
            warm,
            "steady state allocates nothing"
        );
    }
}
