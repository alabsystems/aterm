// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Proof anchors for the scrollback kani harnesses (TRUST_NATIVE_TLA §4, Phase 4).
//!
//! This module is the **kani half of the unified verifier ledger**. Each
//! `proof_anchor!` below binds one `#[kani::proof]` harness in `kani_proofs.rs` to a
//! `(machine, action)` of a derived `aterm-spec` model — the SAME anchor namespace the
//! temporal (`ty`) refinements use. The `spec_xref_closure` gate then prints ONE
//! per-`(machine, action)` ledger line showing whether `ty`, `kani`, or both discharge it.
//!
//! ## Why this module is decoupled from the harnesses (the §4 subtlety)
//!
//! The harnesses in `kani_proofs.rs` are `#[cfg(kani)]`-gated — DORMANT under stock
//! `cargo`. If a `#[proof_anchors]` ATTRIBUTE were placed on a harness fn, it would be
//! stripped along with the fn in normal/test builds and NEVER register in the inventory
//! slice. So the registrations live HERE, in a module gated ONLY by
//! `cfg(any(test, feature = "spec-anchors"))` (NOT `cfg(kani)`), naming each harness by
//! string. They register under stock cargo whenever the feature (or `test`) is on, while
//! the harness fns themselves keep their `#[cfg(kani)]` gate untouched.
//!
//! ## Mapping rationale
//!
//! These harnesses are bounded-local BMC of the tiered scrollback's COUNTING and
//! EVICTION discipline — exactly the bounded data-structure twin of the derived eviction
//! models:
//!   * [`EvictFull`](aterm_spec::derive::evict_full_model) — bounded live-window under
//!     `Push` (counting / hot-bound / line-limit are the local form of its
//!     `EvictOldestContiguous`).
//!   * [`TierResidency`](aterm_spec::derive::tier_residency_model) — spill-not-forget:
//!     `Push` (cold push + drop preserves the tier-sum) and `Demote` (hot→warm→cold
//!     transitions preserve the count / recoverability).
//!
//! Harnesses with no clean model action (the de/serialize round-trips, pressure
//! thresholds, `get_line` bounds) are intentionally NOT anchored — they stay local-only.

// EvictFull::Push — the bounded live-window discipline (counting accuracy / hot bound /
// line-limit enforcement: the local BMC form of EvictOldestContiguous over Push).
aterm_spec::proof_anchor!(
    machine = "EvictFull",
    action = "Push",
    proof = "line_count_accurate"
);
aterm_spec::proof_anchor!(
    machine = "EvictFull",
    action = "Push",
    proof = "hot_bounded"
);
aterm_spec::proof_anchor!(
    machine = "EvictFull",
    action = "Push",
    proof = "line_limit_enforced"
);

// TierResidency::Push — cold push + drop preserves the tier-sum (the spill-on-evict
// path's count consistency).
aterm_spec::proof_anchor!(
    machine = "TierResidency",
    action = "Push",
    proof = "cold_push_drop_preserves_line_count_invariant"
);

// TierResidency::Demote — hot→warm→cold tier transitions preserve the line count
// (recoverability across tiers), incl. the memory-pressure cold-eviction loop.
aterm_spec::proof_anchor!(
    machine = "TierResidency",
    action = "Demote",
    proof = "tier_transition_preserves_count"
);
aterm_spec::proof_anchor!(
    machine = "TierResidency",
    action = "Demote",
    proof = "cold_eviction_loop_preserves_scrollback_total"
);
