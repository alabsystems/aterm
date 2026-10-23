// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Harness supervisor tapes for deterministic offline replay and decision inspection.
//!
//! Records session setup ([`header`], [`super::run::SessionSetup`], [`super::run::SuperviseOpts`],
//! [`super::run::ApprovalEnv`], [`super::SupervisorConfig::set`]) and door/host effects
//! ([`doors`], [`host`], [`core`], [`ctl`], [`codec`], [`delta`], [`frame`], [`recover`],
//! [`store`]) for strict replay ([`replay`], [`report`]), counterfactuals ([`acts`], [`shadow`]),
//! `--why` traces ([`explain`]), text inspection ([`text`], [`show`]), and the regression
//! [`corpus`].

pub mod acts;
pub mod codec;
pub(crate) mod core;
pub mod corpus;
pub(crate) mod ctl;
pub(crate) mod delta;
pub mod doors;
pub mod explain;
pub mod frame;
pub mod header;
pub(crate) mod host;
pub mod recover;
pub mod replay;
pub mod report;
pub mod shadow;
pub mod show;
pub mod store;
pub mod text;

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
#[path = "frame_tests.rs"]
pub(crate) mod frame_tests;
#[cfg(test)]
pub(crate) mod testing;
