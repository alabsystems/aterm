// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Models of capacity retained for an admitted process group independently of guard locks, of
//! the CPU share and memory claim a test run's binaries are dispatched within, and of which
//! claims go first past a waiter of memory that cannot start yet.

mod models_capacity_cohort;
mod models_capacity_history;
mod models_memory_passing;
mod models_qualify_fallback;
mod models_qualify_memory;
mod models_test_dispatch;

pub use models_capacity_cohort::capacity_cohort_model;
pub use models_capacity_history::{capacity_history_model, capacity_terminal_model};
pub use models_memory_passing::memory_passing_model;
pub use models_qualify_fallback::{
    qualify_fallback_admission_liveness, qualify_fallback_admission_model,
};
pub use models_qualify_memory::qualify_memory_model;
pub use models_test_dispatch::{test_dispatch_liveness, test_dispatch_model};
