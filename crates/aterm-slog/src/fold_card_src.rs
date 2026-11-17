// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHERE A CARD'S TITLE AND REASON CAME FROM (the comprehension design §2.3, "Where they came
//! from"): two closed words, `title_src` and `matters_src`, each read from the standing claim's
//! record (its author's rank, `src`, `conf` and `as=`) and never from its text, so the log gains
//! no key. aterm writes the `as=` words itself: its default writer names the rung a title came
//! from, and a spawn's birth claims name where a reason came from.
//!
//! A BIRTH CLAIM is one aterm wrote at a session's open from what its spawn said: the caller's
//! own claim (`src=mark conf=exact`, which no client can write: the `mark` verb writes
//! `conf=claim`), or aterm's derived one (`by=aterm src=aterm conf=exact`) naming its source.
//! [`is_birth`] tells one apart from a later claim.

use super::desc::{DescClaim, Rank, Shown};
use super::{SessionState, shown};
use crate::vocab::{Conf, Src};

/// A title from the session's task's first line.
pub const TITLE_AS_TASK: &str = "task";
/// A title from an agent's first prompt the harness did not type.
pub const TITLE_AS_PROMPT: &str = "prompt";
/// A title from the program (with the folder, or alone).
pub const TITLE_AS_PROGRAM: &str = "program";
/// A title from the folder alone.
pub const TITLE_AS_FOLDER: &str = "folder";
/// A reason from the spawn's own `why=` line.
pub const MATTERS_AS_WHY: &str = "why";
/// A reason copied from the creator's standing reason at the open.
pub const MATTERS_AS_CREATOR: &str = "creator";
/// A reason from the tracker package the session names.
pub const MATTERS_AS_PACKAGE: &str = "package";
/// A `package` from the spawn's own `package=` word.
pub const PACKAGE_AS_SPAWN: &str = "spawn";

/// WHERE THE STANDING TITLE CAME FROM (`title_src`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TitleSrc {
    /// The task's first line, written at the open or by aterm's default writer.
    Task,
    /// An agent's first prompt the harness did not type.
    Prompt,
    /// The program and folder, or the program alone, or aterm's constant `shell`.
    Program,
    /// The folder alone.
    Folder,
    /// Any claim made after birth: a person's, a program's, a writer's, a control client's, an
    /// in-band title.
    Stated,
}

impl TitleSrc {
    /// Every word, in the order the design lists them.
    pub const ALL: [TitleSrc; 5] = [
        TitleSrc::Task,
        TitleSrc::Prompt,
        TitleSrc::Program,
        TitleSrc::Folder,
        TitleSrc::Stated,
    ];

    /// The word a reader prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            TitleSrc::Task => TITLE_AS_TASK,
            TitleSrc::Prompt => TITLE_AS_PROMPT,
            TitleSrc::Program => TITLE_AS_PROGRAM,
            TitleSrc::Folder => TITLE_AS_FOLDER,
            TitleSrc::Stated => "stated",
        }
    }

    /// The word [`TitleSrc::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<TitleSrc> {
        TitleSrc::ALL.into_iter().find(|s| s.word() == word)
    }

    /// THE SOURCE OF A STANDING TITLE, from its record alone. aterm's derived claim names its
    /// rung in `as=`: `task`, `prompt` and `folder` read so, and `program`, no `as=` (the
    /// constant `shell`) or any other name read [`TitleSrc::Program`]. A creator's or a control
    /// client's claim made at birth (`src=mark conf=exact as=task`) reads [`TitleSrc::Task`].
    /// Every other claim, in-band bytes included, reads [`TitleSrc::Stated`].
    #[must_use]
    pub fn of(shown: &Shown<'_>) -> TitleSrc {
        let claim = shown.claim;
        let named = claim.as_name.as_deref();
        match shown.rank {
            Rank::Derived => match named {
                Some(TITLE_AS_TASK) => TitleSrc::Task,
                Some(TITLE_AS_PROMPT) => TitleSrc::Prompt,
                Some(TITLE_AS_FOLDER) => TitleSrc::Folder,
                _ => TitleSrc::Program,
            },
            Rank::Creator | Rank::Ctl if stated_at_birth(claim) && named == Some(TITLE_AS_TASK) => {
                TitleSrc::Task
            }
            _ => TitleSrc::Stated,
        }
    }
}

