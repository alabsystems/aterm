// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a fact came from, one vocabulary for the whole harness.
//!
//! [`Source`] names every input a harness read view takes a fact from, and
//! [`Source::as_str`] spells it, so `harness usage --json` and `harness limits
//! --json` print the same word for the same input.

use std::fmt;

/// One input the harness reads a fact from. The spelling of each is
/// [`Source::as_str`], and it is the word every surface prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Nothing reported it. The absence, not a place.
    None,
    /// aterm's own parsed grid (`text --json`) — what the vendor PAINTED.
    Grid,
    /// The vendor's transcript on disk, and a payload NAMED the session it
    /// belongs to.
    Transcript,
    /// The vendor's transcript on disk, chosen because it is the newest in
    /// this project directory and nothing named a session. Weaker than
    /// [`Source::Transcript`] and spelled differently on purpose: two Claude
    /// Code sessions in one working directory share the project directory.
    TranscriptNewest,
}

impl Source {
    /// Every source, in the order this module declares them.
    #[cfg(test)]
    pub(crate) const ALL: [Source; 4] = [
        Source::None,
        Source::Grid,
        Source::Transcript,
        Source::TranscriptNewest,
    ];

    /// The one wire spelling. Schema 1 everywhere it is printed.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::None => "none",
            Source::Grid => "grid",
            Source::Transcript => "transcript",
            Source::TranscriptNewest => "transcript-newest",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_has_one_distinct_word() {
        let mut seen: Vec<&str> = Vec::new();
        for s in Source::ALL {
            let word = s.as_str();
            assert!(!seen.contains(&word), "two sources spell themselves {word}");
            seen.push(word);
        }
    }
}
