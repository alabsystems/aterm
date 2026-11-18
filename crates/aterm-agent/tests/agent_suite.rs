// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-agent integration files as modules of one test binary.
//!
//! Sharing a process was checked, not assumed: no file below sets or removes an
//! environment variable, installs a hook or an allocator, or re-executes
//! itself, apart from `live_window_ends`, which re-runs this binary as a child
//! that runs its one test alone, so the signal handlers that test reads are
//! installed in the child only; each file's scratch directories carry a prefix
//! of its own (`aterm-footer-model-`, `aterm-conformance-disk-`), so no two
//! files share one; and the checker tier's `ty` spawns
//! (`validate_transition_tiered`, built with aterm-spec's `checkers` feature
//! only) take a scratch directory per call and share nothing else, so they run
//! concurrently with every other `ty`.
//! The two files that count process-wide allocations
//! (`footer_window_allocations`, `transcript_allocations`) are their own
//! `[[test]]`s.
//!
//! One file: `targo --unverified test -p aterm-agent --test agent_suite -- <file>::`.

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

mod common;
mod conformance_account_key;
mod conformance_custody;
mod conformance_disk_pass;
mod conformance_footer_model;
mod conformance_left;
mod conformance_login_wall;
mod conformance_occupancy;
mod conformance_self_claim_facts;
mod conformance_sources;
mod conformance_usage_tap;
mod conformance_wakes;
#[cfg(unix)]
mod live_window_ends;
mod supervise_conformance_account_rotation;
mod supervise_conformance_condition_resume;
mod supervise_conformance_credits_hold;
mod supervise_conformance_cruise;
mod supervise_conformance_land;
mod supervise_conformance_model_switch;
mod supervise_conformance_outage_wait;
mod supervise_conformance_turn_end;
mod supervise_conformance_usage_wall;
mod vendor_corpus;
