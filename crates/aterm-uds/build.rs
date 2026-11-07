// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(trust_std)`: this build's std is Trust's, whose spawned child inherits its stdio and
//! nothing else unless its command asks for upstream's inheritance
//! (`CommandExt::inherit_all_fds`), which a pass-through command (`spawnfd::transparent`) does.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(trust_std)");
    if trust_compiler() {
        println!("cargo::rustc-cfg=trust_std");
    }
}

/// Checks `RUSTC -vV` for a `trust:` line, as `crates/aterm-log/clean_islands.rs` does, rather
/// than `CARGO_CFG_TRUST_VERIFY`: the unverified lane and lint drivers omit that cfg, and their std
/// is Trust's all the same.
fn trust_compiler() -> bool {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    #[expect(
        clippy::disallowed_types,
        reason = "a build script, never linked into the shipped binary: one `-vV` of the compiler"
    )]
    let mut probe = std::process::Command::new(rustc);
    probe.arg("-vV").output().is_ok_and(|out| {
        out.stdout
            .split(|&byte| byte == b'\n')
            .any(|line| line.starts_with(b"trust:"))
    })
}
