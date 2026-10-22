// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARNESS OCCUPANCY (`docs/DESIGN-harness-occupancy-2026-10-20.md`): what the harness measures
//! of a session's wait on its own background work, under `[harness] occupancy`
//! ([`super::config::Occupancy`]). Measures, judges and books, and types nothing:
//!
//! * [`hold`]: the hold's clock — when the wait began, whether the agent declared it
//!   (`WAITING:`), when it ended — and its `fact-hold@v1` ledger rows, which a successor loop
//!   carries the hold by.
//! * [`reading`]: the machine as the window's facts board reads it for one session
//!   (`IdleHost::occupancy`, taped), the empty reading where none is.
//! * [`rules`]: the built-in rules, `wait-nudge@v1` and `hold-room@v1`, and [`decide`], the pure
//!   decider of whether one is due at a break and what holds it.
//! * [`templates`]: the closed lines they would type.
//!
//! The loop feeds the clock and asks the decider from `run::occupancy_loop` on hosted,
//! unattended loops only, and only while the key is not `off`; there it journals `OCCUPANCY`
//! lines and books the rows. What `prompt` would type is booked as `fact-would@v1`: under
//! `observe` that is all, and there is no write path under `prompt` either.

pub mod hold;
pub mod reading;
pub mod rules;
pub mod templates;

pub use hold::{Close, HoldChange, HoldClock, HoldEvent, waiting_declared};
pub use reading::{OccupancyReading, SlotsSeen};
pub use rules::{Books, Guards, HoldSeen, RuleId, Verdict, decide};
pub use templates::FactLine;
