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
//! a fact from; [`SourceSet`] is a set of them, for a fact that came from more
//! than one read at once. Each POSITION admits a subset — a window figure is
//! never a `StopFailure`, an event's reads are never a `Cache` — and the
//! producer for that position is what enforces it; nothing here pretends
//! every variant is admissible everywhere. What IS shared is the spelling:
//! one [`Source::as_str`], so `harness status`, `harness usage --json`,
//! `harness limits --json` and an observe event all print the same word for
//! the same input.
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
//! What is NOT here, deliberately: `align::Attestation`. That answers a
//! different question — who ATTESTS a capability verdict, a signed row or a
//! local run — and it was the fifth "source" only because it had been given
//! the same name. Renaming it was the fix; merging it would have been a
//! category error.

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

/// A SET of sources: what one fact was read from, when it was read from more
/// than one place at once.
///
/// An observe event is the case this exists for — a turn boundary can be
/// carried by aterm's `status` and confirmed on the parsed grid, and the
/// event says both rather than picking. It is a bit-set so an event is
/// `Copy` and cheap; [`SourceSet::names`] is how it prints, in the fixed
/// order of [`Source::ALL`], with the one vocabulary's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceSet(u16);

impl SourceSet {
    /// The empty set: nothing was read.
    pub const EMPTY: SourceSet = SourceSet(0);

    /// The bit for one source. [`Source::None`] is the ABSENCE and has no
    /// bit, so `SourceSet::just(Source::None)` is empty — which is what it
    /// means.
    const fn bit(s: Source) -> u16 {
        match s {
            Source::None => 0,
            Source::Status => 1 << 0,
            Source::Grid => 1 << 1,
            Source::Offscreen => 1 << 2,
            Source::Search => 1 << 3,
            Source::Hook => 1 << 4,
            Source::StopFailure => 1 << 5,
            Source::Notification => 1 << 6,
            Source::PostModelSwitch => 1 << 7,
            Source::StatusLine => 1 << 8,
            Source::Cache => 1 << 9,
            Source::Transcript => 1 << 10,
            Source::TranscriptNewest => 1 << 11,
        }
    }

    /// The set holding exactly one source.
    #[must_use]
    pub const fn just(s: Source) -> SourceSet {
        SourceSet(SourceSet::bit(s))
    }

    /// This set with `s` added.
    #[must_use]
    pub const fn with(self, s: Source) -> SourceSet {
        SourceSet(self.0 | SourceSet::bit(s))
    }

    /// This set with every member of `other` added.
    #[must_use]
    pub const fn union(self, other: SourceSet) -> SourceSet {
        SourceSet(self.0 | other.0)
    }

    /// Whether `s` is in this set. [`Source::None`] is in no set.
    #[must_use]
    pub const fn has(self, s: Source) -> bool {
        let bit = SourceSet::bit(s);
        bit != 0 && self.0 & bit == bit
    }

    /// Whether every member of `other` is in this set. The empty set is in
    /// no set, so `has_all(EMPTY)` is `false` — "nothing" is never a fact
    /// this set carries.
    #[must_use]
    pub const fn has_all(self, other: SourceSet) -> bool {
        other.0 != 0 && self.0 & other.0 == other.0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The members, in the fixed order of [`Source::ALL`].
    #[must_use]
    pub fn members(self) -> Vec<Source> {
        Source::ALL
            .into_iter()
            .filter(|s| self.has(*s))
            .collect::<Vec<_>>()
    }

    /// The source words, in the fixed order of [`Source::ALL`].
    #[must_use]
    pub fn names(self) -> Vec<&'static str> {
        self.members().into_iter().map(Source::as_str).collect()
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

    #[test]
    fn the_set_is_a_set_and_the_absence_is_in_none_of_them() {
        let both = SourceSet::just(Source::Status).with(Source::Grid);
        assert_eq!(both.names(), vec!["status", "grid"]);
        assert!(both.has(Source::Status) && both.has(Source::Grid));
        assert!(!both.has(Source::Search));
        assert!(both.has_all(SourceSet::just(Source::Grid)));
        assert!(!both.has_all(both.with(Source::Search)));
        // Idempotent, and order does not matter.
        assert_eq!(both.with(Source::Grid), both);
        assert_eq!(
            SourceSet::just(Source::Grid).with(Source::Status),
            both,
            "the names are ordered by ALL, not by insertion"
        );
        // NEGATIVE CONTROLS: the absence is not a member, and the empty set
        // is in no set.
        assert!(SourceSet::just(Source::None).is_empty());
        assert!(!both.has(Source::None));
        assert!(!both.has_all(SourceSet::EMPTY));
        assert!(SourceSet::EMPTY.names().is_empty());
        // Every source that can be a member has its own bit.
        let all = Source::ALL
            .into_iter()
            .fold(SourceSet::EMPTY, SourceSet::with);
        assert_eq!(all.members().len(), Source::ALL.len() - 1, "all but None");
    }
}
