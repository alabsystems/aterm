// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::fs;

fn mktemp_dir(tag: &str) -> std::io::Result<PathBuf> {
    Ok(aterm_tempfile::Builder::new()
        .prefix(&format!("{tag}."))
        .tempdir()?
        .keep())
}

fn runnable(path: &Path) -> PathBuf {
    let mut named = path.as_os_str().to_os_string();
    named.push(std::env::consts::EXE_SUFFIX);
    PathBuf::from(named)
}

fn exec_stub(path: &Path) {
    crate::test_script::script(&runnable(path), "#!/bin/sh\nexit 0\n");
}

fn driver_stub(path: &Path) {
    let path = &runnable(path);
    let sysroot = path.parent().expect("bin dir").parent().expect("sysroot");
    fs::create_dir_all(sysroot.join("lib/rustlib")).expect("mkdir rustlib");
    #[cfg(windows)]
    fs::copy(std::env::current_exe().expect("this test binary"), path).expect("copy");
    #[cfg(not(windows))]
    crate::test_script::script(path, &format!("#!/bin/sh\necho '{}'\n", sysroot.display()));
}

/// CRT initializer so a copied test binary on Windows answers `--print sysroot` before libtest.
#[cfg(windows)]
#[used]
#[unsafe(link_section = ".CRT$XCU")]
static ANSWER_AS_A_DRIVER: extern "C" fn() = answer_as_a_driver;

#[cfg(windows)]
extern "C" fn answer_as_a_driver() {
    use std::io::Write as _;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args != [OsStr::new("--print"), OsStr::new("sysroot")] {
        return;
    }
    let Ok(me) = std::env::current_exe() else {
        std::process::exit(1);
    };
    let Some(sysroot) = me.parent().and_then(Path::parent) else {
        std::process::exit(1);
    };
    let mut out = std::io::stdout();
    let wrote = writeln!(out, "{}", sysroot.display()).and_then(|()| out.flush());
    std::process::exit(i32::from(wrote.is_err()));
}

#[test]
fn the_reported_location_when_nothing_resolves_is_the_store() {
    let home = Path::new("/nonexistent-home");
    let prefix = default_atpkg_prefix(home);
    let t = Toolchain::discover_with_store(
        None,
        &rustup_home(None, home),
        Some(&prefix),
        OsStr::new(""),
        None,
        None,
    );
    assert_eq!(t.targo, store_stage2_bin(&prefix).join(tool_file("targo")));
    assert_eq!(
        t.trustdoc,
        store_stage2_bin(&prefix).join(tool_file("trustdoc"))
    );
    assert!(!t.have_targo());
    assert!(t.tippy.is_none());
    let t = Toolchain::discover_with_store(
        None,
        &rustup_home(None, home),
        None,
        OsStr::new(""),
        None,
        None,
    );
    assert_eq!(
        t.targo,
        Path::new("/nonexistent-home/.rustup/toolchains/trust/bin").join(tool_file("targo"))
    );
}

/// Pending `atpkg` stubs on `PATH` are skipped in favor of real toolchains.
#[cfg(unix)]
#[test]
fn a_pending_stub_on_path_is_passed_over() {
    let home = mktemp_dir("atv-pending").expect("mktemp");
    let prefix = default_atpkg_prefix(&home);
    let stubs = home.join("pkg-bin");
    fs::create_dir_all(&stubs).expect("mkdir");
    for name in ["targo", "trustc"] {
        crate::test_script::script(
            &stubs.join(name),
            "#!/bin/sh\n# atpkg pending-program stub v1\nexit 127\n",
        );
    }
    let real = pinned_bin(&home.join("real/bin"));
    let path = std::env::join_paths([&stubs, &real]).expect("join");
    for pinned in [None, Some("trust")] {
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, &home),
            Some(&prefix),
            &path,
            pinned,
            None,
        );
        assert_eq!(t.targo, real.join("targo"), "pinned={pinned:?}");
        assert!(t.have_targo(), "pinned={pinned:?}");
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, &home),
            Some(&prefix),
            stubs.as_os_str(),
            pinned,
            None,
        );
        assert_eq!(
            t.targo,
            store_stage2_bin(&prefix).join("targo"),
            "pinned={pinned:?}"
        );
        assert!(!t.have_targo(), "pinned={pinned:?}");
        let explicit = Toolchain::discover_with_store(
            Some(&stubs),
            &rustup_home(None, &home),
            Some(&prefix),
            OsStr::new(""),
            pinned,
            None,
        );
        assert!(!explicit.have_targo(), "explicit pending stub is not targo");
        assert!(
            explicit.refused.is_none(),
            "pending stub is absent, not an impostor"
        );
    }
    let _ = fs::remove_dir_all(&home);
}

/// Create a valid pinned toolchain `bin` directory under `root` and return its canonical path.
#[cfg(unix)]
fn pinned_bin(root: &Path) -> PathBuf {
    fs::create_dir_all(root).expect("mkdir");
    exec_stub(&root.join("targo"));
    driver_stub(&root.join("trustc"));
    fs::canonicalize(root).expect("canonicalize")
}

/// Resolution order: explicit override > rustup > store > PATH (ignoring unprobed build trees).
#[cfg(unix)]
#[test]
fn one_resolution_order_and_no_build_tree() {
    let home = mktemp_dir("atv-order").expect("mktemp");
    let prefix = default_atpkg_prefix(&home);
    let store = pinned_bin(&prefix.join("store/trust/6808/bin"));
    std::os::unix::fs::symlink(
        prefix.join("store/trust/6808"),
        prefix.join("store/trust/current"),
    )
    .expect("ln current");
    let linked = pinned_bin(&home.join("sealed/bin"));
    fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
    std::os::unix::fs::symlink(home.join("sealed"), home.join(".rustup/toolchains/trust"))
        .expect("ln rustup");
    let on_path = pinned_bin(&home.join("elsewhere/bin"));
    let tree = pinned_bin(&home.join("trust/build/host/stage2/bin"));
    let promoted = pinned_bin(&home.join("toolchains/trust-current/bin"));
    let path = std::env::join_paths([&tree, &promoted, &on_path]).expect("join");
    let found = |explicit: Option<&Path>, path: &OsStr| {
        Toolchain::discover_with_store(
            explicit,
            &rustup_home(None, &home),
            Some(&prefix),
            path,
            Some("trust"),
            None,
        )
    };

    let t = found(Some(&on_path), &path);
    assert_eq!(
        (t.stage2_dir.as_path(), t.store_bin.is_none()),
        (on_path.as_path(), true)
    );
    let t = found(None, &path);
    assert!(t.have_targo());
    assert_eq!(t.stage2_dir, linked);
    assert_eq!(t.store_bin, Some(store_stage2_bin(&prefix)));
    fs::remove_file(home.join(".rustup/toolchains/trust")).expect("rm rustup");
    assert_eq!(found(None, &path).stage2_dir, store);
    fs::remove_file(prefix.join("store/trust/current")).expect("rm current");
    assert_eq!(found(None, &path).stage2_dir, tree);
    let t = found(None, OsStr::new(""));
    assert!(
        !t.have_targo(),
        "a build tree under home is not a candidate: {}",
        t.stage2_dir.display()
    );
    assert_eq!(t.stage2_dir, store_stage2_bin(&prefix));
    fs::remove_dir_all(&home).ok();
}

