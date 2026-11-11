// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::process::ExitStatusExt;
use std::sync::Arc;

use super::*;
use crate::artifact::bottle::tests::{FakeBackend, TarEntry, build_tar, thin_macho_bytes};
use crate::artifact::tarwrite::RawFrames;
use crate::host::fake::FakeHost;
use crate::testkit::Keys;
use crate::triple::{AARCH64_APPLE_DARWIN, X86_64_APPLE_DARWIN};

struct Fixture {
    work: aterm_tempfile::TempDir,
    archive: PathBuf,
    recipe: PinnedTool,
    request: upstream::Request,
}

impl Fixture {
    fn new(entries: &[TarEntry<'_>]) -> Self {
        let work = aterm_tempfile::tempdir().expect("scratch");
        let archive = work.path().join("fixture.tar");
        build_tar(&archive, entries);
        let bytes = std::fs::read(&archive).expect("fixture bytes");
        let hash = Box::leak(atpkg::abrew::fetch::sha256_bytes(&bytes).into_boxed_str());
        let archives = Box::leak(Box::new([Archive {
            platform: "macos-aarch64",
            url: "https://example.invalid/gitleaks_{v}.tar",
            sha256: hash,
            bytes: bytes.len() as u64,
        }]));
        Self {
            request: upstream::Request {
                program: "gitleaks".into(),
                version: "8.30.1".into(),
                build: 1,
                triple: AARCH64_APPLE_DARWIN,
                merge_from: None,
                out: work.path().join("out"),
            },
            recipe: PinnedTool {
                name: "gitleaks",
                version: "8.30.1",
                executable: "gitleaks",
                layout: Layout::Executable,
                build: Build::Prebuilt,
                strip: 0,
                max_extract_bytes: super::super::MAX_BOTTLE_EXTRACT_BYTES,
                licenses: &["LICENSE"],
                shim_env: &[],
                darwin_omit: &[],
                hosts: &["macos-aarch64"],
                archives,
            },
            archive,
            work,
        }
    }

    fn prepare(&self) -> Result<PathBuf> {
        prepare(
            &self.request,
            &self.recipe,
            self.work.path(),
            &|url, destination, cap| {
                assert_eq!(url, self.recipe.archives[0].url_at(self.recipe.version));
                assert_eq!(cap, self.recipe.archives[0].bytes);
                std::fs::copy(&self.archive, destination)
                    .map_err(|error| Error::io("copy fixture", &error))?;
                Ok(())
            },
        )
    }

    fn pack_and_stage(&self, tree: PathBuf) -> (BottleOutcome, PathBuf) {
        let ctx = Keys::mint("m3", &[]).ctx(self.work.path(), Arc::new(FakeHost::new()));
        let request = BottleRequest {
            program: self.request.program.clone(),
            input: BottleInput::InstallTree(tree),
            version: self.request.version.clone(),
            build: 1,
            triple: self.request.triple,
            strip_components: 0,
            dep_bottles: vec![],
            donor_roots: vec![],
            merge_from: None,
            key: None,
            out: self.request.out.clone(),
            signing: Signing::Unsigned,
            upload: false,
        };
        let package = super::super::pack_bottle_with(
            &ctx,
            &request,
            &FakeBackend::default(),
            &RawFrames::WHOLE,
        )
        .expect("package");
        let manifest = crate::lane::rows::local(&package.manifest).expect("manifest");
        let row = manifest
            .artifact_for(request.triple.as_str())
            .expect("target");
        let moved = self.work.path().join("client store with spaces");
        atpkg::verify_for_publication(
            row,
            &package.asset,
            &moved,
            &atpkg::install::StageHooks::NONE,
        )
        .expect("client accepts relocated package");
        (package, moved)
    }
}

#[test]
fn pinned_archive_becomes_unsigned_client_package_with_license_and_provenance() {
    let executable = thin_macho_bytes(0x0100_000c);
    let fixture = Fixture::new(&[
        TarEntry::File("gitleaks", &executable, 0o755),
        TarEntry::File("LICENSE", b"MIT license fixture\n", 0o644),
        TarEntry::File("README.md", b"not installed", 0o644),
    ]);
    let tree = fixture.prepare().expect("pinned tree");
    assert!(!tree.join("README.md").exists());
    let (package, moved) = fixture.pack_and_stage(tree);
    assert!(package.signature.is_none());
    assert!(!package.uploaded);
    assert_eq!(
        std::fs::read(moved.join("bin/gitleaks")).expect("program"),
        executable
    );
    assert_eq!(
        std::fs::read(moved.join("share/licenses/gitleaks/LICENSE")).expect("license"),
        b"MIT license fixture\n"
    );
    let origin: aterm_json::Value = aterm_json::from_slice(
        &std::fs::read(moved.join("share/atpkg/upstream.json")).expect("provenance"),
    )
    .expect("origin JSON");
    assert_eq!(
        origin["sha256"].as_str(),
        Some(fixture.recipe.archives[0].sha256)
    );
    assert_eq!(
        origin["target"].as_str(),
        Some(fixture.request.triple.as_str())
    );
}

#[test]
fn a_versioned_node_exposure_keeps_the_literal_runtime_name_inside_the_package() {
    let executable = thin_macho_bytes(0x0100_000c);
    let pin = aterm_uds::host::upstream::NODE;
    let mut fixture = Fixture::new(&[
        TarEntry::File("bin/node", &executable, 0o755),
        TarEntry::File("LICENSE", b"complete license fixture\n", 0o644),
    ]);
    fixture.request.program = pin.name.into();
    fixture.request.version = pin.version.into();
    fixture.recipe = PinnedTool {
        archives: fixture.recipe.archives,
        strip: 0,
        ..pin
    };
    let tree = fixture.prepare().expect("versioned Node tree");
    assert_eq!(
        std::fs::read_link(tree.join("bin/node22")).unwrap(),
        Path::new("node")
    );
    let (package, moved) = fixture.pack_and_stage(tree);
    assert_eq!(
        crate::lane::rows::local(&package.manifest).unwrap().exposes,
        ["node22"]
    );
    assert_eq!(std::fs::read(moved.join("bin/node")).unwrap(), executable);
    assert_eq!(std::fs::read(moved.join("bin/node22")).unwrap(), executable);
    use std::os::unix::fs::MetadataExt as _;
    let primary = std::fs::symlink_metadata(moved.join("bin/node")).unwrap();
    let alias = std::fs::symlink_metadata(moved.join("bin/node22")).unwrap();
    assert!(primary.is_file() && alias.is_file());
    assert_eq!((primary.dev(), primary.ino()), (alias.dev(), alias.ino()));
    assert!(!atpkg::store::shim_allowed("node"));
    assert!(atpkg::store::shim_allowed("node22"));
}

#[test]
fn cpython_archive_preserves_the_runtime_and_interpreter_aliases() {
    let executable = thin_macho_bytes(0x0100_000c);
    let pin = aterm_uds::host::upstream::CPYTHON;
    let omitted: Vec<_> = pin
        .darwin_omit
        .iter()
        .map(|path| format!("python/{path}"))
        .collect();
    let mut entries = vec![
        TarEntry::File("python/bin/python3.14", &executable, 0o755),
        TarEntry::Symlink("python/bin/python3", "python3.14"),
        TarEntry::Symlink("python/bin/python", "python3.14"),
        TarEntry::File("python/lib/python3.14/LICENSE.txt", b"PSF license\n", 0o644),
        TarEntry::File(
            "python/lib/python3.14/tomllib/__init__.py",
            b"# stdlib\n",
            0o644,
        ),
        TarEntry::File(
            "python/lib/python3.14/lib-dynload/_ssl.so",
            &executable,
            0o755,
        ),
    ];
    entries.extend(
        omitted
            .iter()
            .map(|path| TarEntry::File(path, b"Windows-only launcher", 0o644)),
    );
    let mut fixture = Fixture::new(&entries);
    fixture.request.program = pin.name.into();
    fixture.request.version = pin.version.into();
    fixture.recipe = PinnedTool {
        archives: fixture.recipe.archives,
        ..pin
    };
    let tree = fixture.prepare().expect("Python runtime tree");
    let (_, moved) = fixture.pack_and_stage(tree);
    for name in ["python3.14", "python3", "python"] {
        assert_eq!(
            std::fs::read(moved.join("bin").join(name)).unwrap(),
            executable
        );
    }
    assert_eq!(
        std::fs::read(moved.join("lib/python3.14/tomllib/__init__.py")).unwrap(),
        b"# stdlib\n"
    );
    assert_eq!(
        std::fs::read(moved.join("lib/python3.14/lib-dynload/_ssl.so")).unwrap(),
        executable
    );
    assert_eq!(
        std::fs::read(moved.join("lib/python3.14/LICENSE.txt")).unwrap(),
        b"PSF license\n"
    );
    let origin: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(moved.join("share/atpkg/upstream.json")).unwrap())
            .unwrap();
    assert_eq!(origin["version"].as_str(), Some(pin.version));
    assert_eq!(
        origin["sha256"].as_str(),
        Some(fixture.recipe.archives[0].sha256)
    );
    assert_eq!(
        origin.get("omitted"),
        Some(&aterm_json::json!(pin.darwin_omit))
    );
    for name in pin.darwin_omit {
        assert!(!moved.join(name).exists());
    }
}

