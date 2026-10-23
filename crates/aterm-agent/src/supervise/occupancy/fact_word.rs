// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MANAGER'S DOOR'S WORD (`docs/DESIGN-harness-occupancy-2026-10-20.md` §8 row 2): the closed
//! fact word a managed worker's task record carries (`fact=<rule>@v<n>#<ordinal>`, for example
//! `fact=hold-room@v1#1`), where its manager reads that a line would have gone but for them, and
//! the host's answer to the loop's write of it ([`NoteOutcome`], `IdleHost::task_fact`).
//!
//! Nothing is posted and nothing is typed: the word names a built-in rule and an ordinal, and no
//! text of the agent's, so a task title or label can never reach it. Slice 2.0 lays the type and
//! the door alone; nothing writes the word yet.

use std::num::NonZeroU16;

use super::rules::RuleId;

/// THE FACT WORD (`<rule>@v<n>#<ordinal>`): a built-in rule ([`RuleId::name`]) and the episode's
/// ordinal, 1 to 65535. Its `Debug` holds the rule's variant and a number, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactWord {
    /// The rule whose line would have gone.
    pub rule: RuleId,
    /// The episode's ordinal: in the hold for a hold rule's, the wake's own for a wake's.
    pub ordinal: NonZeroU16,
}

impl FactWord {
    /// The word of `rule`'s episode `ordinal`; `None` for ordinal 0 or one past 65535.
    #[must_use]
    pub fn new(rule: RuleId, ordinal: usize) -> Option<Self> {
        let ordinal = NonZeroU16::new(u16::try_from(ordinal).ok()?)?;
        Some(Self { rule, ordinal })
    }

    /// The word as the record spells it: `hold-room@v1#1`.
    #[must_use]
    pub fn word(self) -> String {
        format!("{}#{}", self.rule.name(), self.ordinal)
    }

    /// THE WORD `text` SPELLS, and only one [`Self::word`] writes: a rule of the closed list
    /// ([`RuleId::of_name`]), `#`, and an ordinal of 1 to 65535 in plain decimal digits with no
    /// sign and no leading zero. Anything else, of any length, is `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (rule, ordinal) = text.split_once('#')?;
        let rule = RuleId::of_name(rule)?;
        let canonical = !ordinal.is_empty()
            && ordinal.len() <= 5
            && ordinal.bytes().all(|b| b.is_ascii_digit())
            && !ordinal.starts_with('0');
        if !canonical {
            return None;
        }
        let ordinal = NonZeroU16::new(ordinal.parse().ok()?)?;
        Some(Self { rule, ordinal })
    }
}

/// THE HOST'S ANSWER TO THE LOOP'S WRITE OF THE FACT WORD (`IdleHost::task_fact`, an act).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteOutcome {
    /// The word stands on the session's task record.
    Noted,
    /// The word was cleared from it.
    Cleared,
    /// The session has no task record: the default host's answer.
    NoTask,
    /// The record is closed, or the host refuses the write (its own copy of the owner's mode is
    /// not `prompt`).
    Closed,
    /// Another write held the record: tried again at the next break.
    Fenced,
}

impl NoteOutcome {
    /// Every outcome.
    pub const ALL: [NoteOutcome; 5] = [
        NoteOutcome::Noted,
        NoteOutcome::Cleared,
        NoteOutcome::NoTask,
        NoteOutcome::Closed,
        NoteOutcome::Fenced,
    ];

    /// Its word (`skipped why=<word>`, the tape's).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            NoteOutcome::Noted => "noted",
            NoteOutcome::Cleared => "cleared",
            NoteOutcome::NoTask => "no-task",
            NoteOutcome::Closed => "closed",
            NoteOutcome::Fenced => "fenced",
        }
    }

    /// The outcome `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.word() == word)
    }
}

#[cfg(test)]
#[path = "fact_word_tests.rs"]
mod tests;
