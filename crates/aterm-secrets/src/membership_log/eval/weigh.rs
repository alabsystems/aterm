// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Weighing the quorum candidates in pairs (the module's "The quorum `Remove`";
//! `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.2).
//!
//! One candidate undoes another when the other no longer holds with the first one's records void
//! too, and strips it when it also takes a vote from one of the other's signers. Read as defined,
//! each pair whose first candidate voids a membership record up to the other's cutoff costs one
//! membership pass there ([`Graph::members_at_cutoff`]: every actor's
//! [`Graph::is_member_upto`]), so Sybils that sign quorum removals at many cutoffs made the
//! weighing about cubic in records.
//!
//! **The walk** finds the same relations. For each cutoff a candidate holding apart names, it
//! steps through every candidate holding apart in one content order (by member, then the member's
//! own records by sequence, then the cutoff's id) and keeps the members at that cutoff under the
//! state its last re-check left (with nothing void at first). Each step tracks which reader
//! records up to the cutoff (a `Remove`, or an `Admit` another Mac signed: what `is_member_upto`
//! reads) are live (authorized and not void) in the step's state and not in the kept one, or the
//! other way round. A step where none differ from plain reuses the members with nothing void, as
//! the pair loop did; a step at an `Admit` the state leaves unauthorized has no voters; any other
//! step re-checks only the Macs those records can reach ([`Walk::reach`]) and keeps the result.
//!
//! What one Mac's membership at a cutoff reads (the read set of `is_member_upto` and
//! `admit_effective_upto`): the liveness of the `Remove`s and `Admit`s of it up to the cutoff, and
//! whether each of those `Admit`s is effective there, which reads the liveness of its signer's
//! `Remove`s and `Admit`s and whether the signer's earlier `Admit`s are effective. So a toggled
//! `Remove` or `Admit` reaches the Mac it is about; a toggled `Remove` of a signer reaches the
//! `Admit`s it signed outside that `Remove`'s cutoff; and a toggled or changed `Admit` of a signer
//! reaches, as a restorer, the `Admit`s the signer signed after it. Three pins stop the reach,
//! each from a `Remove` live in both states (live in every state, or not toggled and live in the
//! step's) up to the cutoff:
//! - a `Remove` that no `Admit` of its target follows keeps the target out in both states;
//! - a `Remove` that no `Admit` of its target followed by an `Admit` the target signed follows
//!   (it is final) leaves every `Admit` the target signed outside its cutoff ineffective in both,
//!   since nothing can restore it;
//! - so a toggled `Admit` its signer's final `Remove` cuts reads the same in both: ineffective.
//!
//! A step reads each toggled record once (at most every reader record up to the cutoff). What its
//! pins and reach read beside that (each pin's scan counted entry by entry, up to its first hit)
//! stops at the size of a full re-check (every actor, and every reader record up to the cutoff);
//! past it the step re-checks every actor, as the pair loop did.
//! So the walk never runs more `is_member_upto` calls than the pair loop on any log, and a step's
//! bookkeeping stays within twice its full re-check. What the pins settle for an actor (its
//! `Remove`s, and the `Admit`s it signed, up to the cutoff) is read once per cutoff. Two Macs that
//! hold the same records walk the same steps: every order and pin is read off record content, and
//! nothing breaks a tie by where a record sits in the order.

use super::{AuthScratch, Authorization, Bits, Candidate, Graph, Memo, Weighed};
use crate::membership_log::{Kind, MemberId};
use std::collections::{BTreeMap, BTreeSet};

mod relations;
pub(super) use relations::Relations;
#[cfg(test)]
mod relations_oracle_tests;

/// The candidates that hold with nothing void, weighed so, and what weighing them read.
pub(super) struct Apart {
    /// Authorization with nothing void.
    pub(super) plain: Authorization,
    /// The members at each cutoff a candidate names, with nothing void.
    pub(super) at_cutoff: BTreeMap<usize, Option<BTreeSet<MemberId>>>,
    /// Each candidate that holds, by index.
    pub(super) weighed: BTreeMap<usize, Weighed>,
}

impl Graph<'_, '_> {
    /// [`Graph::accepted`], with each candidate that holds apart and how it was weighed with
    /// nothing void, and the relations among them.
    pub(super) fn weigh_all(
        &self,
        candidates: &[Candidate],
        memo: &mut Memo,
        scratch: &mut AuthScratch,
    ) -> (Vec<usize>, Apart, Relations) {
        let apart = self.weigh_apart(candidates, memo, scratch);
        let relations = self.weigh_pairs(candidates, &apart, memo, scratch);
        let accepted = grounded(relations.active(), &relations);
        (accepted, apart, relations)
    }

