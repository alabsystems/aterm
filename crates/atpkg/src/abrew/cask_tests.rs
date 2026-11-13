// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(unix)]
use crate::extract::fixtures::{ZipMember, tar_bytes, zip_bytes};

#[test]
fn zap_gates_and_glob_match_like_brew() {
    assert!(segment_matches("com.example.*", "com.example.app"));
    assert!(segment_matches("*.plist", "x.plist"));
    assert!(!segment_matches("*.plist", ".hidden.plist"));
    assert!(segment_matches(".hidden*", ".hidden.plist"));
    assert!(segment_matches("firefo?", "firefox"));
    assert!(segment_matches("[a-c]pp", "app"));
    assert!(!segment_matches("[!a-c]pp", "app"));
    assert_eq!(
        brace_expand("/x/olivevideoeditor.org{/Olive,}"),
        ["/x/olivevideoeditor.org/Olive", "/x/olivevideoeditor.org"]
    );
    assert_eq!(
        brace_expand("/Adobe{/CEP{/extensions,},}"),
        ["/Adobe/CEP/extensions", "/Adobe/CEP", "/Adobe"]
    );
    assert_eq!(brace_expand("/literal{a}"), ["/literala"]);
    assert_eq!(brace_expand("/plain/path"), ["/plain/path"]);

    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(tmp.path());
    let mut cask = Cask {
        token: "demo".to_string(),
        zap_present: true,
        zap_trash: vec!["~/Library/Caches/demo".to_string()],
        ..Cask::default()
    };
    // Not installed: failed, not a delegate.
    assert!(matches!(
        zap(&prefix, "demo", &cask, false, false),
        Err(InstallError::Failed(_))
    ));
    // A sudo-bound delete directive delegates before effects,
    // even before the installed check can matter.
    cask.zap_delete = vec!["/Library/demo".to_string()];
    assert!(matches!(
        zap(&prefix, "demo", &cask, false, false),
        Err(InstallError::Unsupported(_))
    ));
    cask.zap_delete.clear();
    cask.uninstall_unsupported = true;
    assert!(matches!(
        zap(&prefix, "demo", &cask, false, false),
        Err(InstallError::Unsupported(_))
    ));
}

#[cfg(unix)]
#[test]
fn cask_link_round_trip_and_conflict_pre_scan() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let staged = prefix.cask_dir("demo", "1");
    std::fs::create_dir_all(staged.join("Demo.app")).unwrap();
    std::fs::write(staged.join("demotool"), b"tool").unwrap();
    let cask = Cask {
        token: "demo".to_string(),
        version: "1".to_string(),
        artifacts: vec![
            Artifact::App("Demo.app".to_string(), None),
            Artifact::Binary("demotool".to_string(), None),
        ],
        ..Cask::default()
    };
    // Only the binary is linkable; the app never counts.
    let items = linkable_artifacts(&prefix, &cask, "1").unwrap();
    assert_eq!(items.len(), 1);
    let target = items[0].target.clone();
    link_cask(&prefix, "demo", &items, false, false, false).unwrap();
    assert!(target.is_symlink());
    // Already linked is a no-op success.
    link_cask(&prefix, "demo", &items, false, false, false).unwrap();
    unlink_cask(&prefix, "demo", &items, false).unwrap();
    assert!(!target.exists() && !target.is_symlink());
    // A regular file conflicts and the pre-scan mutates nothing.
    std::fs::write(&target, b"mine").unwrap();
    assert!(matches!(
        link_cask(&prefix, "demo", &items, false, false, false),
        Err(InstallError::Failed(m)) if m.contains("brew link --cask --overwrite demo")
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"mine");
    // --overwrite replaces it; unlink never removes foreign files.
    link_cask(&prefix, "demo", &items, false, true, false).unwrap();
    assert!(target.is_symlink());
    unlink_cask(&prefix, "demo", &items, false).unwrap();
    std::fs::write(&target, b"mine").unwrap();
    unlink_cask(&prefix, "demo", &items, false).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"mine");
}

/// Unix only: abrew tells a cask directory's identity only there
/// (`install::same_path_identity`), so elsewhere every removal is refused.
#[cfg(unix)]
#[test]
fn uninstall_runs_stanza_trash_and_deferred_rmdir() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let payload = prefix.cask_dir("demo", "1");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("tool"), b"owned payload").unwrap();
    prefix
        .write_receipt(
            &payload,
            &[
                ("token", "demo"),
                ("version", "1"),
                ("installed_by", "abrew"),
            ],
        )
        .unwrap();
    // Deferred rmdir target: only a .DS_Store inside.
    let leftover = tmp.path().join("leftover");
    std::fs::create_dir(&leftover).unwrap();
    std::fs::write(leftover.join(".DS_Store"), b"junk").unwrap();
    // The API `$APPDIR` placeholder resolves into the prefix.
    let app_leftover = prefix.applications().join("demo-support");
    std::fs::create_dir(&app_leftover).unwrap();
    let cask = Cask {
        token: "demo".to_string(),
        uninstall_trash: vec![tmp.path().join("absent-trash-target").display().to_string()],
        uninstall_rmdir: vec![
            leftover.display().to_string(),
            "$APPDIR/demo-support".to_string(),
        ],
        ..Cask::default()
    };
    let directories = [prefix.applications(), prefix.bin()];
    // Dry run: nothing is touched.
    uninstall_from(
        &prefix,
        "demo",
        true,
        &directories,
        Some(&cask),
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(payload.exists());
    assert!(leftover.is_dir());
    uninstall_from(
        &prefix,
        "demo",
        false,
        &directories,
        Some(&cask),
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(!payload.exists());
    assert!(
        !leftover.exists(),
        "deferred rmdir must remove {leftover:?}"
    );
    assert!(
        !app_leftover.exists(),
        "$APPDIR rmdir must remove {app_leftover:?}"
    );
}

#[test]
fn generic_targets_map_only_into_the_prefix() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(tmp.path());
    assert_eq!(
        map_generic_target(&prefix, "$HOMEBREW_PREFIX/share/x/tree"),
        Some(prefix.root.join("share/x/tree"))
    );
    assert_eq!(
        map_generic_target(&prefix, "/$HOME/Library/Services/W.workflow"),
        Some(prefix.root.join("Library/Services/W.workflow"))
    );
    assert_eq!(
        map_generic_target(&prefix, "$APPDIR/Demo.app"),
        Some(prefix.root.join("Applications/Demo.app"))
    );
    assert_eq!(map_generic_target(&prefix, "$APPDIR/../x"), None);
    assert_eq!(map_generic_target(&prefix, "~/Documents/x"), None);
    assert_eq!(map_generic_target(&prefix, "/usr/local/x"), None);
    assert_eq!(map_generic_target(&prefix, "$HOMEBREW_PREFIX/../x"), None);
    let mut library = Cask {
        token: "demo".to_string(),
        artifacts: vec![Artifact::Library(
            "Demo.qlgenerator".to_string(),
            None,
            "Library/QuickLook".to_string(),
        )],
        ..Cask::default()
    };
    assert_eq!(
        artifact_paths(&prefix, &library).unwrap()[0].1,
        prefix.root.join("Library/QuickLook/Demo.qlgenerator")
    );
    for directory in ["../outside", "/outside"] {
        library.artifacts = vec![Artifact::Library(
            "Demo.qlgenerator".to_string(),
            None,
            directory.to_string(),
        )];
        assert!(artifact_paths(&prefix, &library).is_err());
    }
    library.artifacts = vec![Artifact::Library(
        "Demo.qlgenerator".to_string(),
        Some("/outside".to_string()),
        "Library/QuickLook".to_string(),
    )];
    assert!(artifact_paths(&prefix, &library).is_err());
    let payload = tmp.path().join("payload");
    std::fs::create_dir(&payload).unwrap();
    let wrapper = Cask {
        token: "demo".to_string(),
        artifacts: vec![Artifact::Wrapper(
            "wrap".to_string(),
            "java".to_string(),
            vec!["a'b".to_string()],
            None,
        )],
        ..Cask::default()
    };
    stage_wrappers(&prefix, &wrapper, &payload, &prefix.cask_dir("demo", "1")).unwrap();
    assert_eq!(
        std::fs::read_to_string(payload.join(wrapper_rel("wrap"))).unwrap(),
        "#!/bin/sh\n# abrew command wrapper for demo\nexec 'java' 'a'\\''b' \"$@\"\n"
    );
    assert!(stage_wrappers(&prefix, &wrapper, &payload, &prefix.cask_dir("demo", "1")).is_err());
    assert!(wrapper_program(&prefix, &wrapper, "-java").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(payload.join(wrapper_rel("wrap")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        use std::os::unix::ffi::OsStringExt as _;
        let non_utf8 = Prefix::at(&PathBuf::from(std::ffi::OsString::from_vec(
            b"/captured-\xff".to_vec(),
        )));
        assert!(wrapper_program(&non_utf8, &wrapper, "java").is_err());
        let captured_link = InstallLink {
            source: non_utf8.cask_dir("demo", "1").join("tree"),
            destination: non_utf8.root.join("var/demo"),
            directory: true,
        };
        let fields = complete_artifact_receipt_fields(&non_utf8, &[captured_link]).unwrap();
        let value = aterm_json::Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, aterm_json::Value::String(value)))
                .collect(),
        );
        assert_eq!(
            recorded_artifact_paths(&non_utf8, &value).unwrap(),
            Some(BTreeSet::from([non_utf8.root.join("var/demo")]))
        );
        let aliased_payload = tmp.path().join("aliased-payload");
        let outside = tmp.path().join("owned-outside");
        std::fs::create_dir(&aliased_payload).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("wrap"), b"preserved").unwrap();
        super::super::prefix::symlink_dir(&outside, &aliased_payload.join(".abrew-wrappers"))
            .unwrap();
        assert!(
            stage_wrappers(
                &prefix,
                &wrapper,
                &aliased_payload,
                &prefix.cask_dir("demo", "1")
            )
            .is_err()
        );
        assert_eq!(std::fs::read(outside.join("wrap")).unwrap(), b"preserved");
    }
}

