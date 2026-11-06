// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P1, revocation: which age recipients a Mac's view of the membership log revokes, which are
//! current, and which epochs a writer may seal under (one-mesh §4.2 M3 and M4,
//! `docs/design/one-mesh-proofs/P1-revocation.md`).
//!
//! A view is a [`Table`]: every bound seat row (a founder's genesis binding or a binding `Admit`;
//! superseded seats and the rows of Macs now out included), every `Remove` (an accepted quorum's
//! shares included; a quorum's void marks one dead), and the causal order. Row `i` is taken out by
//! `Remove` `j` when the `Remove` stands, names the row's actor, and either is a quorum's (the
//! actor is out for good) or does not lie in the row's entry's past (a `Remove` beats a
//! concurrent `Admit`). A taken-out row revokes its recipient unless another row restores it: a row
//! of the same recipient, not taken out itself, whose entry has every `Remove` that took the row
//! in its past. Nothing else plays a role: not superseded seats, not actors restored under another
//! key, not co-holders, and never the order the records arrived in.
//!
//! The decisions are the scalar `contract!` kernels below; each states `ensures result ==
//! spec_<name>(..) by <name>_is_exact`, where `spec_<name>` is a definition of
//! `revocation_island.rs`. That island (every Trust build) proves, for every size, the decision
//! exact and sound, the A15 law, finality but for a person's re-grant or a quorum's void, the gate
//! that confidentiality needs, that the answers depend only on the records and their causal order
//! (law 1), and that no automatic path restores a revoked recipient (law 2), for a World that asks
//! its kernels as parameters. `revocation_binding_island.rs` proves each compiled kernel equal to
//! its spec and closes the laws for the World that asks the compiled kernels. [`Table`]'s folds
//! are the island's folds, step for step and with no early exit; until loops reach Trust's E6
//! import, the answer sheet the tests recompute holds them to the island (TG12 removes it).

/// One bound seat row: its actor (`MemberId`), its age recipient and its entry record, as dense
/// ids the caller interns (the decision only compares them). Entry and `Remove` records are
/// indices the caller's `before` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub actor: usize,
    pub recipient: usize,
    pub entry: usize,
}

/// One `Remove`: the actor it names, its record, whether it stands (`false`: a quorum voided it),
/// and whether it is an accepted quorum's share (its actor is out for good).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Remove {
    pub actor: usize,
    pub record: usize,
    pub live: bool,
    pub quorum: bool,
}

contract! {
    /// `Remove` takes a row out: it stands, names the row's actor, and is a quorum's or does not
    /// lie in the row's entry's past.
    pub fn takes(live: bool, names_actor: bool, out_for_good: bool, remove_before_entry: bool) -> bool
    clauses [ensures result == spec_takes(live, names_actor, out_for_good, remove_before_entry) by takes_is_exact]
    { live && (names_actor && (out_for_good || !remove_before_entry)) }
}
contract! {
    /// A `Remove` that took a row lies in a record's causal past (or took nothing).
    pub fn precedes(took: bool, before: bool) -> bool
    clauses [ensures result == spec_precedes(took, before) by precedes_is_exact]
    { !took || before }
}
contract! {
    /// A row restores another's recipient: the same recipient, not out itself, and every `Remove`
    /// that took the other lies in its entry's past.
    pub fn restores(same_recipient: bool, restorer_out: bool, every_taker_before: bool) -> bool
    clauses [ensures result == spec_restores(same_recipient, restorer_out, every_taker_before) by restores_is_exact]
    { same_recipient && (!restorer_out && every_taker_before) }
}
contract! {
    /// A row taken out that no row restores revokes its recipient.
    pub fn row_revokes(taken_out: bool, holder_after: bool) -> bool
    clauses [ensures result == spec_row_revokes(taken_out, holder_after) by row_revokes_is_exact]
    { taken_out && !holder_after }
}
contract! {
    /// An epoch keeps a row's recipient out when the row does not revoke; or every `Remove` that
    /// took it lies in the epoch's causal past; or no row that is not out binds the recipient and
    /// some `Remove` that took it lies there (no Mac that holds the epoch can then read the
    /// recipient as current: the island's `restorer_is_unseen`).
    pub fn gate_row(revokes: bool, every_taker_before: bool, recipient_live: bool, some_taker_before: bool) -> bool
    clauses [ensures result == spec_gate_row(revokes, every_taker_before, recipient_live, some_taker_before) by gate_row_is_exact]
    { !revokes || (every_taker_before || (!recipient_live && some_taker_before)) }
}
contract! {
    /// A recipient is current when a row that is not out binds it and nothing revokes it.
    pub fn current(bound_live: bool, revoked: bool) -> bool
    clauses [ensures result == spec_current(bound_live, revoked) by current_is_exact]
    { bound_live && !revoked }
}
contract! {
    /// A newest epoch wraps a revoked recipient (a held `Wrap` to it): the defence in depth that
    /// keeps a carry of a mixed-version Mac due.
    pub fn wraps_removed(revoked: bool, wrapped: bool) -> bool
    clauses [ensures result == spec_wraps_removed(revoked, wrapped) by wraps_removed_is_exact]
    { revoked && wrapped }
}
contract! {
    /// A newest epoch is due a rotation, and no writer seals under it, when its gate is shut or it
    /// wraps a revoked recipient. `rekey_dues` and `rotation_due` both ask this one rule.
    pub fn head_due(gate_open: bool, wraps_revoked: bool) -> bool
    clauses [ensures result == spec_head_due(gate_open, wraps_revoked) by head_due_is_exact]
    { !gate_open || wraps_revoked }
}

