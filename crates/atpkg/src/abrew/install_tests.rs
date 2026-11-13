// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(unix)]
use crate::extract::fixtures::{gzip_bytes, tar_bytes};

#[test]
fn dependents_read_receipt_closures_not_metadata_versions() {
    // Uninstall protection reads each keg's recorded tab
    // closure: a multi-version neighbour keg (whose
    // versions current metadata cannot all cover) must
    // not make every uninstall refuse, and protection
    // follows the tab even where metadata would differ.
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let keg = |name: &str, version: &str, closure: &str| {
        let dir = prefix.keg(name, version);
        std::fs::create_dir_all(&dir).unwrap();
        prefix
            .write_receipt_raw(
                &dir,
                &[
                    ("installed_by", "abrew"),
                    ("name", name),
                    ("version", version),
                ],
                &[("runtime_dependencies", closure)],
            )
            .unwrap();
    };
    keg("app", "1", r#"[{"full_name":"dep","version":"1"}]"#);
    keg("dep", "1", "[]");
    keg("multi", "1", "[]");
    keg("multi", "2", "[]");
    let api = Api::new(prefix.clone());
    assert_eq!(dependents(&api, &prefix, "dep").unwrap(), ["app"]);
    assert!(dependents(&api, &prefix, "multi").unwrap().is_empty());
    assert!(dependents(&api, &prefix, "app").unwrap().is_empty());
}

#[test]
#[cfg(unix)]
fn prefix_symlinks_are_relativized_and_strays_still_delegate() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let staged = tmp.path().join("staged");
    std::fs::create_dir_all(staged.join("lib")).unwrap();
    symlink_file(
        Path::new("@@HOMEBREW_CELLAR@@/demo/1/lib/libdemo.dylib"),
        &staged.join("lib/libdemo.1.dylib"),
    )
    .unwrap();
    symlink_file(
        Path::new("/opt/homebrew/opt/helper/bin/helper"),
        &staged.join("lib/helper"),
    )
    .unwrap();
    symlink_file(
        Path::new("@@HOMEBREW_JAVA@@/bin/java"),
        &staged.join("lib/java"),
    )
    .unwrap();
    symlink_file(
        Path::new("/usr/lib/libSystem.B.dylib"),
        &staged.join("lib/sys"),
    )
    .unwrap();
    let f = Formula {
        name: "demo".to_string(),
        ..Formula::default()
    };
    let keg = prefix.keg("demo", "1");
    relativize_prefix_symlinks(&prefix, &f, &staged, &keg).unwrap();
    // From <keg>/lib into the same directory: a bare name.
    assert_eq!(
        std::fs::read_link(staged.join("lib/libdemo.1.dylib")).unwrap(),
        Path::new("libdemo.dylib")
    );
    // From <keg>/lib to <prefix>/opt/helper/bin/helper: four `..`.
    assert_eq!(
        std::fs::read_link(staged.join("lib/helper")).unwrap(),
        Path::new("../../../../opt/helper/bin/helper")
    );
    // Unmapped system links are left alone; the java placeholder
    // remains for the leftover scan to delegate.
    assert_eq!(
        std::fs::read_link(staged.join("lib/sys")).unwrap(),
        Path::new("/usr/lib/libSystem.B.dylib")
    );
    let leftovers = linkage_leftovers(&staged);
    assert_eq!(leftovers.len(), 1);
    assert!(leftovers[0].contains("placeholder symlink"));
}

#[test]
fn text_placeholders_are_substituted_in_text_files_only() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let staged = tmp.path().join("staged");
    std::fs::create_dir_all(staged.join("etc/profile.d")).unwrap();
    let script = staged.join("etc/profile.d/demo.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n. @@HOMEBREW_CELLAR@@/demo/1/etc/demo\n",
    )
    .unwrap();
    let binary = staged.join("blob.bin");
    let mut raw = b"@@HOMEBREW_PREFIX@@".to_vec();
    raw.push(0);
    std::fs::write(&binary, &raw).unwrap();
    let f = Formula {
        name: "demo".to_string(),
        ..Formula::default()
    };
    replace_text_placeholders(&prefix, &f, &staged).unwrap();
    let out = std::fs::read_to_string(&script).unwrap();
    assert_eq!(
        out,
        format!(
            "#!/bin/sh\n. {}/demo/1/etc/demo\n",
            prefix.cellar().display()
        )
    );
    // A NUL-bearing file is not text: left byte-identical for the
    // leftover scan to delegate.
    assert_eq!(std::fs::read(&binary).unwrap(), raw);
}

#[cfg(unix)]
#[test]
fn shared_namespace_preserves_payloads_and_unlink_independence() {
    use std::os::unix::fs::MetadataExt as _;

    for namespace in [
        "share/vim",
        "lib/opaque-widget",
        "include/opaque-widget",
        "Frameworks/opaque-widget",
    ] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let make = |name: &str| {
            let keg = prefix.keg(name, "1.0");
            let path = keg.join(namespace).join("nested").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, name).unwrap();
            prefix
                .write_receipt(
                    &keg,
                    &[
                        ("name", name),
                        ("version", "1.0"),
                        ("installed_by", "abrew"),
                    ],
                )
                .unwrap();
            (keg, path)
        };
        let (left, left_file) = make("left");
        let (right, right_file) = make("right");
        let left_identity = std::fs::metadata(&left_file).unwrap();
        link_keg(&prefix, &left, false).unwrap();
        let shared = prefix.root.join(namespace);
        assert!(
            shared.is_symlink(),
            "first installation retains the directory policy"
        );
        link_keg(&prefix, &right, false).unwrap();
        assert!(!shared.is_symlink());
        assert!(shared.is_dir());
        assert_eq!(std::fs::read(shared.join("nested/left")).unwrap(), b"left");
        assert_eq!(
            std::fs::read(shared.join("nested/right")).unwrap(),
            b"right"
        );
        assert!(is_linked(&prefix, "left", "1.0"));
        assert!(is_linked(&prefix, "right", "1.0"));
        unlink_keg(&prefix, "left").unwrap();
        assert!(!shared.join("nested/left").exists());
        assert_eq!(
            std::fs::read(shared.join("nested/right")).unwrap(),
            b"right"
        );
        link_keg(&prefix, &left, false).unwrap();
        unlink_keg(&prefix, "right").unwrap();
        assert_eq!(std::fs::read(shared.join("nested/left")).unwrap(), b"left");
        assert!(!shared.join("nested/right").exists());
        assert_eq!(std::fs::read(&left_file).unwrap(), b"left");
        assert_eq!(std::fs::read(&right_file).unwrap(), b"right");
        assert_eq!(
            std::fs::metadata(&left_file).unwrap().ino(),
            left_identity.ino()
        );
    }
}

