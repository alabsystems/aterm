// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kitty-command listener: the typed-line state machine that decides when a word just typed (`sit`, `kitty jump`, `good kitty`) is a command to the pet.

use aterm_lexicon::{
    Trick, TrickLexicon, TrickRole, is_interior_joiner, is_no_space_script, is_token_char,
};
use aterm_time::{Duration, Instant};

/// A trick word within this long of the previous fire on the same line belongs to that fire's phrase (`sit down`) and is absorbed; later it is a new request.
pub const PHRASE_GAP: Duration = Duration::from_millis(1500);

const TOKEN_CHARS: usize = TrickLexicon::MAX_SURFACE_CHARS;
const WORD_BYTES: usize = TOKEN_CHARS * 4;

const _: () = assert!(WORD_BYTES <= u8::MAX as usize);

const GAP_WINDOW: usize = 8;
const SESSION_MEMORY: usize = 8;

/// What the listener heard; at most one per fed press.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrickEvent<'a> {
    /// A trick word fired.
    Fire {
        /// The trick asked for.
        trick: Trick,
        /// The trick token as typed.
        word: &'a str,
        /// The command inside `word`, for a host's ignore list: `word` itself, or the command half of a glued run (`坐下` of `猫咪坐下`).
        commands: Commands<'a>,
        /// Typed characters between `word` and the caret as it stood when this press arrived (before its echo).
        back_chars: u16,
        /// `true`: final (the line names the pet, or Enter submitted a pure line).
        addressed: bool,
    },
    /// The live tentative fire is final: Enter on a still-pure line, or the line named the pet.
    Confirm {
        /// The trick of the fire being confirmed.
        trick: Trick,
    },
    /// The live tentative fire is withdrawn: the line turned impure, was aborted, was poisoned, or its word was backspaced into.
    Revoke,
}

/// The command surfaces inside a fired word, borrowed from it: the word itself, or for a glued run the half beside the name at each cut that read it (`わるいこねこ`: `わるいこ` and `わるい`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Commands<'a> {
    word: &'a str,
    cuts: Cuts,
}

impl<'a> Commands<'a> {
    /// Each command surface as typed: at least one, never empty, never the name.
    #[inline]
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
    const WHOLE: Cuts = Cuts {
        at: 1,
        name_first: true,
    };
}

impl Default for Cuts {
    #[inline]
    fn default() -> Self {
        Self::WHOLE
    }
}

const _: () = assert!(WORD_BYTES <= u128::BITS as usize);

/// The answer to one fed press.
#[must_use = "an unread feed is a lost fire, confirm or revoke"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TrickFeed<'a> {
    /// What the press meant to the pet, if anything.
    pub event: Option<TrickEvent<'a>>,
    /// Enter submitted a line of nothing but pet vocabulary (at least one word, pure, unpoisoned).
    pub submit_pet_only: bool,
}

impl Default for TrickFeed<'_> {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
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

impl Default for Word {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Word {
    const EMPTY: Word = Word {
        bytes: [0; WORD_BYTES],
        len: 0,
    };

    /// Only whole characters go in and out, so the prefix is valid UTF-8; the
    /// checked conversion runs at boundaries only.
    #[inline]
    const fn as_str(&self) -> &str {
        let len = self.len as usize;
        if len > self.bytes.len() {
            return "";
        }
        let (bytes, _) = self.bytes.split_at(len);
        match std::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => "",
        }
    }

    /// `false` (and nothing written) when the character does not fit — which
    /// the caller's character count already rules out.
    #[inline]
    const fn push(&mut self, c: char) -> bool {
        let start = self.len as usize;
        let end = start.saturating_add(c.len_utf8());
        if end > self.bytes.len() || end > u8::MAX as usize {
            return false;
        }
        let (_, rest) = self.bytes.split_at_mut(start);
        c.encode_utf8(rest);
        self.len = end as u8;
        true
    }

    #[inline]
    fn pop(&mut self) -> Option<char> {
        let c = self.as_str().chars().next_back()?;
        let width = u8::try_from(c.len_utf8()).unwrap_or(u8::MAX);
        self.len = self.len.saturating_sub(width);
        Some(c)
    }

