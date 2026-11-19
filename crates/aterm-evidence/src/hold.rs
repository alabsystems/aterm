// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How a hold ends ([`HoldBound`]): the vocabulary the app-update lane and the vendor-upgrade lane
//! share, so every hold either lapses by a bound or lasts only while what holds it is true now.

use std::time::Duration;

/// How a hold is bounded. Two kinds may last, and only while they are witnessed again at each
/// look: a person's state that is true now, and work that is making progress now. Every other
/// hold has a time past which its lane acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldBound {
    /// It lifts within this long of its start, whatever anyone does.
    Within(Duration),
    /// It lifts at an event this program makes by itself: a worker's answer, a timer, a check,
    /// the end of its own work. A program hold whose event never comes is what a bug looks like,
    /// so past `backstop` its lane acts on it; what that act is (a journal record and the hold
    /// derived again, say) is the lane's.
    Program {
        /// How long the hold may stand before its lane acts on it.
        backstop: Duration,
    },
    /// A person's own state that is true now; it holds only while that state is, never on how
    /// long ago a window took an event or whether anyone is at the Mac.
    PersonNow(PersonHold),
    /// Work that is making progress now; it holds only while a fresh witness shows it progressing.
    WorkNow(WorkHold),
}

impl HoldBound {
    /// The time past which the hold's lane acts: the `Within` bound or the `Program` backstop.
    /// `None` for a person's state or progressing work, which hold only while witnessed again.
    #[must_use]
    pub const fn horizon(self) -> Option<Duration> {
        match self {
            Self::Within(bound) => Some(bound),
            Self::Program { backstop } => Some(backstop),
            Self::PersonNow(_) | Self::WorkNow(_) => None,
        }
    }
}

/// A person's own state that is true now, the only thing that may hold a lane on a person
/// ([`HoldBound::PersonNow`]): a Command Line Tools tab waiting on them, a draft they typed and
/// have not applied, or a choice not yet saved. Each holds only while it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonHold {
    /// A Mac Setup tab that cannot travel with the handoff, its password prompt waiting on them
    /// while they are at it: a Command Line Tools install's one nobody touched for
    /// aterm-gui's `clt_install::UNTOUCHED_PROMPT_BACKSTOP` is stopped by an update's apply.
    DeveloperTools,
    /// Their draft in the launch's setup offer.
    SetupDraft,
    /// A setup choice they (or a client) made that the setup worker has not taken yet, or a
    /// Restore they started that it has not finished.
    SetupChoice,
}

impl PersonHold {
    /// The bound as a line says it: whose state it is, and that it holds only while it is.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::DeveloperTools => {
                " (a person's own setup tab: it holds only while it runs, and a Command Line \
                 Tools install's password prompt only while someone answers it)"
            }
            Self::SetupDraft => " (a person's own draft: it holds only while it is there)",
            Self::SetupChoice => {
                " (a choice someone made and not yet saved: it holds only while it is unsaved)"
            }
        }
    }
}

/// Work that is making progress now ([`HoldBound::WorkNow`]): it holds only while a witness
/// within its lane's progress horizon shows it progressing, never on a spinner or a process
/// listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkHold {
    /// A running turn: its output, its transcript, its token counters or its tools' CPU grow.
    Turn,
    /// Background work: its processes' CPU time or bytes written grow.
    Background,
}

impl WorkHold {
    /// The bound as a line says it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Turn => " (a running turn: it holds only while it is seen making progress)",
            Self::Background => {
                " (background work: it holds only while it is seen making progress)"
            }
        }
    }
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;