#[cfg(unix)]
fn shared_namespace_keg(
    prefix: &Prefix,
    name: &str,
    version: &str,
    files: &[(&str, &[u8])],
) -> PathBuf {
    let keg = prefix.keg(name, version);
    for (rel, bytes) in files {
        let path = keg.join("share/vim").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    prefix
        .write_receipt(
            &keg,
            &[
                ("name", name),
                ("version", version),
                ("installed_by", "abrew"),
            ],
        )
        .unwrap();
    keg
}

#[cfg(unix)]
#[test]
fn shared_namespace_rolls_back_activation_and_empty_parent_creation() {
    use std::os::unix::fs::MetadataExt as _;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let left = shared_namespace_keg(&prefix, "left", "1.0", &[("nested/left", b"left")]);
    let right = shared_namespace_keg(&prefix, "right", "1.0", &[("new/a", b"a"), ("new/b", b"b")]);
    // Model a previously published whole-directory alias beneath a
    // namespace whose current policy creates parents for each leaf.
    for keg in [&left, &right] {
        std::fs::create_dir_all(keg.join("etc")).unwrap();
        std::fs::rename(keg.join("share/vim"), keg.join("etc/shared")).unwrap();
    }
    let namespace = prefix.root.join("etc/shared");
    symlink_dir(&left.join("etc/shared"), &namespace).unwrap();
    let identity = std::fs::symlink_metadata(&namespace).unwrap();
    let target = std::fs::read_link(&namespace).unwrap();
    let result = with_shared_namespaces(&prefix, Some(&right), || {
        let links = keg_link_plan(&prefix, &right)?;
        let mut placed = 0;
        activate_keg_with(&prefix, &right, &links, false, |link| {
            placed += 1;
            if placed == 2 {
                return Err("injected later activation failure".into());
            }
            symlink_file(&link.source, &link.destination)
        })
    });
    assert_eq!(
        result,
        Err(InstallError::Failed(
            "injected later activation failure".into()
        ))
    );
    assert_eq!(std::fs::read_link(&namespace).unwrap(), target);
    assert_eq!(
        std::fs::symlink_metadata(&namespace).unwrap().ino(),
        identity.ino()
    );
    assert_eq!(
        std::fs::read(namespace.join("nested/left")).unwrap(),
        b"left"
    );
    assert!(!namespace.join("new").exists());
    assert_eq!(std::fs::read(right.join("etc/shared/new/a")).unwrap(), b"a");
}

#[cfg(unix)]
#[test]
fn shared_namespace_restores_install_upgrade_and_reinstall_failures() {
    use std::os::unix::fs::MetadataExt as _;
    for mode in [ReplaceMode::None, ReplaceMode::NewKeg, ReplaceMode::SameKeg] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let old = shared_namespace_keg(&prefix, "left", "1.0", &[("nested/left", b"old")]);
        link_keg(&prefix, &old, false).unwrap();
        point_opt(&prefix, "left", "1.0").unwrap();
        let old_receipt = std::fs::read(old.join("INSTALL_RECEIPT.json")).unwrap();
        let namespace = prefix.root.join("share/vim");
        let identity = std::fs::symlink_metadata(&namespace).unwrap();
        let owner = if mode == ReplaceMode::None {
            "right"
        } else {
            "left"
        };
        let version = if mode == ReplaceMode::NewKeg {
            "2.0"
        } else {
            "1.0"
        };
        let live = prefix.keg(owner, version);
        let work = aterm_tempfile::TempDir::new_in(prefix.cellar()).unwrap();
        let stage = work.path().join("payload");
        std::fs::create_dir_all(stage.join("share/vim/new")).unwrap();
        std::fs::write(stage.join("share/vim/new/a"), b"a").unwrap();
        std::fs::create_dir_all(stage.join("share/vim/nested")).unwrap();
        std::fs::write(stage.join("share/vim/nested/left"), b"new").unwrap();
        // A fresh install must use a distinct leaf; a same-name leaf is a
        // genuine conflict owned by the first formula.
        if mode == ReplaceMode::None {
            std::fs::rename(
                stage.join("share/vim/nested/left"),
                stage.join("share/vim/nested/right"),
            )
            .unwrap();
        }
        prefix
            .write_receipt(
                &stage,
                &[
                    ("name", owner),
                    ("version", version),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        let result = with_shared_namespaces(&prefix, Some(&stage), || {
            let mut links = Vec::new();
            plan_links(&stage, &live, &prefix.root, &mut links)?;
            let mut count = 0;
            let place = |link: &InstallLink| {
                count += 1;
                if count == 2 {
                    return Err("injected publication failure".to_string());
                }
                symlink_file(&link.source, &link.destination)
            };
            if mode == ReplaceMode::SameKeg {
                reinstall_payload_with(
                    &prefix,
                    &stage,
                    &live,
                    &links,
                    &prefix.cellar().join(owner),
                    place,
                )
            } else {
                publish_payload_with(
                    &prefix,
                    &stage,
                    &live,
                    &links,
                    (mode == ReplaceMode::NewKeg)
                        .then_some(prefix.cellar().join(owner))
                        .as_deref(),
                    None,
                    place,
                )
            }
        });
        assert_eq!(
            result,
            Err(InstallError::Failed("injected publication failure".into()))
        );
        assert_eq!(
            std::fs::read_link(&namespace).unwrap(),
            old.join("share/vim")
        );
        assert_eq!(
            std::fs::symlink_metadata(&namespace).unwrap().ino(),
            identity.ino()
        );
        assert_eq!(
            std::fs::read(old.join("share/vim/nested/left")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(old.join("INSTALL_RECEIPT.json")).unwrap(),
            old_receipt
        );
        assert_eq!(std::fs::read_link(prefix.opt("left")).unwrap(), old);
        if mode != ReplaceMode::SameKeg {
            assert!(!live.exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn shared_namespace_refuses_foreign_aliases_and_real_leaf_conflicts() {
    for case in 0..4 {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let left = shared_namespace_keg(&prefix, "left", "1.0", &[("nested/item", b"left")]);
        let right = shared_namespace_keg(&prefix, "right", "1.0", &[("nested/item", b"right")]);
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"user bytes").unwrap();
        let namespace = prefix.root.join("share/vim");
        match case {
            0 => {
                symlink_dir(&outside, &namespace).unwrap();
            }
            1 => {
                symlink_dir(&left.join("share"), &namespace).unwrap();
            }
            2 => {
                std::fs::remove_dir_all(left.join("share/vim")).unwrap();
                symlink_dir(&outside, &left.join("share/vim")).unwrap();
                symlink_dir(&left.join("share/vim"), &namespace).unwrap();
            }
            _ => {
                link_keg(&prefix, &left, false).unwrap();
            }
        }
        let target = std::fs::read_link(&namespace).unwrap();
        assert!(link_keg(&prefix, &right, true).is_err());
        assert_eq!(std::fs::read_link(&namespace).unwrap(), target);
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"user bytes");
        assert_eq!(
            std::fs::read(right.join("share/vim/nested/item")).unwrap(),
            b"right"
        );
    }
}

#[cfg(unix)]
#[test]
fn shared_namespace_never_traverses_payload_symlinks_or_deletes_foreign_rollback_data() {
    for conflict in [false, true] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let left = shared_namespace_keg(&prefix, "left", "1.0", &[("left", b"left")]);
        let right = shared_namespace_keg(&prefix, "right", "1.0", &[("alias/new", b"right")]);
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"user bytes").unwrap();
        symlink_dir(&outside, &left.join("share/vim/alias")).unwrap();
        link_keg(&prefix, &left, false).unwrap();
        let namespace = prefix.root.join("share/vim");
        if conflict {
            let error = with_shared_namespaces::<()>(&prefix, Some(&right), || {
                std::fs::write(namespace.join("user-file"), b"retained").unwrap();
                Err(InstallError::Failed(
                    "injected failure with foreign data".into(),
                ))
            })
            .unwrap_err();
            assert!(error.message().contains("original alias retained"));
            assert_eq!(
                std::fs::read(namespace.join("user-file")).unwrap(),
                b"retained"
            );
        } else {
            assert!(link_keg(&prefix, &right, false).is_err());
            assert_eq!(
                std::fs::read_link(&namespace).unwrap(),
                left.join("share/vim")
            );
            assert!(!outside.join("new").exists());
        }
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"user bytes");
        assert_eq!(std::fs::read(left.join("share/vim/left")).unwrap(), b"left");
    }
}

#[test]
fn pkg_config_relocation_preserves_spaced_prefix_as_one_flag() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("Application Support/native"));
    let staged = tmp.path().join("staged");
    std::fs::create_dir_all(staged.join("lib/pkgconfig")).unwrap();
    let pc = staged.join("lib/pkgconfig/demo.pc");
    std::fs::write(&pc, "prefix=@@HOMEBREW_PREFIX@@\nlibdir=${prefix}/lib\nLibs: -L${libdir} -ldemo\nCflags: -I${prefix}/include\n").unwrap();
    let formula = Formula {
        name: "demo".to_owned(),
        ..Formula::default()
    };
    replace_text_placeholders(&prefix, &formula, &staged).unwrap();
    let actual = std::fs::read_to_string(pc).unwrap();
    assert_eq!(
        actual,
        format!(
            "prefix={}\nlibdir=${{prefix}}/lib\nLibs: \"-L${{libdir}}\" -ldemo\nCflags: \"-I${{prefix}}/include\"\n",
            prefix.root.display()
        )
    );
}

// abrew refuses to run on Windows (Homebrew is macOS/Linux software).
#[cfg(unix)]
#[test]
fn plan_links_follows_brew_link_dir_modes() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let keg = tmp.path().join("keg");
    let root = tmp.path().join("prefix");
    let put = |rel: &str| {
        let path = keg.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    };
    put("bin/tool");
    put("bin/sub/hidden");
    put("share/doc/demo/readme.md");
    put("share/man/man1/demo.1");
    put("share/locale/en/LC_MESSAGES/demo.mo");
    put("lib/pkgconfig/demo.pc");
    put("lib/node/demo/index.js");
    put("include/postgresql@16/pg.h");
    put("lib/.DS_Store");
    let mut links = Vec::new();
    plan_links(&keg, &keg, &root, &mut links).unwrap();
    let got: Vec<String> = links
        .iter()
        .map(|l| {
            format!(
                "{}:{}",
                l.destination.strip_prefix(&root).unwrap().display(),
                if l.directory { "dir" } else { "file" }
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            "bin/tool:file",
            "lib/node/demo/index.js:file",
            "lib/pkgconfig/demo.pc:file",
            "include/postgresql@16/pg.h:file",
            "share/doc/demo:dir",
            "share/locale/en/LC_MESSAGES/demo.mo:file",
            "share/man/man1/demo.1:file",
        ]
    );
}

#[test]
fn bottle_dest_matches_brew_downloads_naming() {
    // Oracle: `brew --cache jq` prints this exact tail
    // (sha256 of the bottle URL, then the bottle filename).
    let prefix = Prefix::at(std::path::Path::new("/p"));
    let f = Formula {
        name: "jq".to_string(),
        version: "1.8.2".to_string(),
        bottle_rebuild: 1,
        ..Formula::default()
    };
    let bottle = super::super::formula::Bottle {
        tag: "arm64_golden_gate".to_string(),
        url: "https://ghcr.io/v2/homebrew/core/jq/blobs/sha256:d048dd4c28d867e939065a3bc8209f37e2a5be3e2c18d18c8c7da8f4d1629a4a"
            .to_string(),
        sha256: "d048dd4c28d867e939065a3bc8209f37e2a5be3e2c18d18c8c7da8f4d1629a4a"
            .to_string(),
        cellar: ":any".to_string(),
    };
    assert_eq!(
        bottle_dest(&prefix, &f, &bottle),
        std::path::PathBuf::from(
            "/p/cache/downloads/da850b1765922f36315ec227ef66d8780c3508b916b9dde242025e9828e67b26--jq--1.8.2.arm64_golden_gate.bottle.1.tar.gz"
        )
    );
}

#[test]
fn bottle_filenames_and_dep_parents_match_brew_narration() {
    let mut lz4 = Formula {
        name: "lz4".to_string(),
        version: "1.10.0".to_string(),
        bottle_rebuild: 3,
        ..Formula::default()
    };
    assert_eq!(
        bottle_filename(&lz4, "arm64_golden_gate"),
        "lz4--1.10.0.arm64_golden_gate.bottle.3.tar.gz"
    );
    lz4.bottle_rebuild = 0;
    lz4.revision = 1;
    assert_eq!(
        bottle_filename(&lz4, "arm64_tahoe"),
        "lz4--1.10.0_1.arm64_tahoe.bottle.tar.gz"
    );
    let mk = |name: &str, deps: &[&str]| Formula {
        name: name.to_string(),
        dependencies: deps.iter().map(ToString::to_string).collect(),
        ..Formula::default()
    };
    let planned = vec![mk("leaf", &[]), mk("mid", &["leaf"]), mk("top", &["mid"])];
    let parents = dep_parents(&planned, "top");
    assert_eq!(parents.get("mid").map(String::as_str), Some("top"));
    assert_eq!(parents.get("leaf").map(String::as_str), Some("mid"));
    assert_eq!(specified_of("homebrew/core/zstd"), "zstd");
    assert_eq!(tap_of(&mk("x", &[])), "homebrew/core");
}

#[test]
fn runtime_closure_records_the_recursive_graph_in_declared_order() {
    let mk = |name: &str, deps: &[&str]| Formula {
        name: name.to_string(),
        full_name: name.to_string(),
        version: "1".to_string(),
        dependencies: deps.iter().map(ToString::to_string).collect(),
        ..Formula::default()
    };
    let records = [
        mk("top", &["mid", "shared"]),
        mk("mid", &["leaf", "shared"]),
        mk("leaf", &[]),
        mk("shared", &[]),
    ];
    let read = |short: &str| records.iter().find(|r| r.name == short).cloned();
    let closure = runtime_closure(&records[0], &read);
    let names: Vec<(&str, bool)> = closure
        .iter()
        .map(|d| (d.full_name.as_str(), d.declared))
        .collect();
    assert_eq!(names, [("leaf", false), ("shared", true), ("mid", true),]);
    let json = runtime_deps_json(&closure);
    assert!(json.contains("\"full_name\": \"mid\", \"version\": \"1\""));
    assert!(json.starts_with('[') && json.ends_with(']'));
    assert_eq!(runtime_deps_json(&[]), "[]");
}

