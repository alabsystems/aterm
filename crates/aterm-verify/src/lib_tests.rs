// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_production_planner_pulls_forge_only_for_a_dev_touch_of_a_fork() {
    let manifest_dir = aterm_tempfile::manifest_dir();
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("aterm-verify is two directories below the workspace root");
    let parsed = |words: &[&str]| {
        let parsed = cli::parse(words.iter().copied()).expect("the invocation parses");
        let scope = Scope::packages(&parsed.packages);
        planned_checks(&parsed, &scope, root).0
    };

    let fork = parsed(&["-p", "aterm-window"]);
    assert!(fork.contains(&Check::Forge), "default dev plan: {fork:?}");
    assert!(
        checks::stages_of(&fork).contains(&plan::StageId::Forge),
        "`verify -p aterm-window` must carry the Forge stage that runs OB-7: {fork:?}"
    );
    let unrelated = parsed(&["-p", "aterm-types"]);
    assert!(
        !unrelated.contains(&Check::Forge),
        "an unrelated package does not pay for Forge: {unrelated:?}"
    );
    let explicit_lint = parsed(&["lint", "-p", "aterm-window"]);
    assert!(
        !explicit_lint.contains(&Check::Forge),
        "an explicitly narrow non-dev invocation stays exact: {explicit_lint:?}"
    );

    let dev = cli::parse(std::iter::empty::<&str>()).expect("default invocation parses");
    let vendored_seed = Scope::changed("origin/main", vec!["aterm-gui".into()], true)
        .with_seeds(vec!["aterm-window".into()]);
    let changed = planned_checks(&dev, &vendored_seed, root).0;
    assert!(
        changed.contains(&Check::Forge),
        "a touched vendored seed need not itself be a workspace cone member: {changed:?}"
    );
    let dependent_only = Scope::changed(
        "origin/main",
        vec!["aterm-window".into(), "aterm-gui".into()],
        true,
    )
    .with_seeds(vec!["aterm-gui".into()]);
    let dependent = planned_checks(&dev, &dependent_only, root).0;
    assert!(
        !dependent.contains(&Check::Forge),
        "a fork merely present in the dependent cone was not touched: {dependent:?}"
    );
}

/// The live AppKit drivers are the missing tests of the two packages they drive, so an
/// ordinary macOS dev run (and therefore `verify land`) pulls them when their resolved cone
/// is present. Negative controls: a non-macOS host, an explicitly narrow check and
/// release-shaped reruns all retain their exact selections.
#[test]
fn the_production_planner_pulls_appkit_for_resolved_macos_dev_inputs() {
    let manifest_dir = aterm_tempfile::manifest_dir();
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("aterm-verify is two directories below the workspace root");
    let parse = |words: &[&str]| cli::parse(words.iter().copied()).expect("the invocation parses");
    let dev = parse(&[]);
    let direct = |package: &str| {
        Scope::changed(
            "origin/main",
            vec!["aterm-window".into(), "aterm-gui".into()],
            true,
        )
        .with_seeds(vec![package.into()])
    };

    for package in ["aterm-gui", "aterm-window"] {
        let scope = direct(package);
        let checks = planned_checks_for_host(&dev, &scope, root, true).0;
        assert!(
            checks.contains(&Check::Appkit),
            "a macOS dev cone containing {package} must run its live drivers: {checks:?}"
        );
    }

    let dependent_only = Scope::changed(
        "origin/main",
        vec!["aterm-window".into(), "aterm-gui".into()],
        true,
    )
    .with_seeds(vec!["aterm-types".into()]);
    let dependent = planned_checks_for_host(&dev, &dependent_only, root, true).0;
    assert!(
        dependent.contains(&Check::Appkit),
        "a changed dependency of an AppKit package can change its live drivers: {dependent:?}"
    );

    let gui = direct("aterm-gui");
    let non_macos = planned_checks_for_host(&dev, &gui, root, false).0;
    assert!(
        !non_macos.contains(&Check::Appkit),
        "a host without AppKit cannot acquire its verdict: {non_macos:?}"
    );
    for exact in [
        parse(&["lint"]),
        parse(&["release", "--checks", "test"]),
        parse(&["test", "--all", "--as-release"]),
    ] {
        let checks = planned_checks_for_host(&exact, &gui, root, true).0;
        assert!(
            !checks.contains(&Check::Appkit),
            "an explicit or release-shaped selection stays exact: {checks:?}"
        );
    }
}

#[test]
fn background_runs_admit_their_disk_as_background() {
    assert_eq!(
        run_admit_mode(true, aterm_capacity::Admit::Wait),
        aterm_capacity::Admit::Preemptible
    );
    assert_eq!(
        run_admit_mode(false, aterm_capacity::Admit::Wait),
        aterm_capacity::Admit::Wait,
        "negative control: an ordinary verifier keeps its requested mode"
    );
    assert_eq!(
        run_admit_mode(false, aterm_capacity::Admit::Now),
        aterm_capacity::Admit::Now,
        "negative control: --admit now remains an ordinary override"
    );
}

#[cfg(unix)]
#[test]
fn scratch_permission_changes_refuse_links_and_regular_files() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = root.path().join("directory");
    std::fs::create_dir(&directory).unwrap();
    set_dir_mode_nofollow(&directory, 0o700).unwrap();
    let link = root.path().join("link");
    symlink(&directory, &link).unwrap();
    assert!(set_dir_mode_nofollow(&link, 0o777).is_err());
    assert_eq!(
        std::fs::metadata(&directory).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    let file = root.path().join("file");
    std::fs::write(&file, b"untouched").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(set_dir_mode_nofollow(&file, 0o777).is_err());
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
        0o600
    );
}

/// A child's captured output past the cap on the files verify reads is read whole (a sparse
/// file, so the test writes no 128 MiB to disk).
#[test]
fn a_childs_output_is_read_whole_past_the_file_cap() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let out = dir.path().join("stdout");
    std::fs::File::create(&out)
        .unwrap()
        .set_len(MAX_VERIFY_FILE_BYTES + 1)
        .unwrap();
    assert!(read_regular(&out).is_err(), "an input file past the cap");
    let read = read_captured(&out).expect("a child's output past the cap");
    assert_eq!(
        u64::try_from(read.len()).unwrap(),
        MAX_VERIFY_FILE_BYTES + 1
    );
}

#[cfg(unix)]
#[test]
fn child_tools_find_native_packages_after_the_callers_choices() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let caller = dir.path().join("caller");
    let native = dir.path().join("native");
    standin(&caller.join("oracle"), "printf caller");
    standin(&native.join("bin/oracle"), "printf native");
    let ctx = Ctx::new(
        dir.path().into(),
        Scope::workspace(),
        EnvSnapshot {
            path: caller.as_os_str().to_owned(),
            native_packages: Some(native),
            ..EnvSnapshot::default()
        },
        dir.path().into(),
    );
    let run = || {
        exec::run(
            &exec::Cmd::new(aterm_uds::os_path!("sh")).args(["-c", "oracle"]),
            ctx.exec_env(),
        )
    };
    let first = run();
    assert!(first.ok, "{}", first.output);
    assert_eq!(first.output, "caller");
    std::fs::remove_file(caller.join("oracle")).unwrap();
    let second = run();
    assert!(second.ok, "{}", second.output);
    assert_eq!(second.output, "native");
    assert_eq!(
        native_package_path(caller.as_os_str(), None),
        caller.as_os_str(),
        "an absent or disabled native prefix adds no search path"
    );
}

/// A fixture going removes no name of the file another fixture's stand-in of the same text
/// runs ([`standin`]): that file has one name, outside every fixture, and the stand-in
/// still reads its body beside its own path.
#[cfg(unix)]
#[test]
fn a_fixture_going_leaves_the_file_another_standin_runs_its_one_name() {
    use std::os::unix::fs::MetadataExt as _;
    let gone = mktemp_dir("atv-standin-gone").expect("mktemp");
    let kept = mktemp_dir("atv-standin-kept").expect("mktemp");
    standin_body(&gone.join("tool"), "exit 6");
    standin_body(&kept.join("tool"), "exit 7");
    std::fs::remove_dir_all(&gone).expect("the first fixture goes");
    let ran = aterm_uds::spawnfd::command(kept.join("tool"))
        .status()
        .map(|s| s.code());
    let runs = std::fs::canonicalize(kept.join("tool")).expect("the stand-in resolves");
    let fixture = std::fs::canonicalize(&kept).expect("the fixture resolves");
    let names = std::fs::metadata(&runs).map(|m| m.nlink());
    std::fs::remove_dir_all(&kept).expect("the second fixture goes");
    assert_eq!(ran.ok(), Some(Some(7)), "the stand-in ran its own body");
    assert!(
        !runs.starts_with(&fixture),
        "{} runs from inside its fixture",
        runs.display()
    );
    assert_eq!(
        names.ok(),
        Some(1),
        "{} has names a fixture removes",
        runs.display()
    );
}