#[test]
fn lean_archive_keeps_its_compiler_std_and_licenses_without_a_lean_shim() {
    let exe = thin_macho_bytes(0x0100_000c);
    let pin = aterm_uds::host::upstream::LEAN4;
    let mut fixture = Fixture::new(&[
        TarEntry::File("lean/bin/lake", &exe, 0o755),
        TarEntry::File("lean/bin/lean", &exe, 0o755),
        TarEntry::File("lean/lib/lean/Std/Tactic/BVDecide.olean", b"Std", 0o644),
        TarEntry::File("lean/lib/lean/libleanrt.a", b"runtime", 0o644),
        TarEntry::File("lean/LICENSE", b"Apache", 0o644),
        TarEntry::File("lean/LICENSES", b"runtime licenses", 0o644),
    ]);
    fixture.request.program = pin.name.into();
    fixture.request.version = pin.version.into();
    fixture.recipe = PinnedTool {
        archives: fixture.recipe.archives,
        ..pin
    };
    let tree = fixture.prepare().unwrap();
    let (out, moved) = fixture.pack_and_stage(tree);
    assert_eq!(
        crate::lane::rows::local(&out.manifest).unwrap().exposes,
        ["lake"]
    );
    assert_eq!(std::fs::read(moved.join("bin/lean")).unwrap(), exe);
    assert_eq!(
        std::fs::read(moved.join("lib/lean/Std/Tactic/BVDecide.olean")).unwrap(),
        b"Std"
    );
    assert_eq!(
        std::fs::read(moved.join("lib/lean/libleanrt.a")).unwrap(),
        b"runtime"
    );
    for name in pin.licenses {
        assert!(moved.join(name).is_file());
        assert!(moved.join("share/licenses/lean4").join(name).is_file());
    }
    assert!(can_pack(&pin, X86_64_APPLE_DARWIN).is_err());
}

