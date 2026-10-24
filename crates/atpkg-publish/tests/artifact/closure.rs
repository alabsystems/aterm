// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for `closure::prepare` refusals, symlink handling, and relocated repacks.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use atpkg_publish::artifact::closure::{self, Refusal};
use atpkg_publish::triple::Triple;

use super::fixture::{
    SEAL_COMMIT, SEAL_DRIVER, SEAL_TRIPLE, copy_tree, put, scratch, seal_fixture, seal_probe,
};

fn triple() -> Triple {
    Triple::parse(SEAL_TRIPLE).expect("a target")
}

struct Fx {
    _tmp: aterm_tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    private: PathBuf,
    stage: PathBuf,
}

fn fx(label: &str) -> Fx {
    let tmp = scratch(label);
    let root = std::fs::canonicalize(tmp.path()).expect("root");
    let (source, private) = seal_fixture(&root);
    let stage = root.join("stage");
    Fx {
        _tmp: tmp,
        root,
        source,
        private,
        stage,
    }
}

impl Fx {
    fn prepare(&self) -> Result<String, Refusal> {
        copy_tree(&self.source, &self.stage);
        closure::prepare(&self.source, &self.stage, triple(), &seal_probe)
    }
}

fn ino(p: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt as _;
    let m = std::fs::metadata(p).expect("stat");
    (m.dev(), m.ino())
}

fn no_links(root: &Path) -> bool {
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read").flatten() {
            let ft = std::fs::symlink_metadata(e.path())
                .expect("lstat")
                .file_type();
            if ft.is_symlink() {
                return false;
            }
            if ft.is_dir() {
                stack.push(e.path());
            }
        }
    }
    true
}

#[test]
fn a_complete_closure_survives_a_moved_root_and_a_repack() {
    let f = fx("closure-complete");
    assert_eq!(f.prepare().expect("prepared"), SEAL_COMMIT);
    let moved = f.root.join("elsewhere/toolchain");
    std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&f.stage, &moved).expect("move the stage");
    std::fs::rename(&f.source, f.root.join("hidden-seal")).expect("hide");
    std::fs::rename(
        f.private.parent().expect("registry"),
        f.root.join("hidden-private"),
    )
    .expect("hide");
    let target = moved.join(format!("lib/rustlib/{SEAL_TRIPLE}/lib"));
    assert_eq!(
        std::fs::read(target.join("librustc_middle-fixture.rmeta")).expect("private"),
        b"matching private metadata\n"
    );
    assert_eq!(
        std::fs::read(target.join("libproc_macro-fixture.rmeta")).expect("host"),
        b"matching host metadata\n"
    );
    assert_eq!(
        ino(&target.join(SEAL_DRIVER)),
        ino(&moved.join("lib").join(SEAL_DRIVER))
    );
    assert_eq!(
        ino(&moved.join("lib/rustlib/src/rust/compiler/rustc_driver/src/lib.rs")),
        ino(&moved.join("lib/rustlib/rustc-src/rust/compiler/rustc_driver/src/lib.rs"))
    );
    assert!(no_links(&moved), "no symlink reaches the client");

    std::fs::write(
        moved.join("lib").join(SEAL_DRIVER),
        b"relocated same driver\n",
    )
    .expect("w");
    let repacked = f.root.join("repacked");
    copy_tree(&moved, &repacked);
    assert_ne!(
        ino(&repacked.join("lib").join(SEAL_DRIVER)),
        ino(&repacked.join(format!("lib/rustlib/{SEAL_TRIPLE}/lib/{SEAL_DRIVER}"))),
        "the copy severed the link (the case a repack must repair)"
    );
    closure::prepare(&moved, &repacked, triple(), &seal_probe).expect("repack");
    assert_eq!(
        ino(&repacked.join("lib").join(SEAL_DRIVER)),
        ino(&repacked.join(format!("lib/rustlib/{SEAL_TRIPLE}/lib/{SEAL_DRIVER}")))
    );
}

