// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 build.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let unix = env::var("CARGO_CFG_TARGET_FAMILY")
        .unwrap_or_default()
        .split(',')
        .any(|family| family == "unix");
    let feature = |name: &str| env::var_os(format!("CARGO_FEATURE_{name}")).is_some();

    let apple = matches!(target_os.as_str(), "ios" | "macos");
    let free_unix = unix && !apple && target_os != "android" && target_os != "emscripten";
    let redox = target_os == "redox";

    alias("macos_platform", target_os == "macos");
    alias("windows_platform", target_os == "windows");
    alias("free_unix", free_unix);
    alias("x11_platform", feature("X11") && free_unix && !redox);
    alias("wayland_platform", feature("WAYLAND") && free_unix && !redox);
}

/// Declare `name` to `--check-cfg` and set it when `on`.
fn alias(name: &str, on: bool) {
    println!("cargo:rustc-check-cfg=cfg({name})");
    if on {
        println!("cargo:rustc-cfg={name}");
    }
}
