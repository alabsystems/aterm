// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The linker of a debug build on aarch64 Linux, for the build scripts of the packages whose
//! executables pass 128 MB of code (aterm's, aterm-gui's tests), which mount this file.
//!
//! An AArch64 call reaches 128 MB, so past that the linker routes far calls through stubs it
//! places itself. GNU ld 2.42 (Ubuntu 24.04's) takes 51 s over aterm's debug executable, which
//! lld links in 3.2 s; over aterm-gui's lib test as `targo --unverified test --workspace` builds
//! it (incremental, workspace features), it ran 980 s and still left four calls out of reach
//! (`relocation truncated to fit: R_AARCH64_CALL26`), where lld took 4.7 s. Trust ships no lld
//! on Linux, so a debug build takes the one in the checkout's stock pin ([`stock_pin::STOCK_PIN`],
//! which `verify provision` installs), through its `gcc-ld` folder as rustc links with its own
//! (`-B<gcc-ld> -fuse-ld=lld`): a STOCK EXCEPTION, the stock toolchain's linker over Trust's
//! objects (LLD 22.1 over LLVM 23's here). Where rustup holds no such toolchain, and in every
//! release build, the system's linker links as before. A pin rustup installs later reruns the
//! script, which watches rustup's `update-hashes`, unless the rustup home did not exist yet.

#[path = "src/stock_pin.rs"]
mod stock_pin;

use std::path::PathBuf;

/// Link the calling package's executables with the stock pin's lld where this build takes it
/// ([`gcc_ld`]) and rustup holds it.
pub fn emit() {
    println!("cargo::rerun-if-changed=../aterm-toolchain/link_rules.rs");
    println!("cargo::rerun-if-changed=../aterm-toolchain/src/stock_pin.rs");
    let Some(gcc_ld) = gcc_ld() else {
        return;
    };
    println!("cargo::rerun-if-env-changed=RUSTUP_HOME");
    let lld = gcc_ld.join("ld.lld");
    if lld.is_file() {
        // A toolchain removed later reruns the script, which then leaves the linker alone.
        println!("cargo::rerun-if-changed={}", lld.display());
        println!("cargo::rustc-link-arg=-B{}", gcc_ld.display());
        println!("cargo::rustc-link-arg=-fuse-ld=lld");
    } else if let Some(hashes) = rustup_home()
        .map(|home| home.join("update-hashes"))
        .filter(|hashes| hashes.is_dir())
    {
        // rustup writes a file there for each toolchain it installs, the pin's included.
        println!("cargo::rerun-if-changed={}", hashes.display());
    }
}

/// Where the stock pin's `gcc-ld` folder is, for a debug build for aarch64 Linux on that host;
/// `None` for any other build.
fn gcc_ld() -> Option<PathBuf> {
    let var = |key: &str| std::env::var(key).ok();
    let host = var("HOST")?;
    let wanted = var("PROFILE")? == "debug"
        && var("TARGET")? == host
        && var("CARGO_CFG_TARGET_ARCH")? == "aarch64"
        && var("CARGO_CFG_TARGET_OS")? == "linux"
        && var("CARGO_CFG_TARGET_ENV")? == "gnu";
    if !wanted {
        return None;
    }
    Some(
        rustup_home()?
            .join("toolchains")
            .join(format!("{}-{host}", stock_pin::STOCK_PIN))
            .join("lib/rustlib")
            .join(&host)
            .join("bin/gcc-ld"),
    )
}

/// Rustup's home as `aterm_toolchain::rustup::rustup_home` reads it: a non-empty `$RUSTUP_HOME`,
/// else `$HOME/.rustup`.
fn rustup_home() -> Option<PathBuf> {
    match std::env::var_os("RUSTUP_HOME") {
        Some(home) if !home.is_empty() => Some(PathBuf::from(home)),
        _ => Some(PathBuf::from(std::env::var_os("HOME")?).join(".rustup")),
    }
}