fn cask(url: String) -> Cask {
    Cask {
        token: "demo".to_string(),
        version: "1".to_string(),
        url,
        artifacts: vec![
            Artifact::Binary("tool".to_string(), None),
            Artifact::App("Demo.app".to_string(), None),
            Artifact::Font("fonts/F.ttf".to_string(), Some("Renamed.ttf".to_string())),
            Artifact::Wrapper(
                "demo".to_string(),
                "/Applications/Demo.app/Contents/MacOS/demo".to_string(),
                vec!["--fixed".to_string()],
                None,
            ),
        ],
        ..Cask::default()
    }
}

#[test]
#[cfg(unix)]
fn no_check_download_installs_real_artifacts_and_uninstall_preserves_unknown_versions() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let archive = tmp.path().join("demo.zip");
    std::fs::write(
        &archive,
        zip_bytes(
            &[
                ZipMember {
                    name: "tool",
                    mode: 0o100755,
                    data: b"#!/bin/sh\nprintf 'cask ready\\n'\n",
                    deflate: false,
                },
                ZipMember {
                    name: "Demo.app/Contents/Info.plist",
                    mode: 0o100644,
                    data: b"app",
                    deflate: false,
                },
                ZipMember {
                    name: "Demo.app/Contents/MacOS/demo",
                    mode: 0o100755,
                    data: b"#!/bin/sh\nprintf 'wrapped %s\\n' \"$*\"\n",
                    deflate: false,
                },
                ZipMember {
                    name: "fonts/F.ttf",
                    mode: 0o100644,
                    data: b"font",
                    deflate: false,
                },
            ],
            false,
        ),
    )
    .unwrap();
    let cask = cask(format!("file://{}", archive.display()));
    assert!(cask.sha256.is_none());
    assert!(!download_dest(&prefix, &cask).exists());
    assert_eq!(
        install(&prefix, &cask, false, false, false, false).unwrap(),
        "1"
    );
    let output = aterm_uds::spawnfd::command(prefix.bin().join("tool"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"cask ready\n");
    assert_eq!(
        std::fs::read(prefix.applications().join("Demo.app/Contents/Info.plist")).unwrap(),
        b"app"
    );
    assert_eq!(
        super::super::prefix::receipt_word(&prefix.cask_dir("demo", "1"), "installed_on_request")
            .as_deref(),
        Some("true"),
        "a named install records on-request intent"
    );
    assert_eq!(
        std::fs::read(prefix.root.join("Library/Fonts/Renamed.ttf")).unwrap(),
        b"font"
    );
    let wrapped = aterm_uds::spawnfd::command(prefix.bin().join("demo"))
        .arg("user")
        .output()
        .unwrap();
    assert!(wrapped.status.success());
    assert_eq!(wrapped.stdout, b"wrapped --fixed user\n");
    assert!(matches!(
        install(&prefix, &cask, false, true, false, false),
        Err(InstallError::Unsupported(_))
    ));
    let unknown = prefix.cask_dir("demo", "personal");
    std::fs::create_dir_all(&unknown).unwrap();
    std::fs::write(unknown.join("notes"), b"keep").unwrap();
    super::super::prefix::symlink_file(&unknown.join("notes"), &prefix.bin().join("personal"))
        .unwrap();
    uninstall(&prefix, "demo", false, None).unwrap();
    assert!(!prefix.cask_dir("demo", "1").exists());
    assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
    assert!(std::fs::symlink_metadata(prefix.applications().join("Demo.app")).is_err());
    assert!(std::fs::symlink_metadata(prefix.root.join("Library/Fonts/Renamed.ttf")).is_err());
    assert!(std::fs::symlink_metadata(prefix.bin().join("demo")).is_err());
    assert_eq!(
        std::fs::read(prefix.bin().join("personal")).unwrap(),
        b"keep"
    );
}

