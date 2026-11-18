// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// What the store gave, or the calling test's end: COULD NOT RUN where this machine has no pair
/// to judge with ([`Unready::Absent`]: a scratch HOME's prefix, an uninstalled app), a failure
/// where the pair it has is wrong ([`Unready::Defect`]).
fn ready<T>(got: Result<T, Unready>) -> T {
    match got {
        Ok(value) => value,
        Err(Unready::Absent(why)) => aterm_uds::host::could_not_run(why),
        Err(Unready::Defect(why)) => panic!("the installed pair: {why}"),
    }
}

/// This machine's package store, or the calling test's end ([`ready`]).
fn store(env: &EnvSnapshot) -> Store {
    ready(Store::read(env))
}

/// This machine's package store and its installed release's `--version` line, or the calling
/// test's end ([`ready`]).
fn installed_release(env: &EnvSnapshot) -> (Store, String) {
    let store = store(env);
    let line = ready(store.release());
    (store, line)
}

/// The installed release's pair in an account's home: `released-shim`'s rig, its tree pinned to
/// this checkout's compiler. A rig the pair cannot be made into (a shim with no store path to
/// relocate) fails the test.
fn installed_rig(env: &EnvSnapshot) -> Rig {
    let (store, label) = installed_release(env);
    let rig = Rig::new(env, &store, Pair::Installed, label, Placement::SharedCache)
        .unwrap_or_else(|why| panic!("a rig of the installed pair: {why}"));
    pin_fixture_toolchain(&rig.tree);
    rig
}

/// Miniature build fixtures use this checkout's compiler, like its real bootstrap inputs.
fn pin_fixture_toolchain(tree: &Path) {
    fs::copy(
        aterm_tempfile::manifest_dir().join("../../trust.lock"),
        tree.join("trust.lock"),
    )
    .unwrap();
}

/// A package prefix `shim` (when given) is the `bin/targo` of, with an empty Trust store.
fn prefix_with(shim: Option<&str>) -> aterm_tempfile::TempDir {
    let prefix = aterm_tempfile::tempdir().unwrap();
    fs::create_dir_all(prefix.path().join("store/trust")).unwrap();
    if let Some(text) = shim {
        fs::create_dir_all(prefix.path().join("bin")).unwrap();
        fs::write(prefix.path().join("bin/targo"), text).unwrap();
    }
    prefix
}

/// A caller whose configured package prefix is `prefix`.
fn env_at(prefix: &Path) -> EnvSnapshot {
    EnvSnapshot {
        test_package_prefix: Some(prefix.to_path_buf()),
        ..EnvSnapshot::default()
    }
}

/// Where an installed `aterm.app` keeps its helper and `aterm`.
const APP: &str = "aterm.app/Contents/MacOS";

/// A helper `atpkg` (a file) in the folder `at` names, in a new temporary folder, beside an
/// `aterm` whose `--version` says `said`; the helper.
fn helper_saying(at: &str, said: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let bin = dir.path().join(at);
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("atpkg"), "").unwrap();
    let aterm = bin.join("aterm");
    fs::write(&aterm, format!("#!/bin/sh\necho '{said}'\n")).unwrap();
    fs::set_permissions(&aterm, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, bin.join("atpkg"))
}

/// The line by which a shim names its helper (`shim_helper`).
fn helper_line(helper: &Path) -> String {
    format!("# atpkg cache helper \"{}\"\n", helper.display())
}

/// A package prefix with no `bin/targo` (a scratch HOME's) holds no pair: absent, naming the
/// missing shim and where it comes from.
#[test]
fn a_prefix_with_no_targo_shim_is_absent() {
    let prefix = prefix_with(None);
    match Store::read(&env_at(prefix.path())) {
        Err(Unready::Absent(why)) => assert!(
            why.contains(&prefix.path().join("bin/targo").display().to_string())
                && why.contains("aterm pkg install trust"),
            "{why}"
        ),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("a prefix with no shim gave a store"),
    }
}

