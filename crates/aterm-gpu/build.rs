// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FLIP's one cfg alias: `wgpu_arm` — "the wgpu renderer arm is compiled
//! into this build". True on every non-macOS target (where wgpu remains the
//! production backend) and on macOS ONLY under the `wgpu-oracle` feature
//! (test/bench builds, activated by the manifest's self-dev-dependency), so
//! the differential ladder keeps both arms while the shipped macOS closure
//! carries none of it. A build script rather than a spelled-out
//! `cfg(any(...))` at ~200 sites because the predicate mixes a target test
//! with a feature test and has exactly one correct spelling; first-party
//! build scripts do not move the forge `build_scripts` ratchet (it counts
//! third-party packages only — budget.rs `ScopeDetail`).

fn main() {
    println!("cargo:rustc-check-cfg=cfg(wgpu_arm)");
    let mac = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let oracle = std::env::var("CARGO_FEATURE_WGPU_ORACLE").is_ok();
    if !mac || oracle {
        println!("cargo:rustc-cfg=wgpu_arm");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
