// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel process-tree evidence of stalled background work under a supervised agent.

/// A background call of the session's agent that has made no progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundCall {
    /// The process the row names: the waiting process for a named wait, else the call's leader.
    pub pid: u32,
    /// The call's process group.
    pub pgid: u32,
    /// When the call's leader began (`JobRow::leader`'s start), paired with `pgid` and
    /// [`Self::episode`] in [`crate::supervise::facts::EpisodeKey::of_call`].
    pub birth: Option<u64>,
    /// The engine's suspect episode the reading belongs to (`JobRow::episode`): a run of no
    /// progress opens one, and only progress (the verdict back below quiet's leaving bound) or a
    /// legitimate wait closes it, so a reading that dips names the same one.
    pub episode: u32,
    /// Agent task id for the call (e.g. `bplle5dxj`) when resolved by the host join.
    pub task: Option<String>,
    /// How long it has made no progress, ms.
    pub quiet_ms: u64,
    /// The word of the wait it sleeps in (`aterm_vitals::jobs::WaitName::word`: a Tier B naming's,
    /// else the engine's own), else `None`.
    pub wait: Option<String>,
    /// The process its wait is on, where the engine names one: a lock's holder, a pipe's other end.
    pub held_by: Option<u32>,
}

/// Host jobs reading for the agent's background work
/// ([`crate::supervise::IdleHost::background_jobs`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum BackgroundEvidence {
    /// No reading, a stale one, or one that names no live background call of this session's.
    #[default]
    Unknown,
    /// A live background call of this session's agent progresses or waits on what it declared:
    /// its end wakes the agent, so nothing here is stalled.
    Waiting,
    /// Every live background call of this session's agent has made no progress; `worst` is the
    /// one quiet longest.
    NoProgress { worst: BackgroundCall },
}

impl BackgroundEvidence {
    /// The call that made no progress, if the reading names one.
    #[must_use]
    pub const fn no_progress(&self) -> Option<&BackgroundCall> {
        match self {
            Self::NoProgress { worst } => Some(worst),
            Self::Unknown | Self::Waiting => None,
        }
    }

    /// Whether the reading names a live background call of this session's agent.
    #[must_use]
    pub const fn names_a_call(&self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

const TASK_MAX: usize = 32;

/// Returns `Some(task)` when `task` is a non-empty `[A-Za-z0-9_-]{1,32}` token.
#[must_use]
pub fn task_token(task: &str) -> Option<&str> {
    (!task.is_empty()
        && task.len() <= TASK_MAX
        && task
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then_some(task)
}

/// Maps a wait word (`aterm_vitals::jobs::WaitName::word`) to its phrase, or `None` for a word
/// that says nothing the agent can act on (`unnamed`, `named-unparsed`, an unknown word).
#[must_use]
pub fn wait_phrase(word: &str) -> Option<&'static str> {
    Some(match word {
        "mutex" => "an in-process lock (mutex)",
        "lock" => "a file lock (lock)",
        "timer" => "a timer (timer)",
        "read" => "a read (read)",
        "event-loop" => "an event wait (event-loop)",
        "ipc" => "a Mach message (ipc)",
        "children" => "its children (children)",
        "pipe" => "a pipe nothing writes to (pipe)",
        "pipe-full" => "a full pipe nothing reads (pipe-full)",
        "tcp" => "a network connection (tcp)",
        "kernel" => "the kernel (kernel)",
        _ => return None,
    })
}

impl BackgroundCall {
    /// Whole minutes it has made no progress.
    #[must_use]
    pub const fn quiet_min(&self) -> u64 {
        self.quiet_ms / 60_000
    }

    /// Formats the stall fact as typed into the agent composer: who (the validated task token,
    /// else "a background call"), its pid, its minutes, and what it waits on (the closed-set
    /// [`wait_phrase`] and the pid holding that wait, where the engine names one).
    ///
    /// Never a program name, path, argv, title or output: those are untrusted input on the
    /// prompt channel.
    #[must_use]
    pub fn told(&self) -> String {
        let who = match self.task.as_deref().and_then(task_token) {
            Some(task) => format!("shell task {task}"),
            None => "a background call".to_string(),
        };
        let wait = match (self.wait.as_deref().and_then(wait_phrase), self.held_by) {
            (Some(phrase), Some(holder)) => format!("; it waits on {phrase}, held by pid {holder}"),
            (Some(phrase), None) => format!("; it waits on {phrase}"),
            (None, Some(holder)) => format!("; it waits on pid {holder}"),
            (None, None) => String::new(),
        };
        format!(
            "{who} (pid {}) has made no progress for {} min{wait}",
            self.pid,
            self.quiet_min()
        )
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