    #[inline]
    const fn clear(&mut self) {
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
    /// `. ! ? , …`: closes a token and parks its fire until the next whitespace, fullwidth stop
    /// or Enter.
    Stop,
    /// `。！？、，` — a full boundary.
    WideStop,
    /// Anything else: the line is code.
    Code,
    /// A control character or a zero-width scalar outside a word (ZWJ, a
    /// variation selector, a newline inside a commit).
    Alien,
}

/// Uses the lexicon's own predicates, so a typed line segments exactly as the
/// vocabulary was segmented at load.
#[inline]
fn kind_of(c: char) -> Kind {
    if c.is_ascii() {
        return match c {
            ' ' => Kind::Space,
            '0'..='9' | 'A'..='Z' | 'a'..='z' | '_' => Kind::Word,
            '\'' | '-' => Kind::Joiner,
            '.' | '!' | '?' | ',' => Kind::Stop,
            c if c.is_ascii_control() => Kind::Alien,
            _ => Kind::Code,
        };
    }
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
        '\u{2026}' => Kind::Stop,
        '\u{3002}' | '\u{FF01}' | '\u{FF1F}' | '\u{3001}' | '\u{FF0C}' => Kind::WideStop,
        _ => Kind::Code,
    }
}

/// A character of a script written without spaces between words: the
/// vocabulary's no-space set minus Hangul, which is in that set only to be
/// looked up raw (Korean writes its spaces).
#[inline]
const fn is_unspaced_script(c: char) -> bool {
    is_no_space_script(c) && !matches!(c as u32, 0x1100..=0x11FF | 0xAC00..=0xD7AF)
}

const GLUED_NAME_CHARS: usize = 2;