#[test]
fn rustup_home_is_the_variable_when_set_and_home_dot_rustup_otherwise() {
    let home = Path::new("/h");
    assert_eq!(rustup_home(None, home), Path::new("/h/.rustup"));
    assert_eq!(
        rustup_home(Some(OsStr::new("")), home),
        Path::new("/h/.rustup")
    );
    assert_eq!(
        rustup_home(Some(OsStr::new("/opt/rustup")), home),
        Path::new("/opt/rustup")
    );
}

/// Discovery resolves the pinned toolchain under a relocated `$RUSTUP_HOME`.
#[cfg(unix)]
#[test]
fn a_relocated_rustup_home_is_where_the_pinned_toolchain_is_found() {
    let home = mktemp_dir("atv-rustup-home").expect("mktemp");
    let relocated = home.join("elsewhere/rustup");
    let linked = pinned_bin(&home.join("sealed/bin"));
    fs::create_dir_all(relocated.join("toolchains")).expect("mkdir");
    std::os::unix::fs::symlink(home.join("sealed"), relocated.join("toolchains/trust"))
        .expect("ln rustup");
    let found = |rustup: &Path| {
        Toolchain::discover_with_store(None, rustup, None, OsStr::new(""), Some("trust"), None)
    };
    let t = found(&rustup_home(Some(relocated.as_os_str()), &home));
    assert!(t.have_targo(), "{}", t.stage2_dir.display());
    assert_eq!(t.stage2_dir, linked);
    let t = found(&rustup_home(None, &home));
    assert!(
        !t.have_targo(),
        "the default home holds no entry: {}",
        t.stage2_dir.display()
    );
    fs::remove_dir_all(&home).ok();
}

#[test]
fn a_store_targo_that_is_not_the_pin_is_refused_and_the_search_goes_on() {
    let home = mktemp_dir("atv-storepin").expect("mktemp");
    let prefix = default_atpkg_prefix(&home);
    let store = store_stage2_bin(&prefix);
    fs::create_dir_all(&store).expect("mkdir");
    exec_stub(&store.join("targo"));
    let real = home.join("elsewhere/bin");
    fs::create_dir_all(&real).expect("mkdir");
    exec_stub(&real.join("targo"));
    driver_stub(&real.join("trustc"));

    let t = Toolchain::discover_with_store(
        None,
        &rustup_home(None, &home),
        Some(&prefix),
        OsStr::new(real.to_string_lossy().as_ref()),
        Some("trust"),
        None,
    );
    assert!(t.have_targo(), "PATH still wins past a refused store");
    assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));

    let t = Toolchain::discover_with_store(
        None,
        &rustup_home(None, &home),
        Some(&prefix),
        OsStr::new(""),
        Some("trust"),
        None,
    );
    assert!(!t.have_targo(), "fail-closed: the impostor is not adopted");
    assert_eq!(
        t.refused,
        Some(fs::canonicalize(&store).expect("canonicalize"))
    );
    fs::remove_dir_all(&home).ok();
}

/// Create a dated toolchain `bin` whose `trustc` answers both `--print sysroot` and `-vV`.
#[cfg(unix)]
fn dated_bin(fixture: &Path, sysroot: &Path, date: &str) -> PathBuf {
    let script = fixture.join("dated-trustc");
    if !script.is_file() {
        crate::test_script::script(
            &script,
            "#!/bin/sh\nroot=$(cd \"$(dirname \"$0\")/..\" && pwd -P)\n\
             case \"$1\" in\n  --print) echo \"$root\" ;;\n  \
             -vV) d=$(cat \"$root/commit-date\"); echo \"rustc 1.99.0-dev (0000000 $d)\"; \
             echo \"commit-date: $d\" ;;\nesac\n",
        );
        let _ = std::process::Command::new(&script).output();
    }
    let bin = sysroot.join("bin");
    fs::create_dir_all(sysroot.join("lib/rustlib")).expect("mkdir rustlib");
    fs::create_dir_all(&bin).expect("mkdir bin");
    let _ = fs::remove_file(bin.join("trustc"));
    fs::hard_link(&script, bin.join("trustc")).expect("link trustc");
    if !bin.join("targo").is_file() {
        exec_stub(&bin.join("targo"));
    }
    fs::write(sysroot.join("commit-date"), date).expect("date");
    fs::canonicalize(&bin).expect("canonicalize")
}

#[cfg(unix)]
fn record_dated_probes(fixture: &Path, require_guard: bool) {
    let script = fixture.join("dated-trustc");
    let original = fs::read_to_string(&script).expect("read driver");
    let guard = if require_guard {
        "test -f \"$root/guarded\" || exit 1\n"
    } else {
        ""
    };
    // In place, through the shared inode, so every sysroot's `trustc` link records.
    crate::test_script::script(
        &script,
        &original.replace(
            "case \"$1\" in",
            &format!("{guard}echo probed >> \"$root/probes\"\ncase \"$1\" in"),
        ),
    );
}

#[cfg(unix)]
#[test]
fn a_probe_guard_refusal_runs_no_compiler_and_cannot_fall_back() {
    let fixture = aterm_tempfile::tempdir().unwrap();
    let root = fixture.path();
    let first = dated_bin(root, &root.join("first"), "2026-09-17");
    let second = dated_bin(root, &root.join("second"), "2026-09-18");
    record_dated_probes(root, false);
    let path = std::env::join_paths([&first, &second]).unwrap();
    let absent_rustup = root.join("no-rustup");
    // Negative control: the same executable really runs without a refusing guard.
    let unguarded =
        Toolchain::discover_with_store(None, &absent_rustup, None, &path, Some("trust"), None);
    assert_eq!(unguarded.stage2_dir, first);
    assert!(root.join("first/probes").is_file());
    for name in ["first/probes", "second/probes"] {
        let _ = fs::remove_file(root.join(name));
    }
    let mut called = Vec::new();
    let refused = Toolchain::discover_with_store_guarded(
        None,
        &absent_rustup,
        None,
        &path,
        Some("trust"),
        None,
        &mut |dir| {
            called.push(dir.to_path_buf());
            Err("lease denied".into())
        },
    );
    assert_eq!(refused.unwrap_err(), "lease denied");
    assert_eq!(called, [first]);
    assert!(!root.join("first/probes").exists());
    assert!(!root.join("second/probes").exists());
}