#[test]
fn each_pinned_recipe_keeps_its_own_extraction_bound() {
    let exe = thin_macho_bytes(0x0100_000c);
    let mut fixture = Fixture::new(&[
        TarEntry::File("gitleaks", &exe, 0o755),
        TarEntry::File("LICENSE", b"license", 0o644),
    ]);
    fixture.recipe.max_extract_bytes = 1;
    assert!(
        fixture
            .prepare()
            .unwrap_err()
            .to_string()
            .contains("unpack pinned upstream archive")
    );
    for pin in [
        aterm_uds::host::upstream::GITLEAKS,
        aterm_uds::host::upstream::CPYTHON,
        aterm_uds::host::upstream::OPENSSL_SIGNING,
    ] {
        assert_eq!(
            pin.max_extract_bytes,
            super::super::MAX_BOTTLE_EXTRACT_BYTES
        );
    }
}

#[test]
fn cpython_probe_rejects_wrong_versions_external_paths_and_no_tls_roots() {
    let prefix = Path::new("/fixture/runtime");
    let executable = prefix.join("bin/python3.14");
    let good = aterm_json::json!({
        "version": "3.14.8", "prefix": prefix, "base_prefix": prefix,
        "executable": executable, "stdlib": "/fixture/runtime/lib/python3.14",
        "ssl_ca_count": 1,
    });
    let version = aterm_uds::host::upstream::CPYTHON.version;
    validate_python_probe(&good, version, prefix, prefix, &executable).unwrap();
    for (field, value) in [
        ("version", aterm_json::json!("3.9.0")),
        ("prefix", aterm_json::json!("/outside")),
        ("base_prefix", aterm_json::json!("/outside")),
        ("executable", aterm_json::json!("/usr/bin/python3")),
        ("stdlib", aterm_json::json!("/fixture/runtime/../outside")),
        ("ssl_ca_count", aterm_json::json!(0)),
    ] {
        let mut bad = good.clone();
        bad.as_object_mut().unwrap().insert(field.into(), value);
        assert!(
            validate_python_probe(&bad, version, prefix, prefix, &executable).is_err(),
            "{field}"
        );
    }
}

#[test]
fn cpython_precompile_report_requires_the_pinned_version_and_hash_transform() {
    let good = aterm_json::json!({
        "version": "3.14.8", "implementation": "cpython",
        "invalidation": "checked-hash", "filenames": "relative-to-runtime-root",
        "optimize": [0, 1, 2], "hash_seed": 0, "pyc_files": 3, "source_files": 1,
        "compiler_sha256": "a".repeat(64), "source_tree_sha256": "b".repeat(64),
        "magic_number": "01020304",
    });
    validate_precompile_report(&good, "3.14.8").unwrap();
    for (field, value) in [
        ("version", aterm_json::json!("3.13.0")),
        ("invalidation", aterm_json::json!("timestamp")),
        ("hash_seed", aterm_json::json!(1)),
        ("source_tree_sha256", aterm_json::json!("missing")),
    ] {
        let mut bad = good.clone();
        bad.as_object_mut().unwrap().insert(field.into(), value);
        assert!(
            validate_precompile_report(&bad, "3.14.8").is_err(),
            "{field}"
        );
    }
}

