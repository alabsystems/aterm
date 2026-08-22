// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rehearsal of the cut-time seed gate against the REAL `dist/toolchain-seed`
//! (release spec / TOOLCHAIN-PACKAGE-MANAGER.md §9.1): exactly what
//! `cargo ship cut` will run, so a broken or drifted seed fails here first,
//! not mid-cut. SKIPS (honestly, with a printed reason) when there is no seed
//! dir — a fresh clone or CI box without the owner's registry must stay green.
//!
//! It validates through the SHIPPED CLIENT'S OWN chain (`atpkg::DirFetcher` →
//! roster admission → index selection under the compiled paper-master anchor —
//! see `seedpack::validate`), so it can only pass on a seed the released
//! binary would actually install from. An unarmed fork (empty
//! `atpkg::PKG_TRUST_ANCHORS`) skips: atpkg is inert there and there is no
//! anchor to validate a seed under.

#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/seedpack.rs"]
#[allow(dead_code)]
mod seedpack;

#[test]
fn the_real_seed_validates_under_the_client_chain() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dist");
    let Some(dir) = seedpack::resolve(&dist) else {
        eprintln!("SKIP: no dist/toolchain-seed (nothing staged for the next cut)");
        return;
    };
    if atpkg::PKG_TRUST_ANCHORS.is_empty() {
        eprintln!("SKIP: no paper master pinned in this build (atpkg inert)");
        return;
    }
    let stat = seedpack::validate(&dir)
        .expect("dist/toolchain-seed must pass the exact gate `cargo ship cut` runs");
    assert!(!stat.programs.is_empty());
    println!(
        "seed OK: {} file(s), {} bytes, index_build {}, programs {:?}",
        stat.files, stat.bytes, stat.index_build, stat.programs
    );
}

/// Rehearsal of the PER-ARCH DMG filter against the same real seed: exactly
/// what `dmg::create_arch_filtered` will do to the sealed `.lproj` at the next
/// cut — derive each triple's keep-set from the signed `[[artifact]]` rows,
/// stage "all manifests + only that triple's tarballs", and re-prove the
/// filtered registry through the client chain under `ArchScope::Only`. Runs on
/// hard links, so the gigabytes are never copied. SKIPS with the same honesty
/// as the test above when there is no seed or no anchor.
#[test]
fn the_real_seed_filters_cleanly_into_per_arch_registries() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dist");
    let Some(dir) = seedpack::resolve(&dist) else {
        eprintln!("SKIP: no dist/toolchain-seed (nothing staged for the next cut)");
        return;
    };
    if atpkg::PKG_TRUST_ANCHORS.is_empty() {
        eprintln!("SKIP: no paper master pinned in this build (atpkg inert)");
        return;
    }
    let by_triple =
        seedpack::assets_by_triple(&dir).expect("the validated seed yields its signed asset map");
    let stat = seedpack::validate(&dir).expect("universal validation");
    assert_eq!(
        stat.targets,
        by_triple.keys().cloned().collect(),
        "SeedStat.targets and the asset map must name the same triples"
    );

    let stage_root = std::env::temp_dir().join(format!("seedpack-arch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage_root);
    for (triple, keep) in &by_triple {
        let stage = stage_root.join(triple);
        std::fs::create_dir_all(&stage).unwrap();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_manifest = name.ends_with(".toml") || name.ends_with(".toml.sig");
            if is_manifest || keep.contains(&name) {
                std::fs::hard_link(entry.path(), stage.join(&name))
                    .expect("hard link into the per-arch stage");
            }
        }
        let scoped = seedpack::validate_scoped(&stage, seedpack::ArchScope::Only(triple))
            .unwrap_or_else(|e| panic!("the {triple}-filtered registry must validate: {e}"));
        assert_eq!(
            scoped.targets.iter().collect::<Vec<_>>(),
            vec![triple],
            "the filtered registry serves exactly its own triple"
        );
        // Negative control: the SAME filtered registry must refuse validation
        // under any OTHER covered triple — that other arch's promised rows are
        // absent here, which is exactly the "filter dropped a promised
        // artifact" refusal.
        for other in by_triple.keys().filter(|t| *t != triple) {
            let err = seedpack::validate_scoped(&stage, seedpack::ArchScope::Only(other))
                .expect_err("a wrong-scope validation must refuse");
            assert!(
                err.contains("filter") || err.contains("covers"),
                "the refusal must name the filter failure: {err}"
            );
        }
        println!(
            "per-arch filter OK: {triple} — {} file(s), {} artifact tarball(s)",
            scoped.files,
            keep.len()
        );
    }
    let _ = std::fs::remove_dir_all(&stage_root);
}
