// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What aterm.log may keep of a text.
//!
//! aterm.log (0600, local, the file a person attaches to a bug report) carries words, kinds, ids
//! and numbers. A text that came from somewhere else — a session's screen, the title a program
//! set, a typed prompt, a command line and its arguments, transcript or rollout text, a mail
//! body or other free text from another program, a config value, a panic's message (std's slice
//! panics quote the string they cut) — keeps the home it already has (the Messages store,
//! Manual, the tab mark, stderr, the crash and harness-fault reports, the drive store), and a log
//! line names it through one of these forms. Each form's `Display` depends on the text only
//! through its length and a closed word.

use std::any::Any;
use std::fmt;

/// A text by its length: `<n>B`, its UTF-8 length in bytes, never the text. It has no `Debug`,
/// so no `{:?}` can print what it holds.
#[derive(Clone, Copy)]
pub struct Len<'a>(pub &'a str);

impl fmt::Display for Len<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}B", self.0.len())
    }
}

/// The longest text [`word`] keeps, in bytes.
pub const WORD_MAX: usize = 64;

/// `s` where it is a word aterm.log may keep, else `?`: a word is non-empty, at most
/// [`WORD_MAX`] bytes, and each of its bytes is one of `[A-Za-z0-9@._:=-]` (the alphabet of the
/// harness's `journal::log_words`, aterm-agent's form of the same rule for a loop's lines).
#[must_use]
pub fn word(s: &str) -> &str {
    if is_word(s) { s } else { "?" }
}

/// A text by itself where it is a [`word`], else by its [`Len`] (`<n>B`).
#[derive(Clone, Copy)]
pub struct WordOrLen<'a>(pub &'a str);

impl fmt::Display for WordOrLen<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if is_word(self.0) {
            f.write_str(self.0)
        } else {
            Len(self.0).fmt(f)
        }
    }
}

/// Whether `s` is a [`word`].
fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= WORD_MAX
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b':' | b'=' | b'-')
        })
}

/// What a panic's message says it was, read off its opening words: the closed list
/// aterm.log names a panic by ([`Panic`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanicKind {
    /// A `str` cut where it cannot be: off a char boundary, past its end, or backwards
    /// (`start byte index …`, `end byte index …`, `byte range …`; before Rust 1.9x `byte
    /// index …` and `begin <= end …`, which quote the string they cut).
    StrSlice,
    /// A slice or array index out of its range (`index out of bounds`, `range … index`,
    /// `slice index starts …`).
    Index,
    /// `Option::unwrap` on a `None`.
    UnwrapNone,
    /// `Result::unwrap` on an `Err` (std appends the error's `Debug`).
    UnwrapErr,
    /// Arithmetic that overflowed or divided by zero (`attempt to …`).
    Overflow,
    /// A failed `assert!`, `assert_eq!` or `assert_ne!` (`assertion …`).
    Assert,
    /// Anything else: `panic!`'s own words, `expect`'s message, a payload that is no text.
    Other,
}

impl PanicKind {
    /// Every kind, in the order [`Self::word`] lists them.
    pub const ALL: [Self; 7] = [
        Self::StrSlice,
        Self::Index,
        Self::UnwrapNone,
        Self::UnwrapErr,
        Self::Overflow,
        Self::Assert,
        Self::Other,
    ];

    /// The kind's word in aterm.log.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::StrSlice => "str-slice",
            Self::Index => "index",
            Self::UnwrapNone => "unwrap-none",
            Self::UnwrapErr => "unwrap-err",
            Self::Overflow => "overflow",
            Self::Assert => "assert",
            Self::Other => "other",
        }
    }

    /// The kind `message` opens as; [`Self::Other`] where no std opening matches.
    #[must_use]
    pub fn of(message: &str) -> Self {
        const OPENINGS: [(&str, PanicKind); 13] = [
            ("start byte index ", PanicKind::StrSlice),
            ("end byte index ", PanicKind::StrSlice),
            ("byte range ", PanicKind::StrSlice),
            ("byte index ", PanicKind::StrSlice),
            ("begin <= end ", PanicKind::StrSlice),
            ("index out of bounds", PanicKind::Index),
            ("range start index ", PanicKind::Index),
            ("range end index ", PanicKind::Index),
            ("slice index starts ", PanicKind::Index),
            (
                "called `Option::unwrap()` on a `None` value",
                PanicKind::UnwrapNone,
            ),
            (
                "called `Result::unwrap()` on an `Err` value",
                PanicKind::UnwrapErr,
            ),
            ("attempt to ", PanicKind::Overflow),
            ("assertion ", PanicKind::Assert),
        ];
        OPENINGS
            .iter()
            .find(|(opening, _)| message.starts_with(opening))
            .map_or(Self::Other, |(_, kind)| *kind)
    }
}

/// A caught panic as aterm.log keeps it: `<kind> panic (<n>B)`, its [`PanicKind`] and its
/// message's length, never the message. The full text stays where the panic hook files it (the
/// crash and harness-fault reports, with the backtrace).
#[derive(Clone, Copy)]
pub struct Panic<'a>(Option<&'a str>);

impl<'a> Panic<'a> {
    /// The panic `catch_unwind` handed back: its `&str` or `String` payload; any other payload is
    /// an [`PanicKind::Other`] panic of no text.
    #[must_use]
    pub fn of(payload: &'a (dyn Any + Send)) -> Self {
        Self(
            payload
                .downcast_ref::<&'static str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str)),
        )
    }

    /// The panic whose message the caller already holds (a panic hook's payload, a fence's
    /// record of what it caught).
    #[must_use]
    pub const fn of_text(message: &'a str) -> Self {
        Self(Some(message))
    }

    /// Its kind.
    #[must_use]
    pub fn kind(&self) -> PanicKind {
        self.0.map_or(PanicKind::Other, PanicKind::of)
    }
}

impl fmt::Display for Panic<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} panic ({}B)",
            self.kind().word(),
            self.0.map_or(0, str::len)
        )
    }
}

#[cfg(test)]
#[path = "words_tests.rs"]
mod tests;
