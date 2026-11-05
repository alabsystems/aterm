// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `choose` (`docs/DESIGN-machine-names-2026-11-04.md` §6.2): the number a node takes by itself,
//! and `top`, the highest number this fleet's signed evidence names.
//!
//! Pure over what every reader of one view holds alike: the records, the numbers kept out of
//! automatic use and each node's labels, so every node sharing a view numbers the same list and
//! takes its own place in it.

use std::collections::{BTreeMap, BTreeSet};

use super::Number;
use super::settle::{Anchor, Entry, NodeId, Record, Settled, Standing};

/// A number kept out of automatic use, and whose it is (§6.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    /// A removed, denied or expired node's tombstone: its last verified record, whose NAME and
    /// `was` it keeps. A tombstone whose record carries this machine's anchor is this
    /// machine's own.
    Tombstone {
        /// The node it was.
        node: NodeId,
        /// Its last verified record.
        last: Record,
    },
    /// A folded reservation's range of numbers, both ends included: always another machine's.
    Reserved {
        /// The range's first number.
        lo: u32,
        /// The range's last number.
        hi: u32,
    },
    /// A roster label or alias naming the number for a Mac: the node it is bound to, if any.
    Label {
        /// The number.
        num: Number,
        /// The node the row is bound to.
        node: Option<NodeId>,
    },
    /// A `computers` row (live, retired or planned) holding the number as its label or alias.
    Row {
        /// The number.
        num: Number,
        /// The node the row is bound to.
        node: Option<NodeId>,
        /// The row is planned and bound to no node, so a want of its label binds it.
        planned: bool,
    },
    /// The leading NAME of an older unnamed entry's signed `host=`, which its migration may seed:
    /// off the automatic list, though not off a want.
    OlderHost(Number),
}

impl Kept {
    /// Whether this keeps `num`.
    fn keeps(&self, num: Number) -> bool {
        match self {
            Self::Tombstone { last, .. } => {
                last.num == Some(num) || last.was.iter().any(|was| was.num == num)
            }
            Self::Reserved { lo, hi } => (*lo..=*hi).contains(&num.get()),
            Self::Label { num: kept, .. } | Self::Row { num: kept, .. } | Self::OlderHost(kept) => {
                *kept == num
            }
        }
    }
}

/// A needing node's labels: its vault admission label and its roster label (§6.2 step 1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Labels {
    /// Its membership-log admission label.
    pub admission: Option<Number>,
    /// Its roster label, for a member that can read the roster.
    pub roster: Option<Number>,
}

/// Why a node may not pick at all (§6.2, §6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// It is ephemeral: only a person names it.
    Ephemeral,
    /// Its clock reads before the 2026-01-01 floor.
    ClockBeforeFloor,
    /// It holds no fleet seed, so it cannot publish an anchor yet.
    NoFleetSeed,
}

/// What `choose` decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Nothing to pick: the node holds, waits, is second, or is not in view.
    Keep,
    /// The node may not pick.
    Blocked(Blocked),
    /// Entries opaque to the node are live (§6.1): it waits for these to be read first-hand.
    Defer(Vec<NodeId>),
    /// Claim this want, as `auto`.
    Want(Number),
    /// Claim this pick above `top`, as `auto`.
    Pick(Number),
    /// Every number to m999999 is spent: claim this, the lowest no evidence names, and record
    /// a finding.
    Overflow(Number),
}

/// One view of a fleet, as `choose` and `check` read it.
#[derive(Clone, Copy, Debug)]
pub struct Fleet<'a> {
    /// Every node's entry in view.
    pub view: &'a BTreeMap<NodeId, Entry>,
    /// `settle` over `view`.
    pub settled: &'a Settled,
    /// The view's `top` ([`top`]).
    pub top: u32,
    /// The numbers kept out of automatic use.
    pub kept: &'a [Kept],
    /// Each node's labels, where known.
    pub labels: &'a BTreeMap<NodeId, Labels>,
}

