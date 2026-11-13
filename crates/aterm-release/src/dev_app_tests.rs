// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::dev_identity::fake::{Fake, Kc, World};

fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = std::fs::canonicalize(dir.path()).expect("canonical scratch");
    (dir, path)
}

fn parse_args(args: &[&str]) -> Result<Cmd, String> {
    parse(&mut args.iter().copied())
}

fn build(args: &[&str]) -> Options {
    match parse_args(args) {
        Ok(Cmd::Build(opts)) => opts,
        other => panic!("{args:?}: {other:?}"),
    }
}

fn log(fake: &Fake) -> Vec<String> {
    fake.0.borrow().log.iter().map(|c| c.join(" ")).collect()
}

/// The names in `dir`, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_flags_parse_to_what_they_say() {
    assert_eq!(
        build(&[]),
        Options {
            profile: Profile::Release,
            out: None,
            install: Install::Applications,
            pin: true,
            link: true,
            sign: SignWith::Default,
        }
    );
    assert_eq!(
        build(&[
            "--sign-id",
            "-",
            "--out",
            "o",
            "--install-dir",
            "d",
            "--no-link",
            "--no-pin",
        ]),
        Options {
            profile: Profile::Release,
            out: Some(PathBuf::from("o")),
            install: Install::Into(PathBuf::from("d")),
            pin: false,
            link: false,
            sign: SignWith::AdHoc,
        }
    );
    assert_eq!(build(&["--no-install"]).install, Install::No);
    assert_eq!(build(&["--profile", "release"]), build(&[]));
    let native = build(&[
        "--profile",
        "test",
        "--no-install",
        "--no-link",
        "--sign-id",
        "-",
    ]);
    assert_eq!(native.profile, Profile::Test);
    assert_eq!(native.install, Install::No);
    assert!(!native.link);
    assert!(native.pin);
    assert_eq!(native.sign, SignWith::AdHoc);
    assert_eq!(
        build(&["--sign-id", "Apple Development: Jane"]).sign,
        SignWith::Identity("Apple Development: Jane".into())
    );
    assert_eq!(parse_args(&["--help"]), Ok(Cmd::Help));
    assert_eq!(
        parse_args(&["identity"]),
        Ok(Cmd::Identity {
            remove: false,
            keychain: None
        })
    );
    assert_eq!(
        parse_args(&["identity", "--remove", "--keychain", "k.keychain-db"]),
        Ok(Cmd::Identity {
            remove: true,
            keychain: Some(PathBuf::from("k.keychain-db"))
        })
    );
    for refused in [
        &["--bogus"][..],
        &["--no-install", "--install-dir", "d"],
        &["--no-link", "--no-link"],
        &["--out"],
        &["--out", "--no-pin"],
        &["--install-dir", "-"],
        &["--sign-id", "a", "--sign-id", "b"],
        &["--profile"],
        &["--profile", ""],
        &["--profile", "-"],
        &["--profile", "--no-install"],
        &["--profile", "debug"],
        &["--profile", "test", "--profile", "test"],
        &["--profile", "release", "--profile", "test"],
        &["identity", "--bogus"],
        &["identity", "--keychain"],
        &["identity", "--profile", "test"],
    ] {
        assert!(parse_args(refused).is_err(), "{refused:?}");
    }
    let args: Vec<String> = ["dev-app", "--profile", "test", "--no-link"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(matches!(
        crate::cli::parse(&args),
        Ok(crate::cli::Cmd::DevApp(Cmd::Build(Options {
            profile: Profile::Test,
            link: false,
            ..
        })))
    ));
}

fn stamp() -> Stamp {
    Stamp {
        version: "0.104.0".into(),
        build: 1_790_000_000,
        commit: "0123456789ab-dirty".into(),
        commit_full: "0123456789abcdef0123456789abcdef01234567".into(),
        dev_commits: 3,
    }
}