#[test]
fn a_borrowed_tools_build_is_held_for_the_run() {
    #[cfg(unix)]
    if !aterm_uds::test_binary::isolated_lifecycle(
        "tests::a_borrowed_tools_build_is_held_for_the_run",
    ) {
        return;
    }
    use aterm_toolchain::lease::{self, Holders, Subject};
    let home = std::fs::canonicalize(mktemp_dir("atv-lease").expect("mktemp")).expect("real");
    let env = EnvSnapshot {
        home: home.clone(),
        xdg_config_home: Some(home.join("xdg")),
        ..EnvSnapshot::default()
    };
    let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    let managed = prefix.join("store/trust/9192/bin");
    std::fs::create_dir_all(&managed).expect("store build");
    let own = home.join("trust/build/host/stage2/bin");
    let tools = Toolchain {
        stage2_dir: own.clone(),
        targo: own.join("targo"),
        trustdoc: own.join("trustdoc"),
        tippy: Some(managed.join(toolchain::TIPPY)),
        trustfmt: Some(managed.join(toolchain::TRUSTFMT)),
        borrowed: vec![
            (toolchain::TRUSTFMT, managed.join(toolchain::TRUSTFMT)),
            (toolchain::TIPPY, managed.join(toolchain::TIPPY)),
        ],
        refused: None,
        store_bin: Some(prefix.join("store/trust/current/bin")),
        demoted: None,
        lock: None,
        lock_refusal: None,
        lock_override: None,
    };
    let subject = Subject::of_dir(&prefix, &managed).expect("a store build");
    let (held, said) = hold_toolchain(&env, &tools, Path::new("/repo"));
    assert_eq!(held.len(), 1, "one build, held once");
    assert_eq!(held[0].subject(), &subject);
    assert!(matches!(
        lease::holders(&prefix, &subject),
        Holders::Held(_)
    ));
    let said = said.expect("said in the log");
    assert!(said.contains("trust build 9192"), "{said}");
    // A lease this run never offered to a child lets go by LOCK_UN at its drop, so a
    // sibling spawn's copy, held until its exec, does not keep it held.
    #[cfg(unix)]
    let fork = aterm_uds::fork_window::Parked::park();
    drop(held);
    assert_eq!(lease::holders(&prefix, &subject), Holders::Free);
    #[cfg(unix)]
    assert!(fork.release().success());

    let own_only = Toolchain {
        borrowed: Vec::new(),
        ..tools
    };
    let (none, said) = hold_toolchain(&env, &own_only, Path::new("/repo"));
    assert!(none.is_empty(), "an unmanaged directory takes no lease");
    assert_eq!(said, None);
    assert_eq!(lease::holders(&prefix, &subject), Holders::Free);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let runnable = |path: &Path, body: &str| {
            std::fs::create_dir_all(path.parent().expect("bin")).expect("mkdir");
            std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            aterm_uds::spawnfd::command(path)
                .output()
                .expect("a stand-in just laid runs");
        };
        for bin in [&own, &managed] {
            std::fs::create_dir_all(bin.parent().expect("sysroot").join("lib/rustlib"))
                .expect("rustlib");
        }
        runnable(&own.join("targo"), "exit 0");
        runnable(
            &own.join("trustc"),
            &format!("echo '{}'", own.parent().expect("sysroot").display()),
        );
        runnable(&managed.join(toolchain::TIPPY), "exit 0");
        let rustup = home.join("rustup");
        std::fs::create_dir_all(rustup.join("toolchains")).expect("toolchains");
        std::os::unix::fs::symlink(
            own.parent().expect("sysroot"),
            rustup.join("toolchains/trust"),
        )
        .expect("rustup entry");
        let shims = prefix.join("bin");
        runnable(&shims.join(toolchain::TIPPY), "exit 0");
        runnable(
            &shims.join("trustc"),
            &format!("echo '{}'", managed.parent().expect("sysroot").display()),
        );
        let discovered = Toolchain::discover_with_store(
            None,
            &rustup,
            Some(&prefix),
            shims.as_os_str(),
            Some("trust"),
            None,
        );
        let (held, _) = hold_toolchain(&env, &discovered, Path::new("/repo"));
        assert!(
            matches!(lease::holders(&prefix, &subject), Holders::Held(_)),
            "BORROWED_SHIM_MUST_HOLD_THE_PHYSICAL_BUILD"
        );
        assert_eq!(held.len(), 1);
        assert_eq!(discovered.tippy, Some(managed.join(toolchain::TIPPY)));
        drop(held);
        aterm_uds::deadline::once_released(|| match lease::holders(&prefix, &subject) {
            Holders::Free => Ok(()),
            Holders::Held(_) => Err(std::io::ErrorKind::WouldBlock.into()),
            Holders::Unknown(error) => panic!("lease probe failed: {error}"),
        });
        assert_eq!(lease::holders(&prefix, &subject), Holders::Free);
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn every_child_builds_in_the_callers_target_dir_and_gets_the_threads_asked_for() {
    let theirs = Ctx::new(
        PathBuf::from("/repo"),
        Scope::workspace(),
        EnvSnapshot {
            cargo_target_dir: Some(PathBuf::from("/fast/tgt")),
            ..EnvSnapshot::default()
        },
        PathBuf::from("/tmp"),
    );
    let has = |c: &Ctx, k: &str, v: &str| {
        c.exec_env()
            .add_env
            .iter()
            .any(|(ek, ev)| ek == k && ev == v)
    };
    assert!(has(&theirs, "CARGO_TARGET_DIR", "/fast/tgt"));
    assert!(
        !theirs
            .child_env_add
            .iter()
            .any(|(k, _)| k == "RUST_TEST_THREADS")
    );
    let env = theirs.exec_env();
    assert_eq!(env.remove_env, CHILD_ENV_REMOVED);
    for k in ["CARGO_MANIFEST_DIR", "CARGO_PKG_*", "CARGO_PRIMARY_PACKAGE"] {
        assert!(env.remove_env.contains(&k), "{k}");
    }
    let threaded = theirs.with_test_threads(std::num::NonZeroU32::new(3));
    // The threads asked for, within this process's share (its grant narrows it under verify).
    let asked = 3.min(threaded.test_share()).to_string();
    assert!(has(&threaded, "RUST_TEST_THREADS", &asked));
}

#[test]
fn isolated_side_targets_preserve_native_layout_and_do_not_reach_test_children() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = physical(scratch.path());
    let native = root.join("native.noindex");
    let ctx = Ctx::new(
        root.clone(),
        Scope::workspace(),
        EnvSnapshot {
            cargo_target_dir: Some(native.clone()),
            side_lanes_dir: Some(PathBuf::from("isolated.noindex")),
            ..EnvSnapshot::default()
        },
        PathBuf::from("/tmp"),
    );
    assert_eq!(plan::lane_dir(&ctx, plan::Lane::MainTarget), Some(native));
    assert_eq!(
        plan::lane_dir(&ctx, plan::Lane::TippyTarget),
        Some(root.join("isolated.noindex/tippy.noindex"))
    );
    assert!(ctx.exec_env().remove_env.contains(&SIDE_LANES_ENV));
    assert!(
        !ctx.exec_env()
            .add_env
            .iter()
            .any(|(name, _)| name == SIDE_LANES_ENV)
    );
}

/// A run's children get the temp directory's quiet folder as `TMPDIR` on macOS. The loud
/// directory is a private scratch one, not `/tmp`, whose shared quiet folder another program
/// or user may have made in a form this one refuses (verify passes `std::env::temp_dir()`).
/// NEGATIVE CONTROL: a temp directory quiet already, and any off macOS, is left as it is.
#[test]
fn stage_children_get_a_quiet_tmpdir() {
    let ctx = || {
        Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    };
    let tmpdir = |c: &Ctx| {
        c.child_env_add
            .iter()
            .find(|(k, _)| k == "TMPDIR")
            .map(|(_, v)| PathBuf::from(v))
    };
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    assert_eq!(tmpdir(&ctx().with_quiet_tmpdir(scratch.path())), None);
    let loud = aterm_tempfile::Builder::new()
        .heard()
        .tempdir()
        .expect("a loud scratch directory");
    let got = tmpdir(&ctx().with_quiet_tmpdir(loud.path()));
    if cfg!(target_os = "macos") {
        let got = got.expect("a quiet TMPDIR");
        assert_eq!(got, loud.path().join(".ubd"));
        assert!(got.as_os_str().to_string_lossy().ends_with("/.ubd/"));
        assert!(aterm_tempfile::is_quiet(scratch.path()), "{scratch:?}");
    } else {
        assert_eq!(got, None);
    }
}