#[test]
fn the_seal_and_its_private_closure_are_never_written() {
    let f = fx("closure-readonly");
    let snapshot = || -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        for d in [f.source.clone(), f.private.clone()] {
            for rel in atpkg_publish::artifact::relocate::walk_files(&d).expect("walk") {
                out.insert(d.join(&rel), std::fs::read(d.join(rel)).expect("read"));
            }
        }
        out
    };
    let before = snapshot();
    f.prepare().expect("prepared");
    assert_eq!(snapshot(), before);
    assert!(
        std::fs::symlink_metadata(f.source.join("lib/rustlib/rustc-src/rust"))
            .expect("alias")
            .file_type()
            .is_symlink()
    );
    assert!(
        !f.source
            .join(format!("lib/rustlib/{SEAL_TRIPLE}/lib/.rustc-commit-hash"))
            .exists()
    );
}

#[test]
fn no_other_private_directory_is_ever_taken_for_the_named_one() {
    let f = fx("closure-no-fallback");
    std::fs::rename(&f.private, f.private.with_file_name("plausible-other")).expect("rename");
    assert!(matches!(f.prepare(), Err(Refusal::NotRealDir(_))));
}

type Case = (&'static str, String, &'static [u8], fn(&Refusal) -> bool);

#[test]
fn a_private_closure_of_another_commit_driver_or_colliding_member_is_refused() {
    let cases: [Case; 3] = [
        (
            "commit",
            ".rustc-commit-hash".into(),
            b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            |r| *r == Refusal::PrivateCommit,
        ),
        (
            "driver",
            format!("{SEAL_TRIPLE}/{SEAL_DRIVER}"),
            b"same name, other build",
            |r| *r == Refusal::PrivateDriver,
        ),
        (
            "collision",
            "host/librustc_middle-fixture.rmeta".into(),
            b"other host metadata",
            |r| matches!(r, Refusal::Conflict(p) if p.ends_with("librustc_middle-fixture.rmeta")),
        ),
    ];
    for (name, path, bytes, want) in cases {
        let f = fx(&format!("closure-mismatch-{name}"));
        put(&f.private.join(&path), bytes);
        let got = f.prepare().expect_err(name);
        assert!(want(&got), "{name}: {got:?}");
    }
}

#[test]
fn a_build_stage_std_in_the_private_root_is_neither_required_nor_substituted() {
    let f = fx("closure-nested-std");
    let nested = f.private.join(format!("lib/rustlib/{SEAL_TRIPLE}/lib"));
    put(
        &nested.join("libstd-fixture.rlib"),
        b"different build-stage std",
    );
    put(
        &nested.join("libcore-other-stage.rlib"),
        b"different build-stage core",
    );
    f.prepare().expect("prepared");
    let target = f.stage.join(format!("lib/rustlib/{SEAL_TRIPLE}/lib"));
    assert_eq!(
        std::fs::read(target.join("libstd-fixture.rlib")).expect("std"),
        b"selected std\n"
    );
    assert!(!target.join("libcore-other-stage.rlib").exists());
    std::fs::remove_dir_all(&f.stage).expect("rm stage");
    std::fs::remove_dir_all(&nested).expect("rm nested");
    f.prepare().expect("prepared without it");
}

#[test]
fn missing_private_metadata_and_a_forged_fingerprint_are_refused() {
    let f = fx("closure-metadata");
    let metadata = f
        .private
        .join(SEAL_TRIPLE)
        .join("librustc_middle-fixture.rmeta");
    std::fs::remove_file(&metadata).expect("rm");
    assert!(matches!(
        f.prepare(),
        Err(Refusal::MissingSurface("rustc-private metadata", _))
    ));
    std::fs::remove_dir_all(&f.stage).expect("rm stage");
    put(&metadata, b"restored private metadata");
    let prov = f.source.join("PROVENANCE");
    let text = std::fs::read_to_string(&prov).expect("prov");
    std::fs::write(
        &prov,
        text.replace("artifact_driver_sha256=", "wrong_field="),
    )
    .expect("w");
    assert_eq!(f.prepare(), Err(Refusal::DriverFingerprint));
}

