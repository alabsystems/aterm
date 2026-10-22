// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARNESS OCCUPANCY (`docs/DESIGN-harness-occupancy-2026-10-20.md`): what the harness measures
//! of a session's wait on its own background work, under `[harness] occupancy`
//! ([`super::config::Occupancy`]). Slice 1a measures and books, and types nothing:
//!
//! * [`hold`]: the hold's clock — when the wait began, whether the agent declared it
//!   (`WAITING:`), when it ended — and its `fact-hold@v1` ledger rows, which a successor loop
//!   carries the hold by.
//!
//! The loop feeds the clock from `run::occupancy_loop` on hosted, unattended loops only, and only
//! while the key is not `off`; there it journals `OCCUPANCY` lines and books the rows.

pub mod hold;

pub use hold::{Close, HoldChange, HoldClock, HoldEvent, waiting_declared};
