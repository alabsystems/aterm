// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P6, the membership log cannot wedge (step PF6, `docs/design/one-mesh-proofs/P6-log-capacity.md`
//! as the proof plan's review corrects it): which keyed writes the log takes while the re-keys a
//! held `Remove` made due keep their room.
//!
//! The decisions are the scalar kernels below, each computing a definition `spec_<name>` of
//! `capacity_island.rs`; [`stop_judges`] and [`writer_waits`] are `contract!`s stating `ensures
//! result == spec_<name>(..) by <name>_is_exact`. [`keeps_room`] compares a computed integer,
//! which E6 does not import yet (Trust's I2′), so the answer sheet alone binds it to its spec
//! until then, when it becomes a `contract!` with `keeps_room_is_exact` and no rewrite.
//! `aterm-secrets` asks them: [`keeps_room`] is the room check of a removal's fill, a heal and the
//! tender's re-keys no removal made due, in records and in bytes; [`stop_judges`] is which re-keys
//! the 8,500 stop holds; [`writer_waits`] is a writer's wait on a due rotation. The island proves,
//! for every size and over kernels exact to their specs, that the rule as shipped never refuses a
//! re-key writers wait on, under the faithfulness premises the tests hold and the Calm premises
//! the rule leaves (a proof admission and a person's writes other than a removal, P6's F3 and F4),
//! in records and in the byte reading; that the rule P6 proposes needs no Calm; and, as
//! kernel-evaluated counterexamples, that each rule before PF6 wedges, and so does the shipped
//! rule without Calm. `capacity_binding_island.rs` proves the compiled `contract!` kernels equal
//! to their specs (the Trust lane until TG12), and the answer sheet the tests recompute holds
//! every kernel to the island on every build.

/// A log that holds `held` of `limit` (records or bytes) takes `cost` more and still keeps
/// `need` free: `held + cost + need <= limit`, without overflow at any size. Each subtraction
/// runs only past the comparison that keeps it from wrapping; it is written wrapping, as the
/// spec's `UInt64` one is, because that is the arithmetic E6 reads.
#[must_use]
pub fn keeps_room(held: u64, cost: u64, need: u64, limit: u64) -> bool {
    held <= limit
        && (cost <= limit.wrapping_sub(held) && need <= limit.wrapping_sub(held).wrapping_sub(cost))
}

contract! {
    /// The 8,500 stop judges a re-key only where no held `Remove` made it due (`rotation`):
    /// writers wait on that one, so it is signed at any fill.
    pub fn stop_judges(rotation: bool) -> bool
    clauses [ensures result == spec_stop_judges(rotation) by stop_judges_is_exact]
    { !rotation }
}
contract! {
    /// A writer that waits for keys (`waits_for_key`) and minted nothing itself (`minted_here`)
    /// waits, unsigned, while the audience's newest epoch still wraps a Mac the log removed
    /// (`rotation_due`): the rotation the stop never judges.
    pub fn writer_waits(waits_for_key: bool, minted_here: bool, rotation_due: bool) -> bool
    clauses [ensures result == spec_writer_waits(waits_for_key, minted_here, rotation_due) by writer_waits_is_exact]
    { waits_for_key && (!minted_here && rotation_due) }
}

#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "capacity_island.rs"]
mod island;

#[cfg(test)]
mod tests;
