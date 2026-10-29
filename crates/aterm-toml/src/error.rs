// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Error type for TOML parsing, deserializing, and serializing.

use core::fmt;
use core::ops::Range;

/// A TOML parse, deserialize, or serialize failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
    span: Option<Range<usize>>,
    /// 1-based line and column, resolved at construction so a message printed without
    /// the source still says where.
    line_col: Option<(usize, usize)>,
    /// The offending line, captured at construction so `Display` prints the snippet
    /// without a borrow of the document.
    snippet: Option<Snippet>,
}

/// One source line and the caret run under it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snippet {
    /// The line text, newline stripped.
    text: String,
    /// 0-based column within `text` where the carets start. May be one past the
    /// end: an unterminated construct is reported at the end of input.
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
    #[inline]
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }

    /// The failure text without the position prefix.
    #[inline]
    #[must_use]
    pub fn message_text(&self) -> &str {
        &self.message
    }

    /// What an app log may keep of this error: `TOML error at line L, column C`, or
    /// `TOML error` where it has no position. Never the offending line ([`Display`](fmt::Display)
    /// prints it whole) and never the message, which can quote the value (a serde
    /// `invalid value: string "…"`): the file holds both, and a person reads them where the
    /// error is shown to them.
    #[inline]
    #[must_use]
    pub const fn log_words(&self) -> LogWords {
        LogWords {
            line_col: self.line_col,
        }
    }
}

/// [`Error::log_words`]: an error's position, and nothing of its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogWords {
    line_col: Option<(usize, usize)>,
}

impl fmt::Display for LogWords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line_col {
            Some((line, col)) => write!(f, "TOML error at line {line}, column {col}"),
            None => f.write_str("TOML error"),
        }
    }
}

/// The largest char boundary at or below `i`. `Display` must not panic whatever span it is
/// handed, and slicing inside a character panics; rounding down keeps the caret on the
/// character the span started in.
#[inline]
fn floor_boundary(source: &str, i: usize) -> usize {
    let mut i = i.min(source.len());
    while i > 0 && !source.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Where `offset` sits: 1-based line and column. End of input is on the last line, not the
/// empty line after it (`"x = [1, 2\n"` reports line 1, column 11), so an unterminated
/// construct's diagnostic lands under its own text.
#[inline]
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

fn snippet(source: &str, span: &Range<usize>) -> Snippet {
    let start = floor_boundary(source, span.start.min(source.len()));
    let (_, col, line_start) = locate(source, start);
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |i| line_start + i);
    let text = source[line_start..line_end]
        .trim_end_matches('\r')
        .to_owned();
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
        writeln!(f, "\n{}", self.message)
    }
}

/// Digits in `n`, for the snippet's line-number gutter.
#[inline]
const fn decimal_width(n: usize) -> usize {
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