#[test]
fn git_raw_plan_maps_github_checkout_to_raw_blobs_only() {
    let mut c = cask("https://github.com/google/fonts.git".to_string());
    c.git_branch_path = Some(("main".into(), "ofl/abeezee".into()));
    c.artifacts = vec![
        Artifact::Font("ABeeZee-Regular.ttf".into(), None),
        Artifact::Font("ABeeZee-Italic.ttf".into(), None),
    ];
    let plan = git_raw_plan(&c).expect("raw plan");
    assert_eq!(
        plan[0].1,
        "https://raw.githubusercontent.com/google/fonts/main/ofl/abeezee/ABeeZee-Regular.ttf"
    );
    assert_eq!(plan.len(), 2);
    // Bracketed variable-font names percent-encode.
    c.artifacts = vec![Artifact::Font("42dotSans[wght].ttf".into(), None)];
    assert!(
        git_raw_plan(&c).unwrap()[0]
            .1
            .ends_with("42dotSans%5Bwght%5D.ttf")
    );
    // A cask checksum cannot cover per-file blobs.
    c.sha256 = Some("0".repeat(64));
    assert!(git_raw_plan(&c).is_none());
    c.sha256 = None;
    // Non-GitHub hosts and deeper paths delegate.
    c.url = "https://gitlab.com/google/fonts.git".into();
    assert!(git_raw_plan(&c).is_none());
    c.url = "https://github.com/google/fonts.git".into();
    c.artifacts = vec![Artifact::Font("sub/Foo.ttf".into(), None)];
    assert!(git_raw_plan(&c).is_none());
    for bad_source in ["..", ".", "/", "Foo.ttf/"] {
        c.artifacts = vec![Artifact::Font(bad_source.into(), None)];
        assert!(git_raw_plan(&c).is_none(), "{bad_source:?}");
    }
    c.artifacts = vec![Artifact::Font("ABeeZee-Regular.ttf".into(), None)];
    for (bad_branch, bad_path) in [
        ("..", "ofl/abeezee"),
        (".", "ofl/abeezee"),
        ("main", "ofl//abeezee"),
        ("main", "/ofl/abeezee"),
        ("main", "ofl/abeezee/"),
        ("main", "ofl/../secret"),
    ] {
        c.git_branch_path = Some((bad_branch.into(), bad_path.into()));
        assert!(git_raw_plan(&c).is_none(), "{bad_branch:?} {bad_path:?}");
    }
}

#[test]
fn download_basename_follows_brew_parse_rules() {
    // Plain path basename, percent-decoded.
    assert_eq!(
        parse_download_basename("https://example.com/Firefox%20159.0.dmg"),
        "Firefox 159.0.dmg"
    );
    // The last component WITH an extension wins over a
    // trailing script name via the query scan as well.
    assert_eq!(
        parse_download_basename("https://example.com/download.php?file=foo-1.0.tar.gz"),
        "foo-1.0.tar.gz"
    );
    // response-content-disposition beats both.
    assert_eq!(
        parse_download_basename(
            "https://example.com/x?response-content-disposition=attachment%3B%20filename%3DReal.zip"
        ),
        "Real.zip"
    );
    // No extension anywhere: last path segment; a bare
    // host leaves the "download" fallback.
    assert_eq!(parse_download_basename("https://example.com/get"), "get");
    assert_eq!(parse_download_basename("https://example.com/"), "download");
}

#[test]
fn autodetect_naked_admits_single_raw_file_downloads_only() {
    let font = |url: &str, source: &str| {
        let mut c = cask(url.to_string());
        c.artifacts = vec![Artifact::Font(source.to_string(), None)];
        c
    };
    assert!(autodetect_naked(&font(
        "https://example.com/Foo.ttf",
        "Foo.ttf"
    )));
    for bad_source in ["..", ".", "/", "Foo.ttf/"] {
        assert!(
            !autodetect_naked(&font("https://example.com/Foo.ttf", bad_source)),
            "{bad_source:?}"
        );
    }
    assert_eq!(
        download_basename(&font("https://example.com/download/", "Foo.ttf")),
        "download"
    );
    assert_eq!(
        download_basename(&font("https://example.com/..", "Foo.ttf")),
        "download"
    );
    assert_eq!(
        download_basename(&font(
            "https://example.com/#{version}/Foo-#{version}.ttf?dl=1#frag",
            "Foo.ttf"
        )),
        "Foo-1.ttf"
    );
    // Known archives and images are not naked.
    assert!(!autodetect_naked(&font(
        "https://example.com/Foo.zip",
        "Foo.ttf"
    )));
    assert!(!autodetect_naked(&font(
        "https://example.com/Foo.dmg",
        "Foo.ttf"
    )));
    // VCS checkouts and pkg downloads delegate.
    assert!(!autodetect_naked(&font(
        "https://github.com/example/fonts.git",
        "Foo.ttf"
    )));
    assert!(!autodetect_naked(&font(
        "https://example.com/Foo.pkg",
        "Foo.ttf"
    )));
    // One file cannot carry a nested or second source.
    assert!(!autodetect_naked(&font(
        "https://example.com/Foo.ttf",
        "dir/Foo.ttf"
    )));
    let mut two = font("https://example.com/Foo.ttf", "Foo.ttf");
    two.artifacts
        .push(Artifact::Font("Bar.ttf".to_string(), None));
    assert!(!autodetect_naked(&two));
}

#[test]
#[cfg(unix)]
fn naked_cask_stages_under_artifact_source_name() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let download = tmp.path().join("payload.bin");
    std::fs::write(&download, b"#!/bin/sh\nprintf 'naked ready\\n'\n").unwrap();
    let mut c = cask(format!("file://{}", download.display()));
    c.naked = true;
    c.artifacts = vec![Artifact::Binary("staged-name".to_string(), None)];
    assert_eq!(
        install(&prefix, &c, false, false, false, false).unwrap(),
        "1"
    );
    let output = aterm_uds::spawnfd::command(prefix.bin().join("staged-name"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"naked ready\n");
    std::fs::write(&download, b"#!/bin/sh\nprintf 'naked ready2\\n'\n").unwrap();
    c.version = "2".to_string();
    // Brew's plain cask install upgrades an outdated cask.
    assert_eq!(
        install(&prefix, &c, false, false, false, false).unwrap(),
        "2"
    );
    assert_eq!(
        install(&prefix, &c, false, false, true, false).unwrap(),
        "2"
    );
    let output = aterm_uds::spawnfd::command(prefix.bin().join("staged-name"))
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"naked ready2\n");
    std::fs::write(&download, b"#!/bin/sh\nprintf 'naked readyR\\n'\n").unwrap();
    assert_eq!(install(&prefix, &c, false, true, false, true).unwrap(), "2");
    let output = aterm_uds::spawnfd::command(prefix.bin().join("staged-name"))
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"naked readyR\n");
    assert!(std::fs::symlink_metadata(prefix.caskroom().join(".abrew-reinstall-demo-2")).is_err());
    uninstall(&prefix, "demo", false, None).unwrap();
    std::fs::write(&download, b"#!/bin/sh\nprintf 'dep ready\\n'\n").unwrap();
    c.version = "1".to_string();
    assert_eq!(
        install_inner(&prefix, &c, false, false, false, false, false).unwrap(),
        "1"
    );
    let word = |v: &str| {
        super::super::prefix::receipt_word(&prefix.cask_dir("demo", v), "installed_on_request")
    };
    assert_eq!(word("1").as_deref(), Some("false"));
    std::fs::write(&download, b"#!/bin/sh\nprintf 'dep ready3\\n'\n").unwrap();
    c.version = "3".to_string();
    assert_eq!(
        install(&prefix, &c, false, false, true, false).unwrap(),
        "3"
    );
    assert_eq!(
        word("3").as_deref(),
        Some("false"),
        "upgrade preserves intent"
    );
    uninstall(&prefix, "demo", false, None).unwrap();
    assert!(std::fs::symlink_metadata(prefix.bin().join("staged-name")).is_err());
}

