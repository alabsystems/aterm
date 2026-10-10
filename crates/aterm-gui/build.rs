// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// build.rs — the facts this crate compiles in are COMMITTED CONTENT and the
// TARGET, nothing else: the `a11y_tree` cfg, the update pin (derived from the
// committed paper master) and the target triple, plus the Windows icon of the
// dev `aterm-gui.exe`.
//
// NOT HERE: any build fact that moves with a commit, a version bump, the
// compiler, the profile or the flags (commit, build number, build time, dev
// counter, version, compiler provenance, profile, trust_verify). The binary
// crate owns them (`crates/aterm/src/main.rs`'s `BUILD_STAMP`, stamped by
// `crates/aterm/build.rs`) and installs them at startup
// (`aterm_types::version`). A build script's `rustc-env` reaches every unit of
// its package, so while this script stamped them, every commit (an empty one
// included) recompiled all of aterm-gui and everything linking it, and the
// library the Trust lane verifies carried bytes the shipped build did not.

use aterm_digest::Sha256;

/// Fixed 64-byte lowercase record used only when no paper master is pinned (a fork).
/// This tree always has one (`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`), so the
/// release cutter compares the Mach-O record against it and rejects this sentinel.
const UNPINNED_UPDATE_PIN_SENTINEL: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

fn main() {
    // ONE NAME FOR "the accessibility tree is compiled in". It is a FEATURE on
    // macOS/Windows (where the platform has its own surface, or the weight is
    // genuinely optional) and a PLATFORM FACT on Linux, where AccessKit is the
    // only path to AT-SPI and therefore to a screen reader. Folding both here
    // keeps 70-odd cfg sites from having to spell the policy — and keeps the
    // policy in one place when it changes again.
    println!("cargo::rustc-check-cfg=cfg(a11y_tree)");
    let a11y_feature = std::env::var_os("CARGO_FEATURE_A11Y_ACCESSKIT").is_some();
    let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    if a11y_feature || linux {
        println!("cargo::rustc-cfg=a11y_tree");
    }

    // The update pin: the fingerprint of the paper master, the ONE anchor that
    // authorizes a release — the SAME compile-time input `aterm-update`'s
    // `compiled_update_pin_sha256` reports. `build_info.rs` places this exact
    // fixed-width value in `__DATA,__aterm_upin`, allowing the release cutter to prove
    // the x86_64 slice's authority without executing it under Rosetta. Invalid
    // non-empty inputs fail the build instead of embedding an ambiguous record; a fork
    // with no master gets the explicit zero sentinel.
    // The anchor is the COMMITTED constant, not build-environment state: read it from
    // the one file that owns it so the embedded record proves the same key the runtime
    // verifies under.
    let update_pin_sha256 = match aterm_update_core::pins::PAPER_MASTER_PUBKEYS.first() {
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

    // No `rerun-if-changed` on git or on the workspace manifest, and no
    // `rerun-if-env-changed`: nothing above reads either (the build facts live
    // in `crates/aterm/build.rs`). The update pin comes from a build-dependency,
    // which cargo already tracks.
    println!("cargo:rerun-if-changed=build.rs");

    // Windows: compile + link the application icon into the DEV `aterm-gui.exe`
    // so the taskbar button, Alt-Tab, titlebar and Explorer show the aterm icon
    // instead of the generic exe glyph (the shipped `aterm.exe` gets its
    // resources from crates/aterm/build.rs — link args reach only the emitting
    // package's own bins). Gated on the TARGET os (build scripts run on the
    // host, so `cfg!` would reflect the wrong platform); a missing resource
    // compiler is a warning rather than a failure, so a toolchain-less build
    // still works. No-op on every non-Windows target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/aterm.ico");
        let outcome = aterm_winres::compile(std::path::Path::new("assets/aterm.rc"));
        if !outcome.is_linked() {
            println!("cargo:warning=aterm-gui: window icon not embedded: {outcome}");
        }
    }
}
