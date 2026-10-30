// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pure evaluator of the membership log (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.2).
//!
//! Every Mac runs it over the records it holds, so two Macs that hold the same records agree on
//! who is in and on every label, whatever order the records reached them in.
//!
//! **Authorized.** A record counts only when its signer was a member in the record's own causal
//! past: the membership those strictly earlier records give.
//!
//! **Removes are judged by causality alone.** An authorized `Remove` takes its member out. A
//! concurrent `Remove` of its own signer does not undo it, so two members that remove each other
//! concurrently are both out, and no evaluation order picks a survivor. A member removed in a
//! record's past is no member there, so its later `Remove`s are inert.
//!
//! **Other records also answer to cutoffs.** An `Admit`, `Name` or `Node` stops counting when its
//! signer has an effective `Remove` whose cutoff does not reach it, concurrent or later, unless an
//! effective `Admit` of the signer that follows that `Remove` (a restoring one) is in its past.
//!
//! **Who is in.** A Mac is in when the genesis binds it or an effective `Admit` lets it in, and
//! every effective `Remove` of it lies in that entry's causal past: a `Remove` beats a concurrent
//! `Admit`, and an `Admit` that follows a `Remove` restores the Mac. A founder the genesis lists
//! with no binding is no member until an `Admit` lets it in; until one does, or while none that
//! let it in stands, it is unproved ([`Membership::unproved`]).
//!
//! **The quorum `Remove`.** The `Remove`s that name one member and one cutoff are one quorum
//! `Remove`, its signers the shares; a cutoff is a record of that member or the record that let
//! it in ([`super::Log::insert`] holds to that), and an `Admit` no member signed is none. Its
//! effects: the member is out for good, and every record it signed outside the cutoff's past is
//! void everywhere, its `Remove`s, `Admit`s and shares included; what it signed before stands.
//! It holds when, with its own effects' records void, the shares that count are at least two
//! and more than half of the voters: the members at the cutoff, over the cutoff's causal past,
//! the member excluded. A share counts when its signer votes and the share is not void.
//!
//! Quorums are then weighed in pairs. One undoes another when, with its records void too, the
//! other no longer holds; it strips the other when it also takes a vote from one of the other's
//! signers, as voiding the `Admit`s that let the Macs its member admitted after the cutoff in.
//! One defeats another it undoes, unless that other undoes it back without being the only one
//! that strips; a quorum takes effect unless one that takes effect defeats it. So Macs a voided
//! member let in never outvote the quorum that voided them, and mutual quorum `Remove`s that
//! each void a share of the other remove both, whichever cutoff comes first.
//!
//! **Labels.** A Mac's label claim is the label in its first `Admit` (or its genesis entry). An
//! `Admit` claims when it is authorized in its own causal past, which no later record changes,
//! or under the quorums that take effect. A label claimed by two or more Macs is held bare by
//! none: each takes the label plus the first four hex digits of its binding's SHA-256
//! (`m40-3fa2`), except that a claim never suffixes a Mac whose claim it already had in its
//! past. Claims only accrue as the log grows, so a Mac's label changes at most once, the first
//! time its log shows a clash. The exceptions: a Mac with concurrent first `Admit`s that claim
//! different labels takes the suffix on the label of the one with the lowest id, which a later
//! concurrent `Admit` can move; and a claim that only the quorums authorize lapses if they do.
//!
//! **Names and node keys.** A member's latest effective `Name` (its highest sequence, at a tie the
//! lowest id) overrides the name its entry carried, and its latest effective `Node` the node key,
//! so a founder its genesis listed with no node key is bound by its own `Node`, and a later one
//! rotates the key.
//!
//! **The node-key index** ([`Membership::node`]) answers who a fleet node key is, from the keys
//! records bind: the node keys the genesis gives its founders, the entrants' keys of authorized
//! `Admit`s, and the keys of authorized `Node`s, none of them void. A record that is not
//! authorized binds nothing, since a log holds inert records anyone can sign. A key that a Mac
//! now out ever bound reads removed, whoever else claims it; else a key that is one member's
//! current node key is that member's, and one that is two members' is contested; else a key a
//! member bound and has since replaced is retired; every other key is unknown.

use super::{Body, Entrant, Founder, Kind, MemberId, Record, RecordId};
use crate::age::Recipient;
use crate::commons::binding::Binding;
use aterm_digest::Sha256;
use std::collections::{BTreeMap, BTreeSet};

/// One member, as the log says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// The binding its entry carried.
    pub binding: Binding,
    /// Its fleet node key: from its latest effective `Node`, else from its entry; `None` for a
    /// founder the genesis knew no node key of that has signed no `Node`.
    pub node_key: Option<[u8; 32]>,
    /// Its host name: from its latest effective `Name`, else from its entry.
    pub name: String,
    /// Its label: the `m<N>` it claimed, with its binding's suffix when another Mac claimed the
    /// same label.
    pub label: String,
    /// The member whose `Admit` let it in; `None` for a founder.
    pub admitted_by: Option<MemberId>,
    /// The record that let it in: its `Admit`, or the genesis.
    pub entry: RecordId,
}

/// Why a Mac the log once let in is out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    /// The signers of the effective `Remove`s of it, or of the quorum `Remove` that took it out.
    pub by: BTreeSet<MemberId>,
    /// A quorum `Remove` took it out, and voided its records after the cutoff.
    pub quorum: bool,
}

/// A quorum `Remove` that took effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quorum {
    /// The member it took out.
    pub member: MemberId,
    /// The record after which that member's records are void.
    pub cutoff: RecordId,
    /// The signers that counted: voters at the cutoff.
    pub signers: BTreeSet<MemberId>,
}

/// Where the quorum `Remove` of one member that F16's "Undo" row signs stands, as one log says
/// it ([`super::Log::undo`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undo {
    /// The member whose removals the shares undo.
    pub member: MemberId,
    /// The cutoff every share names ([`super::Log::undo_cutoff`]).
    pub cutoff: RecordId,
    /// The members at the cutoff, the member excluded: who may sign a share that counts.
    pub voters: BTreeSet<MemberId>,
    /// The voters whose share at this cutoff the log holds.
    pub signed: BTreeSet<MemberId>,
    /// A quorum `Remove` of the member at this cutoff took effect.
    pub took_effect: bool,
}

impl Undo {
    /// How many more shares the quorum needs: more than half of the voters, and at least two.
    #[must_use]
    pub fn needed(&self) -> usize {
        let quorum = (self.voters.len() / 2 + 1).max(2);
        quorum.saturating_sub(self.signed.len())
    }
}

/// Who a fleet node key is, as one log says it ([`Membership::node`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeStanding {
    /// The current node key of this member, and of no other.
    Member(MemberId),
    /// A key this Mac, now out ([`Membership::removed`]), bound: it never admits, whichever
    /// member also claims it. Of two such Macs, the lower actor.
    Removed(MemberId),
    /// A key this member bound and has since replaced with another. Of two such members, the
    /// lower actor.
    Retired(MemberId),
    /// The current node key of two or more members.
    Contested,
    /// A key no authorized record binds to a member or to a Mac now out.
    Unknown,
}

/// Membership as one log says it ([`super::Log::membership`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Membership {
    members: BTreeMap<MemberId, Member>,
    removed: BTreeMap<MemberId, Removal>,
    removed_recipients: BTreeSet<Recipient>,
    removed_labels: BTreeSet<String>,
    claimed_labels: BTreeSet<String>,
    quorums: Vec<Quorum>,
    voided: BTreeSet<RecordId>,
    unproved: Vec<Founder>,
    nodes: BTreeMap<[u8; 32], NodeStanding>,
}

