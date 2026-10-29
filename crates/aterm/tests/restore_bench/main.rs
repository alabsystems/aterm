// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure checks for the restore measurement's grammar, readings and tables. The explicit
//! `restore_bench` bench shares these modules and crashes and recovers the live instances;
//! ordinary tests do not measure.
#![cfg(target_os = "macos")]

#[path = "../support/child_output.rs"]
mod child_output;
#[path = "../support/ctl_accept.rs"]
mod ctl_accept;
#[path = "../support/headless_boot.rs"]
mod headless_boot;
#[path = "../support/keeper_world.rs"]
mod keeper_world;
mod keeper_world_tests;
#[path = "../support/launch_isolation.rs"]
mod launch_isolation;
#[path = "../support/live_ctl.rs"]
mod live_ctl;
#[path = "../fabric_bench/profile.rs"]
#[expect(
    dead_code,
    reason = "the verify perf driver also uses this shared profile table's build and CLI methods"
)]
mod profile;
#[expect(
    dead_code,
    reason = "the live measurement entry runs only in the explicit bench"
)]
mod run;
mod run_tests;
#[expect(
    dead_code,
    reason = "the explicit bench reads its required run from the environment"
)]
mod spec;
mod spec_tests;
#[path = "../fabric_bench/stats.rs"]
#[expect(
    dead_code,
    reason = "the fabric bench's table row; this bench writes its own tables"
)]
mod stats;
