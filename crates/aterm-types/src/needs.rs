// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NEED WORDS (`docs/DESIGN-owner-surfaces-2026-10-16.md` §2.2).

/// How a need is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Nothing moves until the owner acts: the stop tone.
    Stuck,
    /// The owner's decision is awaited: the wait tone.
    Decide,
}

impl Severity {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Stuck => "stuck",
            Self::Decide => "decide",
        }
    }
}

/// What a session needs of its owner now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NeedWord {
    /// The program is not reading its input (the server's input stall).
    Frozen,
    /// The program's job is stopped with input queued.
    Stopped,
    /// The task record is `stalled` (never started, held where nothing moves).
    Stalled,
    /// Logged out: only a person can log in, supervised or not.
    Login,
    /// Claude Code's critical-memory banner, and the host will not restart it.
    Memory,
    /// Any other wall nobody answers.
    Wall,
    /// A keyed attention entry of kind ask: someone asked for the owner.
    Ask,
    /// An approval box no harness presses.
    Approval,
    /// A question no harness answers.
    Question,
}

impl NeedWord {
    /// Every word, in precedence order.
    pub const ALL: [Self; 9] = [
        Self::Frozen,
        Self::Stopped,
        Self::Stalled,
        Self::Login,
        Self::Memory,
        Self::Wall,
        Self::Ask,
        Self::Approval,
        Self::Question,
    ];

    /// The wire word (`status needs=`, `survey needs=`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Frozen => "frozen",
            Self::Stopped => "stopped",
            Self::Stalled => "stalled",
            Self::Login => "login",
            Self::Memory => "memory",
            Self::Wall => "wall",
            Self::Ask => "ask",
            Self::Approval => "approval",
            Self::Question => "question",
        }
    }

    /// The word a wire field names, or `None`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|need| need.word() == word)
    }

    /// How the need is shown.
    #[must_use]
    pub const fn severity(self) -> Severity {
        match self {
            Self::Frozen
            | Self::Stopped
            | Self::Stalled
            | Self::Login
            | Self::Memory
            | Self::Wall => Severity::Stuck,
            Self::Ask | Self::Approval | Self::Question => Severity::Decide,
        }
    }
}

/// What aterm is waiting out for a session: a record shown in pull-only rows, never a need.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WaitWord {
    /// A usage limit.
    Limit,
    /// The API cannot be reached: the network coming back answers it.
    Api,
    /// The service is overloaded and the turn is retried.
    Overloaded,
    /// Claude Code's critical-memory banner, and the host restarts the agent at its next idle
    /// point.
    MemoryRestart,
    /// Claude Code's lost login, which the harness carries (an account move, or its own `/login`).
    Login,
    /// An attention entry its writer marked `kind=wait`
    /// (`meta set attention owner=<k> kind=wait <text>`), with no wall on the screen.
    Entry,
}

impl WaitWord {
    /// Every word.
    pub const ALL: [Self; 6] = [
        Self::Limit,
        Self::Api,
        Self::Overloaded,
        Self::MemoryRestart,
        Self::Login,
        Self::Entry,
    ];

    /// The word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Limit => "limit",
            Self::Api => "api",
            Self::Overloaded => "overloaded",
            Self::MemoryRestart => "memory-restart",
            Self::Login => "login",
            Self::Entry => "entry",
        }
    }

    /// The word a wire field names, or `None`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|wait| wait.word() == word)
    }
}

/// Whose hold stands on a session (`hold_by=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HoldWord {
    /// The person at the window took the tab over.
    Person,
    /// Another Mac's node took it over the fleet (the step-in's `hold`).
    Remote,
    /// An Owner client's `hold … on`.
    Local,
    /// The message bus's bridge halted it.
    Bridge,
}

impl HoldWord {
    /// Every word, the person's first: where several slots stand, the first is the one shown.
    pub const ALL: [Self; 4] = [Self::Person, Self::Remote, Self::Local, Self::Bridge];

    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Remote => "remote",
            Self::Local => "local",
            Self::Bridge => "bridge",
        }
    }

    /// The word a wire field names, or `None`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|hold| hold.word() == word)
    }
}

/// What a keyed attention entry is (`meta set attention owner=<k> kind=<ask|wait> <text>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttentionKind {
    /// The owner is asked for: a need.
    Ask,
    /// The writer is waiting something out by itself (the harness at a limit it continues after).
    Wait,
}

impl AttentionKind {
    /// Every kind.
    pub const ALL: [Self; 2] = [Self::Ask, Self::Wait];

    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Wait => "wait",
        }
    }

    /// The kind a wire word names, or `None`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.word() == word)
    }
}

#[cfg(test)]
#[path = "needs_tests.rs"]
mod tests;