#[test]
#[cfg(unix)]
fn container_nested_archive_becomes_the_payload() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let inner = zip_bytes(
        &[ZipMember {
            name: "Demo.app/Contents/MacOS/demo",
            mode: 0o100755,
            data: b"#!/bin/sh\nprintf 'container nested\\n'\n",
            deflate: false,
        }],
        false,
    );
    let archive = tmp.path().join("outer.zip");
    std::fs::write(
        &archive,
        zip_bytes(
            &[ZipMember {
                name: "wrap/Demo.app.zip",
                mode: 0o100644,
                data: &inner,
                deflate: false,
            }],
            false,
        ),
    )
    .unwrap();
    let mut c = cask(format!("file://{}", archive.display()));
    c.artifacts = vec![Artifact::App("Demo.app".to_string(), None)];
    c.nested = Some("wrap/Demo.app.zip".to_string());
    assert_eq!(
        install(&prefix, &c, false, false, false, false).unwrap(),
        "1"
    );
    let out = aterm_uds::spawnfd::command(
        prefix
            .caskroom()
            .join("demo/1/Demo.app/Contents/MacOS/demo"),
    )
    .output()
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "container nested\n");
}

#[test]
#[cfg(unix)]
fn nested_app_tar_inside_the_zip_is_extracted() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let inner = tar_bytes(&[
        ("Demo.app/Contents/Info.plist", b'0', "", b"app", 0o644),
        (
            "Demo.app/Contents/MacOS/demo",
            b'0',
            "",
            b"#!/bin/sh\nprintf 'nested ready\\n'\n",
            0o755,
        ),
    ]);
    let archive = tmp.path().join("demo.zip");
    std::fs::write(
        &archive,
        zip_bytes(
            &[ZipMember {
                name: "Demo.app.tar",
                mode: 0o100644,
                data: &inner,
                deflate: false,
            }],
            false,
        ),
    )
    .unwrap();
    let mut c = cask(format!("file://{}", archive.display()));
    c.artifacts = vec![Artifact::App("Demo.app".to_string(), None)];
    assert_eq!(
        install(&prefix, &c, false, false, false, false).unwrap(),
        "1"
    );
    let out = aterm_uds::spawnfd::command(
        prefix
            .caskroom()
            .join("demo/1/Demo.app/Contents/MacOS/demo"),
    )
    .output()
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "nested ready\n");
}

#[test]
fn extraction_structural_refusals_are_unsupported_vetting_refusals_fail() {
    assert!(matches!(
        dmg_error("x", fetch::DmgError::Attach("unsupported image".into())),
        InstallError::Unsupported(_)
    ));
    for name in ["dmg attach failed", "unsupported archive", "io: zip: "] {
        assert!(matches!(
            dmg_error(
                "x",
                fetch::DmgError::Payload(format!("abrew: dmg contains a nonregular entry: {name}"))
            ),
            InstallError::Failed(_)
        ));
    }
    assert!(matches!(
        dmg_error("x", fetch::DmgError::Detach),
        InstallError::Failed(_)
    ));
    for message in [
        "unsupported image",
        "dmg attach failed",
        "cleanup not mounted",
    ] {
        assert!(matches!(
            dmg_error("x", fetch::DmgError::Uncertain(message.into())),
            InstallError::Failed(_)
        ));
    }
    assert!(matches!(
        extraction_error(
            "empoche",
            "abrew: cannot extract /c/e.zip: io: zip: no end-of-central-directory record"
                .to_string()
        ),
        InstallError::Unsupported(_)
    ));
    assert!(matches!(
        extraction_error(
            "x",
            "abrew: unsupported archive; use brew for this format".to_string()
        ),
        InstallError::Unsupported(_)
    ));
    assert!(matches!(
        extraction_error(
            "x",
            "abrew: cannot extract /c/e.zip: rejected entry \"../evil\": Slip".to_string()
        ),
        InstallError::Failed(_)
    ));
    assert!(matches!(
        extraction_error(
            "x",
            "abrew: No space left on device (os error 28)".to_string()
        ),
        InstallError::Failed(_)
    ));
}

#[test]
fn caskroom_sourced_wrapper_rebases_onto_own_payload() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let c = cask("unused.zip".to_string());
    assert_eq!(
        wrapper_program(
            &prefix,
            &c,
            "$HOMEBREW_PREFIX/Caskroom/demo/1/staged/bin/tool"
        )
        .unwrap(),
        Some(PathBuf::from("staged/bin/tool"))
    );
    for exe in [
        "$HOMEBREW_PREFIX/Caskroom/other/1/bin/tool",
        "$HOMEBREW_PREFIX/Caskroom/demo/2/bin/tool",
        "$HOMEBREW_PREFIX/share/demo/bin/tool",
    ] {
        assert!(matches!(
            wrapper_program(&prefix, &c, exe),
            Err(InstallError::Unsupported(_))
        ));
    }
}

#[test]
fn caskroom_sourced_binary_rebases_onto_own_payload() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let mut c = cask("unused.zip".to_string());
    c.artifacts = vec![Artifact::Binary(
        "$HOMEBREW_PREFIX/Caskroom/demo/1/platform-tools/adb".to_string(),
        None,
    )];
    let paths = artifact_paths(&prefix, &c).unwrap();
    assert_eq!(
        paths,
        [(
            PathBuf::from("platform-tools/adb"),
            prefix.bin().join("adb"),
            false,
            true
        )]
    );
    c.artifacts = vec![
        Artifact::App(
            "$HOMEBREW_PREFIX/Caskroom/demo/1/Demo.app".to_string(),
            None,
        ),
        Artifact::Binary(
            "/Applications/Demo.app/Contents/MacOS/demo".to_string(),
            None,
        ),
        Artifact::Generic(
            "$HOMEBREW_PREFIX/Caskroom/demo/1/tree".to_string(),
            Some("$HOMEBREW_PREFIX/share/demo/tree".to_string()),
        ),
    ];
    assert_eq!(
        artifact_paths(&prefix, &c).unwrap(),
        [
            (
                PathBuf::from("Demo.app"),
                prefix.applications().join("Demo.app"),
                true,
                false
            ),
            (
                PathBuf::from("Demo.app/Contents/MacOS/demo"),
                prefix.bin().join("demo"),
                false,
                true
            ),
            (
                PathBuf::from("tree"),
                prefix.root.join("share/demo/tree"),
                false,
                false
            ),
        ]
    );
    for source in [
        "$HOMEBREW_PREFIX/Caskroom/other/1/platform-tools/adb",
        "$HOMEBREW_PREFIX/Caskroom/demo/2/platform-tools/adb",
        "$HOMEBREW_PREFIX/Caskroom/demo/1/../../other/1/adb",
        "$HOMEBREW_PREFIX/Caskroom/demo/1/",
        "$HOMEBREW_PREFIX/share/elsewhere/adb",
        "$HOME/adb",
    ] {
        for artifact in [
            Artifact::Binary(source.to_string(), None),
            Artifact::Generic(
                source.to_string(),
                Some("$HOMEBREW_PREFIX/share/demo/tree".to_string()),
            ),
        ] {
            c.artifacts = vec![artifact];
            assert!(
                matches!(
                    artifact_paths(&prefix, &c),
                    Err(InstallError::Unsupported(_))
                ),
                "{:?}",
                c.artifacts
            );
        }
    }
}

