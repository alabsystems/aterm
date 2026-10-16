// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The record (design §3.1) and the host's clock reading.

use crate::field::{Fields, Key};
use crate::vocab::{Actor, Conf, Kind, Lvl, Node, Op, Src};

/// The host's clock reading at an event: the only way time enters this
/// crate, which reads no clock itself. `wall_ms` is Unix milliseconds and is
/// printed as `t=`; `mono_ms` is a monotonic clock private to one process,
/// used only for durations (`dur_ms`) and settle deadlines, never printed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Stamp {
    /// Wall-clock Unix milliseconds.
    pub wall_ms: u64,
    /// A monotonic clock in milliseconds, private to the process.
    pub mono_ms: u64,
}

impl Stamp {
    /// A stamp.
    #[must_use]
    pub const fn new(wall_ms: u64, mono_ms: u64) -> Stamp {
        Stamp { wall_ms, mono_ms }
    }

    /// This stamp `ms` later on both clocks.
    #[must_use]
    pub const fn after(self, ms: u64) -> Stamp {
        Stamp {
            wall_ms: self.wall_ms.saturating_add(ms),
            mono_ms: self.mono_ms.saturating_add(ms),
        }
    }
}

/// One record of a session's log.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rec {
    /// Per session, strictly increasing; the order within a session.
    pub seq: u64,
    /// Wall Unix ms: when it happened, per its source.
    pub t: u64,
    /// What it does to its node.
    pub op: Op,
    /// The node it opens, closes, amends or marks.
    pub node: Node,
    /// The parent, on an open only.
    pub par: Option<Node>,
    /// The node's kind (open, close, amend) or the mark's.
    pub k: Kind,
    /// Where the fact was observed.
    pub src: Src,
    /// How sure the source is.
    pub conf: Conf,
    /// Who it is attributed to.
    pub by: Actor,
    /// At most twelve fields.
    pub f: Fields,
}

impl Rec {
    /// The turn class the bus mirror carries (§5.5): opens, closes and
    /// amends of the session, runs and turns; `need`, `clear` and `handoff`
    /// marks; gaps and snapshots. Steps and other marks stay local.
    #[must_use]
    pub fn is_turn_class(&self) -> bool {
        match self.op {
            Op::Open | Op::Close | Op::Amend => {
                matches!(self.node.lvl, Lvl::Session | Lvl::Run | Lvl::Turn)
            }
            Op::Mark => matches!(self.k, Kind::Need | Kind::Clear | Kind::Handoff),
            Op::Gap | Op::Snap => true,
        }
    }

    /// The `out=` word of a close, if any.
    #[must_use]
    pub fn out(&self) -> Option<&'static str> {
        self.f.get_word(Key::Out)
    }
}
