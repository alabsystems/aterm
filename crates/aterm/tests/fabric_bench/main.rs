// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure checks for the fabric measurement's parsing, clocks, record census,
//! build identity and statistics. The explicit `fabric_bench` bench shares
//! these modules and boots the live stacks; ordinary tests do not measure.
#![cfg(unix)]

#[path = "../fabric_world/harness.rs"]
mod harness;
#[path = "../support/patience.rs"]
mod patience;

mod census;
mod census_tests;
use aterm_bench::head_sampling as clock;
mod profile;
mod profile_tests;
#[expect(
    dead_code,
    reason = "the live measurement entry runs only in the explicit bench"
)]
mod run;
#[expect(
    dead_code,
    reason = "the explicit bench reads its required run from the environment"
)]
mod spec;
mod spec_tests;
mod stats;
mod stats_tests;
