// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Models of capacity retained for an admitted process group independently of guard locks, and
//! of the CPU share and memory claim a test run's binaries are dispatched within.

mod models_capacity_cohort;
mod models_capacity_history;
mod models_test_dispatch;

pub use models_capacity_cohort::capacity_cohort_model;
pub use models_capacity_history::{capacity_history_model, capacity_terminal_model};
pub use models_test_dispatch::{test_dispatch_liveness, test_dispatch_model};
