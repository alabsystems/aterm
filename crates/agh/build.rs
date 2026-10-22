// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `cfg(anchor_rules)` for agh's anchor attributes (crates/aterm-spec-anchor/anchor_rules.rs).

// Which anchor attributes this crate writes: aterm-spec-anchor's `macro_rules!` ones on a
// Trust compiler's build, test, check and doc lanes; its proc macros on a stock compiler and
// under the lint driver.
#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    anchor_rules::emit();
}
