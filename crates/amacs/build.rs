// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build script emitting `cfg(clean_islands)` for `amacs`.

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    clean_islands::emit();
}