#[test]
fn the_build_is_the_trust_targo_on_its_unverified_lane() {
    let stage2 = Path::new("/store/trust/9192/bin");
    let led = build_command(
        stage2,
        Path::new("/src/aterm"),
        Path::new("/src/aterm/target"),
        Some("build.build-dir='/src/aterm/target/build.noindex'"),
        OsStr::new("/usr/bin:/bin"),
        &stamp(),
        Profile::Release,
    );
    let led: Vec<&OsStr> = led.get_args().collect();
    assert_eq!(
        led[..3],
        [
            "--config",
            "build.build-dir='/src/aterm/target/build.noindex'",
            "--unverified"
        ]
        .map(OsStr::new),
        "the intermediates go where Spotlight never looks"
    );
    let cmd = build_command(
        stage2,
        Path::new("/src/aterm"),
        Path::new("/src/aterm/target"),
        None,
        OsStr::new("/usr/bin:/bin"),
        &stamp(),
        Profile::Release,
    );
    assert_eq!(cmd.get_program(), stage2.join("targo").as_os_str());
    let args: Vec<&OsStr> = cmd.get_args().collect();
    assert_eq!(
        args,
        [
            "--unverified",
            "build",
            "--release",
            "-p",
            "aterm",
            "--bin",
            "aterm",
            "--target-dir",
            "/src/aterm/target"
        ]
        .map(OsStr::new)
    );
    assert_eq!(cmd.get_current_dir(), Some(Path::new("/src/aterm")));
    let envs: std::collections::BTreeMap<String, Option<String>> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert_eq!(
        envs["PATH"].as_deref(),
        Some("/store/trust/9192/bin:/usr/bin:/bin")
    );
    for scrubbed in [
        "RUSTC",
        "CARGO_BUILD_RUSTC",
        "RUSTUP_TOOLCHAIN",
        "CARGO_BUILD_TARGET",
        aterm_stamp::input::RELEASE,
        aterm_stamp::input::INDEX_HINT,
    ] {
        assert_eq!(envs.get(scrubbed), Some(&None), "{scrubbed}");
    }
    for (key, value) in [
        ("ATERM_STAMP", "commit"),
        ("SOURCE_DATE_EPOCH", "1790000000"),
        ("ATERM_BUILD_GIT_COMMIT", "0123456789ab-dirty"),
        (
            "ATERM_BUILD_GIT_COMMIT_FULL",
            "0123456789abcdef0123456789abcdef01234567",
        ),
        ("ATERM_BUILD_DEV_COMMITS", "3"),
    ] {
        assert_eq!(envs.get(key), Some(&Some(value.to_string())), "{key}");
    }
    assert!(!envs.contains_key("RUSTFLAGS"));
}

#[test]
fn the_test_profile_builds_the_ordinary_app_with_the_same_stamp() {
    let stage2 = Path::new("/store/trust/9192/bin");
    let root = Path::new("/src/aterm");
    let target = Path::new("/warm/test-profile.noindex");
    let stamp = stamp();
    let release = build_command(
        stage2,
        root,
        target,
        None,
        OsStr::new("/usr/bin"),
        &stamp,
        Profile::Release,
    );
    let native = build_command(
        stage2,
        root,
        target,
        None,
        OsStr::new("/usr/bin"),
        &stamp,
        Profile::Test,
    );
    assert_eq!(native.get_program(), release.get_program());
    assert_eq!(native.get_current_dir(), release.get_current_dir());
    assert_eq!(
        native.get_envs().collect::<Vec<_>>(),
        release.get_envs().collect::<Vec<_>>()
    );
    assert_eq!(
        native.get_args().collect::<Vec<_>>(),
        [
            "--unverified",
            "build",
            "--profile",
            "test",
            "-p",
            "aterm",
            "--bin",
            "aterm",
            "--target-dir",
            "/warm/test-profile.noindex",
        ]
        .map(OsStr::new)
    );
    let with_config = build_command(
        stage2,
        root,
        target,
        Some("build.build-dir='/warm/intermediates.noindex'"),
        OsStr::new("/usr/bin"),
        &stamp,
        Profile::Test,
    );
    let configured: Vec<_> = with_config.get_args().collect();
    assert_eq!(
        configured[..2],
        ["--config", "build.build-dir='/warm/intermediates.noindex'"].map(OsStr::new)
    );
    assert_eq!(configured[2..], native.get_args().collect::<Vec<_>>());
}