    /// Every candidate weighed with nothing void, at the members its cutoff has with nothing void;
    /// the ones that hold.
    fn weigh_apart(
        &self,
        candidates: &[Candidate],
        memo: &mut Memo,
        scratch: &mut AuthScratch,
    ) -> Apart {
        let no_void = vec![false; self.records.len()];
        let plain = self.authorized(&no_void, memo, scratch);
        let mut at_cutoff: BTreeMap<usize, Option<BTreeSet<MemberId>>> = BTreeMap::new();
        let mut weighed = BTreeMap::new();
        for (q, cand) in candidates.iter().enumerate() {
            let at = at_cutoff
                .entry(cand.cutoff)
                .or_insert_with(|| self.members_at_cutoff(cand.cutoff, &no_void, &plain, scratch));
            let alone = self.weigh_with_members(cand, at.as_ref(), &no_void);
            if alone.holds() {
                weighed.insert(q, alone);
            }
        }
        Apart {
            plain,
            at_cutoff,
            weighed,
        }
    }

    /// The relations among the candidates that hold apart, by the walk (the module doc).
    fn weigh_pairs(
        &self,
        candidates: &[Candidate],
        apart: &Apart,
        memo: &mut Memo,
        scratch: &mut AuthScratch,
    ) -> Relations {
        let mut relations = {
            let keys: Vec<usize> = apart.weighed.keys().copied().collect();
            Relations::new(&keys).expect("held quorum candidates fit their relation budget")
        };
        if apart.weighed.len() < 2 {
            return relations;
        }
        let mut walk = Walk::new(self, candidates, apart, memo, scratch);
        let mut by_cutoff: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for &q in apart.weighed.keys() {
            by_cutoff.entry(candidates[q].cutoff).or_default().push(q);
        }
        for (&cutoff, here) in &by_cutoff {
            let Some(Some(members)) = apart.at_cutoff.get(&cutoff) else {
                continue;
            };
            walk.at_cutoff(cutoff, here, members, &mut relations, memo, scratch);
        }
        relations
    }
}

/// The candidates that take effect, from the relations among those that hold apart (`apart`,
/// ascending): the grounded labelling of the defeat relation. One defeats another it undoes,
/// unless that other undoes it back without being the only one that strips. A candidate is in
/// once every candidate that defeats it is out, and out once one that defeats it is in; what stays
/// open (a cycle of three or more) takes effect, as a mutual pair does. Each candidate is labelled
/// once. Each relation row is scanned at most twice: counting indegrees, then propagating its
/// candidate's label from the worklist. No second adjacency is retained.
///
/// # Panics
/// `apart` is not the exact active layout of `relations`.
pub(super) fn grounded(apart: &[usize], relations: &Relations) -> Vec<usize> {
    ground(apart, relations).accepted
}

struct Grounded {
    accepted: Vec<usize>,
    #[cfg(test)]
    scratch_bytes: usize,
    #[cfg(test)]
    output_bytes: usize,
}

fn ground(apart: &[usize], relations: &Relations) -> Grounded {
    assert_eq!(
        apart,
        relations.active(),
        "grounding uses the exact active candidate layout"
    );
    // How many candidates that defeat each one are not out yet.
    let mut standing = vec![0usize; apart.len()];
    for &one in apart {
        for other in relations.defeated_by(one) {
            standing[relations.position(other).unwrap()] += 1;
        }
    }
    let mut label: Vec<Option<bool>> = vec![None; apart.len()];
    let mut found_in = Vec::with_capacity(apart.len());
    for (i, &count) in standing.iter().enumerate() {
        if count == 0 {
            found_in.push(i);
        }
    }
    for &i in &found_in {
        label[i] = Some(true);
    }
    while let Some(i) = found_in.pop() {
        for out in relations.defeated_by(apart[i]) {
            let out = relations.position(out).unwrap();
            if label[out].is_some() {
                continue;
            }
            label[out] = Some(false);
            for next in relations.defeated_by(apart[out]) {
                let next = relations.position(next).unwrap();
                standing[next] -= 1;
                if standing[next] == 0 && label[next].is_none() {
                    label[next] = Some(true);
                    found_in.push(next);
                }
            }
        }
    }
    let mut accepted = Vec::with_capacity(apart.len());
    for (&q, &state) in apart.iter().zip(&label) {
        if state != Some(false) {
            accepted.push(q);
        }
    }
    Grounded {
        #[cfg(test)]
        scratch_bytes: standing.capacity() * std::mem::size_of::<usize>()
            + label.capacity() * std::mem::size_of::<Option<bool>>()
            + found_in.capacity() * std::mem::size_of::<usize>(),
        #[cfg(test)]
        output_bytes: accepted.capacity() * std::mem::size_of::<usize>(),
        accepted,
    }
}

#[cfg(test)]
fn grounded_with_storage(apart: &[usize], relations: &Relations) -> (Vec<usize>, usize, usize) {
    let result = ground(apart, relations);
    (result.accepted, result.scratch_bytes, result.output_bytes)
}

#[cfg(any(test, debug_assertions))]
pub(super) fn allocation_probe(dense: bool) -> usize {
    let active: Vec<usize> = (0..(crate::membership_log::MAX_RECORDS - 1) / 2).collect();
    let mut relations = Relations::new(&active).unwrap();
    if dense {
        for &one in &active {
            for &other in &active {
                if one != other {
                    relations.set_undoes(one, other);
                }
                if one < other {
                    relations.set_strips(one, other);
                }
            }
        }
    }
    let accepted = grounded(&active, &relations);
    if dense {
        assert_eq!(accepted.as_slice(), [0]);
    } else {
        assert_eq!(accepted, active);
    }
    accepted.len()
}