impl Membership {
    /// Every member, by actor.
    pub const fn members(&self) -> &BTreeMap<MemberId, Member> {
        &self.members
    }
    /// One member.
    pub fn get(&self, member: &MemberId) -> Option<&Member> {
        self.members.get(member)
    }
    /// Whether `member` is in.
    pub fn contains(&self, member: &MemberId) -> bool {
        self.members.contains_key(member)
    }
    /// A member's label.
    pub fn label(&self, member: &MemberId) -> Option<&str> {
        self.members.get(member).map(|m| m.label.as_str())
    }
    /// Why a Mac is out now: the effective `Remove`s that name it, or the quorum `Remove` that
    /// took it out. `None` for a member, a Mac an `Admit` restored after its removal included, and
    /// for a Mac no effective `Remove` names. A Mac an effective `Remove` names is here even when
    /// no effective record ever let it in.
    pub fn removal(&self, member: &MemberId) -> Option<&Removal> {
        self.removed.get(member)
    }
    /// Every Mac out now ([`Membership::removal`]), by actor.
    pub const fn removed(&self) -> &BTreeMap<MemberId, Removal> {
        &self.removed
    }
    /// Every age recipient a founder seat or binding `Admit` binds to a Mac now out
    /// ([`Membership::removed`]) that no current member ([`Membership::members`]) holds.
    pub const fn removed_recipients(&self) -> &BTreeSet<Recipient> {
        &self.removed_recipients
    }
    /// Every label a founder seat or binding `Admit` binds to a Mac now out
    /// ([`Membership::removed`]) that no current member ([`Membership::members`]) holds.
    pub const fn removed_labels(&self) -> &BTreeSet<String> {
        &self.removed_labels
    }
    /// Every label held by a listed founder in the genesis or bound by a founder seat or
    /// binding `Admit` to a current member ([`Membership::members`]) or a Mac now out
    /// ([`Membership::removed`]).
    pub const fn claimed_labels(&self) -> &BTreeSet<String> {
        &self.claimed_labels
    }
    /// The quorum `Remove`s that took effect, by cutoff id then member.
    pub fn quorums(&self) -> &[Quorum] {
        &self.quorums
    }
    /// The records those quorums voided.
    pub const fn voided(&self) -> &BTreeSet<RecordId> {
        &self.voided
    }
    /// The founders the genesis lists with no binding whose recipient no member and no Mac taken
    /// out holds, in genesis order: each is in once it proves its device key to a member
    /// (`Log::challenge`).
    pub fn unproved(&self) -> &[Founder] {
        &self.unproved
    }
    /// Who the fleet node key `key` is: a member's, a Mac's now out, two members', one a member
    /// replaced, or unknown (the module's "node-key index").
    pub fn node(&self, key: &[u8; 32]) -> NodeStanding {
        self.nodes
            .get(key)
            .copied()
            .unwrap_or(NodeStanding::Unknown)
    }
    /// Every node key whose standing is not [`NodeStanding::Unknown`], by key.
    pub const fn nodes(&self) -> &BTreeMap<[u8; 32], NodeStanding> {
        &self.nodes
    }
}

/// What an entry says of the Mac it lets in: a founder the genesis binds, or an `Admit`'s
/// entrant.
#[derive(Clone, Copy)]
struct Seat<'a> {
    binding: &'a Binding,
    node_key: Option<[u8; 32]>,
    name: &'a str,
    label: &'a str,
}

impl<'a> Seat<'a> {
    fn founder(founder: &'a Founder) -> Option<Self> {
        founder.binding.as_ref().map(|binding| Self {
            binding,
            node_key: founder.node_key.filter(|k| *k != [0; 32]),
            name: &founder.name,
            label: &founder.label,
        })
    }
    fn entrant(entrant: &'a Entrant) -> Self {
        Self {
            binding: entrant.request.binding(),
            node_key: (entrant.node_key != [0; 32]).then_some(entrant.node_key),
            name: &entrant.name,
            label: &entrant.label,
        }
    }
    fn member(&self) -> MemberId {
        self.binding.actor()
    }
}

/// Evaluate `records`: a log's held records, the genesis first and each after its parents.
pub(super) fn evaluate(records: &[Record]) -> Membership {
    Graph::new(records).evaluate(&mut Memo::new())
}

/// How many authorization passes evaluating `records` runs: one for each set of candidates whose
/// void records it weighs under.
#[cfg(test)]
pub(super) fn authorization_passes(records: &[Record]) -> usize {
    let mut memo = Memo::new();
    let _ = Graph::new(records).evaluate(&mut memo);
    memo.len()
}

/// Which held records count for non-membership payloads (`Epoch`, `Wrap`, `Put`, `Delete`), and
/// strict causal precedence among them.
pub(super) struct Effective {
    counts: Vec<bool>,
    past: Vec<Bits>,
}

impl Effective {
    /// Whether record `index` is authorized, unvoided and not cut off by an effective removal.
    pub(super) fn counts(&self, index: usize) -> bool {
        self.counts.get(index).copied().unwrap_or(false)
    }

    /// Whether record `a` lies in record `b`'s strict causal past.
    pub(super) fn before(&self, a: usize, b: usize) -> bool {
        self.past[b].get(a)
    }
}

/// Compute [`Effective`] over `records` (genesis first, each after its parents).
pub(super) fn effective_records(records: &[Record]) -> Effective {
    Graph::new(records).effective()
}

/// Founding `Genesis` or effective `Admit` [`RecordId`] that grants `binding` a seat in `records`,
/// excluding actors taken out for good by an effective quorum `Remove`.
pub(super) fn historical_seat_entry(records: &[Record], binding: &Binding) -> Option<RecordId> {
    Graph::new(records).historical_seat_entry(binding)
}

/// Whether `binding` holds a founding or effective admitted seat in `records`, excluding actors
/// taken out for good by an effective quorum `Remove`.
pub(super) fn has_historical_seat(records: &[Record], binding: &Binding) -> bool {
    historical_seat_entry(records, binding).is_some()
}

/// The cutoff of F16's quorum `Remove` of `actor` ([`super::Log::undo_cutoff`]).
pub(super) fn undo_cutoff(records: &[Record], actor: &MemberId) -> Option<RecordId> {
    let graph = Graph::new(records);
    let mut scratch = AuthScratch::new(records.len());
    let plain = graph.authorized(&vec![false; records.len()], &mut Memo::new(), &mut scratch);
    graph
        .undo_cutoff(actor, &plain, &mut scratch)
        .map(|at| *records[at].id())
}

/// Where F16's quorum `Remove` of `actor` stands ([`super::Log::undo`]); `took_effect` is left
/// for the caller, which holds the whole membership.
pub(super) fn undo(records: &[Record], actor: &MemberId) -> Option<Undo> {
    let graph = Graph::new(records);
    let no_void = vec![false; records.len()];
    let mut scratch = AuthScratch::new(records.len());
    let plain = graph.authorized(&no_void, &mut Memo::new(), &mut scratch);
    let cutoff = graph.undo_cutoff(actor, &plain, &mut scratch)?;
    let voters: BTreeSet<MemberId> = graph
        .members_at_cutoff(cutoff, &no_void, &plain, &mut scratch)
        .unwrap_or_default()
        .into_iter()
        .filter(|mac| mac != actor)
        .collect();
    let signed = graph
        .cutoffs
        .iter()
        .filter(|&(&r, &at)| {
            at == cutoff
                && matches!(records[r].body(), Body::Remove { member, .. } if member == actor)
        })
        .map(|(&r, _)| *graph.signer(r))
        .filter(|signer| voters.contains(signer))
        .collect();
    Some(Undo {
        member: *actor,
        cutoff: *records[cutoff].id(),
        voters,
        signed,
        took_effect: false,
    })
}

/// Historical Commons writer bindings eligible to authenticate table records sealed under the
/// effective `Epoch` record at `epoch_index`.
pub(super) fn historical_bindings_for_epoch(
    records: &[Record],
    epoch_index: usize,
) -> Option<Vec<Binding>> {
    Graph::new(records).historical_bindings_for_epoch(epoch_index)
}

/// A set of record indexes.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Bits(Vec<u64>);

impl Bits {
    fn new(len: usize) -> Self {
        Self(vec![0; len.div_ceil(64)])
    }
    fn set(&mut self, at: usize) {
        self.0[at / 64] |= 1 << (at % 64);
    }
    fn get(&self, at: usize) -> bool {
        (self.0[at / 64] >> (at % 64)) & 1 == 1
    }
    fn union(&mut self, other: &Self) {
        for (word, more) in self.0.iter_mut().zip(&other.0) {
            *word |= *more;
        }
    }
    /// Whether `self` holds `cutoff` or any bit in `past`.
    fn intersects_upto(&self, past: &Self, cutoff: usize) -> bool {
        self.get(cutoff)
            || self
                .0
                .iter()
                .zip(&past.0)
                .any(|(word, mask)| word & mask != 0)
    }
}

/// Reusable per-pass memo for [`Graph::admit_effective_in_past`] and
/// [`Graph::admit_effective_upto`], keyed by an epoch counter so resetting per scope is `O(1)`.
struct AuthScratch {
    admit_epoch: Vec<u32>,
    epoch: u32,
}