#[cfg(unix)]
#[test]
fn a_probe_executes_the_guarded_executable_when_its_alias_changes() {
    let fixture = aterm_tempfile::tempdir().unwrap();
    let root = fixture.path();
    let first = dated_bin(root, &root.join("first"), "2026-09-17");
    let second = dated_bin(root, &root.join("second"), "2026-09-18");
    let alias = root.join("compiler-alias");
    std::os::unix::fs::symlink(first.join("trustc"), &alias).unwrap();
    let mut before = |dir: &Path| {
        assert_eq!(dir, first);
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(second.join("trustc"), &alias).unwrap();
        Ok(())
    };
    let mut probe = Probe {
        before: &mut before,
        error: None,
    };
    let sysroot = probe
        .stdout(&alias, &["--print", "sysroot"], DATE_PROBE_BOUND)
        .unwrap();
    assert_eq!(sysroot.trim(), first.parent().unwrap().to_str().unwrap());
    assert_eq!(fs::canonicalize(&alias).unwrap(), second.join("trustc"));
}

#[cfg(unix)]
#[test]
fn a_probe_guard_covers_both_compilers_before_a_staleness_comparison() {
    let fixture = aterm_tempfile::tempdir().unwrap();
    let root = fixture.path();
    let prefix = root.join("pkg");
    let store = dated_bin(root, &prefix.join("store/trust/9192"), "2026-09-17");
    std::os::unix::fs::symlink(
        prefix.join("store/trust/9192"),
        prefix.join("store/trust/current"),
    )
    .unwrap();
    let older = dated_bin(root, &root.join("older"), "2026-08-20");
    let rustup = root.join("rustup");
    fs::create_dir_all(rustup.join("toolchains")).unwrap();
    std::os::unix::fs::symlink(root.join("older"), rustup.join("toolchains/trust")).unwrap();
    record_dated_probes(root, true);
    let mut guarded = std::collections::BTreeSet::new();
    let found = Toolchain::discover_with_store_guarded(
        None,
        &rustup,
        Some(&prefix),
        OsStr::new(""),
        Some("trust"),
        None,
        &mut |dir| {
            guarded.insert(dir.to_path_buf());
            fs::write(dir.parent().unwrap().join("guarded"), "held").unwrap();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(found.stage2_dir, store);
    assert!(found.demoted.is_some());
    assert_eq!(
        guarded,
        [older.clone(), store.clone()].into_iter().collect()
    );
    assert!(older.parent().unwrap().join("probes").is_file());
    assert!(store.parent().unwrap().join("probes").is_file());
}

/// A rustup `trust` older than the store ranks below it and populates `demoted`.
#[cfg(unix)]
#[test]
fn a_rustup_trust_older_than_the_store_ranks_below_it_and_is_named() {
    let home = mktemp_dir("atv-demote").expect("mktemp");
    let prefix = default_atpkg_prefix(&home);
    let store = dated_bin(&home, &prefix.join("store/trust/9192"), "2026-09-17");
    std::os::unix::fs::symlink(
        prefix.join("store/trust/9192"),
        prefix.join("store/trust/current"),
    )
    .expect("ln current");
    let stage2 = home.join("trust/build/aarch64-apple-darwin/stage2");
    let tree = dated_bin(&home, &stage2, "2026-08-20");
    fs::create_dir_all(home.join("trust/build")).expect("mkdir");
    std::os::unix::fs::symlink(
        home.join("trust/build/aarch64-apple-darwin"),
        home.join("trust/build/host"),
    )
    .expect("ln host");
    fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
    std::os::unix::fs::symlink(
        home.join("trust/build/host/stage2"),
        home.join(".rustup/toolchains/trust"),
    )
    .expect("ln rustup");
    let found = || {
        Toolchain::discover_with_store(
            None,
            &home.join(".rustup"),
            Some(&prefix),
            OsStr::new(""),
            Some("trust"),
            None,
        )
    };

    let t = found();
    assert!(t.have_targo());
    assert_eq!(t.stage2_dir, store, "the older rustup entry must not win");
    let d = t.demoted.clone().expect("the demotion is named");
    assert_eq!(
        (d.dir.as_path(), d.its.as_str(), d.store.as_str()),
        (tree.as_path(), "2026-08-20", "2026-09-17")
    );
    assert!(d.linked, "the measured entry is a link");
    assert!(d.sentence().ends_with(REMEDY_LINK), "{}", d.sentence());

    let entry = home.join(".rustup/toolchains/trust");
    fs::remove_file(&entry).expect("rm the link");
    let entry_bin = dated_bin(&home, &entry, "2026-08-20");
    let t = found();
    assert_eq!(t.stage2_dir, store, "a directory entry is demoted too");
    let d = t.demoted.clone().expect("and named");
    assert_eq!((d.dir.as_path(), d.linked), (entry_bin.as_path(), false));
    let said = d.sentence();
    assert!(
        said.ends_with(REMEDY_DIRECTORY) && !said.contains(REMEDY_LINK),
        "{said}"
    );
    fs::remove_dir_all(&entry).expect("rm the directory");
    std::os::unix::fs::symlink(home.join("trust/build/host/stage2"), &entry)
        .expect("ln rustup again");

    for (date, why) in [
        ("2026-09-18", "newer"),
        ("2026-09-17", "same day"),
        ("unknown", "undated"),
    ] {
        dated_bin(&home, &stage2, date);
        let t = found();
        assert_eq!(t.stage2_dir, tree, "{why}: the rustup entry keeps its rank");
        assert_eq!(t.demoted, None, "{why}");
    }
    dated_bin(&home, &stage2, "2026-08-20");
    fs::create_dir_all(prefix.join("links")).expect("mkdir links");
    fs::write(prefix.join("links/trust"), "a dev-link marker").expect("marker");
    let t = found();
    assert_eq!(
        t.stage2_dir, tree,
        "dev-linked: nothing is stale against the store"
    );
    assert_eq!(t.demoted, None);
    fs::remove_file(prefix.join("links/trust")).expect("rm marker");

    fs::write(prefix.join("store/trust/9192/commit-date"), "2026-09-17").expect("date");
    fs::remove_dir_all(prefix.join("store/trust/9192/lib")).expect("rm sysroot");
    let on_path = pinned_bin(&home.join("elsewhere/bin"));
    let t = Toolchain::discover_with_store(
        None,
        &home.join(".rustup"),
        Some(&prefix),
        on_path.as_os_str(),
        Some("trust"),
        None,
    );
    assert_eq!(
        t.stage2_dir, tree,
        "demoted below the store, never below PATH"
    );
    assert_eq!(t.demoted, None, "it won, so nothing was demoted in effect");
    assert!(t.refused.is_none());
    fs::remove_dir_all(&home).ok();
}

/// A dev link answers each tool it links over the store's `current`: discovery reports the
/// build atpkg's shim for that tool runs (m20, 2026-11-05: with no rustup `trust` entry the
/// shims ran the linked 9178 while verify compiled and linted with `current`'s 9192), and a
/// tool the winner lacks is borrowed from the build its own shim runs. NEGATIVE CONTROLS: a
/// marker that links only `trustc`, names a relative checkout or one without a `lib`, or no
/// marker at all, and the store's `current` answers.
#[cfg(unix)]
#[test]
fn a_dev_linked_checkout_answers_over_the_stores_current() {
    let home = mktemp_dir("atv-linked").expect("mktemp");
    let prefix = default_atpkg_prefix(&home);
    let store = dated_bin(&home, &prefix.join("store/trust/9192"), "2026-09-18");
    exec_stub(&store.join(TIPPY));
    std::os::unix::fs::symlink(
        prefix.join("store/trust/9192"),
        prefix.join("store/trust/current"),
    )
    .expect("ln current");
    let checkout = home.join("trust-packs/4d9a20e3daf");
    let linked = dated_bin(&home, &checkout, "2026-09-27");
    exec_stub(&linked.join(TIPPY));
    fs::create_dir_all(prefix.join("links")).expect("mkdir links");
    let mark = |at: &str, bins: &str| {
        fs::write(
            prefix.join("links/trust"),
            format!("schema = 1\nprogram = \"trust\"\ncheckout = {at}\nbins = {bins}\n"),
        )
        .expect("marker");
    };
    let at = format!("\"{}\"", checkout.display());
    let every =
        "[\n    \"bin/targo\", # the driver\n    'bin/trustc',\n    \"bin/targo-tippy\",\n]";
    mark(&at, every);
    let found = || {
        Toolchain::discover_with_store(
            None,
            &home.join(".rustup"),
            Some(&prefix),
            OsStr::new(""),
            Some("trust"),
            None,
        )
    };

    let t = found();
    assert!(t.have_targo());
    assert_eq!(t.stage2_dir, linked, "the dev-linked build answers");
    assert_eq!(t.store_bin, Some(checkout.join("bin")));
    assert_eq!(t.tippy, Some(linked.join(TIPPY)), "its own tippy");
    assert!(t.borrowed.is_empty(), "{:?}", t.borrowed);

    let sealed = pinned_bin(&home.join("sealed/bin"));
    fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
    let entry = home.join(".rustup/toolchains/trust");
    std::os::unix::fs::symlink(home.join("sealed"), &entry).expect("ln rustup");
    let t = found();
    assert_eq!(t.stage2_dir, sealed, "a rustup entry still ranks first");
    assert_eq!(
        t.borrowed(TIPPY),
        Some(linked.join(TIPPY).as_path()),
        "the tippy its shim runs, never the store build the link left behind"
    );
    fs::remove_file(&entry).expect("rm rustup");

    for (at, bins, why) in [
        (at.as_str(), "[\"bin/trustc\"]", "a link of trustc alone"),
        ("\"trust-packs/4d9a20e3daf\"", every, "a relative checkout"),
        (at.as_str(), "\"bin/targo\"", "bins that are no array"),
    ] {
        mark(at, bins);
        let t = found();
        assert_eq!(t.stage2_dir, store, "{why}: the store answers targo");
        assert_eq!(
            t.tippy,
            Some(store.join(TIPPY)),
            "{why}: the store's own tippy"
        );
    }

    mark(&at, every);
    fs::remove_dir_all(checkout.join("lib")).expect("rm lib");
    assert_eq!(
        found().stage2_dir,
        store,
        "a link to no toolchain: the store"
    );

    fs::remove_file(prefix.join("links/trust")).expect("rm marker");
    let t = found();
    assert_eq!(t.stage2_dir, store, "no link: the store's current");
    assert_eq!(
        t.store_bin.as_deref(),
        Some(store_stage2_bin(&prefix).as_path())
    );
    fs::remove_dir_all(&home).ok();
}

/// A store build `build` under `prefix` whose `trustc` answers `--print sysroot` and `-vV`
/// (commit `commit`), with `targo`, `targo-tippy` and `trustfmt`, and its tree root recorded
/// as `root`; its `bin`.
#[cfg(unix)]
fn locked_store_build(prefix: &Path, build: u64, commit: &str, root: &str) -> PathBuf {
    let dir = crate::trust_lock::build_dir(prefix, build);
    let bin = dir.join("bin");
    fs::create_dir_all(dir.join("lib/rustlib")).expect("mkdir rustlib");
    fs::create_dir_all(&bin).expect("mkdir bin");
    for tool in ["targo", TIPPY, TRUSTFMT] {
        exec_stub(&bin.join(tool));
    }
    crate::test_script::script(
        &bin.join("trustc"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in -vV) printf 'rustc 1.0\\ncommit-hash: {commit}\\n' ;; \
             *) echo '{}' ;; esac\n",
            dir.display()
        ),
    );
    crate::trust_lock::write_tree_root_record(&dir, root).expect("record");
    fs::write(crate::trust_lock::ready_marker(prefix, build), "ok\n").expect("ready");
    fs::canonicalize(bin).expect("canonicalize")
}

