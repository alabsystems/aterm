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
//! This is the one vocabulary. [`Source`] names every input the harness reads
//! a fact from. Each POSITION admits a subset — a window figure is never a
//! `StopFailure` — and the producer for that position is what enforces it;
//! nothing here pretends every variant is admissible everywhere. What IS
//! shared is the spelling: one [`Source::as_str`], so `harness usage --json`
//! and `harness limits --json` print the same word for the same input.
//!
//! `SourceSet` (a bit-set of these, for an observe event read from more than
//! one place) went with `harness::observe` on 2026-09-23: the grid spine it
//! served was the second harness stack, deleted in favour of the one engine
//! in `supervise`. The vendor HOOK variants stay because
//! [`super::limits`]' pair rule still names them; since decision "B" nothing
//! produces one.
//!
//! Two orderings live here and they point opposite ways, which is why both
//! are named rather than folded:
//!
//! * [`Source::rank`] is authority over a NUMBER. A figure the vendor
//!   COMPUTED (its statusLine, its cache, its transcript) beats one read back
//!   off the frame it PAINTED, so [`Source::Grid`] is the floor.
//! * ADMISSIBILITY runs the other way (design §5.8.1, inverted 2026-09-19):
//!   aterm drew the frame, so the grid survives `--bare`, a user statusLine,
//!   a hook rename and a program with no hooks at all, and it is rank 1.
//!

use std::fmt;

