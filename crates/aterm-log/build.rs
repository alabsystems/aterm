// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(clean_islands)`, asked of the compiler: the `allow` of Trust's `env_mutation` lint
//! keys on it. `clean_islands.rs` is the one rule every island-carrying crate mounts.

mod clean_islands;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    clean_islands::emit();
}
