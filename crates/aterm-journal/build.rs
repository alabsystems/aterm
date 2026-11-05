// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;
fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    anchor_rules::emit();
}
