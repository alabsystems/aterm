// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mount in build scripts of crates with `clean { … }` proof islands to emit `cfg(clean_islands)`
//! only under a Trust compiler.

/// Declare `cfg(clean_islands)` for the calling crate, and set it under a Trust compiler.
pub fn emit() {
    println!("cargo::rustc-check-cfg=cfg(clean_islands)");
    if trust_compiler() {
        println!("cargo::rustc-cfg=clean_islands");
    }
}

/// Checks `RUSTC -vV` for a `trust:` line rather than `CARGO_CFG_TRUST_VERIFY`, which Trust sets
/// only during verification and omits under lint drivers.
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
