// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DECLARED WAKES AS THE LOOP SEES THEM (`docs/DESIGN-harness-occupancy-2026-10-20.md` §4.1, §8
//! row 2): the view of the book the window's host keeps of the conditions a session asked to be
//! told of (`wake when=…`, [`super::wakes::WakeBook`]), read by the loop through
//! `IdleHost::wakes`, and the closed words of the book, the doors that tell a wake
//! ([`WakeDoor`]) and the host's answer to a claim on one ([`WakeClaim`]).
//!
//! Closed words and plain numbers only: an agent's label, path or pattern never crosses here, and
//! a wake's need crosses as the grammar's own canonical spelling. The empty view is the default
//! host's, and what a replay of a build without the door reads.

use serde::{Deserialize, Serialize};

use super::door::PaceRefusal;

/// THE DOOR A WAKE IS TOLD THROUGH: the one teller its claim names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
    /// Re-read, its need no longer holds (or its condition never held): the line waits.
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

/// WHO DECLARED A WAKE, as its class word: the agent's tree (`yours`), or the owner through
/// Settings ▸ Harness (`owner's`; in the type, with no surface yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Declarer {
    /// A process of the session's own tree.
    Agent,
    /// The owner.
    Owner,
}

impl Declarer {
    /// Every declarer.
    pub const ALL: [Declarer; 2] = [Declarer::Agent, Declarer::Owner];

    /// Its class word, as a told line names it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Declarer::Agent => "yours",
            Declarer::Owner => "owner's",
        }
    }

    /// The declarer `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.word() == word)
    }
}

/// A WAKE'S PRIORITY (`prio=`): a critical wake is told before the others, and passes no pace
/// and no guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Prio {
    /// The default.
    Normal,
    /// `prio=critical`.
    Critical,
}

impl Prio {
    /// Every priority.
    pub const ALL: [Prio; 2] = [Prio::Normal, Prio::Critical];

    /// Its word (`prio=<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Prio::Normal => "normal",
            Prio::Critical => "critical",
        }
    }

    /// The priority `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.word() == word)
    }
}

/// WHAT AN ATOM OF A WAKE'S CONDITION READS, as the view names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AtomKind {
    /// `file:PATH`: the file is there.
    File,
    /// `file:PATH~/RE/`: a line of the file matches.
    Match,
    /// `gone:PATH`: the path is not there.
    Gone,
    /// `pid:N exited`.
    Pid,
    /// `at:HH:MM`.
    At,
    /// `in:DUR`.
    In,
    /// Machine terms of the one grammar (`slots>=1`, `avail>=8GiB`).
    Sys,
}

impl AtomKind {
    /// Every kind.
    pub const ALL: [AtomKind; 7] = [
        AtomKind::File,
        AtomKind::Match,
        AtomKind::Gone,
        AtomKind::Pid,
        AtomKind::At,
        AtomKind::In,
        AtomKind::Sys,
    ];

    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            AtomKind::File => "file",
            AtomKind::Match => "match",
            AtomKind::Gone => "gone",
            AtomKind::Pid => "pid",
            AtomKind::At => "at",
            AtomKind::In => "in",
            AtomKind::Sys => "sys",
        }
    }

    /// The kind `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.word() == word)
    }
}

/// WHY A WAKE EXPIRED.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExpireWhy {
    /// Its `by=` passed.
    Due,
    /// One of its `unless=` atoms held.
    Unless,
    /// The agent process it was declared for is gone.
    Agent,
    /// A path of its could not be opened without following a link.
    Path,
    /// The owner switched occupancy off.
    Off,
    /// Restored, the running build's rules no longer admit it.
    Admit,
}

impl ExpireWhy {
    /// Every reason.
    pub const ALL: [ExpireWhy; 6] = [
        ExpireWhy::Due,
        ExpireWhy::Unless,
        ExpireWhy::Agent,
        ExpireWhy::Path,
        ExpireWhy::Off,
        ExpireWhy::Admit,
    ];

    /// Its word (`why=<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ExpireWhy::Due => "due",
            ExpireWhy::Unless => "unless",
            ExpireWhy::Agent => "agent",
            ExpireWhy::Path => "path",
            ExpireWhy::Off => "off",
            ExpireWhy::Admit => "admit",
        }
    }

    /// The reason `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.word() == word)
    }
}

