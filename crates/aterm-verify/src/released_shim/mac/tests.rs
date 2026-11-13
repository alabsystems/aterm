// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The installed release's pair in an account's home: `released-shim`'s rig.
fn installed_rig(env: &EnvSnapshot) -> Result<Rig, String> {
    let store = Store::read(env)?;
    let label = store.release()?;
    Rig::new(env, &store, label)
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
    let rig = installed_rig(&env).expect("installed released shim and helper");
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
    let first = rig.build().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(built_artifact(&first.stdout, &rig.home));
}

#[test]
fn installed_release_rejects_config_target_and_accepts_jobs() {
    let env = EnvSnapshot::capture();
    let rig = installed_rig(&env).expect("installed released shim and helper");
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
    let bad = rig.build().unwrap();
    let said = String::from_utf8_lossy(&bad.stderr);
    if rig.label == "aterm 0.197.0" {
        assert!(!bad.status.success(), "0.197.0 must reject ff1626d7ab8");
    }
    // Later public releases may support the layout; the 0.197.0 pair must refuse it.
    if !bad.status.success() {
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
    let good = rig.build().unwrap();
    assert!(
        good.status.success(),
        "{}",
        String::from_utf8_lossy(&good.stderr)
    );
    assert!(rig.tree.join("target/debug/libaterm_time.rlib").is_file());
    assert!(built_artifact(&good.stdout, &rig.home));

    // The same production stage must name the changed input, rather than merely fail
    // an unrelated source build. Commit the accepted config, then reproduce ff1626d7ab8.
    for args in [
        vec!["init", "-q"],
        vec![
            "add",
            "Cargo.toml",
            "Cargo.lock",
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
    let rig = installed_rig(&env).unwrap();
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

fn tags(versions: &[&str]) -> Vec<String> {
    versions.iter().map(|v| (*v).to_owned()).collect()
}

/// The installed app judges when it is the newest fetched public tag or the one before it, and
/// the row says which: after a release is published, a Mac that fetched the tag and has not yet
/// updated can still land a bootstrap-input change.
#[test]
fn the_newest_release_and_the_one_before_it_judge() {
    let fetched = tags(&["0.197.0", "0.198.0", "0.199.0"]);
    let newest = judging_release("aterm 0.199.0", &fetched).unwrap();
    assert_eq!(
        newest,
        Judging {
            version: "0.199.0".into(),
            newer: None
        }
    );
    assert_eq!(newest.who(), "aterm 0.199.0");
    let previous = judging_release("aterm 0.198.0", &fetched).unwrap();
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
    // One fetched tag has no release before it.
    assert_eq!(
        judging_release("aterm 0.199.0", &tags(&["0.199.0"])).unwrap(),
        newest
    );
}

/// Controls: a release two behind, a development build, and an app no fetched tag names decide
/// nothing, each with its remedy.
#[test]
fn an_older_release_a_development_build_and_an_unfetched_one_do_not_judge() {
    let fetched = tags(&["0.196.0", "0.197.0", "0.198.0", "0.199.0"]);
    let two_behind = judging_release("aterm 0.197.0", &fetched).unwrap_err();
    assert!(
        two_behind.contains("2 releases behind")
            && two_behind.contains("v0.199.0")
            && two_behind.contains("update the installed app"),
        "{two_behind}"
    );
    let three_behind = judging_release("aterm 0.196.0", &fetched).unwrap_err();
    assert!(three_behind.contains("3 releases behind"), "{three_behind}");
    for development in [
        "aterm 0.199.0+g1a2b3c4d",
        "aterm 0.200.0+g1a2b3c4d",
        "aterm",
        "",
    ] {
        let why = judging_release(development, &fetched).unwrap_err();
        assert!(why.contains("development or unreadable"), "{why}");
    }
    // The app is ahead of what this checkout has fetched: the tags are stale, not the app.
    let unfetched = judging_release("aterm 0.200.0", &fetched).unwrap_err();
    assert!(
        unfetched.contains("git fetch origin --tags") && unfetched.contains("v0.199.0"),
        "{unfetched}"
    );
    // With one fetched tag, the release before it is not known to be the one before it.
    let lone = judging_release("aterm 0.198.0", &tags(&["0.199.0"])).unwrap_err();
    assert!(lone.contains("git fetch origin --tags"), "{lone}");
    let none = judging_release("aterm 0.199.0", &[]).unwrap_err();
    assert!(none.contains("no fetched public"), "{none}");
}

/// Through the production stage, with the installed pair: a checkout whose newest public tag is
/// one release past the installed app is judged by that app, and its row names both releases.
/// Before this rule it was COULD NOT RUN, as a checkout two releases past still is.
#[test]
fn the_stage_judges_with_the_release_before_the_newest_tag_and_not_an_older_one() {
    let store = Store::read(&EnvSnapshot::capture()).expect("this machine's package store");
    let installed = store.release().expect("installed public release");
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
        fs::write(
            dir.path().join("Cargo.toml"),
            format!(
                "[package]\nname='aterm-time'\nversion='0.0.0'\nedition='2024'\n[workspace]\n\
                 [workspace.package]\nversion='{workspace}'\n"
            ),
        )
        .unwrap();
        git(&["add", "--", "Cargo.toml", "Cargo.lock", "src/lib.rs"]);
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
