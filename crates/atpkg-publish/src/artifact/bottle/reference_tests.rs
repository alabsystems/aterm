// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::api::Signing;
use aterm_uds::host::upstream::{CARCARA, ReferenceLicense};

struct Fixture {
    work: aterm_tempfile::TempDir,
    recipe: ReferenceTool,
    request: BottleRequest,
}

impl Fixture {
    fn new() -> Self {
        let work = aterm_tempfile::tempdir().unwrap();
        let binary = b"qualified fixture binary";
        let kit = b"complete fixture source kit";
        let license = b"original fixture license";
        let digest = |bytes: &[u8]| -> &'static str {
            Box::leak(atpkg::tree::sha256_bytes(bytes).into_boxed_str())
        };
        let recipe = ReferenceTool {
            artifacts: Box::leak(Box::new([ReferenceArtifact {
                platform: "macos-aarch64",
                sha256: digest(binary),
                bytes: binary.len() as u64,
                compiler_query_sha256: digest(b"fixture compiler query"),
                compiler_query_bytes: 22,
                ..CARCARA.artifacts[0]
            }])),
            source_kit_sha256: digest(kit),
            source_kit_bytes: kit.len() as u64,
            licenses: Box::leak(Box::new([ReferenceLicense {
                path: "share/licenses/carcara/LICENSE.txt",
                sha256: digest(license),
                bytes: license.len() as u64,
            }])),
            ..CARCARA
        };
        for (name, data) in [
            ("bin/carcara", binary.as_slice()),
            (
                "share/atpkg/carcara-compiler-vv.txt",
                b"fixture compiler query".as_slice(),
            ),
            (
                "share/source/carcara/corresponding-source.tar.zst",
                kit.as_slice(),
            ),
            ("share/licenses/carcara/LICENSE.txt", license.as_slice()),
            ("share/licenses/carcara/REFERENCE.txt", SCOPE.as_bytes()),
        ] {
            let path = work.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        let origin = work.path().join("share/atpkg/carcara-build.json");
        std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
        std::fs::write(
            origin,
            aterm_json::to_vec(&witness(&recipe, &recipe.artifacts[0])).unwrap(),
        )
        .unwrap();
        let request = BottleRequest {
            program: "carcara".into(),
            input: BottleInput::InstallTree(work.path().into()),
            version: "1.1.0".into(),
            build: 1,
            triple: AARCH64_APPLE_DARWIN,
            strip_components: 0,
            dep_bottles: vec![],
            donor_roots: vec![],
            merge_from: None,
            key: None,
            out: work.path().join("out"),
            signing: Signing::Unsigned,
            upload: false,
        };
        Self {
            work,
            recipe,
            request,
        }
    }
}

#[test]
fn source_derived_cohort_requires_exact_runtime_source_and_license_bytes() {
    for path in [
        "bin/carcara",
        "share/atpkg/carcara-compiler-vv.txt",
        "share/source/carcara/corresponding-source.tar.zst",
        "share/licenses/carcara/LICENSE.txt",
        "share/licenses/carcara/REFERENCE.txt",
    ] {
        let f = Fixture::new();
        prepare(&f.recipe, &f.request, f.work.path()).unwrap();
        let path = f.work.path().join(path);
        let original = std::fs::read(&path).unwrap();
        let mut changed = original.clone();
        changed[0] ^= 1;
        std::fs::write(&path, changed).unwrap();
        assert!(prepare(&f.recipe, &f.request, f.work.path()).is_err());
        std::fs::write(&path, &original).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(prepare(&f.recipe, &f.request, f.work.path()).is_err());
    }
}

#[test]
fn payload_metadata_cannot_replace_the_admitted_build_origin() {
    let f = Fixture::new();
    let path = f.work.path().join("share/atpkg/carcara-build.json");
    std::fs::write(
        &path,
        b"untrusted payload claims a different compiler and source",
    )
    .unwrap();
    prepare(&f.recipe, &f.request, f.work.path()).unwrap();
    let doc: aterm_json::Value = aterm_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(doc, witness(&f.recipe, &f.recipe.artifacts[0]));
}

#[test]
fn replacing_a_payload_origin_hardlink_preserves_the_admitted_runtime_and_source() {
    for target in [
        "bin/carcara",
        "share/source/carcara/corresponding-source.tar.zst",
    ] {
        let f = Fixture::new();
        let origin = f.work.path().join("share/atpkg/carcara-build.json");
        let target = f.work.path().join(target);
        let bytes = std::fs::read(&target).unwrap();
        std::fs::remove_file(&origin).unwrap();
        std::fs::hard_link(&target, &origin).unwrap();
        prepare(&f.recipe, &f.request, f.work.path()).unwrap();
        assert_eq!(std::fs::read(target).unwrap(), bytes);
        let doc: aterm_json::Value =
            aterm_json::from_slice(&std::fs::read(origin).unwrap()).unwrap();
        assert_eq!(doc, witness(&f.recipe, &f.recipe.artifacts[0]));
    }
}

