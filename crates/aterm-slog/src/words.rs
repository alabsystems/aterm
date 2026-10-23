// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed words for wall kinds, approval boxes, outcomes per level, and needs.

use crate::vocab::{BOX_WORDS, NEED_WORDS, OUT_WORDS, UNKNOWN, WALL_WORDS, lookup};

/// A wall kind (`aterm_phase::WallKind::name()`'s words).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WallKind {
    /// `usage-session`.
    UsageSession,
    /// `usage-weekly`.
    UsageWeekly,
    /// `model-bucket`.
    ModelBucket,
    /// `spend`.
    Spend,
    /// `context`.
    Context,
    /// `auth`.
    Auth,
    /// `api-error`.
    ApiError,
    /// `overloaded`.
    Overloaded,
    /// `memory`.
    Memory,
}

impl WallKind {
    /// Every kind, in [`WALL_WORDS`] order.
    pub const ALL: [WallKind; 9] = [
        WallKind::UsageSession,
        WallKind::UsageWeekly,
        WallKind::ModelBucket,
        WallKind::Spend,
        WallKind::Context,
        WallKind::Auth,
        WallKind::ApiError,
        WallKind::Overloaded,
        WallKind::Memory,
    ];

    /// The bare kind (`usage-session`), as `box=`/`wall=` print it.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            WallKind::UsageSession => "usage-session",
            WallKind::UsageWeekly => "usage-weekly",
            WallKind::ModelBucket => "model-bucket",
            WallKind::Spend => "spend",
            WallKind::Context => "context",
            WallKind::Auth => "auth",
            WallKind::ApiError => "api-error",
            WallKind::Overloaded => "overloaded",
            WallKind::Memory => "memory",
        }
    }

    /// `wall:<kind>`, as `out=` and `need=` write it (`WallKind::wire()`).
    #[inline]
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            WallKind::UsageSession => "wall:usage-session",
            WallKind::UsageWeekly => "wall:usage-weekly",
            WallKind::ModelBucket => "wall:model-bucket",
            WallKind::Spend => "wall:spend",
            WallKind::Context => "wall:context",
            WallKind::Auth => "wall:auth",
            WallKind::ApiError => "wall:api-error",
            WallKind::Overloaded => "wall:overloaded",
            WallKind::Memory => "wall:memory",
        }
    }

    /// The kind a bare name (`usage-session`) or a wire word (`wall:usage-session`) names.
    #[must_use]
    pub fn from_name(s: &str) -> Option<WallKind> {
        let bare = s.strip_prefix("wall:").unwrap_or(s);
        WallKind::ALL.into_iter().find(|k| k.name() == bare)
    }

    /// A usage window, a model bucket or spend: the walls the clause names `limit`.
    #[inline]
    #[must_use]
    pub const fn is_limit(self) -> bool {
        matches!(
            self,
            WallKind::UsageSession
                | WallKind::UsageWeekly
                | WallKind::ModelBucket
                | WallKind::Spend
        )
    }
}

/// An approval box word (`box=`): a `PromptKind` word, Bash with its read-only verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BoxWord(&'static str);

impl BoxWord {
    /// The AskUserQuestion dialog's kind: a `question` step, not an approval.
    pub const QUESTION: BoxWord = BoxWord("question");
    /// The generic box.
    pub const OTHER: BoxWord = BoxWord("other");

    /// The box the screen verdict's detail names (`bash:not-read-only`, `edit`, `question`).
    #[must_use]
    pub fn from_detail(detail: &str) -> BoxWord {
        lookup(BOX_WORDS, detail).map_or(BoxWord::OTHER, BoxWord)
    }

    /// A decoded word: unknown stays `unknown` (a reader never guesses).
    #[must_use]
    pub fn from_wire(word: &str) -> BoxWord {
        BoxWord(lookup(BOX_WORDS, word).unwrap_or(UNKNOWN))
    }

    /// The `box=` word.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        self.0
    }

    /// The kind without Bash's read-only verdict (`bash`).
    #[inline]
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self.0 {
            "bash:read-only" | "bash:not-read-only" => "bash",
            other => other,
        }
    }

    /// Whether this is the question dialog.
    #[inline]
    #[must_use]
    pub fn is_question(self) -> bool {
        self.0 == "question"
    }

    /// The need this box raises: `approval:<kind>`, the question dialog's `question`.
    #[must_use]
    pub fn need(self) -> &'static str {
        if self.is_question() {
            return "question";
        }
        let mut want = String::with_capacity(32);
        want.push_str("approval:");
        want.push_str(self.kind());
        lookup(NEED_WORDS, &want).unwrap_or("approval:other")
    }
}

/// A run's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RunOut {
    /// Exit status 0.
    Ok,
    /// A nonzero exit status.
    Fail,
    /// Killed by a signal.
    Signal,
    /// It went away with its parent, or unobserved.
    Gone,
    /// A newer run replaced it.
    Superseded,
    /// A more reliable source contradicted it.
    Retracted,
    /// Not known.
    Unknown,
}

