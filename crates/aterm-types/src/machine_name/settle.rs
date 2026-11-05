// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `settle` (`docs/DESIGN-machine-names-2026-11-04.md` §6.1): who holds each number, and where
//! each node stands, as one pure function of the current accepted records in view.
//!
//! Every reader holding the same records computes the same answer: the input is keyed by node,
//! every ordering ends in a node id, and nothing reads arrival order, liveness or the clock.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::Number;

/// The `since` an older build's implied record carries (§6.1): after every migrated evidence rank
/// (1–6, §7.1) and before every claim stamped on a clock.
pub const IMPLIED_SINCE: u64 = 7;

/// A fleet node's id: 16 bytes (`node-<32hex>`), or 8 for a mesh principal (`n-<16hex>`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    len: u8,
    bytes: [u8; 16],
}

impl NodeId {
    /// The id these bytes spell: `None` unless there are 8 or 16 of them.
    #[must_use]
    pub fn new(id: &[u8]) -> Option<Self> {
        let len = u8::try_from(id.len())
            .ok()
            .filter(|len| *len == 8 || *len == 16)?;
        let mut bytes = [0; 16];
        bytes.get_mut(..id.len())?.copy_from_slice(id);
        Some(Self { len, bytes })
    }

    /// The id's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes
            .get(..usize::from(self.len))
            .unwrap_or(&self.bytes)
    }
}

impl Ord for NodeId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl PartialOrd for NodeId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.len == 8 { "n-" } else { "node-" })?;
        self.as_bytes()
            .iter()
            .try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A machine anchor (§3.2): nodes whose records carry one anchor are one machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Anchor(pub [u8; 8]);

/// How a claim was made (§3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// Picked by `choose`.
    Auto,
    /// A person's, an agent's or a driver's explicit act.
    Chosen,
    /// Carried over by migration, at an evidence rank (§7.1).
    Seeded,
}

impl Class {
    /// The class's place in a claim's rank: a person's or a migration's claim before an
    /// automatic one.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Chosen | Self::Seeded => 0,
            Self::Auto => 1,
        }
    }
}

/// One entry of a record's `was` (§3.2): an earlier NAME and its two flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Was {
    /// The earlier NAME.
    pub num: Number,
    /// A person retired it (§5.6 card 3): it no longer raises `top` past a cap.
    pub retired: bool,
    /// This machine lost it in a contest in this fleet and never reclaims it (§6.2).
    pub lost: bool,
}

/// A node's current accepted record: the fields `settle` and `choose` read.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Record {
    /// The NAME; `None` while unnumbered or second.
    pub num: Option<Number>,
    /// How the claim was made.
    pub class: Class,
    /// When this node first held this NAME in this fleet (ms), or an evidence rank 1–6.
    pub since: u64,
    /// For a take, the node whose original claim the chain descends from; `None` is this node.
    pub base: Option<NodeId>,
    /// The take generation: the taken claim's + 1, 0 without a take. Acceptance refuses
    /// `u32::MAX` (§3.2), so a take's + 1 never saturates into a tie.
    pub tgen: u32,
    /// The succession generation (§6.4 A reinstall); bounded at acceptance as `tgen` is.
    pub sgen: u32,
    /// When this node last changed its claim (ms).
    pub act: u64,
    /// The machine anchor; `None` for a machine of its own.
    pub anchor: Option<Anchor>,
    /// The NAME before the last change; while unnumbered, the NAME the machine kept.
    pub prev: Option<Number>,
    /// A migration that waits (§7.1): it publishes no NAME and keeps it as `prev`, and counts
    /// with `prev` when its group picks a primary.
    pub waits: bool,
    /// An ephemeral node (§6.3): it spends no number and never enters `rank`.
    pub ephemeral: bool,
    /// Up to 8 earlier NAMEs.
    pub was: Vec<Was>,
    /// The highest number this node has seen in this fleet's signed evidence.
    pub top: u32,
    /// A person's cap on `top`.
    pub cap: Option<Cap>,
}

/// A person's cap on `top` (§6.2): the latest one in any record bounds every `top` field and
/// every retired `was` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cap {
    /// The highest `top` it lets those fields raise.
    pub value: u32,
    /// When the person set it (ms).
    pub since: u64,
}

impl Record {
    /// The node `base` names, this one when it took nothing.
    #[must_use]
    pub fn base_or(&self, node: NodeId) -> NodeId {
        self.base.unwrap_or(node)
    }

    /// The numbers this machine lost in a contest in this fleet.
    pub fn lost(&self) -> impl Iterator<Item = Number> + '_ {
        self.was.iter().filter(|was| was.lost).map(|was| was.num)
    }
}

