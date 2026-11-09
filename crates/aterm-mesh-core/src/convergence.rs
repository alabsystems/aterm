// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Law 5, value convergence: two Macs that read the same ops off their logs read the same value,
//! and the same conflict, at every entry `(audience, name)`, `[everyone]`'s included
//! (`docs/DESIGN-one-mesh-proofs-2026-11-05.md`, "Missing properties" 5).
//!
//! An entry is a causal multi-value register (`aterm-secrets`' `membership_log/entries.rs`). Its
//! ops are classified in the order the Mac's log holds them: an op is a carry when what its
//! writer held (the live ops of its frontier) has a winner and the op writes it again, a `Put` of
//! the winner's value or a `Delete` where every op held is a `Delete` (a reseal); a carry stands
//! for its winner's rep. The siblings are the ops no other op replaces (a later op replaces an op
//! in its past, a carry only ops writing what it carries), less each carry an op of another class
//! took out (its rep lies below that op). The winner is the sibling of the lowest (rep's record
//! id, own record id); the register is a conflict when a sibling writes otherwise than the winner.
//!
//! The decisions are the scalar `contract!` kernels below; each states `ensures result ==
//! spec_<name>(..) by <name>_is_exact`, where `spec_<name>` is a definition of
//! `convergence_island.rs`. That island (every Trust build) proves, for every number of ops, that
//! the classification is the one fixed point of its rule on a log that holds each record once and
//! after its causal past, so any two arrival orders of the same ops read the same winner, conflict,
//! siblings, carries and reps (`register_converges`, `arrival_orders_converge`), and two Macs whose
//! logs read the same ops do at every entry (`macs_converge`); the law needs only `beats` to be the
//! spec's, so it holds whatever the other kernels answer, and a register whose first sibling seen
//! wins reads two arrival orders apart (`first_seen_reads_arrival_order`).
//! `convergence_binding_island.rs` proves each compiled kernel equal to its spec and closes the
//! law for the compiled kernels.
//!
//! What the island takes from `aterm-secrets` as given, and what lies outside it: that two Macs
//! whose logs hold the same records read the same ops rests on both counting the same records,
//! opening the same ones (a reader skips an op whose epoch key it lacks, and every op of an
//! audience that leaves it out) and naming what an op writes by its content; and the steps after
//! the registers (dropping a winner whose anchor no entry holds, the conflicts among the `Hosts`
//! audiences' winners, which no `[everyone]` entry takes part in) read only the registers'
//! winners and the causal order between them.
//!
//! `Register` (a test seam: tests here and `aterm-secrets`' register oracle call it) is the
//! island's fold, step for step; until loops reach Trust's E6 import, the answer sheet
//! `convergence::tests` recomputes holds it to the island (TG12 removes it). `aterm-secrets`'
//! resolver is a bitset fold of its own that asks these kernels with the arguments its unions fix
//! (`replaces(true, carry, same)` for which rows join which union, `drops(carry, false,
//! rep_below_other)` for the ops a set keeps, its shortcuts skipping the sets where the spec's
//! `drops` answers false for every op), so its calls are right because the kernels are their
//! specs; it is held to `Register`, not proved, by its tests' differential, which checks every
//! register of at most 128 ops a test resolves (the fold asks `O(k^3)` causal questions).

#[cfg(any(test, debug_assertions))]
use crate::revocation::{all_n, any_n};

