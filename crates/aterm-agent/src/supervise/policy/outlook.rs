// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The outlook of a wall ([`outlook`]): the one table the policy and whatever
//! shows a wall (the tab chip, `status`) both read, so the two cannot
//! disagree about whether a person is needed.

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
    /// the third `/compact`.
    Context,
    /// A request the API refuses the same way each time it is sent
    /// ([`aterm_phase::ApiCause::Request`]: a safeguards refusal, a model not
    /// found, a tool history it rejects): the supervisor's slow ladder goes
    /// on, and what ends it is a person's change in the tab (`/rewind`,
    /// `/model`, a rephrased message), so the host files it per tab.
    Refused,
    /// A screen no reader names, busy on background work alone for hours while
    /// Claude's title says idle ([`super::exception::Exception::stalled`]);
    /// and agy's or Muse Code's own background work, run for hours under
    /// words that stood still ([`super::exception::Exception::stalled_on_work`]).
    Stalled,
    /// A dialog no reader names, unchanged for an hour while Claude's title
    /// says idle ([`super::exception::Exception::dialog`]); and Claude Code's
    /// own Settings panel, which a reader names, left open as long by a hand
    /// other than the harness's ([`super::exception::Exception::settings_panel`]).
    Dialog,
}

impl ExceptionKind {
    /// The one word the journal and the digest key by.
    #[must_use]
    pub const fn word(self) -> &'static str {
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
    /// names one, else on its ladder.
    Resumes,
    /// Claude Code continues it by itself (its auto-continue is armed); the
    /// supervisor is the net under it.
    VendorResumes,
    /// The supervisor tries and a person can lift it faster
    /// ([`ExceptionKind`]): a digest item, never an interruption.
    ResumesAndAsks(ExceptionKind),
    /// The supervisor carries it to the one step that is the owner's, if
    /// any: Claude Code's lost login, which an account move or the loop's own
    /// `/login` signs back in, where only the browser sign-in is the owner's
    /// — and its notice asks for that then ([`super::notice`]), so nothing is
    /// the owner's before it stands.
    Carries(ExceptionKind),
    /// A person is needed; nothing the supervisor types lifts it.
    Person(ExceptionKind),
    /// Nothing supervises this wall here: the owner switched the key off,
    /// or the session has no supervisor.
    Unattended,
}

/// The outlook of a wall of `kind` whose notice `resumes_by_itself` says
/// Claude Code goes on alone, under `cfg`, `supervised` when a loop watches
/// the session.
#[must_use]
pub const fn outlook(
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
        WallKind::ModelBucket { .. } if has_reset => Outlook::Resumes,
        WallKind::ModelBucket { .. } => Outlook::ResumesAndAsks(ExceptionKind::Bucket),
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
        WallKind::Auth if cfg.retry_api_errors => Outlook::Carries(ExceptionKind::Login),
        WallKind::Auth => Outlook::Person(ExceptionKind::Login),
        WallKind::Memory if cfg.relaunch => Outlook::Resumes,
        WallKind::Memory => Outlook::Unattended,
    }
}

/// What a surface that shows a wall (the needs border's pill, the tab chip) needs of
/// its [`Outlook`]: who answers it.
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
/// or not, under `cfg`, `supervised` by this process's own harness.
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
        Outlook::ResumesAndAsks(_) | Outlook::Carries(_) => Answer::Resumes,
        Outlook::Person(_) | Outlook::Unattended => Answer::Nobody,
    }
}

/// What becomes of a session usage credits carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreditsOutlook {
    /// `[harness] usage_credits` on: credits are work going on, and the
    /// supervisor carries the session on them as on its subscription.
    Carried,
    /// Off (the default), the window they carry past named: the supervisor
    /// types nothing until it resets, when Claude Code goes back onto the
    /// subscription by itself, and then goes on.
    WaitsForReset,
    /// Off, no reset named: the supervisor types nothing, and a person
    /// decides — their own turns go on credits, or `[harness]
    /// usage_credits = true` carries the session on them.
    Person,
    /// Nothing supervises the session here.
    Unattended,
}

impl CreditsOutlook {
    /// What a surface that shows the session says of it, a short static
    /// phrase; `None` where the session goes on as any other.
    #[must_use]
    pub const fn phrase(self) -> Option<&'static str> {
        match self {
            Self::WaitsForReset => Some("on usage credits: waits for the subscription's reset"),
            Self::Person => Some("on usage credits: held, no reset named"),
            Self::Carried | Self::Unattended => None,
        }
    }
}

/// The [`CreditsOutlook`] of a session usage credits carry, the window they
/// carry past named (`has_reset`) or not, under `cfg`, `supervised` when a
/// loop watches it.
#[must_use]
pub const fn credits_outlook(
    has_reset: bool,
    cfg: &SupervisorConfig,
    supervised: bool,
) -> CreditsOutlook {
    match (supervised, cfg.usage_credits, has_reset) {
        (false, _, _) => CreditsOutlook::Unattended,
        (true, true, _) => CreditsOutlook::Carried,
        (true, false, true) => CreditsOutlook::WaitsForReset,
        (true, false, false) => CreditsOutlook::Person,
    }
}

#[cfg(test)]
#[path = "outlook_tests.rs"]
mod tests;
