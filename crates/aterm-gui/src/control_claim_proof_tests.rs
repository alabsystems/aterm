// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seam that carries the version an image proof admitted to the claim: the shipping proof
//! of a rostered holder ([`prove_rostered_claim`]) over a real store in a scratch prefix, with
//! only the kernel's connection facts and the claimant's executable path supplied by the test.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use aterm_agent::harness::contract::{SCOPED, UNIVERSAL};

use super::*;
use crate::native_session_policy::{ImageReads, ProofRead, SessionPolicy};

/// A scratch prefix holding `store/grok-alab/<build>/bin/grok-alab`, ready and complete, with
/// the version record atpkg writes beside a staged build where `version` names one
/// (`store/grok-alab/<build>.version`).
fn store_with_build(
    root: &Path,
    build: u64,
    version: Option<&str>,
) -> (atpkg::store::Layout, PathBuf) {
    let layout = atpkg::store::Layout {
        prefix: root.join("pkg"),
    };
    let dir = layout.build_dir("grok-alab", build);
    std::fs::create_dir_all(dir.join("bin")).expect("build dir");
    let exe = dir.join("bin").join("grok-alab");
    std::fs::write(&exe, b"#!/bin/sh\n").expect("exe");
    atpkg::store::mark_build_ready(&dir).expect("ready");
    if let Some(version) = version {
        std::fs::write(
            dir.with_file_name(format!("{build}.version")),
            format!("version v1\n{version}\n"),
        )
        .expect("version record");
        assert_eq!(
            atpkg::store::recorded_version(&dir).as_deref(),
            Some(version),
            "the fixture's record is the one atpkg reads"
        );
    }
    (layout, exe)
}

/// The proof read of `layout`'s own registry, as a claim's proof reads the configured one.
fn read_of(layout: &atpkg::store::Layout, generation: u64) -> ProofRead {
    ProofRead::of((
        aterm_agent::harness::registry::for_layout(layout),
        generation,
    ))
}

/// This process as the claimant: the peer of its connection and the leader of the foreground.
fn me() -> (String, ClaimKernel) {
    let pid = std::process::id();
    (
        format!("grok-alab@{pid}"),
        ClaimKernel {
            peer: Some(pid),
            foreground: i32::try_from(pid).expect("a pid"),
        },
    )
}