#[test]
fn a_fetch_plan_keeps_installed_dependencies_and_their_descendants() {
    let read = |name: &str| -> Result<Formula, InstallError> {
        let dependencies = match name {
            "app" => vec!["dep", "shared"],
            "dep" => vec!["leaf", "shared"],
            "leaf" | "shared" | "tool" => vec![],
            _ => return Err(InstallError::Failed("captured unknown".into())),
        };
        Ok(Formula {
            name: name.into(),
            dependencies: dependencies.into_iter().map(str::to_string).collect(),
            build_dependencies: if name == "app" {
                vec!["tool".to_string()]
            } else {
                Vec::new()
            },
            ..Formula::default()
        })
    };
    let names = |plan: Vec<Formula>| plan.into_iter().map(|f| f.name).collect::<Vec<_>>();
    let mut install_reads = Vec::new();
    let install = dependency_plan(
        "app",
        false,
        false,
        |n| {
            install_reads.push(n.to_string());
            read(n)
        },
        |_, dep| dep.name == "dep",
    )
    .unwrap();
    assert_eq!(names(install), ["shared", "app"]);
    assert_eq!(install_reads, ["app", "dep", "shared"]);
    let mut fetch_reads = Vec::new();
    let fetch = dependency_plan(
        "homebrew/core/app",
        false,
        true,
        |n| {
            fetch_reads.push(n.to_string());
            read(n)
        },
        |_, _| false,
    )
    .unwrap();
    assert_eq!(names(fetch), ["leaf", "shared", "dep", "tool", "app"]);
    assert_eq!(fetch_reads, ["app", "dep", "leaf", "shared", "tool"]);
    assert_eq!(
        names(dependency_plan("app", true, false, read, |_, _| false).unwrap()),
        ["app"]
    );
    assert!(
        dependency_plan(
            "other/tap/app",
            false,
            false,
            |_| panic!("qualified name must be refused before reading"),
            |_, _| false
        )
        .is_err()
    );
    let cycle = |n: &str| {
        Ok(Formula {
            name: n.into(),
            dependencies: vec!["app".into()],
            ..Formula::default()
        })
    };
    assert!(
        matches!(dependency_plan("app", false, false, cycle, |_, _| false), Err(InstallError::Failed(message)) if message.contains("dependency cycle"))
    );
    assert!(
        matches!(dependency_plan("app", false, false, |_| Ok(Formula { disable: true, ..Formula::default() }), |_, _| false), Err(InstallError::Failed(message)) if message.contains("disabled"))
    );
    assert_eq!(
        dependency_plan(
            "app",
            false,
            false,
            |_| Err(InstallError::Failed("captured read failure".into())),
            |_, _| false
        ),
        Err(InstallError::Failed("captured read failure".into()))
    );
}

#[cfg(unix)]
#[test]
fn keg_was_linked_judges_resolving_bin_links() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    p.ensure().unwrap();
    let make = |name: &str, files: &[&str]| {
        let keg = p.keg(name, "1.0");
        std::fs::create_dir_all(&keg).unwrap();
        for file in files {
            let path = keg.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"owned payload").unwrap();
        }
        p.write_receipt(
            &keg,
            &[
                ("name", name),
                ("version", "1.0"),
                ("installed_by", "abrew"),
            ],
        )
        .unwrap();
        symlink_dir(&keg, &p.opt(name)).unwrap();
        keg
    };
    let keg = make("demo", &["bin/tool"]);
    let dest = p.bin().join("tool");
    assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));
    link_keg(&p, &keg, false).unwrap();
    assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(true));
    std::fs::remove_file(&dest).unwrap();
    symlink_file(Path::new("../Cellar/demo/1.0/bin/tool"), &dest).unwrap();
    assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(true));
    std::fs::remove_file(&dest).unwrap();
    symlink_file(&keg.join("bin/other"), &dest).unwrap();
    assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));
    std::fs::remove_file(&dest).unwrap();
    std::fs::write(&dest, b"foreign regular").unwrap();
    assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));
    assert_eq!(std::fs::read(&dest).unwrap(), b"foreign regular");
    std::fs::remove_file(&dest).unwrap();
    assert!(p.opt("demo").symlink_metadata().is_ok());
    assert_eq!(keg_was_linked(&p, "demo", "1.0"), Ok(false));

    let library = make("libdemo", &["lib/libdemo.a", "include/libdemo.h"]);
    assert_eq!(keg_was_linked(&p, "libdemo", "1.0"), Ok(false));
    link_keg(&p, &library, false).unwrap();
    assert_eq!(keg_was_linked(&p, "libdemo", "1.0"), Ok(true));
    std::fs::remove_file(p.root.join("lib/libdemo.a")).unwrap();
    assert!(matches!(
        keg_was_linked(&p, "libdemo", "1.0"),
        Err(InstallError::Failed(_))
    ));
    let next = Formula {
        name: "libdemo".into(),
        full_name: "libdemo".into(),
        version: "2.0".into(),
        ..Formula::default()
    };
    assert!(matches!(
        preflight(
            &p,
            &next,
            &InstallOpts {
                upgrade: true,
                ..InstallOpts::default()
            },
            Machine::this(),
            true,
        ),
        Err(InstallError::Failed(_))
    ));
    assert_eq!(
        std::fs::read(library.join("lib/libdemo.a")).unwrap(),
        b"owned payload"
    );
    assert!(!p.keg("libdemo", "2.0").exists());
    unlink_keg(&p, "libdemo").unwrap();
    assert_eq!(keg_was_linked(&p, "libdemo", "1.0"), Ok(false));
    assert!(p.opt("libdemo").symlink_metadata().is_ok());

    let layout = make(
        "layout",
        &[
            "bin/layout-tool",
            "sbin/layout-admin",
            "lib/layout.a",
            "include/layout.h",
            "share/doc/layout/readme",
            "etc/layout.conf",
        ],
    );
    assert_eq!(keg_was_linked(&p, "layout", "1.0"), Ok(false));
    link_keg(&p, &layout, false).unwrap();
    assert_eq!(keg_was_linked(&p, "layout", "1.0"), Ok(true));
    unlink_keg(&p, "layout").unwrap();
    assert_eq!(keg_was_linked(&p, "layout", "1.0"), Ok(false));

    make("empty", &[]);
    assert!(matches!(
        keg_was_linked(&p, "empty", "1.0"),
        Err(InstallError::Failed(_))
    ));
    let bad = make("bad", &[]);
    std::fs::write(bad.join("bin"), b"not a directory").unwrap();
    assert!(matches!(
        keg_was_linked(&p, "bad", "1.0"),
        Err(InstallError::Failed(_))
    ));
    assert_eq!(std::fs::read(bad.join("bin")).unwrap(), b"not a directory");
    let nested = make("nested", &["share/guarded/file"]);
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("file"), b"outside retained").unwrap();
    symlink_dir(&outside, &p.root.join("share/guarded")).unwrap();
    assert!(matches!(
        keg_was_linked(&p, "nested", "1.0"),
        Err(InstallError::Failed(_))
    ));
    assert_eq!(
        std::fs::read(outside.join("file")).unwrap(),
        b"outside retained"
    );
    assert_eq!(
        std::fs::read(nested.join("share/guarded/file")).unwrap(),
        b"owned payload"
    );
}

#[cfg(unix)]
#[test]
fn unlink_keeps_the_opt_pointer_and_its_removal_is_separate() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    p.ensure().unwrap();
    let keg = p.keg("demo", "1.0");
    std::fs::create_dir_all(keg.join("bin")).unwrap();
    std::fs::write(keg.join("bin/tool"), b"1.0").unwrap();
    p.write_receipt(
        &keg,
        &[
            ("name", "demo"),
            ("version", "1.0"),
            ("installed_by", "abrew"),
        ],
    )
    .unwrap();
    point_opt(&p, "demo", "1.0").unwrap();
    link_keg(&p, &keg, false).unwrap();
    assert!(std::fs::symlink_metadata(p.bin().join("tool")).is_ok());

    unlink_keg(&p, "demo").unwrap();
    assert!(std::fs::symlink_metadata(p.bin().join("tool")).is_err());
    assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), keg);
    assert_eq!(p.optlinked_version("demo").as_deref(), Some("1.0"));

    remove_owned_opt_link(&p, "demo").unwrap();
    assert!(std::fs::symlink_metadata(p.opt("demo")).is_err());
}

