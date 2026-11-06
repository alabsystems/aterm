// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLAUDE CODE'S WORKFLOW JOURNAL, AS A CAPTURE OF ITS BUILD SAYS IT IS WRITTEN (the occupancy
//! design's §3.2 `workflow-step-returned@v1`; the slice-3 plan's §0 Q5).
//!
//! A format is admitted only for a Claude Code version some committed capture was taken of
//! (`lab workflow-capture`), matched exactly: [`FORMATS`] lists each, and every other build reads
//! [`Unread`] (`unread format (claude <v>)`), so the rule stays quiet there. A version range is
//! refused by design: a wrong event is worse than none.
//!
//! Vendor data, never instructions: an [`Event`] keeps a row's kind, an opaque agent key, the
//! closed status a capture recorded, and times. No label, description, prompt, result text or
//! script text is kept. A status no capture recorded is no [`StepStatus`]: the format's reader
//! makes the whole run unread, never `other`.
//!
//! No capture has been taken yet, so [`FORMATS`] is empty and every version reads unread.

/// What a format's reader maps a captured row to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    /// The captured status words, each with the closed status it reads as.
    pub statuses: &'static [(&'static str, StepStatus)],
}

/// EVERY CAPTURED FORMAT, by exact Claude Code version. Empty until a capture is committed.
pub const FORMATS: &[(&str, Format)] = &[];

/// A closed step status: only the words a capture recorded map to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// The step returned its result.
    Ok,
    /// The step returned part of its work.
    Partial,
    /// The step failed.
    Failed,
}

impl StepStatus {
    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            StepStatus::Ok => "ok",
            StepStatus::Partial => "partial",
            StepStatus::Failed => "failed",
        }
    }
}

/// No capture of the running build: its journal is not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unread;

/// THE FORMAT A CLAUDE CODE `version` WRITES, if a capture of exactly that version exists.
/// # Errors
/// [`Unread`]: no capture names it.
pub fn reader_for(version: &str) -> Result<&'static Format, Unread> {
    FORMATS
        .iter()
        .find(|(v, _)| *v == version)
        .map(|(_, format)| format)
        .ok_or(Unread)
}

/// One row of a run's journal, as a capture's reader keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A step started.
    Start {
        /// The agent's opaque key (FNV-1a of its id).
        agent: u64,
        /// When, ms.
        at_ms: i64,
    },
    /// A step returned.
    Result {
        /// The agent's opaque key.
        agent: u64,
        /// When, ms.
        at_ms: i64,
        /// Its closed status.
        status: StepStatus,
        /// Its row in the journal, from 1.
        row: u32,
    },
}

/// A WORKFLOW RUN'S ID IN CLAUDE CODE'S OWN FORM, `wf_<8 hex>-<3 hex>`: the only form a line may
/// name a run by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId(String);

impl RunId {
    /// The run id `text` is, if it is one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let hex = |s: &str, n: usize| {
            s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        };
        let (eight, three) = text.strip_prefix("wf_")?.split_once('-')?;
        (hex(eight, 8) && hex(three, 3)).then(|| Self(text.to_string()))
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A RETURNED STEP'S KEY (F5): FNV-1a over FNV-1a of its run's folder name and its row, so a
/// follower restarted on the same journal books the same step.
#[must_use]
pub fn step_key(run_dir: &str, row: u32) -> u64 {
    let run = aterm_hash::fnv1a_64(run_dir.as_bytes());
    aterm_hash::fnv1a_64(format!("step {run:016x} {row}").as_bytes())
}

#[cfg(test)]
#[path = "workflow_journal_tests.rs"]
mod tests;