/// What the walk reads of the log once, whatever the candidates. The reader records are the
/// `Remove`s ([`Graph::removes_of`]) and the `Admit`s another Mac signed ([`Graph::admits_of`]):
/// every record `is_member_upto` and `admit_effective_upto` read the liveness of.
struct Readers {
    /// The actor a reader record is about (a `Remove`'s target, an `Admit`'s entrant);
    /// `usize::MAX` for any other record.
    about: Vec<usize>,
    /// A `Remove`'s cutoff; `usize::MAX` for any other record.
    cut: Vec<usize>,
    /// Per actor: the reader `Admit`s it signed, ascending.
    signed: Vec<Vec<usize>>,
    /// Per `Remove`: no `Admit` of its target follows it, so while it is live its target is out.
    pins_out: Vec<bool>,
    /// Per `Remove`: no `Admit` of its target that follows it precedes an `Admit` the target
    /// signed, so while it is live every `Admit` the target signed outside its cutoff is
    /// ineffective (nothing restores it).
    cuts_for_good: Vec<bool>,
    /// Per reader `Admit`: a `Remove` of its entrant precedes it and an `Admit` the entrant signed
    /// follows it, so it can restore that later `Admit`.
    restores: Vec<bool>,
    /// The reader records, ascending.
    list: Vec<usize>,
    /// The reader records, as a set.
    bits: Bits,
}

impl Readers {
    fn new(graph: &Graph<'_, '_>) -> Self {
        let n = graph.records.len();
        let mut about = vec![usize::MAX; n];
        let mut cut = vec![usize::MAX; n];
        let mut signed = vec![Vec::new(); graph.actors.len()];
        for (actor, removes) in graph.removes_of.iter().enumerate() {
            for &(r, cutoff) in removes {
                about[r] = actor;
                cut[r] = cutoff;
            }
        }
        for (actor, admits) in graph.admits_of.iter().enumerate() {
            for &e in admits {
                about[e] = actor;
            }
        }
        let mut list = Vec::new();
        let mut bits = Bits::new(n);
        for (i, &actor) in about.iter().enumerate() {
            if actor == usize::MAX {
                continue;
            }
            list.push(i);
            bits.set(i);
            if cut[i] == usize::MAX {
                signed[graph.signer_actor[i]].push(i);
            }
        }
        let mut pins_out = vec![false; n];
        let mut cuts_for_good = vec![false; n];
        let mut restores = vec![false; n];
        let by_actor = graph.removes_of.iter().zip(&graph.admits_of).zip(&signed);
        for ((removes, admits), signed_by) in by_actor {
            if removes.is_empty() {
                continue;
            }
            // Every record some `Admit` of the actor follows; every record some `Admit` it signed
            // follows; every record an `Admit` of it that precedes one it signed follows.
            let mut admitted_after = Bits::new(n);
            for &e in admits {
                graph.union_past_into(e, &mut admitted_after);
            }
            let mut signs_after = Bits::new(n);
            for &x in signed_by {
                graph.union_past_into(x, &mut signs_after);
            }
            let mut restored_after = Bits::new(n);
            let mut removed = Bits::new(n);
            for &(r, _) in removes {
                removed.set(r);
            }
            for &e in admits {
                if signs_after.get(e) {
                    graph.union_past_into(e, &mut restored_after);
                    restores[e] = graph.past_intersects(e, &removed);
                }
            }
            for &(r, _) in removes {
                pins_out[r] = !admitted_after.get(r);
                cuts_for_good[r] = !restored_after.get(r);
            }
        }
        Self {
            about,
            cut,
            signed,
            pins_out,
            cuts_for_good,
            restores,
            list,
            bits,
        }
    }

    fn is_remove(&self, r: usize) -> bool {
        self.cut[r] != usize::MAX
    }
}

/// The state one candidate's effects leave: what is void, what is authorized, and the reader
/// records whose liveness differs from plain's (ascending).
struct Class {
    void: Vec<bool>,
    auth: Authorization,
    delta: Vec<usize>,
}

impl Class {
    fn live(&self, r: usize) -> bool {
        !self.void[r] && self.auth[r]
    }
}

/// The reader records whose liveness one step of the walk toggles against the step before it, and
/// whether each is in the step's own delta; with a set of each when there are many, so a cutoff
/// filters them by words.
struct Diff {
    toggled: Vec<(usize, bool)>,
    dense: Option<(Bits, Bits)>,
}

/// What a `Remove` live in both states settles for one actor at the walk's cutoff, from the
/// `Remove`s live in every state; filled once per actor and cutoff.
struct Steady {
    stamp: Vec<u32>,
    epoch: u32,
    /// A live-everywhere `Remove` up to the cutoff keeps the actor out.
    out: Vec<bool>,
    /// The cutoffs of its live-everywhere final `Remove`s up to the cutoff.
    cuts: Vec<Vec<usize>>,
    /// Its `Remove`s up to the cutoff that keep it out, or are final, live in some state and not
    /// in every one: whether one is live in both states is the step's to read.
    some_out: Vec<Vec<usize>>,
    some_good: Vec<Vec<usize>>,
    /// The `Admit`s it signed up to the cutoff that reaching can change anything for
    /// ([`Step::victims`]), filled once per cutoff.
    victims: Vec<Vec<usize>>,
    victims_stamp: Vec<u32>,
    /// The records read filling these, for the bookkeeping count.
    read: usize,
}

