// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;
fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    clean_islands::emit();
}
