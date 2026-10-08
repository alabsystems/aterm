// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// build.rs — THE compiler gate. This workspace compiles with the Trust toolchain
// and nothing else: rust-toolchain.toml pins `channel = "trust"`, which is the
// aterm-managed store's toolchain (`<prefix>/store/trust/<build>/bin/{targo,
// trustc}`, reached through the `<prefix>/bin` shims — `aterm pkg which targo`;
// on a machine that also has rustup, atpkg maintains ~/.rustup/toolchains/trust
// -> <prefix>/rustup/trust, its view of the store's current build, so rustup's
// proxy resolves the same pin;
// stated 2026-09-18) and .cargo/config.toml's Trust-only flags are scoped to
// that compiler by `[target.'cfg(trust_verify)']`, so an upstream rustc never
// even sees a `-Z` flag. Before this crate, reaching an upstream rustc — a
// `RUSTUP_TOOLCHAIN=stable` override, a Homebrew cargo that ignores the pin, a
// dangling `trust` link after a store swap — failed the build on whichever
// first-party unit hit Trust-only syntax first: deep in the graph, with an error
// naming the syntax rather than the cause. This build script has NO
// dependencies, so cargo runs it among the very first units, and it refuses a
// non-Trust compiler with ONE message that names the fix.
//
// WHO DEPENDS ON IT: crates/aterm, the shipped binary. The library crates do
// not, so `-p <lib>` builds and the dev-only `[[bin]]` conveniences stay
// ungated (there is no CI to build them — owner decision, docs/PROCESS.md); an
// upstream compiler there fails, if at all, on genuine Trust-only syntax, never
// on a Trust-only flag.
//
// THE MARKER and the TARGET == HOST rule are documented and unit-tested in
// src/gate.rs (`is_trust_compiler`, `refusal`). Summary: `rustc -vV` from a
// Trust compiler carries a `trust: <version>` key (measured 2026-08-29 on the
// store build acb08e761); the gate applies only to native builds, because the
// release's x86_64-apple-darwin compat slice (crates/aterm-release/src/
// buildplan.rs — `--target` + `RUSTUP_TOOLCHAIN=stable`) and the Windows
// cfg-validation build are CROSS builds on upstream stable by design. An
// x86_64 host cutting that slice would see TARGET == HOST and be refused, and
// that is fine: buildplan hardcodes aarch64 as the native Trust slice and lipo
// cannot join two x86_64 slices, so no release is ever cut there. Until
// 2026-09-29 the gate also stood down for a tree whose committed pin named a
// non-trust channel, for the PUBLIC SNAPSHOT's then-stock pin; the export now
// keeps `channel = "trust"`, so the stand-down went and every tree enforces.
//
// Ends with `cargo:` directives only where they matter: the script re-runs when
// cargo resolves a different compiler (RUSTC) or rustup picks another toolchain
// (RUSTUP_TOOLCHAIN); a toolchain SWAP behind the same
// path — the store's `current` link moving — re-fingerprints every unit anyway,
// because cargo keys fingerprints on `rustc -vV`.

use std::env;
use std::ffi::OsString;
use std::process::Command;

#[path = "src/gate.rs"]
mod gate;

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");

    let host = env::var("HOST").unwrap_or_default();
    let target = env::var("TARGET").unwrap_or_default();
    // Cheap early-out for the cross lanes; `refusal` re-checks it regardless.
    if host != target {
        return;
    }
    // `RUSTC` is the compiler cargo resolved — usually a bare `rustc`, under
    // rustup's cargo and Homebrew's alike, which then resolves on the PATH cargo
    // gave us (the same PATH its units use). Which toolchain rustup picked is
    // `RUSTUP_TOOLCHAIN`, which rustup's proxy sets for cargo and its build
    // scripts; unset means this cargo is not rustup's.
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let verbose = match Command::new(&rustc).arg("-vV").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        Ok(out) => format!(
            "(`-vV` failed: {})",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => format!("(could not run it: {e})"),
    };
    let Some(message) = gate::refusal(
        &host,
        &target,
        &rustc.to_string_lossy(),
        &verbose,
        aterm_on_path(),
        env::var("RUSTUP_TOOLCHAIN").ok().as_deref(),
    ) else {
        return;
    };
    eprintln!("{message}");
    std::process::exit(1);
}

/// Is an `aterm` executable on this process's PATH? Decides the remedy: a
/// machine with aterm has atpkg's Trust toolchain (`targo`, and `aterm pkg
/// repair` for rustup's `trust` link); one without needs the installer.
fn aterm_on_path() -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    let name = if cfg!(windows) { "aterm.exe" } else { "aterm" };
    env::split_paths(&path).any(|dir| dir.join(name).is_file())
}
