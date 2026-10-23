// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DECLARED WAKES AS THE LOOP SEES THEM (`docs/DESIGN-harness-occupancy-2026-10-20.md` §4.1, §8
//! row 2): the book the window's host keeps of the conditions a session asked to be told of
//! (`wake when=…`), read by the loop through `IdleHost::wakes`, and the words of the doors that
//! tell one ([`WakeDoor`]) and of the host's answer to a claim on one ([`WakeClaim`]).
//!
//! Slice 2.0 lays the doors alone: no host keeps a book yet and the loop asks none of them, so
//! every answer is the default host's ([`WakesSeen::default`], [`WakeClaim::Gone`]). Closed words
//! and plain numbers only: an agent's label never crosses here.

use super::door::PaceRefusal;

/// THE SESSION'S BOOK OF WAKES AS THE LOOP SEES IT (`IdleHost::wakes`, a query): how many are
/// declared and untold, and how many of them hold now. The empty book is the default host's, and
/// what a replay of a build without the door reads (`TapeValue::absent`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WakesSeen {
    /// Wakes declared and not yet told, taken or expired.
    pub pending: u16,
    /// Of them, those whose condition holds now.
    pub ready: u16,
}

/// THE DOOR A WAKE IS TOLD THROUGH: the one teller its claim names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeDoor {
    /// A line the loop typed at a break in the agent's background work.
    Break,
    /// A fact riding the policy's own act at an idle point.
    Point,
    /// The answer to the agent's own `await wake`.
    Await,
}

impl WakeDoor {
    /// Every door.
    pub const ALL: [WakeDoor; 3] = [WakeDoor::Break, WakeDoor::Point, WakeDoor::Await];

    /// Its word (`door=<word>`, the rows' and the tape's).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            WakeDoor::Break => "break",
            WakeDoor::Point => "point",
            WakeDoor::Await => "await",
        }
    }

    /// The door `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.word() == word)
    }
}

/// THE HOST'S ANSWER TO A CLAIM ON A WAKE (`IdleHost::wake_claim`, an act): the wake is the
/// claiming door's to tell, or why it is not. Asked under `occupancy = "prompt"` alone, just
/// before the write; the default host keeps no book, so every wake is [`Self::Gone`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeClaim {
    /// Claimed for the asking door: its line may go.
    Granted,
    /// The agent's own `await wake` answered it first: told, with nothing to type.
    Answered,
    /// Another door holds its claim.
    Busy,
    /// Its need could not be re-read fresh: the line waits.
    Stale,
    /// Re-read, its need no longer holds: the line waits.
    Unmet,
    /// No free build slot is left to reserve for it.
    Reserved,
    /// The machine's pace for wake lines holds it.
    Machine,
    /// The host refuses the line ([`PaceRefusal`]).
    Refused(PaceRefusal),
    /// No such wake in that episode: told, taken, expired, or never declared.
    Gone,
}

impl WakeClaim {
    /// Whether the claiming door may tell it.
    #[must_use]
    pub const fn lets(self) -> bool {
        matches!(self, WakeClaim::Granted)
    }

    /// Its word (`skipped why=<word>`, the tape's).
    #[must_use]
    pub fn word(self) -> String {
        match self {
            WakeClaim::Granted => "granted".to_string(),
            WakeClaim::Answered => "answered".to_string(),
            WakeClaim::Busy => "busy".to_string(),
            WakeClaim::Stale => "stale".to_string(),
            WakeClaim::Unmet => "unmet".to_string(),
            WakeClaim::Reserved => "reserved".to_string(),
            WakeClaim::Machine => "machine".to_string(),
            WakeClaim::Refused(why) => format!("refused:{}", why.word()),
            WakeClaim::Gone => "gone".to_string(),
        }
    }

    /// The answer `word` names ([`Self::word`]).
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Some(match word {
            "granted" => WakeClaim::Granted,
            "answered" => WakeClaim::Answered,
            "busy" => WakeClaim::Busy,
            "stale" => WakeClaim::Stale,
            "unmet" => WakeClaim::Unmet,
            "reserved" => WakeClaim::Reserved,
            "machine" => WakeClaim::Machine,
            "gone" => WakeClaim::Gone,
            _ => WakeClaim::Refused(PaceRefusal::of_word(word.strip_prefix("refused:")?)?),
        })
    }
}

#[cfg(test)]
#[path = "wakes_seen_tests.rs"]
mod tests;
