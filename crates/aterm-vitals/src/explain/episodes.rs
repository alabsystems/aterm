// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained cause records cannot turn published CPU growth into execution-window evidence.

use super::detect::{ActivityId, CauseId, Context};
use crate::model::Snapshot;
use crate::published_cpu::CpuExecutionWindow;

/// An episode that is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCause {
    /// Its cause.
    pub id: CauseId,
    /// When it opened, unix ms (the second split tick it held on).
    pub opened_unix_ms: i64,
    /// When its work began, by the evidence, unix ms: the earliest over the episode.
    pub since_ms: Option<i64>,
    /// Whether the retained record said it held at its observation.
    pub holds: bool,
    /// The retained diagnostic figure, not a physical execution-window bound.
    pub covers_mc: i64,
    /// The contexts that held for it when it last held.
    pub contexts: Vec<Context>,
}

/// The Deck's cause clause: `∵ Spotlight, after start-up`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeckCause {
    /// The activity.
    pub activity: ActivityId,
    /// Its people's word: `Spotlight`.
    pub word: &'static str,
    /// Its context, where it names a time: [`Context::Startup`] or [`Context::Wake`].
    pub context: Option<Context>,
}

/// Published counters cannot substantiate a physical CPU cause clause, including one retained
/// from an earlier observation. Independent machine contexts are read by `detect::machine`.
#[must_use]
pub fn deck_cause(snap: &Snapshot) -> Option<DeckCause> {
    let facts = snap.why.as_deref()?;
    match facts.split.cpu_accounting.execution_window {
        CpuExecutionWindow::Unavailable => None,
    }
}

#[cfg(test)]
#[path = "episodes_tests.rs"]
mod tests;