/// A turn's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TurnOut {
    /// The agent came to rest.
    Done,
    /// The agent asked in prose (`conf=heuristic`).
    Asked,
    /// The turn ended on a wall.
    Wall(WallKind),
    /// The turn was interrupted.
    Interrupted,
    /// An API error ended it.
    Error,
    /// The agent left.
    Gone,
    /// A new turn began while it was open.
    Superseded,
    /// A more reliable source contradicted it.
    Retracted,
    /// Not known.
    Unknown,
}

/// A step's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StepOut {
    /// A tool step that succeeded.
    Ok,
    /// A tool step that failed.
    Error,
    /// The box was approved.
    Approved,
    /// The box was declined.
    Declined,
    /// The question was answered.
    Answered,
    /// The dialog or survey was dismissed.
    Dismissed,
    /// The screen moved on; how is not known.
    Passed,
    /// The step was interrupted.
    Interrupted,
    /// It went away with its parent.
    Gone,
    /// A more reliable source contradicted it.
    Retracted,
    /// Not known: a journal read found the step open and never closed.
    Unknown,
}

/// A session's outcome: `ExitReason`'s tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionOut {
    /// `shell-exit`.
    ShellExit,
    /// `ctl-close`.
    CtlClose,
    /// `ui-close`.
    UiClose,
    /// `window-close`.
    WindowClose,
    /// `app-quit`.
    AppQuit,
    /// The process went away (a synthetic close).
    Gone,
    /// The close path did not say.
    Unknown,
}

/// An outcome at the level of the node it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Out {
    /// A run's.
    Run(RunOut),
    /// A turn's.
    Turn(TurnOut),
    /// A step's.
    Step(StepOut),
    /// The session's.
    Session(SessionOut),
}

impl Out {
    /// The `out=` word (one of [`OUT_WORDS`]).
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Out::Run(o) => match o {
                RunOut::Ok => "ok",
                RunOut::Fail => "fail",
                RunOut::Signal => "signal",
                RunOut::Gone => "gone",
                RunOut::Superseded => "superseded",
                RunOut::Retracted => "retracted",
                RunOut::Unknown => "unknown",
            },
            Out::Turn(o) => match o {
                TurnOut::Done => "done",
                TurnOut::Asked => "asked",
                TurnOut::Wall(k) => k.wire(),
                TurnOut::Interrupted => "interrupted",
                TurnOut::Error => "error",
                TurnOut::Gone => "gone",
                TurnOut::Superseded => "superseded",
                TurnOut::Retracted => "retracted",
                TurnOut::Unknown => "unknown",
            },
            Out::Step(o) => match o {
                StepOut::Ok => "ok",
                StepOut::Error => "error",
                StepOut::Approved => "approved",
                StepOut::Declined => "declined",
                StepOut::Answered => "answered",
                StepOut::Dismissed => "dismissed",
                StepOut::Passed => "passed",
                StepOut::Interrupted => "interrupted",
                StepOut::Gone => "gone",
                StepOut::Retracted => "retracted",
                StepOut::Unknown => "unknown",
            },
            Out::Session(o) => match o {
                SessionOut::ShellExit => "shell-exit",
                SessionOut::CtlClose => "ctl-close",
                SessionOut::UiClose => "ui-close",
                SessionOut::WindowClose => "window-close",
                SessionOut::AppQuit => "app-quit",
                SessionOut::Gone => "gone",
                SessionOut::Unknown => "unknown",
            },
        }
    }

    /// The level whose nodes this outcome closes.
    #[inline]
    #[must_use]
    pub const fn lvl(self) -> crate::vocab::Lvl {
        match self {
            Out::Run(_) => crate::vocab::Lvl::Run,
            Out::Turn(_) => crate::vocab::Lvl::Turn,
            Out::Step(_) => crate::vocab::Lvl::Step,
            Out::Session(_) => crate::vocab::Lvl::Session,
        }
    }

    /// The outcome of a node closed because its parent closed.
    #[inline]
    #[must_use]
    pub const fn gone(lvl: crate::vocab::Lvl) -> Out {
        match lvl {
            crate::vocab::Lvl::Run => Out::Run(RunOut::Gone),
            crate::vocab::Lvl::Turn => Out::Turn(TurnOut::Gone),
            crate::vocab::Lvl::Step => Out::Step(StepOut::Gone),
            crate::vocab::Lvl::Session | crate::vocab::Lvl::Instance => {
                Out::Session(SessionOut::Gone)
            }
        }
    }
}

/// Precedence rank of a need word.
#[must_use]
pub fn need_rank(word: &str) -> u8 {
    match word {
        w if w.starts_with("approval:") => 0,
        "question" => 1,
        w if w.starts_with("wall:") => 2,
        "unresponsive" => 3,
        "failure" => 4,
        "escalated" => 5,
        "exited" => 6,
        _ => 7,
    }
}

/// Whether `word` is an `out=` word at all (decode keeps an unknown one as `unknown`).
#[must_use]
pub fn is_out_word(word: &str) -> bool {
    lookup(OUT_WORDS, word).is_some()
}

/// The `wall=` word a wall kind's bare name is, checked against [`WALL_WORDS`].
#[must_use]
pub fn wall_word(kind: WallKind) -> &'static str {
    lookup(WALL_WORDS, kind.name()).unwrap_or(UNKNOWN)
}