/// A WAKE'S STATE in the book (§3 D4 of the slice-2 plan). `Pending` until its condition holds,
/// then `Ready`, then told by one door: the agent's `await wake` (`Answering` while the reply is
/// written, then `Answered` in a hosted session until the loop sees a turn run after it, or
/// `Told` at once in an unhosted one), or the loop's break or point (`Claimed` for the write,
/// then `Told`). The rest end it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum WakeState {
    /// Declared; its condition has not held.
    Pending,
    /// Its condition held (latched): due to be told.
    Ready,
    /// An `await wake` reply is being written for it.
    Answering {
        /// When the write began, unix ms.
        at_ms: i64,
    },
    /// The agent's `await wake` was answered; in a hosted session, not yet seen heard.
    Answered {
        /// When the reply was written, unix ms.
        at_ms: i64,
    },
    /// A door of the loop holds it for its one write.
    Claimed {
        /// The door.
        door: WakeDoor,
        /// When it was claimed, unix ms.
        at_ms: i64,
    },
    /// Told.
    Told {
        /// The door that told it.
        door: WakeDoor,
        /// When, unix ms.
        at_ms: i64,
    },
    /// The agent took it (`wake taken #<n>`).
    Taken,
    /// It ended untold.
    Expired {
        /// Why.
        why: ExpireWhy,
    },
}

impl WakeState {
    /// Its word: `pending`, `ready`, `answering`, `answered`, `claimed`, `told`, `taken` or
    /// `expired`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            WakeState::Pending => "pending",
            WakeState::Ready => "ready",
            WakeState::Answering { .. } => "answering",
            WakeState::Answered { .. } => "answered",
            WakeState::Claimed { .. } => "claimed",
            WakeState::Told { .. } => "told",
            WakeState::Taken => "taken",
            WakeState::Expired { .. } => "expired",
        }
    }

    /// Whether it is still to be told (not told, taken or expired).
    #[must_use]
    pub const fn live(self) -> bool {
        !matches!(
            self,
            WakeState::Told { .. } | WakeState::Taken | WakeState::Expired { .. }
        )
    }

    /// Whether its condition has held: past `Pending`, and live.
    #[must_use]
    pub const fn met(self) -> bool {
        self.live() && !matches!(self, WakeState::Pending)
    }
}

/// The rule a wake's lines are told under.
pub const WAKE_RULE: &str = "wake@v1";

/// A WAKE'S EPISODE: `fnv1a("wake@v1 builtin <declared_ms> <ord>")`, the form `rules::episode`
/// gives a built-in rule's, so a successor loop and the book name one wake alike.
#[must_use]
pub fn wake_episode(declared_ms: i64, ord: u16) -> u64 {
    aterm_hash::fnv1a_64(format!("{WAKE_RULE} builtin {declared_ms} {ord}").as_bytes())
}

/// ONE LIVE WAKE AS THE LOOP SEES IT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeSeen {
    /// Its ordinal in the session's book, from 1.
    pub ord: u16,
    /// Who declared it.
    pub class: Declarer,
    /// Its priority.
    pub prio: Prio,
    /// Its state (a live one).
    pub state: WakeState,
    /// When it was declared, unix ms (its episode's key).
    pub declared_ms: i64,
    /// When its condition first held, unix ms.
    pub ready_since_ms: Option<i64>,
    /// What its condition's atoms read, in order.
    pub kinds: Vec<AtomKind>,
    /// Its need (`need=`), in the grammar's canonical spelling
    /// (`aterm_vitals::sys::predicate::AwaitSys::canonical`).
    pub need: Option<String>,
    /// Whether its need names build slots (the line reserves one).
    pub need_slots: bool,
    /// Its condition is time alone: only an `await wake` answers it, and no line is typed.
    pub await_only: bool,
}

impl WakeSeen {
    /// Its episode ([`wake_episode`]).
    #[must_use]
    pub fn episode(&self) -> u64 {
        wake_episode(self.declared_ms, self.ord)
    }
}

/// THE SESSION'S BOOK OF WAKES AS THE LOOP SEES IT (`IdleHost::wakes`, a query): each wake still
/// to be told, by ordinal. The empty book is the default host's, and what a replay of a build
/// without the door reads (`TapeValue::absent`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WakesSeen {
    /// The live wakes, lowest ordinal first.
    pub wakes: Vec<WakeSeen>,
}

impl WakesSeen {
    /// Wakes declared and not yet told, taken or expired.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.wakes.iter().filter(|w| w.state.live()).count()
    }

    /// Of them, those whose condition has held.
    #[must_use]
    pub fn ready(&self) -> usize {
        self.wakes.iter().filter(|w| w.state.met()).count()
    }

    /// THE WAKE A LOOP'S DOOR WOULD TELL FIRST: of those `Ready` or `Answered` and not
    /// `await_only`, a critical one first, then the lowest ordinal.
    #[must_use]
    pub fn head_due(&self) -> Option<&WakeSeen> {
        self.wakes
            .iter()
            .filter(|w| {
                !w.await_only && matches!(w.state, WakeState::Ready | WakeState::Answered { .. })
            })
            .min_by_key(|w| head_key(w.prio, w.ord))
    }
}

/// The order wakes are told in: a critical one first, then the lowest ordinal.
#[must_use]
pub fn head_key(prio: Prio, ord: u16) -> (bool, u16) {
    (prio != Prio::Critical, ord)
}

#[cfg(test)]
#[path = "wakes_seen_tests.rs"]
mod tests;
