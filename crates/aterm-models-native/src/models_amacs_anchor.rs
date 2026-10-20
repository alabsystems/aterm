// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AmacsAnchor`: an edit script applied against `line:h6` anchors
//! (docs/DESIGN-amacs-2026-10-19.md §7c, §4.9; slice S6).
//!
//! Invariants: AllOrNothing; AnchorExact (an op lands only on a hash match,
//! unless `near=`); SubstituteUnique. Buggy mutants: OffByOne, PartialApply,
//! FirstMatchWins. Tier-1: the `#[cfg(test)]` conformance module in
//! `crates/amacs/src/protocol/script.rs`.