/// A shim whose `aterm.app` is gone (the release was uninstalled) leads to no installed public
/// release: absent, naming the helper and the app.
#[test]
fn a_shim_whose_installed_app_is_gone_is_absent() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let app = dir.path().join("aterm.app");
    let helper = app.join("Contents/MacOS/atpkg");
    let prefix = prefix_with(Some(&helper_line(&helper)));
    let store = Store::read(&env_at(prefix.path())).unwrap();
    match store.release() {
        Err(Unready::Absent(why)) => assert!(
            why.contains(&helper.display().to_string()) && why.contains(&app.display().to_string()),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
}

/// A pair that is there and wrong is a defect, never an absence: a shim that cannot be read or
/// names no helper, a helper outside an `aterm.app` (loose, or in a dev bundle), an installed
/// app without the helper, and a development app. The loose and dev-bundle helpers sit beside an
/// `aterm` that says a public version, so only their place refuses them. A public pair whose
/// shim has no store path to relocate gives no rig.
#[test]
fn an_installed_pair_that_is_there_and_wrong_is_a_defect() {
    let (_public, public) = helper_saying(APP, "aterm 0.199.0");
    let (_dev, dev) = helper_saying(APP, "aterm 0.199.0+g0123456789a");
    let (_bare, bare) = helper_saying(APP, "aterm 0.199.0");
    fs::remove_file(&bare).unwrap();
    let (_loose, loose) = helper_saying("target/debug", "aterm 0.199.0");
    let (_bundle, bundle) = helper_saying("aterm (dev).app/Contents/MacOS", "aterm 0.199.0");
    for (what, shim) in [
        ("no helper", "#!/bin/sh\nexec targo \"$@\"\n".to_owned()),
        ("a loose helper", helper_line(&loose)),
        ("a dev bundle's helper", helper_line(&bundle)),
        ("an installed app without the helper", helper_line(&bare)),
        ("a development app", helper_line(&dev)),
    ] {
        let prefix = prefix_with(Some(&shim));
        let got = Store::read(&env_at(prefix.path())).unwrap().release();
        assert!(matches!(got, Err(Unready::Defect(_))), "{what}: {got:?}");
    }
    let unreadable = prefix_with(None);
    fs::create_dir_all(unreadable.path().join("bin/targo")).unwrap();
    let got = Store::read(&env_at(unreadable.path()));
    assert!(
        matches!(got, Err(Unready::Defect(_))),
        "a shim that cannot be read: {:?}",
        got.err()
    );
    let prefix = prefix_with(Some(&helper_line(&public)));
    let env = env_at(prefix.path());
    let store = Store::read(&env).unwrap();
    let label = store.release().unwrap();
    assert_eq!(label, "aterm 0.199.0");
    let rig = Rig::new(&env, &store, Pair::Installed, label, Placement::SharedCache);
    assert!(
        rig.as_ref().is_err_and(|why| why.contains("relocatable")),
        "no relocatable store path: {:?}",
        rig.err()
    );
}

#[test]
fn candidate_listing_uses_the_selected_git_provider() {
    let env = EnvSnapshot::capture();
    let dir = aterm_tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let mut cmd = crate::git_provider::command();
        cmd.current_dir(dir.path()).args(args);
        for name in crate::changed::GIT_REDIRECTS {
            cmd.env_remove(name);
        }
        let out =
            aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "--quiet"]);
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(dir.path().join("untracked"), "not a source input\n").unwrap();
    run(&["add", "--", "Cargo.toml"]);
    assert_eq!(
        git_files(dir.path(), &env).unwrap(),
        [PathBuf::from("Cargo.toml")]
    );
}

/// The judgment is the rig's first build. aterm 0.199.0's shim took its no-cache placement in
/// a home with no `Library/Caches`, its helper refused that placement, and every later build in
/// the same home passed: a test whose accepted build came second never saw it.
#[test]
fn the_rigs_first_build_is_in_a_home_with_a_caches_folder() {
    let mut env = EnvSnapshot::capture();
    let absent = aterm_tempfile::tempdir().unwrap();
    env.rustup_home = Some(absent.path().join("rustup-not-installed").into_os_string());
    let rig = installed_rig(&env);
    let context = crate::Ctx::new(
        rig.tree.clone(),
        crate::Scope::workspace(),
        env.clone(),
        rig.home.join("run"),
    );
    assert!(rig.home.join("Library/Caches").is_dir());
    io(fs::create_dir_all(rig.tree.join("src"))).unwrap();
    io(fs::write(
        rig.tree.join("Cargo.toml"),
        "[package]\nname='aterm-time'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
    ))
    .unwrap();
    io(fs::write(
        rig.tree.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"aterm-time\"\nversion = \"0.0.0\"\n",
    ))
    .unwrap();
    io(fs::write(
        rig.tree.join("src/lib.rs"),
        "pub fn bootstrap_probe() {}\n",
    ))
    .unwrap();
    let first = rig.build(context.exec_env()).unwrap();
    assert!(first.run.ok, "{}", &first.run.output);
    assert!(built_artifact(
        first.stdout.as_deref().unwrap().as_bytes(),
        &rig.home
    ));
}