/// A LOCKED TREE RESOLVES THE BUILD ITS LOCK NAMES AND NOTHING ELSE (A14): over a rustup
/// entry, the store's `current` and `PATH`, all of which answer, it takes the lock's build and
/// every tool from it; a build the store lacks, holds with another tree, or whose compiler is
/// another commit resolves nothing (no fallback); `TRUST_STAGE2_BIN` and a dev-linked checkout
/// take its place, said. NEGATIVE CONTROL: with no lock the rustup entry answers.
#[cfg(unix)]
#[test]
fn a_locked_tree_resolves_its_build_and_nothing_else() {
    let home = mktemp_dir("atv-locked").expect("mktemp");
    let prefix = default_atpkg_prefix(&home);
    let commit = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";
    let root = "4".repeat(64);
    let locked = locked_store_build(&prefix, 16387, commit, &root);
    let current = locked_store_build(&prefix, 16500, &"b".repeat(40), &"5".repeat(64));
    std::os::unix::fs::symlink(
        crate::trust_lock::build_dir(&prefix, 16500),
        prefix.join("store/trust/current"),
    )
    .expect("ln current");
    let sealed = pinned_bin(&home.join("sealed/bin"));
    fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
    std::os::unix::fs::symlink(home.join("sealed"), home.join(".rustup/toolchains/trust"))
        .expect("ln rustup");
    let on_path = pinned_bin(&home.join("onpath/bin"));
    let tree = home.join("checkout");
    fs::create_dir_all(&tree).expect("mkdir tree");
    let lock = |build: u64, root: &str, commit: &str| {
        let lock = crate::trust_lock::TrustLock {
            build,
            tree_root: root.to_string(),
            compiler_commit: commit.to_string(),
            receipt: "SEED-MINT sha256:00".to_string(),
            stock_toolchain: "1.99.1".to_string(),
        };
        fs::write(
            tree.join(crate::trust_lock::LOCK_FILE),
            crate::trust_lock::render_body(&lock),
        )
        .expect("write lock");
        crate::trust_lock::find(&tree).expect("the lock")
    };
    let found = |stage2: Option<&Path>, lock: Option<&crate::trust_lock::Found>| {
        Toolchain::discover_with_store(
            stage2,
            &home.join(".rustup"),
            Some(&prefix),
            on_path.as_os_str(),
            Some("trust"),
            lock,
        )
    };

    let t = found(None, None);
    assert_eq!(t.stage2_dir, sealed, "no lock: the rustup entry answers");

    let named = lock(16387, &root, commit);
    let t = found(None, Some(&named));
    assert!(t.have_targo(), "{}", t.missing_targo_label());
    assert_eq!(
        t.stage2_dir, locked,
        "the lock's build, over rustup, current and PATH"
    );
    assert_eq!(t.tippy, Some(locked.join(TIPPY)));
    assert_eq!(t.trustfmt, Some(locked.join(TRUSTFMT)));
    assert!(t.borrowed.is_empty(), "{:?}", t.borrowed);
    assert_eq!(
        t.store_bin,
        Some(crate::trust_lock::build_dir(&prefix, 16387).join("bin"))
    );
    assert!(t.lock_override.is_none() && t.lock_refusal.is_none());

    fs::remove_file(locked.join(TRUSTFMT)).expect("rm trustfmt");
    let t = found(None, Some(&named));
    assert_eq!(
        t.trustfmt, None,
        "a locked tree borrows no formatter of another build"
    );
    assert!(t.borrowed.is_empty(), "{:?}", t.borrowed);

    for (named, why, says) in [
        (lock(16999, &root, commit), "absent", "does not hold"),
        (
            lock(16387, &"e".repeat(64), commit),
            "another tree",
            "another build",
        ),
        (
            lock(16387, &root, &"c".repeat(40)),
            "another compiler",
            "reports commit",
        ),
    ] {
        let t = found(None, Some(&named));
        assert!(!t.have_targo(), "{why}");
        let label = t.missing_targo_label();
        assert!(label.contains(says), "{why}: {label}");
        for fallback in [&sealed, &on_path, &current] {
            assert_ne!(&t.stage2_dir, fallback, "{why}: no fallback");
        }
    }

    let named = lock(16387, &root, commit);
    let t = found(Some(&on_path), Some(&named));
    assert_eq!(t.stage2_dir, on_path);
    assert!(t.have_targo());
    assert!(
        t.lock_override
            .as_deref()
            .is_some_and(|o| o.starts_with("TRUST_STAGE2_BIN=")),
        "{:?}",
        t.lock_override
    );

    // A dev-linked checkout takes the place of a lock, readable or not, as the shim's hop
    // runs it.
    let dev = pinned_bin(&home.join("devlink/bin"));
    fs::create_dir_all(prefix.join("links")).expect("mkdir links");
    fs::write(
        prefix.join("links/trust"),
        format!(
            "schema = 1\nprogram = \"trust\"\ncheckout = \"{}\"\nbins = [\"bin/targo\", \
             \"bin/trustc\"]\n",
            home.join("devlink").display()
        ),
    )
    .expect("marker");
    let unreadable = {
        fs::write(tree.join(crate::trust_lock::LOCK_FILE), "version = 2\n").expect("lock");
        crate::trust_lock::find(&tree).expect("the lock")
    };
    for (named, why) in [(&named, "a lock"), (&unreadable, "an unreadable lock")] {
        let t = found(None, Some(named));
        assert!(t.have_targo(), "{why}: {}", t.missing_targo_label());
        assert_eq!(t.stage2_dir, dev, "{why}");
        assert!(
            t.lock_override
                .as_deref()
                .is_some_and(|o| o.starts_with("the dev-linked checkout")),
            "{why}: {:?}",
            t.lock_override
        );
    }
    fs::remove_dir_all(&home).ok();
}