impl Fleet<'_> {
    fn record(&self, node: NodeId) -> Option<&Record> {
        match self.view.get(&node)? {
            Entry::Record(record) => Some(record),
            Entry::Implied(_) | Entry::Unknown(_) => None,
        }
    }

    /// Whether `num` is free for `node` (§6.2 step 1): no other machine holds it or leaves it
    /// unconfirmed, claims it, keeps it (`prev`) or lists it in `was`, no tombstone or
    /// reservation keeps it for another machine, no other node has it as an admission or roster
    /// label, no roster label, alias or `computers` row names it for another Mac, and it is not
    /// in `node`'s `lost`. A `want` may bind a planned row nobody holds.
    #[must_use]
    pub fn is_free(&self, node: NodeId, num: Number, want: bool) -> bool {
        let mine = |other: NodeId| self.settled.same_machine(node, other);
        let anchor: Option<Anchor> = self.record(node).and_then(|record| record.anchor);
        match self.settled.holders.get(&num) {
            Some(super::settle::Holder::Node(holder)) if !mine(*holder) => return false,
            Some(super::settle::Holder::Unconfirmed(_)) => return false,
            _ => {}
        }
        let named = self.view.iter().any(|(other, entry)| {
            !mine(*other)
                && matches!(entry, Entry::Record(record)
                    if record.num == Some(num)
                        || record.prev == Some(num)
                        || record.was.iter().any(|was| was.num == num))
        });
        let labelled = self.labels.iter().any(|(other, labels)| {
            !mine(*other) && (labels.admission == Some(num) || labels.roster == Some(num))
        });
        if named || labelled {
            return false;
        }
        let kept = self
            .kept
            .iter()
            .filter(|kept| kept.keeps(num))
            .any(|kept| match kept {
                Kept::Tombstone { last, .. } => anchor.is_none() || last.anchor != anchor,
                Kept::Reserved { .. } => true,
                Kept::Label { node: of, .. } => !of.is_some_and(mine),
                Kept::Row {
                    node: of, planned, ..
                } => !(of.is_some_and(mine) || (want && *planned && of.is_none())),
                Kept::OlderHost(_) => false,
            });
        if kept {
            return false;
        }
        !self
            .record(node)
            .is_some_and(|record| record.lost().any(|l| l == num))
    }

    /// `node`'s wants, in order (§6.2 step 1, §6.4): the NAME it claims, the NAME it kept or held
    /// before (`prev`, which a displaced renamer falls back to), its admission label, then, only
    /// above `top`, its roster label.
    fn wants(&self, node: NodeId) -> impl Iterator<Item = Number> + '_ {
        let record = self.record(node);
        let num = record.and_then(|record| record.num);
        let prev = record.and_then(|record| record.prev);
        let labels = self.labels.get(&node).copied().unwrap_or_default();
        let roster = labels.roster.filter(|label| label.get() > self.top);
        [num, prev, labels.admission, roster].into_iter().flatten()
    }

    /// The want `node` takes in this view: its first want free for it.
    fn want_of(&self, node: NodeId) -> Option<Number> {
        self.wants(node)
            .find(|want| self.is_free(node, *want, true))
    }

    /// The nodes that need a number now: unnumbered or displaced primaries with a record, none
    /// ephemeral and none a migration that waits (§7.1), which keeps its NAME and fetches.
    fn needing(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.settled
            .standings
            .iter()
            .filter(|(_, standing)| {
                matches!(standing, Standing::Unnumbered | Standing::Displaced { .. })
            })
            .map(|(node, _)| *node)
            .filter(|node| {
                self.record(*node)
                    .is_some_and(|record| !record.ephemeral && !record.waits)
            })
    }
}

/// The number `me` takes by itself in this view, if it needs one and may pick (§6.2).
///
/// `blocked` is why it may not pick at all, and `opaque` the live entries it cannot read whole
/// (§6.1): while there are any it defers.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberGroup",
        action = "Pick",
        project = "aterm_types::machine_name::model_tests::project_group"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberStripped",
        action = "Pick",
        project = "aterm_types::machine_name::model_tests::project_stripped"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberChoose",
        action = "Pick",
        project = "aterm_types::machine_name::model_tests::project_choose"
    )
)]
pub fn choose(
    fleet: &Fleet<'_>,
    me: NodeId,
    blocked: Option<Blocked>,
    opaque: &[NodeId],
) -> Choice {
    let needs = matches!(
        fleet.settled.standings.get(&me),
        Some(Standing::Unnumbered | Standing::Displaced { .. })
    );
    let Some(record) = fleet.record(me).filter(|record| needs && !record.waits) else {
        return Choice::Keep;
    };
    if let Some(why) = blocked.or(record.ephemeral.then_some(Blocked::Ephemeral)) {
        return Choice::Blocked(why);
    }
    if !opaque.is_empty() {
        let mut on = opaque.to_vec();
        on.sort_unstable();
        on.dedup();
        return Choice::Defer(on);
    }
    if let Some(want) = fleet.want_of(me) {
        return Choice::Want(want);
    }
    let ranked: Vec<NodeId> = fleet
        .needing()
        .filter(|node| fleet.want_of(*node).is_none())
        .collect();
    let rank = ranked.iter().position(|node| *node == me).unwrap_or(0);
    let spent = fleet.spent();
    let unspent = |num: &Number| !spent.contains(num) && !fleet.kept.iter().any(|k| k.keeps(*num));
    (fleet.top.saturating_add(1)..=Number::MAX)
        .filter_map(Number::new)
        .filter(unspent)
        .nth(rank)
        .map(Choice::Pick)
        .or_else(|| {
            (1..=Number::MAX)
                .filter_map(Number::new)
                .filter(unspent)
                .nth(rank)
                .map(Choice::Overflow)
        })
        .unwrap_or(Choice::Keep)
}

