// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Comment- and formatting-preserving TOML document model.

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

/// Raw, unparsed source text.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawString(String);

impl RawString {
    /// The text as written.
    #[inline]
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

/// The raw text on either side of a node (`None` substitutes canonical spacing).
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
    #[inline]
    #[must_use]
    pub const fn prefix(&self) -> Option<&RawString> {
        self.prefix.as_ref()
    }

    /// The raw text after the node, if it was parsed or explicitly set.
    #[inline]
    #[must_use]
    pub const fn suffix(&self) -> Option<&RawString> {
        self.suffix.as_ref()
    }

    /// Replace the leading text.
    #[inline]
    pub fn set_prefix(&mut self, prefix: impl Into<RawString>) {
        self.prefix = Some(prefix.into());
    }

    /// Replace the trailing text.
    #[inline]
    pub fn set_suffix(&mut self, suffix: impl Into<RawString>) {
        self.suffix = Some(suffix.into());
    }

    /// Forget both sides, returning the node to canonical spacing.
    #[inline]
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

/// The verbatim spelling of a scalar.
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
    #[inline]
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// The spelling this node encodes to, authored or canonical.
    pub fn display_repr(&self) -> String {
        self.repr
            .as_ref()
            .map_or_else(|| self.value.to_repr(), ToString::to_string)
    }

    /// Formatting around the scalar.
    #[inline]
    pub const fn decor(&self) -> &Decor {
        &self.decor
    }

    /// Mutable formatting around the scalar.
    #[inline]
    pub const fn decor_mut(&mut self) -> &mut Decor {
        &mut self.decor
    }

    /// Byte range of the scalar token in the document it was parsed from.
    #[inline]
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

/// One segment of a key, compared and hashed by its decoded text.
#[derive(Debug, Clone)]
pub struct Key {
    key: String,
    repr: Option<Repr>,
    decor: Decor,
    span: Option<Range<usize>>,
    /// The authored text of the key-value line this key is the leaf of. Dotted lines
    /// (`a.b = 1`, `a.c = 2`) share their parent nodes, so each line's trivia lives on its leaf,
    /// which is unique per line (a duplicate leaf is a parse error).
    pub(crate) path_repr: Option<PathRepr>,
    /// Position of this key's line among the document's key-value lines. Dotted keys share
    /// parent nodes, so flattening sorts on this to keep the authored line order. `usize::MAX`
    /// (not authored) sends inserted keys to the end of their table.
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
    /// A key from its decoded text, encoded in the narrowest legal spelling.
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

    /// Split a dotted key (`a.b."c.d"`) into its segments, each with its authored spelling.
    ///
    /// # Errors
    /// If `repr` is not a syntactically valid TOML key.
    pub fn parse(repr: &str) -> crate::Result<Vec<Key>> {
        parse::parse_key_path(repr)
    }

    /// The decoded name.
    #[inline]
    #[must_use]
    pub fn get(&self) -> &str {
        &self.key
    }

    /// The spelling this key encodes to, authored or canonical.
    #[must_use]
    pub fn display_repr(&self) -> String {
        self.repr
            .as_ref()
            .map_or_else(|| encode::encode_key(&self.key), ToString::to_string)
    }

    /// Mutable formatting around the key.
    #[inline]
    pub const fn decor_mut(&mut self) -> &mut Decor {
        &mut self.decor
    }

    /// Byte range of the key token in the document it was parsed from.
    #[inline]
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
