// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! atpkg's binaries link as the shipped `aterm` image does on Apple silicon, with chained
//! fixups (`crates/aterm/build.rs`'s `chained_fixups`), so code that links only without them
//! (an address taken of a load-time constructor's slot, which that link turns into an
//! `__init_offsets` entry with no address) fails atpkg's own build, not first the image's.

fn main() {
    let target = |key: &str| std::env::var(key).unwrap_or_default();
    if target("CARGO_CFG_TARGET_OS") == "macos" && target("CARGO_CFG_TARGET_ARCH") == "aarch64" {
        println!("cargo:rustc-link-arg-bins=-Wl,-fixup_chains");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
