// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The comment-preserving document model — aterm's replacement for `toml_edit`.
//!
//! # Why a second model exists next to [`crate::Value`]
//!
//! [`crate::from_str`] answers "what does this file MEAN": it throws away
//! comments, key order, quoting style, and whitespace, because a
//! `#[derive(Deserialize)]` struct has nowhere to put them. Three jobs in aterm
//! need the opposite answer — what the file SAYS, byte for byte:
//!
//! * the Preferences window writes back only the keys the user touched and must
//!   leave every other line of `aterm.toml` exactly as it was found;
//! * the config editor underlines diagnostics, which needs the byte span of the
//!   offending token, not its value;
//! * `cargo forge` rewrites `vendor/forge.toml` and asserts the round-trip does
//!   not move a single byte.
//!
//! So this module parses into a tree that keeps the ORIGINAL text of every
//! token and every run of whitespace and comments between tokens. Rendering is
//! concatenation, which is what makes "parse then print is the identity" a
//! structural property rather than a hope.
//!
//! # The formatting model
//!
//! Every node carries a [`Decor`]: the raw text immediately BEFORE it
//! (`prefix`) and immediately AFTER it (`suffix`). Whitespace, newlines, and
//! comments all live in one of those two strings, owned by whichever node they
//! precede or follow, so no byte of the source is unaccounted for.
//!
//! aterm's decor differs from `toml_edit`'s in exactly one deliberate way: the
//! LINE TERMINATOR belongs to the suffix. `toml_edit` writes the newline from
//! the encoder and so cannot represent a file whose last line has none; folding
//! it into the suffix makes the encoder a pure `concat` and makes round-trip
//! exact for those files too.
//!
//! A `None` prefix/suffix means "this node was built, not parsed — use the
//! canonical spacing for its position", which is how a freshly inserted key
//! comes out as `key = value` and not `key=value`.

mod document;
mod encode;
mod item;
mod parse;
mod value;

use core::fmt;
use core::ops::Range;

pub use document::DocumentMut;
pub use item::{ArrayOfTables, Item, Table, TableLike};
pub use value::{Array, InlineTable, Value};

pub(crate) use parse::{ParseLimits, parse_document};

/// A parse failure. One type across the crate — see [`crate::Error`].
pub type TomlError = crate::Error;

/// Raw, unparsed source text: a run of whitespace, newlines, and comments, or
/// the verbatim spelling of a scalar.
///
/// It is a distinct type rather than a bare `String` for the same reason
/// `toml_edit` makes one — so a `set_prefix("junk")` reads as a formatting
/// operation at the call site, and so the crate can later validate that what a
/// caller injects is actually whitespace/comment without a breaking change.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawString(String);

impl RawString {
    /// The text as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RawString {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for RawString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&RawString> for RawString {
    fn from(value: &RawString) -> Self {
        value.clone()
    }
}

impl fmt::Display for RawString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The raw text on either side of a node.
///
/// `None` on either side means "unset": the encoder substitutes the canonical
/// spacing for the position the node sits in (see the module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decor {
    prefix: Option<RawString>,
    suffix: Option<RawString>,
}

impl Decor {
    /// Decor with both sides fixed.
    #[must_use]
    pub fn new(prefix: impl Into<RawString>, suffix: impl Into<RawString>) -> Self {
        Self {
            prefix: Some(prefix.into()),
            suffix: Some(suffix.into()),
        }
    }

    /// The raw text before the node, if it was parsed or explicitly set.
    #[must_use]
    pub fn prefix(&self) -> Option<&RawString> {
        self.prefix.as_ref()
    }

    /// The raw text after the node, if it was parsed or explicitly set.
    #[must_use]
    pub fn suffix(&self) -> Option<&RawString> {
        self.suffix.as_ref()
    }

    /// Replace the leading text.
    pub fn set_prefix(&mut self, prefix: impl Into<RawString>) {
        self.prefix = Some(prefix.into());
    }

    /// Replace the trailing text.
    pub fn set_suffix(&mut self, suffix: impl Into<RawString>) {
        self.suffix = Some(suffix.into());
    }

    /// Forget both sides, returning the node to canonical spacing.
    pub fn clear(&mut self) {
        self.prefix = None;
        self.suffix = None;
    }

    pub(crate) fn prefix_or(&self, default: &str) -> String {
        self.prefix
            .as_ref()
            .map_or_else(|| default.to_owned(), |raw| raw.0.clone())
    }

    pub(crate) fn suffix_or(&self, default: &str) -> String {
        self.suffix
            .as_ref()
            .map_or_else(|| default.to_owned(), |raw| raw.0.clone())
    }
}

/// The verbatim spelling of a scalar, kept so `0x1F`, `1_000_000`, `+3.0e2` and
/// `'''literal'''` all survive a round-trip as themselves.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Repr {
    raw: RawString,
}

impl Repr {
    /// Wrap already-valid TOML source text as a representation.
    #[must_use]
    pub fn new_unchecked(raw: impl Into<RawString>) -> Self {
        Self { raw: raw.into() }
    }

    /// The source text.
    #[must_use]
    pub fn as_raw(&self) -> &RawString {
        &self.raw
    }
}

impl fmt::Display for Repr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.raw, f)
    }
}

/// A scalar plus the way it was written.
#[derive(Debug, Clone)]
pub struct Formatted<T> {
    value: T,
    repr: Option<Repr>,
    decor: Decor,
    span: Option<Range<usize>>,
}

impl<T: ValueRepr> Formatted<T> {
    /// A scalar with no authored spelling — the encoder renders it canonically.
    pub fn new(value: T) -> Self {
        Self {
            value,
            repr: None,
            decor: Decor::default(),
            span: None,
        }
    }

