// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(anchor_rules)`: which anchor attributes this crate writes
//! (crates/aterm-spec-anchor/anchor_rules.rs); and the test binaries
//! (`aterm_update-<hash>.exe`, `install_sh-<hash>.exe`) run as their invoker on Windows
//! (crates/aterm-winres/as_invoker.rs).

#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

#[path = "../aterm-winres/as_invoker.rs"]
mod as_invoker;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=../aterm-winres/as_invoker.rs");
    anchor_rules::emit();
    as_invoker::emit();
}