/// What a reader holds for one node in view (§6.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Entry {
    /// Its current accepted record.
    Record(Record),
    /// An older build's entry whose signed `name=` is a bare NAME: class `seeded`, since
    /// [`IMPLIED_SINCE`], no anchor, never moving.
    Implied(Number),
    /// An entry at or above `RECORD_SINCE` with no current acceptable record. It holds and
    /// displaces nothing; the signed `name=` its copy kept, if any, leaves that number
    /// unconfirmed.
    Unknown(Option<Number>),
}

/// One machine's claim on a number, as step 3 ranks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Claim {
    /// The number.
    pub num: Number,
    /// How it was made.
    pub class: Class,
    /// Its since.
    pub since: u64,
    /// The chain's original claimant.
    pub base: NodeId,
    /// Its take generation.
    pub tgen: u32,
    /// The machine's primary node.
    pub node: NodeId,
}

impl Claim {
    /// The claim's place among claims on its number (§6.1 step 3), first first.
    fn key(&self) -> (u8, u64, NodeId, NodeId) {
        (self.class.rank(), self.since, self.base, self.node)
    }

    /// The `(since, base, tgen)` a take of this claim writes: it stands exactly where this one
    /// stood, one take generation on.
    #[must_use]
    pub fn taken(&self) -> (u64, NodeId, u32) {
        (self.since, self.base, self.tgen.saturating_add(1))
    }
}

/// Who holds a number.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Holder {
    /// That node's machine holds it.
    Node(NodeId),
    /// An unknown claim names it, so no reader can say who holds it until its record arrives;
    /// these nodes are the unknown claimants.
    Unconfirmed(Vec<NodeId>),
}

/// Where a node stands (§6.1 step 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Standing {
    /// It is its machine's primary and holds this number.
    Holds(Number),
    /// Another node of its machine is the primary.
    Second {
        /// The machine's primary.
        of: NodeId,
    },
    /// Its claim is on a number an unknown claim leaves unconfirmed: it neither moves nor holds.
    Waiting(Number),
    /// Its claim lost.
    Displaced {
        /// The number it lost.
        lost: Number,
        /// The node that holds it.
        to: NodeId,
    },
    /// It is its machine's primary and carries no NAME.
    Unnumbered,
}

/// `settle`'s answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settled {
    /// Every claimed or unconfirmed number and who holds it.
    pub holders: BTreeMap<Number, Holder>,
    /// Every node with a record or an implied record, and where it stands. Unknown claimants
    /// stand nowhere.
    pub standings: BTreeMap<NodeId, Standing>,
    /// Each machine's nodes, keyed by its primary.
    pub machines: BTreeMap<NodeId, BTreeSet<NodeId>>,
}

impl Settled {
    /// The primary of `node`'s machine, if `node` stands anywhere.
    #[must_use]
    pub fn primary_of(&self, node: NodeId) -> Option<NodeId> {
        match self.standings.get(&node)? {
            Standing::Second { of } => Some(*of),
            _ => Some(node),
        }
    }

    /// Whether `a` and `b` are nodes of one machine.
    #[must_use]
    pub fn same_machine(&self, a: NodeId, b: NodeId) -> bool {
        a == b
            || self
                .primary_of(a)
                .is_some_and(|p| self.primary_of(b) == Some(p))
    }

    /// The node holding `num`, if one does.
    #[must_use]
    pub fn holder(&self, num: Number) -> Option<NodeId> {
        match self.holders.get(&num)? {
            Holder::Node(node) => Some(*node),
            Holder::Unconfirmed(_) => None,
        }
    }
}

