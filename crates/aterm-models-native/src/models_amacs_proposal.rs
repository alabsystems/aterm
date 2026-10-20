// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AmacsProposal`: an agent's proposed edit, reviewed hunk by hunk before it
//! touches the buffer (docs/DESIGN-amacs-2026-10-19.md §4.9; slice A5).
//!
//! Invariants: AcceptOnce; RejectLeavesText; StaleHunkRefused. Buggy
//! mutants: AcceptTwice, ApplyStale. Tier-1: aterm-gui's
//! `proposal_conformance.rs`.