#[cfg(unix)]
#[test]
fn an_opt_update_retains_unowned_entries_and_refuses_a_directory_alias() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    p.ensure().unwrap();
    let old = p.keg("demo", "0.9");
    let new = p.keg("demo", "1.0");
    let other = p.keg("other", "1.0");
    for (name, version) in [("demo", "0.9"), ("demo", "1.0"), ("other", "1.0")] {
        let keg = p.keg(name, version);
        std::fs::create_dir_all(keg.join("bin")).unwrap();
        std::fs::write(keg.join("bin/tool"), version).unwrap();
        let run = keg.join("bin/run");
        std::fs::write(&run, format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n")).unwrap();
        std::fs::set_permissions(run, std::fs::Permissions::from_mode(0o755)).unwrap();
        if name == "demo" && version == "0.9" {
            std::fs::write(keg.join("bin/old-only"), b"old artifact").unwrap();
        }
        p.write_receipt(
            &keg,
            &[
                ("name", name),
                ("version", version),
                ("installed_by", "abrew"),
            ],
        )
        .unwrap();
    }
    point_opt(&p, "demo", "0.9").unwrap();
    link_keg(&p, &old, false).unwrap();
    assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
    let formula = Formula {
        name: "demo".to_string(),
        ..Formula::default()
    };
    for linked_parent in [false, true] {
        let target = if linked_parent {
            new.join("share/new/tool")
        } else {
            new.join("bin/conflict")
        };
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"new artifact").unwrap();
        let foreign = if linked_parent {
            let outside = tmp.path().join("external-share");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("keep"), b"owner bytes").unwrap();
            let link = p.root.join("share/new");
            symlink_dir(&outside, &link).unwrap();
            link
        } else {
            let file = p.bin().join("conflict");
            std::fs::write(&file, b"owner bytes").unwrap();
            file
        };
        for action in 0..3 {
            let result = match action {
                0 => switch_keg(&p, "demo", "1.0", false),
                1 => relink(&p, &formula).map(|_| ()),
                _ => link_keg(&p, &new, false),
            };
            assert!(
                result.is_err(),
                "activation {action}, alias={linked_parent}"
            );
            assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
            assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"0.9");
            assert_eq!(
                std::fs::read(p.bin().join("old-only")).unwrap(),
                b"old artifact"
            );
            let output = aterm_uds::spawnfd::command(p.bin().join("run"))
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"0.9\n");
            let bytes = if linked_parent {
                std::fs::read(foreign.join("keep")).unwrap()
            } else {
                std::fs::read(&foreign).unwrap()
            };
            assert_eq!(bytes, b"owner bytes");
        }
        std::fs::remove_file(foreign).unwrap();
        std::fs::remove_file(target).unwrap();
    }
    for remove_old in [false, true] {
        let (keg, links) = activation_links(&p, "demo", "1.0", false).unwrap();
        for fail_at in [2, links.len()] {
            let mut placed = 0;
            let error = activate_keg_with(&p, &keg, &links, remove_old, |link| {
                placed += 1;
                if placed == fail_at {
                    return Err("injected later link failure".to_string());
                }
                symlink_dir(&link.source, &link.destination)
            })
            .unwrap_err();
            assert_eq!(
                error,
                InstallError::Failed("injected later link failure".to_string())
            );
            assert_eq!(placed, fail_at);
            assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
            assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"0.9");
            assert_eq!(
                std::fs::read(p.bin().join("old-only")).unwrap(),
                b"old artifact"
            );
            let output = aterm_uds::spawnfd::command(p.bin().join("run"))
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"0.9\n");
        }
    }
    let foreign_target = tmp.path().join("foreign-target");
    for foreign_link in [false, true] {
        let (keg, links) = activation_links(&p, "demo", "1.0", false).unwrap();
        let mut first: Option<PathBuf> = None;
        let error = activate_keg_with(&p, &keg, &links, true, |link| {
            if let Some(path) = &first {
                std::fs::remove_file(path).unwrap();
                if foreign_link {
                    symlink_file(&foreign_target, path).unwrap();
                } else {
                    std::fs::write(path, b"foreign replacement").unwrap();
                }
                return Err("injected replacement".to_string());
            }
            symlink_dir(&link.source, &link.destination)?;
            first = Some(link.destination.clone());
            Ok(())
        })
        .unwrap_err();
        assert!(
            matches!(error, InstallError::Failed(ref message) if message.contains("could not restore aliases")),
            "foreign_link={foreign_link}: {error:?}"
        );
        let path = first.unwrap();
        if foreign_link {
            assert_eq!(std::fs::read_link(&path).unwrap(), foreign_target);
        } else {
            assert_eq!(std::fs::read(&path).unwrap(), b"foreign replacement");
        }
        assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
        std::fs::remove_file(&path).unwrap();
        let old_target = old.join("bin").join(path.file_name().unwrap());
        symlink_file(&old_target, &path).unwrap();
    }
    let (run, tool) = (p.bin().join("run"), p.bin().join("tool"));
    for foreign_link in [false, true] {
        let (keg, links) = activation_links(&p, "demo", "1.0", false).unwrap();
        let error = activate_keg_with(&p, &keg, &links, false, |link| {
            symlink_dir(&link.source, &link.destination)?;
            if link.destination == run {
                std::fs::remove_file(&tool).unwrap();
                if foreign_link {
                    symlink_file(&foreign_target, &tool).unwrap();
                } else {
                    std::fs::write(&tool, b"foreign replacement").unwrap();
                }
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(
            error,
            InstallError::Failed(format!(
                "abrew: link changed before activation: {}",
                tool.display()
            )),
            "foreign_link={foreign_link}"
        );
        if foreign_link {
            assert_eq!(std::fs::read_link(&tool).unwrap(), foreign_target);
        } else {
            assert_eq!(std::fs::read(&tool).unwrap(), b"foreign replacement");
        }
        assert_eq!(std::fs::read_link(&run).unwrap(), old.join("bin/run"));
        assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), old);
        std::fs::remove_file(&tool).unwrap();
        symlink_file(&old.join("bin/tool"), &tool).unwrap();
    }
    point_opt(&p, "demo", "1.0").unwrap();
    assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), new);
    std::fs::remove_file(p.opt("demo")).unwrap();
    symlink_dir(&other, &p.opt("demo")).unwrap();
    assert!(point_opt(&p, "demo", "1.0").is_err());
    assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), other);
    std::fs::remove_file(p.opt("demo")).unwrap();
    std::fs::write(p.opt("demo"), b"foreign regular bytes").unwrap();
    assert!(point_opt(&p, "demo", "1.0").is_err());
    assert_eq!(
        std::fs::read(p.opt("demo")).unwrap(),
        b"foreign regular bytes"
    );
    std::fs::remove_file(p.opt("demo")).unwrap();
    symlink_dir(Path::new("../Cellar/demo/missing"), &p.opt("demo")).unwrap();
    assert!(point_opt(&p, "demo", "1.0").is_err());
    assert_eq!(
        std::fs::read_link(p.opt("demo")).unwrap(),
        Path::new("../Cellar/demo/missing")
    );
    std::fs::remove_file(p.opt("demo")).unwrap();
    std::fs::create_dir(p.opt("demo")).unwrap();
    std::fs::write(p.opt("demo").join("keep"), b"directory bytes").unwrap();
    assert!(point_opt(&p, "demo", "1.0").is_err());
    assert_eq!(
        std::fs::read(p.opt("demo").join("keep")).unwrap(),
        b"directory bytes"
    );
    std::fs::remove_file(p.opt("demo").join("keep")).unwrap();
    std::fs::remove_dir(p.opt("demo")).unwrap();
    std::fs::remove_dir(p.root.join("opt")).unwrap();
    let outside = tmp.path().join("external-opt");
    std::fs::create_dir(&outside).unwrap();
    symlink_dir(&old, &outside.join("demo")).unwrap();
    symlink_dir(&outside, &p.root.join("opt")).unwrap();
    assert!(point_opt(&p, "demo", "1.0").is_err());
    assert!(switch_keg(&p, "demo", "1.0", false).is_err());
    assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"0.9");
    assert_eq!(std::fs::read_link(outside.join("demo")).unwrap(), old);
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    assert!(point_opt(&p, "../demo", "1.0").is_err());
    assert!(point_opt(&p, "demo", "../1.0").is_err());
    std::fs::remove_file(p.root.join("opt")).unwrap();
    switch_keg(&p, "demo", "1.0", false).unwrap();
    assert_eq!(std::fs::read_link(p.opt("demo")).unwrap(), new);
    assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"1.0");
    assert!(std::fs::symlink_metadata(p.bin().join("old-only")).is_err());
    for bad in [
        p.cellar().join("demo/../other/1.0"),
        p.cellar().join("demo/./1.0"),
        p.cellar().join("demo/1.0/"),
    ] {
        assert!(link_keg(&p, &bad, true).is_err(), "{bad:?}");
    }
    assert!(unlink_keg(&p, "..").is_err());
    let dead = p.bin().join("deadtool");
    symlink_dir(&p.keg("gone", "1.0").join("bin/deadtool"), &dead).unwrap();
    let foreign = p.bin().join("foreigntool");
    symlink_dir(Path::new("/nonexistent/foreigntool"), &foreign).unwrap();
    assert_eq!(remove_dangling_links(&p, "gone").unwrap(), 1);
    assert!(std::fs::symlink_metadata(&dead).is_err());
    assert!(std::fs::symlink_metadata(&foreign).is_ok());
    assert_eq!(remove_dangling_links(&p, "gone").unwrap(), 0);
    assert!(remove_dangling_links(&p, "..").is_err());
    let alias = p.keg("alias", "1.0");
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    symlink_dir(&new, &alias).unwrap();
    assert!(link_keg(&p, &alias, true).is_err());
    assert_eq!(std::fs::read(p.bin().join("tool")).unwrap(), b"1.0");
}

/// An entry of another kind is never the entry an identity was read from, even
/// on that entry's inode number, which ext4 hands to the next entry made: a file
/// where an admitted link was, or a link where a cask's directory was, is a
/// change for every caller (cask's parents and payloads, the prefix's ancestor
/// chain), whether or not the caller also compares a link's target.
#[cfg(unix)]
#[test]
fn an_entry_of_another_kind_is_not_the_same_path_identity() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let path = tmp.path().join("entry");
    let make = |kind: &str| match kind {
        "link" => symlink_dir(&target, &path).unwrap(),
        "file" => std::fs::write(&path, b"foreign bytes").unwrap(),
        _ => std::fs::create_dir(&path).unwrap(),
    };
    let remove = |kind: &str| match kind {
        "directory" => std::fs::remove_dir(&path).unwrap(),
        _ => std::fs::remove_file(&path).unwrap(),
    };
    for (was, now) in [("link", "file"), ("directory", "link"), ("file", "link")] {
        make(was);
        let identity = std::fs::symlink_metadata(&path).unwrap();
        assert!(same_path_identity(&path, &identity), "an untouched {was}");
        remove(was);
        make(now);
        assert!(
            !same_path_identity(&path, &identity),
            "a {now} where a {was} was"
        );
        remove(now);
    }
}

fn cache_formula(
    prefix: &Prefix,
    revision: u64,
    url: &str,
    sha: &str,
    cellar: &str,
    deps: &[&str],
) {
    let value = aterm_json::json!({
        "name": "demo", "versions": { "stable": "1.0" }, "revision": revision,
        "dependencies": deps,
        "bottle": { "stable": { "files": { "all": { "url": url, "sha256": sha, "cellar": cellar } } } }
    });
    let dir = prefix.cache().join("api");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("formula-demo.json"),
        aterm_json::to_vec(&value).unwrap(),
    )
    .unwrap();
}

#[test]
#[cfg(unix)]
fn already_current_top_pours_no_dependencies_like_brew() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    // demo installed current (as a dependency: not on
    // request); its dep is not installed at all. Brew has no
    // installer for a current top: it only warns and marks
    // the keg on-request, pouring nothing.
    let demo_keg = prefix.keg("demo", "1.0");
    std::fs::create_dir_all(&demo_keg).unwrap();
    prefix
        .write_receipt(
            &demo_keg,
            &[
                ("installed_by", "abrew"),
                ("name", "demo"),
                ("version", "1.0"),
                ("installed_on_request", "false"),
            ],
        )
        .unwrap();
    cache_formula(
        &prefix,
        0,
        "file:///nonexistent/demo.tar.gz",
        "abc",
        ":any_skip_relocation",
        &["dep"],
    );
    let archive = tmp.path().join("dep.tar.gz");
    std::fs::write(
        &archive,
        gzip_bytes(&tar_bytes(&[(
            "dep/3/bin/dep",
            b'0',
            "",
            b"#!/bin/sh\nprintf 'dep\\n'\n",
            0o755,
        )])),
    )
    .unwrap();
    let dep_record = aterm_json::json!({
        "name": "dep", "versions": { "stable": "3" },
        "bottle": { "stable": { "files": { "all": {
            "url": format!("file://{}", archive.display()),
            "sha256": fetch::sha256_file(&archive).unwrap(),
            "cellar": ":any_skip_relocation"
        } } } }
    });
    std::fs::write(
        prefix.cache().join("api/formula-dep.json"),
        aterm_json::to_vec(&dep_record).unwrap(),
    )
    .unwrap();
    let api = Api::new(prefix.clone());
    assert_eq!(
        install(&api, &prefix, "demo", &InstallOpts::default()).unwrap(),
        "1.0"
    );
    assert!(
        prefix.installed_one(Kind::Formula, "dep").is_none(),
        "dep must not pour for an already-current top"
    );
    let receipt = std::fs::read_to_string(demo_keg.join("INSTALL_RECEIPT.json")).unwrap();
    assert!(
        receipt.contains("\"installed_on_request\": \"true\""),
        "named install flips on-request: {receipt}"
    );
    // The dry plan likewise has no rows at all.
    let (top, rows) = dry_rows(&api, &prefix, "demo", &InstallOpts::default()).unwrap();
    assert!(
        top.is_none() && rows.is_empty(),
        "dry rows: {} row(s)",
        rows.len()
    );
}

