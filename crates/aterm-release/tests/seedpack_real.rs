// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rehearsal of the cut-time seed gate against the REAL `dist/toolchain-seed`
//! (release spec / TOOLCHAIN-PACKAGE-MANAGER.md §9.1): exactly what
//! `cargo ship cut` will run, so a broken or drifted seed fails here first,
//! not mid-cut. SKIPS (honestly, with a printed reason) when there is no seed
//! dir or no `ATERM_PKG_ROOTKEY` in the environment — a fresh clone or CI box
//! without the owner's registry must stay green.

#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/seedpack.rs"]
#[allow(dead_code)]
mod seedpack;

#[test]
fn the_real_seed_validates_under_the_configured_root_key() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dist");
    let Some(dir) = seedpack::resolve(&dist) else {
        eprintln!("SKIP: no dist/toolchain-seed (nothing staged for the next cut)");
        return;
    };
    let Ok(root_key) = std::env::var("ATERM_PKG_ROOTKEY") else {
        eprintln!("SKIP: ATERM_PKG_ROOTKEY not in the environment (owner-only rehearsal)");
        return;
    };
    let stat = seedpack::validate(&dir, &root_key)
        .expect("dist/toolchain-seed must pass the exact gate `cargo ship cut` runs");
    assert!(!stat.programs.is_empty());
    println!(
        "seed OK: {} file(s), {} bytes, index_build {}, programs {:?}",
        stat.files, stat.bytes, stat.index_build, stat.programs
    );
}