/// A step's reach: from a toggled `Remove` of `g`, the `Admit`s `g` signed outside the cutoff
/// `Cut(g, κ)` names; from a toggled or changed `Admit` `a` of `g`, the `Admit`s `g` signed after
/// it (`After(g, a)`).
#[derive(Clone, Copy)]
enum Reach {
    Cut(usize, usize),
    After(usize, usize),
}

/// The walk's state: the classes, the order, and the marks and caches its steps reuse.
struct Walk<'w, 'a, 'cache> {
    graph: &'w Graph<'a, 'cache>,
    candidates: &'w [Candidate],
    apart: &'w Apart,
    readers: Readers,
    classes: Vec<Class>,
    class_of: BTreeMap<usize, usize>,
    /// The candidates holding apart, in walk order.
    order: Vec<usize>,
    /// `diffs[t]`: what step `t` toggles against step `t - 1` (against plain for the first).
    diffs: Vec<Diff>,
    /// Reader records live with nothing void and in every class.
    always: Vec<bool>,
    /// Per actor: its `Remove`s that keep it out (`out`) or are final (`good`), live in some state
    /// and not in every one.
    sometimes_out: Vec<Vec<usize>>,
    sometimes_good: Vec<Vec<usize>>,
    /// Each actor's index, by member.
    actor_at: BTreeMap<MemberId, usize>,
    /// Per candidate holding apart: the actors whose shares counted for it alone.
    counted_alone: BTreeMap<usize, Vec<usize>>,
    steady: Steady,
    step: u32,
    dirty_mark: Vec<u32>,
    reached_mark: Vec<u32>,
    after_mark: Vec<u32>,
}

impl<'w, 'a, 'cache> Walk<'w, 'a, 'cache> {
    fn new(
        graph: &'w Graph<'a, 'cache>,
        candidates: &'w [Candidate],
        apart: &'w Apart,
        memo: &mut Memo,
        scratch: &mut AuthScratch,
    ) -> Self {
        let n = graph.records.len();
        let readers = Readers::new(graph);
        let mut keys: BTreeMap<Bits, usize> = BTreeMap::new();
        let mut classes: Vec<Class> = Vec::new();
        let mut class_of = BTreeMap::new();
        for &q in apart.weighed.keys() {
            let void = graph.effects(candidates, &[q]).void;
            let key = graph.membership_void(&void);
            let class = if let Some(&class) = keys.get(&key) {
                class
            } else {
                let auth = graph.authorized(&void, memo, scratch);
                let delta = readers
                    .list
                    .iter()
                    .copied()
                    .filter(|&r| (!void[r] && auth[r]) != apart.plain[r])
                    .collect();
                classes.push(Class { void, auth, delta });
                keys.insert(key, classes.len() - 1);
                classes.len() - 1
            };
            class_of.insert(q, class);
        }
        let mut order: Vec<usize> = apart.weighed.keys().copied().collect();
        order.sort_by_cached_key(|&q| {
            let Candidate { member, cutoff, .. } = &candidates[q];
            let record = &graph.records[*cutoff];
            let own = record.actor() == member;
            (
                *member,
                own,
                if own { record.sequence() } else { 0 },
                *record.id(),
            )
        });
        let mut diffs = Vec::with_capacity(order.len());
        let mut in_some = Bits::new(n);
        for class in &classes {
            for &r in &class.delta {
                in_some.set(r);
            }
        }
        for (t, &q) in order.iter().enumerate() {
            let now = class_of[&q];
            let was = (t > 0).then(|| class_of[&order[t - 1]]);
            let toggled = if was == Some(now) {
                Vec::new()
            } else {
                let before = was.map_or(&[][..], |c| classes[c].delta.as_slice());
                symmetric(before, &classes[now].delta)
            };
            let dense = (toggled.len() > n / 64).then(|| {
                let (mut all, mut added) = (Bits::new(n), Bits::new(n));
                for &(r, now) in &toggled {
                    all.set(r);
                    if now {
                        added.set(r);
                    }
                }
                (all, added)
            });
            diffs.push(Diff { toggled, dense });
        }
        let always: Vec<bool> = (0..n)
            .map(|r| readers.bits.get(r) && apart.plain[r] && !in_some.get(r))
            .collect();
        let mut sometimes_out = vec![Vec::new(); graph.actors.len()];
        let mut sometimes_good = vec![Vec::new(); graph.actors.len()];
        for (actor, removes) in graph.removes_of.iter().enumerate() {
            for &(r, _) in removes {
                if in_some.get(r) && readers.pins_out[r] {
                    sometimes_out[actor].push(r);
                }
                if in_some.get(r) && readers.cuts_for_good[r] {
                    sometimes_good[actor].push(r);
                }
            }
        }
        let actor_at: BTreeMap<MemberId, usize> = graph
            .actors
            .iter()
            .enumerate()
            .map(|(i, m)| (*m, i))
            .collect();
        let counted_alone = apart
            .weighed
            .iter()
            .map(|(&q, alone)| (q, alone.counted.iter().map(|m| actor_at[m]).collect()))
            .collect();
        let actors = graph.actors.len();
        Self {
            graph,
            candidates,
            apart,
            readers,
            classes,
            class_of,
            order,
            diffs,
            always,
            sometimes_out,
            sometimes_good,
            actor_at,
            counted_alone,
            steady: Steady {
                stamp: vec![0; actors],
                epoch: 0,
                out: vec![false; actors],
                cuts: vec![Vec::new(); actors],
                some_out: vec![Vec::new(); actors],
                some_good: vec![Vec::new(); actors],
                victims: vec![Vec::new(); actors],
                victims_stamp: vec![0; actors],
                read: 0,
            },
            step: 0,
            dirty_mark: vec![0; actors],
            reached_mark: vec![0; n],
            after_mark: vec![0; n],
        }
    }