#[test]
fn the_main_lane_uses_the_quiet_cache_even_with_an_older_shim() {
    let tmp = std::fs::canonicalize(mktemp_dir("atv-cache").expect("mktemp")).expect("real");
    let home = tmp.join("home");
    let root = home.join("Developer/repo");
    std::fs::create_dir_all(home.join("Library/Caches")).expect("mkdir");
    std::fs::create_dir_all(&root).expect("mkdir");
    let env = EnvSnapshot {
        home: home.clone(),
        ..EnvSnapshot::default()
    };
    let got = main_build_dir(&env, &root);
    if cfg!(target_os = "macos") {
        assert_eq!(
            got,
            Some(format!(
                "build.build-dir='{}/Library/Caches/aterm/.ubd/cargo-build.noindex\
                 {{workspace-root}}/.cargo-build'",
                home.display()
            ))
        );
        let c = Ctx::new(
            root.clone(),
            Scope::workspace(),
            env.clone(),
            PathBuf::from("/tmp"),
        );
        assert_eq!(c.main_build_dir, got);
        let cache = disk::build_dir_path(got.as_deref().unwrap(), &root).unwrap();
        assert_eq!(plan::lanes_dir(&c), cache.join("verify/.ubd"));
        assert!(plan::lanes_dir(&c).starts_with(&cache));
        assert!(!plan::lanes_dir(&c).starts_with(&root));
        let disk = disk::run_of(&c).unwrap().unwrap();
        assert!(disk.dirs.contains(&cache));
        let lane = plan::lane_dir(&c, plan::Lane::TippyTarget).unwrap();
        std::fs::create_dir_all(&lane).unwrap();
        std::fs::write(lane.join("retained-artifact"), b"cached").unwrap();
        std::fs::remove_dir(&root).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let recreated = Ctx::new(
            root.clone(),
            Scope::workspace(),
            env.clone(),
            PathBuf::from("/tmp"),
        );
        let retained = plan::lane_dir(&recreated, plan::Lane::TippyTarget).unwrap();
        assert_eq!(retained, lane);
        assert_eq!(
            std::fs::read(retained.join("retained-artifact")).unwrap(),
            b"cached"
        );
        let other = home.join("Developer/other");
        std::fs::create_dir_all(&other).unwrap();
        let other = Ctx::new(
            other,
            Scope::workspace(),
            env.clone(),
            PathBuf::from("/tmp"),
        );
        assert_ne!(plan::lane_dir(&other, plan::Lane::TippyTarget), Some(lane));
        assert_eq!(
            c.child_env_add
                .iter()
                .find(|(k, _)| k == "CARGO_TARGET_DIR")
                .map(|(_, v)| PathBuf::from(v)),
            Some(root.join(aterm_toolchain::build_cache::DEFAULT_TARGET)),
            "pinned beside the shim's cache, which the main lane names by --config"
        );
        assert_eq!(
            c.target_dir,
            root.join(aterm_toolchain::build_cache::DEFAULT_TARGET)
        );
        for var in &CARGO_LAYOUT_VARS[..3] {
            let chosen = EnvSnapshot {
                cargo_layout: vec![(*var, OsString::new())],
                ..env.clone()
            };
            assert_eq!(main_build_dir(&chosen, &root), None, "{var}");
            let selected = Ctx::new(
                root.clone(),
                Scope::workspace(),
                chosen,
                PathBuf::from("/tmp"),
            );
            assert_eq!(selected.cached_lanes_dir, None, "{var}");
        }
        assert_eq!(
            main_build_dir(&env, &tmp.join("elsewhere")),
            None,
            "outside HOME"
        );
        // An installed shim can describe the retired cache. Its spelling cannot change
        // this version's placement; the current shim also leaves that choice unchanged.
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        let on_path = EnvSnapshot {
            path: bin.clone().into_os_string(),
            ..env.clone()
        };
        for under in [
            aterm_toolchain::build_cache::MOVED_FROM_UNDER_HOME,
            aterm_toolchain::build_cache::CACHE_UNDER_HOME,
        ] {
            std::fs::write(
                bin.join("targo"),
                format!("#!/bin/sh\n# atpkg shim\n__atb_t=\n__atb_c=${{HOME-}}/{under}\n"),
            )
            .expect("a shim");
            assert_eq!(main_build_dir(&on_path, &root), got);
        }
    } else {
        assert_eq!(got, None, "the shim's cache is macOS only");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Caller-selected target links are physical before their spelling reaches Cargo's runtime
/// metadata. Automatic paths retain the lexical components their descriptor preparation checks;
/// on macOS a linked automatic parent refuses instead of becoming caller authority.
#[cfg(unix)]
#[test]
fn caller_targets_are_physical_and_automatic_targets_keep_their_guarded_spelling() {
    let tmp = mktemp_dir("atv-physical").expect("mktemp");
    // HOME is physical, so the shim's cache holds the checkout; the root is handed spelled
    // through a link of its own (and, on macOS, the one `/tmp` is).
    let home = std::fs::canonicalize(&tmp).expect("real").join("home");
    std::fs::create_dir_all(home.join("Library/Caches")).expect("mkdir");
    std::os::unix::fs::symlink("home", tmp.join("linked-home")).expect("link");
    let root = tmp.join("linked-home/Developer/repo");
    std::fs::create_dir_all(root.join("target.noindex")).expect("mkdir");
    std::os::unix::fs::symlink("target.noindex", root.join("target")).expect("link");
    let real_root = std::fs::canonicalize(&root).expect("real");
    let physical = real_root.join("target.noindex");
    let pinned = |c: &Ctx| {
        c.child_env_add
            .iter()
            .find(|(k, _)| k == "CARGO_TARGET_DIR")
            .map(|(_, v)| PathBuf::from(v))
    };
    let shim = EnvSnapshot {
        home: home.clone(),
        ..EnvSnapshot::default()
    };
    let linked = root.join("target");
    let theirs = EnvSnapshot {
        cargo_target_dir: Some(linked.clone()),
        cargo_layout: vec![("CARGO_TARGET_DIR", linked.into_os_string())],
        ..EnvSnapshot::default()
    };
    let shares = cfg!(target_os = "macos");
    // HOME spelled through the link holds no physical checkout, so no shim cache, as the
    // shim decides from its own physical working directory: the target is pinned all the same.
    let linked_home = EnvSnapshot {
        home: tmp.join("linked-home"),
        ..EnvSnapshot::default()
    };
    for (env, cache) in [
        (shim, shares),
        (linked_home, false),
        (EnvSnapshot::default(), false),
        (theirs, false),
    ] {
        let c = Ctx::new(
            root.clone(),
            Scope::workspace(),
            env.clone(),
            PathBuf::from("/tmp"),
        );
        assert_eq!(c.cached_lanes_dir.is_some(), cache, "{env:?}");
        assert_eq!(c.root, real_root, "{env:?}");
        let expected = if env.cargo_target_dir.is_some() {
            physical.clone()
        } else {
            real_root.join(aterm_toolchain::build_cache::DEFAULT_TARGET)
        };
        assert_eq!(c.target_dir, expected, "{env:?}");
        assert_eq!(pinned(&c), Some(expected.clone()), "{env:?}");
        assert_eq!(stages::main_bin(&c, "aterm"), expected.join("debug/aterm"));
        assert!(c.state_dir().starts_with(&expected), "{env:?}");
        #[cfg(target_os = "macos")]
        assert_eq!(
            c.native_build_plan
                .as_ref()
                .unwrap()
                .prepare_target()
                .is_ok(),
            env.cargo_target_dir.is_some()
        );
        let lanes = plan::lanes_dir(&c);
        if cache || env.cargo_target_dir.is_some() {
            assert_eq!(lanes, crate::physical(&lanes), "{env:?}");
        } else {
            assert_eq!(
                lanes,
                expected.join("verify").join(aterm_tempfile::QUIET),
                "{env:?}"
            );
            assert_ne!(
                lanes,
                crate::physical(&lanes),
                "automatic linked parents stay guarded"
            );
        }
    }
    std::fs::remove_dir_all(&tmp).expect("cleanup");
}

#[test]
fn a_pinned_main_lane_keeps_its_intermediates_out_of_spotlight() {
    let pinned = |c: &Ctx| {
        c.child_env_add
            .iter()
            .find(|(k, _)| k == "CARGO_TARGET_DIR")
            .map(|(_, v)| PathBuf::from(v))
    };
    let ctx = |root: &Path, env: EnvSnapshot| {
        Ctx::new(
            root.to_path_buf(),
            Scope::workspace(),
            env,
            PathBuf::from("/tmp"),
        )
    };
    let fast = std::path::absolute("/fast/tgt").expect("absolute");
    let theirs = EnvSnapshot {
        cargo_target_dir: Some(fast.clone()),
        cargo_layout: vec![("CARGO_TARGET_DIR", fast.clone().into_os_string())],
        ..EnvSnapshot::default()
    };
    let repo = std::path::absolute("/repo").expect("absolute");
    let c = ctx(&repo, theirs.clone());
    let fast_cfg = format!(
        "build.build-dir='{}'",
        aterm_toolchain::build_cache::target_build_path(&fast).display()
    );
    assert_eq!(pinned(&c), Some(fast));
    assert_eq!(c.main_build_dir.as_deref(), Some(fast_cfg.as_str()));
    let argv = stages::main_targo(&c, vec!["--unverified".into(), "test".into()]).argv();
    let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
    assert_eq!(
        argv[1..],
        [
            "--config",
            fast_cfg.as_str(),
            "--unverified",
            config,
            workspace,
            "test",
            aterm_toolchain::build_cache::TEST_GRAPH_FLAG
        ]
    );
    let c = ctx(&repo, EnvSnapshot::default());
    let repo_cfg = format!(
        "build.build-dir='{}'",
        aterm_toolchain::build_cache::target_build_path(
            &repo.join(aterm_toolchain::build_cache::DEFAULT_TARGET)
        )
        .display()
    );
    assert_eq!(
        pinned(&c),
        Some(repo.join(aterm_toolchain::build_cache::DEFAULT_TARGET))
    );
    assert_eq!(c.main_build_dir.as_deref(), Some(repo_cfg.as_str()));
    let chose = EnvSnapshot {
        cargo_layout: vec![("CARGO_BUILD_BUILD_DIR", OsString::new())],
        ..theirs.clone()
    };
    let c = ctx(Path::new("/repo"), chose);
    assert_eq!((pinned(&c).is_some(), c.main_build_dir), (true, None));
    let tmp = std::fs::canonicalize(mktemp_dir("atv-bdir").expect("mktemp")).expect("real");
    let root = tmp.join("repo");
    std::fs::create_dir_all(root.join(".cargo")).expect("mkdir");
    std::fs::write(
        root.join(".cargo/config.toml"),
        "[build]\nbuild-dir = 'mine'\n",
    )
    .expect("config");
    let c = ctx(&root, theirs);
    assert_eq!((pinned(&c).is_some(), c.main_build_dir), (true, None));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn invalid_caller_intermediates_refuse_before_direct_native_preparation() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = physical(temp.path());
    std::fs::create_dir(root.join(".cargo")).unwrap();
    let config = root.join(".cargo/config.toml");
    for text in [
        "build.build-dir=3",
        "build.build-dir=''",
        "build.build-dir=[",
        "include=['unknown.toml']",
    ] {
        std::fs::write(&config, text).unwrap();
        assert!(NativeBuildPlan::capture(&EnvSnapshot::default(), &root).is_err());
        let plan = NativeBuildPlan::for_default_target(&EnvSnapshot::default(), &root, &root);
        assert!(plan.prepare().is_err());
        let mut command = aterm_uds::spawnfd::command("probe");
        assert!(plan.apply_to_command(&mut command).is_err());
        assert!(!root.join("target").exists());
    }
}

#[test]
fn native_target_selection_captures_workspace_config_and_environment_precedence() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = physical(temp.path());
    std::fs::create_dir(root.join(".cargo")).unwrap();
    let config = root.join(".cargo/config.toml");
    std::fs::write(&config, "[build]\ntarget-dir='configured.noindex'\n").unwrap();
    for (layout, expected) in [
        (Vec::new(), "configured.noindex"),
        (
            vec![("CARGO_BUILD_TARGET_DIR", "build-env.noindex".into())],
            "build-env.noindex",
        ),
        (
            vec![
                ("CARGO_BUILD_TARGET_DIR", "build-env.noindex".into()),
                ("CARGO_TARGET_DIR", "target-env.noindex".into()),
            ],
            "target-env.noindex",
        ),
    ] {
        let plan = NativeBuildPlan::capture(
            &EnvSnapshot {
                cargo_layout: layout,
                ..EnvSnapshot::default()
            },
            &root,
        )
        .unwrap();
        assert_eq!(plan.target(), root.join(expected));
        assert!(plan.default_root.is_none());
        assert!(!plan.target().exists(), "selection creates no target");
    }
    assert!(
        NativeBuildPlan::capture(
            &EnvSnapshot {
                cargo_layout: vec![("CARGO_TARGET_DIR", OsString::new())],
                ..EnvSnapshot::default()
            },
            &root
        )
        .is_err()
    );
    for invalid in ["[build\n", "include=['unknown.toml']\n"] {
        std::fs::write(&config, invalid).unwrap();
        assert!(NativeBuildPlan::capture(&EnvSnapshot::default(), &root).is_err());
    }
    std::fs::write(&config, "").unwrap();
    let plan = NativeBuildPlan::capture(&EnvSnapshot::default(), &root).unwrap();
    assert_eq!(plan.default_root.as_deref(), Some(root.as_path()));
    assert_eq!(
        plan.target(),
        root.join(aterm_toolchain::build_cache::DEFAULT_TARGET)
    );
    std::fs::write(&config, "[build]\ntarget-dir='later.noindex'\n").unwrap();
    assert_eq!(
        plan.target(),
        root.join(aterm_toolchain::build_cache::DEFAULT_TARGET)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn generated_artifacts_use_their_own_root_and_survive_source_cleanup() {
    use std::os::unix::fs::DirBuilderExt as _;
    for linked in [false, true] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let temp = physical(temp.path());
        let source = temp.join("source");
        let artifacts = temp.join("artifacts");
        let outside = temp.join("outside");
        for path in [&source, &artifacts, &outside] {
            std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        std::fs::create_dir(source.join(".cargo")).unwrap();
        std::fs::write(
            source.join(".cargo/config.toml"),
            "[build]\nbuild-dir='caller-intermediates'\n",
        )
        .unwrap();
        std::fs::write(outside.join("keep"), b"outside data").unwrap();
        if linked {
            std::fs::create_dir(artifacts.join("target")).unwrap();
            std::os::unix::fs::symlink(&outside, artifacts.join("target/.ubd")).unwrap();
        }
        let plan =
            NativeBuildPlan::for_default_target(&EnvSnapshot::default(), &source, &artifacts);
        assert_eq!(
            plan.target(),
            artifacts.join(aterm_toolchain::build_cache::DEFAULT_TARGET)
        );
        assert!(
            plan.build_dir().is_none(),
            "intermediates read the source configuration"
        );
        let mut ran = false;
        let mut run = || -> std::io::Result<()> {
            plan.prepare()?;
            let out = aterm_uds::spawnfd::command("/bin/echo")
                .arg("artifact-retained")
                .current_dir(&source)
                .output()?;
            assert!(out.status.success());
            ran = true;
            std::fs::write(plan.target().join("retained"), out.stdout)
        };
        assert_eq!(run().is_ok(), !linked);
        assert_eq!(ran, !linked);
        assert!(!source.join("target").exists());
        std::fs::remove_dir_all(&source).unwrap();
        if !linked {
            assert_eq!(
                std::fs::read(plan.target().join("retained")).unwrap(),
                b"artifact-retained\n"
            );
        } else {
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
        }
        assert_eq!(
            std::fs::read(outside.join("keep")).unwrap(),
            b"outside data"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_final_targets_prepare_from_captured_provenance_before_any_stage() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    fn stage(ctx: &Ctx, spec: &plan::StageSpec) -> Report {
        let out = aterm_uds::spawnfd::command("/bin/echo")
            .arg("native-stage-started")
            .output()
            .unwrap();
        assert!(out.status.success());
        std::fs::write(ctx.root.join("native-ran"), out.stdout).unwrap();
        let mut report = Report::new(spec.title.clone());
        report.pass("native stage ran");
        report
    }
    for shape in [
        "fallback",
        "shared",
        "override",
        "parent-link",
        "final-link",
        "configured-link",
        "env-link",
        "external-env-link",
        "unmanaged-env-link",
        "configured-default-link",
        "changed-config",
        "child-target-changed",
        "malformed",
    ] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let temp = physical(temp.path());
        let home = temp.join("home");
        let root = home.join("source");
        let owned_target = shape == "env-link";
        let outside = if owned_target {
            home.join("owned-output.noindex")
        } else if shape == "unmanaged-env-link" {
            home.join("unmanaged-output.noindex")
        } else {
            temp.join("outside")
        };
        for path in [&home, &root] {
            std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        if owned_target {
            // An environment target selects its own fallback even with a shared cache.
            // Establish actual final-output custody before linking to that destination.
            assert!(
                atpkg::build_cache::registered::create_checked(&home, &outside, &root).unwrap()
            );
        } else {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&outside)
                .unwrap();
        }
        std::fs::write(outside.join("keep"), b"outside data").unwrap();
        let original_mode = std::fs::metadata(&outside).unwrap().permissions().mode();
        std::fs::create_dir(root.join(".cargo")).unwrap();
        let shared = matches!(
            shape,
            "shared"
                | "parent-link"
                | "final-link"
                | "child-target-changed"
                | "configured-link"
                | "configured-default-link"
        );
        if shared || matches!(shape, "external-env-link" | "unmanaged-env-link") {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(home.join("Library/Caches"))
                .unwrap();
        }
        let config = root.join(".cargo/config.toml");
        let final_target = root.join(aterm_toolchain::build_cache::DEFAULT_TARGET);
        let mut env = EnvSnapshot {
            home: if shape == "fallback" {
                PathBuf::new()
            } else {
                home.clone()
            },
            ..EnvSnapshot::default()
        };
        match shape {
            "parent-link" | "configured-default-link" => {
                std::fs::create_dir(root.join("target")).unwrap();
                std::os::unix::fs::symlink(&outside, root.join("target/.ubd")).unwrap();
                if shape == "configured-default-link" {
                    std::fs::write(
                        &config,
                        format!(
                            "[build]\ntarget-dir='{}'\n",
                            aterm_toolchain::build_cache::DEFAULT_TARGET
                        ),
                    )
                    .unwrap();
                }
            }
            "final-link" => {
                std::fs::create_dir_all(final_target.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(&outside, &final_target).unwrap();
            }
            "configured-link" | "env-link" | "external-env-link" | "unmanaged-env-link" => {
                let selected = root.join("selected.noindex");
                std::os::unix::fs::symlink(&outside, &selected).unwrap();
                if shape == "configured-link" {
                    std::fs::write(&config, "[build]\ntarget-dir='selected.noindex'\n").unwrap();
                } else {
                    env.cargo_layout
                        .push(("CARGO_BUILD_TARGET_DIR", selected.into_os_string()));
                }
            }
            "override" => env.cargo_layout.push((
                "CARGO_BUILD_BUILD_DIR",
                temp.join("caller-build").into_os_string(),
            )),
            "malformed" => std::fs::write(&config, "[build\n").unwrap(),
            _ => {}
        }
        let mut ctx = Ctx::new(
            root.clone(),
            Scope::crate_only("probe"),
            env,
            temp.join("scratch"),
        )
        .with_checks(&[Check::Test]);
        if shape == "changed-config" {
            std::fs::write(&config, "[build]\ntarget-dir='later.noindex'\n").unwrap();
        }
        if shape == "child-target-changed" {
            ctx.child_env_add
                .push(("CARGO_TARGET_DIR".into(), outside.clone().into_os_string()));
        }
        if matches!(
            shape,
            "fallback" | "shared" | "override" | "parent-link" | "final-link" | "changed-config"
        ) {
            assert_eq!(ctx.target_dir, final_target, "{shape}");
        }
        if shape == "fallback" {
            assert_eq!(
                ctx.main_build_dir,
                aterm_toolchain::build_cache::target_build_config(&final_target)
            );
        }
        if matches!(
            shape,
            "configured-link" | "env-link" | "external-env-link" | "unmanaged-env-link"
        ) {
            assert_eq!(ctx.target_dir, outside, "{shape}");
        }
        if shape == "configured-default-link" {
            assert_eq!(ctx.target_dir, outside.join("main.noindex"));
        }
        if matches!(
            shape,
            "env-link" | "external-env-link" | "unmanaged-env-link"
        ) {
            assert_eq!(
                ctx.main_build_dir,
                aterm_toolchain::build_cache::target_build_config(&outside),
                "{shape}"
            );
        }
        if shared {
            assert!(ctx.main_build_dir.is_some(), "{shape}");
            assert_ne!(
                ctx.main_build_dir,
                aterm_toolchain::build_cache::target_build_config(&ctx.target_dir),
                "{shape}"
            );
        }
        if shape == "override" {
            assert!(ctx.main_build_dir.is_none());
        }
        let admitted = !matches!(
            shape,
            "parent-link"
                | "final-link"
                | "child-target-changed"
                | "unmanaged-env-link"
                | "malformed"
        );
        let mut output = Vec::new();
        let code = run_with(
            &ctx,
            &mut output,
            &mut std::io::sink(),
            &Show::default(),
            Seams {
                stage,
                prepare_cells: None,
                ..LIVE
            },
        )
        .unwrap();
        assert_eq!(
            code,
            if admitted {
                exit::PASS
            } else {
                exit::COULD_NOT_RUN
            },
            "{shape}: {}",
            String::from_utf8_lossy(&output)
        );
        assert_eq!(root.join("native-ran").exists(), admitted, "{shape}");
        let registered: Vec<_> = atpkg::build_cache::registered::roots(&home)
            .into_iter()
            .map(|root| root.root)
            .collect();
        let expected_roots = if owned_target {
            vec![outside.clone()]
        } else if shape == "unmanaged-env-link" {
            // Quiet-child custody does not acknowledge this unmarked final target.
            vec![aterm_toolchain::build_cache::target_build_path(&outside)]
        } else {
            Vec::new()
        };
        assert_eq!(registered, expected_roots, "{shape}");
        assert_eq!(
            std::fs::read(outside.join("keep")).unwrap(),
            b"outside data"
        );
        assert_eq!(
            std::fs::metadata(&outside).unwrap().permissions().mode(),
            original_mode,
            "{shape}"
        );
        if shape == "unmanaged-env-link" {
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 2);
        } else if !admitted {
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1, "{shape}");
        }
        if shape == "external-env-link" {
            assert!(aterm_toolchain::build_cache::target_build_path(&outside).is_dir());
        }
        if matches!(shape, "fallback" | "shared" | "override" | "changed-config") {
            assert!(final_target.is_dir(), "{shape}");
        }
        if shape == "fallback" {
            assert!(aterm_toolchain::build_cache::target_build_path(&final_target).is_dir());
        }
        assert!(!root.join("later.noindex").exists());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn quiet_native_children_start_only_after_safe_generated_fallback_preparation() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    fn stage(ctx: &Ctx, spec: &plan::StageSpec) -> Report {
        let out = aterm_uds::spawnfd::command("/bin/echo")
            .arg("native-compiler-started")
            .output()
            .unwrap();
        assert!(out.status.success());
        std::fs::write(ctx.root.join("native-ran"), &out.stdout).unwrap();
        let mut report = Report::new(spec.title.clone());
        report.pass("native child ran");
        report
    }
    for shape in [
        "safe",
        "parent-link",
        "parent-writable",
        "leaf-link",
        "override",
        "file-override",
        "config-changed",
        "external",
        "external-link",
        "external-fresh",
    ] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let temp = temp.path().canonicalize().unwrap();
        let home = temp.join("home");
        let owner = home.join("source");
        let external = temp.join("external-source");
        for directory in [&home, &owner, &external] {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(directory)
                .unwrap();
        }
        for root in [&owner, &external] {
            std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        }
        let target = home.join("outputs.noindex");
        let external_workspace = shape.starts_with("external");
        let fresh = shape == "external-fresh";
        let registered = !matches!(shape, "override" | "file-override") && !external_workspace;
        if registered {
            assert!(
                atpkg::build_cache::registered::create_checked(&home, &target, &owner).unwrap()
            );
        } else if !fresh {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&target)
                .unwrap();
        }
        if !fresh {
            std::fs::write(target.join("keep"), b"final output").unwrap();
        }
        let outside = temp.join("outside");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&outside)
            .unwrap();
        std::fs::write(outside.join("keep"), b"outside output").unwrap();
        let quiet = target.join(".ubd");
        let build = aterm_toolchain::build_cache::target_build_path(&target);
        match shape {
            "parent-link" | "external-link" => {
                std::os::unix::fs::symlink(&outside, &quiet).unwrap()
            }
            "parent-writable" => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&quiet)
                    .unwrap();
                std::fs::set_permissions(&quiet, std::fs::Permissions::from_mode(0o777)).unwrap();
            }
            "leaf-link" => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&quiet)
                    .unwrap();
                std::os::unix::fs::symlink(&outside, &build).unwrap();
            }
            _ => {}
        }
        let mut layout = vec![("CARGO_TARGET_DIR", target.clone().into_os_string())];
        if shape == "override" {
            layout.push((
                "CARGO_BUILD_BUILD_DIR",
                target.join("caller-chosen").into_os_string(),
            ));
        }
        if shape == "file-override" {
            std::fs::create_dir(owner.join(".cargo")).unwrap();
            std::fs::write(
                owner.join(".cargo/config.toml"),
                "[build]\nbuild-dir='caller-chosen'\n",
            )
            .unwrap();
        }
        let ctx = Ctx::new(
            if external_workspace { external } else { owner },
            Scope::crate_only("probe"),
            EnvSnapshot {
                home: home.clone(),
                cargo_target_dir: Some(target.clone()),
                cargo_layout: layout,
                ..EnvSnapshot::default()
            },
            temp.join("scratch"),
        )
        .with_checks(&[Check::Test]);
        if shape == "config-changed" {
            assert_eq!(
                ctx.main_build_dir,
                aterm_toolchain::build_cache::target_build_config(&target)
            );
            std::fs::create_dir(ctx.root.join(".cargo")).unwrap();
            std::fs::write(
                ctx.root.join(".cargo/config.toml"),
                "[build]\nbuild-dir='changed-after-selection'\n",
            )
            .unwrap();
            std::os::unix::fs::symlink(&outside, &quiet).unwrap();
        }
        let mut output = Vec::new();
        let code = run_with(
            &ctx,
            &mut output,
            &mut std::io::sink(),
            &Show::default(),
            Seams {
                stage,
                prepare_cells: None,
                ..LIVE
            },
        )
        .unwrap();
        let admitted = matches!(
            shape,
            "safe" | "override" | "file-override" | "external" | "external-fresh"
        );
        assert_eq!(
            code,
            if admitted {
                exit::PASS
            } else {
                exit::COULD_NOT_RUN
            },
            "{shape}: {}",
            String::from_utf8_lossy(&output)
        );
        assert_eq!(ctx.root.join("native-ran").exists(), admitted, "{shape}");
        if admitted {
            assert_eq!(
                std::fs::read(ctx.root.join("native-ran")).unwrap(),
                b"native-compiler-started\n"
            );
        }
        assert_eq!(
            atpkg::build_cache::registered::roots(&home).len(),
            usize::from(registered),
            "{shape}"
        );
        if !fresh {
            assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"final output");
        }
        assert_eq!(
            std::fs::read(outside.join("keep")).unwrap(),
            b"outside output"
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1, "{shape}");
        if matches!(shape, "safe" | "external" | "external-fresh") {
            assert!(build.is_dir());
        }
        if matches!(shape, "override" | "file-override") {
            assert!(!quiet.exists());
        }
    }
}