#[test]
fn a_reference_recipe_does_not_admit_archives_donors_or_unqualified_targets() {
    let mut f = Fixture::new();
    f.request.input = BottleInput::Archive(f.work.path().join("archive.tar"));
    assert!(prepare(&f.recipe, &f.request, f.work.path()).is_err());
    f.request.input = BottleInput::InstallTree(f.work.path().into());
    f.request.donor_roots.push(f.work.path().into());
    assert!(prepare(&f.recipe, &f.request, f.work.path()).is_err());
    f.request.donor_roots.clear();
    f.request.triple = crate::triple::X86_64_APPLE_DARWIN;
    assert!(prepare(&f.recipe, &f.request, f.work.path()).is_err());
    f.request.triple = AARCH64_APPLE_DARWIN;
    f.request.version = "1.1.1".into();
    assert!(prepare(&f.recipe, &f.request, f.work.path()).is_err());
    assert!(super::super::upstream::can_build("carcara", AARCH64_APPLE_DARWIN).is_err());
    assert!(super::super::bottle_row("carcara", crate::triple::AARCH64_UNKNOWN_LINUX_GNU).is_err());
}

#[test]
fn foreign_reference_assembly_is_unsigned_and_never_a_native_qualification() {
    let mut f = Fixture::new();
    assert!(native_plan(&f.request, Some(AARCH64_APPLE_DARWIN)).unwrap());
    assert!(!native_plan(&f.request, Some(crate::triple::X86_64_APPLE_DARWIN)).unwrap());
    assert!(!native_plan(&f.request, None).unwrap());
    f.request.signing = Signing::MachineKey;
    assert!(native_plan(&f.request, Some(crate::triple::X86_64_APPLE_DARWIN)).is_err());
    assert!(native_plan(&f.request, Some(AARCH64_APPLE_DARWIN)).unwrap());
    f.request.signing = Signing::Unsigned;
    f.request.upload = true;
    assert!(native_plan(&f.request, Some(crate::triple::X86_64_APPLE_DARWIN)).is_err());
}

#[cfg(unix)]
#[test]
fn reference_trees_keep_the_existing_link_containment_and_relative_material_checks() {
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new();
    let path = f.work.path().join("bin/carcara");
    let other = f.work.path().join("other");
    std::fs::rename(&path, &other).unwrap();
    symlink("../other", &path).unwrap();
    super::super::materialize_symlinks(f.work.path()).unwrap();
    prepare(&f.recipe, &f.request, f.work.path()).unwrap();
    assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
    std::fs::remove_file(&path).unwrap();
    let outside = aterm_tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("binary");
    std::fs::copy(&other, &outside_file).unwrap();
    symlink(&outside_file, &path).unwrap();
    assert!(super::super::materialize_symlinks(f.work.path()).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&other, &path).unwrap();
    f.recipe.licenses = &[ReferenceLicense {
        path: "../outside",
        sha256: "",
        bytes: 1,
    }];
    assert!(prepare(&f.recipe, &f.request, f.work.path()).is_err());
}

#[cfg(unix)]
#[test]
fn runtime_qualification_requires_exact_version_honest_acceptance_and_normal_negatives() {
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt;
    for fault in [
        "none",
        "version",
        "honest",
        "false-resolution",
        "wrong-problem",
        "truncated",
        "invalid-utf8",
        "missing",
        "crash",
    ] {
        let work = aterm_tempfile::tempdir().unwrap();
        let calls = Cell::new(0);
        let executable = Path::new("/relocated package/bin/carcara");
        let result = qualify_with(&CARCARA, executable, work.path(), &|program, args, cwd| {
            assert_eq!(program, executable);
            assert_eq!(cwd, work.path());
            calls.set(calls.get() + 1);
            let (code, text) = if args == ["--version"] {
                (
                    0,
                    if fault == "version" {
                        "carcara 1.1.0 [git none wrong]"
                    } else {
                        "carcara 1.1.0 [git none 9a352ee]"
                    },
                )
            } else {
                assert_eq!(
                    &args[..3],
                    &["check", "--no-color", "--allow-int-real-subtyping"]
                );
                assert_eq!(
                    std::fs::read_to_string(args[4]).unwrap(),
                    "(set-logic QF_UF)\n(declare-const p Bool)\n(assert p)\n(assert (not p))\n(check-sat)\n"
                );
                let name = Path::new(args[3]).file_stem().unwrap().to_str().unwrap();
                assert_eq!(Path::new(args[3]).is_file(), name != "missing");
                let valid = name == "honest";
                if fault == name {
                    (
                        if valid { 1 } else { 0 },
                        if valid { "invalid" } else { "valid" },
                    )
                } else {
                    (
                        if valid { 0 } else { 1 },
                        if valid { "valid" } else { "invalid" },
                    )
                }
            };
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(if fault == "crash" {
                    9
                } else {
                    code << 8
                }),
                stdout: text.as_bytes().into(),
                stderr: vec![],
            })
        });
        assert_eq!(result.is_ok(), fault == "none", "{fault}");
        if fault == "none" {
            assert_eq!(calls.get(), 7);
        }
    }
}