#[test]
fn a_binary_from_the_other_profile_cannot_replace_the_selected_output() {
    for (selected, other, folder) in [
        (Profile::Test, Profile::Release, "debug"),
        (Profile::Release, Profile::Test, "release"),
    ] {
        let (_dir, target) = scratch();
        let old = other.binary(&target);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"other profile").unwrap();
        let expected = target.join(folder).join("aterm");
        let refused = built_binary(&target, selected).unwrap_err();
        assert!(refused.contains(&expected.display().to_string()));
        assert!(refused.contains(selected.name()));
        std::fs::create_dir_all(expected.parent().unwrap()).unwrap();
        std::fs::write(&expected, b"selected profile").unwrap();
        assert_eq!(built_binary(&target, selected).unwrap(), expected);
        assert_eq!(std::fs::read(old).unwrap(), b"other profile");
    }
}

#[test]
fn the_stamp_is_heads_facts_as_the_build_script_reads_them() {
    let (_dir, root) = scratch();
    let git = |args: &[&str]| {
        let out = crate::ledger::testing::git()
            .current_dir(&root)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_COMMITTER_DATE", "1790000000 +0000")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.package]\nversion = \"0.104.0\"\n",
    )
    .unwrap();
    git(&["add", "Cargo.toml"]);
    git(&["commit", "-q", "-m", "one"]);
    let first = Stamp::of(&root).expect("stamp");
    assert_eq!(first.dev_commits, 0, "no tag");
    git(&["tag", "v0.104.0"]);
    git(&["commit", "-q", "--allow-empty", "-m", "two"]);
    git(&["commit", "-q", "--allow-empty", "-m", "three"]);
    let full = git(&["rev-parse", "HEAD"]);
    assert_eq!(
        Stamp::of(&root).expect("stamp"),
        Stamp {
            version: "0.104.0".into(),
            build: 1_790_000_000,
            commit: full[..12].to_string(),
            commit_full: full.clone(),
            dev_commits: 2,
        }
    );
    // The build script's rule: a file in the measured ledger, which no build reads, leaves
    // the stamp clean; a changed source file below marks it dirty.
    std::fs::create_dir_all(root.join("docs/measured/fabric")).unwrap();
    std::fs::write(
        root.join("docs/measured/fabric/2026-11-04-fabric-1001.json"),
        "{}",
    )
    .unwrap();
    assert_eq!(
        Stamp::of(&root).expect("stamp").commit,
        full[..12],
        "a ledger entry dirtied the stamp"
    );
    std::fs::write(root.join("Cargo.toml"), "edited").unwrap();
    assert!(Stamp::of(&root).is_err(), "no workspace version");
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.package]\nversion = \"0.104.0\"\n\n",
    )
    .unwrap();
    assert_eq!(
        Stamp::of(&root).expect("stamp").commit,
        format!("{}-dirty", &full[..12])
    );
    git(&["add", "Cargo.toml"]);
    git(&["commit", "-q", "-m", "calendar"]);
    git(&["tag", "v1.20261110.1"]);
    git(&["commit", "-q", "--allow-empty", "-m", "after calendar"]);
    git(&["tag", "v1.20261110.2-rc1"]);
    git(&["tag", "v1.20261110.2+preview"]);
    git(&["tag", "vnotes.not.a.release"]);
    git(&["tag", "v1.20261110.2garbage"]);
    git(&["tag", "v1.20261110.2.0"]);
    git(&["tag", "v1.20260231.1"]);
    git(&["tag", "v1.20261110.0"]);
    git(&["tag", "v0.104.1"]);
    assert_eq!(Stamp::of(&root).expect("stamp").dev_commits, 1);
}