/// wezterm's shape: brew's `$APPDIR/WezTerm.app/...` binaries and
/// command wrappers (blender's, firefox's) name the app the cask
/// installs, which its zip keeps in a folder; and isabelle's: the app is
/// a generic artifact installed into `$APPDIR/`.
#[test]
#[cfg(unix)]
fn appdir_sources_resolve_to_the_app_the_cask_installs() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let machine = Machine::mac("arm64", "27.0", Some("27.0"), true);
    let binary = aterm_json::json!({"binary": ["$APPDIR/Demo.app/Contents/MacOS/demo"]});
    let wrapper = |executable: &str| {
        aterm_json::json!({"command_wrapper": [
            "wrapdemo", {"executable": executable, "args": ["--fixed"]}
        ]})
    };
    let installs = |shape: &str, root: &str, installer: aterm_json::Value| {
        let plist = format!("{root}Demo.app/Contents/Info.plist");
        let script = format!("{root}Demo.app/Contents/MacOS/demo");
        let members = [
            ZipMember {
                name: &plist,
                mode: 0o100644,
                data: b"app",
                deflate: false,
            },
            ZipMember {
                name: &script,
                mode: 0o100755,
                data: b"#!/bin/sh\nprintf 'demo ready [%s]\\n' \"$*\"\n",
                deflate: false,
            },
        ];
        let archive = tmp.path().join(format!("{shape}.zip"));
        std::fs::write(&archive, zip_bytes(&members, false)).unwrap();
        let prefix = Prefix::at(&tmp.path().join(shape));
        let cask = Cask::from_api(&aterm_json::json!({
            "token": "demo", "version": "1", "sha256": "no_check",
            "url": format!("file://{}", archive.display()),
            "artifacts": [installer, binary, wrapper("$APPDIR/Demo.app/Contents/MacOS/demo")],
        }))
        .unwrap();
        preflight(&prefix, &cask, &machine).unwrap();
        assert_eq!(
            install(&prefix, &cask, false, false, false, false).unwrap(),
            "1"
        );
        for (command, said) in [
            ("demo", "demo ready []\n"),
            ("wrapdemo", "demo ready [--fixed]\n"),
        ] {
            let output = aterm_uds::spawnfd::command(prefix.bin().join(command))
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                said,
                "{shape} {command}"
            );
        }
    };
    installs(
        "wezterm",
        "Top/",
        aterm_json::json!({"app": ["Top/Demo.app"]}),
    );
    for (shape, root) in [("isabelle", ""), ("isabelle-nested", "Top/")] {
        installs(
            shape,
            root,
            aterm_json::json!({"artifact": [format!("{root}Demo.app"), {"target": "$APPDIR/Demo.app"}]}),
        );
    }
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let renamed = Cask::from_api(&aterm_json::json!({
        "token": "demo", "version": "1", "sha256": "no_check", "url": "unused.zip",
        "artifacts": [
            {"app": ["Studio.app", {"target": "Studio Preview.app"}]},
            {"binary": ["$APPDIR/studio preview.app/Contents/MacOS/studio"]},
        ],
    }))
    .unwrap();
    assert_eq!(
        artifact_paths(&prefix, &renamed).unwrap()[1].0,
        PathBuf::from("Studio.app/Contents/MacOS/studio")
    );
    let app = aterm_json::json!({"app": ["Top/Demo.app"]});
    let tree = |target: &str| aterm_json::json!({"artifact": ["Top/Demo.app", {"target": target}]});
    for (installer, source) in [
        (app, "$APPDIR/Other.app/Contents/MacOS/demo"),
        (
            tree("$APPDIR/Demo.app"),
            "$APPDIR/Other.app/Contents/MacOS/demo",
        ),
        (
            tree("$APPDIR/Demo.app"),
            "$APPDIR/Demo.apple/Contents/MacOS/demo",
        ),
        (
            tree("$HOMEBREW_PREFIX/Demo.app"),
            "$APPDIR/Demo.app/Contents/MacOS/demo",
        ),
    ] {
        for artifact in [aterm_json::json!({"binary": [source]}), wrapper(source)] {
            let elsewhere = Cask::from_api(&aterm_json::json!({
                "token": "demo", "version": "1", "sha256": "no_check", "url": "unused.zip",
                "artifacts": [installer.clone(), artifact.clone()],
            }))
            .unwrap();
            assert!(
                matches!(
                    preflight(&prefix, &elsewhere, &machine),
                    Err(InstallError::Unsupported(why)) if why.contains("outside its payload")
                ),
                "{installer} {artifact}"
            );
        }
    }
}

#[test]
fn applications_sourced_binary_rebases_onto_the_app_payload() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let mut c = cask("unused.zip".to_string());
    c.artifacts = vec![
        Artifact::App("Demo.app".to_string(), None),
        Artifact::Binary(
            "/Applications/Demo.app/Contents/MacOS/tool".to_string(),
            None,
        ),
    ];
    let paths = artifact_paths(&prefix, &c).unwrap();
    assert!(paths.iter().any(|(src, dst, _, exec)| {
        src == &PathBuf::from("Demo.app/Contents/MacOS/tool")
            && dst == &prefix.bin().join("tool")
            && *exec
    }));
    c.artifacts = vec![
        Artifact::App(
            "build/Demo.app".to_string(),
            Some("Renamed.app".to_string()),
        ),
        Artifact::Binary(
            "/Applications/Renamed.app/Contents/MacOS/tool".to_string(),
            None,
        ),
    ];
    let paths = artifact_paths(&prefix, &c).unwrap();
    assert!(
        paths
            .iter()
            .any(|(src, _, _, _)| src == &PathBuf::from("build/Demo.app/Contents/MacOS/tool"))
    );
    c.artifacts = vec![Artifact::Binary(
        "/Applications/Other.app/Contents/MacOS/tool".to_string(),
        None,
    )];
    assert!(matches!(
        artifact_paths(&prefix, &c),
        Err(InstallError::Unsupported(_))
    ));
}

