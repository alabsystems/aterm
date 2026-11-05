// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `check` (`docs/DESIGN-machine-names-2026-11-04.md` §5.3, §6.4): what a person's rename to a
//! typed number would do in one view, and [`renamed`], the record the press writes.

use super::choose::{Fleet, Kept};
use super::settle::{Class, Entry, Holder, NodeId, Record, Standing, Was, settle};
use super::{Number, Refusal};

/// The most `was` entries a record keeps (§3.2).
pub const WAS_MAX: usize = 8;

/// What renaming this machine to a typed number would do (§5.3). Tests assert kinds, never the
/// sheet's prose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The text is no number.
    Grammar(Refusal),
    /// This node is a second node: another account on this machine holds its NAME, and only
    /// **Make This Mac** applies.
    SecondNode {
        /// The machine's primary.
        of: NodeId,
    },
    /// Another node of this machine carries a claim the renamed record would rank behind (a
    /// NAME it has not dropped as second yet, or a migration that waits): the rename would leave
    /// this node second.
    OtherAccountNamed {
        /// That node.
        node: NodeId,
    },
    /// It is this machine's NAME already.
    Same,
    /// Nothing holds or keeps it.
    Free,
    /// Another machine holds it automatically; a person's claim outranks it and that machine
    /// moves.
    TakesAuto {
        /// The holder.
        from: NodeId,
    },
    /// A person chose it for another machine: only a take applies.
    HeldByPerson {
        /// The holder.
        by: NodeId,
    },
    /// Migration carried it over to another machine: only a take applies.
    HeldSeeded {
        /// The holder.
        by: NodeId,
    },
    /// An older aterm holds it, whose record cannot be taken.
    HeldByOlderBuild {
        /// The holder.
        by: NodeId,
    },
    /// An unknown claim names it, whose record has not reached this machine.
    Unconfirmed {
        /// The unknown claimants.
        by: Vec<NodeId>,
    },
    /// A machine that left held it last: the rename takes that machine's last record.
    Retired {
        /// The node it was.
        of: NodeId,
    },
    /// A machine that left long ago had it, and only the number is kept.
    Reserved,
    /// Another machine had it earlier, a machine that left included.
    EarlierName {
        /// That machine's node.
        of: NodeId,
    },
    /// A planned `computers` row is labelled with it; the rename binds that row.
    Planned,
}

impl Verdict {
    /// Whether **Rename** applies it as typed.
    #[must_use]
    pub const fn renames(&self) -> bool {
        matches!(
            self,
            Self::Free
                | Self::TakesAuto { .. }
                | Self::Retired { .. }
                | Self::Reserved
                | Self::EarlierName { .. }
                | Self::Planned
        )
    }

    /// Whether a separate **Take** is offered.
    #[must_use]
    pub const fn takes(&self) -> bool {
        matches!(self, Self::HeldByPerson { .. } | Self::HeldSeeded { .. })
    }
}

/// What renaming `me` to `text` would do in `fleet`.
///
/// As §5.3 says, it settles the view with the record the press would write: a verdict that
/// renames or takes stands only when this node would then hold the number, and otherwise names
/// the node of this machine that would outrank it (a sibling's stronger claim, or a migration
/// that waits). The written record's `since` is stamped past every stamp in view, the latest a
/// press could write, so the simulation is the press's worst case.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberHolder",
        action = "Rename",
        project = "aterm_types::machine_name::model_tests::project_holder"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberHolder",
        action = "Take",
        project = "aterm_types::machine_name::model_tests::project_holder"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberHolder",
        action = "TakeTwice",
        project = "aterm_types::machine_name::model_tests::project_holder"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberPress",
        action = "Rename",
        project = "aterm_types::machine_name::model_tests::project_press"
    )
)]
pub fn check(text: &str, fleet: &Fleet<'_>, me: NodeId) -> Verdict {
    let num = match Number::parse(text) {
        Ok(num) => num,
        Err(refusal) => return Verdict::Grammar(refusal),
    };
    let settled = fleet.settled;
    if let Some(Standing::Second { of }) = settled.standings.get(&me) {
        return Verdict::SecondNode { of: *of };
    }
    if settled.standings.get(&me) == Some(&Standing::Holds(num)) {
        return Verdict::Same;
    }
    let verdict = classify(fleet, me, num);
    let current = match fleet.view.get(&me) {
        Some(Entry::Record(record)) => record,
        _ => return verdict,
    };
    if !(verdict.renames() || verdict.takes()) {
        return verdict;
    }
    let stamp = fleet
        .view
        .values()
        .filter_map(|entry| match entry {
            Entry::Record(record) => Some(record.since.max(record.act)),
            Entry::Implied(_) | Entry::Unknown(_) => None,
        })
        .max()
        .map_or(0, |latest| latest.saturating_add(1));
    let mut after = fleet.view.clone();
    after.insert(
        me,
        Entry::Record(renamed(current, num, taken(fleet, &verdict), stamp)),
    );
    let after = settle(&after);
    match after.standings.get(&me) {
        Some(Standing::Holds(held)) if *held == num => verdict,
        Some(Standing::Second { of }) => Verdict::OtherAccountNamed { node: *of },
        Some(Standing::Displaced { to, .. }) => held_by(fleet, *to),
        _ => match after.holders.get(&num) {
            Some(Holder::Unconfirmed(by)) => Verdict::Unconfirmed { by: by.clone() },
            _ => verdict,
        },
    }
}