#[test]
fn installed_release_rejects_config_target_and_accepts_jobs() {
    let env = EnvSnapshot::capture();
    let rig = installed_rig(&env);
    let context = crate::Ctx::new(
        rig.tree.clone(),
        crate::Scope::workspace(),
        env.clone(),
        rig.home.join("run"),
    );
    io(fs::create_dir_all(rig.tree.join("src"))).unwrap();
    io(fs::create_dir_all(rig.tree.join(".cargo"))).unwrap();
    io(fs::write(
        rig.tree.join("Cargo.toml"),
        format!("[package]\nname='aterm-time'\nversion='0.0.0'\nedition='2024'\n[workspace]\n[workspace.package]\nversion='{}'\n", rig.label.trim_start_matches("aterm ")),
    ))
    .unwrap();
    io(fs::write(
        rig.tree.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"aterm-time\"\nversion = \"0.0.0\"\n",
    ))
    .unwrap();
    io(fs::write(
        rig.tree.join("src/lib.rs"),
        "pub fn bootstrap_probe() {}\n",
    ))
    .unwrap();
    // Exact regression introduced by ff1626d7ab8, exercised by the release pair.
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"target/.ubd/main.noindex\"\n",
    ))
    .unwrap();
    let bad = rig.build(context.exec_env()).unwrap();
    let said = &bad.run.output;
    if rig.label == "aterm 0.197.0" {
        assert!(!bad.run.ok, "0.197.0 must reject ff1626d7ab8");
    }
    // Later public releases may support the layout; the 0.197.0 pair must refuse it.
    if !bad.run.ok {
        assert!(
            said.contains("explicit build target is unmanaged or unsupported"),
            "{said}"
        );
    }
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\njobs = 1\n",
    ))
    .unwrap();
    let good = rig.build(context.exec_env()).unwrap();
    assert!(good.run.ok, "{}", &good.run.output);
    assert!(rig.tree.join("target/debug/libaterm_time.rlib").is_file());
    assert!(built_artifact(
        good.stdout.as_deref().unwrap().as_bytes(),
        &rig.home
    ));

    // The same production stage must name the changed input, rather than merely fail
    // an unrelated source build. Commit the accepted config, then reproduce ff1626d7ab8.
    for args in [
        vec!["init", "-q"],
        vec![
            "add",
            "Cargo.toml",
            "Cargo.lock",
            "trust.lock",
            ".cargo/config.toml",
            "src/lib.rs",
        ],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "accepted bootstrap",
        ],
    ] {
        let mut command = crate::git_provider::command();
        command.current_dir(&rig.tree).args(args);
        let out = io(aterm_uds::deadline::output_within(
            &mut command,
            Duration::from_secs(60),
        ))
        .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut command = crate::git_provider::command();
    command.current_dir(&rig.tree).args([
        "tag",
        &format!("v{}", rig.label.trim_start_matches("aterm ")),
    ]);
    let out = io(aterm_uds::deadline::output_within(
        &mut command,
        Duration::from_secs(60),
    ))
    .unwrap();
    assert!(out.status.success());
    let context = crate::Ctx::new(
        rig.tree.clone(),
        crate::Scope::changed("HEAD", Vec::new(), false),
        env,
        rig.home.join("run"),
    );
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"target/.ubd/main.noindex\"\n",
    ))
    .unwrap();
    let mut red = crate::ladder::Report::new("released-shim regression");
    super::super::run(&context, &mut red);
    if rig.label == "aterm 0.197.0" {
        assert!(
            red.outcomes().any(|(o, label)| o
                == crate::ladder::Outcome::Fail(crate::ladder::Severity::GateFailed)
                && label.contains(".cargo/config.toml")
                && label.contains("explicit build target is unmanaged or unsupported")),
            "{}",
            red.render()
        );
    }
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\njobs = 2\n",
    ))
    .unwrap();
    let mut green = crate::ladder::Report::new("released-shim negative control");
    super::super::run(&context, &mut green);
    assert!(
        green
            .outcomes()
            .any(|(o, label)| o == crate::ladder::Outcome::Ok
                && label.contains("built aterm-time")),
        "{}",
        green.render()
    );

    // A source-only edit must cost no snapshot, no release lookup and no build.
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\njobs = 1\n",
    ))
    .unwrap();
    io(fs::write(
        rig.tree.join("src/lib.rs"),
        "pub fn changed() {}\n",
    ))
    .unwrap();
    let mut untouched = crate::ladder::Report::new("no bootstrap change");
    super::super::run(&context, &mut untouched);
    assert!(
        untouched
            .outcomes()
            .any(|(o, label)| o == crate::ladder::Outcome::Ok && label.contains("no child build")),
        "{}",
        untouched.render()
    );
}