/// An `atpkg` rustup view resolves through to the store build or dev-linked checkout it
/// presents.
#[cfg(unix)]
#[test]
fn atpkgs_view_is_read_through_to_the_build_it_presents() {
    let home = mktemp_dir("atv-view").expect("mktemp");
    let prefix = default_atpkg_prefix(&home);
    let store = dated_bin(&home, &prefix.join("store/trust/9200"), "2026-09-24");
    std::os::unix::fs::symlink(
        prefix.join("store/trust/9200"),
        prefix.join("store/trust/current"),
    )
    .expect("ln current");
    let view = dated_bin(&home, &prefix.join("rustup/trust"), "2026-09-17");
    fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
    let entry = home.join(".rustup/toolchains/trust");
    std::os::unix::fs::symlink(prefix.join("rustup/trust"), &entry).expect("ln rustup");
    let found = || {
        Toolchain::discover_with_store(
            None,
            &home.join(".rustup"),
            Some(&prefix),
            OsStr::new(""),
            Some("trust"),
            None,
        )
    };
    let t = found();
    assert_eq!(
        t.stage2_dir, store,
        "a lagging view presents the store's build"
    );
    assert_ne!(t.stage2_dir, view, "never the view's own clones");
    assert_eq!(t.demoted, None, "read through, so never demoted");
    dated_bin(&home, &prefix.join("rustup/trust"), "2026-09-24");
    assert_eq!(
        found().stage2_dir,
        store,
        "a current view: the store's own directory"
    );
    let checkout = home.join("trust-checkout");
    let linked = dated_bin(&home, &checkout, "2026-09-20");
    fs::create_dir_all(prefix.join("links")).expect("mkdir links");
    fs::write(
        prefix.join("links/trust"),
        format!(
            "program = \"trust\"\ncheckout = '{}' # literal with comment\nbins = [\"bin/targo\"]\n",
            checkout.display()
        ),
    )
    .expect("marker");
    assert_eq!(
        found().stage2_dir,
        linked,
        "a linked view presents its checkout"
    );
    fs::remove_dir_all(checkout.join("lib")).expect("rm lib");
    assert_eq!(
        found().stage2_dir,
        store,
        "an unpresentable link: the store"
    );
    fs::write(prefix.join("links/trust"), "not a marker").expect("garbage");
    assert_eq!(found().stage2_dir, store, "an unreadable marker is no link");
    fs::remove_file(prefix.join("links/trust")).expect("rm marker");
    let tree = dated_bin(&home, &home.join("stage2"), "2026-09-17");
    fs::remove_file(&entry).expect("rm link");
    std::os::unix::fs::symlink(home.join("stage2"), &entry).expect("ln foreign");
    let d = found().demoted.expect("a foreign older link is said");
    assert_eq!(d.dir, tree);
    assert!(d.sentence().ends_with(REMEDY_LINK));
    fs::remove_dir_all(&home).ok();
}

