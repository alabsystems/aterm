// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The test binary (`aterm_update_core-<hash>.exe`) runs as its invoker on Windows
//! (crates/aterm-winres/as_invoker.rs).

#[path = "../aterm-winres/as_invoker.rs"]
mod as_invoker;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=../aterm-winres/as_invoker.rs");
    as_invoker::emit();
}