#[test]
fn sandbox_only_allows_scratch_and_resource_bookkeeping_writes() {
    let env = EnvSnapshot::capture();
    let rig = installed_rig(&env);
    let outside = aterm_tempfile::tempdir().unwrap();
    for (path, accepted) in [
        (outside.path().join("protected"), false),
        (rig.home.join("allowed"), true),
    ] {
        let mut cmd = rig.sandbox().unwrap();
        cmd.arg("/usr/bin/touch").arg(&path);
        let out = io(aterm_uds::deadline::output_within(
            &mut cmd,
            Duration::from_secs(60),
        ))
        .unwrap();
        assert_eq!(
            out.status.success(),
            accepted,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(path.exists(), accepted);
    }
}

#[test]
fn a_pooled_sandbox_opens_only_its_held_fifo_and_cannot_write_the_adjacent_file() {
    let env = EnvSnapshot::capture();
    let rig = installed_rig(&env);
    let outside = aterm_tempfile::tempdir().unwrap();
    let pool = aterm_capacity::Jobserver::new(&outside.path().join("jobs"), 2).unwrap();
    let adjacent = outside.path().join("protected");
    fs::write(&adjacent, b"unchanged").unwrap();
    crate::compile_width::run(Some(&pool), Some(2), None, &|| false, || {
        let mut fifo = rig.sandbox().unwrap();
        fifo.args(["/bin/sh", "-c", "exec 3<> \"$1\"", "fifo"])
            .arg(pool.path());
        let opened =
            aterm_uds::deadline::output_within(&mut fifo, Duration::from_secs(60)).unwrap();
        assert!(
            opened.status.success(),
            "{}",
            String::from_utf8_lossy(&opened.stderr)
        );
        let mut ordinary = rig.sandbox().unwrap();
        ordinary
            .args(["/bin/sh", "-c", "printf changed > \"$1\"", "ordinary"])
            .arg(&adjacent);
        let denied =
            aterm_uds::deadline::output_within(&mut ordinary, Duration::from_secs(60)).unwrap();
        assert!(!denied.status.success());
        assert_eq!(fs::read(&adjacent).unwrap(), b"unchanged");
    })
    .unwrap();
    let available: Vec<_> = std::iter::from_fn(|| pool.try_acquire().unwrap()).collect();
    assert_eq!(available.len(), 2);
}

#[test]
fn a_real_candidate_first_build_inherits_the_charged_pool_inside_its_sandbox() {
    let helper = aterm_tempfile::bin_exe("atpkg");
    let outside = aterm_tempfile::tempdir().unwrap();
    let pool = aterm_capacity::Jobserver::new(&outside.path().join("jobs"), 2).unwrap();
    crate::compile_width::run(Some(&pool), Some(2), None, &|| false, || {
        for (outcome, label) in candidate_rows(&helper) {
            assert_eq!(outcome, crate::ladder::Outcome::Ok, "{label}");
        }
    })
    .unwrap();
    let available: Vec<_> = std::iter::from_fn(|| pool.try_acquire().unwrap()).collect();
    assert_eq!(available.len(), 2);
}

/// The public tags [`public_tags`] reads: `held` the candidate holds, `beside` fetched outside its
/// ancestry, oldest first.
fn tags(held: &[&str], beside: &[&str]) -> Vec<PublicTag> {
    let mut all: Vec<_> = held
        .iter()
        .map(|version| (version, true))
        .chain(beside.iter().map(|version| (version, false)))
        .map(|(version, held)| PublicTag {
            version: (*version).to_owned(),
            held,
        })
        .collect();
    all.sort_by_key(|tag| release_version(&tag.version));
    all
}

/// The installed app judges when it is the newest public tag the candidate holds at or before
/// its workspace version or the one before it, and the row says which: after a release is
/// published, a Mac that fetched the tag and has not yet updated can still land a
/// bootstrap-input change. A tag past the workspace version is not yet the candidate's release,
/// and the row names the newer tag as the newest at or before that version, never as the newest
/// the candidate holds.
#[test]
fn the_newest_release_and_the_one_before_it_judge() {
    let fetched = tags(&["0.197.0", "0.198.0", "0.199.0"], &[]);
    let newest = judging_release("aterm 0.199.0", "0.199.0", &fetched).unwrap();
    assert_eq!(
        newest,
        Judging {
            version: "0.199.0".into(),
            newer: None
        }
    );
    assert_eq!(newest.who(), "aterm 0.199.0");
    let previous = judging_release("aterm 0.198.0", "0.199.0", &fetched).unwrap();
    assert_eq!(
        previous,
        Judging {
            version: "0.198.0".into(),
            newer: Some("0.199.0".into())
        }
    );
    let who = previous.who();
    assert!(
        who.starts_with("aterm 0.198.0 (") && who.contains("v0.199.0"),
        "{who}"
    );
    let past = tags(&["0.197.0", "0.198.0", "0.199.0", "0.200.0"], &[]);
    let beside_past = judging_release("aterm 0.198.0", "0.199.0", &past).unwrap();
    assert_eq!(beside_past, previous);
    let who_past = beside_past.who();
    assert!(
        who_past.contains(
            "v0.199.0, the newest public tag the candidate holds at or before its workspace \
             version;"
        ),
        "{who_past}"
    );
    // One held tag has no release before it.
    assert_eq!(
        judging_release("aterm 0.199.0", "0.199.0", &tags(&["0.199.0"], &[])).unwrap(),
        newest
    );
    assert_eq!(
        judging_release(
            "aterm 0.199.0",
            "0.199.0",
            &tags(&["0.198.0", "0.199.0", "0.200.0"], &["0.201.0"])
        )
        .unwrap(),
        newest
    );
}

/// Controls: a release two or more behind the candidate's newest tag and a development build
/// decide nothing, each with its remedy; so does a release older than two of the candidate's
/// tags whose own tag this clone never fetched, which no fetch would make judge.
#[test]
fn an_older_release_and_a_development_build_do_not_judge() {
    let fetched = tags(&["0.196.0", "0.197.0", "0.198.0", "0.199.0"], &[]);
    let two_behind = judging_release("aterm 0.197.0", "0.199.0", &fetched).unwrap_err();
    assert!(
        two_behind.contains("2 releases behind")
            && two_behind.contains("v0.199.0")
            && two_behind.contains("update the installed app"),
        "{two_behind}"
    );
    // The candidate holds v0.200.0 too, past its workspace version: the refusal names v0.199.0
    // as the newest at or before that version, not as the newest it holds.
    let past = tags(
        &["0.196.0", "0.197.0", "0.198.0", "0.199.0", "0.200.0"],
        &[],
    );
    let two_behind_past = judging_release("aterm 0.197.0", "0.199.0", &past).unwrap_err();
    assert!(
        two_behind_past.contains(
            "2 releases behind v0.199.0, the newest public tag the candidate holds at or before \
             its workspace version 0.199.0;"
        ),
        "{two_behind_past}"
    );
    let three_behind = judging_release("aterm 0.196.0", "0.199.0", &fetched).unwrap_err();
    assert!(three_behind.contains("3 releases behind"), "{three_behind}");
    // A release whose tag sits off the candidate's ancestry is still a release the installed
    // app is behind: v0.197.0 here counts though the candidate does not hold it.
    let off_line = tags(&["0.196.0", "0.198.0", "0.199.0"], &["0.197.0"]);
    let behind_off_line = judging_release("aterm 0.196.0", "0.199.0", &off_line).unwrap_err();
    assert!(
        behind_off_line.contains("3 releases behind v0.199.0"),
        "{behind_off_line}"
    );
    let ancient = judging_release("aterm 0.150.0", "0.199.0", &fetched).unwrap_err();
    assert!(
        ancient.contains("4 releases behind") && !ancient.contains("git fetch"),
        "{ancient}"
    );
    for development in [
        "aterm 0.199.0+g1a2b3c4d",
        "aterm 0.200.0+g1a2b3c4d",
        "aterm 0.199.1",
        "aterm",
        "",
    ] {
        let why = judging_release(development, "0.199.0", &fetched).unwrap_err();
        assert!(why.contains("development or unreadable"), "{why}");
    }
    let unversioned = judging_release("aterm 0.199.0", "0.199.0-dev", &fetched).unwrap_err();
    assert!(
        unversioned.contains("is not MAJOR.MINOR.0"),
        "{unversioned}"
    );
}

/// An installed release the candidate's tags do not place is told where its own tag stands,
/// and only the act that clears that: a tag this clone never fetched is fetched; a tag fetched
/// beside the candidate's ancestry (the app updated itself to a release published after the
/// branch forked) is brought in by merging main where main holds it, which no fetch does, or
/// else the newest release the candidate holds is installed; a tag the candidate holds past its
/// workspace version means that version went back, and that refusal names no other tag as the
/// newest the candidate holds. With one or no held tag the same holds of the release before it.
/// NEGATIVE CONTROL: each cause's remedy is absent from the others' refusals, and a candidate
/// that holds no release at or before its version is offered none to install.
#[test]
fn an_app_the_candidates_tags_do_not_place_is_told_where_its_tag_stands() {
    let held = ["0.198.0", "0.199.0"];
    let fetch = "git fetch origin --tags";
    let merge = "merge origin/main into the candidate";
    let version = "take origin/main's workspace version";
    let unfetched = judging_release("aterm 0.200.0", "0.200.0", &tags(&held, &[])).unwrap_err();
    assert!(
        unfetched.contains("tag v0.200.0 is not fetched")
            && unfetched.contains(
                "the newest public tag the candidate holds at or before its workspace version \
                 0.200.0 is v0.199.0"
            )
            && unfetched.contains(fetch)
            && !unfetched.contains(merge)
            && !unfetched.contains(version),
        "{unfetched}"
    );
    let beside =
        judging_release("aterm 0.200.0", "0.199.0", &tags(&held, &["0.200.0"])).unwrap_err();
    assert!(
        beside.contains("tag v0.200.0 is fetched but not in the candidate's ancestry")
            && beside.contains(merge)
            && beside.contains("where origin/main holds v0.200.0")
            && beside.contains("or else install aterm 0.199.0")
            && !beside.contains(fetch)
            && !beside.contains(version),
        "{beside}"
    );
    let ahead = judging_release(
        "aterm 0.200.0",
        "0.199.0",
        &tags(&["0.198.0", "0.199.0", "0.200.0"], &[]),
    )
    .unwrap_err();
    assert!(
        ahead.contains("the candidate's workspace version 0.199.0 is behind it")
            && !ahead.contains("v0.199.0")
            && ahead.contains(version)
            && !ahead.contains(fetch)
            && !ahead.contains(merge),
        "{ahead}"
    );
    let lone = judging_release("aterm 0.198.0", "0.199.0", &tags(&["0.199.0"], &[])).unwrap_err();
    assert!(lone.contains(fetch) && !lone.contains(merge), "{lone}");
    let lone_beside = judging_release(
        "aterm 0.198.0",
        "0.199.0",
        &tags(&["0.199.0"], &["0.198.0"]),
    )
    .unwrap_err();
    assert!(
        lone_beside.contains(merge)
            && lone_beside.contains("or else install aterm 0.199.0")
            && !lone_beside.contains(fetch),
        "{lone_beside}"
    );
    let none_beside =
        judging_release("aterm 0.199.0", "0.199.0", &tags(&[], &["0.199.0"])).unwrap_err();
    assert!(
        none_beside.contains(merge) && !none_beside.contains("install aterm"),
        "{none_beside}"
    );
    let none = judging_release("aterm 0.199.0", "0.199.0", &[]).unwrap_err();
    assert!(
        none.contains(
            "the candidate holds no public tag at or before its workspace version 0.199.0"
        ) && none.contains(fetch),
        "{none}"
    );
}

/// Every public tag this clone fetched is read, in version order and not name order, annotated
/// or lightweight, each with whether the candidate holds it; a tag that is no `vMAJOR.MINOR.0`
/// is no release. The releases a judgment and the land seal's `release-tags` are read by are the
/// held ones at or before the workspace version.
/// NEGATIVE CONTROL: a tag on a commit beside HEAD's ancestry is read, and not held, so an app
/// updated to it is told to merge main and not to fetch the tag it already has.
#[test]
fn every_fetched_public_tag_says_whether_the_candidate_holds_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let mut cmd = crate::git_provider::command();
        cmd.current_dir(dir.path())
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
            ])
            .args(args);
        for name in crate::changed::GIT_REDIRECTS {
            cmd.env_remove(name);
        }
        let out =
            aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "0.99.0"]);
    git(&["tag", "-a", "-m", "v0.99.0", "v0.99.0"]);
    git(&["commit", "-q", "--allow-empty", "-m", "0.100.0"]);
    for tag in ["v0.100.0", "v0.100.1", "v0.100", "vnext"] {
        git(&["tag", tag]);
    }
    git(&["checkout", "-q", "-b", "side"]);
    git(&["commit", "-q", "--allow-empty", "-m", "0.101.0"]);
    git(&["tag", "-a", "-m", "v0.101.0", "v0.101.0"]);
    git(&["checkout", "-q", "-"]);
    let env = EnvSnapshot::capture();
    let tags = public_tags(dir.path(), &env, "HEAD").unwrap();
    assert_eq!(
        tags,
        [
            PublicTag {
                version: "0.99.0".into(),
                held: true,
            },
            PublicTag {
                version: "0.100.0".into(),
                held: true,
            },
            PublicTag {
                version: "0.101.0".into(),
                held: false,
            },
        ]
    );
    assert_eq!(
        public_releases(dir.path(), &env, "HEAD", "0.99.0").unwrap(),
        ["0.99.0"]
    );
    assert_eq!(
        public_releases(dir.path(), &env, "HEAD", "0.101.0").unwrap(),
        ["0.99.0", "0.100.0"]
    );
    let beside = judging_release("aterm 0.101.0", "0.100.0", &tags).unwrap_err();
    assert!(
        beside.contains("fetched but not in the candidate's ancestry")
            && beside.contains("merge origin/main")
            && !beside.contains("git fetch"),
        "{beside}"
    );
}

