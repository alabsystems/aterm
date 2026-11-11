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

/// Iterator over the command surfaces inside a [`Commands`].
#[derive(Clone, Copy, Debug)]
pub struct CommandsIter<'a> {
    word: &'a str,
    at: u128,
    name_first: bool,
}

impl<'a> CommandsIter<'a> {
    /// Advances the iterator and returns the next command slice (`const`-friendly).
    #[inline]
    #[must_use]
    pub const fn next_command(&mut self) -> Option<&'a str> {
        if self.at == 0 {
            return None;
        }
        let cut = self.at.trailing_zeros() as usize;
        self.at &= self.at - 1;
        let half = match self.word.as_bytes().split_at_checked(cut) {
            Some((left, right)) => {
                let bytes = if self.name_first { right } else { left };
                match std::str::from_utf8(bytes) {
                    Ok(s) => Some(s),
                    Err(_) => None,
                }
            }
            None => None,
        };
        Some(match half {
            Some(h) => h,
            None => self.word,
        })
    }
}

impl<'a> Iterator for CommandsIter<'a> {
    type Item = &'a str;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.next_command()
    }
}

impl<'a> Commands<'a> {
    /// Each command surface as typed: at least one, never empty, never the name.
    #[inline]
    #[must_use]
    pub const fn iter(self) -> CommandsIter<'a> {
        CommandsIter {
            word: self.word,
            at: self.cuts.at,
            name_first: self.cuts.name_first,
        }
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
        let len = self.len as usize;
        if len == 0 {
            return None;
        }
        let last = self.bytes[len - 1];
        if last < 0x80 {
            self.len -= 1;
            return Some(last as char);
        }
        let c = self.as_str().chars().next_back()?;
        let width = c.len_utf8() as u8;
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

#[inline]
const fn is_stop(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | ',' | '\u{2026}')
}

/// A character of a script written without spaces between words: the
/// vocabulary's no-space set minus Hangul, which is in that set only to be
/// looked up raw (Korean writes its spaces).
#[inline]
const fn is_unspaced_script(c: char) -> bool {
    is_no_space_script(c) && !matches!(c as u32, 0x1100..=0x11FF | 0xAC00..=0xD7AF)
}

const GLUED_NAME_CHARS: usize = 2;

#[inline]
fn glued(lexicon: &TrickLexicon, token: &str, scratch: &mut String) -> Option<(Trick, Cuts)> {
    if token.is_empty() || token.as_bytes()[0].is_ascii() {
        return None;
    }
    let mut split_offsets = [0u8; TOKEN_CHARS];
    let mut chars = 0usize;
    for (at, c) in token.char_indices() {
        if !is_unspaced_script(c) {
            return None;
        }
        if chars > 0 {
            if at >= u128::BITS as usize || chars > split_offsets.len() {
                return None;
            }
            split_offsets[chars - 1] = at as u8;
        }
        chars += 1;
    }
    if chars <= GLUED_NAME_CHARS {
        return None;
    }
    let mut found: Option<(Trick, Cuts)> = None;
    let split_count = chars - 1;
    let mut i = 0usize;
    while i < split_count {
        let head_chars = i + 1;
        let at = split_offsets[i] as usize;
        i += 1;
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
        let cut = 1u128 << (at as u32);
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
    #[inline]
    pub const fn rekey(&mut self, session: u64) {
        if matches!(self.session, Some(s) if s == session) {
            return;
        }
        if let Some(old) = self.session {
            let dirty = self.line.line_chars > 0 || self.line.poisoned;
            self.forget(old);
            if dirty {
                let mut i = SESSION_MEMORY - 1;
                while i > 0 {
                    self.dirty[i] = self.dirty[i - 1];
                    i -= 1;
                }
                self.dirty[0] = Some(old);
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
    #[inline]
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
    pub const fn note_line_reset(&mut self) -> TrickFeed<'_> {
        let out = self.fresh_line();
        self.feed(out, false)
    }

    /// Ctrl+Z or Ctrl+\: a shell at its prompt keeps the line through them, while under a program the tty flushes it, so only a provably empty line (unpoisoned, nothing counted) is reset; any other is a break.
    #[inline]
    pub const fn note_job_chord(&mut self) -> TrickFeed<'_> {
        if self.line.line_chars == 0 && !self.line.poisoned {
            self.note_line_reset()
        } else {
            self.note_break()
        }
    }

    /// A backward word-kill (Ctrl+W, Alt+Backspace, Ctrl+Backspace).
    #[inline]
    pub fn note_word_kill(&mut self) -> TrickFeed<'_> {
        if self.line.poisoned || self.line.line_chars == 0 {
            return TrickFeed::NONE;
        }
        let after_space = match self.line.left {
            None => true,
            Some(c) => c.is_whitespace(),
        };
        let mut plain_token = after_space && self.line.token_is_live();
        if plain_token {
            let s = self.line.token.as_str();
            if s.is_ascii() {
                plain_token = s.bytes().all(|b| b.is_ascii_alphanumeric());
            } else {
                for c in s.chars() {
                    if !c.is_alphanumeric() {
                        plain_token = false;
                        break;
                    }
                }
            }
        }
        let out = if plain_token {
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
    pub const fn note_break(&mut self) -> TrickFeed<'_> {
        let out = self.poison();
        self.feed(out, false)
    }

    /// A press at a prompt whose tty swallows input (a password): poisons the line, so a passphrase cannot move the pet.
    #[inline]
    pub const fn note_no_echo(&mut self) -> TrickFeed<'_> {
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
    #[inline]
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
                self.line.left = Some(self.line.gap[run - 2]);
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
    #[inline]
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
    #[inline]
    fn push_token_char(&mut self, c: char) -> Option<Outcome> {
        let mut out = None;
        if self.line.token_chars == 0 {
            if !c.is_ascii() && aterm_grapheme::char_width(c) == 0 {
                return self.poison();
            }
            let glued = self.line.tail.is_some() || matches!(self.line.left, Some(l) if is_stop(l));
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
    #[inline]
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
                let run_continues = matches!(self.line.left, Some(l) if is_stop(l));
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
    #[inline]
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
                            (
                                TrickRole::Trick {
                                    trick,
                                    needs_vocative: false,
                                },
                                cuts,
                            )
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
                let in_phrase = matches!(
                    self.line.last_fire,
                    Some(at) if now.saturating_duration_since(at) <= PHRASE_GAP
                );
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
                    None => match guess {
                        Some(g) => Some(g),
                        None => held,
                    },
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
    const fn deliver_tail(&mut self, now: Instant, fed: u32) -> Option<Outcome> {
        let Some(verdict) = self.line.tail.take() else {
            return None;
        };
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
    #[inline]
    const fn deliver(&mut self, now: Instant, verdict: Verdict, fed: u32) -> Option<Outcome> {
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
    const fn go_impure(&mut self) -> Option<Outcome> {
        self.line.pure = false;
        self.line.held = None;
        self.line.tail = None;
        self.line.guess = None;
        if self.line.tentative.take().is_some() {
            Some(Outcome::Revoke)
        } else {
            None
        }
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
    const fn poison(&mut self) -> Option<Outcome> {
        self.line.poisoned = true;
        self.line.held = None;
        self.line.tail = None;
        self.line.guess = None;
        self.snapshot = None;
        if self.line.tentative.take().is_some() {
            Some(Outcome::Revoke)
        } else {
            None
        }
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
#[path = "typed_tricks_tests.rs"]
mod tests;