/// THE VERSION A CLAIM'S PROOF CARRIES IS THE ONE ITS IMAGE STEP READ FROM THE BUILD IT ADMITTED
/// (D7), through the shipping proof: a store build's recorded version under contract 4 and under
/// contract 3, `link` for a development link's checkout, nothing for a build that records none.
/// A standing claim's re-proof reuses the pass without reading the store again and carries the
/// same version. Served through the shipping `meta set supervisor`, that version is what
/// `status` and `sessions` print (`wire_versions`), not the claim's own `version=`.
/// NEGATIVE HALF: a peer that is not the holder's pid, a foreground it does not lead, and an
/// executable outside the store prove nothing and carry no version.
#[test]
fn the_proof_carries_the_version_of_the_build_its_image_step_admitted() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let (layout, exe) = store_with_build(&root.join("recorded"), 412, Some("0.7.0+g1a2b3c"));
    let (holder, kernel) = me();
    let policy = SessionPolicy::default();
    let read = read_of(&layout, 1);
    let asked = Cell::new(0_u32);
    let prove = |policy: &SessionPolicy,
                 read: &ProofRead,
                 layout: &atpkg::store::Layout,
                 exe: &Path,
                 contract: Option<u32>,
                 kernel: ClaimKernel,
                 reuse: bool| {
        prove_rostered_claim(
            policy,
            read,
            &holder,
            contract,
            kernel,
            reuse,
            &ImageReads {
                exe: &|_| Some(exe.to_path_buf()),
                layout: &|| {
                    asked.set(asked.get() + 1);
                    Some(layout.clone())
                },
            },
        )
    };

    for contract in [Some(UNIVERSAL), Some(SCOPED), None] {
        let proof = prove(&policy, &read, &layout, &exe, contract, kernel, false)
            .unwrap_or_else(|| panic!("contract {contract:?} proves"));
        assert_eq!(proof.proven.pid, std::process::id());
        assert_eq!(
            proof.version.as_deref(),
            Some("0.7.0+g1a2b3c"),
            "contract {contract:?}: the admitted build's recorded version"
        );
    }

    // A re-proof of the standing claim reuses the pass: the store is not read, and the version
    // is the kept pass's.
    let full = asked.get();
    assert_eq!(full, 3, "each claim judged its image in full");
    let again = prove(&policy, &read, &layout, &exe, None, kernel, true).expect("the re-proof");
    assert_eq!(again.version.as_deref(), Some("0.7.0+g1a2b3c"));
    assert_eq!(asked.get(), full, "the reused pass read no store");
    // Another registry generation is another key: judged in full again, the same version.
    let moved = read_of(&layout, 2);
    let rejudged = prove(&policy, &moved, &layout, &exe, None, kernel, true).expect("re-judged");
    assert_eq!(rejudged.version.as_deref(), Some("0.7.0+g1a2b3c"));
    assert_eq!(asked.get(), full + 1);

    // A build that records no version: proven, with none to show.
    let (bare, bare_exe) = store_with_build(&root.join("bare"), 7, None);
    let proof = prove(
        &SessionPolicy::default(),
        &read_of(&bare, 1),
        &bare,
        &bare_exe,
        None,
        kernel,
        false,
    )
    .expect("an unversioned build proves");
    assert_eq!(proof.version, None);

    // A development link's checkout: `link`.
    let (linked, _) = store_with_build(&root.join("linked"), 9, Some("0.1.0"));
    let checkout = root.join("checkout");
    let built = checkout.join("target").join("release").join("grok-alab");
    std::fs::create_dir_all(built.parent().expect("dir")).expect("dir");
    std::fs::write(&built, b"x").expect("built");
    atpkg::link(
        &linked,
        "grok-alab",
        &checkout,
        &[PathBuf::from("target/release/grok-alab")],
    )
    .expect("dev link");
    for contract in [Some(UNIVERSAL), Some(SCOPED)] {
        let proof = prove(
            &SessionPolicy::default(),
            &read_of(&linked, 1),
            &linked,
            &built,
            contract,
            kernel,
            false,
        )
        .unwrap_or_else(|| panic!("the link proves contract {contract:?}"));
        assert_eq!(proof.version.as_deref(), Some("link"), "{contract:?}");
    }

    // NEGATIVE HALF.
    let fresh = SessionPolicy::default();
    let stray = root.join("stray-grok-alab");
    std::fs::write(&stray, b"x").expect("stray");
    assert!(
        prove(&fresh, &read, &layout, &stray, None, kernel, false).is_none(),
        "an executable outside the store"
    );
    let other_peer = ClaimKernel {
        peer: Some(1),
        ..kernel
    };
    assert!(prove(&fresh, &read, &layout, &exe, None, other_peer, false).is_none());
    let background = ClaimKernel {
        foreground: 1,
        ..kernel
    };
    assert!(prove(&fresh, &read, &layout, &exe, None, background, false).is_none());

    // Served as a claim: `status` and `sessions` print the version that proof carried.
    let store = crate::session_store::new_store();
    let handle = crate::session_store::test_handle(1);
    store
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .register(handle.clone());
    let (reply, _) = meta_wakes_admitting_for_test(
        &store,
        &handle.term,
        handle.local_id,
        &handle.ctx,
        &format!("set supervisor {holder} contract=4 scope=relaunch version=0.3.0 vendor=2.1.300"),
        Some(7),
        &|_, contract| prove(&policy, &read, &layout, &exe, contract, kernel, false),
        &|_, _| None,
    );
    assert!(reply.starts_with("OK scope=relaunch revision="), "{reply}");
    let meta = handle
        .ctx
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        crate::harness_claim::wire_versions(&meta, crate::metrics::now_us()),
        ("0.7.0+g1a2b3c".to_string(), "2.1.300".to_string()),
        "the proof's version, never the claim's `version=0.3.0`"
    );
}
