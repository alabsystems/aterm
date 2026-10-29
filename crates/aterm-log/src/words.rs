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
mod tests {
    use super::*;

    const PLANT: &str = "QZP7E7W1";

    /// The message a real panic of `act` carries, whatever its payload's type, and the form
    /// [`Panic::of`] makes of the payload itself.
    fn caught(act: impl FnOnce() + std::panic::UnwindSafe) -> (String, String) {
        let payload = std::panic::catch_unwind(act).expect_err("the act panics");
        let text = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .expect("a text payload");
        (text, Panic::of(payload.as_ref()).to_string())
    }

    /// Every std panic shape a reader can raise over a string holding the plant: each payload's
    /// form names its kind and its length and holds no byte of the message, and an equal-length
    /// message with the plant overwritten gives the same form (the law: the text reaches the form
    /// only through its length and its kind). `Len` and `word` likewise.
    #[test]
    fn panic_words_name_the_kind_and_never_the_text() {
        let row = || std::hint::black_box(format!("é {PLANT} row"));
        let cases: Vec<(&str, PanicKind, (String, String))> = vec![
            (
                "a char-boundary slice",
                PanicKind::StrSlice,
                caught(|| {
                    let _ = &row()[1..];
                }),
            ),
            (
                "an out-of-bounds slice",
                PanicKind::StrSlice,
                caught(|| {
                    let _ = &row()[..999];
                }),
            ),
            (
                "a backwards slice",
                PanicKind::StrSlice,
                caught(|| {
                    let s = row();
                    let (from, to) = std::hint::black_box((6, 4));
                    let _ = &s[from..to];
                }),
            ),
            (
                "an index out of bounds",
                PanicKind::Index,
                caught(|| {
                    let v = std::hint::black_box(vec![row()]);
                    let _ = &v[9];
                }),
            ),
            (
                "a slice range past the end",
                PanicKind::Index,
                caught(|| {
                    let v = std::hint::black_box(row().into_bytes());
                    let _ = &v[..999];
                }),
            ),
            (
                "an Option unwrap",
                PanicKind::UnwrapNone,
                caught(|| {
                    let _ = std::hint::black_box(None::<String>).unwrap();
                }),
            ),
            (
                "a Result unwrap",
                PanicKind::UnwrapErr,
                caught(|| {
                    std::hint::black_box(Err::<(), String>(row())).unwrap();
                }),
            ),
            (
                "an expect",
                PanicKind::Other,
                caught(|| {
                    let message = row();
                    let _ = std::hint::black_box(None::<u8>).expect(&message);
                }),
            ),
            (
                "an overflow",
                PanicKind::Overflow,
                caught(|| {
                    let _ = std::hint::black_box(u8::MAX) + 1;
                }),
            ),
            (
                "an assert_eq",
                PanicKind::Assert,
                caught(|| assert_eq!(row(), "x")),
            ),
            (
                "a formatted panic",
                PanicKind::Other,
                caught(|| panic!("reading {}", row())),
            ),
        ];
        for (what, kind, (text, form)) in &cases {
            assert_eq!(PanicKind::of(text), *kind, "{what}: the kind of {text:?}");
            assert_eq!(
                form,
                &format!("{} panic ({}B)", kind.word(), text.len()),
                "{what}"
            );
            assert!(!form.contains("QZP7E7W"), "{what}: {form}");
            let other = text.replace(PLANT, &"x".repeat(PLANT.len()));
            assert_eq!(
                Panic::of_text(&other).to_string(),
                *form,
                "{what}: the form reads the text past its length and kind"
            );
        }
        let held = cases
            .iter()
            .filter(|(.., (text, _))| text.contains(PLANT))
            .count();
        assert!(
            held >= 4,
            "CONTROL: std's messages quote the plant ({held} of them)"
        );
        // The str-slice shapes of the std before this toolchain's, which quote the string they
        // cut (a stock toolchain's cross builds still print them).
        for older in [
            format!(
                "byte index 1 is not a char boundary; it is inside 'é' (bytes 0..2) of `é {PLANT}`"
            ),
            format!("byte index 999 is out of bounds of `é {PLANT}`"),
            format!("begin <= end (6 <= 4) when slicing `é {PLANT}`"),
        ] {
            let form = Panic::of_text(&older).to_string();
            assert_eq!(
                form,
                format!("str-slice panic ({}B)", older.len()),
                "{older}"
            );
        }
        for kind in PanicKind::ALL {
            assert!(is_word(kind.word()), "{kind:?}");
        }
        let opaque: Box<dyn Any + Send> = Box::new(7_u32);
        assert_eq!(Panic::of(opaque.as_ref()).to_string(), "other panic (0B)");

        let said = format!("font_family {PLANT:?} is not installed here");
        assert_eq!(Len(&said).to_string(), format!("{}B", said.len()));
        for (text, kept) in [
            ("claude-code@2.1.312", true),
            ("harness.resume-ask.s-1", true),
            ("key=value", true),
            ("", false),
            ("two words", false),
            ("Couldn't", false),
            ("é", false),
            ("a/b", false),
        ] {
            assert_eq!(word(text) == text, kept, "{text:?}");
            assert_eq!(word(text) == "?", !kept, "{text:?}");
            let named = WordOrLen(text).to_string();
            assert_eq!(
                named,
                if kept {
                    text.to_string()
                } else {
                    format!("{}B", text.len())
                },
                "{text:?}"
            );
        }
        let long = "w".repeat(WORD_MAX);
        assert_eq!(word(&long), long);
        assert_eq!(word(&format!("{long}w")), "?");
        assert_eq!(word(&format!("{PLANT} row")), "?");
        assert_eq!(WordOrLen(&format!("{PLANT} row")).to_string(), "12B");
    }
}