/// Through the production stage, with the installed pair: a checkout whose newest public tag is
/// one release past the installed app is judged by that app, and its row names both releases.
/// Before this rule it was COULD NOT RUN, as a checkout two releases past still is.
#[test]
fn the_stage_judges_with_the_release_before_the_newest_tag_and_not_an_older_one() {
    let (_, installed) = installed_release(&EnvSnapshot::capture());
    let version = public_version(&installed).unwrap();
    let mut parts = version.split('.').map(|part| part.parse::<u64>().unwrap());
    let (major, minor) = (parts.next().unwrap(), parts.next().unwrap());
    let later = |by: u64| format!("{major}.{}.0", minor + by);
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = aterm_tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let mut cmd = crate::git_provider::command();
        cmd.current_dir(dir.path())
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args);
        for name in crate::changed::GIT_REDIRECTS {
            cmd.env_remove(name);
        }
        let out =
            aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let release = |workspace: &str| {
        pin_fixture_toolchain(dir.path());
        fs::write(
            dir.path().join("Cargo.toml"),
            format!(
                "[package]\nname='aterm-time'\nversion='0.0.0'\nedition='2024'\n[workspace]\n\
                 [workspace.package]\nversion='{workspace}'\n"
            ),
        )
        .unwrap();
        git(&[
            "add",
            "--",
            "Cargo.toml",
            "Cargo.lock",
            "trust.lock",
            "src/lib.rs",
        ]);
        git(&["commit", "-qm", workspace]);
        git(&["tag", &format!("v{workspace}")]);
    };
    let judged = || {
        let context = crate::Ctx::new(
            dir.path().to_path_buf(),
            crate::Scope::workspace(),
            EnvSnapshot::capture(),
            state.path().to_path_buf(),
        );
        let mut report = crate::ladder::Report::new("released-shim");
        super::super::run(&context, &mut report);
        let rows: Vec<_> = report
            .outcomes()
            .map(|(outcome, label)| (outcome, label.to_owned()))
            .collect();
        assert_eq!(rows.len(), 1, "{}", report.render());
        rows.into_iter().next().unwrap()
    };
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"aterm-time\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn bootstrap_probe() {}\n",
    )
    .unwrap();
    git(&["init", "-q"]);
    release(version);
    release(&later(1));
    let (outcome, label) = judged();
    assert_eq!(outcome, crate::ladder::Outcome::Ok, "{label}");
    assert!(
        label.contains(&format!("{installed} (the release before v{}", later(1)))
            && label.contains("built aterm-time"),
        "{label}"
    );
    release(&later(2));
    let (outcome, label) = judged();
    assert_eq!(
        outcome,
        crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun),
        "{label}"
    );
    assert!(
        label.contains("2 releases behind") && label.contains(&format!("v{}", later(2))),
        "{label}"
    );
}