#[test]
fn unsupported_casks_refuse_before_native_changes() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    for artifact in [
        Artifact::Pkg("x.pkg".to_string()),
        Artifact::Other("uninstall:x".to_string()),
    ] {
        let mut cask = cask("unused.zip".to_string());
        cask.artifacts.push(artifact);
        assert!(matches!(
            install(&prefix, &cask, false, false, false, false),
            Err(InstallError::Unsupported(_))
        ));
        assert!(!prefix.root.exists());
    }
    let mut named = cask("unused.zip".to_string());
    named.artifacts.push(Artifact::Other(
        "generate_completions_from_executable".to_string(),
    ));
    let Err(InstallError::Unsupported(msg)) = install(&prefix, &named, false, false, false, false)
    else {
        panic!("expected native refusal");
    };
    assert!(
        msg.contains("generate_completions_from_executable"),
        "{msg}"
    );
    assert!(!prefix.root.exists());
    let cask = cask("unused.xar".to_string());
    assert!(matches!(
        install(&prefix, &cask, false, false, false, false),
        Err(InstallError::Unsupported(_))
    ));
    assert!(!prefix.root.exists());
    let with_deps = aterm_json::from_str(
        r#"{"token":"demo","version":"1","url":"unused.zip","sha256":"no_check","depends_on":{"formula":["other"],"cask":["helper"],"macos":{}},"artifacts":[{"app":["Demo.app"]}]}"#,
    )
    .unwrap();
    let with_deps = Cask::from_api(&with_deps).unwrap();
    assert_eq!(with_deps.unsupported_constraints, None);
    assert_eq!(with_deps.depends_formula, ["other"]);
    assert_eq!(with_deps.depends_cask, ["helper"]);
    for constraint in [
        r#""container":{"type":"unknown"}"#,
        r#""depends_on":{"formula":[42]}"#,
        r#""conflicts_with":{"cask":[42]}"#,
    ] {
        let constrained = aterm_json::from_str(&format!(r#"{{"token":"demo","version":"1","url":"unused.zip","sha256":"no_check",{constraint},"artifacts":[{{"app":["Demo.app"]}}],"variations":{{"arm64_tahoe":{{"artifacts":[{{"app":["Demo.app"]}}]}}}}}}"#)).unwrap();
        let constrained = Cask::from_api(&constrained).unwrap();
        assert!(constrained.unsupported_constraints.is_some());
        assert!(matches!(
            install(&prefix, &constrained, false, false, false, false),
            Err(InstallError::Unsupported(_))
        ));
        assert!(!prefix.root.exists());
    }
    prefix.ensure().unwrap();
    std::fs::remove_dir(prefix.cellar()).unwrap();
    std::fs::write(prefix.cellar(), b"unknown census").unwrap();
    let mut conflicting = self::cask("unused.zip".to_string());
    conflicting.conflicts = vec!["other".to_string()];
    assert!(preflight(&prefix, &conflicting, Machine::this()).is_err());
    assert_eq!(std::fs::read(prefix.cellar()).unwrap(), b"unknown census");
}

#[test]
#[cfg(unix)]
fn a_missing_artifact_leaves_no_receipt_or_links() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let archive = tmp.path().join("demo.zip");
    std::fs::write(
        &archive,
        zip_bytes(
            &[ZipMember {
                name: "other",
                mode: 0o100644,
                data: b"not the promised tool",
                deflate: false,
            }],
            false,
        ),
    )
    .unwrap();
    let cask = cask(format!("file://{}", archive.display()));
    assert!(install(&prefix, &cask, false, false, false, false).is_err());
    assert!(prefix.installed(Kind::Cask).is_empty());
    assert!(!prefix.cask_dir("demo", "1").exists());
    assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
    std::fs::write(
        &archive,
        zip_bytes(
            &[
                ZipMember {
                    name: "tool",
                    mode: 0o100644,
                    data: b"not executable",
                    deflate: false,
                },
                ZipMember {
                    name: "Demo.app/Contents/Info.plist",
                    mode: 0o100644,
                    data: b"app",
                    deflate: false,
                },
            ],
            false,
        ),
    )
    .unwrap();
    std::fs::remove_file(download_dest(&prefix, &cask)).unwrap();
    assert!(install(&prefix, &cask, false, false, false, false).is_err());
    assert!(prefix.installed(Kind::Cask).is_empty());
    assert!(!prefix.cask_dir("demo", "1").exists());
    assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
}

