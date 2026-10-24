// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed fact word (`<rule>@v<n>#<ordinal>`) on a managed worker's task record, and the host's
//! answer to the loop's write of it ([`NoteOutcome`]).

use std::num::NonZeroU16;

use super::rules::RuleId;

/// THE FACT WORD (`<rule>@v<n>#<ordinal>`): a built-in rule ([`RuleId::name`]) and an ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactWord {
    /// The rule whose line would have gone.
    pub rule: RuleId,
    /// The episode's ordinal: in the hold for a hold rule's, the wake's own for a wake's.
    pub ordinal: NonZeroU16,
}

impl FactWord {
    /// The word of `rule`'s episode `ordinal`.
    #[must_use]
    pub fn new(rule: RuleId, ordinal: usize) -> Option<Self> {
        let ordinal = NonZeroU16::new(u16::try_from(ordinal).ok()?)?;
        Some(Self { rule, ordinal })
    }

    /// The word as the record spells it.
    #[must_use]
    pub fn word(self) -> String {
        format!("{}#{}", self.rule.name(), self.ordinal)
    }

    /// THE WORD `text` SPELLS, and only one [`Self::word`] writes ([`RuleId::of_name`]).
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
    /// The record is closed.
    Closed,
    /// Another write or the host's current mode withholds the record: tried again at the next
    /// break with a grant.
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
