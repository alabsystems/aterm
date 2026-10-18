// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// build.rs compiles in only committed content and the target (the `a11y_tree` cfg,
// the update pin, the target triple and the dev `aterm-gui.exe` icon), plus
// `cfg(clean_islands)`, which the compiler decides. Other facts that move with a
// commit, version or toolchain belong to `crates/aterm/build.rs` (`BUILD_STAMP`,
// installed at startup via `aterm_types::version`): a build script's `rustc-env`
// reaches every unit of its package, so stamping them here would rebuild all of
// aterm-gui on every commit.

use aterm_digest::Sha256;

// The committed trust anchors, compiled in as source rather than through an
// `aterm-update-core` build-dependency, which would build that crate's closure
// (`ring` included) a second time for the host to read one constant. `pins.rs` is
// self-contained outside its `#[cfg(test)]` module, so it compiles here unchanged.
#[path = "../aterm-update-core/src/pins.rs"]
#[allow(dead_code)]
mod pins;

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

/// Fixed 64-byte lowercase record used only when no paper master is pinned (a fork).
/// This tree always has one (`aterm-update-core`'s `pins::PAPER_MASTER_PUBKEYS`), so the
/// release cutter compares the Mach-O record against it and rejects this sentinel.
const UNPINNED_UPDATE_PIN_SENTINEL: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

fn main() {
    // One cfg for "the accessibility tree is compiled in": on Linux, always, where the
    // AT-SPI publisher (aterm-a11y) is the only path to a screen reader. The cfg sites
    // never spell the policy.
    println!("cargo::rustc-check-cfg=cfg(a11y_tree)");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo::rustc-cfg=a11y_tree");
    }
    clean_islands::emit();

    // The update pin: the SHA-256 fingerprint of the committed paper master, the same
    // value `aterm-update`'s `compiled_update_pin_sha256` reports. `build_info.rs`
    // places it in `__DATA,__aterm_upin` so the release cutter can check the x86_64
    // slice's authority without running it. A malformed key fails the build; a fork
    // with no master gets the zero sentinel.
    let update_pin_sha256 = match pins::PAPER_MASTER_PUBKEYS.first() {
        Some(encoded) if !encoded.is_empty() => {
            let raw = aterm_codec::base64::decode_strict(encoded.as_bytes())
                .expect("PAPER_MASTER_PUBKEYS[0] must be standard base64");
            assert_eq!(
                raw.len(),
                32,
                "PAPER_MASTER_PUBKEYS[0] must decode to an Ed25519 32-byte public key"
            );
            aterm_codec::hex::encode_lower(&Sha256::digest(raw))
        }
        _ => UNPINNED_UPDATE_PIN_SENTINEL.to_string(),
    };
    println!("cargo:rustc-env=ATERM_UPDATE_PIN_SHA256={update_pin_sha256}");

    println!(
        "cargo:rustc-env=ATERM_BINARY_TARGET={}",
        std::env::var("TARGET").expect("Cargo target")
    );

    // Nothing above reads git, the workspace manifest or a non-Cargo env var. `pins.rs`
    // is compiled in by `#[path]`, not tracked as a build-dependency, so it is named.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../aterm-update-core/src/pins.rs");

    // Windows: link the application icon into the dev `aterm-gui.exe` (the shipped
    // `aterm.exe` gets its resources from crates/aterm/build.rs: link args reach only
    // the emitting package's own bins). Gated on the TARGET os, since a build script
    // runs on the host; a missing resource compiler is a warning, not a failure.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // The dev `aterm-gui.exe`'s main-thread stack, as crates/aterm/build.rs reserves
        // for the shipped images (a link arg reaches only its own package's bins).
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg-bins=/STACK:{}", 8 * 1024 * 1024);
        }
        println!("cargo:rerun-if-changed=assets/aterm.ico");
        let outcome = aterm_winres::compile(std::path::Path::new("assets/aterm.rc"));
        if !outcome.is_linked() {
            println!("cargo:warning=aterm-gui: window icon not embedded: {outcome}");
        }
    }
}
