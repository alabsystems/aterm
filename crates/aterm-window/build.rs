// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 build.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (Replaced the
// `cfg_aliases!` build dependency with the explicit `cargo:rustc-cfg` lines it expanded to;
// the Android, iOS, Orbital and Web aliases went with those backends.)

use std::env;

fn main() {
    // The script doesn't depend on our code.
    println!("cargo:rerun-if-changed=build.rs");

    // The platform cfgs the sources use. Each `alias(...)` below is what upstream's
    // `cfg_aliases!` table declared, evaluated against the target cargo hands a build script.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let unix = env::var("CARGO_CFG_TARGET_FAMILY")
        .unwrap_or_default()
        .split(',')
        .any(|family| family == "unix");
    let feature = |name: &str| env::var_os(format!("CARGO_FEATURE_{name}")).is_some();

    // apple: { any(target_os = "ios", target_os = "macos") }
    let apple = matches!(target_os.as_str(), "ios" | "macos");
    // free_unix: { all(unix, not(apple), not(android_platform), not(target_os = "emscripten")) }
    let free_unix = unix && !apple && target_os != "android" && target_os != "emscripten";
    // x11_platform / wayland_platform: { all(feature = "…", free_unix, not(redox)) }
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
