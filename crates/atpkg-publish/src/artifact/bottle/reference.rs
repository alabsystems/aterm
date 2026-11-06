// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Source-derived reference cohorts use the normal bottle gates and retain their rebuild kit.

use std::path::{Component, Path};

use aterm_uds::host::upstream::{ReferenceArtifact, ReferenceTool};

use super::{BottleInput, BottleRequest};
use crate::error::{Error, Result};
use crate::triple::{AARCH64_APPLE_DARWIN, Triple};

pub(crate) mod qualification;

pub(super) const SCOPE: &str = "Pinned Carcara independent reference\n\n\
Carcara's own source is Apache-2.0. This executable also statically links LGPL\n\
rug and GMP code through gmp-mpfr-sys. Original license texts, every registry source,\n\
the exact public source Git bundle and offline rebuild instructions are in\n\
share/source/carcara/corresponding-source.tar.zst. Accessible Apache, LGPL and\n\
GPL texts are in share/licenses/carcara.\n\n\
This unverified build does not establish checker soundness. The pinned checker\n\
has known evaluate, binder and ACI misjudgments. AY's pinned-Carcara guard is\n\
required before treating its verdict as independent comparison evidence.\n";

fn artifact(recipe: &ReferenceTool, triple: Triple) -> Result<&ReferenceArtifact> {
    let platform = match triple {
        AARCH64_APPLE_DARWIN => "macos-aarch64",
        crate::triple::X86_64_APPLE_DARWIN => "macos-x86_64",
        crate::triple::AARCH64_UNKNOWN_LINUX_GNU => "linux-aarch64",
        crate::triple::X86_64_UNKNOWN_LINUX_GNU => "linux-x86_64",
        _ => "unsupported",
    };
    recipe
        .artifacts
        .iter()
        .find(|artifact| artifact.platform == platform)
        .ok_or_else(|| {
            Error::cannot_run(format!(
                "{} has no qualified artifact for {triple}",
                recipe.name
            ))
        })
}

pub(super) fn witness(recipe: &ReferenceTool, artifact: &ReferenceArtifact) -> aterm_json::Value {
    aterm_json::json!({
        "schema": 1,
        "name": recipe.name,
        "version": recipe.version,
        "source": {
            "repository": recipe.repository,
            "revision": recipe.revision,
            "tree": recipe.source_tree,
            "cargo_lock_sha256": recipe.lock_sha256
        },
        "build": {
            "trust_build": artifact.trust_build,
            "compiler_revision": artifact.compiler,
            "compiler_query_sha256": artifact.compiler_query_sha256,
            "lane": "unverified",
            "profile": "release",
            "offline": true,
            "locked": true,
            "package": "carcara-cli",
            "bin": "carcara"
        },
        "artifact": { "platform": artifact.platform, "sha256": artifact.sha256 },
        "source_kit_sha256": recipe.source_kit_sha256,
        "scope": "independent-reference-with-AY-pinned-Carcara-guard"
    })
}

fn check_file(root: &Path, relative: &str, bytes: u64, sha256: &str) -> Result<()> {
    if relative.is_empty()
        || !Path::new(relative)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(Error::refused(
            "reference material has an unsafe relative path",
        ));
    }
    let data = crate::identity::read_regular_bytes(&root.join(relative), bytes)
        .map_err(|e| Error::io(format!("read reference material {relative}"), &e))?;
    if u64::try_from(data.len()).unwrap_or(u64::MAX) != bytes
        || atpkg::tree::sha256_bytes(&data) != sha256
    {
        return Err(Error::refused(format!(
            "reference material differs from its qualified cohort: {relative}"
        )));
    }
    Ok(())
}