impl AuthScratch {
    fn new(len: usize) -> Self {
        Self {
            admit_epoch: vec![0; len],
            epoch: 0,
        }
    }

    fn next_scope(&mut self) {
        if self.epoch >= u32::MAX - 2 {
            self.admit_epoch.fill(0);
            self.epoch = 2;
        } else {
            self.epoch += 2;
        }
    }
}

/// What a set of quorums does: records void everywhere, and members out for good.
struct Effects {
    void: Vec<bool>,
    out: BTreeSet<MemberId>,
}

/// One quorum `Remove` the log holds: the `Remove`s that name one member and one cutoff.
struct Candidate {
    member: MemberId,
    /// The cutoff's record index.
    cutoff: usize,
    /// The shares' record indexes.
    shares: Vec<usize>,
}

/// One candidate weighed: the members at its cutoff, its member excluded, and those whose shares
/// count.
#[derive(Default)]
struct Weighed {
    voters: BTreeSet<MemberId>,
    counted: BTreeSet<MemberId>,
}

impl Weighed {
    /// At least two shares count, and more than half of the voters.
    fn holds(&self) -> bool {
        self.counted.len() >= 2 && 2 * self.counted.len() > self.voters.len()
    }
}

/// Membership over one causally closed part of the log.
struct Standing {
    /// Each member and the entry (record index) that lets it in.
    members: BTreeMap<MemberId, usize>,
    /// The effective `Remove`s of each Mac.
    removes: BTreeMap<MemberId, Vec<usize>>,
    /// The effective `Admit`s of each Mac.
    admits: BTreeMap<MemberId, Vec<usize>>,
    /// The effective `Name`s each member signed.
    names: BTreeMap<MemberId, Vec<usize>>,
    /// The effective `Node`s each member signed.
    nodes: BTreeMap<MemberId, Vec<usize>>,
}

struct Graph<'a> {
    records: &'a [Record],
    /// `past[i]` holds every record in record `i`'s strict causal past.
    past: Vec<Bits>,
    /// Each `Remove`'s cutoff, by record index.
    cutoffs: BTreeMap<usize, usize>,
    /// Compact actor table (`0..actors.len()`) over every founder, signer, entrant and target.
    actors: Vec<MemberId>,
    /// `signer_actor[i]` is the compact actor index of `records[i].actor()`.
    signer_actor: Vec<usize>,
    /// Whether actor `a` is bound in the genesis.
    is_founder: Vec<bool>,
    /// For each target actor `a`, `(record_idx, cutoff_idx)` of every `Remove` naming `a`, in
    /// record order.
    removes_of: Vec<Vec<(usize, usize)>>,
    /// For each entrant actor `a`, `record_idx` of every `Admit` naming `a`, in record order.
    admits_of: Vec<Vec<usize>>,
}

impl<'a> Graph<'a> {
    fn new(records: &'a [Record]) -> Self {
        let at: BTreeMap<&RecordId, usize> = records
            .iter()
            .enumerate()
            .map(|(i, r)| (r.id(), i))
            .collect();
        let mut past: Vec<Bits> = Vec::with_capacity(records.len());
        for record in records {
            let mut mine = Bits::new(records.len());
            for parent in record.parents() {
                // A held record's parents are held, and earlier in the order.
                if let Some(&p) = at.get(parent) {
                    mine.union(&past[p]);
                    mine.set(p);
                }
            }
            past.push(mine);
        }
        let cutoffs: BTreeMap<usize, usize> = records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r.body() {
                Body::Remove { cutoff, .. } => at.get(cutoff).map(|&c| (i, c)),
                _ => None,
            })
            .collect();

        let mut actor_ids: BTreeMap<MemberId, usize> = BTreeMap::new();
        let mut actors: Vec<MemberId> = Vec::new();
        let mut intern = |id: MemberId| -> usize {
            if let Some(&idx) = actor_ids.get(&id) {
                idx
            } else {
                let idx = actors.len();
                actors.push(id);
                actor_ids.insert(id, idx);
                idx
            }
        };
        let listed = match records.first().map(Record::body) {
            Some(Body::Genesis { founders, .. }) => founders.as_slice(),
            _ => &[],
        };
        let mut founder_indices = Vec::new();
        for seat in listed.iter().filter_map(Seat::founder) {
            founder_indices.push(intern(seat.member()));
        }
        let mut signer_actor = Vec::with_capacity(records.len());
        for (i, record) in records.iter().enumerate() {
            if i == 0 {
                signer_actor.push(0);
                continue;
            }
            signer_actor.push(intern(*record.actor()));
            match record.body() {
                Body::Admit { entrant, .. } => {
                    let _ = intern(entrant.member());
                }
                Body::Remove { member, .. } => {
                    let _ = intern(*member);
                }
                _ => {}
            }
        }
        let mut is_founder = vec![false; actors.len()];
        for idx in founder_indices {
            is_founder[idx] = true;
        }
        let mut removes_of = vec![Vec::new(); actors.len()];
        let mut admits_of = vec![Vec::new(); actors.len()];
        for (i, record) in records.iter().enumerate().skip(1) {
            match record.body() {
                Body::Remove { member, .. } => {
                    if let Some(&cutoff) = cutoffs.get(&i) {
                        removes_of[actor_ids[member]].push((i, cutoff));
                    }
                }
                Body::Admit { entrant, .. } if record.actor() != &entrant.member() => {
                    admits_of[actor_ids[&entrant.member()]].push(i);
                }
                _ => {}
            }
        }

