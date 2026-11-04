// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the `all` and `cli` controls link: the linker flags `verify perf cli-start` passes in
//! `ATERM_START_CONTROL_ALL` and `ATERM_START_CONTROL_CLI` (whitespace-separated, one per dylib,
//! from the aterm it measures), each handed to its program alone. A build without them links
//! both as `start-control-none` is linked.

fn main() {
    for (bin, var) in [
        ("start-control-all", "ATERM_START_CONTROL_ALL"),
        ("start-control-cli", "ATERM_START_CONTROL_CLI"),
    ] {
        println!("cargo::rerun-if-env-changed={var}");
        if let Some(flags) = std::env::var_os(var) {
            for flag in flags.to_string_lossy().split_whitespace() {
                println!("cargo::rustc-link-arg-bin={bin}={flag}");
            }
        }
    }
    println!("cargo::rerun-if-changed=build.rs");
}