#[test]
#[cfg(unix)]
fn uninstall_rechecks_planned_links_parents_and_payloads() {
    use super::super::prefix::{symlink_dir, symlink_file};
    use std::os::unix::ffi::OsStringExt;

    for change in [
        "foreign-file",
        "same-target-link",
        "physical-parent",
        "aliased-parent",
        "payload",
        "receipt",
    ] {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let payload = prefix.cask_dir("demo", "1");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("tool"), b"owned payload").unwrap();
        prefix
            .write_receipt(
                &payload,
                &[
                    ("token", "demo"),
                    ("version", "1"),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        let earlier = prefix.applications().join("also-owned");
        symlink_file(&payload.join("tool"), &earlier).unwrap();
        let applications = tmp.path().join("captured-applications");
        std::fs::create_dir(&applications).unwrap();
        let artifact = applications.join("tool");
        symlink_file(&payload.join("tool"), &artifact).unwrap();
        let plan = UninstallPlan::prepare(
            &prefix,
            "demo",
            &["1".to_string()],
            &[prefix.applications(), applications.clone()],
        )
        .unwrap();

        match change {
            "foreign-file" | "same-target-link" => {
                std::fs::rename(&artifact, tmp.path().join("held-link")).unwrap();
                if change == "foreign-file" {
                    std::fs::write(&artifact, b"foreign artifact").unwrap();
                } else {
                    symlink_file(&payload.join("tool"), &artifact).unwrap();
                }
            }
            "physical-parent" | "aliased-parent" => {
                std::fs::rename(&applications, tmp.path().join("held-parent")).unwrap();
                if change == "physical-parent" {
                    std::fs::create_dir(&applications).unwrap();
                    std::fs::write(&artifact, b"foreign artifact").unwrap();
                } else {
                    let outside = tmp.path().join("foreign-parent");
                    std::fs::create_dir(&outside).unwrap();
                    std::fs::write(outside.join("tool"), b"foreign artifact").unwrap();
                    symlink_dir(&outside, &applications).unwrap();
                }
            }
            "payload" => {
                let held = tmp.path().join("held-payload");
                std::fs::rename(&payload, &held).unwrap();
                std::fs::create_dir(&payload).unwrap();
                std::fs::write(payload.join("tool"), b"foreign payload").unwrap();
                std::fs::copy(
                    held.join("INSTALL_RECEIPT.json"),
                    payload.join("INSTALL_RECEIPT.json"),
                )
                .unwrap();
            }
            "receipt" => {
                std::fs::write(payload.join("INSTALL_RECEIPT.json"), b"foreign receipt").unwrap();
            }
            _ => unreachable!(),
        }
        assert!(plan.apply(&prefix).is_err(), "{change}");
        assert!(payload.is_dir(), "{change}");
        assert_eq!(
            std::fs::read_link(&earlier).unwrap(),
            payload.join("tool"),
            "{change}"
        );
        if matches!(
            change,
            "foreign-file" | "physical-parent" | "aliased-parent"
        ) {
            assert_eq!(
                std::fs::read(&artifact).unwrap(),
                b"foreign artifact",
                "{change}"
            );
        } else {
            assert_eq!(
                std::fs::read_link(&artifact).unwrap(),
                payload.join("tool"),
                "{change}"
            );
        }
        if change == "payload" {
            assert_eq!(
                std::fs::read(payload.join("tool")).unwrap(),
                b"foreign payload"
            );
        }
        if change == "receipt" {
            assert_eq!(
                std::fs::read(payload.join("INSTALL_RECEIPT.json")).unwrap(),
                b"foreign receipt"
            );
        }
    }
    let captured = Prefix::at(&PathBuf::from(std::ffi::OsString::from_vec(
        b"/captured-\xff".to_vec(),
    )));
    let link = InstallLink {
        source: captured.cask_dir("demo", "1").join("tool"),
        destination: captured.bin().join("tool"),
        directory: false,
    };
    assert!(artifact_receipt_fields(&[link]).is_empty());
    assert!(
        recorded_artifact_paths(&captured, &aterm_json::from_str("{}").unwrap())
            .unwrap()
            .is_none()
    );
    let spellings: &[&str] = if cfg!(target_os = "linux") {
        &["plain", "non-utf8", "old-lossy"]
    } else {
        &["plain"]
    };
    for &spelling in spellings {
        let non_utf8 = spelling != "plain";
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let name = if non_utf8 {
            std::ffi::OsString::from_vec(b"prefix-\xff".to_vec())
        } else {
            std::ffi::OsString::from("prefix")
        };
        let prefix = Prefix::at(&tmp.path().join(name));
        prefix.ensure().unwrap();
        let mut owned = Vec::new();
        for version in ["1", "2"] {
            let payload = prefix.cask_dir("demo", version);
            std::fs::create_dir_all(&payload).unwrap();
            std::fs::write(payload.join("tool"), version.as_bytes()).unwrap();
            let destination = prefix.bin().join(format!("tool-{version}"));
            let link = InstallLink {
                source: payload.join("tool"),
                destination: destination.clone(),
                directory: false,
            };
            let fields = if version == "2" {
                artifact_receipt_fields(std::slice::from_ref(&link))
            } else {
                Vec::new()
            };
            assert_eq!(fields.is_empty(), version == "1" || non_utf8);
            let mut receipt = vec![
                ("token", "demo"),
                ("version", version),
                ("installed_by", "abrew"),
            ];
            receipt.extend(
                fields
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            );
            prefix.write_receipt(&payload, &receipt).unwrap();
            if version == "2" && spelling == "old-lossy" {
                let old = destination.to_string_lossy();
                prefix
                    .write_receipt(
                        &payload,
                        &[
                            ("token", "demo"),
                            ("version", version),
                            ("installed_by", "abrew"),
                            ("artifact_0", old.as_ref()),
                        ],
                    )
                    .unwrap();
            }
            symlink_file(&link.source, &link.destination).unwrap();
            owned.push(destination);
        }
        let extra = prefix.bin().join("not-recorded");
        let target = prefix.cask_dir("demo", "2").join("tool");
        symlink_file(&target, &extra).unwrap();
        let plan = UninstallPlan::prepare(
            &prefix,
            "demo",
            &["1".to_string(), "2".to_string()],
            &[prefix.bin()],
        )
        .unwrap();
        assert!(owned.iter().all(|path| plan.links.contains_key(path)));
        assert_eq!(plan.links.contains_key(&extra), non_utf8);
        plan.apply(&prefix).unwrap();
        assert!(
            owned
                .iter()
                .all(|path| std::fs::symlink_metadata(path).is_err())
        );
        assert!(!prefix.cask_dir("demo", "1").exists());
        assert!(!prefix.cask_dir("demo", "2").exists());
        if non_utf8 {
            assert!(std::fs::symlink_metadata(&extra).is_err());
        } else {
            assert_eq!(std::fs::read_link(&extra).unwrap(), target);
        }
    }
}

#[test]
#[cfg(unix)]
fn uninstall_refuses_aliased_artifact_directories_before_removing_anything() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let payload = prefix.cask_dir("demo", "1");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("tool"), b"owned payload").unwrap();
    prefix
        .write_receipt(
            &payload,
            &[
                ("token", "demo"),
                ("version", "1"),
                ("installed_by", "abrew"),
            ],
        )
        .unwrap();
    let unknown = prefix.cask_dir("demo", "personal");
    std::fs::create_dir(&unknown).unwrap();
    std::fs::write(unknown.join("notes"), b"owner notes").unwrap();
    let artifact = prefix.applications().join("tool");
    super::super::prefix::symlink_file(&payload.join("tool"), &artifact).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let external_link = outside.join("keep");
    super::super::prefix::symlink_file(&payload.join("tool"), &external_link).unwrap();
    std::fs::write(outside.join("marker"), b"external bytes").unwrap();
    std::fs::remove_dir(prefix.bin()).unwrap();
    super::super::prefix::symlink_dir(&outside, &prefix.bin()).unwrap();
    let local_applications = tmp.path().join("captured-applications");
    std::fs::create_dir(&local_applications).unwrap();
    let man = prefix.root.join("share/man");
    std::fs::write(&man, b"unknown man root").unwrap();
    assert!(artifact_directories(&prefix, local_applications.clone()).is_err());
    assert!(payload.join("INSTALL_RECEIPT.json").is_file());
    assert_eq!(std::fs::read(&artifact).unwrap(), b"owned payload");
    std::fs::remove_file(&man).unwrap();
    let directories = [prefix.applications(), prefix.bin(), local_applications];
    assert!(uninstall_from(&prefix, "demo", false, &directories, None, &BTreeSet::new()).is_err());
    assert_eq!(std::fs::read(&artifact).unwrap(), b"owned payload");
    assert_eq!(
        std::fs::read_link(&external_link).unwrap(),
        payload.join("tool")
    );
    assert_eq!(
        std::fs::read(outside.join("marker")).unwrap(),
        b"external bytes"
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 2);
    assert!(payload.join("INSTALL_RECEIPT.json").is_file());
    std::fs::remove_file(prefix.bin()).unwrap();
    std::fs::write(prefix.bin(), b"foreign root file").unwrap();
    assert!(uninstall_from(&prefix, "demo", false, &directories, None, &BTreeSet::new()).is_err());
    assert_eq!(std::fs::read(&artifact).unwrap(), b"owned payload");
    assert_eq!(std::fs::read(prefix.bin()).unwrap(), b"foreign root file");
    std::fs::remove_file(prefix.bin()).unwrap();
    std::fs::create_dir(prefix.bin()).unwrap();
    let foreign = prefix.bin().join("foreign");
    super::super::prefix::symlink_file(&outside.join("marker"), &foreign).unwrap();
    uninstall_from(&prefix, "demo", false, &directories, None, &BTreeSet::new()).unwrap();
    assert!(std::fs::symlink_metadata(&artifact).is_err());
    assert!(!payload.exists());
    assert_eq!(
        std::fs::read(unknown.join("notes")).unwrap(),
        b"owner notes"
    );
    assert_eq!(std::fs::read(&foreign).unwrap(), b"external bytes");
    assert_eq!(
        std::fs::read_link(&external_link).unwrap(),
        payload.join("tool")
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 2);
    let generic_world = aterm_tempfile::TempDir::new().unwrap();
    let generic_prefix = Prefix::at(&generic_world.path().join("prefix"));
    generic_prefix.ensure().unwrap();
    let generic_payload = generic_prefix.cask_dir("demo", "1");
    std::fs::create_dir_all(&generic_payload).unwrap();
    std::fs::write(generic_payload.join("tree"), b"payload").unwrap();
    let destinations = [
        generic_prefix.root.join("var/demo"),
        generic_prefix.root.join("share/a/b/c/d/demo"),
    ];
    let mut links = Vec::new();
    for destination in &destinations {
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        super::super::prefix::symlink_file(&generic_payload.join("tree"), destination).unwrap();
        links.push(InstallLink {
            source: generic_payload.join("tree"),
            destination: destination.clone(),
            directory: false,
        });
    }
    let fields = complete_artifact_receipt_fields(&generic_prefix, &links).unwrap();
    let mut receipt = vec![
        ("token", "demo"),
        ("version", "1"),
        ("installed_by", "abrew"),
    ];
    receipt.extend(
        fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    for invalid in ["../outside", "/absolute", "var/../demo"] {
        generic_prefix
            .write_receipt(
                &generic_payload,
                &[
                    ("token", "demo"),
                    ("version", "1"),
                    ("installed_by", "abrew"),
                    ("relative_artifact_0", invalid),
                ],
            )
            .unwrap();
        let saved = std::fs::read(generic_payload.join("INSTALL_RECEIPT.json")).unwrap();
        assert!(
            UninstallPlan::prepare(
                &generic_prefix,
                "demo",
                &["1".to_string()],
                &[generic_prefix.bin()]
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(generic_payload.join("INSTALL_RECEIPT.json")).unwrap(),
            saved
        );
        assert!(
            destinations
                .iter()
                .all(|path| std::fs::read_link(path).unwrap() == generic_payload.join("tree"))
        );
        assert_eq!(
            std::fs::read(generic_payload.join("tree")).unwrap(),
            b"payload"
        );
    }
    generic_prefix
        .write_receipt(&generic_payload, &receipt)
        .unwrap();
    let saved = std::fs::read(generic_payload.join("INSTALL_RECEIPT.json")).unwrap();
    let oversized = "x".repeat(1024 * 1024);
    assert!(
        generic_prefix
            .write_receipt(&generic_payload, &[("oversized", &oversized)])
            .is_err()
    );
    assert_eq!(
        std::fs::read(generic_payload.join("INSTALL_RECEIPT.json")).unwrap(),
        saved
    );
    let plan = UninstallPlan::prepare(
        &generic_prefix,
        "demo",
        &["1".to_string()],
        &[generic_prefix.bin()],
    )
    .unwrap();
    assert!(
        destinations
            .iter()
            .all(|path| plan.links.contains_key(path))
    );
    plan.apply(&generic_prefix).unwrap();
    assert!(
        destinations
            .iter()
            .all(|path| std::fs::symlink_metadata(path).is_err())
    );
    assert!(!generic_payload.exists());
    let legacy_world = aterm_tempfile::TempDir::new().unwrap();
    let legacy_prefix = Prefix::at(&legacy_world.path().join("prefix"));
    legacy_prefix.ensure().unwrap();
    let legacy_payload = legacy_prefix.cask_dir("demo", "1");
    std::fs::create_dir_all(&legacy_payload).unwrap();
    std::fs::write(legacy_payload.join("tool"), b"legacy payload").unwrap();
    legacy_prefix
        .write_receipt(
            &legacy_payload,
            &[
                ("token", "demo"),
                ("version", "1"),
                ("installed_by", "abrew"),
            ],
        )
        .unwrap();
    let legacy_parent = legacy_prefix.root.join("share/x");
    std::fs::create_dir(&legacy_parent).unwrap();
    let legacy_link = legacy_parent.join("tool");
    super::super::prefix::symlink_file(&legacy_payload.join("tool"), &legacy_link).unwrap();
    let legacy_library_link = legacy_prefix.root.join("Library/QuickLook/tool");
    super::super::prefix::symlink_file(&legacy_payload.join("tool"), &legacy_library_link).unwrap();
    let legacy_applications = legacy_world.path().join("captured-applications");
    std::fs::create_dir(&legacy_applications).unwrap();
    let directories = artifact_directories(&legacy_prefix, legacy_applications).unwrap();
    let plan =
        UninstallPlan::prepare(&legacy_prefix, "demo", &["1".to_string()], &directories).unwrap();
    assert!(plan.links.contains_key(&legacy_link));
    assert!(plan.links.contains_key(&legacy_library_link));
    plan.apply(&legacy_prefix).unwrap();
    assert!(std::fs::symlink_metadata(&legacy_link).is_err());
    assert!(std::fs::symlink_metadata(&legacy_library_link).is_err());
    assert!(!legacy_payload.exists());
    for relative in ["share/man/man1", "share/man"] {
        let world = aterm_tempfile::TempDir::new().unwrap();
        let prefix = Prefix::at(&world.path().join("prefix"));
        prefix.ensure().unwrap();
        let payload = prefix.cask_dir("demo", "1");
        let tree = payload.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("page"), b"owned page").unwrap();
        let destination = prefix.root.join(relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        super::super::prefix::symlink_dir(&tree, &destination).unwrap();
        let fields = complete_artifact_receipt_fields(
            &prefix,
            &[InstallLink {
                source: tree.clone(),
                destination: destination.clone(),
                directory: true,
            }],
        )
        .unwrap();
        let mut receipt = vec![
            ("token", "demo"),
            ("version", "1"),
            ("installed_by", "abrew"),
        ];
        receipt.extend(
            fields
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        prefix.write_receipt(&payload, &receipt).unwrap();
        let applications = world.path().join("captured-applications");
        std::fs::create_dir(&applications).unwrap();
        if relative == "share/man" {
            let held = world.path().join("held-owned-link");
            std::fs::rename(&destination, &held).unwrap();
            let outside = world.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("keep"), b"preserved").unwrap();
            super::super::prefix::symlink_dir(&outside, &destination).unwrap();
            let directories = artifact_directories(&prefix, applications.clone()).unwrap();
            assert!(
                UninstallPlan::prepare(&prefix, "demo", &["1".to_string()], &directories).is_err()
            );
            assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"preserved");
            assert_eq!(std::fs::read(tree.join("page")).unwrap(), b"owned page");
            std::fs::remove_file(&destination).unwrap();
            std::fs::rename(held, &destination).unwrap();
        }
        let directories = artifact_directories(&prefix, applications).unwrap();
        let plan =
            UninstallPlan::prepare(&prefix, "demo", &["1".to_string()], &directories).unwrap();
        assert!(plan.links.contains_key(&destination), "{relative}");
        plan.apply(&prefix).unwrap();
        assert!(
            std::fs::symlink_metadata(&destination).is_err(),
            "{relative}"
        );
        assert!(!payload.exists(), "{relative}");
    }
}
