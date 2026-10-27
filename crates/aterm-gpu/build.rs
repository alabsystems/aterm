// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Emits `gl_arm`, `d3d11_arm`, `clean_islands`, and `anchor_rules` cfgs.

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

fn main() {
    clean_islands::emit();
    anchor_rules::emit();
    println!("cargo:rustc-check-cfg=cfg(gl_arm)");
    println!("cargo:rustc-check-cfg=cfg(d3d11_arm)");
    let os = std::env::var("CARGO_CFG_TARGET_OS");
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32");
    let gl_arm = os.as_deref() == Ok("linux") || wasm;
    if gl_arm {
        println!("cargo:rustc-cfg=gl_arm");
    }
    let d3d11_arm = os.as_deref() == Ok("windows");
    if d3d11_arm {
        println!("cargo:rustc-cfg=d3d11_arm");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
