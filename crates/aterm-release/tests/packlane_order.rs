// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bundle pack lane's ORDER, enforced.
//!
//! `tools/atpkg-pack-bundle.sh`'s whole attestation story is sequencing: every
//! byte mutation (single-arch prune, fat-thin, retired-entrypoint hygiene,
//! symbol strip) happens on the mktemp stage BEFORE `atpkg relocate` (so the
//! optional Developer-ID signature is applied to final bytes and never
//! invalidated), before the smoke-exec and hello-world gates (so what they
//! prove is exactly what ships), and before tar/sha256/tree_root/sign (so the
//! Ed25519-signed manifest describes exactly the bytes clients receive —
//! the script's own §14 invariant). A refactor that moved any pass after the
//! relocate/sign boundary would ship bytes the signature does not describe,
//! and nothing at runtime would notice until a client's re-verify failed.
//!
//! Pinned by source order, the same technique `transcript_grid.rs` uses for
//! seedpack's warning text: the script is data, and the order IS the property.

use std::path::Path;

fn script() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/atpkg-pack-bundle.sh");
    std::fs::read_to_string(&path).expect("tools/atpkg-pack-bundle.sh is readable")
}

/// Byte offset of `needle`'s first occurrence, with a named failure.
fn at(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("the pack lane lost its {needle:?} marker"))
}

#[test]
fn every_byte_mutation_precedes_relocate_gates_tar_and_sign() {
    let s = script();
    // Section markers (`# --- …`), not loose phrases: the header's pipeline
    // diagram and gate list legitimately NAME the passes before any of them
    // runs, so a first-occurrence scan must key on the section heads.
    let prune = at(&s, "# --- single-arch hygiene");
    let hygiene = at(&s, "# --- retired-entrypoint hygiene");
    let strip = at(&s, "# --- PACK-TIME SYMBOL STRIP");
    let relocate = at(&s, "# --- relocate at PACK time");
    let smoke = at(&s, "# --- PER-BIN SMOKE-EXEC");
    let hello = at(&s, "# --- TOOLCHAIN HELLO-WORLD");
    let tar = at(&s, "# --- tar.zst with the SAME hygiene");
    let disk = at(&s, "DISK_INSTALLED=");

    // Mutations first, in their landed order…
    assert!(
        prune < hygiene && hygiene < strip,
        "prune -> hygiene -> strip"
    );
    // …then relocation (the last writer: vendoring + optional Dev-ID signing)…
    assert!(strip < relocate, "the strip must precede relocate/signing");
    // …then the gates that prove the FINAL bytes…
    assert!(
        relocate < smoke && smoke < hello,
        "gates run on relocated bytes"
    );
    // …and only then the tarball and the size that feed the signed manifest.
    assert!(hello < tar && tar < disk, "tar + DISK_INSTALLED come last");
}

#[test]
fn the_lane_keeps_its_fail_closed_guards_and_its_escape_hatches() {
    let s = script();
    // The still-runs probes: a stripped binary that will not start must fail
    // the pack, which is the entire safety argument for stripping at all.
    for guard in [
        "smoke_one",            // per-bin smoke-exec probe
        "hello, atpkg sysroot", // hello-world compile+run proof
        "strip -x -S",          // one flag discipline, all classes
        "codesign -f -s -",     // the explicit re-sign: strip only
        // regenerates LINKER-SIGNED adhoc
        // signatures; an explicitly signed
        // Mach-O (the trust-wp drivers) is
        // invalidated and native arm64
        // SIGKILLs it at exec
        "REFUSING fat Mach-O", // thin guard fails closed
        "has NO members — refusing to sign an empty tarball", // member-listing refusal
        "AppleDouble",         // xattr leak refusal
    ] {
        assert!(s.contains(guard), "the lane lost its {guard:?} guard");
    }
    // The escapes are documented knobs, never silent defaults.
    assert!(
        s.contains("--no-strip"),
        "the strip must keep its debugging escape"
    );
    assert!(
        s.contains("BUNDLE_KEEP_TARGETS"),
        "the prune must keep its cross-capable local escape"
    );
    // Hardlink awareness: a naive per-file strip would sever librustc_driver's
    // inode group and ship the 522 MB dylib twice.
    assert!(
        s.contains("ln -f") || s.contains("re-linked"),
        "the strip must stay hardlink-aware"
    );
}