/// WHERE THE STANDING REASON CAME FROM (`matters_src`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MattersSrc {
    /// A person's (rank 1).
    Person,
    /// The session's own program's (rank 2).
    Program,
    /// The creator's: a `why=` at birth or one copied from the creator, at any rank, or a later
    /// claim of the creator's or a control client's (rank 3 or 5).
    Creator,
    /// aterm's line from the tracker package the session names.
    Package,
    /// A writer aterm ran (rank 4).
    Writer,
    /// Nothing stands: the card reads `no reason stated`.
    Unset,
}

impl MattersSrc {
    /// Every word, in the order the design lists them.
    pub const ALL: [MattersSrc; 6] = [
        MattersSrc::Person,
        MattersSrc::Program,
        MattersSrc::Creator,
        MattersSrc::Package,
        MattersSrc::Writer,
        MattersSrc::Unset,
    ];

    /// The word a reader prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            MattersSrc::Person => "person",
            MattersSrc::Program => "program",
            MattersSrc::Creator => "creator",
            MattersSrc::Package => "package",
            MattersSrc::Writer => "writer",
            MattersSrc::Unset => "none",
        }
    }

    /// The word [`MattersSrc::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<MattersSrc> {
        MattersSrc::ALL.into_iter().find(|s| s.word() == word)
    }

    /// THE SOURCE OF A STANDING REASON, from its record alone: by its author's rank, aterm's
    /// derived reason by its `as=` (`package` reads [`MattersSrc::Package`]; a `why=` or one
    /// copied from the creator, or any other, [`MattersSrc::Creator`]). Nothing standing reads
    /// [`MattersSrc::Unset`], as in-band bytes and a listed author do, which never stand for it.
    #[must_use]
    pub fn of(shown: Option<&Shown<'_>>) -> MattersSrc {
        let Some(shown) = shown else {
            return MattersSrc::Unset;
        };
        match shown.rank {
            Rank::Person => MattersSrc::Person,
            Rank::Program => MattersSrc::Program,
            Rank::Creator | Rank::Ctl => MattersSrc::Creator,
            Rank::Writer => MattersSrc::Writer,
            Rank::Derived if shown.claim.as_name.as_deref() == Some(MATTERS_AS_PACKAGE) => {
                MattersSrc::Package
            }
            Rank::Derived => MattersSrc::Creator,
            Rank::Inband | Rank::Listed => MattersSrc::Unset,
        }
    }
}

/// Whether `claim` is its author's own claim made at birth: a mark's source at `conf=exact`.
fn stated_at_birth(claim: &DescClaim) -> bool {
    claim.src == Src::Mark && claim.conf == Conf::Exact
}

/// WHETHER `claim`, of an author of `rank`, IS A BIRTH CLAIM: a creator's or a control client's
/// own at `src=mark conf=exact`, or aterm's derived one at `conf=exact` naming a spawn's word
/// (`as=why`, `creator` or `spawn`). aterm's default writer's claims are `conf=heuristic`, and
/// every later claim of a client `conf=claim`.
#[must_use]
pub fn is_birth(claim: &DescClaim, rank: Rank) -> bool {
    match rank {
        Rank::Creator | Rank::Ctl => stated_at_birth(claim),
        Rank::Derived => {
            claim.conf == Conf::Exact
                && matches!(
                    claim.as_name.as_deref(),
                    Some(MATTERS_AS_WHY | MATTERS_AS_CREATOR | PACKAGE_AS_SPAWN)
                )
        }
        Rank::Person | Rank::Program | Rank::Writer | Rank::Inband | Rank::Listed => false,
    }
}

/// The source of the title a reader is shown at `now_wall` ([`shown`]): `None` where none stands,
/// which a reader prints `-`.
#[must_use]
pub fn title_src(st: &SessionState, now_wall: u64) -> Option<TitleSrc> {
    shown(st, "title", now_wall).map(|s| TitleSrc::of(&s))
}

/// The source of the reason a reader is shown at `now_wall` ([`shown`]).
#[must_use]
pub fn matters_src(st: &SessionState, now_wall: u64) -> MattersSrc {
    MattersSrc::of(shown(st, "matters", now_wall).as_ref())
}