impl Fleet<'_> {
    /// Every number some evidence in view names, whoever's: holders and unconfirmed numbers,
    /// every record's NAME, kept NAME (`prev`) and `was`, implied and unknown claims' names, and
    /// every node's labels. With [`Kept::keeps`], it is the same for every node sharing a view,
    /// so all of them number one free list (§6.2 step 2), which leaves out every want and every
    /// number a machine lost.
    fn spent(&self) -> BTreeSet<Number> {
        let mut spent: BTreeSet<Number> = self.settled.holders.keys().copied().collect();
        for entry in self.view.values() {
            match entry {
                Entry::Record(record) => {
                    spent.extend(record.num);
                    spent.extend(record.prev);
                    spent.extend(record.was.iter().map(|was| was.num));
                }
                Entry::Implied(num) => {
                    spent.insert(*num);
                }
                Entry::Unknown(num) => spent.extend(*num),
            }
        }
        for labels in self.labels.values() {
            spent.extend(labels.admission);
            spent.extend(labels.roster);
        }
        spent
    }
}

/// `top` (§6.2): the highest number in this fleet's signed evidence, which never falls except
/// under a person's cap.
///
/// Every record's NAME and active `was` entries, implied names, unknown claims' signed names,
/// `signed` (membership-log and roster labels), tombstones and reservations raise it whatever
/// the cap; every record's `top`, `persisted` (this node's own for this fleet's seed) and every
/// retired `was` entry raise it only up to the latest cap in any record (ties to the lower node
/// id). A `top`, `persisted` or cap past m999999 counts as m999999, so no record sends every
/// node to the overflow path at once.
#[must_use]
pub fn top(
    view: &BTreeMap<NodeId, Entry>,
    kept: &[Kept],
    signed: impl IntoIterator<Item = Number>,
    persisted: u32,
) -> u32 {
    let mut firm = 0u32;
    let mut capped = persisted.min(Number::MAX);
    let mut cap: Option<(u64, std::cmp::Reverse<NodeId>, u32)> = None;
    let raise = |into: &mut u32, num: u32| *into = (*into).max(num);
    for (node, entry) in view {
        match entry {
            Entry::Record(record) => {
                if let Some(num) = record.num {
                    raise(&mut firm, num.get());
                }
                for was in &record.was {
                    if was.retired {
                        raise(&mut capped, was.num.get());
                    } else {
                        raise(&mut firm, was.num.get());
                    }
                }
                raise(&mut capped, record.top.min(Number::MAX));
                if let Some(set) = record.cap {
                    let key = (
                        set.since,
                        std::cmp::Reverse(*node),
                        set.value.min(Number::MAX),
                    );
                    cap = Some(cap.map_or(key, |held| held.max(key)));
                }
            }
            Entry::Implied(num) | Entry::Unknown(Some(num)) => raise(&mut firm, num.get()),
            Entry::Unknown(None) => {}
        }
    }
    for num in signed {
        raise(&mut firm, num.get());
    }
    for held in kept {
        match held {
            Kept::Tombstone { last, .. } => {
                for num in last
                    .num
                    .into_iter()
                    .chain(last.was.iter().map(|was| was.num))
                {
                    raise(&mut firm, num.get());
                }
            }
            Kept::Reserved { hi, .. } => raise(&mut firm, (*hi).min(Number::MAX)),
            Kept::Label { .. } | Kept::Row { .. } | Kept::OlderHost(_) => {}
        }
    }
    let capped = cap.map_or(capped, |(_, _, value)| capped.min(value));
    firm.max(capped)
}