#[test]
fn corrupt_size_or_hash_refuses_before_extraction() {
    for wrong_size in [false, true] {
        let mut fixture = Fixture::new(&[]);
        let mut archive = fixture.recipe.archives[0];
        if wrong_size {
            archive.bytes += 1;
        } else {
            archive.sha256 = "bad hash";
        }
        fixture.recipe.archives = Box::leak(Box::new([archive]));
        let error = fixture
            .prepare()
            .expect_err("integrity refusal")
            .to_string();
        assert!(
            error.contains(if wrong_size { "bytes" } else { "SHA-256" }),
            "{error}"
        );
        assert!(!fixture.work.path().join("unpacked").exists());
    }
}

#[test]
fn an_unsupported_pinned_container_refuses_before_download() {
    let mut fixture = Fixture::new(&[]);
    fixture.recipe.archives = Box::leak(Box::new([Archive {
        url: "https://example.invalid/gitleaks.tar.xz",
        ..fixture.recipe.archives[0]
    }]));
    assert!(
        can_pack(&fixture.recipe, fixture.request.triple)
            .unwrap_err()
            .to_string()
            .contains("unsupported bottle archive extension")
    );
    let error = prepare(
        &fixture.request,
        &fixture.recipe,
        fixture.work.path(),
        &|_, _, _| panic!("an unsupported container must not download"),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported bottle archive extension")
    );
    assert!(!fixture.work.path().join("unpacked").exists());
}

#[test]
fn wrong_pin_or_unavailable_target_refuses_before_download() {
    for wrong_version in [false, true] {
        let mut fixture = Fixture::new(&[]);
        if wrong_version {
            fixture.request.version = "0.0.0".into();
        } else {
            fixture.request.triple = X86_64_APPLE_DARWIN;
        }
        prepare(
            &fixture.request,
            &fixture.recipe,
            fixture.work.path(),
            &|_, _, _| panic!("invalid request must not download"),
        )
        .expect_err("invalid request");
    }
}

#[test]
fn target_license_permissions_and_regular_program_are_required() {
    let arm = thin_macho_bytes(0x0100_000c);
    let x64 = thin_macho_bytes(0x0100_0007);
    for (entries, expected) in [
        (vec![TarEntry::File("gitleaks", &arm, 0o755)], "LICENSE"),
        (
            vec![TarEntry::File("gitleaks", &arm, 0o644)],
            "execute permission",
        ),
        (
            vec![TarEntry::File("gitleaks", &x64, 0o755)],
            "ARCHITECTURE",
        ),
        (
            vec![
                TarEntry::File("actual", &arm, 0o755),
                TarEntry::Symlink("gitleaks", "actual"),
            ],
            "regular",
        ),
    ] {
        let fixture = Fixture::new(&entries);
        let error = fixture.prepare().expect_err("layout refusal").to_string();
        assert!(error.contains(expected), "expected {expected}: {error}");
    }
}

#[test]
fn qualification_requires_the_version_clean_scan_and_reported_seeded_finding() {
    for failure in [
        None,
        Some("version"),
        Some("clean"),
        Some("detect"),
        Some("report"),
    ] {
        let work = aterm_tempfile::tempdir().expect("scratch");
        let result = qualify_tree(
            Path::new("gitleaks"),
            "8.30.1",
            work.path(),
            &|_, args, cwd| {
                let mut code = 0;
                let mut stdout = Vec::new();
                if args == ["version"] {
                    stdout.extend_from_slice(if failure == Some("version") {
                        b"0.0.0"
                    } else {
                        b"8.30.1"
                    });
                } else {
                    let text =
                        std::fs::read_to_string(cwd.join("scan input/fixture.txt")).expect("input");
                    if text.contains("ALAB_SEEDED") {
                        code = if failure == Some("detect") { 0 } else { 17 };
                        std::fs::write(
                            cwd.join("report.json"),
                            if failure == Some("report") {
                                "[]"
                            } else {
                                "[{\"RuleID\":\"qualification\"}]"
                            },
                        )
                        .expect("report");
                    } else if failure == Some("clean") {
                        code = 17;
                    }
                }
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(code << 8),
                    stdout,
                    stderr: Vec::new(),
                })
            },
        );
        assert_eq!(
            result.is_ok(),
            failure.is_none(),
            "qualification case {failure:?}: {result:?}"
        );
    }
}