#[test]
#[cfg(unix)]
fn fresh_install_upgrades_below_minimum_dep_like_brew() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    // dep installed at 2, below the 3 demo's bottle was
    // built against. Brew's installer expansion (shared by
    // install and upgrade) prunes only satisfied deps, so a
    // fresh plain install of demo upgrades dep to 3.
    let dep_keg = prefix.keg("dep", "2");
    std::fs::create_dir_all(dep_keg.join("bin")).unwrap();
    std::fs::write(dep_keg.join("bin/dep"), b"#!/bin/sh\nprintf 'dep2\\n'\n").unwrap();
    prefix
        .write_receipt(
            &dep_keg,
            &[
                ("installed_by", "abrew"),
                ("name", "dep"),
                ("version", "2"),
                ("installed_on_request", "false"),
            ],
        )
        .unwrap();
    let demo_archive = tmp.path().join("demo.tar.gz");
    std::fs::write(
        &demo_archive,
        gzip_bytes(&tar_bytes(&[(
            "demo/1.0/bin/demo",
            b'0',
            "",
            b"#!/bin/sh\nprintf 'demo\\n'\n",
            0o755,
        )])),
    )
    .unwrap();
    let demo_sha = fetch::sha256_file(&demo_archive).unwrap();
    cache_formula(
        &prefix,
        0,
        &format!("file://{}", demo_archive.display()),
        &demo_sha,
        ":any_skip_relocation",
        &["dep"],
    );
    let dep_archive = tmp.path().join("dep.tar.gz");
    std::fs::write(
        &dep_archive,
        gzip_bytes(&tar_bytes(&[(
            "dep/3/bin/dep",
            b'0',
            "",
            b"#!/bin/sh\nprintf 'dep\\n'\n",
            0o755,
        )])),
    )
    .unwrap();
    let dep_record = aterm_json::json!({
        "name": "dep", "versions": { "stable": "3" },
        "bottle": { "stable": { "files": { "all": {
            "url": format!("file://{}", dep_archive.display()),
            "sha256": fetch::sha256_file(&dep_archive).unwrap(),
            "cellar": ":any_skip_relocation"
        } } } }
    });
    std::fs::write(
        prefix.cache().join("api/formula-dep.json"),
        aterm_json::to_vec(&dep_record).unwrap(),
    )
    .unwrap();
    let api = Api::new(prefix.clone());
    let demo = api.formula("demo").unwrap();
    let tab = aterm_json::json!({
        "runtime_dependencies": [{ "full_name": "dep", "pkg_version": "3" }]
    });
    let manifest = aterm_json::json!({
        "manifests": [{ "annotations": {
            "sh.brew.bottle.digest": demo_sha,
            "sh.brew.tab": aterm_json::to_string(&tab).unwrap()
        } }]
    });
    std::fs::write(
        bottle_manifest_dest(&prefix, &demo),
        aterm_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(
        install(&api, &prefix, "demo", &InstallOpts::default()).unwrap(),
        "1.0"
    );
    assert_eq!(
        prefix
            .installed_one(Kind::Formula, "dep")
            .map(|inst| inst.version)
            .as_deref(),
        Some("3"),
        "fresh install must upgrade the below-minimum dep"
    );
}

#[test]
#[cfg(unix)]
fn already_current_top_below_minimum_dep_is_not_gated() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    // demo current; dep installed at 2, below the 3 its
    // bottle was built against. Admission must not gate the
    // dep replacement brew never attempts for a current top.
    let demo_keg = prefix.keg("demo", "1.0");
    std::fs::create_dir_all(&demo_keg).unwrap();
    prefix
        .write_receipt(
            &demo_keg,
            &[
                ("installed_by", "abrew"),
                ("name", "demo"),
                ("version", "1.0"),
                ("installed_on_request", "false"),
            ],
        )
        .unwrap();
    let dep_keg = prefix.keg("dep", "2");
    std::fs::create_dir_all(&dep_keg).unwrap();
    prefix
        .write_receipt(
            &dep_keg,
            &[("installed_by", "abrew"), ("name", "dep"), ("version", "2")],
        )
        .unwrap();
    cache_formula(
        &prefix,
        0,
        "file:///nonexistent/demo.tar.gz",
        "abc",
        ":any_skip_relocation",
        &["dep"],
    );
    let dep_record = aterm_json::json!({
        "name": "dep", "versions": { "stable": "3" },
        "bottle": { "stable": { "files": { "all": {
            "url": "file:///nonexistent/dep.tar.gz", "sha256": "abc",
            "cellar": ":any_skip_relocation"
        } } } }
    });
    std::fs::write(
        prefix.cache().join("api/formula-dep.json"),
        aterm_json::to_vec(&dep_record).unwrap(),
    )
    .unwrap();
    let api = Api::new(prefix.clone());
    let demo = api.formula("demo").unwrap();
    let tab = aterm_json::json!({
        "runtime_dependencies": [{ "full_name": "dep", "pkg_version": "3" }]
    });
    let manifest = aterm_json::json!({
        "manifests": [{ "annotations": {
            "sh.brew.bottle.digest": "abc",
            "sh.brew.tab": aterm_json::to_string(&tab).unwrap()
        } }]
    });
    std::fs::write(
        bottle_manifest_dest(&prefix, &demo),
        aterm_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert!(
        preflight_plan(&api, &prefix, "demo", &InstallOpts::default()).is_ok(),
        "current top must not gate its planned dep replacement"
    );
    assert_eq!(
        install(&api, &prefix, "demo", &InstallOpts::default()).unwrap(),
        "1.0"
    );
    assert_eq!(
        prefix
            .installed_one(Kind::Formula, "dep")
            .map(|inst| inst.version)
            .as_deref(),
        Some("2"),
        "below-minimum dep must not upgrade for a current top"
    );
}

#[test]
#[cfg(unix)]
fn alias_and_oldname_install_treats_resolved_formula_as_top() {
    let aliased_read = |n: &str| {
        Ok(match n {
            "app-alias" | "app" => Formula {
                name: "app".into(),
                aliases: vec!["app-alias".into()],
                dependencies: vec!["left".into(), "right".into()],
                ..Formula::default()
            },
            "left" => Formula {
                name: "left".into(),
                dependencies: vec!["shared-old".into()],
                ..Formula::default()
            },
            "right" => Formula {
                name: "right".into(),
                dependencies: vec!["shared".into()],
                ..Formula::default()
            },
            "shared-old" | "shared" => Formula {
                name: "shared".into(),
                oldnames: vec!["shared-old".into()],
                ..Formula::default()
            },
            other => Formula {
                name: other.into(),
                ..Formula::default()
            },
        })
    };
    let planned_names: Vec<String> =
        dependency_plan("app-alias", false, false, aliased_read, |_, _| false)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
    assert_eq!(planned_names, ["shared", "left", "right", "app"]);
    let aliased_cycle = |n: &str| {
        Ok(match n {
            "app-alias" => Formula {
                name: "app".into(),
                aliases: vec!["app-alias".into()],
                dependencies: vec!["dep".into()],
                ..Formula::default()
            },
            _ => Formula {
                name: "dep".into(),
                dependencies: vec!["app".into()],
                ..Formula::default()
            },
        })
    };
    assert!(matches!(
        dependency_plan("app-alias", false, false, aliased_cycle, |_, _| false),
        Err(InstallError::Failed(msg)) if msg.contains("dependency cycle")
    ));

    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    std::fs::create_dir_all(prefix.cache().join("api")).unwrap();
    let demo_archive = tmp.path().join("demo.tar.gz");
    std::fs::write(
        &demo_archive,
        gzip_bytes(&tar_bytes(&[(
            "demo/1.0/bin/demo",
            b'0',
            "",
            b"#!/bin/sh\nprintf 'demo\\n'\n",
            0o755,
        )])),
    )
    .unwrap();
    let demo_sha = fetch::sha256_file(&demo_archive).unwrap();
    let dep_archive = tmp.path().join("dep.tar.gz");
    std::fs::write(
        &dep_archive,
        gzip_bytes(&tar_bytes(&[(
            "dep/3/bin/dep",
            b'0',
            "",
            b"#!/bin/sh\nprintf 'dep\\n'\n",
            0o755,
        )])),
    )
    .unwrap();
    let demo_record = aterm_json::json!({
        "name": "demo",
        "full_name": "demo",
        "aliases": ["demo-alias"],
        "oldnames": ["demo-legacy"],
        "versions": { "stable": "1.0" },
        "dependencies": ["dep"],
        "bottle": { "stable": { "files": { "all": {
            "url": format!("file://{}", demo_archive.display()),
            "sha256": demo_sha,
            "cellar": ":any_skip_relocation"
        } } } }
    });
    for cache_key in [
        "formula-demo.json",
        "formula-demo-alias.json",
        "formula-demo-legacy.json",
    ] {
        std::fs::write(
            prefix.cache().join("api").join(cache_key),
            aterm_json::to_vec(&demo_record).unwrap(),
        )
        .unwrap();
    }
    let dep_record = aterm_json::json!({
        "name": "dep",
        "full_name": "dep",
        "versions": { "stable": "3" },
        "bottle": { "stable": { "files": { "all": {
            "url": format!("file://{}", dep_archive.display()),
            "sha256": fetch::sha256_file(&dep_archive).unwrap(),
            "cellar": ":any_skip_relocation"
        } } } }
    });
    std::fs::write(
        prefix.cache().join("api/formula-dep.json"),
        aterm_json::to_vec(&dep_record).unwrap(),
    )
    .unwrap();
    let api = Api::new(prefix.clone());
    let (dry_top, dry_deps) = dry_rows(
        &api,
        &prefix,
        "demo-alias",
        &InstallOpts {
            quiet: true,
            ..InstallOpts::default()
        },
    )
    .unwrap();
    assert_eq!(dry_top.as_ref().map(|r| r.name.as_str()), Some("demo"));
    assert_eq!(
        dry_deps.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["dep"]
    );

    assert_eq!(
        install(
            &api,
            &prefix,
            "demo-alias",
            &InstallOpts {
                quiet: true,
                ..InstallOpts::default()
            },
        )
        .unwrap(),
        "1.0"
    );
    let demo_inst = prefix.installed_one(Kind::Formula, "demo").unwrap();
    let dep_inst = prefix.installed_one(Kind::Formula, "dep").unwrap();
    assert!(
        super::super::commands::requested(&prefix, &demo_inst).unwrap(),
        "alias install must mark the resolved formula installed_on_request"
    );
    assert!(
        !super::super::commands::requested(&prefix, &dep_inst).unwrap(),
        "dependency stays installed_on_request = false"
    );

    // With the resolved top already current, a plain alias install must short-circuit
    // without gating or upgrading a below-minimum dependency, while marking on-request.
    super::super::prefix::set_installed_on_request(&prefix.keg("demo", "1.0"), false).unwrap();
    remove_owned_opt_link(&prefix, "dep").unwrap();
    let _ = std::fs::remove_file(prefix.bin().join("dep"));
    std::fs::remove_dir_all(prefix.keg("dep", "3")).unwrap();
    let dep_old = prefix.keg("dep", "2");
    std::fs::create_dir_all(dep_old.join("bin")).unwrap();
    std::fs::write(dep_old.join("bin/dep"), b"old-dep").unwrap();
    prefix
        .write_receipt(
            &dep_old,
            &[
                ("installed_by", "abrew"),
                ("name", "dep"),
                ("version", "2"),
                ("installed_on_request", "false"),
            ],
        )
        .unwrap();
    let demo = api.formula("demo-alias").unwrap();
    let tab = aterm_json::json!({
        "runtime_dependencies": [{ "full_name": "dep", "pkg_version": "3" }]
    });
    let manifest = aterm_json::json!({
        "manifests": [{ "annotations": {
            "sh.brew.bottle.digest": demo_sha,
            "sh.brew.tab": aterm_json::to_string(&tab).unwrap()
        } }]
    });
    std::fs::write(
        bottle_manifest_dest(&prefix, &demo),
        aterm_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert!(preflight_plan(&api, &prefix, "demo-legacy", &InstallOpts::default()).is_ok());
    assert_eq!(
        dry_rows(
            &api,
            &prefix,
            "demo-legacy",
            &InstallOpts {
                quiet: true,
                ..InstallOpts::default()
            },
        )
        .unwrap()
        .0
        .map(|r| r.name),
        None
    );
    assert_eq!(
        install(
            &api,
            &prefix,
            "demo-legacy",
            &InstallOpts {
                quiet: true,
                ..InstallOpts::default()
            },
        )
        .unwrap(),
        "1.0"
    );
    assert_eq!(
        prefix.installed_one(Kind::Formula, "dep").unwrap().version,
        "2"
    );
    let demo_inst = prefix.installed_one(Kind::Formula, "demo").unwrap();
    assert!(super::super::commands::requested(&prefix, &demo_inst).unwrap());

    // An outdated keg reached via alias stays gated as top in plain preflight,
    // and outdated_linked_formula resolves it when opt-linked.
    remove_owned_opt_link(&prefix, "demo").unwrap();
    let _ = std::fs::remove_file(prefix.bin().join("demo"));
    std::fs::remove_dir_all(prefix.keg("demo", "1.0")).unwrap();
    let old_demo = prefix.keg("demo", "0.9");
    std::fs::create_dir_all(old_demo.join("bin")).unwrap();
    std::fs::write(old_demo.join("bin/demo"), b"old").unwrap();
    prefix
        .write_receipt(
            &old_demo,
            &[
                ("installed_by", "abrew"),
                ("name", "demo"),
                ("version", "0.9"),
                ("installed_on_request", "true"),
            ],
        )
        .unwrap();
    assert!(matches!(
        preflight_plan(&api, &prefix, "demo-alias", &InstallOpts::default()),
        Err(InstallError::Unsupported(_))
    ));
    point_opt(&prefix, "demo", "0.9").unwrap();
    let (resolved, have) =
        super::super::commands::outdated_linked_formula(&prefix, &api, "demo-alias").unwrap();
    assert_eq!(resolved.name, "demo");
    assert_eq!(have, "0.9");
}

#[test]
#[cfg(unix)]
fn pinned_outdated_dependency_blocks_install_like_brew() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    // dep installed at 2 (latest 3) and pinned there.
    let dep_keg = prefix.keg("dep", "2");
    std::fs::create_dir_all(&dep_keg).unwrap();
    prefix
        .write_receipt(
            &dep_keg,
            &[("installed_by", "abrew"), ("name", "dep"), ("version", "2")],
        )
        .unwrap();
    prefix.pin("dep").unwrap();
    cache_formula(
        &prefix,
        0,
        "file:///nonexistent/demo.tar.gz",
        "abc",
        ":any_skip_relocation",
        &["dep"],
    );
    std::fs::write(
        prefix.cache().join("api/formula-dep.json"),
        br#"{"name":"dep","versions":{"stable":"3"}}"#,
    )
    .unwrap();
    let api = Api::new(prefix.clone());
    let err = install(&api, &prefix, "demo", &InstallOpts::default()).unwrap_err();
    assert_eq!(
        err.message(),
        "Error: demo: You must `brew unpin dep` as installing demo requires the latest version of pinned dependencies."
    );
    // Brew never sanity-errors a dry run, and
    // --ignore-dependencies skips the check entirely (both
    // then fail later for the missing bottle file, if at all).
    let dry = install(
        &api,
        &prefix,
        "demo",
        &InstallOpts {
            dry_run: true,
            ..InstallOpts::default()
        },
    );
    assert!(dry.is_ok(), "dry run: {dry:?}");
    let ignoring = install(
        &api,
        &prefix,
        "demo",
        &InstallOpts {
            ignore_dependencies: true,
            ..InstallOpts::default()
        },
    );
    assert!(
        !format!("{ignoring:?}").contains("unpin"),
        "ignore-dependencies: {ignoring:?}"
    );
}

