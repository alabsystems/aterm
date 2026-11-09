// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The own-inbox move across this Mac's key rotation (one-mesh §11 C3b): when the node service
//! may start moving the unread mail its `bus.log` holds for a node this Mac held before to the
//! node it holds now. The move follows that node's inbox group from where its commit stands, so a
//! record the node's last bridge delivered and had not committed would be moved, and delivered a
//! second time.
//!
//! [`move_opens`] is the decision, a scalar `contract!` kernel stating `ensures result ==
//! spec_move_opens(..) by move_opens_is_exact` for a definition of `inbox_move_island.rs`. That
//! island (every Trust build) proves that a move the kernel opens resumes past every record the
//! old node's bridge delivered, wherever that bridge, once it can deliver no more, committed or
//! handed over what it delivered; and, as kernel-evaluated counterexamples, that a kernel dropping
//! either input opens a move that resumes below a delivered record. `inbox_move_binding_island.rs`
//! proves the compiled kernel equal to its spec and closes the law for it (the Trust lane until
//! TG12); the answer sheet `inbox_move::tests` recomputes holds the Rust to the island on every
//! build.
//!
//! [`shows_anew`] is the endpoint's half: a record the move put on this node's inbox reaches its
//! session as a copy at a new offset, which names where it was first (`was=`: the offset the first
//! move took, or the earliest one its bridge could read it back to), so the endpoint answers it
//! from its memory of either offset as it answers a repeat of one. The island proves that, where
//! the endpoint remembers each piece of mail it showed by every offset a delivery of it named and
//! remembers no offset of mail it did not show, each piece of mail is shown once across the move,
//! and refutes the two kernels that drop an input: the offset-only one shows a copy of a record
//! shown before the move again, and the origin-only one a plain repeat. The one memory outside
//! that premise, a broadcast the endpoint refused over quota, is answered without being shown, so
//! that a refusal counts once.

contract! {
    /// The move of a retired node's unread mail opens only where no bridge of that node may still
    /// deliver it (`may_deliver`: the bridge state dir names that node, so a bridge of it may hold
    /// the state lock) and the inbox commit that node's last bridge handed over is made on its
    /// group (`owed_unmade` while it is not).
    pub fn move_opens(may_deliver: bool, owed_unmade: bool) -> bool
    clauses [ensures result == spec_move_opens(may_deliver, owed_unmade) by move_opens_is_exact]
    { !may_deliver && !owed_unmade }
}

contract! {
    /// The endpoint shows a delivery to its session only where it remembers neither its offset
    /// (`off_known`: it showed mail there or answered a copy of shown mail there, or refused a
    /// broadcast there over quota, which counts once) nor, for a record moved here across this
    /// Mac's key rotation, the offset its `was=` names (`was_known`); else it answers it from that
    /// memory, showing nothing.
    pub fn shows_anew(off_known: bool, was_known: bool) -> bool
    clauses [ensures result == spec_shows_anew(off_known, was_known) by shows_anew_is_exact]
    { !off_known && !was_known }
}

#[cfg(test)]
mod tests;