/// Who holds each number and where each node stands, over every node's entry in view.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberEvidence",
        action = "Settle",
        project = "aterm_types::machine_name::model_tests::project_evidence"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberGroup",
        action = "Settle",
        project = "aterm_types::machine_name::model_tests::project_group"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberSuccession",
        action = "Settle",
        project = "aterm_types::machine_name::model_tests::project_succession"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "MachineNumberStripped",
        action = "Settle",
        project = "aterm_types::machine_name::model_tests::project_stripped"
    )
)]
pub fn settle(view: &BTreeMap<NodeId, Entry>) -> Settled {
    let mut settled = Settled::default();
    let mut claims: BTreeMap<Number, Vec<Claim>> = BTreeMap::new();
    let mut unconfirmed: BTreeMap<Number, Vec<NodeId>> = BTreeMap::new();
    let mut groups: BTreeMap<Anchor, Vec<NodeId>> = BTreeMap::new();
    for (node, entry) in view {
        match entry {
            Entry::Record(record) => match record.anchor {
                Some(anchor) => groups.entry(anchor).or_default().push(*node),
                None => {
                    machine(&mut settled, &mut claims, view, *node, &[*node]);
                }
            },
            Entry::Implied(num) => {
                settled.machines.insert(*node, BTreeSet::from([*node]));
                claims.entry(*num).or_default().push(Claim {
                    num: *num,
                    class: Class::Seeded,
                    since: IMPLIED_SINCE,
                    base: *node,
                    tgen: 0,
                    node: *node,
                });
            }
            Entry::Unknown(num) => {
                if let Some(num) = num {
                    unconfirmed.entry(*num).or_default().push(*node);
                }
            }
        }
    }
    for nodes in groups.values() {
        if let Some(first) = nodes.first() {
            machine(&mut settled, &mut claims, view, *first, nodes);
        }
    }
    for (num, by) in unconfirmed {
        settled.holders.insert(num, Holder::Unconfirmed(by));
    }
    for (num, on) in &claims {
        if settled.holders.contains_key(num) {
            continue;
        }
        let standing = on
            .iter()
            .filter(|c| !on.iter().any(|d| d.base == c.base && d.tgen > c.tgen))
            .min_by_key(|c| c.key());
        if let Some(holder) = standing {
            settled.holders.insert(*num, Holder::Node(holder.node));
        }
    }
    for on in claims.values() {
        for claim in on {
            let standing = match settled.holders.get(&claim.num) {
                Some(Holder::Node(holder)) if *holder == claim.node => Standing::Holds(claim.num),
                Some(Holder::Node(holder)) => Standing::Displaced {
                    lost: claim.num,
                    to: *holder,
                },
                Some(Holder::Unconfirmed(_)) | None => Standing::Waiting(claim.num),
            };
            settled.standings.insert(claim.node, standing);
        }
    }
    settled
}

/// One machine's primary, its claim and its nodes' standings (§6.1 step 2). `first` is any of
/// `nodes`, all of which hold a [`Entry::Record`].
fn machine(
    settled: &mut Settled,
    claims: &mut BTreeMap<Number, Vec<Claim>>,
    view: &BTreeMap<NodeId, Entry>,
    first: NodeId,
    nodes: &[NodeId],
) {
    let records: Vec<(NodeId, &Record)> = nodes
        .iter()
        .filter_map(|node| match view.get(node) {
            Some(Entry::Record(record)) => Some((*node, record)),
            _ => None,
        })
        .collect();
    let carries = |record: &Record| {
        record
            .num
            .or_else(|| record.waits.then_some(record.prev).flatten())
    };
    // `sgen` decides first: a superseded node's later takes, made on a view that lacked its
    // successor, never rank it again. Only within the greatest `sgen` does a same-base take drop
    // the claim it took.
    let top_sgen = records.iter().map(|(_, r)| r.sgen).max().unwrap_or(0);
    let tier: Vec<(NodeId, &Record)> = records
        .iter()
        .copied()
        .filter(|(_, r)| r.sgen == top_sgen)
        .collect();
    let remaining: Vec<(NodeId, &Record)> = tier
        .iter()
        .copied()
        .filter(|(node, record)| {
            record.num.is_none_or(|num| {
                !tier.iter().any(|(other, rival)| {
                    rival.num == Some(num)
                        && rival.base_or(*other) == record.base_or(*node)
                        && rival.tgen > record.tgen
                })
            })
        })
        .collect();
    let tier = remaining.iter();
    let named = tier
        .clone()
        .filter(|(_, r)| carries(r).is_some())
        .min_by_key(|(node, r)| (r.class.rank(), r.since, r.act, *node));
    let primary = named
        .or_else(|| {
            tier.clone()
                .min_by_key(|(node, r)| (std::cmp::Reverse(r.act), *node))
        })
        .map_or(first, |(node, _)| *node);
    settled
        .machines
        .insert(primary, nodes.iter().copied().collect());
    for (node, _) in &records {
        if *node != primary {
            settled
                .standings
                .insert(*node, Standing::Second { of: primary });
        }
    }
    let Some((_, record)) = records.iter().find(|(node, _)| *node == primary) else {
        return;
    };
    match record.num {
        Some(num) => claims.entry(num).or_default().push(Claim {
            num,
            class: record.class,
            since: record.since,
            base: record.base_or(primary),
            tgen: record.tgen,
            node: primary,
        }),
        None => {
            settled.standings.insert(primary, Standing::Unnumbered);
        }
    }
}

/// The stamp an automatic claim's `since`, or any `act`, takes at `now` (§6.2): past every stamp
/// seen that is not past this node's own clock, so a claim made after seeing another carries the
/// later stamp, and one fast clock's stamps are not copied into every later claim.
#[must_use]
pub fn next_stamp(now: u64, seen: impl IntoIterator<Item = u64>) -> u64 {
    let latest = seen.into_iter().filter(|stamp| *stamp <= now).max();
    latest.map_or(now, |stamp| now.max(stamp.saturating_add(1)))
}
