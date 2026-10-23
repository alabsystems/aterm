// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Emits `wgpu_arm`, `gl_arm`, `multi_arm`, `clean_islands`, and `anchor_rules` cfgs.

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

fn main() {
    clean_islands::emit();
    anchor_rules::emit();
    println!("cargo:rustc-check-cfg=cfg(wgpu_arm)");
    println!("cargo:rustc-check-cfg=cfg(multi_arm)");
    println!("cargo:rustc-check-cfg=cfg(gl_arm)");
    let os = std::env::var("CARGO_CFG_TARGET_OS");
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32");
    let mac = os.as_deref() == Ok("macos");
    let oracle = std::env::var("CARGO_FEATURE_WGPU_ORACLE").is_ok();
    let gl_arm = os.as_deref() == Ok("linux") || wasm;
    let wgpu_arm = ((!mac && !gl_arm) || oracle) && !wasm;
    if wgpu_arm {
        println!("cargo:rustc-cfg=wgpu_arm");
    }
    if gl_arm {
        println!("cargo:rustc-cfg=gl_arm");
    }
    let arms = usize::from(mac) + usize::from(wgpu_arm) + usize::from(gl_arm);
    if arms > 1 {
        println!("cargo:rustc-cfg=multi_arm");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