#[test]
fn the_prefix_mirror_honours_a_configured_packages_prefix() {
    let home = Path::new("/fake-home");
    let opt = |rest: &str| {
        if cfg!(windows) {
            format!("C:/opt/{rest}")
        } else {
            format!("/opt/{rest}")
        }
    };
    assert_eq!(
        configured_prefix_in("[packages]\nprefix = \"~/lab/pkg\"\n", home),
        Some(PathBuf::from("/fake-home/lab/pkg"))
    );
    assert_eq!(
        configured_prefix_in(
            "[ packages ] # inline comment\nprefix_note = \"~/decoy\"\nprefix = '~/lab/pkg' # literal\n",
            home
        ),
        Some(PathBuf::from("/fake-home/lab/pkg")),
        "table header comments/whitespace, sibling prefix_* keys, and literal strings must work"
    );
    assert_eq!(
        configured_prefix_in("[packages]\nprefix = \"~/lab/\\u0070kg\"\n", home),
        Some(PathBuf::from("/fake-home/lab/pkg")),
        "basic-string escapes must decode"
    );
    assert_eq!(
        configured_prefix_in("[packages]\nprefix = \"~/lab/pkg\" trailing\n", home),
        None,
        "trailing non-comment tokens after closing quote must fail closed"
    );
    assert_eq!(
        configured_prefix_in("[packages.links]\nprefix = \"~/lab/pkg\"\n", home),
        None,
        "subtables must not match [packages]"
    );
    assert_eq!(
        configured_prefix_in(
            &format!(
                "[packages]\n# prefix = \"~/decoy\"\nprefix = \"{}\"\n",
                opt("aterm/pkg")
            ),
            home
        ),
        Some(PathBuf::from(opt("aterm/pkg")))
    );
    assert_eq!(
        configured_prefix_in("[packages]\nprefix = \"relative/pkg\"\n", home),
        None
    );
    assert_eq!(
        configured_prefix_in("[packages]\nprefix = \"~user/pkg\"\n", home),
        None
    );
    assert_eq!(
        configured_prefix_in("[other]\nprefix = \"/opt/x\"\n", home),
        None
    );
    assert_eq!(configured_prefix_in("", home), None);

    let tmp = mktemp_dir("atv-prefix").expect("mktemp");
    assert_eq!(atpkg_prefix(&tmp, None), default_atpkg_prefix(&tmp));
    let xdg = tmp.join("xdg");
    fs::create_dir_all(xdg.join("aterm")).expect("mkdir");
    fs::write(
        xdg.join("aterm/aterm.toml"),
        format!("[packages]\nprefix = \"{}\"\n", opt("lab")),
    )
    .expect("write");
    assert_eq!(atpkg_prefix(&tmp, Some(&xdg)), PathBuf::from(opt("lab")));
    fs::create_dir_all(tmp.join(".config/aterm")).expect("mkdir");
    fs::write(
        tmp.join(".config/aterm/aterm.toml"),
        "[packages]\nprefix = \"~/mine\"\n",
    )
    .expect("write");
    assert_eq!(atpkg_prefix(&tmp, None), tmp.join("mine"));
    assert_eq!(
        store_stage2_bin(&default_atpkg_prefix(Path::new("/h"))),
        Path::new(if cfg!(target_os = "macos") {
            "/h/Library/Application Support/aterm/pkg/store/trust/current/bin"
        } else if cfg!(windows) {
            "/h/AppData/Local/aterm/pkg/store/trust/current/bin"
        } else {
            "/h/.local/share/aterm/pkg/store/trust/current/bin"
        })
    );
    fs::remove_dir_all(&tmp).ok();
}

/// Symlinked toolchain directories are canonicalized to their physical path.
#[cfg(unix)]
#[test]
fn a_symlinked_stage2_resolves_to_its_physical_path() {
    let tmp = mktemp_dir("atv-tc").expect("mktemp");
    let real = tmp.join("aarch64-apple-darwin/stage2/bin");
    fs::create_dir_all(&real).expect("mkdir");
    exec_stub(&real.join("targo"));
    std::os::unix::fs::symlink(tmp.join("aarch64-apple-darwin"), tmp.join("host")).expect("ln");

    let via_link = tmp.join("host/stage2/bin");
    let t = Toolchain::discover(
        Some(&via_link),
        Path::new("/unused"),
        OsStr::new(""),
        None,
        None,
    );
    assert!(t.have_targo());
    assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));
    assert!(
        !t.stage2_dir.to_string_lossy().contains("/host/"),
        "the symlinked spelling must not survive into the tool paths"
    );
    fs::remove_dir_all(&tmp).ok();
}

#[test]
fn tippy_is_found_by_its_trust_name_only() {
    let tmp = mktemp_dir("atv-tippy").expect("mktemp");
    let real = fs::canonicalize(&tmp).expect("canonicalize");
    exec_stub(&tmp.join("targo-clippy"));
    let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
    assert_eq!(
        t.tippy, None,
        "the retired pre-rebrand name is not a tippy: the lane is NOT RUN"
    );

    exec_stub(&tmp.join("targo-tippy"));
    let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
    assert_eq!(
        t.tippy.as_deref(),
        Some(real.join(tool_file("targo-tippy")).as_path())
    );
    fs::remove_dir_all(&tmp).ok();
}

