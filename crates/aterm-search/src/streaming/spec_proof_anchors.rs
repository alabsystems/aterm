// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Proof anchors for the streaming-search kani harnesses (TRUST_NATIVE_TLA §4).
//!
//! The **kani half of the unified verifier ledger** for the derived
//! `StreamingSearch` machine (`aterm_spec::derive::streaming_search_model`).
//! Each `proof_anchor!` binds one `#[kani::proof]` harness in `proofs.rs` /
//! `proofs_gaps.rs` to a `(machine, action)` — the SAME anchor namespace the
//! temporal `#[refines]` bindings in `engine/operations.rs` use — so the
//! `spec_xref_closure` gate prints ONE per-action ledger line spanning `ty`
//! (temporal) and `kani` (bounded-local).
//!
//! ## Why this module is decoupled from the harnesses (the §4 subtlety)
//!
//! The harnesses are `#[cfg(kani)]`-gated — DORMANT under stock `cargo`. An
//! attribute on a harness fn would be stripped with it and never register. So
//! the registrations live HERE, gated ONLY by `cfg(any(test, feature =
//! "spec-anchors"))`, naming each harness by string (the aterm-scrollback
//! precedent).
//!
//! ## Mapping rationale
//!
//! The scan-pipeline harnesses (`memory_always_bounded`, dedup, positions,
//! totals) are the bounded-local form of `ScanHit` — INV-SEARCH-2/-4 among them
//! are NOT scalar-expressible, so kani is their only verifier and this join is
//! what puts them on the ledger. Harnesses with no clean model action —
//! `update_pattern_*` (pattern content is unbounded; no model action) and the
//! matching-layer proofs — are intentionally NOT anchored: they stay local-only,
//! the documented convention.

// ScanHit — the scan pipeline's store-or-count discipline and its per-result
// data invariants (INV-SEARCH-2/-3/-4/-6; -2 and -4 are kani-only).
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "ScanHit",
    proof = "memory_always_bounded"
);
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "ScanHit",
    proof = "no_duplicate_results"
);
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "ScanHit",
    proof = "result_positions_always_valid"
);
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "ScanHit",
    proof = "result_positions_valid_symbolic_scan"
);
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "ScanHit",
    proof = "total_matches_consistent"
);

// ScanMiss — scan-progress consistency through no-match rows and completion
// (INV-SEARCH-5).
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "ScanMiss",
    proof = "scan_progress_consistent"
);
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "ScanMiss",
    proof = "scan_progress_consistent_symbolic"
);

// NextMatch — navigation keeps the 1-based index valid (INV-SEARCH-1).
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "NextMatch",
    proof = "current_index_always_valid"
);
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "NextMatch",
    proof = "navigation_index_valid"
);

// Add — content_added preserves dedup (INV-SEARCH-4's kani-only half).
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "Add",
    proof = "no_duplicate_results_content_added"
);

// Invalidate — the #7472/#7244 clamp class (the model's Buggy dial), proven
// bounded-locally over seeded terminal states.
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "Invalidate",
    proof = "content_invalidated_preserves_invariants"
);
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "Invalidate",
    proof = "content_invalidated_prefix_preserves_invariants"
);
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "Invalidate",
    proof = "content_invalidated_state_transitions"
);

// Cancel — everything cleared, back to Idle.
aterm_spec::proof_anchor!(
    machine = "streaming_search",
    action = "Cancel",
    proof = "cancel_clears_state"
);
