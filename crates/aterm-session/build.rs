// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Emits `cfg(clean_islands)` and `cfg(anchor_rules)` build configuration for `aterm-session`.

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    clean_islands::emit();
    anchor_rules::emit();
}