const BUILD_WAIT_FIXTURE: &str = "ATERM_RELEASED_SHIM_WAIT_FIXTURE";

#[test]
fn sandboxed_build_wait_fixture() {
    let Some(home) = std::env::var_os(BUILD_WAIT_FIXTURE).map(PathBuf::from) else {
        return;
    };
    let waits = aterm_toolchain::waits::dir().expect("runner supplies its wait directory");
    assert!(
        waits.starts_with(&home),
        "waits must stay in writable scratch"
    );
    let mark = aterm_toolchain::waits::Mark::begin("bootstrap fixture waiting for capacity")
        .expect("sandbox permits the runner's wait mark");
    assert!(
        aterm_toolchain::waits::waiting(&waits)
            .iter()
            .any(|wait| wait.pid == std::process::id()),
        "the parent can observe this locked wait mark"
    );
    eprintln!("bootstrap fixture entered");
    fs::write(home.join("ready"), "ready").unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "bootstrap fixture release",
        || home.join("release").is_file().then_some(()),
    );
    drop(mark);
    eprintln!("bootstrap fixture admitted");
    if home.join("hold-work").is_file() {
        fs::write(home.join("working"), "working").unwrap();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "inactivity detector to end fixture",
            || home.join("finish-work").is_file().then_some(()),
        );
    }
    println!("bootstrap fixture stdout");
}

/// Advances only after the actual child proves a sandboxed, locked wait. The first usage
/// read may precede the wait observation, so a one-second step forces a later regular look
/// before the long jump. The third usage read releases the child after 69 virtual seconds.
#[derive(Debug)]
struct BuildClock {
    home: PathBuf,
    ready_at: std::sync::OnceLock<std::time::Instant>,
    phase: std::sync::atomic::AtomicU8,
    progressing: bool,
}