    /// Weigh every candidate holding apart at `cutoff` (`here`) beside every other one, walking
    /// the order from the members there with nothing void (`members`).
    fn at_cutoff(
        &mut self,
        cutoff: usize,
        here: &[usize],
        members: &BTreeSet<MemberId>,
        relations: &mut Relations,
        memo: &mut Memo,
        scratch: &mut AuthScratch,
    ) {
        let graph = self.graph;
        let n = graph.records.len();
        let mut member = vec![false; graph.actors.len()];
        for m in members {
            member[self.actor_at[m]] = true;
        }
        let mut count = members.len();
        let mut upto = Bits::new(n);
        graph.copy_past_into(cutoff, &mut upto);
        upto.set(cutoff);
        let budget = graph.actors.len() + upto.count_and(&self.readers.bits);
        self.steady.epoch += 1;
        // `pending`: the reader records up to the cutoff whose liveness differs between the state
        // `member` holds and the step's; `touched`, every record toggled since the last re-check;
        // `differ`, how many reader records up to the cutoff the step's state toggles against plain.
        let mut pending = Bits::new(n);
        let mut touched: Vec<usize> = Vec::new();
        let mut differ = 0usize;
        for t in 0..self.order.len() {
            let one = self.order[t];
            let diff = &self.diffs[t];
            let mut toggle = |r: usize, now: bool| {
                pending.flip(r);
                touched.push(r);
                if now {
                    differ += 1;
                } else {
                    differ -= 1;
                }
            };
            if let Some((all, added)) = &diff.dense {
                for r in all.and(&upto).ones() {
                    toggle(r, added.get(r));
                }
            } else {
                for &(r, now) in &diff.toggled {
                    if upto.get(r) {
                        toggle(r, now);
                    }
                }
            }
            if here.iter().all(|&j| j == one) {
                continue;
            }
            let class = &self.classes[self.class_of[&one]];
            if differ == 0 {
                // Up to the cutoff the state reads as plain: the members there with nothing void.
                for &j in here.iter().filter(|&&j| j != one) {
                    let alone = &self.apart.weighed[&j];
                    let counted: BTreeSet<MemberId> = self.candidates[j]
                        .shares
                        .iter()
                        .filter(|&&s| !class.void[s])
                        .map(|&s| *graph.signer(s))
                        .filter(|signer| alone.voters.contains(signer))
                        .collect();
                    if !(counted.len() >= 2 && 2 * counted.len() > alone.voters.len()) {
                        relations.set_undoes(one, j);
                    }
                    #[cfg(test)]
                    self.check_pair(one, j, &counted, None, memo);
                }
                continue;
            }
            if graph.records[cutoff].kind() == Kind::Admit && !class.auth[cutoff] {
                // No cutoff: nobody votes there.
                for &j in here.iter().filter(|&&j| j != one) {
                    relations.set_undoes(one, j);
                    relations.set_strips(one, j);
                    #[cfg(test)]
                    self.check_pair(one, j, &BTreeSet::new(), Some(0), memo);
                }
                continue;
            }
            touched.sort_unstable();
            touched.dedup();
            let toggled: Vec<usize> = touched.drain(..).filter(|&r| pending.get(r)).collect();
            let class_index = self.class_of[&one];
            let reached = self.reach(cutoff, class_index, &toggled, &pending, budget, scratch);
            let class = &self.classes[class_index];
            scratch.next_scope();
            let mut recheck = |a: usize| {
                let now = graph.is_member_upto(a, cutoff, &class.void, &class.auth, scratch);
                if now != member[a] {
                    member[a] = now;
                    if now {
                        count += 1;
                    } else {
                        count -= 1;
                    }
                }
            };
            match reached {
                Some(dirty) => dirty.into_iter().for_each(&mut recheck),
                None => (0..graph.actors.len()).for_each(&mut recheck),
            }
            for &r in &toggled {
                pending.flip(r);
            }
            #[cfg(test)]
            self.check_members(cutoff, class_index, &member);
            for &j in here.iter().filter(|&&j| j != one) {
                let cand = &self.candidates[j];
                let voters = count - usize::from(member[self.actor_at[&cand.member]]);
                let mut counted: Vec<usize> = cand
                    .shares
                    .iter()
                    .filter(|&&s| !class.void[s] && member[graph.signer_actor[s]])
                    .map(|&s| graph.signer_actor[s])
                    .collect();
                counted.sort_unstable();
                counted.dedup();
                if !(counted.len() >= 2 && 2 * counted.len() > voters) {
                    relations.set_undoes(one, j);
                }
                if self.counted_alone[&j].iter().any(|&x| !member[x]) {
                    relations.set_strips(one, j);
                }
                #[cfg(test)]
                {
                    let counted = counted.iter().map(|&a| graph.actors[a]).collect();
                    self.check_pair(one, j, &counted, Some(voters), memo);
                }
            }
        }
        let read = std::mem::take(&mut self.steady.read);
        book(scratch, read);
        #[cfg(test)]
        {
            scratch.fills += read;
        }
        #[cfg(not(test))]
        let _ = memo;
    }

