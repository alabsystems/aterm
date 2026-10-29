// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-release`: the release cutter behind the `targo --unverified ship` alias.

#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the release cutter, run from source"
)]
mod apple;

mod artifact;
mod buildplan;
mod bundle;
mod changelog;

mod channel;

mod clean;
mod cli;

mod credential_wait;

mod cutter;

mod dev_app;

mod dev_identity;
mod dmg;

mod durable;

mod fixtures;
mod gates;

mod harness_versions;

mod github;

mod hook;

mod interrupt;
mod ledger;

mod machines;
mod manifest_out;

mod provision;
mod publish;

mod release;
mod release_keychain;
mod release_steps;
mod sign;

mod site;

mod tap;

mod snapshot;

mod transforms;

mod trust_credit;
mod trust_facts;

mod trust_lane;

mod trust_oblig;
mod verify;

fn main() {
    cutter::judge_at_start();
    std::process::exit(cli::run());
}