#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "revocation_island.rs"]
mod island;

/// The island's `anyN`: `||` folded left over `0..n`, every index asked, no early exit.
pub(crate) fn any_n(n: usize, mut p: impl FnMut(usize) -> bool) -> bool {
    let mut acc = false;
    for i in 0..n {
        acc |= p(i);
    }
    acc
}

/// The island's `allN`: `&&` folded left over `0..n`, every index asked, no early exit.
fn all_n(n: usize, mut p: impl FnMut(usize) -> bool) -> bool {
    let mut acc = true;
    for i in 0..n {
        acc &= p(i);
    }
    acc
}

/// One Mac's view of the membership log, as the revocation decision reads it. `before(a, b)` is
/// whether record `a` lies in record `b`'s strict causal past; it must be the causal order
/// (`Reach`), never the order the records arrived in.
pub struct Table<'a, B> {
    rows: &'a [Row],
    removes: &'a [Remove],
    before: B,
    /// Whether each row is out: some `Remove` takes it (the island's `isOut`).
    out: Vec<bool>,
    /// Whether each row revokes its recipient (the island's `revokes`).
    revoking: Vec<bool>,
}

impl<'a, B: Fn(usize, usize) -> bool> Table<'a, B> {
    /// The view of `rows` and `removes` under the causal order `before`. Each row's `isOut` and
    /// `revokes` is folded once, here.
    pub fn new(rows: &'a [Row], removes: &'a [Remove], before: B) -> Self {
        let mut table = Self {
            rows,
            removes,
            before,
            out: Vec::new(),
            revoking: Vec::new(),
        };
        table.out = (0..rows.len())
            .map(|i| any_n(removes.len(), |j| table.takes(j, i)))
            .collect();
        table.revoking = (0..rows.len())
            .map(|i| row_revokes(table.out[i], table.restored(i)))
            .collect();
        table
    }

    /// The rows, as given.
    pub const fn rows(&self) -> &'a [Row] {
        self.rows
    }

    /// Whether `Remove` `j` takes row `i` out.
    pub fn takes(&self, j: usize, i: usize) -> bool {
        let (remove, row) = (&self.removes[j], &self.rows[i]);
        takes(
            remove.live,
            remove.actor == row.actor,
            remove.quorum,
            (self.before)(remove.record, row.entry),
        )
    }

    /// Whether some `Remove` takes row `i` out.
    pub fn is_out(&self, i: usize) -> bool {
        self.out[i]
    }

    /// Whether every `Remove` that took row `i` lies in record `x`'s causal past.
    pub fn takers_before(&self, i: usize, x: usize) -> bool {
        all_n(self.removes.len(), |j| {
            precedes(self.takes(j, i), (self.before)(self.removes[j].record, x))
        })
    }

    /// Whether some `Remove` that took row `i` lies in record `x`'s causal past.
    pub fn some_taker_before(&self, i: usize, x: usize) -> bool {
        any_n(self.removes.len(), |j| {
            self.takes(j, i) && (self.before)(self.removes[j].record, x)
        })
    }

    /// Whether row `m` restores row `i`'s recipient.
    pub fn restores(&self, i: usize, m: usize) -> bool {
        restores(
            self.rows[m].recipient == self.rows[i].recipient,
            self.out[m],
            self.takers_before(i, self.rows[m].entry),
        )
    }

    /// Whether some row restores row `i`'s recipient.
    pub fn restored(&self, i: usize) -> bool {
        any_n(self.rows.len(), |m| self.restores(i, m))
    }

    /// Whether row `i` revokes its recipient.
    pub fn revokes(&self, i: usize) -> bool {
        self.revoking[i]
    }

    /// Whether recipient `r` is revoked (`Membership::removed_recipients`).
    pub fn revoked(&self, r: usize) -> bool {
        any_n(self.rows.len(), |i| {
            self.rows[i].recipient == r && self.revoking[i]
        })
    }

    /// The rows that revoke their recipient, ascending.
    pub fn revoking_rows(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.rows.len()).filter(|&i| self.revoking[i])
    }

    /// Whether a row that is not out binds recipient `r`.
    pub fn bound_live(&self, r: usize) -> bool {
        any_n(self.rows.len(), |m| {
            self.rows[m].recipient == r && !self.out[m]
        })
    }

    /// Whether recipient `r` is current (`Log::current_recipients`).
    pub fn current(&self, r: usize) -> bool {
        current(self.bound_live(r), self.revoked(r))
    }

    /// Whether epoch record `e` keeps every revoked recipient out: for each row whose recipient
    /// stays revoked, every `Remove` that took it lies in `e`'s causal past, or one does and no
    /// row that is not out binds the recipient. A writer seals only under an epoch whose gate is
    /// open, and an epoch whose gate is shut is due a rotation.
    pub fn gate(&self, e: usize) -> bool {
        all_n(self.rows.len(), |i| {
            gate_row(
                self.revoking[i],
                self.takers_before(i, e),
                self.bound_live(self.rows[i].recipient),
                self.some_taker_before(i, e),
            )
        })
    }
}

#[cfg(test)]
mod tests;