        Self {
            records,
            past,
            cutoffs,
            actors,
            signer_actor,
            is_founder,
            removes_of,
            admits_of,
        }
    }

    /// Whether record `a` is in record `b`'s strict causal past.
    fn before(&self, a: usize, b: usize) -> bool {
        self.past[b].get(a)
    }
    /// Whether record `a` is record `b` or in its causal past.
    fn upto(&self, a: usize, b: usize) -> bool {
        a == b || self.before(a, b)
    }
    fn signer(&self, i: usize) -> &MemberId {
        self.records[i].actor()
    }
    fn listed(&self) -> &'a [Founder] {
        match self.records[0].body() {
            Body::Genesis { founders, .. } => founders,
            _ => &[],
        }
    }
    /// The founders the genesis binds.
    fn founders(&self) -> impl Iterator<Item = Seat<'a>> {
        self.listed().iter().filter_map(Seat::founder)
    }

    fn evaluate(&self, memo: &mut Memo) -> Membership {
        let mut scratch = AuthScratch::new(self.records.len());
        let candidates = self.candidates();
        let accepted = self.accepted(&candidates, memo, &mut scratch);
        let effects = self.effects(&candidates, &accepted);
        let authorized = self.authorized(&effects.void, memo, &mut scratch);
        let all = |_: usize| true;
        let standing = self.standing(&all, &effects.void, &effects.out, &authorized);
        let plain = self.authorized(&vec![false; self.records.len()], memo, &mut scratch);
        let labels = self.labels(&plain, &authorized);
        let mut quorums: Vec<Quorum> = accepted
            .iter()
            .map(|&q| {
                let Candidate { member, cutoff, .. } = &candidates[q];
                Quorum {
                    member: *member,
                    cutoff: *self.records[*cutoff].id(),
                    signers: self.weigh(&candidates, q, None, memo, &mut scratch).counted,
                }
            })
            .collect();
        quorums.sort_by(|a, b| a.cutoff.cmp(&b.cutoff).then(a.member.cmp(&b.member)));

        let mut members = BTreeMap::new();
        for (member, &entry) in &standing.members {
            let Some((seat, admitted_by)) = self.entrant(entry, member) else {
                continue;
            };
            let name = self
                .latest(standing.names.get(member))
                .and_then(|i| match self.records[i].body() {
                    Body::Name { name } => Some(name.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| seat.name.to_owned());
            let node_key = self
                .latest(standing.nodes.get(member))
                .and_then(|i| match self.records[i].body() {
                    Body::Node { node_key, .. } => Some(*node_key),
                    _ => None,
                })
                .or(seat.node_key);
            members.insert(
                *member,
                Member {
                    binding: seat.binding.clone(),
                    node_key,
                    name,
                    label: labels
                        .get(member)
                        .cloned()
                        .unwrap_or_else(|| seat.label.to_owned()),
                    admitted_by,
                    entry: *self.records[entry].id(),
                },
            );
        }

        let mut removed = BTreeMap::new();
        for quorum in &quorums {
            let removal = removed.entry(quorum.member).or_insert_with(|| Removal {
                by: BTreeSet::new(),
                quorum: true,
            });
            removal.by.extend(quorum.signers.iter().copied());
        }
        for (member, removes) in &standing.removes {
            if members.contains_key(member) || effects.out.contains(member) {
                continue;
            }
            removed.insert(
                *member,
                Removal {
                    by: removes.iter().map(|&r| *self.signer(r)).collect(),
                    quorum: false,
                },
            );
        }
        let voided = (0..self.records.len())
            .filter(|&i| effects.void[i])
            .map(|i| *self.records[i].id())
            .collect();
        let active_recipients: BTreeSet<Recipient> =
            members.values().map(|m| m.binding.recipient()).collect();
        let active_labels: BTreeSet<&str> = members.values().map(|m| m.label.as_str()).collect();
        let mut seat_recipients: BTreeMap<MemberId, Recipient> = BTreeMap::new();
        for seat in self.bound_seats(&effects.out, &authorized, &effects.void, &standing) {
            seat_recipients
                .entry(seat.member())
                .or_insert_with(|| seat.binding.recipient());
        }
        let mut superseded_by: BTreeMap<MemberId, MemberId> = BTreeMap::new();
        for (&removed_actor, removal) in &removed {
            if removal.quorum {
                continue;
            }
            let Some(removes) = standing.removes.get(&removed_actor) else {
                continue;
            };
            if removes.is_empty() {
                continue;
            }
            if let Some((&active_actor, _)) = members.iter().find(|&(active_actor, m)| {
                (active_actor[..32] == removed_actor[..32]
                    || seat_recipients.get(&removed_actor) == Some(&m.binding.recipient()))
                    && standing
                        .members
                        .get(active_actor)
                        .is_some_and(|&entry| removes.iter().all(|&r| self.before(r, entry)))
            }) {
                superseded_by.insert(removed_actor, active_actor);
            }
        }
        let mut removed_recipients = BTreeSet::new();
        let mut removed_labels = BTreeSet::new();
        let mut claimed_labels: BTreeSet<String> =
            self.listed().iter().map(|f| f.label.clone()).collect();
        for seat in self.bound_seats(&effects.out, &authorized, &effects.void, &standing) {
            let member = seat.member();
            if members.contains_key(&member) || removed.contains_key(&member) {
                claimed_labels.insert(seat.label.to_owned());
            }
            if removed.contains_key(&member) && !superseded_by.contains_key(&member) {
                let recipient = seat.binding.recipient();
                if !active_recipients.contains(&recipient) {
                    removed_recipients.insert(recipient);
                }
                if !active_labels.contains(seat.label) {
                    removed_labels.insert(seat.label.to_owned());
                }
            }
        }
        let unproved = self.unproved(&members, &removed, &effects, &authorized, &standing);
        let nodes = self.nodes(
            &members,
            &removed,
            &superseded_by,
            &effects.out,
            (&authorized, &effects.void),
            &standing,
        );
        Membership {
            members,
            removed,
            removed_recipients,
            removed_labels,
            claimed_labels,
            quorums,
            voided,
            unproved,
            nodes,
        }
    }

    /// The latest of one signer's records `of`: its highest sequence, at a tie the lowest id.
    fn latest(&self, of: Option<&Vec<usize>>) -> Option<usize> {
        of?.iter().copied().max_by(|&a, &b| {
            let (a, b) = (&self.records[a], &self.records[b]);
            a.sequence().cmp(&b.sequence()).then(b.id().cmp(a.id()))
        })
    }

    /// Whether `member` is now out (either voided by a quorum `Remove` or removed by a standing
    /// `Remove` with no restoring `Admit` after it).
    fn is_removed_in(
        &self,
        member: &MemberId,
        out: &BTreeSet<MemberId>,
        standing: &Standing,
    ) -> bool {
        !standing.members.contains_key(member)
            && (out.contains(member) || standing.removes.contains_key(member))
    }

    /// Whether an `Admit` at `i` for `target` binds `target`'s recipient and entry node key: it
    /// counts (`!void`, `authorized`, not cut off, and not self-signed), and when both its signer
    /// and `target` are now out, another member also removed `target` after `i`.
    fn admit_binds(
        &self,
        i: usize,
        target: MemberId,
        out: &BTreeSet<MemberId>,
        authorized: &[bool],
        void: &[bool],
        standing: &Standing,
    ) -> bool {
        if void[i]
            || !authorized[i]
            || self.signer(i) == &target
            || self.cut(i, &standing.removes, &standing.admits)
        {
            return false;
        }
        if self.is_removed_in(&target, out, standing)
            && self.is_removed_in(self.signer(i), out, standing)
        {
            return standing.removes.get(&target).is_some_and(|rs| {
                rs.iter()
                    .any(|&r| self.signer(r) != self.signer(i) && self.before(i, r))
            });
        }
        true
    }

    /// The node-key index: the standing of every key the genesis, a binding `Admit` or an
    /// authorized `Node` binds, none of them void, given who is in and who is out.
    fn nodes(
        &self,
        members: &BTreeMap<MemberId, Member>,
        removed: &BTreeMap<MemberId, Removal>,
        superseded_by: &BTreeMap<MemberId, MemberId>,
        out: &BTreeSet<MemberId>,
        (authorized, void): (&[bool], &[bool]),
        standing: &Standing,
    ) -> BTreeMap<[u8; 32], NodeStanding> {
        // Every Mac each key was bound to. Never a raw scan: a log holds inert records anyone
        // can sign, and an `Admit` carries no proof by `entrant.node_key`, so a removed Mac's
        // cut-off, self-signed or Sybil `Admit` binds nothing.
        let mut bound: BTreeMap<[u8; 32], BTreeSet<MemberId>> = BTreeMap::new();
        for seat in self.founders() {
            if let Some(key) = seat.node_key.filter(|k| *k != [0; 32]) {
                bound.entry(key).or_default().insert(seat.member());
            }
        }
        for (i, record) in self.records.iter().enumerate().skip(1) {
            if !authorized[i] || void[i] {
                continue;
            }
            let (key, mac) = match record.body() {
                Body::Admit { entrant, .. }
                    if entrant.node_key != [0; 32]
                        && self.admit_binds(
                            i,
                            entrant.member(),
                            out,
                            authorized,
                            void,
                            standing,
                        ) =>
                {
                    (entrant.node_key, entrant.member())
                }
                Body::Node { node_key, .. } if *node_key != [0; 32] => (*node_key, *record.actor()),
                _ => continue,
            };
            bound.entry(key).or_default().insert(mac);
        }
        let mut current: BTreeMap<[u8; 32], Vec<MemberId>> = BTreeMap::new();
        for (actor, member) in members {
            if let Some(key) = member.node_key.filter(|k| *k != [0; 32]) {
                current.entry(key).or_default().push(*actor);
                bound.entry(key).or_default().insert(*actor);
            }
        }
        bound
            .into_iter()
            .filter_map(|(key, macs)| {
                let standing = if let Some(out) = macs
                    .iter()
                    .find(|m| removed.contains_key(*m) && !superseded_by.contains_key(*m))
                {
                    NodeStanding::Removed(*out)
                } else {
                    match current.get(&key).map(Vec::as_slice) {
                        Some([member]) => NodeStanding::Member(*member),
                        Some([_, _, ..]) => NodeStanding::Contested,
                        _ => NodeStanding::Retired(*macs.iter().find_map(|m| {
                            members
                                .contains_key(m)
                                .then_some(m)
                                .or_else(|| superseded_by.get(m))
                        })?),
                    }
                };
                Some((key, standing))
            })
            .collect()
    }

    /// Every seat the genesis or a binding `Admit` ([`Self::admit_binds`]) binds.
    fn bound_seats(
        &self,
        out: &BTreeSet<MemberId>,
        authorized: &[bool],
        void: &[bool],
        standing: &Standing,
    ) -> impl Iterator<Item = Seat<'a>> {
        let entrants = (1..self.records.len()).filter_map(move |i| {
            let Body::Admit { entrant, .. } = self.records[i].body() else {
                return None;
            };
            self.admit_binds(i, entrant.member(), out, authorized, void, standing)
                .then(|| Seat::entrant(entrant))
        });
        self.founders().chain(entrants)
    }

    /// The founders listed with no binding whose recipient no member and no Mac taken out holds:
    /// none that any non-void, authorized admission by another member binds to such a Mac.
    fn unproved(
        &self,
        members: &BTreeMap<MemberId, Member>,
        removed: &BTreeMap<MemberId, Removal>,
        effects: &Effects,
        authorized: &[bool],
        standing: &Standing,
    ) -> Vec<Founder> {
        // Every recipient a standing or independently removed admission binds to a member or a Mac
        // taken out. A removed Mac's own unauthorized, voided, cut-off or self-signed `Admit` can
        // neither release its own founding recipient nor squat another listed founder's recipient.
        let held: BTreeSet<Recipient> = self
            .bound_seats(&effects.out, authorized, &effects.void, standing)
            .filter(|seat| {
                let member = seat.member();
                members.contains_key(&member) || removed.contains_key(&member)
            })
            .map(|seat| seat.binding.recipient())
            .collect();
        self.listed()
            .iter()
            .filter(|f| f.binding.is_none() && !held.contains(&f.recipient))
            .cloned()
            .collect()
    }

    /// What record `entry` says of the Mac it lets in as `member`, and who signed it.
    fn entrant(&self, entry: usize, member: &MemberId) -> Option<(Seat<'a>, Option<MemberId>)> {
        match self.records[entry].body() {
            Body::Genesis { .. } => self
                .founders()
                .find(|seat| seat.member() == *member)
                .map(|seat| (seat, None)),
            Body::Admit { entrant, .. } => {
                Some((Seat::entrant(entrant), Some(*self.signer(entry))))
            }
            _ => None,
        }
    }

    /// Every quorum `Remove` with at least two signers: the `Remove`s that name one member and
    /// one cutoff, signed by others than that member.
    fn candidates(&self) -> Vec<Candidate> {
        let mut shares: BTreeMap<(usize, MemberId), Vec<usize>> = BTreeMap::new();
        for (&r, &cutoff) in &self.cutoffs {
            if let Body::Remove { member, .. } = self.records[r].body()
                && member != self.signer(r)
            {
                shares.entry((cutoff, *member)).or_default().push(r);
            }
        }
        shares
            .into_iter()
            .filter(|(_, shares)| {
                let signers: BTreeSet<&MemberId> = shares.iter().map(|&r| self.signer(r)).collect();
                signers.len() >= 2
            })
            .map(|((cutoff, member), shares)| Candidate {
                member,
                cutoff,
                shares,
            })
            .collect()
    }

    /// The candidates that take effect, by index: each holds under its own effects, and no
    /// candidate that takes effect defeats it. One defeats another when the records one voids
    /// undo the other while the other's do not undo one, or when both undo each other but only
    /// one's take votes away (the Macs its member let in after its cutoff). Two that undo each
    /// other only by voiding shares defeat neither, and both take effect.
    fn accepted(
        &self,
        candidates: &[Candidate],
        memo: &mut Memo,
        scratch: &mut AuthScratch,
    ) -> Vec<usize> {
        let no_void = vec![false; self.records.len()];
        let plain_auth = self.authorized(&no_void, memo, scratch);
        let mut plain_cutoffs: BTreeMap<usize, Option<BTreeSet<MemberId>>> = BTreeMap::new();
        let mut apart: BTreeMap<usize, Weighed> = BTreeMap::new();
        for (q, cand) in candidates.iter().enumerate() {
            let at_cutoff = plain_cutoffs.entry(cand.cutoff).or_insert_with(|| {
                self.members_at_cutoff(cand.cutoff, &no_void, &plain_auth, scratch)
            });
            let weighed = self.weigh_with_members(cand, at_cutoff.as_ref(), &no_void);
            if weighed.holds() {
                apart.insert(q, weighed);
            }
        }
        // `undoes`: the other no longer holds once one's effects void records too. `strips`: one's
        // effects also take a vote from a signer that counted for the other.
        let mut undoes = BTreeSet::new();
        let mut strips = BTreeSet::new();
        for &one in apart.keys() {
            let one_void = self.effects(candidates, &[one]).void;
            let one_mvoid = self.membership_void(&one_void);
            let one_auth = self.authorized(&one_void, memo, scratch);
            let mut one_cutoffs: BTreeMap<usize, Option<BTreeSet<MemberId>>> = BTreeMap::new();
            for (&other, alone) in &apart {
                if one == other {
                    continue;
                }
                let cand = &candidates[other];
                if !one_mvoid.intersects_upto(&self.past[cand.cutoff], cand.cutoff) {
                    let counted: BTreeSet<MemberId> = cand
                        .shares
                        .iter()
                        .filter(|&&s| !one_void[s])
                        .map(|&s| *self.signer(s))
                        .filter(|signer| alone.voters.contains(signer))
                        .collect();
                    if !(counted.len() >= 2 && 2 * counted.len() > alone.voters.len()) {
                        undoes.insert((one, other));
                    }
                    continue;
                }
                let at_cutoff = one_cutoffs.entry(cand.cutoff).or_insert_with(|| {
                    self.members_at_cutoff(cand.cutoff, &one_void, &one_auth, scratch)
                });
                let weighed = self.weigh_with_members(cand, at_cutoff.as_ref(), &one_void);
                if !weighed.holds() {
                    undoes.insert((one, other));
                }
                if !alone.counted.is_subset(&weighed.voters) {
                    strips.insert((one, other));
                }
            }
        }
        let defeats = |one: usize, other: usize| {
            undoes.contains(&(one, other))
                && (!undoes.contains(&(other, one))
                    || strips.contains(&(one, other)) && !strips.contains(&(other, one)))
        };
        let apart: Vec<usize> = apart.into_keys().collect();
        // The grounded verdict: in once every defeater is out, out once one defeater is in.
        // What stays open (a cycle of three or more) takes effect, as a mutual pair does.
        let mut verdict: BTreeMap<usize, bool> = BTreeMap::new();
        loop {
            let mut decided = false;
            for &other in &apart {
                if verdict.contains_key(&other) {
                    continue;
                }
                let by: Vec<Option<bool>> = apart
                    .iter()
                    .filter(|&&one| defeats(one, other))
                    .map(|one| verdict.get(one).copied())
                    .collect();
                if by.contains(&Some(true)) {
                    verdict.insert(other, false);
                    decided = true;
                } else if by.iter().all(|v| *v == Some(false)) {
                    verdict.insert(other, true);
                    decided = true;
                }
            }
            if !decided {
                break;
            }
        }
        apart
            .into_iter()
            .filter(|q| verdict.get(q) != Some(&false))
            .collect()
    }

    /// Candidate `q` weighed when its own effects and `other`'s void records: its voters, and
    /// the voters whose shares count.
    ///
    /// Only `other`'s effects are applied. `q`'s own void only its member's records outside the
    /// cutoff's past, and no share is its member's ([`Graph::candidates`] leaves out a Mac's
    /// removal of itself), so nothing weighed here reads them. The authorization pass is then
    /// keyed by the voided membership records alone.
    ///
    /// The voters are the members at the cutoff: membership over the cutoff's causal past, the
    /// member excluded, so nothing signed after the cutoff adds or takes one. A share counts
    /// when its signer votes and the share is not void; what happened to a voter after the
    /// cutoff does not take its share away, since that is what a share contests. An `Admit`
    /// that is not authorized is no cutoff, and nobody votes at it.
    fn weigh(
        &self,
        candidates: &[Candidate],
        q: usize,
        other: Option<usize>,
        memo: &mut Memo,
        scratch: &mut AuthScratch,
    ) -> Weighed {
        let set: Vec<usize> = other.into_iter().collect();
        let void = self.effects(candidates, &set).void;
        let authorized = self.authorized(&void, memo, scratch);
        let cand = &candidates[q];
        let at_cutoff = self.members_at_cutoff(cand.cutoff, &void, &authorized, scratch);
        self.weigh_with_members(cand, at_cutoff.as_ref(), &void)
    }

    fn weigh_with_members(
        &self,
        cand: &Candidate,
        members_at_cutoff: Option<&BTreeSet<MemberId>>,
        void: &[bool],
    ) -> Weighed {
        let Some(members) = members_at_cutoff else {
            return Weighed::default();
        };
        let voters: BTreeSet<MemberId> = members
            .iter()
            .copied()
            .filter(|mac| mac != &cand.member)
            .collect();
        let counted = cand
            .shares
            .iter()
            .filter(|&&s| !void[s])
            .map(|&s| *self.signer(s))
            .filter(|signer| voters.contains(signer))
            .collect();
        Weighed { voters, counted }
    }

    fn members_at_cutoff(
        &self,
        cutoff: usize,
        void: &[bool],
        authorized: &[bool],
        scratch: &mut AuthScratch,
    ) -> Option<BTreeSet<MemberId>> {
        if self.records[cutoff].kind() == Kind::Admit && !authorized[cutoff] {
            return None;
        }
        scratch.next_scope();
        let mut members = BTreeSet::new();
        for actor in 0..self.actors.len() {
            if self.is_member_upto(actor, cutoff, void, authorized, scratch) {
                members.insert(self.actors[actor]);
            }
        }
        Some(members)
    }

    /// Which [`Kind::Admit`] and [`Kind::Remove`] records `void` voids: non-membership payloads
    /// (`Name`, `Epoch`, `Wrap`, `Put`, `Delete`) never change authorization or voters.
    fn membership_void(&self, void: &[bool]) -> Bits {
        let mut bits = Bits::new(self.records.len());
        for (i, &is_void) in void.iter().enumerate().skip(1) {
            if is_void && matches!(self.records[i].kind(), Kind::Admit | Kind::Remove) {
                bits.set(i);
            }
        }
        bits
    }

    /// What the candidates `quorums` do: each one's member is out for good, and its records
    /// outside its cutoff's causal past are void.
    fn effects(&self, candidates: &[Candidate], quorums: &[usize]) -> Effects {
        let mut effects = Effects {
            void: vec![false; self.records.len()],
            out: BTreeSet::new(),
        };
        for &q in quorums {
            let Candidate { member, cutoff, .. } = &candidates[q];
            effects.out.insert(*member);
            for (i, is_void) in effects.void.iter_mut().enumerate().skip(1) {
                if self.signer(i) == member && !self.upto(i, *cutoff) {
                    *is_void = true;
                }
            }
        }
        effects
    }

    /// Whether each record is authorized when `void` holds the void records: its signer is a
    /// member in its strict causal past. Out for good does not reach back, so what a quorum's
    /// member signed before its cutoff stands. The voided membership records key the memo.
    fn authorized(&self, void: &[bool], memo: &mut Memo, scratch: &mut AuthScratch) -> Vec<bool> {
        let key = self.membership_void(void);
        if let Some(found) = memo.get(&key) {
            return found.clone();
        }
        let mut authorized = vec![false; self.records.len()];
        if !authorized.is_empty() {
            authorized[0] = true;
        }
        for i in 1..self.records.len() {
            scratch.next_scope();
            authorized[i] =
                self.is_member_before(self.signer_actor[i], i, void, &authorized, scratch);
        }
        memo.insert(key, authorized.clone());
        authorized
    }

    fn is_member_before(
        &self,
        actor: usize,
        i: usize,
        void: &[bool],
        authorized: &[bool],
        scratch: &mut AuthScratch,
    ) -> bool {
        let mut any_remove = false;
        for &(r, _) in &self.removes_of[actor] {
            if r >= i {
                break;
            }
            if !void[r] && authorized[r] && self.before(r, i) {
                any_remove = true;
                break;
            }
        }
        if !any_remove && self.is_founder[actor] {
            return true;
        }
        for &e in &self.admits_of[actor] {
            if e >= i {
                break;
            }
            if void[e] || !authorized[e] || !self.before(e, i) {
                continue;
            }
            let after_every_remove = self.removes_of[actor]
                .iter()
                .take_while(|&&(r, _)| r < i)
                .filter(|&&(r, _)| !void[r] && authorized[r] && self.before(r, i))
                .all(|&(r, _)| self.before(r, e));
            if after_every_remove && self.admit_effective_in_past(e, i, void, authorized, scratch) {
                return true;
            }
        }
        false
    }

    fn admit_effective_in_past(
        &self,
        e: usize,
        i: usize,
        void: &[bool],
        authorized: &[bool],
        scratch: &mut AuthScratch,
    ) -> bool {
        let cached = scratch.admit_epoch[e];
        if cached == scratch.epoch {
            return false;
        }
        if cached == scratch.epoch + 1 {
            return true;
        }
        let signer = self.signer_actor[e];
        for &(r, cutoff) in &self.removes_of[signer] {
            if r >= i {
                break;
            }
            if void[r] || !authorized[r] || !self.before(r, i) || self.upto(e, cutoff) {
                continue;
            }
            let restored = self.admits_of[signer]
                .iter()
                .copied()
                .take_while(|&e2| e2 < e)
                .any(|e2| {
                    !void[e2]
                        && authorized[e2]
                        && self.before(r, e2)
                        && self.before(e2, e)
                        && self.admit_effective_in_past(e2, i, void, authorized, scratch)
                });
            if !restored {
                scratch.admit_epoch[e] = scratch.epoch;
                return false;
            }
        }
        scratch.admit_epoch[e] = scratch.epoch + 1;
        true
    }

    fn is_member_upto(
        &self,
        actor: usize,
        cutoff: usize,
        void: &[bool],
        authorized: &[bool],
        scratch: &mut AuthScratch,
    ) -> bool {
        let mut any_remove = false;
        for &(r, _) in &self.removes_of[actor] {
            if r > cutoff {
                break;
            }
            if !void[r] && authorized[r] && self.upto(r, cutoff) {
                any_remove = true;
                break;
            }
        }
        if !any_remove && self.is_founder[actor] {
            return true;
        }
        for &e in &self.admits_of[actor] {
            if e > cutoff {
                break;
            }
            if void[e] || !authorized[e] || !self.upto(e, cutoff) {
                continue;
            }
            let after_every_remove = self.removes_of[actor]
                .iter()
                .take_while(|&&(r, _)| r <= cutoff)
                .filter(|&&(r, _)| !void[r] && authorized[r] && self.upto(r, cutoff))
                .all(|&(r, _)| self.before(r, e));
            if after_every_remove && self.admit_effective_upto(e, cutoff, void, authorized, scratch)
            {
                return true;
            }
        }
        false
    }

    fn admit_effective_upto(
        &self,
        e: usize,
        cutoff_scope: usize,
        void: &[bool],
        authorized: &[bool],
        scratch: &mut AuthScratch,
    ) -> bool {
        let cached = scratch.admit_epoch[e];
        if cached == scratch.epoch {
            return false;
        }
        if cached == scratch.epoch + 1 {
            return true;
        }
        let signer = self.signer_actor[e];
        for &(r, r_cutoff) in &self.removes_of[signer] {
            if r > cutoff_scope {
                break;
            }
            if void[r] || !authorized[r] || !self.upto(r, cutoff_scope) || self.upto(e, r_cutoff) {
                continue;
            }
            let restored = self.admits_of[signer]
                .iter()
                .copied()
                .take_while(|&e2| e2 < e)
                .any(|e2| {
                    !void[e2]
                        && authorized[e2]
                        && self.before(r, e2)
                        && self.before(e2, e)
                        && self.admit_effective_upto(e2, cutoff_scope, void, authorized, scratch)
                });
            if !restored {
                scratch.admit_epoch[e] = scratch.epoch;
                return false;
            }
        }
        scratch.admit_epoch[e] = scratch.epoch + 1;
        true
    }

    /// Membership over the records `scope` holds, which must be causally closed, given which
    /// records are void and authorized; the Macs `out` are no members. Only records in the
    /// scope's own past decide each one, and those come first in the order.
    fn standing(
        &self,
        scope: &dyn Fn(usize) -> bool,
        void: &[bool],
        out: &BTreeSet<MemberId>,
        authorized: &[bool],
    ) -> Standing {
        let counts = |i: usize| scope(i) && !void[i] && authorized[i];
        let mut removes: BTreeMap<MemberId, Vec<usize>> = BTreeMap::new();
        for (&r, _) in self.cutoffs.iter().filter(|(r, _)| counts(**r)) {
            if let Body::Remove { member, .. } = self.records[r].body() {
                removes.entry(*member).or_default().push(r);
            }
        }
        let mut admits: BTreeMap<MemberId, Vec<usize>> = BTreeMap::new();
        let mut names: BTreeMap<MemberId, Vec<usize>> = BTreeMap::new();
        let mut nodes: BTreeMap<MemberId, Vec<usize>> = BTreeMap::new();
        for i in (1..self.records.len()).filter(|&i| counts(i)) {
            let kind = self.records[i].kind();
            if !matches!(kind, Kind::Admit | Kind::Name | Kind::Node)
                || self.cut(i, &removes, &admits)
            {
                continue;
            }
            match self.records[i].body() {
                Body::Admit { entrant, .. } if self.signer(i) != &entrant.member() => {
                    admits.entry(entrant.member()).or_default().push(i);
                }
                Body::Admit { .. } => {}
                Body::Name { .. } => names.entry(*self.signer(i)).or_default().push(i),
                _ => nodes.entry(*self.signer(i)).or_default().push(i),
            }
        }
        let mut valid_entries: BTreeMap<MemberId, Vec<usize>> = BTreeMap::new();
        let founders = self.founders().map(|seat| (seat.member(), 0));
        let admitted = admits
            .iter()
            .flat_map(|(member, entries)| entries.iter().map(move |&e| (*member, e)));
        for (member, entry) in founders.chain(admitted) {
            let after_every_remove = removes
                .get(&member)
                .is_none_or(|rs| rs.iter().all(|&r| self.before(r, entry)));
            if out.contains(&member) || !after_every_remove {
                continue;
            }
            valid_entries.entry(member).or_default().push(entry);
        }
        let mut members: BTreeMap<MemberId, usize> = BTreeMap::new();
        for (member, entries) in valid_entries {
            // Among the entries no earlier standing entry of `member` precedes (the genesis
            // first, else the earliest admissions), pick the lowest id: the same whatever order
            // the records arrived in.
            if let Some(&entry) = entries
                .iter()
                .filter(|&&e| {
                    !entries
                        .iter()
                        .any(|&other| other != e && self.before(other, e))
                })
                .min_by_key(|&&e| (e != 0, self.records[e].id()))
            {
                members.insert(member, entry);
            }
        }
        Standing {
            members,
            removes,
            admits,
            names,
            nodes,
        }
    }

    fn effective(self) -> Effective {
        let mut scratch = AuthScratch::new(self.records.len());
        let candidates = self.candidates();
        let mut memo = Memo::new();
        let accepted = self.accepted(&candidates, &mut memo, &mut scratch);
        let effects = self.effects(&candidates, &accepted);
        let authorized = self.authorized(&effects.void, &mut memo, &mut scratch);
        let all = |_: usize| true;
        let standing = self.standing(&all, &effects.void, &effects.out, &authorized);
        let mut counts = vec![false; self.records.len()];
        if !counts.is_empty() {
            counts[0] = true;
        }
        for (i, slot) in counts.iter_mut().enumerate().skip(1) {
            *slot = !effects.void[i]
                && authorized[i]
                && !self.cut(i, &standing.removes, &standing.admits);
        }
        Effective {
            counts,
            past: self.past,
        }
    }

    fn historical_seat_entry(self, binding: &Binding) -> Option<RecordId> {
        let mut scratch = AuthScratch::new(self.records.len());
        let candidates = self.candidates();
        let mut memo = Memo::new();
        let accepted = self.accepted(&candidates, &mut memo, &mut scratch);
        let effects = self.effects(&candidates, &accepted);
        let actor = binding.actor();
        if effects.out.contains(&actor) {
            return None;
        }
        if self.founders().any(|seat| seat.binding == binding) {
            return Some(*self.records[0].id());
        }
        let authorized = self.authorized(&effects.void, &mut memo, &mut scratch);
        let all = |_: usize| true;
        let standing = self.standing(&all, &effects.void, &effects.out, &authorized);
        standing.admits.get(&actor).and_then(|entries| {
            let matching: Vec<usize> = entries
                .iter()
                .copied()
                .filter(|&i| {
                    self.admit_binds(
                        i,
                        actor,
                        &effects.out,
                        &authorized,
                        &effects.void,
                        &standing,
                    ) && matches!(
                        self.records[i].body(),
                        Body::Admit { entrant, .. } if entrant.request.binding() == binding
                    )
                })
                .collect();
            matching
                .iter()
                .copied()
                .filter(|&i| {
                    !matching
                        .iter()
                        .any(|&other| other != i && self.before(other, i))
                })
                .min_by_key(|&i| self.records[i].id())
                .map(|i| *self.records[i].id())
        })
    }

    fn historical_bindings_for_epoch(self, epoch_index: usize) -> Option<Vec<Binding>> {
        if epoch_index == 0 || epoch_index >= self.records.len() {
            return None;
        }
        let mut scratch = AuthScratch::new(self.records.len());
        let candidates = self.candidates();
        let mut memo = Memo::new();
        let accepted = self.accepted(&candidates, &mut memo, &mut scratch);
        let effects = self.effects(&candidates, &accepted);
        let authorized = self.authorized(&effects.void, &mut memo, &mut scratch);
        let all = |_: usize| true;
        let standing = self.standing(&all, &effects.void, &effects.out, &authorized);
        let counts = |i: usize| {
            !effects.void[i] && authorized[i] && !self.cut(i, &standing.removes, &standing.admits)
        };
        if !counts(epoch_index) {
            return None;
        }
        let epoch_id = *self.records[epoch_index].id();
        let mut commons_epochs = Vec::new();
        let mut wrapped_for_epoch = BTreeSet::new();
        for i in (1..self.records.len()).filter(|&i| counts(i)) {
            match self.records[i].body() {
                Body::Epoch {
                    audience: super::EpochAudience::Commons,
                    ..
                } => commons_epochs.push(i),
                Body::Wrap {
                    epoch, recipient, ..
                } if *epoch == epoch_id => {
                    wrapped_for_epoch.insert(*recipient);
                }
                _ => {}
            }
        }
        let allowed_recipient = |binding: &Binding| {
            wrapped_for_epoch.is_empty() || wrapped_for_epoch.contains(&binding.recipient())
        };
        let mut set: BTreeSet<Binding> = BTreeSet::new();
        for seat in self.founders() {
            let member = seat.member();
            if effects.out.contains(&member) {
                continue;
            }
            let removed_before_epoch = standing
                .removes
                .get(&member)
                .is_some_and(|rs| rs.iter().any(|&r| self.before(r, epoch_index)));
            if !removed_before_epoch && allowed_recipient(seat.binding) {
                set.insert(seat.binding.clone());
            }
        }
        for (&member, entries) in &standing.admits {
            if effects.out.contains(&member) {
                continue;
            }
            for &entry_idx in entries {
                if !self.admit_binds(
                    entry_idx,
                    member,
                    &effects.out,
                    &authorized,
                    &effects.void,
                    &standing,
                ) {
                    continue;
                }
                let removed_before_epoch = standing.removes.get(&member).is_some_and(|rs| {
                    rs.iter()
                        .any(|&r| !self.before(r, entry_idx) && self.before(r, epoch_index))
                });
                let superseded_before_entry = commons_epochs
                    .iter()
                    .any(|&e2| self.before(epoch_index, e2) && self.before(e2, entry_idx));
                if !removed_before_epoch
                    && !superseded_before_entry
                    && let Body::Admit { entrant, .. } = self.records[entry_idx].body()
                    && allowed_recipient(entrant.request.binding())
                {
                    set.insert(entrant.request.binding().clone());
                }
            }
        }
        Some(set.into_iter().collect())
    }

    /// The cutoff of a quorum `Remove` of `actor` that every honest Mac holding these records
    /// signs (§3.3.7, F16's "Undo"), by record index; `plain` is authorization with nothing void.
    ///
    /// `actor`'s acts are its `Admit`s and its `Remove`s of other Macs. An act is acknowledged
    /// when an independent Mac built on it before it saw a later act: an authorized record whose
    /// signer is neither `actor` nor a Mac `actor`'s `Admit`s let in, directly or through Macs
    /// those let in, holds the act in its causal past and holds no act of `actor` outside the
    /// act's own past; every act in an acknowledged act's past is acknowledged too. A record that
    /// is a `Remove` of `actor`, or holds one in its causal past, contests `actor` rather than
    /// carries on from it, so it acknowledges nothing. The first contested act is the
    /// unacknowledged one with the lowest sequence (at a tie the lowest id), and the cutoff is
    /// `actor`'s last record in its causal past, else the record that let `actor` in. `None`
    /// where `actor` has no contested act or no entry.
    ///
    /// One more rule keeps one Mac from taking the lab out for good: the Macs `actor` removed
    /// after the cutoff must be able to make the quorum there (more than half of the voters, and
    /// at least two). Where they cannot, an acknowledgment walked the cutoff past Macs the later
    /// removals took out: a Mac that `actor` let stay to carry on from its first removals, and
    /// then removed, leaves no quorum (one voter). The acknowledgments at the highest act are
    /// then set aside, one level at a time, and the first cutoff whose removed Macs can make the
    /// quorum is the one; where none can (a lab of two, or a removal of a few Macs whose members
    /// in remove `actor` and allow them again), the cutoff stands as every acknowledgment puts it.
    ///
    /// So `Admit`s `actor` made on a branch nobody else saw before its removals (a stolen Mac's
    /// Sybils, and the Macs they let in) fall after the cutoff, and those Macs never vote; an
    /// act the lab carried on from (a stolen Mac removed long ago) stands. A record that later
    /// arrives can move the cutoff; the Mac re-signs its share there.
    fn undo_cutoff(
        &self,
        actor: &MemberId,
        plain: &[bool],
        scratch: &mut AuthScratch,
    ) -> Option<usize> {
        let n = self.records.len();
        let mine: Vec<usize> = (1..n).filter(|&i| self.signer(i) == actor).collect();
        let mut let_in: BTreeSet<MemberId> = BTreeSet::new();
        let mut acts: Vec<usize> = Vec::new();
        for &i in &mine {
            match self.records[i].body() {
                Body::Admit { entrant, .. } => {
                    if entrant.member() != *actor {
                        let_in.insert(entrant.member());
                    }
                    acts.push(i);
                }
                Body::Remove { member, .. } if member != actor => acts.push(i),
                _ => {}
            }
        }
        // The Macs those let in, and so on: none of them is independent of `actor`.
        loop {
            let before = let_in.len();
            for i in 1..n {
                if let Body::Admit { entrant, .. } = self.records[i].body()
                    && let_in.contains(self.signer(i))
                    && entrant.member() != *actor
                {
                    let_in.insert(entrant.member());
                }
            }
            if let_in.len() == before {
                break;
            }
        }
        let contests: Vec<usize> = (1..n)
            .filter(|&r| {
                self.signer(r) != actor
                    && matches!(self.records[r].body(), Body::Remove { member, .. } if member == actor)
            })
            .collect();
        let mut acknowledged: Vec<usize> = Vec::new();
        for (r, &authorized) in plain.iter().enumerate().skip(1) {
            let signer = self.signer(r);
            if !authorized
                || signer == actor
                || let_in.contains(signer)
                || contests.iter().any(|&x| self.upto(x, r))
            {
                continue;
            }
            let seen: Vec<usize> = acts
                .iter()
                .copied()
                .filter(|&y| self.before(y, r))
                .collect();
            if let Some(&latest) = seen.iter().max_by_key(|&&y| self.records[y].sequence())
                && seen.iter().all(|&y| self.upto(y, latest))
            {
                acknowledged.push(latest);
            }
        }
        let order = |a: &usize, b: &usize| {
            let (a, b) = (&self.records[*a], &self.records[*b]);
            a.sequence().cmp(&b.sequence()).then(a.id().cmp(b.id()))
        };
        acknowledged.sort_by(order);
        acknowledged.dedup();
        let cutoff_given = |acknowledged: &[usize]| -> Option<usize> {
            let first = acts
                .iter()
                .copied()
                .filter(|&x| !acknowledged.iter().any(|&d| self.upto(x, d)))
                .min_by(order)?;
            let before = mine
                .iter()
                .copied()
                .filter(|&i| self.before(i, first))
                .max_by(|&a, &b| {
                    let (a, b) = (&self.records[a], &self.records[b]);
                    a.sequence().cmp(&b.sequence()).then(b.id().cmp(a.id()))
                });
            before.or_else(|| {
                let founder = self.founders().any(|seat| seat.member() == *actor);
                if founder {
                    return Some(0);
                }
                (1..n)
                    .filter(|&i| {
                        plain[i]
                            && matches!(self.records[i].body(),
                                Body::Admit { entrant, .. } if entrant.member() == *actor)
                    })
                    .min_by(|&a, &b| self.records[a].id().cmp(self.records[b].id()))
            })
        };
        let no_void = vec![false; n];
        let mut reachable = |cutoff: usize| -> bool {
            let Some(members) = self.members_at_cutoff(cutoff, &no_void, plain, scratch) else {
                return false;
            };
            let voters: BTreeSet<MemberId> =
                members.into_iter().filter(|mac| mac != actor).collect();
            let removed: BTreeSet<MemberId> = acts
                .iter()
                .filter(|&&x| !self.upto(x, cutoff))
                .filter_map(|&x| match self.records[x].body() {
                    Body::Remove { member, .. } if voters.contains(member) => Some(*member),
                    _ => None,
                })
                .collect();
            removed.len() >= (voters.len() / 2 + 1).max(2)
        };
        let settled = cutoff_given(&acknowledged)?;
        let mut kept = acknowledged;
        loop {
            let Some(cutoff) = cutoff_given(&kept) else {
                return Some(settled);
            };
            if reachable(cutoff) {
                return Some(cutoff);
            }
            let Some(&top) = kept.last() else {
                return Some(settled);
            };
            kept.retain(|&d| order(&d, &top).is_lt());
        }
    }

    /// Whether record `i` stops counting under `removes` and `admits`: its signer has a
    /// `Remove` there whose cutoff does not reach it, and no `Admit` of the signer that follows
    /// that `Remove` (a restoring one) is in its past.
    fn cut(
        &self,
        i: usize,
        removes: &BTreeMap<MemberId, Vec<usize>>,
        admits: &BTreeMap<MemberId, Vec<usize>>,
    ) -> bool {
        let signer = self.signer(i);
        let restored_after = |r: usize| {
            admits.get(signer).is_some_and(|entries| {
                entries
                    .iter()
                    .any(|&e| self.before(r, e) && self.before(e, i))
            })
        };
        removes.get(signer).is_some_and(|rs| {
            rs.iter()
                .any(|&r| !self.upto(i, self.cutoffs[&r]) && !restored_after(r))
        })
    }

    /// Every Mac's label, from every label claim the log holds.
    ///
    /// An `Admit` claims when it is authorized in its own past (`plain`, which no later record
    /// changes) or under the quorums that take effect (`settled`).
    fn labels(&self, plain: &[bool], settled: &[bool]) -> BTreeMap<MemberId, String> {
        // Every claim: the record that makes it, the Mac, and what the record says of it.
        let mut claims: Vec<(usize, MemberId, Seat<'a>)> = self
            .founders()
            .map(|seat| (0, seat.member(), seat))
            .collect();
        for (i, record) in self.records.iter().enumerate().skip(1) {
            if let Body::Admit { entrant, .. } = record.body()
                && self.signer(i) != &entrant.member()
                && (plain[i] || settled[i])
            {
                claims.push((i, entrant.member(), Seat::entrant(entrant)));
            }
        }
        // A Mac's first claims: those no other claim of it precedes.
        let first: Vec<&(usize, MemberId, Seat<'a>)> = claims
            .iter()
            .filter(|(i, member, _)| {
                !claims
                    .iter()
                    .any(|(j, other, _)| other == member && self.before(*j, *i))
            })
            .collect();
        let mut claimants: BTreeMap<&str, Vec<(usize, MemberId, Recipient)>> = BTreeMap::new();
        let mut mine: BTreeMap<MemberId, Vec<(usize, Seat<'a>)>> = BTreeMap::new();
        for (i, member, seat) in first {
            claimants
                .entry(seat.label)
                .or_default()
                .push((*i, *member, seat.binding.recipient()));
            mine.entry(*member).or_default().push((*i, *seat));
        }
        mine.into_iter()
            .filter_map(|(member, claims)| {
                let (at, base) = claims.iter().min_by_key(|(i, _)| self.records[*i].id())?;
                let base_rec = base.binding.recipient();
                let own_labels: BTreeSet<&str> =
                    claims.iter().map(|(_, seat)| seat.label).collect();
                // Another Mac's claim of the label clashes unless it already had this one in its
                // past: a claim that follows never relabels the Mac it follows, and a restored
                // Mac's new incarnation that follows its own earlier seat keeps that seat's label.
                let clash = own_labels.len() > 1
                    || claimants.get(base.label).is_some_and(|c| {
                        c.iter().any(|(j, other, other_rec)| {
                            *other != member
                                && !self.before(*at, *j)
                                && !(self.before(*j, *at)
                                    && (other[..32] == member[..32] || *other_rec == base_rec))
                        })
                    });
                let label = if clash {
                    let digest = Sha256::digest(base.binding.bytes());
                    format!("{}-{:02x}{:02x}", base.label, digest[0], digest[1])
                } else {
                    base.label.to_owned()
                };
                Some((member, label))
            })
            .collect()
    }
}

/// Authorization under the voided membership records of a candidate set.
type Memo = BTreeMap<Bits, Vec<bool>>;
