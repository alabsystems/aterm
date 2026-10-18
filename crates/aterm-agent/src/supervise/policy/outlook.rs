// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The outlook of a wall ([`outlook`]): the one table the policy and whatever
//! shows a wall (the tab chip, `status`) both read, so the two cannot
//! disagree about whether a person is needed. Pure, total over [`WallKind`].
//!
//! [`ExceptionKind`] names the points only a person can answer;
//! [`super::exception`] raises them with their why and their one action.

use aterm_phase::wall::WallKind;

use super::super::config::SupervisorConfig;

/// Which exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ExceptionKind {
    /// The login is gone: only a browser sign-in restores it.
    Login,
    /// A spend limit, a credit balance or an account the admin switched off:
    /// no time window ends it.
    Spend,
    /// A model bucket no reset ends (`requires usage credits`) whose fallback
    /// the tab cannot make: it waits on the tab's own running jobs, or there
    /// is no other model.
    Bucket,
    /// Claude dropped the `/goal` after an unrecoverable error.
    GoalCleared,
    /// A wall with a reset that keeps answering the continuation after it.
    Futile,
    /// A full context `/compact` cannot lower, by Claude Code's own word after
    /// the third `/compact`. Going back is a person's, done in the tab, so the
    /// host files it per tab.
    Context,
    /// A request the API refuses the same way each time it is sent
    /// ([`aterm_phase::ApiCause::Request`]: a safeguards refusal, a model not
    /// found, a tool history it rejects): the supervisor's slow ladder goes
    /// on, and what ends it is a person's change in the tab (`/rewind`,
    /// `/model`, a rephrased message), so the host files it per tab.
    Refused,
    /// A screen no reader names, busy on background work alone for hours while
    /// Claude's title says idle ([`super::exception::Exception::stalled`]).
    Stalled,
    /// A dialog no reader names, unchanged for an hour while Claude's title
    /// says idle ([`super::exception::Exception::dialog`]). Its own kind, not
    /// a `Stalled`: dismissing one must not silence the other in the same tab,
    /// and the action differs.
    Dialog,
}

impl ExceptionKind {
    /// The one word the journal and the digest key by.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Spend => "spend",
            Self::Bucket => "bucket",
            Self::GoalCleared => "goal-cleared",
            Self::Futile => "futile",
            Self::Context => "context",
            Self::Refused => "refused",
            Self::Stalled => "stalled",
            Self::Dialog => "dialog",
        }
    }
}

/// What becomes of a wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outlook {
    /// The supervisor continues it by itself, at the wall's reset when it
    /// names one, else on its ladder. Nobody is needed; the mark says when.
    Resumes,
    /// Claude Code continues it by itself (its auto-continue is armed); the
    /// supervisor is the net under it.
    VendorResumes,
    /// The supervisor tries and a person can lift it faster
    /// ([`ExceptionKind`]): a digest item, never an interruption.
    ResumesAndAsks(ExceptionKind),
    /// A person is needed; nothing the supervisor types lifts it.
    Person(ExceptionKind),
    /// Nothing supervises this wall here: the owner switched the key off,
    /// or the session has no supervisor.
    Unattended,
}

/// The outlook of a wall of `kind` whose notice `resumes_by_itself` says
/// Claude Code goes on alone, under `cfg`, `supervised` when a loop watches
/// the session. Total over [`WallKind`]: a new kind does not compile until it
/// is answered here.
#[must_use]
pub fn outlook(
    kind: WallKind,
    has_reset: bool,
    resumes_by_itself: bool,
    cfg: &SupervisorConfig,
    supervised: bool,
) -> Outlook {
    if !supervised {
        return Outlook::Unattended;
    }
    match kind {
        WallKind::UsageSession | WallKind::UsageWeekly if !cfg.resume_limits => Outlook::Unattended,
        WallKind::UsageSession | WallKind::UsageWeekly if resumes_by_itself => {
            Outlook::VendorResumes
        }
        WallKind::UsageSession | WallKind::UsageWeekly => Outlook::Resumes,
        WallKind::Spend if !cfg.resume_limits => Outlook::Person(ExceptionKind::Spend),
        WallKind::Spend if has_reset => Outlook::Resumes,
        WallKind::Spend => Outlook::ResumesAndAsks(ExceptionKind::Spend),
        // A bucket with a reset ends at it; one without ends when a person
        // adds credits or picks another model (or, if the tab can switch by
        // itself, when its jobs end): a digest item, and the chip says so.
        WallKind::ModelBucket { .. } if has_reset => Outlook::Resumes,
        WallKind::ModelBucket { .. } => Outlook::ResumesAndAsks(ExceptionKind::Bucket),
        // A refused request: its slow ladder goes on, and a person's change
        // in the tab ends it sooner (a digest item, never an interruption).
        WallKind::ApiError {
            cause: aterm_phase::ApiCause::Request,
            ..
        } if cfg.retry_api_errors => Outlook::ResumesAndAsks(ExceptionKind::Refused),
        WallKind::ApiError { .. } | WallKind::Overloaded if cfg.retry_api_errors => {
            Outlook::Resumes
        }
        WallKind::ApiError { .. } | WallKind::Overloaded => Outlook::Unattended,
        WallKind::Context if cfg.compact_on_context_wall => Outlook::Resumes,
        WallKind::Context => Outlook::Unattended,
        WallKind::Auth => Outlook::Person(ExceptionKind::Login),
        WallKind::Memory if cfg.relaunch => Outlook::Resumes,
        WallKind::Memory => Outlook::Unattended,
    }
}

/// What a surface that shows a wall (the presence row, the tab chip) needs of
/// its [`Outlook`]: who answers it. A wall the supervisor resumes is a record;
/// only a wall a person must (or can) act on says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Nobody: a person is needed, or nothing supervises the wall here.
    Nobody,
    /// The supervisor (or Claude Code, with the supervisor the net under it)
    /// resumes it by itself.
    Resumes,
    /// The supervisor resumes it, slowly, and a person can lift it faster:
    /// the one thing to do, as a short static phrase for the row.
    ResumesAsks(&'static str),
}

/// The [`Answer`] to a wall of `kind` whose notice names a reset (`has_reset`)
/// or not, under `cfg`, `supervised` by this process's own harness. A
/// critical-memory banner is nobody's: it is drawn under a running spinner, the
/// one place the supervisor never decides (`status_item::outlives_supervision`),
/// so the stop stays for a person to see whatever `relaunch` says.
#[must_use]
pub fn answer(kind: WallKind, has_reset: bool, cfg: &SupervisorConfig, supervised: bool) -> Answer {
    if kind == WallKind::Memory {
        return Answer::Nobody;
    }
    match outlook(kind, has_reset, false, cfg, supervised) {
        Outlook::Resumes | Outlook::VendorResumes => Answer::Resumes,
        Outlook::ResumesAndAsks(ExceptionKind::Bucket) => Answer::ResumesAsks("/model to switch"),
        Outlook::ResumesAndAsks(ExceptionKind::Spend) => Answer::ResumesAsks("add credits"),
        Outlook::ResumesAndAsks(ExceptionKind::Refused) => Answer::ResumesAsks("fix in the tab"),
        Outlook::ResumesAndAsks(_) => Answer::Resumes,
        Outlook::Person(_) | Outlook::Unattended => Answer::Nobody,
    }
}

#[cfg(test)]
#[path = "outlook_tests.rs"]
mod tests;