impl exec::Host for BuildClock {
    fn monotonic(&self) -> std::time::Instant {
        use std::sync::atomic::Ordering;
        if !self.home.join("ready").is_file() {
            return std::time::Instant::now();
        }
        let ready_at = *self.ready_at.get_or_init(std::time::Instant::now);
        let seconds = match self.phase.load(Ordering::SeqCst) {
            0 => 1,
            1 => 2,
            2 => 71,
            3 => 71 + u64::from(self.home.join("working").is_file()),
            phase => 141 + u64::from(phase.saturating_sub(4)) * 70,
        };
        ready_at + Duration::from_secs(seconds)
    }

    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        use std::sync::atomic::Ordering;
        match self.phase.load(Ordering::SeqCst) {
            0 if self.home.join("ready").is_file() => self.phase.store(1, Ordering::SeqCst),
            1 => self.phase.store(2, Ordering::SeqCst),
            2 => {
                fs::write(self.home.join("release"), "release").unwrap();
                self.phase.store(3, Ordering::SeqCst);
            }
            3 if self.home.join("working").is_file() => self.phase.store(4, Ordering::SeqCst),
            phase if phase >= 4 => {
                self.phase.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
        Some(vec![aterm_sysprobe::MemberUsage {
            pid: pgid,
            start: 1,
            cpu_ns: 1,
            runnable_ns: 1,
            ..Default::default()
        }])
    }

    fn idle(&self) -> Option<Duration> {
        None
    }

    fn process_progress(
        &self,
        _root: u32,
        _probe: &mut aterm_uds::deadline::ProcessProgressProbe,
    ) -> Option<u64> {
        let phase = self.phase.load(std::sync::atomic::Ordering::SeqCst);
        if self.progressing && phase >= 7 {
            fs::write(self.home.join("finish-work"), "finished").unwrap();
        }
        Some(if self.progressing {
            u64::from(phase)
        } else {
            0
        })
    }

    fn cores(&self) -> u32 {
        1
    }
}

#[test]
fn sandboxed_bootstrap_excludes_admission_wait_and_still_detects_a_hang() {
    for work in [None, Some(false), Some(true)] {
        let env = EnvSnapshot::capture();
        let rig = installed_rig(&env);
        if work.is_some() {
            fs::write(rig.home.join("hold-work"), "hold").unwrap();
        }
        let context = crate::Ctx::new(
            rig.tree.clone(),
            crate::Scope::workspace(),
            env,
            rig.home.join("run"),
        );
        let clock = BuildClock {
            home: rig.home.clone(),
            ready_at: std::sync::OnceLock::new(),
            phase: std::sync::atomic::AtomicU8::new(0),
            progressing: work == Some(true),
        };
        let mut command = rig.sandbox().unwrap();
        command
            .env(BUILD_WAIT_FIXTURE, &rig.home)
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "released_shim::mac::tests::sandboxed_build_wait_fixture",
                "--nocapture",
                "--test-threads=1",
            ]);
        let output = rig.run_build(
            &command,
            exec::ExecEnv {
                host: &clock,
                ..context.exec_env()
            },
        );
        assert!(rig.home.join("release").is_file(), "child was admitted");
        assert!(
            output.run.output.contains("bootstrap fixture admitted"),
            "{:?}",
            output.run
        );
        assert!(output.run.usage.unwrap().waited >= Duration::from_secs(60));
        if work == Some(false) {
            assert!(
                !output.run.ok && output.run.code.is_none(),
                "{:?}",
                output.run
            );
            assert!(
                output
                    .run
                    .output
                    .contains("no captured output or complete descendant CPU/I/O progress"),
                "{:?}",
                output.run
            );
        } else {
            assert!(output.run.ok, "{:?}", output.run);
            assert!(output.stdout.unwrap().contains("bootstrap fixture stdout"));
        }
    }
}

/// A release asks this judgment of its own tree at step 1, before it qualifies anything: it is
/// the stage's own function, so the cut's refusal and the check's row are the same words.
/// NEGATIVE CONTROL: the same checkout with a library the release cannot compile is refused,
/// in a row that names the judging release, its exit and the compiler's own line.
#[test]
fn a_cut_asks_the_stages_own_judgment_and_reads_its_row() {
    use super::super::{Verdict, WHOLE_TREE, judge};
    let env = EnvSnapshot::capture();
    let (_, installed) = installed_release(&env);
    let version = public_version(&installed).unwrap().to_owned();
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = aterm_tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let mut cmd = crate::git_provider::command();
        cmd.current_dir(dir.path())
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args);
        for name in crate::changed::GIT_REDIRECTS {
            cmd.env_remove(name);
        }
        let out =
            aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        format!(
            "[package]\nname='aterm-time'\nversion='0.0.0'\nedition='2024'\n[workspace]\n\
             [workspace.package]\nversion='{version}'\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.path().join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"aterm-time\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn bootstrap_probe() {}\n",
    )
    .unwrap();
    git(&["init", "-q"]);
    pin_fixture_toolchain(dir.path());
    git(&[
        "add",
        "--",
        "Cargo.toml",
        "Cargo.lock",
        "trust.lock",
        "src/lib.rs",
    ]);
    git(&["commit", "-qm", "candidate"]);
    git(&["tag", &format!("v{version}")]);
    let whole = [WHOLE_TREE.to_owned()];

    let accepted = judge(dir.path(), &env, &whole);
    assert_eq!(accepted.verdict, Verdict::Accepted, "{}", accepted.row);
    assert_eq!(
        accepted.row,
        format!("released-shim: {installed} built aterm-time; inputs: {WHOLE_TREE}")
    );
    let context = crate::Ctx::new(
        dir.path().to_path_buf(),
        crate::Scope::workspace(),
        EnvSnapshot::capture(),
        state.path().to_path_buf(),
    );
    let mut report = crate::ladder::Report::new("released-shim");
    super::super::run(&context, &mut report);
    let rows: Vec<_> = report.outcomes().collect();
    assert_eq!(
        rows,
        [(crate::ladder::Outcome::Ok, accepted.row.as_str())],
        "{}",
        report.render()
    );

    // The tracked file as the checkout holds it is what the release builds.
    fs::write(dir.path().join("src/lib.rs"), "pub fn broken( {\n").unwrap();
    let refused = judge(dir.path(), &env, &whole);
    assert_eq!(refused.verdict, Verdict::Refused, "{}", refused.row);
    assert!(
        refused
            .row
            .starts_with(&format!("released-shim: {installed} refuses candidate ("))
            && refused.row.contains(WHOLE_TREE)
            && refused.row.contains("error:"),
        "{}",
        refused.row
    );
    // A refused judgment keeps its rig for whoever reads the red; this one is the test's own.
    let retained = refused
        .output
        .lines()
        .find_map(|line| line.strip_prefix("released-shim failure inputs retained at "))
        .expect("a refusal names the rig it kept");
    fs::remove_dir_all(retained).unwrap();
}