fn glued(lexicon: &TrickLexicon, token: &str, scratch: &mut String) -> Option<(Trick, Cuts)> {
    if token.as_bytes().first().is_none_or(u8::is_ascii) {
        return None;
    }
    let mut chars = 0usize;
    for c in token.chars() {
        if !is_unspaced_script(c) {
            return None;
        }
        chars += 1;
    }
    if chars <= GLUED_NAME_CHARS {
        return None;
    }
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
        let ((Some(TrickRole::Vocative), Some(TrickRole::Trick { trick, .. }))
        | (Some(TrickRole::Trick { trick, .. }), Some(TrickRole::Vocative))) =
            (head_role, lexicon.classify_glued_half(tail, scratch))
        else {
            continue;
        };
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
            Some((first, cuts)) if *first == trick && cuts.name_first == name_first => {
                cuts.at |= cut;
            }
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
/// token and Enter) into the one event a press reports.
#[inline]
const fn fold(acc: Option<Outcome>, next: Option<Outcome>) -> Option<Outcome> {
    match (acc, next) {
        (acc, None) => acc,
        (
            Some(Outcome::Fire {
                trick, back_chars, ..
            }),
            Some(Outcome::Confirm { trick: confirmed }),
        ) if (trick as u8) == (confirmed as u8) => Some(Outcome::Fire {
            trick,
            back_chars,
            addressed: true,
        }),
        (_, next) => next,
    }
}

/// [`TrickEvent::Fire::back_chars`]. `since_end`: characters typed after the
/// word and before this press's closer.
#[inline]
const fn back_chars(since_end: u32, fed: u32, word_chars: u32) -> u16 {
    let distance = if fed <= since_end {
        since_end.saturating_sub(fed)
    } else {
        fed.saturating_sub(since_end).saturating_sub(word_chars)
    };
    if distance > u16::MAX as u32 {
        u16::MAX
    } else {
        distance as u16
    }
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
    /// back (marks past the token window are uncounted; see `step`).
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
    /// A glued run's fire, waiting for a name or Enter.
    guess: Option<Verdict>,
    /// The live tentative fire.
    tentative: Option<Trick>,
    /// When the last fire on this line was delivered (the phrase law).
    last_fire: Option<Instant>,
    /// Which trick that fire asked for (the attention law).
    last_trick: Option<Trick>,
}

impl Default for Line {
    #[inline]
    fn default() -> Self {
        Self::FRESH
    }
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
    #[inline]
    const fn token_is_live(&self) -> bool {
        self.token_chars > 0 && self.token_chars as usize <= TOKEN_CHARS
    }
}

/// The per-window kitty-command listener, keyed to the window's front session ([`rekey`](Self::rekey)).
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
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl TrickListener {
    /// A listener on a fresh, pure line, bound to no session yet.
    #[inline]
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

    /// Whether [`note_char`](Self::note_char) with `c` would consult the vocabulary: `c` closes a lookup-able open token on a pure, unpoisoned line.
    #[inline]
    #[must_use]
    pub fn needs_lexicon(&self, c: char) -> bool {
        self.judging()
            && self.line.token_is_live()
            && matches!(kind_of(c), Kind::Space | Kind::Stop | Kind::WideStop)
    }

    /// [`needs_lexicon`](Self::needs_lexicon) for an IME commit.
    #[inline]
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
    #[inline]
    #[must_use]
    pub const fn submit_needs_lexicon(&self) -> bool {
        self.judging() && self.line.token_is_live()
    }

    /// The line can still fire: pure and unpoisoned.
    #[inline]
    const fn judging(&self) -> bool {
        self.line.pure && !self.line.poisoned
    }

    /// Every token so far was pet vocabulary and no code has appeared.
    #[inline]
    #[must_use]
    pub const fn is_pure(&self) -> bool {
        self.line.pure
    }

    /// The line holds something the listener did not see; nothing fires until a reset.
    #[inline]
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.line.poisoned
    }

    /// The live tentative fire, if any.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn tentative(&self) -> Option<Trick> {
        self.line.tentative
    }

    /// Characters typed on this line while unpoisoned.
    #[inline]
    #[must_use]
    pub const fn line_chars(&self) -> u32 {
        self.line.line_chars
    }

    /// Bind the listener to `session`, the window's front session; the same session again is one compare.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "RekeyClean",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
    #[inline]
    const fn forget(&mut self, session: u64) -> bool {
        let mut at = 0;
        while at < SESSION_MEMORY {
            if matches!(self.dirty[at], Some(s) if s == session) {
                let mut i = at;
                while i + 1 < SESSION_MEMORY {
                    self.dirty[i] = self.dirty[i + 1];
                    i += 1;
                }
                self.dirty[SESSION_MEMORY - 1] = None;
                return true;
            }
            at += 1;
        }
        false
    }

    /// One typed character (the shifted glyph; Space arrives here too).
    #[inline]
    pub fn note_char(
        &mut self,
        now: Instant,
        c: char,
        lexicon: Option<&TrickLexicon>,
    ) -> TrickFeed<'_> {
        let out = self.step(now, c, 0, lexicon);
        self.feed(out, false)
    }

    /// One IME commit.
    #[inline]
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

    /// Enter: close the open token (or deliver a parked fire), confirm a live tentative fire on a still-pure line, report [`TrickFeed::submit_pet_only`], and start a fresh line.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "SubmitQuiet",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "SubmitFinal",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "SubmitConfirm",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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

    /// The line was abandoned without being judged: Ctrl+C, Ctrl+J, Ctrl+M, Ctrl+U, controller bytes ending in CR / LF (the line that ran may be `sitxyz`), Enter at a no-echo prompt.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "Reset",
            project = "conformance_trick_listener::project"
        )
    )]
    #[inline]
    pub fn note_line_reset(&mut self) -> TrickFeed<'_> {
        let out = self.fresh_line();
        self.feed(out, false)
    }

    /// Ctrl+Z or Ctrl+\: a shell at its prompt keeps the line through them, while under a program the tty flushes it, so only a provably empty line (unpoisoned, nothing counted) is reset; any other is a break.
    #[inline]
    pub fn note_job_chord(&mut self) -> TrickFeed<'_> {
        if self.line.line_chars == 0 && !self.line.poisoned {
            self.note_line_reset()
        } else {
            self.note_break()
        }
    }

    /// A backward word-kill (Ctrl+W, Alt+Backspace, Ctrl+Backspace).
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

    /// Tab, Esc, arrows, Home / End / PgUp / PgDn, any other chord, raw controller bytes, a paste: poisoned until a reset.
    #[inline]
    pub fn note_break(&mut self) -> TrickFeed<'_> {
        let out = self.poison();
        self.feed(out, false)
    }

    /// A press at a prompt whose tty swallows input (a password): poisons the line, so a passphrase cannot move the pet.
    #[inline]
    pub fn note_no_echo(&mut self) -> TrickFeed<'_> {
        let out = self.poison();
        self.feed(out, false)
    }

    /// A plain Backspace: takes a character off the line's count, and one that crosses back over
    /// the closer of the last token restores the line from before it closed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EraseSome",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EraseToken",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EraseBlind",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EraseLast",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "Restore",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
            // The run's first character goes: the caret is back at the end of
            // the token that run closed, and the line is exactly the snapshot.
            if let Some(before) = self.snapshot.take() {
                let fired_here = self.line.last_fire != before.last_fire;
                if fired_here && self.line.tentative.is_some() {
                    out = Some(Outcome::Revoke);
                }
                self.line = before;
            }
        } else {
            let run = self.line.gap_chars as usize;
            if (2..=GAP_WINDOW).contains(&run) {
                self.line.left = self.line.gap.get(run.saturating_sub(2)).copied();
            } else {
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

    /// One character of the line.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
            && !c.is_ascii()
            && aterm_grapheme::char_width(c) == 0
        {
            return None;
        }
        let joins = kind == Kind::Joiner && self.line.token_chars > 0;
        let out = match kind {
            Kind::Word => self.push_token_char(c),
            Kind::Joiner if joins => self.push_token_char(c),
            Kind::Space | Kind::WideStop => self.edge(now, Edge::Open, fed, lexicon),
            Kind::Stop => self.edge(now, Edge::Stop, fed, lexicon),
            Kind::Joiner | Kind::Code => {
                self.end_token();
                self.go_impure()
            }
            Kind::Alien => self.poison(),
        };
        if self.line.poisoned {
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
        self.line.line_chars = self.line.line_chars.saturating_add(1);
        out
    }

    /// A word character (or an interior joiner) goes onto the open token.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "Letter",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "LetterGlued",
            project = "conformance_trick_listener::project"
        )
    )]
    fn push_token_char(&mut self, c: char) -> Option<Outcome> {
        let mut out = None;
        if self.line.token_chars == 0 {
            if !c.is_ascii() && aterm_grapheme::char_width(c) == 0 {
                return self.poison();
            }
            let glued = self.line.tail.is_some()
                || self.line.left.is_some_and(|l| kind_of(l) == Kind::Stop);
            if glued {
                out = self.go_impure();
            }
        }
        if (self.line.token_chars as usize) < TOKEN_CHARS {
            self.line.token.push(c);
        }
        self.line.token_chars = self.line.token_chars.saturating_add(1);
        out
    }

    /// Whitespace, a stop or a fullwidth stop arrived.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
                    None
                } else {
                    self.go_impure()
                }
            }
        }
    }

    /// Snapshot the line, then take the open token off it. The snapshot is the
    /// one level of undo Backspace has.
    #[inline]
    const fn end_token(&mut self) -> (Word, u32) {
        let token = (self.line.token, self.line.token_chars);
        if self.line.token_chars > 0 {
            self.snapshot = Some(self.line);
            self.line.token.clear();
            self.line.token_chars = 0;
            self.line.gap_chars = 0;
        }
        token
    }

    /// The boundary: the only place the vocabulary is consulted.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EdgeQuiet",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EdgeName",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "MarkName",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EdgeGlued",
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
            return None;
        }
        let role = match lexicon {
            Some(lexicon) if word_chars as usize <= TOKEN_CHARS => {
                match lexicon.classify(word.as_str(), &mut self.scratch) {
                    Some(role) => Some((role, Cuts::WHOLE)),
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
                let attention_only =
                    self.line.last_trick == Some(Trick::Look) && trick != Trick::Look;
                if in_phrase && !attention_only {
                    None
                } else if cuts != Cuts::WHOLE {
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
                self.line.tail = Some(verdict);
                None
            }
        }
    }

    /// An open edge with no open token: whatever a stop parked is due.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EdgeConfirm",
            project = "conformance_trick_listener::project"
        )
    )]
    #[inline]
    fn deliver_tail(&mut self, now: Instant, fed: u32) -> Option<Outcome> {
        let verdict = self.line.tail.take()?;
        self.deliver(now, verdict, fed)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EdgeFire",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "MarkImpure",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EdgeUnknown",
            project = "conformance_trick_listener::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "EdgeGluedUnknown",
            project = "conformance_trick_listener::project"
        )
    )]
    #[inline]
    fn go_impure(&mut self) -> Option<Outcome> {
        self.line.pure = false;
        self.line.held = None;
        self.line.tail = None;
        self.line.guess = None;
        self.line.tentative.take().map(|_| Outcome::Revoke)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "TrickListenerPurity",
            action = "Lose",
            project = "conformance_trick_listener::project"
        )
    )]
    #[inline]
    fn poison(&mut self) -> Option<Outcome> {
        self.line.poisoned = true;
        self.line.held = None;
        self.line.tail = None;
        self.line.guess = None;
        self.snapshot = None;
        self.line.tentative.take().map(|_| Outcome::Revoke)
    }

    #[inline]
    const fn fresh_line(&mut self) -> Option<Outcome> {
        let revoked = self.line.tentative.is_some();
        self.line = Line::FRESH;
        self.snapshot = None;
        if revoked { Some(Outcome::Revoke) } else { None }
    }

    /// Take one cluster off the open token, as an editor's Backspace does:
    /// trailing zero-width scalars go with their base.
    #[inline]
    fn pop_cluster(&mut self) -> u32 {
        if self.line.token_chars as usize > TOKEN_CHARS {
            self.line.token_chars = self.line.token_chars.saturating_sub(1);
            return 1;
        }
        let mut popped = 0u32;
        while let Some(c) = self.line.token.pop() {
            popped = popped.saturating_add(1);
            self.line.token_chars = self.line.token_chars.saturating_sub(1);
            if c.is_ascii() || aterm_grapheme::char_width(c) != 0 {
                break;
            }
        }
        popped
    }

    #[inline]
    const fn feed(&self, out: Option<Outcome>, submit_pet_only: bool) -> TrickFeed<'_> {
        let event = match out {
            Some(Outcome::Fire {
                trick,
                back_chars,
                addressed,
            }) => {
                let word = self.word.as_str();
                Some(TrickEvent::Fire {
                    trick,
                    word,
                    commands: Commands {
                        word,
                        cuts: self.cuts,
                    },
                    back_chars,
                    addressed,
                })
            }
            Some(Outcome::Confirm { trick }) => Some(TrickEvent::Confirm { trick }),
            Some(Outcome::Revoke) => Some(TrickEvent::Revoke),
            None => None,
        };
        TrickFeed {
            event,
            submit_pet_only,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        /// `submit_pet_only`, recorded after the same press's event.
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
            let mut rig = Rig::new();
            rig.run("sit␣");
            rig.key(mark);
            if mark == '_' {
                rig.run("␣");
            }
            assert_eq!(rig.seen.last(), Some(&Seen::Revoke), "{mark:?}");
            let mut rig = Rig::new();
            rig.run("sit");
            rig.key(mark);
            rig.run("␣⏎");
            assert!(rig.seen.is_empty(), "{mark:?}: {:?}", rig.seen);
        }
    }

    #[test]
    fn the_lexicons_code_context_set_is_code_here_too() {
        let mut checked = 0;
        for c in (0..=0x2FFFu32).filter_map(char::from_u32) {
            if aterm_lexicon::is_code_adjacent_punct(c) {
                assert_eq!(kind_of(c), Kind::Code, "{c:?}");
                checked += 1;
            }
        }
        assert!(checked >= 16, "the oracle went vacuous: {checked}");
        assert_eq!(kind_of('-'), Kind::Joiner);
        assert_eq!(kind_of('.'), Kind::Stop);
        assert_eq!(kind_of(':'), Kind::Code);
        assert_eq!(kind_of(';'), Kind::Code);
    }

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
        let mut rig = Rig::new();
        rig.run("cat.sit␣⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
        let mut rig = Rig::new();
        rig.run("kitty,sit␣⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
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
        let mut rig = Rig::new();
        rig.run("play␣cat.");
        assert_eq!(rig.seen, [fire(Trick::Play, "play", 0, false)]);
        rig.run("mp3⏎");
        assert_eq!(
            rig.seen,
            [fire(Trick::Play, "play", 0, false), Seen::Revoke],
            "never confirmed"
        );
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
        rig.run("!stay␣");
        assert_eq!(rig.seen.last(), Some(&Seen::Revoke));
    }

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
                fire(Trick::Sleep, "night", 0, false),
                Seen::Confirm(Trick::Sleep),
            ]
        );
    }

    #[test]
    fn a_greeting_or_a_call_never_wins_the_phrase() {
        let mut rig = Rig::new();
        rig.run("hey␣kitty␣sit␣");
        assert_eq!(
            rig.seen,
            [
                fire(Trick::Look, "hey", 6, true),
                fire(Trick::Sit, "sit", 0, true)
            ]
        );
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
    fn a_name_confirms_the_tentative_fire_and_drops_a_later_held_word() {
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.wait(Duration::from_secs(2));
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
    fn a_new_line_is_a_new_phrase() {
        let mut rig = Rig::new();
        rig.run("sit⏎jump⏎");
        assert_eq!(rig.fires(), [Trick::Sit, Trick::Jump]);
    }

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
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.word_kill();
        assert!(rig.listener.is_poisoned());
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false), Seen::Revoke]);
        let mut rig = Rig::new();
        rig.run("night-night");
        rig.word_kill();
        assert!(rig.listener.is_poisoned());
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
        rig.run("e␣");
        assert_eq!(rig.seen.len(), 2);
        assert!(!rig.listener.is_pure());
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
        rig.run("sot");
        rig.word_kill();
        rig.word_kill();
        assert!(!rig.listener.is_poisoned());
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

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
        rig.line_reset();
        assert_eq!(rig.seen.last(), Some(&Seen::Revoke));
        rig.brk();
        rig.line_reset();
        rig.run("kitty␣jump␣");
        assert_eq!(rig.seen.last(), Some(&fire(Trick::Jump, "jump", 0, true)));
    }

    #[test]
    fn a_character_with_no_column_of_its_own_poisons() {
        for alien in ['\u{200d}', '\u{fe0f}', '\u{0}', '\n', '\u{7f}'] {
            let mut rig = Rig::new();
            rig.run("sit");
            rig.key(alien);
            assert!(rig.listener.is_poisoned(), "{alien:?}");
            rig.run("⌫␣⏎");
            assert!(rig.seen.is_empty(), "{alien:?}: {:?}", rig.seen);
        }
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.key('\u{301}');
        assert!(rig.listener.is_poisoned());
        assert_eq!(rig.seen.last(), Some(&Seen::Revoke));
        let mut rig = Rig::new();
        rig.run("site\u{301}⌫␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

    #[test]
    fn a_no_echo_prompt_poisons_and_says_nothing() {
        let mut rig = Rig::new();
        rig.no_echo();
        rig.run("sleep␣tight␣");
        assert!(rig.seen.is_empty(), "a passphrase must not move the cat");
        rig.line_reset();
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        rig.no_echo();
        assert_eq!(rig.seen.last(), Some(&Seen::Revoke));
    }

    #[test]
    fn raw_bytes_ending_in_a_newline_reset_without_judging() {
        let mut rig = Rig::new();
        rig.run("sit");
        rig.line_reset();
        assert!(rig.seen.is_empty(), "the open token is NOT evaluated");
        assert_eq!(rig.listener.line_chars(), 0);
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
    }

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
        let mut rig = Rig::new();
        rig.run("นั่ง");
        assert!(rig.seen.is_empty());
        rig.run("␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "นั่ง", 0, false)]);
    }

    #[test]
    fn a_lone_committed_ideograph_on_the_way_to_a_longer_word_never_fires() {
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

    #[test]
    fn a_name_glued_to_its_command_waits_for_enter_either_way_round() {
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
        let mut rig = Rig::new();
        rig.ime("猫咪坐下");
        rig.run("␣kitty␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "猫咪坐下", 6, true)]);
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
        let mut rig = Rig::new();
        rig.run("good␣");
        rig.ime("猫咪坐下");
        rig.run("␣⏎");
        assert_eq!(rig.fires(), [Trick::Sit]);
        let mut rig = Rig::new();
        rig.run("sit␣");
        rig.ime("坐下猫咪");
        rig.run("␣⏎");
        assert_eq!(rig.fires(), [Trick::Sit]);
        assert_eq!(rig.seen[0], fire(Trick::Sit, "sit", 0, false));
        let mut rig = Rig::new();
        rig.run("kitty␣");
        rig.ime("坐下猫咪");
        rig.run("␣tight␣⏎");
        assert!(rig.seen.is_empty(), "{:?}", rig.seen);
    }

    #[test]
    fn a_script_that_writes_its_spaces_never_splits() {
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
        for run in ["猫咪玩具", "玩具猫咪", "宝宝坐下", "坐下宝宝"] {
            let mut rig = Rig::new();
            rig.ime(run);
            rig.run("␣⏎");
            assert!(rig.seen.is_empty(), "{run}: {:?}", rig.seen);
        }
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
        let mut rig = Rig::shipped_english();
        rig.ime("喂");
        rig.run("␣");
        rig.ime("猫咪");
        rig.run("␣");
        assert_eq!(rig.fires(), [Trick::Look]);
    }

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
        rig.listener.rekey(2);
        rig.listener.rekey(1);
        assert!(rig.listener.is_poisoned());
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
        for session in (1..sessions).rev() {
            rig.listener.rekey(session);
            assert!(rig.listener.is_poisoned(), "session {session}");
            rig.listener.rekey(u64::MAX);
        }
        rig.listener.rekey(0);
        assert!(!rig.listener.is_poisoned());
    }

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
        for _ in 0..=long.chars().count() {
            rig.backspace();
        }
        assert_eq!(rig.listener.line_chars(), 0);
        rig.run("sit␣");
        assert_eq!(rig.seen, [fire(Trick::Sit, "sit", 0, false)]);
        let mut rig = Rig::new();
        rig.run(&long).run("⌫␣");
        assert_eq!(rig.fires(), [Trick::Look]);
    }

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
        rig.run("x␣");
        assert!(!rig.listener.is_pure());
        rig.run("sit");
        assert!(!rig.listener.needs_lexicon(' '));
        assert!(!rig.listener.submit_needs_lexicon());
        assert!(!rig.listener.ime_needs_lexicon("sit "));
        let mut rig = Rig::new();
        rig.run("sit");
        rig.brk();
        assert!(!rig.listener.needs_lexicon(' '));
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
        let mut listener = TrickListener::default();
        let empty = TrickLexicon::default();
        for c in "sit".chars() {
            let _ = listener.note_char(now, c, Some(&empty));
        }
        assert_eq!(listener.note_submit(now, Some(&empty)), TrickFeed::NONE);
    }

    #[test]
    fn back_chars_measures_from_the_caret_the_press_arrived_at() {
        for (since_end, fed, word_chars, expected) in [
            (0, 0, 3, 0),
            (6, 0, 4, 6),
            (2, 0, 3, 2),
            (0, 7, 4, 3),
            (0, 3, 3, 0),
            (0, 1, 2, 0),
            (6, 6, 4, 0),
            (u32::MAX, 0, 1, u16::MAX),
        ] {
            assert_eq!(
                back_chars(since_end, fed, word_chars),
                expected,
                "({since_end}, {fed}, {word_chars})"
            );
        }
    }

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
        let mut rig = Rig::shipped_english();
        rig.run("good␣night␣kitty␣");
        assert_eq!(rig.fires(), [Trick::Sleep], "{:?}", rig.seen);
        assert!(rig.something_stuck());
        assert_eq!(rig.listener.tentative(), None, "final");
    }

    const ORDINARY_LINES: &[&str] = &[
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
        "please hide the debug logs",
        "ok roll it back",
        "now play it",
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
        "look at the cat output",
        "run the cat command",
        "wait for cat to finish",
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
        "wait",
        "stop",
        "no",
        "good",
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

    struct Dice(u64);

    impl Dice {
        const fn roll(&mut self, sides: usize) -> usize {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let mixed = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
            let denom = if sides > 1 { sides } else { 1 };
            (mixed >> 33) as usize % denom
        }
    }

    const ALPHABET: &[char] = &[
        's', 'i', 't', 'k', 'y', 'g', 'o', 'd', 'n', 'a', 'p', 'l', 'u', 'm', 'j', 'S', 'T', ' ',
        ' ', ' ', ' ', '\u{3000}', '.', '!', '?', ',', '…', '。', '！', '、', ':', ';', '/', '-',
        '\'', '_', '=', '$', '坐', '下', '跳', '猫', '咪', '앉', '아', 'น', 'ั', '่', 'ง', '𝐚', 'ß',
        '\u{301}', '\u{200d}', '\n', '\t', '\u{0}',
    ];

    fn random_walk(rig: &mut Rig, dice: &mut Dice, steps: usize) {
        for _ in 0..steps {
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
        for line in ORDINARY_LINES {
            for mut rig in [Rig::new(), Rig::shipped_english()] {
                rig.run(line).run("⌫⌫⌫").run(line).run("⏎");
            }
        }
    }

    /// A test-sized line editor that knows only text, so it can check what the
    /// listener believes about the line.
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

        fn roles(&self, lexicon: &TrickLexicon) -> Vec<Option<TrickRole>> {
            let text: String = self.line.iter().collect();
            let mut scratch = String::new();
            text.split(|c: char| matches!(kind_of(c), Kind::Space | Kind::WideStop))
                .map(|piece| piece.trim_end_matches(|c: char| kind_of(c) == Kind::Stop))
                .filter(|word| !word.is_empty())
                .map(|word| read(lexicon, word, &mut scratch))
                .collect()
        }

        fn is_whole_word(&self, start: usize, end: usize) -> bool {
            let line = &self.line;
            let bounded_left =
                start == 0 || matches!(kind_of(line[start - 1]), Kind::Space | Kind::WideStop);
            let bounded_right = line
                .get(end)
                .is_none_or(|c| !matches!(kind_of(*c), Kind::Word | Kind::Joiner));
            bounded_left && bounded_right
        }

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

    fn judge(rig: &Rig, before: usize, editor: &Editor, closer: Option<char>, judging: bool) {
        let heard = &rig.seen[before..];
        if heard.iter().all(|seen| *seen == Seen::Revoke) {
            return;
        }
        let line = &editor.line;
        assert!(judging, "{heard:?} on a dead line {line:?}");
        assert!(
            closer.is_none_or(|c| matches!(kind_of(c), Kind::Space | Kind::WideStop)),
            "{heard:?} at {closer:?} on {line:?}"
        );
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
                    assert_eq!(*addressed, named || submitted, "{heard:?} on {line:?}");
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

    fn assert_only_takes_back(rig: &Rig, before: usize) {
        let heard = &rig.seen[before..];
        assert!(heard.iter().all(|seen| *seen == Seen::Revoke), "{heard:?}");
    }

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

    fn assert_beliefs_hold(rig: &Rig, editor: &Editor) {
        let listener = &rig.listener;
        if listener.is_poisoned() {
            return;
        }
        let line = &editor.line;
        if listener.line_chars() == 0 {
            assert!(line.is_empty(), "count 0 on {line:?}");
        }
        if !listener.is_pure() {
            return;
        }
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
                        rig.brk();
                        assert_only_takes_back(&rig, before);
                    }
                    13 => rig.wait(Duration::from_millis(dice.roll(3000) as u64)),
                    14 | 15 => {
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
}
