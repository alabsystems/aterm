// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a fact came from, one vocabulary for the whole harness.
//!
//! [`Source`] names every input a harness read view takes a fact from, and
//! [`Source::as_str`] spells it, so `harness usage --json` and `harness limits
//! --json` print the same word for the same input.

use std::fmt;

/// One input the harness reads a fact from, spelled by [`Source::as_str`] on every surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Nothing reported it. The absence, not a place.
    None,
    /// aterm's own parsed grid (`text --json`): what the vendor painted.
    Grid,
    /// The vendor's transcript on disk, of the session a payload named.
    Transcript,
    /// The vendor's transcript on disk, chosen because it is the newest in
    /// this project directory and nothing named a session. Weaker than
    /// [`Source::Transcript`] and spelled differently on purpose: two Claude
    /// Code sessions in one working directory share the project directory.
    TranscriptNewest,
    /// Codex's rollouts on disk, of the conversation of the thread a Codex
    /// session's own tools named (`CODEX_THREAD_ID`).
    Rollout,
}

impl Source {
    /// The one wire spelling. Schema 1 everywhere it is printed.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::None => "none",
            Source::Grid => "grid",
            Source::Transcript => "transcript",
            Source::TranscriptNewest => "transcript-newest",
            Source::Rollout => "rollout",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
