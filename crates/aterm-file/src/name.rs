// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A shared file's name: `<set>/<leaf>`, each part `[a-z][a-z0-9-]{0,19}`
//! (docs/DESIGN-shared-files-2026-11-11.md §3).

use std::fmt;

/// The most bytes one part of a name holds.
pub const MAX_PART: usize = 20;

/// Whether `bytes[from..to]` is one part of a name: a lower-case letter, then at most 19 of
/// `[a-z0-9-]`.
const fn part_at(bytes: &[u8], from: usize, to: usize) -> bool {
    if from >= to || to - from > MAX_PART || !bytes[from].is_ascii_lowercase() {
        return false;
    }
    let mut i = from + 1;
    while i < to {
        let b = bytes[i];
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether `text` is one part of a name.
#[must_use]
pub const fn part(text: &str) -> bool {
    part_at(text.as_bytes(), 0, text.len())
}

/// Where `text`'s one `/` is, when both sides are parts ([`part`]).
const fn split(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            if at.is_some() {
                return None;
            }
            at = Some(i);
        }
        i += 1;
    }
    match at {
        Some(slash) if part_at(bytes, 0, slash) && part_at(bytes, slash + 1, bytes.len()) => {
            Some(slash)
        }
        _ => None,
    }
}

/// Whether `text` is a whole name, for a compiled registry's build-time check.
#[must_use]
pub const fn well_formed(text: &str) -> bool {
    split(text).is_some()
}

/// A shared file's name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name {
    set: String,
    leaf: String,
}

/// `text` is not `<set>/<leaf>` in the grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotAName;

impl fmt::Display for NotAName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a shared file is named <set>/<leaf>, each part [a-z][a-z0-9-]{0,19}")
    }
}

impl std::error::Error for NotAName {}

impl Name {
    /// Parse `<set>/<leaf>`.
    ///
    /// # Errors
    /// `text` breaks the grammar.
    pub fn parse(text: &str) -> Result<Self, NotAName> {
        let slash = split(text).ok_or(NotAName)?;
        Ok(Self {
            set: text[..slash].to_owned(),
            leaf: text[slash + 1..].to_owned(),
        })
    }

    /// The set (`ai`).
    #[must_use]
    pub fn set(&self) -> &str {
        &self.set
    }

    /// The leaf (`accounts`).
    #[must_use]
    pub fn leaf(&self) -> &str {
        &self.leaf
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.set, self.leaf)
    }
}
