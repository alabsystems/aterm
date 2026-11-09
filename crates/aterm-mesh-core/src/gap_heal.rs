// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P2, gap heal: which `Wrap`s a holder of an epoch signs to close a gap, whom a new epoch
//! carries, and which removal would strand an epoch (one-mesh §4.2 M1, M4 and M5,
//! `docs/design/one-mesh-proofs/P2-gap-heal.md`).
//!
//! Only an explicit `Remove` revokes, so an omission is a gap: a current seat that a newest epoch
//! of a seat audience (`[everyone]` or Commons, never a host list, which names its Macs) does not
//! wrap is a gap of every Mac that opens the epoch, whatever its minter knew, unless the epoch
//! wraps a revoked recipient and a carrier (a current seat no roster denies that holds every
//! newest epoch of the audience) will re-key it. With no carrier the omission heals even where
//! the epoch is due, because that heal is how a carrier comes to be: the rule shipped before PF2
//! skipped it, and two concurrent heads that each wrapped a removed Mac, with no seat holding
//! both, locked the audience out. A seat the minter could not know (its entry is not in the
//! epoch's causal past) is a gap of any epoch it lacks. The heal, the carry and a removal's mint
//! read one deny, the union of this Mac's own roster and the opened one. A removal that would
//! leave a newest epoch of a seat audience with no current holder, where the remover's batch
//! does not re-key that audience, is refused ([`strands`]).
//!
//! The decisions are the scalar `contract!` kernels below; each states `ensures result ==
//! spec_<name>(..) by <name>_is_exact`, where `spec_<name>` is a definition of
//! `gap_heal_island.rs`. That island takes P1's decisions (`crate::revocation`) as parameters with
//! exactness premises, never redefining them, and proves for every size: a heal is exact (a
//! seat audience, a current seat, no deny, an unwrapped epoch the closer opens) and never targets
//! a host list; the carry is the current, undenied seats; an omission is a gap; one heal pass
//! closes every gap it finds; only a `Remove` revokes; the guard keeps a current holder of every
//! newest epoch in every run; and, under a stable membership with weakly fair heals and re-keys,
//! every seat audience converges (no re-key due, every newest epoch wrapping every current seat
//! no roster denies: law 4) and an admitted Mac comes to hold every newest epoch (law 3).
//! `gap_heal_binding_island.rs` proves each compiled kernel equal to its spec and closes the laws
//! for the World that asks the compiled kernels. The folds below are the island's folds, with no
//! early exit; until loops reach Trust's E6 import, the answer sheet the tests recompute holds
//! them to the island (TG12 removes it).

use crate::revocation::wraps_removed;

contract! {
    /// A current seat that the newest epoch omits is a gap of it, whatever its minter knew,
    /// unless the epoch wraps a revoked recipient and a carrier already holds every newest epoch
    /// of the audience (its re-key wraps every current seat). With no carrier the omission heals
    /// even where the epoch is due.
    pub fn heals_omission(newest: bool, wraps_revoked: bool, carried: bool) -> bool
    clauses [ensures result == spec_heals_omission(newest, wraps_revoked, carried) by heals_omission_is_exact]
    { newest && (!wraps_revoked || !carried) }
}

contract! {
    /// An epoch of a seat audience this Mac opens lacks a `Wrap` to a current seat, and the
    /// epoch heals omissions ([`heals_omission`]) or its minter could not know the seat (the
    /// seat's entry is not in the epoch's causal past).
    pub fn gap(opened: bool, current: bool, wrapped: bool, known: bool, heals: bool) -> bool
    clauses [ensures result == spec_gap(opened, current, wrapped, known, heals) by gap_is_exact]
    { opened && (current && (!wrapped && (heals || !known))) }
}

contract! {
    /// One deny: this Mac's own roster's or the opened roster's, read by the heal, the carry and
    /// a removal's mint alike.
    pub fn denied(local_roster: bool, opened_roster: bool) -> bool
    clauses [ensures result == spec_denied(local_roster, opened_roster) by denied_is_exact]
    { local_roster || opened_roster }
}

contract! {
    /// The closer signs a gap's `Wrap` unless the seat is [`denied`].
    pub fn heal_signs(gap: bool, denied: bool) -> bool
    clauses [ensures result == spec_heal_signs(gap, denied) by heal_signs_is_exact]
    { gap && !denied }
}

contract! {
    /// A new epoch of a seat audience wraps a recipient that is current, that its batch does not
    /// remove, and that is not [`denied`].
    pub fn carried(current: bool, excluded: bool, denied: bool) -> bool
    clauses [ensures result == spec_carried(current, excluded, denied) by carried_is_exact]
    { current && (!excluded && !denied) }
}

contract! {
    /// A removal strands a newest epoch of a seat audience when no current seat holds it after
    /// the removal and the remover's batch does not mint the audience's next epoch (the remover
    /// does not open the newest).
    pub fn strands(holder_left: bool, remover_mints: bool) -> bool
    clauses [ensures result == spec_strands(holder_left, remover_mints) by strands_is_exact]
    { !holder_left && !remover_mints }
}

/// The island's `wrapsRev`: whether an epoch wraps a revoked recipient, P1's `wraps_removed`
/// folded over `(revoked, wrapped)` per recipient, every one asked.
pub fn wraps_revoked(recipients: impl IntoIterator<Item = (bool, bool)>) -> bool {
    recipients
        .into_iter()
        .fold(false, |acc, (revoked, wrapped)| {
            acc | wraps_removed(revoked, wrapped)
        })
}

/// The island's `carriedA`: whether some seat is current, is not [`denied`] and holds every
/// newest epoch of the audience, folded over `(current, denied, holds_every_newest)` per seat.
/// Such a seat may mint the audience's next epoch, and a denied Mac never does.
pub fn carrier(seats: impl IntoIterator<Item = (bool, bool, bool)>) -> bool {
    seats
        .into_iter()
        .fold(false, |acc, (current, denied, holds)| {
            acc | (current && (!denied && holds))
        })
}

/// The island's `holderOf`: whether some current recipient holds an epoch, folded over
/// `(current, wrapped)` per recipient.
pub fn holder(recipients: impl IntoIterator<Item = (bool, bool)>) -> bool {
    recipients
        .into_iter()
        .fold(false, |acc, (current, wrapped)| acc | (current && wrapped))
}

/// The island's `strandsAny`: whether a removal strands some newest epoch of a seat audience,
/// [`strands`] folded over `(holder_left, remover_mints)` per newest epoch, every one asked.
/// `remove_member`'s guard refuses such a removal.
pub fn strands_any(heads: impl IntoIterator<Item = (bool, bool)>) -> bool {
    heads
        .into_iter()
        .fold(false, |acc, (holder_left, remover_mints)| {
            acc | strands(holder_left, remover_mints)
        })
}

#[cfg(test)]
mod tests;