contract! {
    /// An op is a carry: what its writer held has a winner, and the op writes it again (a `Put`
    /// of the winner's value), or it is a `Delete` and every op the writer held is a `Delete`.
    pub fn carries(is_delete: bool, has_winner: bool, winner_same: bool, all_deletes: bool) -> bool
    clauses [ensures result == spec_carries(is_delete, has_winner, winner_same, all_deletes) by carries_is_exact]
    { has_winner && ((is_delete && all_deletes) || (!is_delete && winner_same)) }
}
contract! {
    /// A later op replaces an op in its causal past, unless it is a carry of another value.
    pub fn replaces(below: bool, later_carries: bool, same_op: bool) -> bool
    clauses [ensures result == spec_replaces(below, later_carries, same_op) by replaces_is_exact]
    { below && (!later_carries || same_op) }
}
contract! {
    /// An op takes a carry out of a set they share: it writes something else and the carry's
    /// rep lies in its causal past (a reseal loses to an edit or `Delete` made beside it).
    pub fn drops(carries: bool, same_op: bool, rep_below: bool) -> bool
    clauses [ensures result == spec_drops(carries, same_op, rep_below) by drops_is_exact]
    { carries && (!same_op && rep_below) }
}
contract! {
    /// A sibling beats another in the order every Mac reads the winner by: the lower record id of
    /// what it stands for, then the lower id of its own.
    pub fn beats(rep_less: bool, rep_same: bool, own_less: bool) -> bool
    clauses [ensures result == spec_beats(rep_less, rep_same, own_less) by beats_is_exact]
    { rep_less || (rep_same && own_less) }
}
contract! {
    /// A sibling that writes otherwise than the winner makes the register a conflict.
    pub fn conflicts(same_op: bool) -> bool
    clauses [ensures result == spec_conflicts(same_op) by conflicts_is_exact]
    { !same_op }
}

/// The kernels a [`Register`] asks: the compiled ones ([`KERNELS`]), or a test's mutant (the
/// island's `Kernels`).
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy)]
pub(crate) struct Kernels {
    pub(crate) carries: fn(bool, bool, bool, bool) -> bool,
    pub(crate) replaces: fn(bool, bool, bool) -> bool,
    pub(crate) drops: fn(bool, bool, bool) -> bool,
    pub(crate) beats: fn(bool, bool, bool) -> bool,
    pub(crate) conflicts: fn(bool) -> bool,
}

/// The compiled kernels.
#[cfg(any(test, debug_assertions))]
pub(crate) const KERNELS: Kernels = Kernels {
    carries,
    replaces,
    drops,
    beats,
    conflicts,
};

/// One op of a register: its record's id (a dense id the caller interns from the `RecordId`,
/// keeping their order: the winner order compares them), what it writes (two ops of one class
/// write the same thing), and whether it is a `Delete`.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Op {
    pub rid: usize,
    pub class: usize,
    pub delete: bool,
}

/// One Mac's view of one register: its ops in the order the Mac's log holds them (each record
/// once, every record after its causal past), and what the island's pass decided for each.
/// `before(a, b)` is whether record id `a` lies in record id `b`'s strict causal past; it must be
/// the causal order, never the order the records arrived in.
///
/// A test seam (`cfg(any(test, debug_assertions))`): no shipped code reads a register this way.
#[cfg(any(test, debug_assertions))]
pub struct Register<'a, B> {
    ops: &'a [Op],
    before: B,
    k: Kernels,
    /// Whether each op is a carry (the island's `St.c`, by position).
    carry: Vec<bool>,
    /// The record id of the op each stands for (the island's `St.p`, by position).
    rep: Vec<usize>,
    /// Whether each op is a sibling (the island's `sib`).
    sibling: Vec<bool>,
}

#[cfg(any(test, debug_assertions))]
impl<'a, B: Fn(usize, usize) -> bool> Register<'a, B> {
    /// The register of `ops` under the causal order `before`, classified by the island's pass.
    pub fn new(ops: &'a [Op], before: B) -> Self {
        Self::with_kernels(ops, before, KERNELS)
    }

    /// [`Register::new`] asking the kernels `k`.
    pub(crate) fn with_kernels(ops: &'a [Op], before: B, k: Kernels) -> Self {
        let mut register = Self {
            ops,
            before,
            k,
            carry: vec![false; ops.len()],
            rep: ops.iter().map(|op| op.rid).collect(),
            sibling: Vec::new(),
        };
        // The island's `pass`: each op in the log's order, from what was decided before it.
        for at in 0..ops.len() {
            let (carry, rep) = register.classify(at);
            register.carry[at] = carry;
            register.rep[at] = rep;
        }
        let maximal: Vec<bool> = (0..ops.len()).map(|a| !register.replaced(a)).collect();
        register.sibling = register.live(&maximal);
        register
    }