/// Bind a prepared local build and complete corresponding source before relocation changes bytes.
pub(super) fn prepare(recipe: &ReferenceTool, request: &BottleRequest, stage: &Path) -> Result<()> {
    if request.program != recipe.name || request.version != recipe.version {
        return Err(Error::refused(
            "reference package differs from its source recipe",
        ));
    }
    if !matches!(request.input, BottleInput::InstallTree(_))
        || !request.dep_bottles.is_empty()
        || !request.donor_roots.is_empty()
    {
        return Err(Error::refused(
            "source-derived references require their qualified install tree without runtime donors",
        ));
    }
    let artifact = artifact(recipe, request.triple)?;
    check_file(
        stage,
        &format!("bin/{}", recipe.name),
        artifact.bytes,
        artifact.sha256,
    )?;
    check_file(
        stage,
        &format!("share/source/{}/corresponding-source.tar.zst", recipe.name),
        recipe.source_kit_bytes,
        recipe.source_kit_sha256,
    )?;
    check_file(
        stage,
        &format!("share/atpkg/{}-compiler-vv.txt", recipe.name),
        artifact.compiler_query_bytes,
        artifact.compiler_query_sha256,
    )?;
    for license in recipe.licenses {
        check_file(stage, license.path, license.bytes, license.sha256)?;
    }
    if crate::identity::read_regular_bytes(
        &stage.join("share/licenses/carcara/REFERENCE.txt"),
        4096,
    )
    .map_err(|e| Error::io("read reference scope", &e))?
        != SCOPE.as_bytes()
    {
        return Err(Error::refused(
            "reference scope and known limitations must be retained",
        ));
    }
    // This metadata belongs to the producer: derive it from the admitted cohort,
    // replacing any payload-supplied origin claims in the private copied stage.
    let origin = stage.join(format!("share/atpkg/{}-build.json", recipe.name));
    std::fs::create_dir_all(
        origin
            .parent()
            .ok_or_else(|| Error::refused("reference origin has no parent"))?,
    )
    .map_err(|e| Error::io("create reference origin directory", &e))?;
    // Generic symlink materialization may have made this leaf a hardlink to an
    // admitted runtime or source file. Replace the leaf, never truncate its inode.
    match std::fs::remove_file(&origin) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::io("replace reference build witness", &error)),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut file = options
        .open(&origin)
        .map_err(|e| Error::io("create reference build witness", &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))
            .map_err(|e| Error::io("set reference build witness mode", &e))?;
    }
    use std::io::Write as _;
    file.write_all(
        &aterm_json::to_vec(&witness(recipe, artifact))
            .map_err(|e| Error::refused(format!("encode reference build witness: {e}")))?,
    )
    .map_err(|e| Error::io("write reference build witness", &e))?;
    Ok(())
}

type Runner<'a> = dyn Fn(&Path, &[&str], &Path) -> Result<std::process::Output> + 'a;

fn native_plan(request: &BottleRequest, host: Option<Triple>) -> Result<bool> {
    if host == Some(request.triple) {
        return Ok(true);
    }
    if request.signing == crate::api::Signing::Unsigned && !request.upload {
        return Ok(false);
    }
    Err(Error::cannot_run(
        "a foreign reference target may only be prepared unsigned; qualify its final archive natively before separate signing and publication",
    ))
}

fn native_host() -> Option<Triple> {
    (aterm_uds::process::translation() == aterm_uds::process::Translation::Native)
        .then(Triple::host)
        .flatten()
}

pub(super) fn check_request(request: &BottleRequest) -> Result<()> {
    native_plan(request, native_host()).map(|_| ())
}