#[test]
#[cfg(unix)]
fn bottle_install_runs_revised_keg_and_preserves_foreign_files() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let archive = tmp.path().join("demo.tar.gz");
    std::fs::write(
        &archive,
        gzip_bytes(&tar_bytes(&[(
            "demo/1.0_2/bin/demo",
            b'0',
            "",
            b"#!/bin/sh\nprintf 'ready\\n'\n",
            0o755,
        )])),
    )
    .unwrap();
    let url = format!("file://{}", archive.display());
    let sha = fetch::sha256_file(&archive).unwrap();
    cache_formula(&prefix, 2, &url, &sha, ":any_skip_relocation", &[]);
    let api = Api::new(prefix.clone());
    assert_eq!(
        install(&api, &prefix, "demo", &InstallOpts::default()).unwrap(),
        "1.0_2"
    );
    let output = aterm_uds::spawnfd::command(prefix.bin().join("demo"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"ready\n");
    assert_eq!(
        prefix.installed_one(Kind::Formula, "demo").unwrap().version,
        "1.0_2"
    );

    let archive3 = tmp.path().join("demo3.tar.gz");
    std::fs::write(
        &archive3,
        gzip_bytes(&tar_bytes(&[(
            "demo/1.0_3/bin/demo",
            b'0',
            "",
            b"#!/bin/sh\nprintf 'ready3\\n'\n",
            0o755,
        )])),
    )
    .unwrap();
    let url3 = format!("file://{}", archive3.display());
    let sha3 = fetch::sha256_file(&archive3).unwrap();
    cache_formula(&prefix, 3, &url3, &sha3, ":any_skip_relocation", &[]);
    assert!(matches!(
        install(&api, &prefix, "demo", &InstallOpts::default()),
        Err(InstallError::Unsupported(_))
    ));
    assert!(matches!(
        install(
            &api,
            &prefix,
            "demo",
            &InstallOpts {
                force: true,
                ..InstallOpts::default()
            }
        ),
        Err(InstallError::Unsupported(_))
    ));
    assert_eq!(
        aterm_uds::spawnfd::command(prefix.bin().join("demo"))
            .output()
            .unwrap()
            .stdout,
        b"ready\n"
    );
    assert_eq!(
        install(
            &api,
            &prefix,
            "demo",
            &InstallOpts {
                upgrade: true,
                ..InstallOpts::default()
            }
        )
        .unwrap(),
        "1.0_3"
    );
    assert_eq!(
        prefix.installed_one(Kind::Formula, "demo").unwrap().version,
        "1.0_3"
    );
    assert_eq!(
        aterm_uds::spawnfd::command(prefix.bin().join("demo"))
            .output()
            .unwrap()
            .stdout,
        b"ready3\n"
    );
    assert_eq!(
        install(
            &api,
            &prefix,
            "demo",
            &InstallOpts {
                force: true,
                reinstall: true,
                ..InstallOpts::default()
            }
        )
        .unwrap(),
        "1.0_3"
    );
    assert_eq!(
        aterm_uds::spawnfd::command(prefix.bin().join("demo"))
            .output()
            .unwrap()
            .stdout,
        b"ready3\n"
    );
    assert!(
        std::fs::symlink_metadata(prefix.cellar().join(".abrew-reinstall-demo-1.0_3")).is_err()
    );

    let foreign = prefix.keg("demo", "personal");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("notes"), b"keep").unwrap();
    let other = prefix.keg("other", "1");
    std::fs::create_dir_all(other.join("bin")).unwrap();
    std::fs::write(other.join("bin/other"), b"other").unwrap();
    prefix
        .write_receipt(
            &other,
            &[
                ("name", "other"),
                ("version", "1"),
                ("installed_by", "abrew"),
            ],
        )
        .unwrap();
    symlink_file(&other.join("bin/other"), &prefix.bin().join("other")).unwrap();
    std::fs::remove_file(prefix.bin().join("demo")).unwrap();
    symlink_file(Path::new("../owner/tool"), &prefix.bin().join("demo")).unwrap();
    let foreign_link = std::fs::read_link(prefix.bin().join("demo")).unwrap();
    assert!(link_keg(&prefix, &prefix.keg("demo", "1.0_2"), true).is_err());
    assert_eq!(
        std::fs::read_link(prefix.bin().join("demo")).unwrap(),
        foreign_link
    );
    uninstall(&api, &prefix, "demo", true, true, false).unwrap();
    assert!(!prefix.keg("demo", "1.0_2").exists());
    assert_eq!(std::fs::read(foreign.join("notes")).unwrap(), b"keep");
    assert_eq!(
        std::fs::read_link(prefix.bin().join("demo")).unwrap(),
        foreign_link
    );
    assert!(prefix.bin().join("other").exists());
}