#[test]
fn a_relative_keychain_is_the_current_directorys() {
    let (home, cwd) = (Path::new("/Users//u"), Path::new("/work"));
    assert_eq!(
        identity_keychain(Some(Path::new("k.keychain-db")), home, cwd),
        (PathBuf::from("/work/k.keychain-db"), false)
    );
    assert_eq!(
        identity_keychain(Some(Path::new("/abs/k.keychain-db")), home, cwd),
        (PathBuf::from("/abs/k.keychain-db"), false)
    );
    assert_eq!(
        identity_keychain(None, home, cwd),
        (dev_identity::default_keychain(home), true)
    );
}

#[test]
fn the_dev_bundle_is_the_release_layout_under_its_own_identity() {
    let root = aterm_tempfile::manifest_dir().join("../..");
    let (_dir, out) = scratch();
    let bin = out.join("aterm-binary");
    std::fs::write(&bin, b"\xcf\xfa\xed\xfe not really a Mach-O").unwrap();
    let stamp = stamp();
    for pin in [true, false] {
        let app = assemble(&root, &out, &stamp, &bin, pin).expect("assemble");
        assert_eq!(app, out.join(DEV_APP_NAME));
        let macos = app.join("Contents/MacOS");
        assert!(macos.join("aterm").is_file());
        for alias in crate::bundle::ARGV0_ALIASES {
            assert_eq!(
                std::fs::read_link(macos.join(alias)).ok(),
                Some(PathBuf::from("aterm")),
                "{alias}"
            );
        }
        let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        for (key, value) in [
            ("CFBundleIdentifier", DEV_BUNDLE_ID),
            ("CFBundleDisplayName", DEV_DISPLAY_NAME),
            ("CFBundleVersion", "1790000000"),
            ("CFBundleShortVersionString", "0.104.0"),
            ("ATermGitCommit", "0123456789ab-dirty"),
        ] {
            assert!(
                plist.contains(&format!("<key>{key}</key>\n\t<string>{value}</string>")),
                "{key}: {plist}"
            );
        }
        #[cfg(target_os = "macos")]
        assert_eq!(aterm_update::plist_marks_dev_build(&plist), pin);
    }
}

#[test]
fn a_rebuilt_binary_changes_the_dev_bundles_info_plist() {
    let root = aterm_tempfile::manifest_dir().join("../..");
    let (_dir, out) = scratch();
    let bin = out.join("aterm-binary");
    let stamp = stamp();
    let plist_of = |bytes: &[u8]| {
        std::fs::write(&bin, bytes).unwrap();
        let app = assemble(&root, &out, &stamp, &bin, false).expect("assemble");
        std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap()
    };
    let first = plist_of(b"\xcf\xfa\xed\xfe build one");
    assert!(
        first.contains(&format!(
            "<key>{DEV_BINARY_KEY}</key>\n\t<string>{}</string>",
            crate::dmg::sha256_file(&bin).unwrap()
        )),
        "{first}"
    );
    assert_eq!(
        plist_of(b"\xcf\xfa\xed\xfe build one"),
        first,
        "the same code"
    );
    assert_ne!(
        plist_of(b"\xcf\xfa\xed\xfe build two"),
        first,
        "one dirty tree's next build, at the same version, build and commit"
    );
}

fn bundle_at(dir: &Path) -> PathBuf {
    let app = dir.join(DEV_APP_NAME);
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::write(app.join("Contents/MacOS/aterm"), b"exe").unwrap();
    std::os::unix::fs::symlink("aterm", app.join("Contents/MacOS/aterm-ctl")).unwrap();
    std::fs::write(app.join("Contents/Info.plist"), b"<plist/>").unwrap();
    app
}

#[test]
fn ad_hoc_is_signed_and_verified_with_no_keychain_asked() {
    let (_dir, root) = scratch();
    let app = bundle_at(&root);
    let fake = Fake::new(World::default());
    sign(&app, &SignWith::AdHoc, &root, &fake).expect("sign");
    let shown = app.display();
    assert_eq!(
        log(&fake),
        [
            format!("codesign --force --sign - {shown}"),
            format!("codesign --verify --strict {shown}"),
        ]
    );
}