    /// The actors whose membership at `cutoff` the reader records `toggled` can change between the
    /// kept state and `class`'s (`pending` holds `toggled`), or `None` once what the pins and the
    /// reach read passes `budget`, the size of a full re-check (the module doc's pins and reach).
    fn reach(
        &mut self,
        cutoff: usize,
        class: usize,
        toggled: &[usize],
        pending: &Bits,
        budget: usize,
        scratch: &mut AuthScratch,
    ) -> Option<Vec<usize>> {
        self.step = if self.step == u32::MAX {
            self.dirty_mark.fill(0);
            self.reached_mark.fill(0);
            self.after_mark.fill(0);
            1
        } else {
            self.step + 1
        };
        let step = Step {
            graph: self.graph,
            readers: &self.readers,
            always: &self.always,
            sometimes_out: &self.sometimes_out,
            sometimes_good: &self.sometimes_good,
            class: &self.classes[class],
            cutoff,
            pending,
        };
        let mark = self.step;
        // Each toggled record is read once (at most every reader record up to the cutoff); `work`
        // counts what the pins and the reach read beside them, against the budget.
        let mut work = 0usize;
        let mut dirty = Vec::new();
        let mut reach: Vec<Reach> = Vec::new();
        let mut cuts_seen: BTreeSet<(usize, usize)> = BTreeSet::new();
        let dirty_mark = &mut self.dirty_mark;
        let mut out_of_reach = |actor: usize, dirty: &mut Vec<usize>| {
            if dirty_mark[actor] != mark {
                dirty_mark[actor] = mark;
                dirty.push(actor);
            }
        };
        for &r in toggled {
            let about = self.readers.about[r];
            if self.readers.is_remove(r) {
                if !step.pinned_out(&mut self.steady, about, &mut work) {
                    out_of_reach(about, &mut dirty);
                }
                let cut = self.readers.cut[r];
                if !step.pinned_cut(&mut self.steady, about, cut, &mut work)
                    && cuts_seen.insert((about, cut))
                {
                    reach.push(Reach::Cut(about, cut));
                }
            } else {
                if step.dead(&mut self.steady, r, &mut work) {
                    continue;
                }
                if !step.pinned_out(&mut self.steady, about, &mut work) {
                    out_of_reach(about, &mut dirty);
                }
                if self.readers.restores[r] && self.after_mark[r] != mark {
                    self.after_mark[r] = mark;
                    reach.push(Reach::After(about, r));
                }
            }
            if work > budget {
                // The step's bookkeeping passed a full re-check: re-check every actor.
                abandon(scratch, toggled.len() + work);
                return None;
            }
        }
        while let Some(next) = reach.pop() {
            let (g, from) = match next {
                Reach::Cut(g, cut) => (g, cut),
                Reach::After(g, a) => (g, a),
            };
            if step.pinned_cut(&mut self.steady, g, from, &mut work) {
                continue;
            }
            step.victims(&mut self.steady, g);
            work += self.steady.victims[g].len();
            for i in 0..self.steady.victims[g].len() {
                let e = self.steady.victims[g][i];
                let reaches = match next {
                    Reach::Cut(_, cut) => !self.graph.upto(e, cut),
                    Reach::After(_, a) => self.graph.before(a, e),
                };
                if !reaches || self.reached_mark[e] == mark {
                    continue;
                }
                self.reached_mark[e] = mark;
                let entrant = self.readers.about[e];
                if !step.pinned_out(&mut self.steady, entrant, &mut work) {
                    out_of_reach(entrant, &mut dirty);
                }
                if self.readers.restores[e] && self.after_mark[e] != mark {
                    self.after_mark[e] = mark;
                    reach.push(Reach::After(entrant, e));
                }
            }
            if work > budget {
                // The step's bookkeeping passed a full re-check: re-check every actor.
                abandon(scratch, toggled.len() + work);
                return None;
            }
        }
        book(scratch, toggled.len() + work);
        Some(dirty)
    }
}