#[test]
fn requested_test_cores_narrow_but_never_grant_a_share() {
    let make = |env| {
        Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            env,
            PathBuf::from("/tmp"),
        )
    };
    let held = || {
        make(EnvSnapshot {
            build_slot_held: Some("1".into()),
            cargo_build_jobs: Some("6".into()),
            ..EnvSnapshot::default()
        })
    };
    let capped = held().with_test_cores(std::num::NonZeroU32::new(3));
    assert_eq!(capped.test_share(), 3);
    assert_eq!(capped.test_thread_override(), Some(3));
    assert_eq!(
        held()
            .with_test_cores(std::num::NonZeroU32::new(20))
            .test_share(),
        6
    );
    let unheld = make(EnvSnapshot::default()).with_test_cores(std::num::NonZeroU32::new(1));
    assert_eq!(unheld.test_share(), 1);
    assert_eq!(
        test_workers::share(&unheld.env, None),
        Ok(test_workers::Share::Claim(None))
    );
    let malformed = make(EnvSnapshot {
        build_slot_held: Some("1".into()),
        ..EnvSnapshot::default()
    })
    .with_test_cores(std::num::NonZeroU32::new(20));
    assert_eq!(malformed.test_share(), 1);
    assert!(test_workers::share(&malformed.env, None).is_err());
}