fn probe(executable: &Path, args: &[&str], cwd: &Path) -> Result<std::process::Output> {
    let mut command = aterm_uds::spawnfd::command(executable);
    command
        .env_clear()
        .env("HOME", cwd)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(cwd)
        .args(args);
    aterm_uds::deadline::output_until(
        &mut command,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .map_err(|e| Error::cannot_run(format!("reference native qualification: {e}")))
}

pub(super) fn qualify(
    recipe: &ReferenceTool,
    request: &BottleRequest,
    tree: &Path,
) -> Result<Option<usize>> {
    if !native_plan(request, native_host())? {
        eprintln!(
            "atpkg-publish: prepared unsigned reference {} for {}; final archive still requires native runtime qualification before separate signing and publication",
            recipe.name, request.triple
        );
        return Ok(None);
    }
    let before =
        atpkg::tree::tree_root(tree).map_err(|e| Error::io("hash reference runtime", &e))?;
    let work = crate::channel::Scratch::new("reference-probe")?;
    let checks = qualify_with(
        recipe,
        &tree.join("bin").join(recipe.name),
        work.path(),
        &probe,
    )?;
    if atpkg::tree::tree_root(tree).map_err(|e| Error::io("rehash reference runtime", &e))?
        != before
    {
        return Err(Error::refused(
            "reference qualification changed the packaged tree",
        ));
    }
    eprintln!(
        "atpkg-publish: reference native qualification OK — {} {checks} controls; package tree unchanged",
        recipe.name
    );
    Ok(Some(checks))
}

fn qualify_with(
    recipe: &ReferenceTool,
    executable: &Path,
    cwd: &Path,
    run: &Runner<'_>,
) -> Result<usize> {
    if recipe.name != "carcara" {
        return Err(Error::refused(
            "source-derived reference has no runtime qualifier",
        ));
    }
    let short = recipe
        .revision
        .get(..7)
        .ok_or_else(|| Error::refused("reference revision has no short identity"))?;
    let version = run(executable, &["--version"], cwd)?;
    if !version.status.success()
        || String::from_utf8_lossy(&version.stdout).trim()
            != format!("carcara {} [git none {short}]", recipe.version)
    {
        return Err(Error::refused(
            "reference executable version differs from its source revision",
        ));
    }
    const PROBLEM: &str =
        "(set-logic QF_UF)\n(declare-const p Bool)\n(assert p)\n(assert (not p))\n(check-sat)\n";
    const HONEST: &[u8] =
        b"(assume a0 p)\n(assume a1 (not p))\n(step t0 (cl) :rule resolution :premises (a0 a1))\n";
    let problem = cwd.join("problem.smt2");
    std::fs::write(&problem, PROBLEM).map_err(|e| Error::io("write reference problem", &e))?;
    let cases: &[(&str, Option<&[u8]>, bool)] = &[
        ("honest", Some(HONEST), true),
        (
            "false-resolution",
            Some(b"(assume a0 p)\n(step t0 (cl) :rule resolution :premises (a0))\n"),
            false,
        ),
        ("wrong-problem", Some(b"(assume a0 q)\n"), false),
        (
            "truncated",
            Some(b"(step t0 (cl) :rule resolution :premises ("),
            false,
        ),
        ("invalid-utf8", Some(&[0xff]), false),
        ("missing", None, false),
    ];
    for (name, proof, valid) in cases {
        let path = cwd.join(format!("{name}.alethe"));
        if let Some(proof) = proof {
            std::fs::write(&path, proof).map_err(|e| Error::io("write reference proof", &e))?;
        }
        let answer = run(
            executable,
            &[
                "check",
                "--no-color",
                "--allow-int-real-subtyping",
                path.to_str()
                    .ok_or_else(|| Error::refused("non-UTF-8 reference scratch path"))?,
                problem
                    .to_str()
                    .ok_or_else(|| Error::refused("non-UTF-8 reference scratch path"))?,
            ],
            cwd,
        )?;
        let expected = if *valid {
            (Some(0), "valid")
        } else {
            (Some(1), "invalid")
        };
        if (
            answer.status.code(),
            String::from_utf8_lossy(&answer.stdout).trim(),
        ) != expected
        {
            return Err(Error::refused(format!(
                "reference {name} control failed: {}{}",
                String::from_utf8_lossy(&answer.stdout),
                String::from_utf8_lossy(&answer.stderr)
            )));
        }
    }
    Ok(cases.len() + 1)
}

#[cfg(test)]
mod tests {
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
        assert!(
            super::super::bottle_row("carcara", crate::triple::AARCH64_UNKNOWN_LINUX_GNU).is_err()
        );
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
}
