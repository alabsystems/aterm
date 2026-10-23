// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AI CLI connections, login state, and cross-AI reviews.

pub mod change;
pub mod cli;
pub mod codex_limits;
pub mod config;
pub mod connector;
pub mod facts;
pub mod fleet;
pub mod login;
pub mod permission;
pub mod review;
pub mod run;
pub mod state;
pub mod store;

#[cfg(test)]
pub(crate) mod testing;