    pub(crate) fn with_repr(value: T, repr: Repr, span: Range<usize>) -> Self {
        Self {
            value,
            repr: Some(repr),
            decor: Decor::default(),
            span: Some(span),
        }
    }

    /// The parsed value.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// Take the parsed value, dropping the formatting.
    pub fn into_value(self) -> T {
        self.value
    }

    /// The authored spelling, if this node came from a parse.
    pub fn repr(&self) -> Option<&Repr> {
        self.repr.as_ref()
    }

    /// The spelling this node encodes to, authored or canonical.
    pub fn display_repr(&self) -> String {
        self.repr
            .as_ref()
            .map_or_else(|| self.value.to_repr(), ToString::to_string)
    }

    /// Formatting around the scalar.
    pub fn decor(&self) -> &Decor {
        &self.decor
    }

    /// Mutable formatting around the scalar.
    pub fn decor_mut(&mut self) -> &mut Decor {
        &mut self.decor
    }

    /// Byte range of the scalar token in the document it was parsed from.
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }
}

/// A scalar that knows how to spell itself when nothing authored it.
pub trait ValueRepr: Clone + fmt::Debug {
    /// The canonical TOML spelling of this value.
    fn to_repr(&self) -> String;
}

impl ValueRepr for String {
    fn to_repr(&self) -> String {
        crate::edit::encode::encode_basic_string(self)
    }
}

impl ValueRepr for i64 {
    fn to_repr(&self) -> String {
        self.to_string()
    }
}

impl ValueRepr for f64 {
    fn to_repr(&self) -> String {
        crate::edit::encode::encode_float(*self)
    }
}

impl ValueRepr for bool {
    fn to_repr(&self) -> String {
        self.to_string()
    }
}

impl ValueRepr for crate::Datetime {
    fn to_repr(&self) -> String {
        self.to_string()
    }
}

/// One segment of a key: what it means, and how it was spelled.
///
/// Equality, ordering, and hashing are on the DECODED text only, so
/// `"font-px"`, `'font-px'`, and `font-px` are the same key — which is what the
/// spec says and what duplicate detection has to agree with.
#[derive(Debug, Clone)]
pub struct Key {
    key: String,
    repr: Option<Repr>,
    decor: Decor,
    span: Option<Range<usize>>,
    /// The verbatim text of the key-value LINE this key leads, when it was
    /// parsed as the leaf of one.
    ///
    /// This is the crate's answer to a problem a per-segment decor model cannot
    /// solve. `a.b = 1` and `a.c = 2` share one `a` node in the tree, so the two
    /// lines' leading comments and indentation have nowhere separate to live if
    /// formatting hangs off path segments. Anchoring the whole authored path on
    /// the LEAF — which is unique per line, because a duplicate leaf is a parse
    /// error — makes round-trip exact by construction: the head is literally the
    /// source bytes from the start of the line's trivia to the leaf key.
    pub(crate) path_repr: Option<PathRepr>,
    /// Where this key's LINE stood among the document's key-value lines.
    ///
    /// Dotted keys share their parent nodes, so tree order alone cannot tell
    /// `net.listen` / `font_px` / `net.timeout_ms` apart from the same three
    /// lines regrouped — and regrouping a user's file is exactly what the
    /// non-destructive-save contract forbids. Flattening sorts on this.
    /// `usize::MAX` means "not authored", which sends programmatically inserted
    /// keys to the end of their table rather than into the middle of someone
    /// else's lines.
    pub(crate) order: usize,
}

/// The authored text around a leaf key: everything before it on its line, and
/// everything between it and the `=`.
#[derive(Debug, Clone)]
pub(crate) struct PathRepr {
    pub(crate) head: RawString,
    pub(crate) tail: RawString,
}

impl Key {
    /// A key from its decoded text; the encoder picks the narrowest legal
    /// spelling (bare when the characters allow it, quoted otherwise).
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            repr: None,
            decor: Decor::default(),
            span: None,
            path_repr: None,
            order: usize::MAX,
        }
    }

    pub(crate) fn with_repr(key: String, repr: Repr, span: Range<usize>) -> Self {
        Self {
            key,
            repr: Some(repr),
            decor: Decor::default(),
            span: Some(span),
            path_repr: None,
            order: usize::MAX,
        }
    }

    /// Split a dotted key EXPRESSION (`a.b."c.d"`) into its segments.
    ///
    /// This is the parser, not a `split('.')`: a dot inside a quoted segment is
    /// part of the name, and the segments come back with their authored
    /// spellings intact.
    ///
    /// # Errors
    /// If `repr` is not a syntactically valid TOML key.
    pub fn parse(repr: &str) -> crate::Result<Vec<Key>> {
        parse::parse_key_path(repr)
    }

    /// The decoded name.
    #[must_use]
    pub fn get(&self) -> &str {
        &self.key
    }

    /// The authored spelling, if this key came from a parse.
    #[must_use]
    pub fn repr(&self) -> Option<&Repr> {
        self.repr.as_ref()
    }

    /// The spelling this key encodes to, authored or canonical.
    #[must_use]
    pub fn display_repr(&self) -> String {
        self.repr
            .as_ref()
            .map_or_else(|| encode::encode_key(&self.key), ToString::to_string)
    }

    /// Formatting around the key.
    #[must_use]
    pub fn decor(&self) -> &Decor {
        &self.decor
    }

    /// Mutable formatting around the key.
    pub fn decor_mut(&mut self) -> &mut Decor {
        &mut self.decor
    }

    /// Byte range of the key token in the document it was parsed from.
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_repr())
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for Key {}

impl core::hash::Hash for Key {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

impl From<&str> for Key {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Key {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl core::str::FromStr for DocumentMut {
    type Err = crate::Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        parse_document(s, ParseLimits::default())
    }
}
