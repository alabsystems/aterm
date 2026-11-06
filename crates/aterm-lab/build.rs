// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The build profile `ATERM_LAB_PROFILE` (cargo's `PROFILE` and `OPT_LEVEL`) the GPU and learn
// benches record. The lab's commit is read when it runs (`src/lab_build.rs`), so this script
// watches nothing but itself and no edit elsewhere rebuilds the lab.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
    let opt = std::env::var("OPT_LEVEL").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=ATERM_LAB_PROFILE={profile} opt-level={opt}");
}
