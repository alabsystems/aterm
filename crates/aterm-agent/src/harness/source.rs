// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHERE DID THIS FACT COME FROM — once, for the whole harness.
//!
//! There used to be four answers to that one question, and the word `grid`
//! appeared in all four as four different types: `usage::WindowSource` ranked
//! a figure's authority, `cli::Source` said which input answered a read verb,
//! `observe::Provenance` was a bit-set of the aterm reads behind an event,
//! and `limits::Channel` was the independence channel evidence arrived on.
//! Four vocabularies meant a surface could print `spine` where another
//! printed `grid` for the identical fact, and nothing in the type system
//! noticed.
//!
//! This is the one vocabulary. [`Source`] names every input a harness read
//! view takes a fact from, and one [`Source::as_str`] spells it, so `harness
//! usage --json` and `harness limits --json` print the same word for the same
//! input.
//!
//! What went, and why nothing names it any more: `SourceSet` and the
//! `status`/`offscreen`/`search` reads with `harness::observe` (2026-09-23,
//! the second harness stack), and the vendor HOOK variants (`hook`,
//! `StopFailure`, `Notification`, `PostModelSwitch`) with the pair rule and
//! independence channels of `harness::limits` (2026-09-25) — since decision
//! "B" nothing produced one.
//!
//! [`Source::figure_authority`] is authority over a NUMBER: a figure the
//! vendor COMPUTED (its statusLine, its cache, its transcript) beats one read
//! back off the frame it PAINTED, so [`Source::Grid`] is the floor. That is
//! not design §5.8.1's evidence order, which runs the other way: aterm drew
//! the frame, so the grid survives `--bare`, a user statusLine and a program
//! with no hooks at all, and it is rank 1 there.

use std::fmt;

/// One input the harness reads a fact from. The spelling of each is
/// [`Source::as_str`], and it is the word every surface prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Nothing reported it. The absence, not a place.
    None,
    /// aterm's own parsed grid (`text --json`) — what the vendor PAINTED.
    Grid,
    /// The vendor's live statusLine input.
    StatusLine,
    /// The vendor's on-disk utilization cache (`~/.claude.json`).
    Cache,
    /// The vendor's transcript on disk, and a payload NAMED the session it
    /// belongs to.
    Transcript,
    /// The vendor's transcript on disk, chosen because it is the NEWEST in
    /// this project directory and nothing named a session. A weaker answer
    /// than [`Source::Transcript`] and spelled differently on purpose: two
    /// Claude Code sessions in one working directory write into the same
    /// project directory, so "newest" is this session only while it is the
    /// one that wrote last.
    TranscriptNewest,
}

impl Source {
    /// Every source, in the order this module declares them.
    #[cfg(test)]
    pub(crate) const ALL: [Source; 6] = [
        Source::None,
        Source::Grid,
        Source::StatusLine,
        Source::Cache,
        Source::Transcript,
        Source::TranscriptNewest,
    ];

    /// The one wire spelling. Schema 1 everywhere it is printed.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::None => "none",
            Source::Grid => "grid",
            Source::StatusLine => "statusline",
            Source::Cache => "cache",
            Source::Transcript => "transcript",
            Source::TranscriptNewest => "transcript-newest",
        }
    }

    /// Read back [`Source::as_str`]. An unknown word is `None`, never a
    /// guess — a renamed source must fail closed, not fold into a neighbour.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn parse(s: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Authority over a NUMBER: higher wins when two sources report one
    /// window.
    ///
    /// NOT design §5.8.1's ordinal. Renamed from `rank` 2026-09-22 because
    /// the one word meant two opposite things: here 1 is the WEAKEST source
    /// that carries a figure (the painted grid) and 4 the strongest, while
    /// §5.8.1's "rank 1" is its FIRST row — the grid, the top of the
    /// evidence order.
    ///
    /// The statusLine, the cache and the transcript each carry a figure the
    /// vendor COMPUTED; the grid carries one it PAINTED, and a reading of a
    /// painted number is the weaker of the two. The absence ranks lowest.
    #[must_use]
    pub fn figure_authority(self) -> u8 {
        match self {
            Source::StatusLine => 4,
            Source::Cache => 3,
            Source::Transcript | Source::TranscriptNewest => 2,
            Source::Grid => 1,
            Source::None => 0,
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
    fn every_source_has_one_distinct_word_that_reads_back() {
        let mut seen: Vec<&str> = Vec::new();
        for s in Source::ALL {
            let word = s.as_str();
            assert!(!seen.contains(&word), "two sources spell themselves {word}");
            seen.push(word);
            assert_eq!(Source::parse(word), Some(s));
        }
        assert_eq!(seen.len(), Source::ALL.len());
        // NEGATIVE CONTROL: a renamed source fails closed rather than
        // folding into a neighbour. `spine` is here on purpose — it was the
        // FOURTH spelling of `grid` before this module existed, and nothing
        // may quietly accept it again; `StopFailure` went with the hooks.
        for unknown in ["spine", "window", "Grid", "", "statusline ", "StopFailure"] {
            assert_eq!(Source::parse(unknown), None, "{unknown}");
        }
    }

    #[test]
    fn a_computed_figure_outranks_a_painted_one() {
        assert!(Source::StatusLine.figure_authority() > Source::Cache.figure_authority());
        assert!(Source::Cache.figure_authority() > Source::Transcript.figure_authority());
        assert_eq!(
            Source::Transcript.figure_authority(),
            Source::TranscriptNewest.figure_authority()
        );
        assert!(Source::Transcript.figure_authority() > Source::Grid.figure_authority());
        assert!(Source::Grid.figure_authority() > Source::None.figure_authority());
    }
}