/// One slow step's view for the pins: the cutoff, the state stepped to, and the reader records up
/// to the cutoff whose liveness differs from the kept state's (`pending`).
struct Step<'s, 'a, 'cache> {
    graph: &'s Graph<'a, 'cache>,
    readers: &'s Readers,
    always: &'s [bool],
    sometimes_out: &'s [Vec<usize>],
    sometimes_good: &'s [Vec<usize>],
    class: &'s Class,
    cutoff: usize,
    pending: &'s Bits,
}

impl Step<'_, '_, '_> {
    /// Whether the `Remove` `r` up to the cutoff, live in some state but not every one, is live in
    /// both the kept state and the step's: not toggled, and live in the step's.
    fn steady(&self, r: usize) -> bool {
        !self.pending.get(r) && self.class.live(r)
    }

    /// Fill what the `Remove`s of `actor` up to the cutoff settle, once per cutoff (its cost is
    /// not the step's: each actor's `Remove`s that come before the cutoff once per cutoff in all).
    fn settle(&self, steady: &mut Steady, actor: usize) {
        if steady.stamp[actor] == steady.epoch {
            return;
        }
        steady.stamp[actor] = steady.epoch;
        let mut out = false;
        let mut cuts = std::mem::take(&mut steady.cuts[actor]);
        cuts.clear();
        // A record's past comes before it in the order, so the scan stops past the cutoff.
        for &(r, cut) in &self.graph.removes_of[actor] {
            if r > self.cutoff {
                break;
            }
            steady.read += 1;
            if !self.always[r] || !self.graph.upto(r, self.cutoff) {
                continue;
            }
            out |= self.readers.pins_out[r];
            if self.readers.cuts_for_good[r] {
                cuts.push(cut);
            }
        }
        steady.out[actor] = out;
        steady.cuts[actor] = cuts;
        let upto = |some: &[usize], read: &mut usize, into: &mut Vec<usize>| {
            into.clear();
            for &r in some {
                if r > self.cutoff {
                    break;
                }
                *read += 1;
                if self.graph.upto(r, self.cutoff) {
                    into.push(r);
                }
            }
        };
        let (mut some_out, mut some_good) = (
            std::mem::take(&mut steady.some_out[actor]),
            std::mem::take(&mut steady.some_good[actor]),
        );
        upto(&self.sometimes_out[actor], &mut steady.read, &mut some_out);
        upto(
            &self.sometimes_good[actor],
            &mut steady.read,
            &mut some_good,
        );
        steady.some_out[actor] = some_out;
        steady.some_good[actor] = some_good;
    }

    /// Whether a `Remove` live in both states keeps `actor` out at the cutoff in both: no `Admit`
    /// of it follows that `Remove`, so no entry has every live `Remove` of it in its past.
    fn pinned_out(&self, steady: &mut Steady, actor: usize, work: &mut usize) -> bool {
        self.settle(steady, actor);
        if steady.out[actor] {
            return true;
        }
        let some = &steady.some_out[actor];
        some.iter().any(|&r| {
            *work += 1;
            self.steady(r)
        })
    }

    /// Whether a final `Remove` of `g` live in both states, its cutoff up to `from`, leaves every
    /// `Admit` `g` signed that `from` reaches (outside `from` for a `Cut`, after it for an
    /// `After`) ineffective in both: no such `Admit` lies in that cutoff's past, and nothing can
    /// restore it.
    fn pinned_cut(&self, steady: &mut Steady, g: usize, from: usize, work: &mut usize) -> bool {
        self.settle(steady, g);
        if steady.cuts[g].iter().any(|&cut| {
            *work += 1;
            self.graph.upto(cut, from)
        }) {
            return true;
        }
        let some = &steady.some_good[g];
        some.iter().any(|&r| {
            *work += 1;
            self.steady(r) && self.graph.upto(self.readers.cut[r], from)
        })
    }

    /// Whether the reader `Admit` `e` is ineffective in both states: a final `Remove` of its
    /// signer live in both cuts it (its cutoff does not reach `e`), so whether `e` is live reads
    /// the same either way.
    fn dead(&self, steady: &mut Steady, e: usize, work: &mut usize) -> bool {
        let g = self.graph.signer_actor[e];
        self.settle(steady, g);
        if steady.cuts[g].iter().any(|&cut| {
            *work += 1;
            !self.graph.upto(e, cut)
        }) {
            return true;
        }
        let some = &steady.some_good[g];
        some.iter().any(|&r| {
            *work += 1;
            self.steady(r) && !self.graph.upto(e, self.readers.cut[r])
        })
    }

    /// Fill the `Admit`s `g` signed up to the cutoff whose reach can change anything: those whose
    /// entrant no live-everywhere `Remove` keeps out, and those that can restore. Reaching any
    /// other one marks no Mac and reaches nothing further, in any state. Once per cutoff.
    fn victims(&self, steady: &mut Steady, g: usize) {
        if steady.victims_stamp[g] == steady.epoch {
            return;
        }
        steady.victims_stamp[g] = steady.epoch;
        let mut victims = std::mem::take(&mut steady.victims[g]);
        victims.clear();
        for &e in &self.readers.signed[g] {
            if e > self.cutoff {
                break;
            }
            steady.read += 1;
            if !self.graph.upto(e, self.cutoff) {
                continue;
            }
            let entrant = self.readers.about[e];
            self.settle(steady, entrant);
            if self.readers.restores[e] || !steady.out[entrant] {
                victims.push(e);
            }
        }
        steady.victims[g] = victims;
    }
}