/// What holds or keeps `num` away from `me`, before any simulation.
fn classify(fleet: &Fleet<'_>, me: NodeId, num: Number) -> Verdict {
    let settled = fleet.settled;
    match settled.holders.get(&num) {
        Some(Holder::Unconfirmed(by)) => return Verdict::Unconfirmed { by: by.clone() },
        Some(Holder::Node(by)) if !settled.same_machine(me, *by) => return held_by(fleet, *by),
        _ => {}
    }
    let anchor = match fleet.view.get(&me) {
        Some(Entry::Record(record)) => record.anchor,
        _ => None,
    };
    let tombstones = fleet.kept.iter().filter_map(|kept| match kept {
        Kept::Tombstone { node, last } if anchor.is_none() || last.anchor != anchor => {
            Some((*node, last))
        }
        _ => None,
    });
    let retired = tombstones
        .clone()
        .filter(|(_, last)| last.num == Some(num))
        .map(|(node, _)| node)
        .min();
    if let Some(of) = retired {
        return Verdict::Retired { of };
    }
    let reserved = fleet
        .kept
        .iter()
        .any(|kept| matches!(kept, Kept::Reserved { lo, hi } if (*lo..=*hi).contains(&num.get())));
    if reserved {
        return Verdict::Reserved;
    }
    let left = tombstones
        .filter(|(_, last)| last.was.iter().any(|was| was.num == num))
        .map(|(node, _)| node);
    let live = fleet
        .view
        .iter()
        .filter(|(node, entry)| {
            !settled.same_machine(me, **node)
                && matches!(entry, Entry::Record(record) if record.was.iter().any(|was| was.num == num))
        })
        .map(|(node, _)| *node);
    if let Some(of) = left.chain(live).min() {
        return Verdict::EarlierName { of };
    }
    let planned = fleet.kept.iter().any(
        |kept| matches!(kept, Kept::Row { num: row, planned: true, node: None } if *row == num),
    );
    if planned {
        Verdict::Planned
    } else {
        Verdict::Free
    }
}

/// The verdict for a number `by` holds: by the class of its record, or an older build's.
fn held_by(fleet: &Fleet<'_>, by: NodeId) -> Verdict {
    match fleet.view.get(&by) {
        Some(Entry::Record(record)) => match record.class {
            Class::Auto => Verdict::TakesAuto { from: by },
            Class::Chosen => Verdict::HeldByPerson { by },
            Class::Seeded => Verdict::HeldSeeded { by },
        },
        _ => Verdict::HeldByOlderBuild { by },
    }
}

/// The `(since, base, tgen)` a rename under `verdict` copies from the claim it takes: the
/// holder's for a take over a person's or a migration's claim, the tombstone's last record for
/// a retired number, and `None` for a plain `chosen` claim.
#[must_use]
pub fn taken(fleet: &Fleet<'_>, verdict: &Verdict) -> Option<(u64, NodeId, u32)> {
    let of = |node: NodeId, record: &Record| {
        (
            record.since,
            record.base_or(node),
            record.tgen.saturating_add(1),
        )
    };
    match verdict {
        Verdict::HeldByPerson { by } | Verdict::HeldSeeded { by } => match fleet.view.get(by)? {
            Entry::Record(record) => Some(of(*by, record)),
            Entry::Implied(_) | Entry::Unknown(_) => None,
        },
        Verdict::Retired { of: gone } => fleet.kept.iter().find_map(|kept| match kept {
            Kept::Tombstone { node, last } if node == gone => Some(of(*node, last)),
            _ => None,
        }),
        _ => None,
    }
}

/// The record a rename of `current` to `num` writes (§6.4 A person's rename): class `chosen`,
/// `since`, `base` and `tgen` copied from `take` (else a fresh `since` stamped at `stamp` and no
/// take), the old NAME as `prev` (kept when the rename retakes the current NAME) and at the
/// front of `was`, and a new `act`.
#[must_use]
pub fn renamed(
    current: &Record,
    num: Number,
    take: Option<(u64, NodeId, u32)>,
    stamp: u64,
) -> Record {
    let (since, base, tgen) = take.map_or((stamp, None, 0), |(since, base, tgen)| {
        (since, Some(base), tgen)
    });
    let mut was = current.was.clone();
    if let Some(old) = current.num.filter(|old| *old != num) {
        was.retain(|entry| entry.num != old);
        was.insert(
            0,
            Was {
                num: old,
                retired: false,
                lost: false,
            },
        );
    }
    was.retain(|entry| entry.num != num);
    was.truncate(WAS_MAX);
    Record {
        num: Some(num),
        class: Class::Chosen,
        since,
        base,
        tgen,
        sgen: current.sgen,
        act: stamp,
        anchor: current.anchor,
        prev: current.num.filter(|old| *old != num).or(current.prev),
        waits: false,
        ephemeral: current.ephemeral,
        was,
        top: current.top.max(num.get()),
        cap: current.cap,
    }
}
