// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release cutter's integration tests, as ONE test binary.
//!
//! aterm-release is binary-only on purpose (src/main.rs: the spec's §9 file
//! plan has no `lib.rs`, and nothing outside the cut tool may link this code),
//! so the tests compile the pipeline's modules directly, by `#[path]`. Each
//! such mount compiles the whole cutter and runs every inline `#[cfg(test)]`
//! test in it, so the mounts live here, once, and every test file below is a
//! module of this crate that reaches the pipeline through `crate::`. The
//! binary target has `test = false` (Cargo.toml), so this is where the inline
//! unit tests run, once each.
//!
//! The modules are mounted at the crate root because they cross-reference
//! each other through `crate::` (publish.rs reaches every pipeline stage).
//! Every mount is `allow(dead_code)`: a test exercises part of a module, and
//! the real crate's own build — `mod` declarations private to the binary — is
//! what holds the dead-code line for src/, so an unexercised item here is not
//! evidence of anything.

#[path = "../../src/apple.rs"]
#[allow(dead_code)]
mod apple;
#[path = "../../src/artifact.rs"]
#[allow(dead_code)]
mod artifact;
#[path = "../../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../../src/channel.rs"]
#[allow(dead_code)]
mod channel;
#[path = "../../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../../src/durable.rs"]
#[allow(dead_code)]
mod durable;
#[path = "../../src/fixtures.rs"]
#[allow(dead_code)]
mod fixtures;
#[path = "../../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../../src/release.rs"]
#[allow(dead_code)]
mod release;
#[path = "../../src/release_steps.rs"]
#[allow(dead_code)]
mod release_steps;
#[path = "../../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../../src/snapshot.rs"]
#[allow(dead_code)]
mod snapshot;
#[path = "../../src/trust_closure.rs"]
#[allow(dead_code)]
mod trust_closure;
#[path = "../../src/trust_compare.rs"]
#[allow(dead_code)]
mod trust_compare;
#[path = "../../src/trust_facts.rs"]
#[allow(dead_code)]
mod trust_facts;
#[path = "../../src/trust_oblig.rs"]
#[allow(dead_code)]
mod trust_oblig;
#[path = "../../src/verify.rs"]
#[allow(dead_code)]
mod verify;

mod apple_tier;
mod build_once;
mod channel_floor_model;
mod channel_latest;
mod claim_landing_model;
mod d27_refute;
mod durable_post_intent_model;
mod handoff_policy_bundle;
mod journal_prefix_model;
mod ledger_race;
mod linux_release_arch;
mod machine_roster;
mod pack_arch_gate;
mod packlane_order;
mod plist_stamp;
mod publisher_fence_liveness;
mod publisher_fence_model;
mod release_fixtures;
mod release_flow;
mod release_lease;
mod resume;
mod signconf;
mod snapshot_take;
mod transcript_grid;
mod trust_facts_model;
