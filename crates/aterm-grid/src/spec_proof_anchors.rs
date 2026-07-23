// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Proof anchors for the grid ring-buffer kani harnesses (TRUST_NATIVE_TLA §4, Phase 4).
//!
//! The kani half of the unified verifier ledger for `aterm-grid`. Each `proof_anchor!`
//! binds a `#[kani::proof]` harness in `grid/proofs_kani_ring.rs` to the
//! [`Ring`](aterm_spec::derive::ring_model) model's `Push` action — the SAME anchor
//! namespace the temporal (`ty`) refinements use.
//!
//! ## Why this module is decoupled from the harnesses (the §4 subtlety)
//!
//! `grid/proofs_kani_ring.rs` is `#[cfg(kani)]`-gated — DORMANT under stock `cargo`. A
//! `#[proof_anchors]` ATTRIBUTE on a harness fn would be stripped (never registering) in
//! normal/test builds. So these registrations live HERE, UN-GATED w.r.t. `cfg(kani)`,
//! gated only by `cfg(any(test, feature = "spec-anchors"))`, naming each harness by
//! string. The `#[cfg(kani)]` gate on the harness fns themselves is untouched.
//!
//! ## Mapping rationale
//!
//! Both harnesses verify the ring-buffer bounded-length invariant
//! `ring_hyperlinks.len() == ring_buffer_scrollback()` across scroll/erase sequences —
//! the bounded-local BMC twin of [`Ring`]'s `LenBounded` invariant (`seq - lo + 1 <=
//! Cap`) maintained by its single `Push` action.

// Ring::Push — the ring-buffer length invariant under push/scroll (the local BMC form
// of LenBounded), and the same invariant re-established after an erase_scrollback reset.
aterm_spec::proof_anchor!(
    machine = "Ring",
    action = "Push",
    proof = "ring_hyperlinks_len_matches_scrollback"
);
aterm_spec::proof_anchor!(
    machine = "Ring",
    action = "Push",
    proof = "ring_hyperlinks_invariant_across_erase"
);