    /// The island's `between`: some op of the register lies strictly between records `a`, `b`.
    fn between(&self, a: usize, b: usize) -> bool {
        any_n(self.ops.len(), |m| {
            (self.before)(a, self.ops[m].rid) && (self.before)(self.ops[m].rid, b)
        })
    }

    /// The island's `dropped`: an op of `set` takes op `j` out.
    fn dropped(&self, set: &[bool], j: usize) -> bool {
        any_n(self.ops.len(), |m| {
            set[m]
                && (self.k.drops)(
                    self.carry[j],
                    self.ops[m].class == self.ops[j].class,
                    (self.before)(self.rep[j], self.ops[m].rid),
                )
        })
    }

    /// The island's `live`: `set` less each carry an op of it took out.
    fn live(&self, set: &[bool]) -> Vec<bool> {
        (0..self.ops.len())
            .map(|j| set[j] && !self.dropped(set, j))
            .collect()
    }

    /// The island's `beatsAt`: op `i` beats op `w`.
    fn beats_at(&self, i: usize, w: usize) -> bool {
        (self.k.beats)(
            self.rep[i] < self.rep[w],
            self.rep[i] == self.rep[w],
            self.ops[i].rid < self.ops[w].rid,
        )
    }

    /// The island's `win`: a left fold that takes an op of `set` only when it beats the one
    /// taken so far.
    fn win(&self, set: &[bool]) -> Option<usize> {
        set.iter()
            .enumerate()
            .fold(None, |best, (i, &held)| match best {
                None => held.then_some(i),
                Some(w) => Some(if held && self.beats_at(i, w) { i } else { w }),
            })
    }

    /// The island's `carryAt` and `repAt` for op `k`, from what was decided so far.
    fn classify(&self, k: usize) -> (bool, usize) {
        let x = self.ops[k].rid;
        let front: Vec<bool> = (0..self.ops.len())
            .map(|j| (self.before)(self.ops[j].rid, x) && !self.between(self.ops[j].rid, x))
            .collect();
        let held = self.live(&front);
        let winner = self.win(&held);
        let all_deletes = all_n(self.ops.len(), |j| !held[j] || self.ops[j].delete);
        let carry = (self.k.carries)(
            self.ops[k].delete,
            winner.is_some(),
            winner.is_some_and(|w| self.ops[w].class == self.ops[k].class),
            all_deletes,
        );
        let rep = match winner {
            Some(w) if carry => self.rep[w],
            _ => x,
        };
        (carry, rep)
    }

    /// The island's `replaced`: some op replaces op `a`.
    fn replaced(&self, a: usize) -> bool {
        any_n(self.ops.len(), |b| {
            (self.k.replaces)(
                (self.before)(self.ops[a].rid, self.ops[b].rid),
                self.carry[b],
                self.ops[a].class == self.ops[b].class,
            )
        })
    }

    /// Whether op `i` (a position) is a carry.
    pub fn carries(&self, i: usize) -> bool {
        self.carry[i]
    }

    /// The record id of the op op `i` stands for.
    pub fn rep(&self, i: usize) -> usize {
        self.rep[i]
    }

    /// Whether op `i` is a sibling.
    pub fn is_sibling(&self, i: usize) -> bool {
        self.sibling[i]
    }

    /// The winner's position (the island's `winnerOf`), if the register has a sibling.
    pub fn winner(&self) -> Option<usize> {
        self.win(&self.sibling)
    }

    /// Whether a sibling writes otherwise than the winner (the island's `conflictOf`).
    pub fn conflict(&self) -> bool {
        self.winner().is_some_and(|w| {
            any_n(self.ops.len(), |i| {
                self.sibling[i] && (self.k.conflicts)(self.ops[i].class == self.ops[w].class)
            })
        })
    }
}

#[cfg(test)]
mod tests;
