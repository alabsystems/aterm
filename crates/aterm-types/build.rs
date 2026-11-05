// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(anchor_rules)` for this crate's anchor attributes (crates/aterm-spec-anchor/anchor_rules.rs).

// `cfg(anchor_rules)`: which anchor attributes this crate writes.
#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    anchor_rules::emit();
}
