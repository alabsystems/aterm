// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Proof anchors for the grid ring-buffer Kani harnesses.
//!
//! Each `proof_anchor!` binds a `#[kani::proof]` harness in
//! `grid/proofs_kani_ring.rs` to the [`Ring`](aterm_spec::derive::ring_model)
//! model's `Push` action, the same anchor namespace the `ty` refinements use.
//!
//! The harnesses are `#[cfg(kani)]`, so an attribute on them would never
//! register in normal builds; the registrations live here instead, gated only
//! by `cfg(any(test, feature = "spec-anchors"))`, naming each harness by string.
//!
//! Both harnesses check `ring_hyperlinks.len() == ring_buffer_scrollback()`
//! across scroll/erase sequences, the bounded-local twin of [`Ring`]'s
//! `LenBounded` invariant (`seq - lo + 1 <= Cap`) under `Push`.

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