#[test]
fn the_default_signer_is_the_dev_identity_when_it_exists() {
    let (_dir, home) = scratch();
    let app = bundle_at(&home);
    let fake = Fake::new(World::default());
    sign(&app, &SignWith::Default, &home, &fake).expect("ad hoc without a keychain");
    assert!(log(&fake).contains(&format!("codesign --force --sign - {}", app.display())));

    let keychain = dev_identity::default_keychain(&home);
    std::fs::create_dir_all(keychain.parent().unwrap()).unwrap();
    let fake = Fake::new(World {
        search_list: vec![home.join("login.keychain-db")],
        ..World::default()
    });
    dev_identity::create(&keychain, false, &fake).expect("the identity");
    let leaf = fake.0.borrow().keychains[&keychain].identities[0].clone();
    fake.0.borrow_mut().log.clear();
    sign(&app, &SignWith::Default, &home, &fake).expect("signed by the identity");
    let shown = app.display();
    let codesigns: Vec<String> = log(&fake)
        .into_iter()
        .filter(|c| c.starts_with("codesign"))
        .collect();
    assert_eq!(
        codesigns,
        [
            format!("codesign --force --sign {leaf} {shown}"),
            format!("codesign -d -r- {shown}"),
            format!("codesign --verify --strict {shown}"),
        ]
    );

    fake.0.borrow_mut().keychains.insert(
        keychain.clone(),
        Kc {
            password: "not-ours".into(),
            locked: true,
            identities: Vec::new(),
        },
    );
    fake.0.borrow_mut().log.clear();
    let stopped = sign(&app, &SignWith::Default, &home, &fake).unwrap_err();
    assert!(stopped.contains("cannot be used now"), "{stopped}");
    assert!(stopped.contains("--sign-id -"), "{stopped}");
    assert!(!log(&fake).iter().any(|c| c.starts_with("codesign")));
}

#[test]
fn an_identity_whose_requirement_names_bytes_is_refused() {
    let (_dir, root) = scratch();
    let app = bundle_at(&root);
    let fake = Fake::new(World {
        requirement: Some("# designated => cdhash H\"240c6c45\"\n".into()),
        ..World::default()
    });
    let refused = sign(&app, &SignWith::Identity("X".into()), &root, &fake).unwrap_err();
    assert!(refused.contains("not identity-based"), "{refused}");
}

#[test]
fn a_codesign_that_does_not_answer_stops_with_the_bundle_state() {
    let (_dir, root) = scratch();
    let app = bundle_at(&root);
    let fake = Fake::new(World {
        hang_on: Some("codesign --force".into()),
        ..World::default()
    });
    let stopped = sign(&app, &SignWith::AdHoc, &root, &fake).unwrap_err();
    assert_eq!(
        stopped,
        format!(
            "macOS's code-signing service (trustd) did not answer within 60 s; {} is \
             assembled but not signed and nothing was installed — build again once it \
             answers (a restart clears a wedged trustd)",
            app.display()
        )
    );
    let fake = Fake::new(World {
        fail_on: Some("codesign --verify".into()),
        ..World::default()
    });
    let failed = sign(&app, &SignWith::AdHoc, &root, &fake).unwrap_err();
    assert!(failed.contains("codesign --verify --strict"), "{failed}");
    assert!(failed.contains("scripted failure"), "{failed}");
}