/// A non-override toolchain missing `trustfmt`/`targo-tippy` borrows them from the managed
/// build.
#[cfg(unix)]
#[test]
fn a_toolchain_lacking_the_formatter_and_lint_takes_the_managed_builds() {
    let home = mktemp_dir("atv-borrow").expect("mktemp");
    let home = fs::canonicalize(&home).expect("canonicalize");
    let prefix = default_atpkg_prefix(&home);
    let store = pinned_bin(&prefix.join("store/trust/9192/bin"));
    for tool in [TRUSTFMT, TIPPY] {
        exec_stub(&store.join(tool));
    }
    std::os::unix::fs::symlink(
        prefix.join("store/trust/9192"),
        prefix.join("store/trust/current"),
    )
    .expect("ln current");
    let stage2 = pinned_bin(&home.join("trust/build/host/stage2/bin"));
    fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
    std::os::unix::fs::symlink(
        home.join("trust/build/host/stage2"),
        home.join(".rustup/toolchains/trust"),
    )
    .expect("ln rustup");
    let stray = home.join("stray/bin");
    fs::create_dir_all(&stray).expect("mkdir");
    exec_stub(&stray.join(TRUSTFMT));
    let found = |explicit: Option<&Path>, path: &OsStr| {
        Toolchain::discover_with_store(
            explicit,
            &rustup_home(None, &home),
            Some(&prefix),
            path,
            Some("trust"),
            None,
        )
    };
    let t = found(None, OsStr::new(""));
    assert_eq!(
        t.stage2_dir, stage2,
        "the toolchain is still the rustup entry"
    );
    assert!(t.have_targo());
    assert_eq!(t.trustfmt.as_deref(), Some(store.join(TRUSTFMT).as_path()));
    assert_eq!(t.tippy.as_deref(), Some(store.join(TIPPY).as_path()));
    assert_eq!(t.borrowed(TRUSTFMT), Some(store.join(TRUSTFMT).as_path()));
    let note = t.borrowed_note().expect("said");
    assert!(
        note.contains(&format!("trustfmt from {}", store.join(TRUSTFMT).display()))
            && note.contains(&format!("targo-tippy from {}", store.join(TIPPY).display()))
            && note.contains(&format!("{} has none", stage2.display())),
        "{note}"
    );
    let t = found(Some(&stage2), OsStr::new(""));
    assert_eq!(
        (t.stage2_dir.as_path(), t.store_bin.is_none()),
        (stage2.as_path(), true)
    );
    assert_eq!((t.trustfmt.as_deref(), t.tippy.as_deref()), (None, None));
    assert!(t.borrowed.is_empty());
    let skip = t.missing_tippy_label();
    assert!(
        skip.contains("TRUST_STAGE2_BIN=") && skip.contains(Toolchain::OVERRIDE_REMEDY),
        "{skip}"
    );

    let moved = home.join("store-away");
    fs::rename(prefix.join("store"), &moved).expect("move the store away");
    let t = found(None, stray.as_os_str());
    assert_eq!((t.trustfmt.as_deref(), t.tippy.as_deref()), (None, None));
    assert!(t.borrowed.is_empty() && t.borrowed_note().is_none());
    assert!(
        t.missing_trustfmt_label()
            .contains("the atpkg store or PATH")
            && t.missing_tippy_label().contains("the atpkg store or PATH"),
        "the skip names everywhere it looked: {}",
        t.missing_tippy_label()
    );
    fs::rename(&moved, prefix.join("store")).expect("move the store back");
    let current = prefix.join("store/trust/current");
    let hidden = prefix.join("store/trust/hidden-current");
    fs::rename(&current, &hidden).expect("hide current");
    let shims = pinned_bin(&prefix.join("bin"));
    crate::test_script::script(
        &shims.join("trustc"),
        &format!(
            "#!/bin/sh\necho '{}'\n",
            store.parent().expect("sysroot").display()
        ),
    );
    exec_stub(&shims.join(TIPPY));
    let path = std::env::join_paths([&stray, &shims]).expect("join");
    for pin in [None, Some("stable")] {
        assert_eq!(
            managed_tool(TIPPY, None, shims.as_os_str(), pin),
            Some(shims.join(TIPPY))
        );
    }
    let t = found(None, &path);
    assert_eq!(
        t.tippy.as_deref(),
        Some(store.join(TIPPY).as_path()),
        "BORROWED_TOOL_MUST_NAME_THE_REPORTED_PHYSICAL_BUILD"
    );
    assert_eq!(t.trustfmt, None, "the stray one is still not taken");
    let parked = store.join("tippy-away");
    fs::rename(store.join(TIPPY), &parked).expect("hide the physical tool");
    assert_eq!(
        found(None, &path).tippy,
        None,
        "a shim alone is not the tool"
    );
    fs::rename(&parked, store.join(TIPPY)).expect("restore the physical tool");
    fs::rename(&hidden, &current).expect("restore current");

    for tool in [TRUSTFMT, TIPPY] {
        exec_stub(&stage2.join(tool));
    }
    let t = found(None, OsStr::new(""));
    assert_eq!(t.trustfmt.as_deref(), Some(stage2.join(TRUSTFMT).as_path()));
    assert_eq!(t.tippy.as_deref(), Some(stage2.join(TIPPY).as_path()));
    assert!(t.borrowed.is_empty() && t.borrowed_note().is_none());
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn path_is_only_rewritten_when_a_targo_is_really_there() {
    let tmp = mktemp_dir("atv-path").expect("mktemp");
    let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
    assert_eq!(
        t.path_with_stage2_first(OsStr::new("/usr/bin")),
        OsString::from("/usr/bin")
    );

    exec_stub(&tmp.join("targo"));
    let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
    let want =
        std::env::join_paths([t.stage2_dir.as_os_str(), OsStr::new("/usr/bin")]).expect("join");
    assert_eq!(t.path_with_stage2_first(OsStr::new("/usr/bin")), want);
    fs::remove_dir_all(&tmp).ok();
}

#[test]
fn the_pinned_channel_is_the_one_line_the_manifest_declares() {
    assert_eq!(
        pinned_channel_in("# channel = \"decoy\"\n[toolchain]\nchannel = \"trust\"\n").as_deref(),
        Some("trust"),
        "a commented-out channel must not be mistaken for the pin"
    );
    assert_eq!(
        pinned_channel_in(
            "[other]\nchannel = \"decoy\"\n[toolchain] # comment\nchannel = \"trust\"\n"
        )
        .as_deref(),
        Some("trust"),
        "a channel key in another table must not be mistaken for [toolchain].channel"
    );
    assert_eq!(
        pinned_channel_in("[other]\nchannel = \"decoy\"\n").as_deref(),
        None
    );
    assert_eq!(
        pinned_channel_in("channel = \"trust\"\n").as_deref(),
        Some("trust")
    );
    assert_eq!(
        pinned_channel_in(
            "[ toolchain ] # comment\nchannels = [\"decoy\"]\nchannel = 'trust' # literal\n"
        )
        .as_deref(),
        Some("trust")
    );
    assert_eq!(
        pinned_channel_in("[toolchain]\nchannel = \"trust\" trailing\n").as_deref(),
        None
    );
    assert_eq!(
        pinned_channel_in("[toolchain]\nchannel = \"\"\"trust\"\"\"\n").as_deref(),
        None
    );
    assert_eq!(pinned_channel_in("[toolchain]\n").as_deref(), None);
    assert_eq!(
        pinned_channel(Path::new("/nonexistent-repo")).as_deref(),
        None
    );
    let tmp = mktemp_dir("atv-pin-bounded").expect("mktemp");
    fs::write(
        tmp.join("rust-toolchain.toml"),
        format!(
            "[toolchain]\nchannel = \"trust\"\n# {}\n",
            "x".repeat(65 << 10)
        ),
    )
    .expect("write");
    assert_eq!(
        pinned_channel(&tmp),
        None,
        "an oversized rust-toolchain.toml is rejected"
    );
    fs::remove_dir_all(&tmp).ok();
}

#[test]
fn a_targo_that_is_not_the_pinned_toolchain_is_refused_not_adopted() {
    let tmp = mktemp_dir("atv-pin").expect("mktemp");
    let bare = tmp.join("bare");
    fs::create_dir_all(&bare).expect("mkdir");
    exec_stub(&bare.join("targo"));

    let t = Toolchain::discover(
        Some(&bare),
        Path::new("/unused"),
        OsStr::new(""),
        Some("trust"),
        None,
    );
    assert!(!t.have_targo(), "fail-closed: not the pinned toolchain");
    assert!(t.refused.is_some());

    let half = tmp.join("half");
    fs::create_dir_all(&half).expect("mkdir");
    exec_stub(&half.join("targo"));
    exec_stub(&half.join("trustc"));
    let t = Toolchain::discover(
        Some(&half),
        Path::new("/unused"),
        OsStr::new(""),
        Some("trust"),
        None,
    );
    assert!(
        !t.have_targo(),
        "a driver that names no sysroot is not the pin (mid-rebuild stage tree)"
    );
    assert!(t.refused.is_some());

    let pinned = tmp.join("pinned");
    fs::create_dir_all(&pinned).expect("mkdir");
    exec_stub(&pinned.join("targo"));
    driver_stub(&pinned.join("trustc"));
    let t = Toolchain::discover(
        Some(&pinned),
        Path::new("/unused"),
        OsStr::new(""),
        Some("trust"),
        None,
    );
    assert!(t.have_targo());
    assert!(t.refused.is_none());

    let t = Toolchain::discover(
        Some(&bare),
        Path::new("/unused"),
        OsStr::new(""),
        Some("stable"),
        None,
    );
    assert!(t.have_targo(), "upstream channels are a passthrough");
    fs::remove_dir_all(&tmp).ok();
}

#[test]
fn the_path_fallback_adopts_only_the_pinned_toolchain() {
    let tmp = mktemp_dir("atv-pinpath").expect("mktemp");
    let impostor = tmp.join("impostor");
    let real = tmp.join("stage/bin");
    fs::create_dir_all(&impostor).expect("mkdir");
    fs::create_dir_all(&real).expect("mkdir");
    exec_stub(&impostor.join("targo"));
    exec_stub(&real.join("targo"));
    driver_stub(&real.join("trustc"));
    let path = std::env::join_paths([&impostor, &real]).expect("join");
    let home = Path::new("/nonexistent-home");
    let discover = |path: &OsStr| {
        Toolchain::discover_with_store(
            None,
            &rustup_home(None, home),
            Some(&default_atpkg_prefix(home)),
            path,
            Some("trust"),
            None,
        )
    };

    let t = discover(OsStr::new(&path));
    assert!(t.have_targo());
    assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));

    let t = discover(OsStr::new(impostor.to_string_lossy().as_ref()));
    assert!(!t.have_targo());
    assert_eq!(
        t.refused,
        Some(fs::canonicalize(&impostor).expect("canonicalize"))
    );
    fs::remove_dir_all(&tmp).ok();
}

