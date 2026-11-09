// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The digest trigger (PF5b's finding H1; one-mesh §4.6, "No anti-entropy timer"): the gap notice
//! that asks an `Inventory` exchange when a neighbour's presence rows show it holding other than
//! this Mac does of a store an exchange moves. A record that reaches a Mac by `Inventory` never
//! enters its log, so its relays owe their neighbours nothing of it; this trigger is what carries
//! it on, hop by hop (`relay_live_island.rs`: `h1_hole`, `h1_digest_closes_it`, and the premise
//! `FairDigest` of `every_seat_gets_the_record`).
//!
//! A row names one digest of each such store: the membership log's records, and the lab tables'
//! operations and conflict records (aterm-mesh `presence::Held`). `FairDigest` holds where those
//! digests are faithful, a record one Mac holds and its neighbour lacks making them differ, and it
//! covers what the row's digests name: aterm-mesh's tests hold each unit of an exchange to it (the
//! membership log, a table's conflict proofs, its operations).
//!
//! A neighbour's row asks an exchange ([`digest_asks`]) when a store's digest differs from this
//! Mac's own latest row's, both known; the two digests are the ones the neighbour's last row was
//! compared under (two rows in a row, and neither Mac's store moved in between, so neither relay
//! carried anything either Mac's store took: both relays are caught up as far as either Mac's
//! store shows); this Mac's tables relay to it follows; the trigger has not asked for this pair of
//! digests since the last exchange with the neighbour that failed; and `NO_DIGEST_TRIGGER` is off.
//! So it never asks between Macs in step (no timer), asks once for each pair of digests (the quiet
//! rule's "until a digest changes"), and always asks on a difference that lasts two rows.
//! [`Trigger`] is the fold over one neighbour's rows of one store's digest that reads those
//! inputs; aterm-mesh keeps one for each store a row names, and a row asks where either asks.
//!
//! `relay_digest_island.rs` (every Trust build) defines the kernel's spec, models the fold, and
//! proves that it asks only on a lasting difference with the relay following
//! (`asks_only_on_a_lasting_difference`, so `never_in_step`), never twice in a row for one pair
//! (`once_per_pair`), and on every difference that lasts two rows (`a_lasting_difference_is_asked`);
//! runs by `rfl` show each input of the kernel is needed. `relay_digest_binding_island.rs` proves
//! the compiled kernel equal to its spec; the answer sheet `relay_digest::tests` recomputes holds
//! the kernel and the fold to the island until TG12.

contract! {
    /// A neighbour's presence row asks an `Inventory` exchange: its digest of a store `differs`
    /// from this Mac's own latest row's (both known), the two are `steady` (the ones its last row
    /// was compared under), this Mac's tables relay to it `follows`, the trigger has not `asked`
    /// for this pair since the last exchange that failed, and the trigger is not `off`.
    pub fn digest_asks(differs: bool, steady: bool, follows: bool, asked: bool, off: bool) -> bool
    clauses [ensures result == spec_digest_asks(differs, steady, follows, asked, off) by digest_asks_is_exact]
    { differs && steady && follows && !asked && !off }
}

/// The kernel a [`Trigger`] asks: [`digest_asks`], or a test's mutant of it.
type Kernel = fn(bool, bool, bool, bool, bool) -> bool;

/// The digest trigger's memory of one neighbour and one store: the store's two digests (the
/// neighbour's, then this Mac's own latest row's, `None` where unknown) its last row was compared
/// under, and the pair it last asked an exchange for. The island's `Mem`, step for step (`stepM`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trigger<D> {
    last: Option<(Option<D>, Option<D>)>,
    asked: Option<(D, D)>,
}

impl<D> Default for Trigger<D> {
    fn default() -> Self {
        Self {
            last: None,
            asked: None,
        }
    }
}

impl<D: Copy + Eq> Trigger<D> {
    /// Read the neighbour's next row: its digest of the store `theirs`, this Mac's own latest
    /// row's `mine`, whether this Mac's tables relay to it `follows`, and whether the trigger is
    /// `off`. Answers whether the row asks an exchange.
    pub fn row(&mut self, theirs: Option<D>, mine: Option<D>, follows: bool, off: bool) -> bool {
        self.step(digest_asks, theirs, mine, follows, off)
    }

    /// An exchange with the neighbour failed: the pair it last asked for may be asked again.
    pub fn failed(&mut self) {
        self.asked = None;
    }

    /// [`Self::row`] under the kernel `asks`.
    fn step(
        &mut self,
        asks: Kernel,
        theirs: Option<D>,
        mine: Option<D>,
        follows: bool,
        off: bool,
    ) -> bool {
        let pair = theirs.zip(mine);
        let differs = pair.is_some_and(|(t, m)| t != m);
        let steady = self.last == Some((theirs, mine));
        let asked = pair.is_some() && self.asked == pair;
        self.last = Some((theirs, mine));
        let ask = asks(differs, steady, follows, asked, off);
        if ask {
            self.asked = pair;
        }
        ask
    }
}

#[cfg(test)]
mod tests;