/// A tracked one-package checkout named as the rig's build names it.
fn tracked_package() -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().unwrap();
    pin_fixture_toolchain(dir.path());
    fs::create_dir_all(dir.path().join("src")).unwrap();
    for (path, text) in [
        (
            "Cargo.toml",
            "[package]\nname='aterm-time'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
        ),
        (
            "Cargo.lock",
            "version = 4\n[[package]]\nname = \"aterm-time\"\nversion = \"0.0.0\"\n",
        ),
        ("src/lib.rs", "pub fn bootstrap_probe() {}\n"),
    ] {
        fs::write(dir.path().join(path), text).unwrap();
    }
    for args in [
        &["init", "--quiet"][..],
        &[
            "add",
            "--",
            "Cargo.toml",
            "Cargo.lock",
            "trust.lock",
            "src/lib.rs",
        ],
    ] {
        let mut cmd = crate::git_provider::command();
        cmd.current_dir(dir.path()).args(args);
        for name in crate::changed::GIT_REDIRECTS {
            cmd.env_remove(name);
        }
        let out =
            aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    dir
}

/// A stand-in for the helper's front (`<helper> __build_admit --shim <tool> <placed> <program>
/// <words>`): it runs the fixture's pinned compiler, except where `refused` (a `case` pattern
/// over the call's words) matches. This stand-in controls admission, as aterm 0.199.0's helper
/// refused the no-cache fallback; it does not implement the real helper's lock resolution.
fn helper(dir: &Path, refused: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let env = EnvSnapshot::capture();
    // The stand-in runs the store's pinned compiler; a machine with no store ends the test here,
    // COULD NOT RUN, before any rig reads it.
    let store = store(&env);
    let pin = atpkg::trust_lock::find(&aterm_tempfile::manifest_dir())
        .unwrap()
        .lock
        .unwrap();
    let program = store
        .prefix
        .join(format!("store/trust/{}/bin/targo", pin.build));
    let program = crate::findings::shell_word(program.to_str().unwrap());
    let path = dir.join("helper");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprogram={program}\nshift 5\ncase \"$*\" in\n  {refused})\n    \
             printf '%s\\n' 'atpkg: build cache ownership refused: the selected quiet build \
             target could not be resolved safely; no build started' >&2\n    exit 75 ;;\nesac\n\
             exec \"$program\" \"$@\"\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn candidate_rows(helper: &Path) -> Vec<(crate::ladder::Outcome, String)> {
    let env = EnvSnapshot::capture();
    let tree = tracked_package();
    let state = aterm_tempfile::tempdir().unwrap();
    let context = crate::Ctx::new(
        tree.path().to_path_buf(),
        crate::Scope::workspace(),
        env.clone(),
        state.path().to_path_buf(),
    );
    let mut report = crate::ladder::Report::new("candidate-shim");
    candidate(tree.path(), &env, helper, context.exec_env(), &mut report);
    let rows: Vec<_> = report
        .outcomes()
        .map(|(outcome, label)| (outcome, label.to_owned()))
        .collect();
    assert_eq!(rows.len(), Placement::ALL.len(), "{}", report.render());
    for ((_, label), placement) in rows.iter().zip(Placement::ALL) {
        assert!(label.contains(placement.name()), "{}", report.render());
    }
    // A refused judgment keeps its rig for whoever reads the red; these are the test's own.
    let kept: Vec<_> = report
        .render()
        .lines()
        .filter_map(|line| line.strip_prefix("candidate-shim failure inputs retained at "))
        .map(PathBuf::from)
        .collect();
    let red = rows
        .iter()
        .filter(|(outcome, _)| *outcome != crate::ladder::Outcome::Ok)
        .count();
    assert_eq!(kept.len(), red, "{}", report.render());
    for rig in kept {
        // The fallback's home holds a folder nothing may write; its own removal needs no write.
        fs::remove_dir_all(rig).unwrap();
    }
    rows
}

/// The shim a candidate rig runs is this tree's text, naming the helper it was handed and the
/// scratch store, and a helper that admits every build is green under every placement.
#[test]
fn a_candidate_pair_that_admits_every_placement_is_green_under_each() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let helper = helper(dir.path(), "'no call is refused'");
    let env = EnvSnapshot::capture();
    let store = store(&env);
    let rig = Rig::new(
        &env,
        &store,
        Pair::Candidate { helper: &helper },
        "fixture".into(),
        Placement::NewHome,
    )
    .unwrap();
    assert!(!rig.home.join("Library").exists());
    let text = fs::read_to_string(&rig.shim).unwrap();
    assert_eq!(
        atpkg::build_cache::registered::shim_helper(&text).as_deref(),
        Some(helper.as_path())
    );
    assert!(text.contains(rig.home.join("pkg/store/trust").to_str().unwrap()));
    assert!(!text.contains(store.prefix.to_str().unwrap()), "{text}");
    for (outcome, label) in candidate_rows(&helper) {
        assert_eq!(outcome, crate::ladder::Outcome::Ok, "{label}");
    }
}

/// The 0.199.0 defect, as the rig sees it: a helper that refuses the shim's in-tree fallback
/// and admits the rest is red under the home where this tree's shim selects that fallback,
/// named, and green under the other three. The installed-release judgment ran one placement
/// and could not see it until that helper shipped.
#[test]
fn a_candidate_helper_that_refuses_the_no_cache_fallback_is_red_under_it_alone() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let helper = helper(
        dir.path(),
        "*\"build.build-dir='{workspace-root}/target/.ubd/build.noindex'\"*",
    );
    let rows = candidate_rows(&helper);
    for ((outcome, label), placement) in rows.iter().zip(Placement::ALL) {
        if placement == Placement::NoCache {
            assert_eq!(
                *outcome,
                crate::ladder::Outcome::Fail(crate::ladder::Severity::GateFailed),
                "{label}"
            );
            assert!(
                label.contains("refuse a first build under the no-cache fallback")
                    && label.contains("build cache ownership refused"),
                "{label}"
            );
        } else {
            assert_eq!(*outcome, crate::ladder::Outcome::Ok, "{label}");
        }
    }
}