#[test]
fn install_moves_a_verified_copy_into_place() {
    let (_dir, root) = scratch();
    let app = bundle_at(&root.join("dist"));
    let dest = root.join("Applications");
    let installed = dest.join(DEV_APP_NAME);
    let exe = installed.join("Contents/MacOS/aterm");
    let fake = Fake::new(World::default());
    fake.0
        .borrow_mut()
        .versions
        .insert(exe.clone(), "aterm 0.104.0 (dev)".into());
    install(&app, &dest, false, None, &fake).expect("install");
    assert_eq!(std::fs::read(&exe).unwrap(), b"exe");
    assert_eq!(
        std::fs::read_link(installed.join("Contents/MacOS/aterm-ctl")).ok(),
        Some(PathBuf::from("aterm"))
    );
    assert_eq!(
        entries(&dest),
        [INSTALL_LOCK, DEV_APP_NAME],
        "no staging left"
    );
    let calls = log(&fake);
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("codesign --verify --strict"))
    );
    assert!(calls.contains(&"aterm --version".to_string()), "{calls:?}");
    assert!(!calls.iter().any(|c| c.starts_with("lsregister")));

    std::fs::write(app.join("Contents/MacOS/aterm"), b"exe 2").unwrap();
    install(&app, &dest, true, None, &fake).expect("reinstall");
    assert_eq!(std::fs::read(&exe).unwrap(), b"exe 2");
    assert!(log(&fake).contains(&format!("lsregister -f {}", installed.display())));

    std::fs::write(app.join("Contents/MacOS/aterm"), b"exe 3").unwrap();
    fake.0.borrow_mut().fail_on = Some("codesign --verify".into());
    assert!(install(&app, &dest, false, None, &fake).is_err());
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        b"exe 2",
        "the previous install stands"
    );
    assert_eq!(
        entries(&dest),
        [INSTALL_LOCK, DEV_APP_NAME],
        "no staging left"
    );
}

/// An install whose first `--version` never exited stays exactly where it is, the next install
/// refused, until its hold is spent; an install then replaces it.
#[test]
fn an_install_that_never_showed_it_ran_stays_until_its_hold_is_spent() {
    let (_dir, root) = scratch();
    let app = bundle_at(&root.join("dist"));
    let dest = root.join("Applications");
    let exe = dest.join(DEV_APP_NAME).join("Contents/MacOS/aterm");
    let marks = dest.join(EXEC_MARKS);
    let fake = Fake::new(World {
        hang_on: Some("aterm --version".into()),
        ..World::default()
    });
    install(&app, &dest, false, None, &fake).expect("installed, its version unknown");
    assert!(marks.join(aterm_uds::deadline::EXEC_STAMP).is_file());
    assert!(!marks.join(RAN).exists());

    std::fs::write(app.join("Contents/MacOS/aterm"), b"exe 2").unwrap();
    let refused = install(&app, &dest, false, None, &fake).unwrap_err();
    assert!(refused.contains("never showed it ran"), "{refused}");
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        b"exe",
        "the unproven install stays"
    );

    std::fs::write(
        marks.join(aterm_uds::deadline::EXEC_STAMP),
        u64::MAX.to_string(),
    )
    .unwrap();
    fake.0.borrow_mut().hang_on = None;
    fake.0
        .borrow_mut()
        .versions
        .insert(exe.clone(), "aterm 0.104.0 (dev)".into());
    install(&app, &dest, false, None, &fake).expect("the hold is spent");
    assert_eq!(std::fs::read(&exe).unwrap(), b"exe 2");
    assert!(!marks.exists(), "an exit of its own takes the marks away");
}

/// An install into a directory another install holds refuses before it looks or
/// removes anything there, and runs once that one ends.
#[test]
fn an_install_into_a_directory_another_install_holds_refuses_and_touches_nothing() {
    let (_dir, root) = scratch();
    let app = bundle_at(&root.join("dist"));
    let dest = root.join("Applications");
    let exe = dest.join(DEV_APP_NAME).join("Contents/MacOS/aterm");
    let fake = Fake::new(World::default());
    fake.0
        .borrow_mut()
        .versions
        .insert(exe.clone(), "aterm 0.104.0 (dev)".into());
    install(&app, &dest, false, None, &fake).expect("install");

    let me = aterm_toolchain::named_lock::Holder::me("a test", &root);
    let other = crate::trust_lane::RunLock::take(&dest.join(INSTALL_LOCK), &me, "a test holds it")
        .expect("free once an install returned");
    std::fs::write(app.join("Contents/MacOS/aterm"), b"exe 2").unwrap();
    let refused = install(&app, &dest, false, None, &fake).unwrap_err();
    assert!(
        refused.contains("another ship dev-app is installing here"),
        "{refused}"
    );
    assert_eq!(std::fs::read(&exe).unwrap(), b"exe", "the install stands");
    assert_eq!(
        entries(&dest),
        [INSTALL_LOCK, DEV_APP_NAME],
        "nothing staged"
    );

    drop(other);
    install(&app, &dest, false, None, &fake).expect("the other install ended");
    assert_eq!(std::fs::read(&exe).unwrap(), b"exe 2");
}

