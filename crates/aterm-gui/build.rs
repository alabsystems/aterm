// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_digest::Sha256;

#[path = "../aterm-update-core/src/pins.rs"]
#[allow(dead_code)]
mod pins;

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

/// Zero sentinel used when no paper master is pinned (`pins::PAPER_MASTER_PUBKEYS`).
const UNPINNED_UPDATE_PIN_SENTINEL: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

fn main() {
    println!("cargo::rustc-check-cfg=cfg(a11y_tree)");
    if matches!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("linux" | "macos" | "windows")
    ) {
        println!("cargo::rustc-cfg=a11y_tree");
    }
    clean_islands::emit();
    anchor_rules::emit();

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

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../aterm-update-core/src/pins.rs");

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
