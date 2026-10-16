// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Defines two cfgs, so a mixed target/feature predicate has one spelling
//! instead of a `cfg(any(...))` at ~200 sites:
//!
//! * `wgpu_arm`: the wgpu renderer arm is compiled into this build. True on
//!   every non-macOS target (where wgpu is the production backend) and on
//!   macOS only under the `wgpu-oracle` feature (test builds, via the
//!   manifest's self-dev-dependency), so the differential tests keep both arms
//!   while the shipped macOS build carries none of wgpu.
//! * `multi_arm`: more than one backend arm is compiled into this build (the
//!   device layer's enums then have more than one variant). It gates the
//!   crossing refusals, the match arms that panic by name when a handle from
//!   one arm reaches another arm's verb; a single-arm build has no such
//!   pattern to spell. The arms counted: Metal (macOS) and wgpu (`wgpu_arm`).
//!   A first-party arm (GL at docs/AUDIT-third-party-2026-10-15.md GW.4,
//!   D3D11 at GW.5) adds itself to the count here.
//!
//! It also asks the compiler for `cfg(clean_islands)`.

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

fn main() {
    clean_islands::emit();
    println!("cargo:rustc-check-cfg=cfg(wgpu_arm)");
    println!("cargo:rustc-check-cfg=cfg(multi_arm)");
    let mac = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let oracle = std::env::var("CARGO_FEATURE_WGPU_ORACLE").is_ok();
    let wgpu_arm = !mac || oracle;
    if wgpu_arm {
        println!("cargo:rustc-cfg=wgpu_arm");
    }
    let arms = usize::from(mac) + usize::from(wgpu_arm);
    if arms > 1 {
        println!("cargo:rustc-cfg=multi_arm");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