/// Count an abandoned step and its bookkeeping for the weighing cost tests.
fn abandon(scratch: &mut AuthScratch, work: usize) {
    book(scratch, work);
    #[cfg(test)]
    {
        scratch.abandoned += 1;
    }
}

/// Count a step's bookkeeping for the weighing cost tests.
fn book(scratch: &mut AuthScratch, work: usize) {
    #[cfg(test)]
    {
        scratch.bookkeeping += work;
    }
    #[cfg(not(test))]
    let _ = (scratch, work);
}

/// The symmetric difference of two ascending lists, each entry with whether it is in `now`.
fn symmetric(before: &[usize], now: &[usize]) -> Vec<(usize, bool)> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < before.len() || j < now.len() {
        match (before.get(i), now.get(j)) {
            (Some(&a), Some(&b)) if a == b => {
                i += 1;
                j += 1;
            }
            (Some(&a), Some(&b)) if a < b => {
                out.push((a, false));
                i += 1;
            }
            (Some(&a), None) => {
                out.push((a, false));
                i += 1;
            }
            (_, Some(&b)) => {
                out.push((b, true));
                j += 1;
            }
            (None, None) => break,
        }
    }
    out
}

impl Bits {
    fn flip(&mut self, at: usize) {
        self.0[at / 64] ^= 1 << (at % 64);
    }
    fn count_and(&self, other: &Self) -> usize {
        self.0
            .iter()
            .zip(&other.0)
            .map(|(a, b)| (a & b).count_ones() as usize)
            .sum()
    }
    fn and(&self, other: &Self) -> Self {
        Self(self.0.iter().zip(&other.0).map(|(a, b)| a & b).collect())
    }
    /// The set indexes, ascending.
    fn ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(w, &word)| {
            let mut word = word;
            std::iter::from_fn(move || {
                (word != 0).then(|| {
                    let bit = word.trailing_zeros() as usize;
                    word &= word - 1;
                    w * 64 + bit
                })
            })
        })
    }
}

#[cfg(test)]
impl Walk<'_, '_, '_> {
    /// In a checked walk, the members the walk keeps at `cutoff` are the members there under the
    /// class, by a full pass.
    fn check_members(&self, cutoff: usize, class: usize, member: &[bool]) {
        if !CHECKED.with(std::cell::Cell::get) {
            return;
        }
        let class = &self.classes[class];
        let mut fresh = AuthScratch::new(self.graph.records.len());
        let want = self
            .graph
            .members_at_cutoff(cutoff, &class.void, &class.auth, &mut fresh)
            .expect("a slow step's cutoff is authorized");
        let kept: BTreeSet<MemberId> = (0..member.len())
            .filter(|&a| member[a])
            .map(|a| self.graph.actors[a])
            .collect();
        assert_eq!(kept, want, "the walk's members at cutoff {cutoff}");
    }

    /// In a checked walk, the pair `(one, j)` weighed as the definition weighs it
    /// ([`Graph::weigh`]): the same shares count, from the same number of voters.
    fn check_pair(
        &self,
        one: usize,
        j: usize,
        counted: &BTreeSet<MemberId>,
        voters: Option<usize>,
        memo: &mut Memo,
    ) {
        if !CHECKED.with(std::cell::Cell::get) {
            return;
        }
        let mut fresh = AuthScratch::new(self.graph.records.len());
        let want = self
            .graph
            .weigh(self.candidates, j, Some(one), memo, &mut fresh);
        assert_eq!(*counted, want.counted, "counted for ({one}, {j})");
        let voters = voters.unwrap_or_else(|| self.apart.weighed[&j].voters.len());
        assert_eq!(voters, want.voters.len(), "voters for ({one}, {j})");
    }
}

#[cfg(test)]
thread_local! {
    /// Whether this thread's walks check every step against a full pass.
    pub(super) static CHECKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The pair loop the walk replaced, kept to check the walk against and to count what it cost:
/// each candidate holding apart weighed beside every other one at a full membership pass, and
/// the verdict found by sweeping until nothing changes.
#[cfg(test)]
impl Graph<'_, '_> {
    pub(super) fn accepted_by_pairs(
        &self,
        candidates: &[Candidate],
        memo: &mut Memo,
        scratch: &mut AuthScratch,
    ) -> (Vec<usize>, Relations) {
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
                if !one_mvoid.get(cand.cutoff) && !self.past_intersects(cand.cutoff, &one_mvoid) {
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
        let accepted = apart
            .iter()
            .copied()
            .filter(|q| verdict.get(q) != Some(&false))
            .collect();
        let mut relations =
            Relations::new(&apart).expect("test quorum candidates fit their relation budget");
        for (one, other) in undoes {
            relations.set_undoes(one, other);
        }
        for (one, other) in strips {
            relations.set_strips(one, other);
        }
        (accepted, relations)
    }
}