/// A placeholder dylib's install name, loads and run paths become the
/// `install_name_tool` arguments that move them to this prefix, each name
/// once however many slices carry it; `@loader_path` and libSystem stay.
#[cfg(target_os = "macos")]
#[test]
fn relocation_moves_every_placeholder_name_the_load_commands_carry() {
    use crate::macho::fixtures::{
        ID_DYLIB, LOAD_DYLIB, dylib_command, fat, other_command, rpath_command, thin,
    };
    let placeholder = |id: &str| {
        thin(
            &[
                dylib_command(ID_DYLIB, id, true),
                other_command(0x1B, 24, true),
                dylib_command(
                    LOAD_DYLIB,
                    "@@HOMEBREW_CELLAR@@/xz/1/lib/liblzma.5.dylib",
                    true,
                ),
                rpath_command("@@HOMEBREW_PREFIX@@/lib", true),
                dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", true),
                rpath_command("@loader_path/../lib", true),
            ],
            true,
        )
    };
    let relocate = |name: &str| -> Option<String> {
        [
            ("@@HOMEBREW_CELLAR@@", "/p/Cellar"),
            ("@@HOMEBREW_PREFIX@@", "/p"),
        ]
        .iter()
        .find_map(|(old, new)| name.strip_prefix(old).map(|rest| format!("{new}{rest}")))
    };
    let id = "@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib";
    let want = [
        "-id",
        "/p/opt/x/lib/libx.1.dylib",
        "-change",
        "@@HOMEBREW_CELLAR@@/xz/1/lib/liblzma.5.dylib",
        "/p/Cellar/xz/1/lib/liblzma.5.dylib",
        "-rpath",
        "@@HOMEBREW_PREFIX@@/lib",
        "/p/lib",
    ];
    for bytes in [
        placeholder(id),
        fat(&[placeholder(id), placeholder(id)], false),
    ] {
        let names = crate::macho::load_names(&bytes).unwrap();
        assert_eq!(relocation_args(&names, relocate).unwrap(), want);
    }
    let system = thin(
        &[dylib_command(
            LOAD_DYLIB,
            "/usr/lib/libSystem.B.dylib",
            true,
        )],
        true,
    );
    let names = crate::macho::load_names(&system).unwrap();
    assert!(relocation_args(&names, relocate).unwrap().is_empty());
    let split = fat(
        &[
            placeholder(id),
            placeholder("@@HOMEBREW_PREFIX@@/opt/y/lib/liby.dylib"),
        ],
        false,
    );
    let names = crate::macho::load_names(&split).unwrap();
    let refused = relocation_args(&names, relocate).unwrap_err();
    assert!(refused.contains("2 different install names"), "{refused}");
    assert_eq!(
        replace_bytes(
            b"a@@HOMEBREW_PREFIX@@/b@@HOMEBREW_PREFIX@@",
            b"@@HOMEBREW_PREFIX@@",
            b"/p"
        ),
        b"a/p/b/p".to_vec()
    );
}

/// `:any` relocation runs the developer directory's own
/// `install_name_tool`, never the `/usr/bin` shim, which raises Apple's
/// install dialog on a Mac without the developer tools: with none, a
/// bottle whose Mach-O carries a placeholder name is refused as unsupported
/// before anything runs, its bytes untouched. NEGATIVE CONTROL:
/// with a tool given, that tool runs on the file with the names'
/// arguments (a stand-in that records them and refuses, so nothing is
/// signed), and a bottle with no placeholder Mach-O needs no tool at all.
#[cfg(target_os = "macos")]
#[test]
fn relocation_without_the_developer_tools_is_refused_before_running_anything() {
    use crate::macho::fixtures::{ID_DYLIB, LOAD_DYLIB, dylib_command, thin};
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let f = Formula {
        name: "demo".into(),
        ..Formula::default()
    };
    let staged = tmp.path().join("staged");
    let lib = staged.join("lib/libx.1.dylib");
    std::fs::create_dir_all(lib.parent().unwrap()).unwrap();
    let bytes = thin(
        &[
            dylib_command(ID_DYLIB, "@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib", true),
            dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", true),
        ],
        true,
    );
    std::fs::write(&lib, &bytes).unwrap();
    match relocate_staged(&prefix, &f, &staged, None) {
        Err(InstallError::Unsupported(why)) => assert!(
            why.contains("needs install_name_tool, and no developer tools are installed"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&lib).unwrap(), bytes, "nothing changed");

    let argv = tmp.path().join("argv");
    let stand_in = tmp.path().join("bin/install_name_tool");
    std::fs::create_dir_all(stand_in.parent().unwrap()).unwrap();
    std::fs::write(
        &stand_in,
        format!(
            "#!/bin/sh\necho \"$@\" > '{}'\necho 'stand-in refuses' >&2\nexit 1\n",
            argv.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    match relocate_staged(&prefix, &f, &staged, Some(&stand_in)) {
        Err(InstallError::Unsupported(why)) => {
            assert!(why.contains("install_name_tool: stand-in refuses"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    let root = prefix.root.display();
    assert_eq!(
        std::fs::read_to_string(&argv).expect("the given tool ran"),
        format!("-id {root}/opt/x/lib/libx.1.dylib {}\n", lib.display())
    );

    std::fs::write(
        &lib,
        thin(
            &[dylib_command(
                LOAD_DYLIB,
                "/usr/lib/libSystem.B.dylib",
                true,
            )],
            true,
        ),
    )
    .unwrap();
    assert!(relocate_staged(&prefix, &f, &staged, None).is_ok());
}

#[test]
fn the_leftover_scan_reads_load_commands_without_otool() {
    use crate::macho::fixtures::{
        ID_DYLIB, LOAD_DYLIB, archive, dylib_command, dylib_command32, fat, thin, thin32,
    };
    let p = Path::new("/k/bin/tool");
    let homebrew = thin(
        &[
            dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", true),
            dylib_command(LOAD_DYLIB, "/opt/homebrew/opt/xz/lib/liblzma.5.dylib", true),
        ],
        true,
    );
    assert_eq!(
        macos_leftovers(p, &homebrew),
        ["/k/bin/tool -> /opt/homebrew/opt/xz/lib/liblzma.5.dylib"]
    );
    assert_eq!(
        macos_leftovers(p, &fat(&[homebrew.clone(), homebrew.clone()], true)),
        ["/k/bin/tool -> /opt/homebrew/opt/xz/lib/liblzma.5.dylib"],
        "a universal file's slices are listed once"
    );
    let id = thin(
        &[dylib_command(
            ID_DYLIB,
            "/usr/local/opt/x/lib/libx.dylib",
            false,
        )],
        false,
    );
    assert_eq!(
        macos_leftovers(p, &id),
        ["/k/bin/tool -> /usr/local/opt/x/lib/libx.dylib"]
    );
    let watch = thin32(
        &[dylib_command32(
            LOAD_DYLIB,
            "/opt/homebrew/opt/xz/lib/liblzma.5.dylib",
            false,
        )],
        false,
    );
    assert_eq!(
        macos_leftovers(p, &watch),
        ["/k/bin/tool -> /opt/homebrew/opt/xz/lib/liblzma.5.dylib"],
        "a 32-bit Mach-O is read too"
    );
    let mut truncated = homebrew.clone();
    truncated.truncate(40);
    let listed = macos_leftovers(p, &truncated);
    assert!(
        listed.len() == 1
            && listed[0].starts_with("/k/bin/tool: its load commands could not be checked ("),
        "{listed:?}"
    );
    assert!(
        macos_leftovers(p, &fat(&[archive(), archive()], false)).is_empty(),
        "a universal static archive loads nothing"
    );
    let clean = thin(
        &[dylib_command(
            LOAD_DYLIB,
            "/usr/lib/libSystem.B.dylib",
            true,
        )],
        true,
    );
    assert!(macos_leftovers(p, &clean).is_empty());
    assert!(macos_leftovers(p, b"#!/bin/sh\necho /opt/homebrew/bin\n").is_empty());
    let class = [0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52, 0, 0];
    assert!(crate::macho::is_java_class(&class));
    assert!(macos_leftovers(p, &class).is_empty());
    assert!(!crate::macho::is_java_class(&fat(&[clean], false)));
}

#[test]
fn source_and_relocation_recipes_fall_back_before_dependencies() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    cache_formula(
        &prefix,
        0,
        "unused",
        &"0".repeat(64),
        "/opt/homebrew/Cellar",
        &[],
    );
    let api = Api::new(prefix.clone());
    assert!(matches!(
        install(&api, &prefix, "demo", &InstallOpts::default()),
        Err(InstallError::Unsupported(_))
    ));
    assert!(!prefix.cellar().exists());
    assert!(!prefix.cellar().exists());
}

#[test]
#[cfg(unix)]
fn publication_conflicts_and_link_failures_leave_no_installed_marker() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let staged = tmp.path().join("stage");
    std::fs::create_dir(&staged).unwrap();
    std::fs::write(staged.join("a"), b"a").unwrap();
    std::fs::write(staged.join("b"), b"b").unwrap();
    let live = prefix.keg("demo", "1");
    let receipt = [
        ("name", "demo"),
        ("version", "1"),
        ("installed_by", "abrew"),
    ];
    prefix.write_receipt(&staged, &receipt).unwrap();
    let links = [
        InstallLink {
            source: live.join("a"),
            destination: prefix.bin().join("a"),
            directory: false,
        },
        InstallLink {
            source: live.join("b"),
            destination: prefix.bin().join("b"),
            directory: false,
        },
    ];
    std::fs::write(&links[1].destination, b"owner").unwrap();
    assert!(publish_payload(&prefix, &staged, &live, &links).is_err());
    assert!(!live.exists());
    assert_eq!(std::fs::read(&links[1].destination).unwrap(), b"owner");
    assert!(!links[0].destination.exists());
    std::fs::remove_file(&links[1].destination).unwrap();

    let mut calls = 0;
    let error = publish_payload_with(&prefix, &staged, &live, &links, None, None, |link| {
        calls += 1;
        assert!(
            prefix.installed(Kind::Formula).is_empty(),
            "receipt preceded successful placement"
        );
        if calls == 2 {
            return Err("injected symlink refusal".to_string());
        }
        symlink_file(&link.source, &link.destination)
    })
    .unwrap_err();
    assert_eq!(
        error,
        InstallError::Failed("injected symlink refusal".to_string())
    );
    assert_eq!(calls, 2);
    assert!(prefix.installed(Kind::Formula).is_empty());
    assert!(!live.exists());
    assert!(
        links
            .iter()
            .all(|link| std::fs::symlink_metadata(&link.destination).is_err())
    );
    assert!(staged.exists());

    prefix.write_receipt(&staged, &receipt).unwrap();
    publish_payload(&prefix, &staged, &live, &links).unwrap();
    assert_eq!(
        prefix.installed_one(Kind::Formula, "demo").unwrap().version,
        "1"
    );
    assert_eq!(std::fs::read(&links[0].destination).unwrap(), b"a");

    let next = prefix.keg("demo", "2");
    let upgrade = tmp.path().join("upgrade");
    std::fs::create_dir(&upgrade).unwrap();
    for name in ["a", "b"] {
        std::fs::write(upgrade.join(name), b"new").unwrap();
    }
    let next_receipt = [
        ("name", "demo"),
        ("version", "2"),
        ("installed_by", "abrew"),
    ];
    prefix.write_receipt(&upgrade, &next_receipt).unwrap();
    let next_links: Vec<_> = links
        .iter()
        .map(|link| InstallLink {
            source: next.join(link.source.file_name().unwrap()),
            destination: link.destination.clone(),
            directory: false,
        })
        .collect();
    let root = prefix.cellar().join("demo");
    let personal = root.join("personal");
    std::fs::create_dir(&personal).unwrap();
    std::fs::write(personal.join("b"), b"personal").unwrap();
    std::fs::remove_file(&links[1].destination).unwrap();
    symlink_file(&personal.join("b"), &links[1].destination).unwrap();
    assert!(publish_payload_replacing(&prefix, &upgrade, &next, &next_links, &root).is_err());
    assert!(!next.exists());
    assert_eq!(std::fs::read(&links[1].destination).unwrap(), b"personal");
    std::fs::remove_file(&links[1].destination).unwrap();
    symlink_file(&links[1].source, &links[1].destination).unwrap();

    let saved = tmp.path().join("saved-link");
    let mut calls = 0;
    assert!(
        publish_payload_with(
            &prefix,
            &upgrade,
            &next,
            &next_links,
            Some(&root),
            None,
            |link| {
                calls += 1;
                symlink_file(&link.source, &link.destination)?;
                if calls == 1 {
                    std::fs::rename(&links[1].destination, &saved).unwrap();
                    std::fs::write(&links[1].destination, b"replacement owner").unwrap();
                }
                Ok(())
            }
        )
        .is_err()
    );
    assert_eq!(calls, 1);
    assert_eq!(
        std::fs::read(&links[1].destination).unwrap(),
        b"replacement owner"
    );
    assert_eq!(
        std::fs::read_link(&links[0].destination).unwrap(),
        links[0].source
    );
    assert!(live.exists());
    assert!(!next.exists());
    std::fs::remove_file(&links[1].destination).unwrap();
    std::fs::rename(saved, &links[1].destination).unwrap();
    prefix.write_receipt(&upgrade, &next_receipt).unwrap();
    publish_payload_replacing(&prefix, &upgrade, &next, &next_links, &root).unwrap();
    assert_eq!(std::fs::read(&links[0].destination).unwrap(), b"new");
    assert!(live.exists(), "upgrades retain the preceding version");
    assert_eq!(
        prefix.installed_one(Kind::Formula, "demo").unwrap().version,
        "2"
    );

    let reinstall = tmp.path().join("reinstall");
    std::fs::create_dir(&reinstall).unwrap();
    for name in ["a", "b"] {
        std::fs::write(reinstall.join(name), b"reinstalled").unwrap();
    }
    prefix.write_receipt(&reinstall, &next_receipt).unwrap();
    let saved_receipt = std::fs::read(next.join("INSTALL_RECEIPT.json")).unwrap();
    let mut calls = 0;
    assert!(
        reinstall_payload_with(&prefix, &reinstall, &next, &next_links, &root, |link| {
            calls += 1;
            if calls == 2 {
                return Err("injected reinstall refusal".to_string());
            }
            symlink_file(&link.source, &link.destination)
        })
        .is_err()
    );
    assert_eq!(calls, 2);
    assert_eq!(
        std::fs::read(next.join("INSTALL_RECEIPT.json")).unwrap(),
        saved_receipt
    );
    for link in &next_links {
        assert_eq!(std::fs::read_link(&link.destination).unwrap(), link.source);
        assert_eq!(std::fs::read(&link.destination).unwrap(), b"new");
    }
    prefix.write_receipt(&reinstall, &next_receipt).unwrap();
    reinstall_payload_with(&prefix, &reinstall, &next, &next_links, &root, |link| {
        symlink_file(&link.source, &link.destination)
    })
    .unwrap();
    assert_eq!(
        std::fs::read(&next_links[0].destination).unwrap(),
        b"reinstalled"
    );
    assert!(live.exists());
    assert_eq!(std::fs::read(personal.join("b")).unwrap(), b"personal");
}

#[test]
#[cfg(unix)]
fn owned_cask_versions_with_commas_reinstall_through_the_saved_receipt() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let prefix = Prefix::at(tmp.path());
    prefix.ensure().unwrap();
    let version = "4.2.1,20260531.1";
    let live = prefix.cask_dir("demo", version);
    std::fs::create_dir_all(&live).unwrap();
    std::fs::write(live.join("tool"), b"old cask").unwrap();
    let receipt = [
        ("token", "demo"),
        ("version", version),
        ("installed_by", "abrew"),
    ];
    prefix.write_receipt(&live, &receipt).unwrap();
    let links = [InstallLink {
        source: live.join("tool"),
        destination: prefix.bin().join("demo"),
        directory: false,
    }];
    symlink_file(&links[0].source, &links[0].destination).unwrap();
    let root = prefix.caskroom().join("demo");
    assert!(replacement_target_owned(
        &prefix,
        &links[0].destination,
        &links[0].source,
        &root
    ));
    let stage = tmp.path().join("stage");
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(stage.join("tool"), b"new cask").unwrap();
    prefix.write_receipt(&stage, &receipt).unwrap();
    publish_payload_reinstall(&prefix, &stage, &live, &links, &root).unwrap();
    assert_eq!(std::fs::read(&links[0].destination).unwrap(), b"new cask");
    assert!(super::super::prefix::owned_receipt(
        &live,
        Kind::Cask,
        "demo",
        version
    ));
}

#[test]
fn upgrade_request_inputs_preserve_roots_and_refuse_unknown_status() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let prefix = Prefix::at(tmp.path());
    for (name, requested) in [("app", "true"), ("dep", "false")] {
        let keg = prefix.keg(name, "1");
        std::fs::create_dir_all(&keg).unwrap();
        prefix
            .write_receipt(
                &keg,
                &[
                    ("name", name),
                    ("version", "1"),
                    ("installed_by", "abrew"),
                    ("installed_on_request", requested),
                ],
            )
            .unwrap();
    }
    let prior = prior_requests(&prefix, ["app", "dep", "new"].into_iter()).unwrap();
    assert_eq!(prior.get("app"), Some(&true));
    assert_eq!(prior.get("dep"), Some(&false));
    assert!(!prior.contains_key("new"));
    let old = prefix.keg("dep", "0");
    std::fs::create_dir(&old).unwrap();
    prefix
        .write_receipt(
            &old,
            &[
                ("name", "dep"),
                ("version", "0"),
                ("installed_by", "abrew"),
                ("installed_on_request", "true"),
            ],
        )
        .unwrap();
    assert_eq!(
        prior_requests(&prefix, ["dep"].into_iter())
            .unwrap()
            .get("dep"),
        Some(&true)
    );
    prefix
        .write_receipt(
            &old,
            &[("name", "dep"), ("version", "0"), ("installed_by", "abrew")],
        )
        .unwrap();
    assert!(prior_requests(&prefix, ["dep"].into_iter()).is_err());
    assert!(prefix.keg("dep", "1").exists());
    assert!(old.exists());
}

#[test]
#[cfg(unix)]
fn plain_uninstall_removes_only_default_keg_and_refuses_pinned() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    for v in ["1", "2"] {
        let keg = prefix.keg("demo", v);
        std::fs::create_dir_all(keg.join("bin")).unwrap();
        std::fs::write(keg.join("bin/tool"), b"x").unwrap();
        prefix
            .write_receipt(
                &keg,
                &[("name", "demo"), ("version", v), ("installed_by", "abrew")],
            )
            .unwrap();
    }
    crate::abrew::prefix::symlink_dir(&prefix.keg("demo", "2"), &prefix.opt("demo")).unwrap();
    std::fs::create_dir_all(prefix.bin()).unwrap();
    crate::abrew::prefix::symlink_file(
        &prefix.keg("demo", "2").join("bin/tool"),
        &prefix.bin().join("tool"),
    )
    .unwrap();
    let api = Api::new(prefix.clone());
    uninstall(&api, &prefix, "demo", true, false, false).unwrap();
    assert!(!prefix.keg("demo", "2").exists(), "default keg removed");
    assert!(prefix.keg("demo", "1").exists(), "other version kept");
    assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
    assert!(std::fs::symlink_metadata(prefix.opt("demo")).is_err());
    prefix.pin("demo").unwrap();
    let err = uninstall(&api, &prefix, "demo", true, false, false).unwrap_err();
    assert!(matches!(err, InstallError::Failed(m) if m.contains("is pinned")));
    assert!(prefix.keg("demo", "1").exists());
    assert!(prefix.pinned_version("demo").is_some());
    uninstall(&api, &prefix, "demo", true, true, false).unwrap();
    assert!(!prefix.keg("demo", "1").exists());
    assert!(prefix.pinned_version("demo").is_none());
}

#[test]
fn unknown_dependency_metadata_refuses_removal_unless_explicitly_ignored() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    for name in ["demo", "other"] {
        let keg = prefix.keg(name, "1");
        std::fs::create_dir_all(&keg).unwrap();
        prefix
            .write_receipt(
                &keg,
                &[("name", name), ("version", "1"), ("installed_by", "abrew")],
            )
            .unwrap();
    }
    let cache = prefix.cache().join("api");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("formula-other.json"), b"{}").unwrap();
    let api = Api::new(prefix.clone());
    assert!(uninstall(&api, &prefix, "demo", false, true, false).is_err());
    assert!(prefix.keg("demo", "1").exists());
    uninstall(&api, &prefix, "demo", true, true, false).unwrap();
    assert!(!prefix.keg("demo", "1").exists());
    let keg = prefix.keg("demo", "1");
    std::fs::create_dir_all(&keg).unwrap();
    prefix
        .write_receipt(
            &keg,
            &[
                ("name", "demo"),
                ("version", "1"),
                ("installed_by", "abrew"),
            ],
        )
        .unwrap();
    std::fs::write(
        cache.join("formula-other.json"),
        br#"{"name":"other","versions":{"stable":"1"},"revision":0,"dependencies":[]}"#,
    )
    .unwrap();
    uninstall(&api, &prefix, "demo", false, true, false).unwrap();
    assert!(!keg.exists());
}