/// One input the harness reads a fact from.
///
/// The spelling of each is [`Source::as_str`], and it is the word every
/// surface prints. The three vendor hook events keep the vendor's own
/// CamelCase names, because a reason line that says `StopFailure` is naming
/// the hook the vendor documents and a kebab-case rendering of it would name
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Nothing reported it. The absence, not a place.
    None,
    /// aterm's own `status` classification.
    Status,
    /// aterm's own parsed grid (`text --json`) — what the vendor PAINTED.
    Grid,
    /// Rows a full-screen program scrolled past (`offscreen`).
    Offscreen,
    /// The full-scrollback sweep (`search`).
    Search,
    /// A vendor hook fired, event unspecified.
    Hook,
    /// The `StopFailure` hook.
    StopFailure,
    /// The `Notification` hook.
    Notification,
    /// The `PostModelSwitch` hook.
    PostModelSwitch,
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
    pub const ALL: [Source; 13] = [
        Source::None,
        Source::Status,
        Source::Grid,
        Source::Offscreen,
        Source::Search,
        Source::Hook,
        Source::StopFailure,
        Source::Notification,
        Source::PostModelSwitch,
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
            Source::Status => "status",
            Source::Grid => "grid",
            Source::Offscreen => "offscreen",
            Source::Search => "search",
            Source::Hook => "hook",
            Source::StopFailure => "StopFailure",
            Source::Notification => "Notification",
            Source::PostModelSwitch => "PostModelSwitch",
            Source::StatusLine => "statusline",
            Source::Cache => "cache",
            Source::Transcript => "transcript",
            Source::TranscriptNewest => "transcript-newest",
        }
    }

    /// Read back [`Source::as_str`]. An unknown word is `None`, never a
    /// guess — a renamed source must fail closed, not fold into a neighbour.
    #[must_use]
    pub fn parse(s: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Authority over a NUMBER: higher wins when two sources report one
    /// window.
    ///
    /// NOT design §5.8.1's ordinal. Renamed from `rank` 2026-09-22 because
    /// the one word meant two opposite things in one module: here 1 was the
    /// WEAKEST source that carries a figure (the painted grid) and 4 the
    /// strongest, while `limits`, `usage`, `profile` and `watch` all spell
    /// "rank 1" for §5.8.1's FIRST row — the spine, the top of the evidence
    /// order. A ledger reason an operator reads carried both in one sentence.
    /// One vocabulary, which is what this module exists for.
    ///
    /// The statusLine, the cache and the transcript each carry a figure the
    /// vendor COMPUTED; the grid carries one it PAINTED, and a reading of a
    /// painted number is the weaker of the two. Everything that never
    /// carries a figure ranks with the absence, because for this question it
    /// IS the absence.
    #[must_use]
    pub fn figure_authority(self) -> u8 {
        match self {
            Source::StatusLine => 4,
            Source::Cache => 3,
            Source::Transcript | Source::TranscriptNewest => 2,
            Source::Grid => 1,
            _ => 0,
        }
    }

    /// The independent CHANNEL this source arrives on (design §5.8.2).
    ///
    /// **Independence is by channel, not by reading**: two readings of the
    /// SAME grid frame are one source, so a `limit_notice` banner and a
    /// window figure read back off the same frame both answer
    /// [`Source::Grid`] and together are still one. A figure the vendor
    /// COMPUTED answers [`Source::StatusLine`] whether it came from the
    /// statusLine or its on-disk cache: one vendor computation, delivered
    /// twice, is one channel.
    #[must_use]
    pub fn channel(self) -> Source {
        match self {
            Source::Status | Source::Grid | Source::Offscreen | Source::Search => Source::Grid,
            Source::StatusLine | Source::Cache => Source::StatusLine,
            other => other,
        }
    }

    /// Whether this channel NAMES a failure rather than corroborating one
    /// somebody else named.
    ///
    /// A pair needs one of these. The grid is one because it carries the
    /// vendor's own words (design §5.8.1 rank 1); `StopFailure` and
    /// `Notification` are, because each is a vendor VALUE for this failure.
    /// A `PostModelSwitch` is a switch that names no failure, and a window
    /// figure is a percentage, so two corroborators are not a pair however
    /// well they agree.
    #[must_use]
    pub fn names_the_failure(self) -> bool {
        matches!(
            self.channel(),
            Source::Grid | Source::StopFailure | Source::Notification
        )
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
        // may quietly accept it again.
        for unknown in ["spine", "window", "Grid", "", "statusline "] {
            assert_eq!(Source::parse(unknown), None, "{unknown}");
        }
    }

    #[test]
    fn authority_and_admissibility_point_opposite_ways() {
        // Authority over a number: the vendor's own computation wins.
        assert!(Source::StatusLine.figure_authority() > Source::Cache.figure_authority());
        assert!(Source::Cache.figure_authority() > Source::Transcript.figure_authority());
        assert!(Source::Transcript.figure_authority() > Source::Grid.figure_authority());
        assert!(Source::Grid.figure_authority() > Source::None.figure_authority());
        // A source that carries no figure ranks with the absence.
        for s in [Source::Status, Source::StopFailure, Source::Hook] {
            assert_eq!(s.figure_authority(), 0, "{s}");
        }
        // Admissibility: the grid NAMES a failure; a computed figure does
        // not, however high its authority over the number.
        assert!(Source::Grid.names_the_failure());
        assert!(!Source::StatusLine.names_the_failure());
        assert!(!Source::Cache.names_the_failure());
    }

    #[test]
    fn two_readings_of_one_frame_are_one_channel() {
        for s in [
            Source::Status,
            Source::Grid,
            Source::Offscreen,
            Source::Search,
        ] {
            assert_eq!(s.channel(), Source::Grid, "{s}");
        }
        // One vendor computation delivered twice is one channel.
        assert_eq!(Source::Cache.channel(), Source::StatusLine);
        assert_eq!(Source::StatusLine.channel(), Source::StatusLine);
        // NEGATIVE CONTROL: the hook events stay distinct channels, or the
        // pair rule would accept a storm of one hook as two sources.
        for s in [
            Source::StopFailure,
            Source::Notification,
            Source::PostModelSwitch,
        ] {
            assert_eq!(s.channel(), s, "{s}");
        }
        assert_ne!(
            Source::StopFailure.channel(),
            Source::Notification.channel()
        );
    }
}
