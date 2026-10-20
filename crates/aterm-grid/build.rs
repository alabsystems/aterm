// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(clean_islands)` for this crate's proof islands, asked of the compiler, and
//! `cfg(anchor_rules)` for its anchor attributes (crates/aterm-spec-anchor/anchor_rules.rs).

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

// `cfg(anchor_rules)`: which anchor attributes this crate writes.
#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    clean_islands::emit();
    anchor_rules::emit();
}