#[test]
fn a_linked_private_member_is_never_read_or_written_through() {
    let f = fx("closure-private-link");
    let outside = f.root.join("outside");
    put(&outside.join("member"), b"not closure data");
    std::os::unix::fs::symlink(&outside, f.private.join("host/escaped")).expect("link");
    assert!(matches!(f.prepare(), Err(Refusal::LinkInClosure(_))));
    assert_eq!(
        std::fs::read_dir(&outside).expect("outside").count(),
        1,
        "nothing written through the link"
    );
}

#[test]
fn a_linked_target_parent_is_never_written_through() {
    let f = fx("closure-target-link");
    let outside = f.root.join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    std::os::unix::fs::symlink(
        &outside,
        f.source
            .join(format!("lib/rustlib/{SEAL_TRIPLE}/lib/nested")),
    )
    .expect("link");
    put(
        &f.private.join("host/nested/deeper/member"),
        b"must not escape",
    );
    assert!(matches!(f.prepare(), Err(Refusal::NotRealDir(_))));
    assert_eq!(std::fs::read_dir(&outside).expect("outside").count(), 0);
}

#[test]
fn a_live_source_link_or_any_alias_but_the_sealed_one_is_refused() {
    let f = fx("closure-live-alias");
    let alias = f.source.join("lib/rustlib/rustc-src/rust");
    std::fs::remove_file(&alias).expect("rm");
    std::os::unix::fs::symlink(f.source.join("lib/rustlib/src/rust"), &alias).expect("link");
    assert!(matches!(f.prepare(), Err(Refusal::AliasNotSealed(_))));
}

#[test]
fn a_link_among_the_rust_src_riders_is_not_materialized() {
    let f = fx("closure-src-rider");
    let outside = f.root.join("outside-file");
    put(&outside, b"must not become source");
    std::os::unix::fs::symlink(&outside, f.source.join("lib/rustlib/src/rust/extra")).expect("l");
    assert!(matches!(f.prepare(), Err(Refusal::LinkInClosure(_))));
    assert!(
        std::fs::symlink_metadata(f.stage.join("lib/rustlib/rustc-src/rust"))
            .expect("alias")
            .file_type()
            .is_symlink(),
        "nothing was laid in its place"
    );
}

#[test]
fn a_traversal_in_provenance_cannot_select_an_unrelated_root() {
    let f = fx("closure-traversal");
    let prov = f.source.join("PROVENANCE");
    let text = std::fs::read_to_string(&prov).expect("prov");
    std::fs::write(
        &prov,
        text.replace(
            "private_sysroot=exact-fixture",
            "private_sysroot=../unrelated",
        ),
    )
    .expect("w");
    assert!(matches!(
        f.prepare(),
        Err(Refusal::PrivateSysrootName(n)) if n == "../unrelated"
    ));
}

#[test]
fn missing_compiler_sources_are_refused() {
    let f = fx("closure-sources");
    std::fs::remove_file(
        f.source
            .join("lib/rustlib/src/rust/compiler/rustc_driver/src/lib.rs"),
    )
    .expect("rm");
    assert_eq!(f.prepare(), Err(Refusal::EmptySource("compiler")));
}

#[test]
fn another_host_or_another_reported_commit_is_refused() {
    let f = fx("closure-identity");
    copy_tree(&f.source, &f.stage);
    let other = Triple::parse("x86_64-unknown-linux-gnu").expect("t");
    assert!(matches!(
        closure::prepare(&f.source, &f.stage, other, &seal_probe),
        Err(Refusal::CompilerIdentity(_))
    ));
    std::fs::write(f.source.join("PROVENANCE"), "reported_commit=wrong\n").expect("w");
    assert_eq!(
        closure::prepare(&f.source, &f.stage, triple(), &seal_probe),
        Err(Refusal::SealCommit)
    );
    assert_eq!(
        closure::prepare(&f.source, &f.source, triple(), &seal_probe),
        Err(Refusal::OverlappingRoots)
    );
}
