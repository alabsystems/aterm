// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Defines the `wgpu_arm` cfg: the wgpu renderer arm is compiled into this
//! build. True on every non-macOS target (where wgpu is the production
//! backend) and on macOS only under the `wgpu-oracle` feature (test builds,
//! via the manifest's self-dev-dependency), so the differential tests keep
//! both arms while the shipped macOS build carries none of wgpu. A build
//! script gives the mixed target/feature predicate one spelling instead of a
//! `cfg(any(...))` at ~200 sites.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(wgpu_arm)");
    let mac = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let oracle = std::env::var("CARGO_FEATURE_WGPU_ORACLE").is_ok();
    if !mac || oracle {
        println!("cargo:rustc-cfg=wgpu_arm");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
