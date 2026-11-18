// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A shared file's name: `<set>/<leaf>`, each part `[a-z][a-z0-9-]{0,19}`
//! (docs/DESIGN-shared-files-2026-11-11.md §3). The grammar is `aterm_types::shared_file`'s, which
//! the Editor tab that saves a face judges its path by too.

use std::fmt;

use aterm_types::shared_file::split;
pub use aterm_types::shared_file::{MAX_PART, part, well_formed};

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
