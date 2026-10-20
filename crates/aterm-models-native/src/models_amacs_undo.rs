// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AmacsUndo`: labelled linear undo under concurrent and agent edits
//! (docs/DESIGN-amacs-2026-10-19.md §4.5, §4.9; slice S7).
//!
//! Invariants: UndoInvertsLatestUnblocked; NoSilentDrop;
//! BlockedFrameNeverApplied; RebaseKeepsDisjointFrames. Buggy mutants:
//! SkipRebase, ApplyBlocked, SnapshotRestore. Tier-1: the `#[cfg(test)]`
//! conformance module in `crates/amacs/src/editor/history.rs`.
