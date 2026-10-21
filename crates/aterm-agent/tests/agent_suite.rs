// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-agent integration files as modules of ONE test binary: each file
//! keeps its path and its own `#[test]`s. `conformance_upgrade_models` keeps its
//! `#[path]`-mounted submodules (resolved from `tests/`, as before).
//!
//! Sharing a process was checked, not assumed: no file below sets or removes an
//! environment variable, installs a hook or an allocator, or re-executes
//! itself; each file's scratch directories carry a prefix of its own
//! (`aterm-footer-model-`, `aterm-conformance-disk-`), so no two files share
//! one; and the checker tier's `ty` spawns (`validate_transition_tiered`,
//! built with aterm-spec's `checkers` feature only) take a scratch directory
//! per call and share nothing else, so they run concurrently with every other
//! `ty`.
//! The two files that count process-wide allocations
//! (`footer_window_allocations`, `transcript_allocations`) are their own
//! `[[test]]`s.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-agent --test agent_suite -- <file>::`.

mod common;
mod conformance_disk_pass;
mod conformance_footer_model;
mod conformance_login_wall;
mod conformance_upgrade_models;
mod conformance_usage_tap;
mod supervise_conformance_account_rotation;
mod supervise_conformance_credits_hold;
mod supervise_conformance_cruise;
mod supervise_conformance_land;
mod supervise_conformance_model_switch;
mod supervise_conformance_network_wall;
mod supervise_conformance_turn_end;
mod supervise_conformance_usage_wall;
mod vendor_corpus;
