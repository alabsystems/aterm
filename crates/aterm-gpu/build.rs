// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Defines three cfgs, so a mixed target/feature predicate has one spelling
//! instead of a `cfg(any(...))` at ~200 sites:
//!
//! * `wgpu_arm`: the wgpu renderer arm is compiled into this build. True where
//!   wgpu is the production backend (Windows, the BSDs) and, on macOS and
//!   Linux, only under the `wgpu-oracle` feature (the differential lane,
//!   `targo --unverified test -p aterm-gpu --features wgpu-oracle`), so those
//!   tests keep both arms while every other macOS and Linux build carries none
//!   of wgpu. Never on wasm32, which has no wgpu at all.
//! * `gl_arm`: the first-party OpenGL ES arm (`src/gl`,
//!   docs/AUDIT-third-party-2026-10-15.md GW.4), is compiled into this build:
//!   every Linux build (GLES 3.0 over EGL, and over GLX for X11 windows) and
//!   every wasm32 build (WebGL2 through aterm-wasm-abi imports, wasm-platform
//!   WP.8).
//! * `multi_arm`: more than one backend arm is compiled into this build (the
//!   device layer's enums then have more than one variant). It gates the
//!   crossing refusals, the match arms that panic by name when a handle from
//!   one arm reaches another arm's verb; a single-arm build has no such
//!   pattern to spell. The arms counted: Metal (macOS), wgpu (`wgpu_arm`) and
//!   GL (`gl_arm`). D3D11 (GW.5) adds itself to the count here.
//!
//! It also asks the compiler for `cfg(clean_islands)`, and Cargo's cfg query for
//! `cfg(anchor_rules)` (crates/aterm-spec-anchor/anchor_rules.rs).

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

// `cfg(anchor_rules)`: which anchor attributes this crate writes.
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