#[test]
fn the_path_link_is_a_choice_that_names_what_it_displaced() {
    let (_dir, root) = scratch();
    let bin = root.join("bin");
    let release = root.join("aterm.app/Contents/MacOS/aterm");
    std::fs::create_dir_all(release.parent().unwrap()).unwrap();
    std::fs::write(&release, b"release").unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&release, bin.join("aterm")).unwrap();
    let exe = root.join("dev/aterm");
    let fake = Fake::new(World::default());
    assert_eq!(link_cli(&exe, &bin, &fake), Ok(Some(release.clone())));
    assert_eq!(
        std::fs::read_link(bin.join("aterm")).ok(),
        Some(exe.clone())
    );
    assert!(log(&fake).contains(&"aterm --version".to_string()));
    assert_eq!(link_cli(&exe, &bin, &fake), Ok(None), "already there");

    std::fs::remove_file(bin.join("aterm")).unwrap();
    std::fs::write(bin.join("aterm"), b"a copied binary").unwrap();
    assert_eq!(link_cli(&exe, &bin, &fake), Ok(None));
    assert_eq!(
        std::fs::read(bin.join("aterm")).unwrap(),
        b"a copied binary"
    );
}

#[test]
fn the_editor_link_goes_with_aterm() {
    let (_dir, root) = scratch();
    let bin = root.join("bin");
    let bundle = |app: &str, editor: bool| -> PathBuf {
        let macos = root.join(app).join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("aterm"), app.as_bytes()).unwrap();
        if editor {
            for alias in ["amacs", "agh"] {
                std::os::unix::fs::symlink("aterm", macos.join(alias)).unwrap();
            }
        }
        macos
    };
    let release = bundle("aterm.app", true);
    let dev = bundle("aterm (dev).app", true);
    let older = bundle("older (dev).app", false);
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["aterm", "amacs", "agh"] {
        std::os::unix::fs::symlink(release.join(name), bin.join(name)).unwrap();
    }
    let target = |name: &str| std::fs::read_link(bin.join(name)).ok();
    let fake = Fake::new(World::default());

    assert_eq!(
        link_cli(&dev.join("aterm"), &bin, &fake),
        Ok(Some(release.join("aterm")))
    );
    assert_eq!(target("aterm"), Some(dev.join("aterm")));
    assert_eq!(target("amacs"), Some(dev.join("amacs")));
    assert_eq!(target("agh"), Some(dev.join("agh")));
    assert_eq!(log(&fake), vec!["aterm --version".to_string()]);

    assert_eq!(
        link_cli(&older.join("aterm"), &bin, &fake),
        Ok(Some(dev.join("aterm")))
    );
    assert_eq!(target("aterm"), Some(older.join("aterm")));
    assert_eq!(
        target("amacs"),
        Some(dev.join("amacs")),
        "no editor in the bundle, no link"
    );
    assert_eq!(target("agh"), Some(dev.join("agh")));

    std::fs::remove_file(bin.join("amacs")).unwrap();
    std::fs::write(bin.join("amacs"), b"someone's own").unwrap();
    assert_eq!(
        link_cli(&dev.join("aterm"), &bin, &fake),
        Ok(Some(older.join("aterm")))
    );
    assert_eq!(std::fs::read(bin.join("amacs")).unwrap(), b"someone's own");
}
