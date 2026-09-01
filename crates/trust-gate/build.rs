// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// build.rs — THE compiler gate. This workspace compiles with the Trust toolchain
// and nothing else: rust-toolchain.toml pins rustup's `trust` toolchain (a
// symlink atpkg maintains at ~/.rustup/toolchains/trust -> <prefix>/store/trust/
// current) and .cargo/config.toml's verification opt-out is scoped to that
// compiler by `[target.'cfg(trust_verify)']`, so an upstream rustc never even
// sees the `-Z` flag. Before this crate, reaching an upstream rustc — a
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
// on the opt-out flag.
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
// cannot join two x86_64 slices, so no release is ever cut there. The THIRD
// sanctioned lane is the PUBLIC SNAPSHOT: its export swaps rust-toolchain.toml
// for the stock pin (publish/transforms.sh), so the gate reads the tree's own
// committed pin and stands down only when it explicitly names a non-trust
// channel (gate::tree_pins_trust — fail-closed on a missing or channelless pin).
//
// Ends with `cargo:` directives only where they matter: the script re-runs when
// cargo resolves a different compiler (RUSTC); a toolchain SWAP behind the same
// path — the store's `current` link moving — re-fingerprints every unit anyway,
// because cargo keys fingerprints on `rustc -vV`.

use std::env;
use std::ffi::OsString;
use std::process::Command;

#[path = "src/gate.rs"]
mod gate;

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTC");

    // The tree's OWN pin decides whether this gate is armed. The dev workspace
    // pins `channel = "trust"` (the 2026-08-30 standing directive — enforce);
    // the PUBLIC SNAPSHOT's export deliberately swaps this file for the stock
    // pin (publish/transforms.sh), and its committed stock-Rust gate
    // (publish/DECISIONS.md) then builds with upstream rustc under anonymous
    // git — enforcing the trust marker there made the public snapshot
    // unbuildable by its own gate. Fail-closed: missing/unreadable pin, or no
    // channel line, still enforces (gate::tree_pins_trust).
    let pin_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rust-toolchain.toml");
    println!("cargo:rerun-if-changed={}", pin_path.display());
    let pin = std::fs::read_to_string(&pin_path).ok();

    let host = env::var("HOST").unwrap_or_default();
    let target = env::var("TARGET").unwrap_or_default();
    // Cheap early-out for the cross lanes; `refusal` re-checks it regardless.
    if host != target {
        return;
    }
    // `RUSTC` is the compiler cargo resolved (rustup's proxy hands cargo the
    // toolchain's absolute path; Homebrew's cargo says a bare `rustc`, which
    // then resolves on the PATH cargo gave us — the same PATH its units use).
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
        pin.as_deref(),
    ) else {
        return;
    };
    eprintln!("{message}");
    std::process::exit(1);
}

/// Is an `aterm` executable on this process's PATH? Decides the remedy: a
/// machine with aterm has atpkg, and `aterm pkg doctor` re-links the toolchain;
/// one without needs the installer.
fn aterm_on_path() -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    let name = if cfg!(windows) { "aterm.exe" } else { "aterm" };
    env::split_paths(&path).any(|dir| dir.join(name).is_file())
}
