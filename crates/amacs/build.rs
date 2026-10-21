// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(clean_islands)`, asked of the compiler: the rule every island-carrying
//! crate mounts (crates/aterm-log/clean_islands.rs). amacs's islands cite the
//! kernel imports Trust mints only while it verifies, so, like aterm-log's,
//! they key on `trust_verify`; a standalone island (one that cites no import)
//! keys on `clean_islands` and is kernel-checked at every Trust build.

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    clean_islands::emit();
}
