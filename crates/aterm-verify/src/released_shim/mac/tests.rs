// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The installed release's pair in an account's home: `released-shim`'s rig.
fn installed_rig(env: &EnvSnapshot) -> Result<Rig, String> {
    let store = Store::read(env)?;
    let label = store.release()?;
    Rig::new(env, &store, Pair::Installed, label, Placement::SharedCache)
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
    let rig = installed_rig(&env).expect("installed released shim and helper");
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
        let rig = installed_rig(&env).unwrap();
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
    let store = Store::read(&env).expect("this machine's package store");
    let installed = store.release().expect("installed public release");
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
    git(&["add", "--", "Cargo.toml", "Cargo.lock", "src/lib.rs"]);
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
        &["add", "--", "Cargo.toml", "Cargo.lock", "src/lib.rs"],
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
/// <words>`): it becomes the program, as a front that admits the build does, except where
/// `refused` (a `case` pattern over the call's words) matches, which it refuses as aterm
/// 0.199.0's helper refused the no-cache fallback.
fn helper(dir: &Path, refused: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("helper");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprogram=$5\nshift 5\ncase \"$*\" in\n  {refused})\n    \
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
    let store = Store::read(&env).expect("this machine's package store");
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