#[test]
#[cfg(unix)]
fn bottle_links_are_admitted_against_the_subtree_that_is_published() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    for (label, target, allowed) in [
        ("ordinary", None, true),
        ("local-link", Some("tool"), true),
        ("subtree-escape", Some("../../../outside"), false),
    ] {
        let mut members = vec![
            ("outside", b'0', "", b"outside bytes".as_slice(), 0o644),
            (
                "demo/1.0/bin/tool",
                b'0',
                "",
                b"tool bytes".as_slice(),
                0o644,
            ),
        ];
        if let Some(target) = target {
            members.push(("demo/1.0/bin/alias", b'2', target, b"".as_slice(), 0o777));
        }
        let archive = tmp.path().join(format!("{label}.tar"));
        std::fs::write(&archive, tar_bytes(&members)).unwrap();
        let staging = tmp.path().join(label);
        fetch::untar(&archive, &staging).unwrap();
        if let Some(target) = target {
            assert_eq!(
                std::fs::read_link(staging.join("demo/1.0/bin/alias")).unwrap(),
                Path::new(target)
            );
        }
        let selected = selected_bottle_payload(&staging, "demo", "1.0");
        assert_eq!(selected.is_ok(), allowed, "{label}");
        assert_eq!(
            std::fs::read(staging.join("outside")).unwrap(),
            b"outside bytes"
        );
        assert_eq!(
            std::fs::read(staging.join("demo/1.0/bin/tool")).unwrap(),
            b"tool bytes"
        );
    }
    let staging = tmp.path().join("aliased-name");
    std::fs::create_dir_all(staging.join("other/1.0")).unwrap();
    symlink_dir(Path::new("other"), &staging.join("demo")).unwrap();
    assert!(selected_bottle_payload(&staging, "demo", "1.0").is_err());
    assert_eq!(
        std::fs::read_link(staging.join("demo")).unwrap(),
        Path::new("other")
    );
}
