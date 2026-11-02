// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The record (design §3.1) and the host's clock reading.

use crate::field::{Fields, Key, Tier};
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
    #[inline]
    #[must_use]
    pub const fn new(wall_ms: u64, mono_ms: u64) -> Stamp {
        Stamp { wall_ms, mono_ms }
    }

    /// This stamp `ms` later on both clocks.
    #[inline]
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
    #[inline]
    #[must_use]
    pub const fn is_turn_class(&self) -> bool {
        match self.op {
            Op::Open | Op::Close | Op::Amend => {
                matches!(self.node.lvl, Lvl::Session | Lvl::Run | Lvl::Turn)
            }
            Op::Mark => matches!(self.k, Kind::Need | Kind::Clear | Kind::Handoff),
            Op::Gap | Op::Snap => true,
        }
    }

    /// THE BUS CLASS (§5.5): what the bus mirror carries.
    /// The turn class ([`Rec::is_turn_class`], unchanged, which `log
    /// class=turn` and `subscribe … log:class=turn` still mean), and beside
    /// it what a remote fold needs to hold the needs a local one holds: the
    /// opens, closes and amends of the steps a need is raised from
    /// (`approval`, `question`, `wall`, `stall`: the fold raises those needs
    /// only from the step's open and drops them only at its close), and the
    /// `seen` mark that lifts `need=failure`. Tool, subagent and compact
    /// steps and every other mark stay local; a turn's close counts them.
    #[inline]
    #[must_use]
    pub const fn is_bus_class(&self) -> bool {
        match self.op {
            Op::Open | Op::Close | Op::Amend => match self.node.lvl {
                Lvl::Session | Lvl::Run | Lvl::Turn => true,
                Lvl::Step => matches!(
                    self.k,
                    Kind::Approval | Kind::Question | Kind::Wall | Kind::Stall
                ),
                Lvl::Instance => false,
            },
            Op::Mark => matches!(
                self.k,
                Kind::Need | Kind::Clear | Kind::Handoff | Kind::Seen
            ),
            Op::Gap | Op::Snap => true,
        }
    }

    /// WHAT THE MIRROR PUBLISHES (§5.5, fix unit E5, E2E-16): a record of the bus class
    /// ([`Rec::is_bus_class`]) whose body would carry something past its header. An amend of
    /// the bus class whose every field is above the bus's ceiling ([`Tier::WireMeta`]), such
    /// as Claude Code's transcript match `amend t<n> turn vref=<id>` (`vref=` is a host
    /// fact), would cross as an amend of nothing, a no-op to every fold, so it is not
    /// mirrored: the log gives it no `ms` (`Log::push`), so it takes no slot and the bus
    /// numbers no hole for it. Under `presence = minimal` a take is encoded at
    /// [`Tier::Wire`], so an amend whose fields are all W* (`ver=`, `model=`) keeps the slot
    /// it was given at append and crosses with none of them (ruled residual: `ms` is fixed
    /// before any take's tier is known).
    ///
    /// [`Rec::is_bus_class`] and [`Rec::is_turn_class`] stay as they are, and are still what
    /// `encode_mirror` and `bus::BusSpace::offer` ask: a log carried from an older build, or
    /// a broker's log, holds field-less amends that already have an `ms`, which must still
    /// encode as records (refusing them would turn each slot into a `gap why=dropped` cover
    /// and every reader's fold partial), and a reader folds an older writer's field-less
    /// amend as a record, never as foreign. The turn class stays within the bus class.
    #[must_use]
    pub fn is_mirrored(&self) -> bool {
        self.is_bus_class()
            && (self.op != Op::Amend || self.f.iter().any(|(k, _)| k.tier() <= Tier::WireMeta))
    }

    /// The `out=` word of a close, if any.
    #[inline]
    #[must_use]
    pub fn out(&self) -> Option<&'static str> {
        self.f.get_word(Key::Out)
    }
}