#[test]
fn a_directory_named_targo_is_not_a_driver() {
    let tmp = mktemp_dir("atv-dir").expect("mktemp");
    fs::create_dir_all(tmp.join("targo")).expect("mkdir");
    let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
    assert!(
        !t.have_targo(),
        "fail-closed: a directory is not the verified driver"
    );
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::PermissionsExt as _;
        let fifo_dir = tmp.join("fifo-bin");
        fs::create_dir_all(&fifo_dir).expect("mkdir fifo-bin");
        let fifo = fifo_dir.join(tool_file("targo"));
        let c_fifo = CString::new(fifo.as_os_str().as_bytes()).expect("cstr");
        // SAFETY: `c_fifo` is a live NUL-terminated path in our private temp directory; `mkfifo` retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o755) }, 0, "mkfifo");
        fs::set_permissions(&fifo, fs::Permissions::from_mode(0o755)).expect("chmod fifo");
        assert!(!is_executable_file(&fifo));
        assert!(!is_pending_stub(&fifo));
        let t = Toolchain::discover(
            Some(&fifo_dir),
            Path::new("/unused"),
            OsStr::new(""),
            None,
            None,
        );
        assert!(
            !t.have_targo(),
            "fail-closed: an executable FIFO is not a driver and must not hang discovery"
        );
    }
    fs::remove_dir_all(&tmp).ok();
}

#[cfg(unix)]
#[test]
fn a_delayed_compiler_still_answers_its_identity_probe() {
    let fixture = aterm_tempfile::tempdir().unwrap();
    let bin = dated_bin(
        fixture.path(),
        &fixture.path().join("compiler"),
        "2026-09-17",
    );
    let answer = bounded_stdout_at(
        &bin.join("trustc"),
        &["--print", "sysroot"],
        probe_bound(),
        std::time::Instant::now() - Duration::from_secs(6),
    );
    assert_eq!(
        answer.as_deref().map(str::trim),
        bin.parent().unwrap().to_str()
    );
    assert_eq!(
        bounded_stdout_at(
            &bin.join("trustc"),
            &["--print", "sysroot"],
            probe_bound(),
            std::time::Instant::now() - Duration::from_secs(61),
        ),
        None,
    );
}

#[cfg(unix)]
#[test]
fn bounded_stdout_drains_large_output_without_pipe_deadlock_and_rejects_oversize() {
    let sh = Path::new("/bin/sh");
    let exact = bounded_stdout(
        sh,
        &[
            "-c",
            "dd if=/dev/zero bs=1024 count=64 2>/dev/null | tr '\\0' x",
        ],
        aterm_spec::wait::MUST_HAPPEN,
    );
    assert_eq!(exact.as_ref().map(String::len), Some(64 << 10));
    let over = bounded_stdout(
        sh,
        &[
            "-c",
            "dd if=/dev/zero bs=1024 count=65 2>/dev/null | tr '\\0' x",
        ],
        aterm_spec::wait::MUST_HAPPEN,
    );
    assert_eq!(over, None);
}