#[test]
fn a_callers_rust_test_threads_pins_every_binary() {
    let c = Ctx::new(
        PathBuf::from("/repo"),
        Scope::workspace(),
        EnvSnapshot {
            rust_test_threads: Some("5".into()),
            ..EnvSnapshot::default()
        },
        PathBuf::from("/tmp"),
    );
    assert_eq!(c.test_threads, Some(5));
    // The pin, within the share: a share wider than it runs the pin, a narrower one (this
    // process's own parallelism, which a verifier's grant narrows) clamps it.
    assert_eq!(c.test_threads_within(8), 5);
    assert_eq!(c.test_threads_within(2), 2);
    assert_eq!(c.test_threads(), 5.min(c.test_share()));
    let unpinned = Ctx::new(
        PathBuf::from("/repo"),
        Scope::workspace(),
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    );
    assert_eq!(unpinned.test_threads, None);
}

/// A red narrowed cell's rerun judges that crate again, not the whole graph and the oracles.
#[test]
fn a_cells_rerun_keeps_the_p_that_narrowed_it() {
    let c = |scope| {
        Ctx::new(
            PathBuf::from("/repo"),
            scope,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    };
    let narrowed = c(Scope::packages(&["aterm-agent".to_string()]));
    assert!(narrowed.rerun(Check::Cells).contains(" -p aterm-agent"));
    let whole = c(Scope::workspace());
    assert!(!whole.rerun(Check::Cells).contains(" --all"));
    assert!(!whole.rerun(Check::Cells).contains(" -p "));
}

#[test]
fn a_rerun_keeps_no_windows_where_the_flag_changes_the_check() {
    let c = |no_windows| {
        Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .with_no_windows(no_windows)
    };
    let (quiet, plain) = (c(true), c(false));
    for check in [Check::Browser, Check::Appkit] {
        assert!(quiet.rerun(check).ends_with(" --no-windows"), "{check:?}");
        assert!(!plain.rerun(check).contains("--no-windows"), "{check:?}");
    }
    assert!(!quiet.rerun(Check::Fmt).contains("--no-windows"));
}

/// A bundle names its version and commit from its `Info.plist`, and HEAD built it only when
/// that commit, less `-dirty`, begins HEAD.
#[cfg(unix)]
#[test]
fn a_measured_bundle_names_the_commit_that_built_it() {
    let tmp = mktemp_dir("atv-app").expect("mktemp");
    let app = tmp.join("aterm.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).expect("mkdir");
    let bin = app.join("Contents/MacOS/aterm");
    std::fs::write(&bin, "#!/bin/sh\n").expect("write");
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let unnamed = MeasureArtifact::at(&app).expect("a bundle");
    assert_eq!(
        (unnamed.version.as_deref(), unnamed.commit.as_deref()),
        (None, None)
    );
    assert!(
        !unnamed.built_from("22cb722a2ec9ab"),
        "no commit is no build of HEAD"
    );
    std::fs::write(
        app.join("Contents/Info.plist"),
        "<dict>\n\t<key>ATermGitCommit</key>\n\t<string>22cb722a2ec9</string>\n\
         \t<key>CFBundleShortVersionString</key>\n\t<string>0.137.0</string>\n</dict>\n",
    )
    .expect("write");
    let named = MeasureArtifact::at(&app).expect("a bundle");
    assert_eq!(named.version.as_deref(), Some("0.137.0"));
    assert_eq!(named.commit.as_deref(), Some("22cb722a2ec9"));
    assert!(
        named
            .describe()
            .starts_with("aterm 0.137.0 (22cb722a2ec9) at ")
    );
    assert!(named.built_from("22cb722a2ec9abcdef0123456789abcdef012345"));
    assert!(!named.built_from("215fb53331abd9acb2a33e7d7a21d1fa3c2bd8f2"));
    let dirty = MeasureArtifact {
        commit: Some("22cb722a2ec9-dirty".into()),
        ..named
    };
    assert!(dirty.built_from("22cb722a2ec9abcdef0123456789abcdef012345"));
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A usage error is 2; no checkout or no pin is 3 with its fix, before anything runs.
#[cfg(unix)]
#[test]
fn the_cli_refuses_usage_and_a_place_it_cannot_run_from() {
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let tmp = mktemp_dir("atv-cli").expect("mktemp");
    assert_eq!(run_cli(&args(&["--fast"]), &tmp), exit::USAGE);
    assert_eq!(run_cli(&args(&["release"]), &tmp), exit::USAGE);
    assert_eq!(
        run_cli(&args(&["measure", "--app", "/nonexistent/aterm.app"]), &tmp),
        exit::USAGE
    );
    assert_eq!(
        run_cli(&args(&["fmt"]), &tmp),
        exit::COULD_NOT_RUN,
        "no checkout"
    );
    let git = crate::git_provider::command()
        .args(["init", "-q"])
        .current_dir(&tmp)
        .status()
        .expect("git");
    assert!(git.success());
    assert_eq!(
        run_cli(&args(&["fmt"]), &tmp),
        exit::COULD_NOT_RUN,
        "no rust-toolchain.toml"
    );
    assert_eq!(run_cli(&args(&["list"]), &tmp), exit::PASS);
    assert_eq!(run_cli(&args(&["--help"]), &tmp), exit::PASS);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A release's header names the half it runs, or the checks `--checks` listed, so its log
/// says which part of the release it answers for.
#[test]
fn a_release_header_names_its_half() {
    let head = |words: &[&str]| {
        let a = cli::parse(words.iter().copied()).expect("parses");
        let ctx = Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .with_checks(&a.wanted(|_| true))
        .with_release(a.release);
        header(&ctx, None)
    };
    let starts = |words: &[&str], what: &str| {
        let line = head(words);
        assert!(line.starts_with(&format!("verify: {what} — ")), "{line}");
    };
    starts(&["release", "--app", "a"], "release");
    starts(&["release", "--tree"], "release --tree");
    starts(&["release", "--bundle", "--app", "a"], "release --bundle");
    starts(&["release", "--checks", "test,spec"], "release: test spec");
}

#[test]
fn the_header_names_what_the_selection_pulled_and_why() {
    let head = |crates: &[&str]| {
        let scope = Scope::changed(
            "origin/main",
            crates.iter().map(|c| c.to_string()).collect(),
            true,
        );
        let holds = |p: &str| scope.includes_crate(p);
        let checks = checks::with_pulled(&checks::DEV, false, holds);
        let ctx = Ctx::new(
            PathBuf::from("/repo"),
            scope.clone(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .with_checks(&checks);
        header(&ctx, Some("the cone"))
    };
    assert_eq!(head(&["aterm-gui"]), "verify: dev — the cone");
    assert_eq!(head(&["aterm-gpu"]), "verify: dev — the cone");
    assert_eq!(
        head(&["aterm-core", "aterm-gpu", "aterm-gpu-web", "aterm-wasm"]),
        "verify: dev + wasm-smoke (the selection holds aterm-wasm) — the cone"
    );
    let pulled = |crates: &[&str]| {
        let scope = Scope::changed(
            "origin/main",
            crates.iter().map(|c| c.to_string()).collect(),
            true,
        );
        let holds = |p: &str| scope.includes_crate(p);
        let checks = checks::with(
            &checks::with_pulled(&checks::DEV, false, holds),
            Check::Cells,
        );
        let ctx = Ctx::new(
            PathBuf::from("/repo"),
            scope.clone(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .with_checks(&checks)
        .with_cells_pulled(true);
        header(&ctx, Some("the cone"))
    };
    assert_eq!(
        pulled(&["aterm-gui"]),
        "verify: dev + cells (each other OS's cell and one of this OS's that builds a selected crate) — the cone"
    );
    assert_eq!(
        pulled(&["aterm-wasm"]),
        "verify: dev + wasm-smoke (the selection holds aterm-wasm) + cells (each other OS's cell and one of this OS's that builds a selected crate) — the cone"
    );

    if cfg!(target_os = "macos") {
        let scope = Scope::changed("origin/main", vec!["aterm-gui".into()], true);
        let checks = checks::with(&checks::DEV, Check::Appkit);
        let ctx = Ctx::new(
            PathBuf::from("/repo"),
            scope,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .with_checks(&checks);
        assert_eq!(
            header(&ctx, Some("the cone")),
            "verify: dev + appkit (the selection holds aterm-gui) — the cone"
        );
    }
}

/// A stage that commits in the run's checkout, standing in for another session's
/// fast-forward of a shared checkout mid-run.
fn committing_stage(ctx: &Ctx, spec: &plan::StageSpec) -> Report {
    let out = changed::git(&ctx.root, &ctx.path_env)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["commit", "-q", "--allow-empty", "-m", "moved"])
        .output()
        .expect("git runs");
    assert!(out.status.success(), "commit: {out:?}");
    let mut r = Report::new(spec.title.clone());
    r.pass("ran");
    r
}

fn passing_stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
    let mut r = Report::new(spec.title.clone());
    r.pass("ran");
    r
}

#[test]
fn the_tree_half_collects_every_report_after_a_red() {
    let args = cli::parse(["release", "--tree"]).unwrap();
    assert!(args.tree_only);
    let halt = halt_of(&args);
    assert!(!halt.armed());
    let ctx = Ctx::new(
        PathBuf::from("/repo"),
        Scope::workspace(),
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
    .with_checks(&args.checks);
    let specs = plan::plan(&ctx);
    let mut seen = Vec::new();
    let reports = sched::run_stages(
        &specs,
        u64::MAX,
        &halt,
        |spec, _| {
            let mut report = Report::new(spec.title.clone());
            if spec.id == plan::StageId::Formatting {
                assert!(!halt.red("fmt"));
                report.fail("planted formatting red");
            } else {
                report.pass("ran after or beside the red");
            }
            report
        },
        |i, _| seen.push(i),
    );
    assert_eq!(seen, (0..specs.len()).collect::<Vec<_>>());
    assert_eq!(reports.len(), specs.len());
    assert!(halt.words().is_none());
    for words in [
        vec!["release", "--tree", "--fail-fast"],
        vec!["release", "--bundle", "--app", "/a.app"],
        vec!["release", "--tree", "--bundle", "--app", "/a.app"],
        vec!["--fail-fast"],
    ] {
        assert!(halt_of(&cli::parse(words).unwrap()).armed());
    }
}

#[test]
fn a_completed_stage_is_reported_before_its_dependent_runs() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REPORTED: AtomicBool = AtomicBool::new(false);
    static STARTED: AtomicBool = AtomicBool::new(false);
    static DEPENDENT_RAN: AtomicBool = AtomicBool::new(false);
    struct LiveOutput;
    impl std::io::Write for LiveOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(json) = bytes.strip_prefix(b"verify-event: ") {
                let event: aterm_json::Value = aterm_json::from_slice(json).unwrap();
                match event["event"].as_str() {
                    Some("stage-started") => STARTED.store(true, Ordering::SeqCst),
                    Some("stage-finished") => REPORTED.store(true, Ordering::SeqCst),
                    _ => {}
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
        assert!(
            STARTED.load(Ordering::SeqCst),
            "stage start was not reported"
        );
        if spec.id != plan::StageId::TestCompile {
            assert!(
                REPORTED.load(Ordering::SeqCst),
                "compile completion was buffered"
            );
            DEPENDENT_RAN.store(true, Ordering::SeqCst);
        }
        let mut report = Report::new(spec.title.clone());
        report.pass("ran");
        report
    }
    REPORTED.store(false, Ordering::SeqCst);
    STARTED.store(false, Ordering::SeqCst);
    DEPENDENT_RAN.store(false, Ordering::SeqCst);
    let ctx = Ctx::new(
        PathBuf::from("/repo"),
        Scope::crate_only("aterm-verify"),
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
    .with_checks(&[Check::Test]);
    let result = run_with(
        &ctx,
        &mut LiveOutput,
        &mut std::io::sink(),
        &Show::default(),
        Seams {
            stage,
            prepare_cells: None,
            prepare_build_dir: |_| Ok(()),
            ..LIVE
        },
    );
    assert_eq!(result.unwrap(), exit::PASS);
    assert!(REPORTED.load(Ordering::SeqCst));
    assert!(DEPENDENT_RAN.load(Ordering::SeqCst));
}

#[test]
fn a_private_companion_refuses_public_aliases_and_keeps_internal_producer_records() {
    fn stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
        let mut report = Report::new(spec.title.clone());
        report.pass("ran");
        report
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("chosen.log");
    let context = || {
        Ctx::new(
            dir.path().to_path_buf(),
            Scope::crate_only("aterm-verify"),
            EnvSnapshot::default(),
            dir.path().join("scratch"),
        )
        .with_checks(&[Check::Fmt])
    };
    let mut previous = context();
    let log = open_run_log(&mut previous, Some(&path), dir.path()).unwrap();
    drop(log);
    drop(previous);
    let alias = dir.path().join("external-alias.events");
    std::fs::hard_link(run_log::events_path(&path), &alias).unwrap();
    assert!(event_log::EventLog::open(&alias).is_err());
    assert!(event_log::EventLog::open(&run_log::events_path(&path)).is_err());
    let human_alias = dir.path().join("external-human-alias.log");
    std::fs::hard_link(&path, &human_alias).unwrap();
    assert!(event_log::EventLog::open(&path).is_err());
    assert!(event_log::EventLog::open(&human_alias).is_err());
    let mut ctx = context();
    let mut log = open_run_log(&mut ctx, Some(&path), dir.path()).unwrap();
    assert_eq!(
        run_with(
            &ctx,
            &mut Vec::new(),
            &mut log,
            &Show::default(),
            Seams {
                stage,
                prepare_cells: None,
                prepare_build_dir: |_| Ok(()),
                ..LIVE
            }
        )
        .unwrap(),
        exit::PASS
    );
    drop(log);
    drop(ctx);
    assert!(
        std::fs::read(&path)
            .unwrap()
            .starts_with(run_log::PRIVATE_HUMAN_HEADER.as_bytes())
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(&human_alias).unwrap()
    );
    assert!(event_log::EventLog::open(&human_alias).is_err());
    let text = std::fs::read_to_string(&alias).unwrap();
    assert_eq!(
        std::fs::read(&alias).unwrap(),
        std::fs::read(run_log::events_path(&path)).unwrap()
    );
    let observed = status::observe(&text);
    assert!(text.starts_with(run_log::PRIVATE_EVENTS_HEADER));
    assert_eq!(observed.malformed_complete_records, 0);
    assert_eq!(observed.invocations.len(), 1);
    assert_eq!(observed.invocations[0].observed.terminal_records, 1);
    assert_eq!(
        observed.invocations[0].observed.terminal_consistency,
        "matching-observed-rows"
    );
    assert_eq!(observed.invocations[0].observed.checks[0].occurrences, 1);
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("invocation-started"))
            .count(),
        1
    );
}

#[test]
fn structured_records_survive_the_external_temp_directory_and_keep_actual_producer_bytes() {
    fn stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
        let mut report = Report::new(spec.title.clone());
        report.pass("ran the bounded fixture stage");
        report
    }
    let retained = aterm_tempfile::tempdir().unwrap();
    let transient = aterm_tempfile::tempdir().unwrap();
    let external = transient.path().join("verify.events");
    let mut ctx = Ctx::new(
        retained.path().to_path_buf(),
        Scope::crate_only("aterm-verify"),
        EnvSnapshot::default(),
        retained.path().join("scratch"),
    )
    .with_checks(&[Check::Fmt])
    .with_events(Some(event_log::EventLog::open(&external).unwrap()));
    let path = retained.path().join("kept.log");
    let mut log = open_run_log(&mut ctx, Some(&path), retained.path()).unwrap();
    let id = ctx.events.as_ref().unwrap().id().unwrap().to_owned();
    let mut out = Vec::new();
    let code = run_with(
        &ctx,
        &mut out,
        &mut log,
        &Show::default(),
        Seams {
            stage,
            prepare_cells: None,
            prepare_build_dir: |_| Ok(()),
            ..LIVE
        },
    )
    .unwrap();
    assert_eq!(code, exit::PASS);
    #[cfg(unix)]
    let_disk_go(
        disk::Disk::Unclaimed,
        true,
        Some(&mut log),
        ctx.events.as_ref(),
    );
    drop(log);
    drop(ctx);
    let external_bytes = std::fs::read(&external).unwrap();
    drop(transient);
    assert!(!external.exists());
    let at = run_log::events_path(&path);
    let kept = std::fs::read(&at).unwrap();
    assert_eq!(
        kept.strip_prefix(run_log::PRIVATE_EVENTS_HEADER.as_bytes())
            .unwrap(),
        external_bytes
    );
    let text = String::from_utf8(kept).unwrap();
    let observed = status::observe(&text);
    assert_eq!(observed.malformed_complete_records, 0);
    assert_eq!(observed.invocations.len(), 1);
    let run = &observed.invocations[0];
    assert_eq!(run.id, id);
    assert!(run.source_observation.is_some());
    assert_eq!(run.observed.terminal_consistency, "matching-observed-rows");
    let rows = text
        .lines()
        .filter_map(event_log::CheckRow::of_event)
        .collect();
    let finished = text
        .lines()
        .rev()
        .find_map(event_log::Finished::of_event)
        .unwrap();
    assert!(
        event_log::Judged {
            code: finished.code,
            checks: rows,
            secs: 0,
            finished: Some(finished)
        }
        .whole()
    );
    let output = String::from_utf8(out).unwrap();
    for line in output
        .lines()
        .filter(|line| line.starts_with("verify-event: "))
    {
        assert!(
            text.lines().any(|kept| kept == line),
            "same prepared bytes in both sinks"
        );
    }
    let kinds = text
        .lines()
        .map(|line| {
            aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
                .unwrap()["event"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert!(kinds.iter().any(|kind| kind == "stage-started"));
    assert!(kinds.iter().any(|kind| kind == "stage-finished"));
    #[cfg(unix)]
    assert!(
        kinds
            .iter()
            .position(|kind| kind == "run-finished")
            .unwrap()
            < kinds.iter().position(|kind| kind == "disk").unwrap()
    );
    assert_eq!(
        status::run(
            &[
                "--events".into(),
                at.to_string_lossy().into_owned(),
                "--json".into()
            ],
            retained.path()
        ),
        exit::PASS
    );
}

/// Each check's `check-finished` event is written once, whole, right before its own line and
/// before the verdict, with the word that line opens with, and the same line to the run's
/// events file, which a release's cutter keeps one record per check from (A19); the file
/// ends with the run's `run-finished` line, naming its exit and each check's word, by which
/// `verify land` knows the record whole (B10). Negative controls: emitting the event after
/// the rendered line fails the order assertion; one never recorded leaves the events file
/// without it.
#[test]
fn each_check_says_check_finished_once_before_its_line_and_the_verdict() {
    fn stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
        let mut report = Report::new(spec.title.clone());
        if spec.id == plan::StageId::Census {
            report.fail("a guard found a banned call");
        } else {
            report.pass("ran");
        }
        report
    }
    let ctx = Ctx::new(
        PathBuf::from("/repo"),
        Scope::workspace(),
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
    .with_checks(&[Check::Fmt, Check::Census]);
    let dir = aterm_tempfile::tempdir().unwrap();
    let at = dir.path().join("06-verify.events");
    let ctx = ctx.with_events(Some(event_log::EventLog::open(&at).unwrap()));
    let mut out: Vec<u8> = Vec::new();
    let code = run_with(
        &ctx,
        &mut out,
        &mut std::io::sink(),
        &Show::default(),
        Seams {
            stage,
            prepare_cells: None,
            prepare_build_dir: |_| Ok(()),
            ..LIVE
        },
    )
    .unwrap();
    assert_eq!(code, exit::FAILED);
    let text = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let verdict = lines
        .iter()
        .position(|l| l.starts_with("verify: FAIL"))
        .expect("a verdict line");
    for (name, word) in [("fmt", "ok"), ("census", "FAIL")] {
        let events: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| {
                let json = l.strip_prefix("verify-event: ")?;
                let event: aterm_json::Value = aterm_json::from_slice(json.as_bytes()).ok()?;
                (event["event"].as_str() == Some("check-finished")
                    && event["check"].as_str() == Some(name))
                .then(|| {
                    assert_eq!(event["outcome"].as_str(), Some(word), "{l}");
                    i
                })
            })
            .collect();
        assert_eq!(events.len(), 1, "{name}: {text}");
        let own = lines
            .iter()
            .position(|l| l.starts_with(word) && l.split_whitespace().nth(1) == Some(name))
            .expect("the check's own line");
        assert_eq!(
            events[0] + 1,
            own,
            "{name}'s event comes right before its line: {text}"
        );
        assert!(own < verdict, "{text}");
    }
    let said: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| {
            l.strip_prefix("verify-event: ")
                .and_then(|json| aterm_json::from_str::<aterm_json::Value>(json).ok())
                .is_some_and(|event| event["event"].as_str() == Some("check-finished"))
        })
        .collect();
    let kept = std::fs::read_to_string(&at).unwrap();
    let drivers = kept
        .lines()
        .filter_map(|line| line.strip_prefix("verify-event: "))
        .filter_map(|json| aterm_json::from_str::<aterm_json::Value>(json).ok())
        .filter(|event| event["event"].as_str() == Some("measure-driver"))
        .collect::<Vec<_>>();
    assert_eq!(drivers.len(), 1);
    assert_eq!(drivers[0]["sha256"].as_str(), report::running_driver());
    let (last, checks) = kept
        .lines()
        .collect::<Vec<_>>()
        .split_last()
        .map(|(last, checks)| (last.to_string(), checks.to_vec()))
        .expect("the events file");
    let mut kept_checks = checks
        .iter()
        .copied()
        .filter(|line| event_log::CheckRow::of_event(line).is_some())
        .collect::<Vec<_>>();
    let mut said = said;
    kept_checks.sort_unstable();
    said.sort_unstable();
    assert_eq!(
        kept_checks, said,
        "exact records in both sinks, independent concurrent ordering: {kept}"
    );
    assert!(
        checks.iter().any(|line| line.contains("stage-started")),
        "actual stage records are retained too"
    );
    let finished: aterm_json::Value =
        aterm_json::from_str(last.strip_prefix("verify-event: ").expect("an event")).unwrap();
    assert_eq!(finished["event"].as_str(), Some("run-finished"), "{last}");
    assert_eq!(
        finished["code"].as_i64(),
        Some(i64::from(exit::FAILED)),
        "{last}"
    );
    let words: Vec<(&str, &str)> = finished["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter_map(|c| Some((c["check"].as_str()?, c["outcome"].as_str()?)))
        .collect();
    assert_eq!(words, [("fmt", "ok"), ("census", "FAIL")], "{last}");
}

/// A run whose checkout's HEAD moves while its stages run ends COULD NOT RUN, its checks'
/// own PASS beside it. Negative control: the same run with HEAD still ends PASS.
#[test]
fn a_run_whose_checkout_moved_under_its_stages_could_not_run() {
    let root = mktemp_dir("atv-moved-checkout").expect("mktemp");
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    for args in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ],
    ] {
        let out = changed::git(&root, &path_env)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    let run = |stage: fn(&Ctx, &plan::StageSpec) -> Report| {
        let mut ctx = Ctx::new(
            root.clone(),
            Scope::crate_only("aterm-verify"),
            EnvSnapshot::default(),
            root.join("scratch"),
        )
        .with_checks(&[Check::Fmt]);
        ctx.path_env.clone_from(&path_env);
        let mut out = Vec::new();
        let code = run_with(
            &ctx,
            &mut out,
            &mut std::io::sink(),
            &Show::default(),
            Seams {
                stage,
                prepare_cells: None,
                admit_quiet: LIVE.admit_quiet,
                admit_memory: LIVE.admit_memory,
                prepare_build_dir: |_| Ok(()),
            },
        )
        .expect("the result written");
        (code, String::from_utf8_lossy(&out).into_owned())
    };

    let (code, out) = run(passing_stage);
    assert_eq!(code, exit::PASS, "HEAD never moved: {out}");

    let (code, out) = run(committing_stage);
    assert_eq!(code, exit::COULD_NOT_RUN, "{out}");
    assert!(out.contains("the checkout moved from"), "{out}");
    std::fs::remove_dir_all(&root).ok();
}

/// A stage the run's memory budget held back says so on its time line, naming for how long,
/// behind which stages, and what it claims of the budget; one never held says nothing.
/// NEGATIVE CONTROL: the same stage, not held, has no note.
#[test]
fn a_stage_held_for_memory_says_how_long_and_behind_what() {
    const GIB: u64 = 1 << 30;
    let stages: Vec<plan::StageSpec> = plan::declared(&Scope::crate_only("aterm-gui"))
        .into_iter()
        .filter(|s| matches!(s.id, plan::StageId::TestCompile | plan::StageId::Tippy))
        .map(|s| plan::StageSpec { mem: 12 * GIB, ..s })
        .collect();
    let test_compile = stages
        .iter()
        .position(|s| s.id == plan::StageId::TestCompile)
        .expect("test compile");
    let tippy = stages
        .iter()
        .position(|s| s.id == plan::StageId::Tippy)
        .expect("tippy");
    let held = sched::Held {
        waited: Duration::from_millis(41_250),
        behind: vec![test_compile],
    };
    let note = held_note(&stages, tippy, &held, 43 * GIB / 2).expect("held");
    assert_eq!(
        note,
        "memory: held 41.2s for room behind test compile (-p aterm-gui): it claims 12 GiB of \
         the run's 21.5 GiB"
    );
    assert_eq!(
        held_note(&stages, tippy, &sched::Held::default(), 43 * GIB / 2),
        None
    );
}

#[test]
fn built_from_rejects_short_or_non_hex_commits() {
    let art = |commit: &str| MeasureArtifact {
        app: PathBuf::from("/tmp/aterm.app"),
        version: Some("0.137.0".into()),
        commit: Some(commit.into()),
    };
    let head = "22cb722a2ec9abcdef0123456789abcdef012345";
    assert!(!art("22cb72").built_from(head), "6 hex chars is too short");
    assert!(art("22cb722").built_from(head), "7 hex chars is valid");
    assert!(!art("22cb722z").built_from("22cb722zabcdef0123456789abcdef0123456789"));
}

/// A complete store build `build` under `prefix` whose `trustc` answers `--print sysroot` and
/// `-vV` (commit `commit`) and whose `targo` runs: its tree's fold.
#[cfg(unix)]
fn locked_build(prefix: &Path, build: u64, commit: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = atpkg::trust_lock::build_dir(prefix, build);
    std::fs::create_dir_all(dir.join("lib/rustlib")).expect("rustlib");
    std::fs::write(atpkg::trust_lock::ready_marker(prefix, build), "ok\n").expect("ready");
    std::fs::create_dir_all(dir.join("bin")).expect("bin");
    for (tool, body) in [
        ("targo", "exit 0".to_string()),
        (
            "trustc",
            format!(
                "case \"$1\" in -vV) printf 'commit-hash: {commit}\\n' ;; *) echo '{}' ;; esac",
                dir.display()
            ),
        ),
    ] {
        let path = dir.join("bin").join(tool);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("stand-in");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    // A store build's `rustc` is a hard link of its `trustc`.
    std::fs::hard_link(dir.join("bin/trustc"), dir.join("bin/rustc")).expect("rustc");
    atpkg::tree::tree_root(&dir).expect("fold")
}

/// A LOCKED CHECKOUT'S RUN COMPILES WITH THE LOCK'S BUILD (A14): the store's `current` names
/// another build, and the run takes the lock's, folding and recording a build installed before
/// records; it says so in its log, and says a `targo` shim laid for `current` makes plain
/// builds and the run alternate units; a `RUSTC`, `CARGO_BUILD_RUSTC` or `build.rustc` whose
/// compiler is not the lock's refuses the run's preflight. NEGATIVE CONTROL: without the lock
/// the run takes `current`, says no lock, notes nothing and takes any `RUSTC`.
#[cfg(unix)]
#[test]
fn a_locked_checkout_runs_the_locked_build_and_says_a_shim_that_does_not() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let home = scratch.path().canonicalize().expect("canonical");
    let env = EnvSnapshot {
        home: home.clone(),
        xdg_config_home: Some(home.join("xdg")),
        ..EnvSnapshot::default()
    };
    let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    let commit = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";
    let root_of_locked = locked_build(&prefix, 16387, commit);
    locked_build(&prefix, 16500, &"b".repeat(40));
    std::os::unix::fs::symlink(
        atpkg::trust_lock::build_dir(&prefix, 16500),
        prefix.join("store/trust/current"),
    )
    .expect("current");
    std::fs::create_dir_all(prefix.join("bin")).expect("shims");
    std::fs::write(
        prefix.join("bin/targo"),
        format!(
            "#!/bin/sh\n# atpkg shim\nexec '{}' \"$@\"\n",
            atpkg::trust_lock::build_dir(&prefix, 16500)
                .join("bin/targo")
                .display()
        ),
    )
    .expect("a shim laid for current");
    let checkout = home.join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::write(
        checkout.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"trust\"\n",
    )
    .expect("pin");

    let unlocked = run_toolchain(&env, &checkout);
    assert_eq!(
        unlocked.stage2_dir,
        atpkg::trust_lock::build_dir(&prefix, 16500).join("bin"),
        "no lock: current"
    );
    assert_eq!(lock_line(&unlocked), None);
    assert!(lock_notes(&env, &unlocked).is_empty());

    let lock = atpkg::trust_lock::TrustLock {
        build: 16387,
        tree_root: root_of_locked,
        compiler_commit: commit.to_string(),
        receipt: "SEED-MINT sha256:00".to_string(),
        stock_toolchain: "1.99.1".to_string(),
    };
    std::fs::write(
        checkout.join(atpkg::trust_lock::LOCK_FILE),
        atpkg::trust_lock::render_body(&lock),
    )
    .expect("lock");
    let locked = run_toolchain(&env, &checkout);
    assert!(locked.have_targo(), "{}", locked.missing_targo_label());
    assert_eq!(
        locked.stage2_dir,
        atpkg::trust_lock::build_dir(&prefix, 16387).join("bin")
    );
    assert!(
        lock_line(&locked).is_some_and(|l| l.contains("names Trust build 16387")),
        "{:?}",
        lock_line(&locked)
    );
    let notes = lock_notes(&env, &locked);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].starts_with("plain `targo` resolves 16500"),
        "{notes:?}"
    );

    // What names another compiler reaches every compile child: only the lock's compiler
    // passes the run's preflight, wherever it is named, resolved as the child resolves it.
    let trustc_of = |build| atpkg::trust_lock::build_dir(&prefix, build).join("bin/trustc");
    let with = |vars: &[(&'static str, &Path)]| EnvSnapshot {
        rustc_vars: vars
            .iter()
            .map(|(k, v)| (*k, v.display().to_string()))
            .collect(),
        ..env.clone()
    };
    let other = trustc_of(16500);
    let same = trustc_of(16387);
    assert!(preflight(&env, &checkout).is_ok(), "nothing names another");
    for var in ["RUSTC", "CARGO_BUILD_RUSTC"] {
        let refused = preflight(&with(&[(var, &other)]), &checkout).expect_err(var);
        assert!(
            refused.contains(&format!("commit {}", "b".repeat(40)))
                && refused.contains(&format!("Fix: unset {var}")),
            "{refused}"
        );
        assert!(
            preflight(&with(&[(var, &same)]), &checkout).is_ok(),
            "{var}"
        );
    }
    assert!(
        preflight(&with(&[("RUSTC", Path::new("rustc"))]), &checkout).is_ok(),
        "a bare name is the child's PATH's, whose first entry is the locked build"
    );
    let config = checkout.join(".cargo/config.toml");
    std::fs::create_dir_all(checkout.join(".cargo")).expect(".cargo");
    std::fs::write(
        &config,
        format!("[build]\nrustc = \"{}\"\n", other.display()),
    )
    .expect("config");
    let refused = preflight(&env, &checkout).expect_err("build.rustc in the checkout's config");
    assert!(
        refused.contains(&format!(
            "Fix: remove build.rustc from {}",
            config.display()
        )),
        "{refused}"
    );
    assert!(
        preflight(&with(&[("RUSTC", &same)]), &checkout).is_ok(),
        "RUSTC outranks build.rustc"
    );
    std::fs::remove_file(&config).expect("rm config");
    std::fs::remove_file(checkout.join(atpkg::trust_lock::LOCK_FILE)).expect("rm lock");
    assert!(
        preflight(&with(&[("RUSTC", &other)]), &checkout).is_ok(),
        "no lock: RUSTC is the caller's"
    );
}

#[test]
fn pinned_engine_snapshot_values_are_reader_inputs_not_test_child_environment() {
    let snapshot = EnvSnapshot {
        publication_engine_git_dir: Some(OsString::new()),
        publication_engine_commit: Some(OsString::from("invalid")),
        ..EnvSnapshot::default()
    };
    assert_eq!(
        snapshot.engine_var(engine::GIT_DIR_ENV),
        Some(OsString::new())
    );
    assert_eq!(
        snapshot.engine_var(engine::COMMIT_ENV),
        Some(OsString::from("invalid"))
    );
    let ctx = Ctx::new(
        PathBuf::from("/repo"),
        Scope::workspace(),
        snapshot,
        PathBuf::from("/tmp"),
    );
    for name in [engine::GIT_DIR_ENV, engine::COMMIT_ENV] {
        assert!(ctx.exec_env().remove_env.contains(&name));
        assert!(ctx.exec_env().add_env.iter().all(|(key, _)| key != name));
    }
}
