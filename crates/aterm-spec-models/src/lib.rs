// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TLA+ models of aterm's load-bearing invariants (ROADMAP WS-I).
//!
//! The specs live in `specs/*.tla` (+ `.cfg`). They are model-checked by the
//! `ty` explicit-state checker from `tests/model_check.rs`, so model-checking
//! runs under `cargo test` and the CI gate — not as a separate manual step.
//!
//! Families (ATERM_DESIGN §6.4):
//! - CONSISTENCY — the buffer kernel (event-log monotonicity, snapshot/branch).
//! - ISOLATION — the capability/sandbox boundary.
//! - META — refinement / coverage.
//!
//! After TRUST_NATIVE_TLA Phase 1, the kernel-family CONSISTENCY specs
//! (Kernel/Subscribe/Snapshot/Transact/Evict) — which are SUPERSEDED by the
//! drift-free derived twins in `aterm-spec` — live under `specs/legacy/` and are
//! NOT in the checked set; the active `specs/` dir holds the ISOLATION family
//! (the legitimate home of full-TLA+ design specs, bound to source in Phase 2).

use std::path::{Path, PathBuf};

/// The directory holding the ACTIVE external `.tla`/`.cfg` specs (the ISOLATION
/// family after Phase 1). Resolved from this crate's manifest dir, so a consumer
/// (e.g. the `spec_xref_closure` gate in aterm-core) can locate and parse them as
/// `SpecModule::External` anchor targets without hard-coding a path.
pub fn specs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("specs")
}

/// The quarantined kernel-family specs, superseded by derived twins (Phase 1).
/// Kept on disk under `specs/legacy/` for provenance; excluded from the checked
/// set (`tests/model_check.rs`) and from the xref gate's external resolution.
pub fn legacy_specs_dir() -> PathBuf {
    specs_dir().join("legacy")
}
