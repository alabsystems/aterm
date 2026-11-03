// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Select the first-party anchor representation for the configured compiler.

#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    anchor_rules::emit();
}
