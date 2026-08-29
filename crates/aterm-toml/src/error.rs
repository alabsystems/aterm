// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ONE error type for both halves of the crate.
//!
//! `toml` splits its errors into `toml::de::Error` and `toml::ser::Error` and
//! `toml_edit` adds a third (`TomlError`); all three carry the same two pieces
//! of information — a message and an optional byte span into the source — and
//! every consumer in this tree uses them the same way (`error.span()` to
//! underline a diagnostic, `error.to_string()` for the text). One type,
//! re-exported under the three historical names, keeps that surface without
//! three near-identical definitions.

use core::fmt;
use core::ops::Range;

/// A TOML parse, deserialize, or serialize failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
    span: Option<Range<usize>>,
    /// 1-based line and column, resolved against the source at construction
    /// time. Kept alongside the span because the span alone is useless in a
    /// message printed without the source next to it.
    line_col: Option<(usize, usize)>,
    /// The offending line, captured at construction so [`Display`] can print
    /// the annotated snippet without holding a borrow of the whole document.
    ///
    /// [`Display`]: fmt::Display
    snippet: Option<Snippet>,
}

/// One source line and the caret run under it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snippet {
    /// The line text, newline stripped.
    text: String,
    /// 0-based column within `text` where the carets start. May be one past the
    /// end: an unterminated construct is reported AT the end of input.
    caret_col: usize,
    /// How many carets. Never zero — an empty span still points somewhere.
    caret_len: usize,
}

impl Error {
    /// A failure with no source position (serializer errors, mostly).
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
            line_col: None,
            snippet: None,
        }
    }

    /// A failure anchored at a byte span of `source`.
    pub(crate) fn at(source: &str, span: Range<usize>, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            line_col: Some(line_col(source, span.start)),
            snippet: Some(snippet(source, &span)),
            span: Some(span),
        }
    }

    /// Attach a span to an error raised without one (serde `custom` errors from
    /// a `Deserialize` impl, which cannot know where they are).
    pub(crate) fn with_span(mut self, source: Option<&str>, span: Option<Range<usize>>) -> Self {
        if self.span.is_none()
            && let Some(span) = span
        {
            self.line_col = source.map(|s| line_col(s, span.start));
            self.snippet = source.map(|s| snippet(s, &span));
            self.span = Some(span);
        }
        self
    }

    /// The byte range in the original document this error points at, when the
    /// failure has a source position.
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }

    /// The failure text without the position prefix.
    #[must_use]
    pub fn message_text(&self) -> &str {
        &self.message
    }
}

/// The largest char boundary at or below `i`.
///
/// `Display` must not panic, whatever span it is handed: a caller can build an
/// [`Error`] from a span it computed itself, and slicing a string at a byte that
/// is inside a character aborts. Rounding DOWN keeps the caret on the character
/// the span started in.
fn floor_boundary(source: &str, i: usize) -> usize {
    let mut i = i.min(source.len());
    while i > 0 && !source.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Where `offset` sits: 1-based line, 1-based column, and the byte offset the
/// line starts at.
///
/// END OF INPUT IS ON THE LAST LINE, not on the empty line after it. MEASURED
/// against the oracle: `"x = [1, 2\n"` reports *line 1, column 11* for its
/// end-of-input span, and `"# Manual\nfont_px = [ \n"` reports *line 2, column
/// 14* — in both cases the line whose text the caret belongs under. Counting the
/// final newline would put every unterminated-construct diagnostic on a blank
/// line below the file, which is where a config editor would then scroll.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let (line, col, _) = locate(source, offset);
    (line, col)
}

fn locate(source: &str, offset: usize) -> (usize, usize, usize) {
    let offset = floor_boundary(source, offset.min(source.len()));
    let scan = if offset == source.len() && offset > 0 {
        floor_boundary(source, offset - 1)
    } else {
        offset
    };
    let head = &source[..scan];
    let line = 1 + head.bytes().filter(|b| *b == b'\n').count();
    let line_start = head.rfind('\n').map_or(0, |i| i + 1);
    let col = 1 + source[line_start..offset].chars().count();
    (line, col, line_start)
}

/// Cut the line containing `span.start` out of `source` and work out where the
/// carets go under it.
fn snippet(source: &str, span: &Range<usize>) -> Snippet {
    let start = floor_boundary(source, span.start.min(source.len()));
    let (_, col, line_start) = locate(source, start);
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |i| line_start + i);
    let text = source[line_start..line_end]
        .trim_end_matches('\r')
        .to_owned();
    // The caret run stops at the end of the line: a span that reaches over a
    // newline (an unterminated multi-line string) would otherwise underline
    // text that is not on the line being printed.
    // `line_end` can be BEFORE `start` when the position is end-of-input on a
    // file that ends with a newline: the line the caret belongs under has
    // already ended. One caret, at the column past that line's last character.
    let cap = line_end.max(start);
    let end = floor_boundary(source, span.end.clamp(start, cap));
    let span_chars = source[start..end.max(start)].chars().count();
    Snippet {
        text,
        caret_col: col - 1,
        caret_len: span_chars.max(1),
    }
}

impl fmt::Display for Error {
    /// The annotated form `toml` and `toml_edit` print, because it is what the
    /// config editor shows the operator:
    ///
    /// ```text
    /// TOML parse error at line 1, column 11
    ///   |
    /// 1 | font_px = "huge"
    ///   |           ^^^^^^
    /// invalid type: string "huge", expected f64
    /// ```
    ///
    /// The offending LINE is the part that carries the information — a
    /// diagnostic that named only a line number would make the editor's status
    /// bar say "line 1, column 11" and nothing about `font_px`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some((line, col)) = self.line_col else {
            return f.write_str(&self.message);
        };
        write!(f, "TOML parse error at line {line}, column {col}")?;
        if let Some(snippet) = &self.snippet {
            let gutter = decimal_width(line);
            write!(f, "\n{:gutter$} |", "")?;
            write!(f, "\n{line:gutter$} | {}", snippet.text)?;
            write!(
                f,
                "\n{:gutter$} | {:caret_col$}{:^<carets$}",
                "",
                "",
                "",
                caret_col = snippet.caret_col,
                carets = snippet.caret_len
            )?;
        }
        // The TRAILING NEWLINE is the oracle's, and it is kept on purpose: both
        // `toml` and `toml_edit` end this form with one, every consumer in this
        // tree was written against that, and a Display that differs by a byte
        // from the crate it replaces is a difference someone has to chase.
        writeln!(f, "\n{}", self.message)
    }
}

/// Digits in `n`, for the snippet's line-number gutter.
fn decimal_width(n: usize) -> usize {
    if n == 0 { 1 } else { n.ilog10() as usize + 1 }
}

impl std::error::Error for Error {}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::message(msg.to_string())
    }
}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::message(msg.to_string())
    }
}

/// The result of a parse or a deserialize.
pub type Result<T> = core::result::Result<T, Error>;
