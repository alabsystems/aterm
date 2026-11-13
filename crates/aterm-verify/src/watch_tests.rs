// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn bootstrap_stop_cause_keeps_named_environment_before_build_output() {
    use std::os::unix::process::ExitStatusExt as _;
    let output = std::iter::once(crate::exec::ENOSPC_SENTENCE.to_owned())
        .chain((0..80).map(|n| format!("compiler note {n}")))
        .collect::<Vec<_>>()
        .join("\n");
    let ran = crate::exec::Run {
        case_observations: None,
        ok: false,
        output,
        code: Some(75),
        spawn_error: None,
        usage: None,
        stopped_at: None,
        hung: None,
    };
    let mut record: Record = aterm_toml::from_str("commit='c'\ntree='t'\nhost='h'\nverifier=''\ninputs=''\ntoken='k'\nstate='judging'\nstarted=0\nupdated=1\nlease_until=0\nrows=[]").unwrap();
    cancel_record(
        &mut record,
        std::process::ExitStatus::from_raw(75 << 8),
        &stopped_run("bootstrap", &ran, false),
    );
    assert!(
        record.cause.contains("watch bootstrap stopped:"),
        "{}",
        record.cause
    );
    assert!(
        record.cause.contains("kind=environment"),
        "{}",
        record.cause
    );
    assert!(
        record.cause.contains(crate::exec::ENOSPC_SENTENCE),
        "{}",
        record.cause
    );
}
#[test]
fn preemption_cause_survives_later_not_run_reporting() {
    let cause = "capacity background preemption: signal=15 sender_pid=42; Release claim pid=43";
    let output = std::iter::once(cause.to_owned())
        .chain((0..80).map(|n| format!("NOT RUN sibling {n}")))
        .collect::<Vec<_>>()
        .join("\n");
    let sample = Sample {
        state: "could-not-run".into(),
        verifier: "sampled-head".into(),
        inputs: "inputs".into(),
        rows: vec![row("tree", None, &output)],
    };
    let mut record: Record = aterm_toml::from_str("commit='c'\ntree='t'\nhost='h'\nverifier=''\ninputs=''\ntoken='k'\nstate='judging'\nstarted=0\nupdated=1\nlease_until=0\nrows=[]").unwrap();
    accept_sample_result(&aterm_toml::to_string(&sample).unwrap(), &mut record).unwrap();
    assert!(record.cause.contains(cause), "{}", record.cause);
}
#[test]
fn a_planted_signal_is_named_in_the_portable_cause() {
    const CHILD: &str = "ATERM_VERIFY_CAUSE_SIGNAL_CHILD";
    const TEST: &str = "watch::tests::a_planted_signal_is_named_in_the_portable_cause";
    if std::env::var_os(CHILD).is_some() {
        aterm_uds::process::kill_self_now();
    }
    let status = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, "1")
        .spawn()
        .unwrap()
        .wait()
        .unwrap();
    let mut record: Record = aterm_toml::from_str("commit='c'\ntree='t'\nhost='h'\nverifier=''\ninputs=''\ntoken='k'\nstate='judging'\nstarted=0\nupdated=1\nlease_until=0\nrows=[]").unwrap();
    cancel_record(&mut record, status, "planted signal control");
    let portable = aterm_toml::to_string(&record).unwrap();
    let record: Record = aterm_toml::from_str(&portable).unwrap();
    assert_eq!(record.state, "cancelled");
    let cause = record.cause.clone();
    assert!(
        cause.starts_with(&format!("sample signal={};", libc::SIGKILL)),
        "{cause}"
    );
    assert!(cause.contains("planted signal control"));
    use std::os::unix::process::ExitStatusExt as _;
    let mut record = record;
    cancel_record(
        &mut record,
        std::process::ExitStatus::from_raw(75 << 8),
        "stage=tree: Release preemption",
    );
    let refused = record.cause;
    assert!(refused.contains("exit=75"));
    assert!(refused.contains("stage=tree: Release preemption"));
    assert_ne!(cause, refused);
}
#[test]
fn a_rehomed_candidates_signal_remains_a_cancellation() {
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(
        background_status(std::process::ExitStatus::from_raw(libc::SIGHUP)),
        75
    );
    assert_eq!(
        background_status(std::process::ExitStatus::from_raw(libc::SIGINT)),
        75
    );
    assert_eq!(
        background_status(std::process::ExitStatus::from_raw(libc::SIGTERM)),
        75
    );
    assert_eq!(
        background_status(std::process::ExitStatus::from_raw(7 << 8)),
        7
    );
    assert_eq!(
        background_status(std::process::ExitStatus::from_raw(libc::SIGKILL)),
        crate::exit::COULD_NOT_RUN
    );
}
#[test]
fn an_exec_handoff_leaves_each_interrupt_at_its_default_disposition() {
    const CHILD: &str = "ATERM_VERIFY_EXEC_INTERRUPT_HANDOFF_TEST";
    const TEST: &str =
        "watch::tests::an_exec_handoff_leaves_each_interrupt_at_its_default_disposition";
    if std::env::var_os(CHILD).is_some() {
        crate::exec::group::kill_on_interrupt();
        assert!(crate::exec::group::default_interrupts_for_exec());
        for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
            // SAFETY: setting one valid signal to SIG_DFL; the returned disposition is the
            // one the production handoff left there, and this keeps the same disposition.
            let previous = unsafe { libc::signal(signal, libc::SIG_DFL) };
            assert_eq!(previous, libc::SIG_DFL, "signal {signal}");
        }
        return;
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--test-threads=1"])
        .env(CHILD, "1");
    let output =
        aterm_uds::deadline::output_until(&mut command, Instant::now() + Duration::from_secs(60))
            .expect("the disposition probe must finish before the hang detector");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn the_sample_bootstrap_execs_the_sampled_head_image() {
    match std::env::var(SAMPLE_EXEC_TEST_MODE).as_deref() {
        Ok("candidate") => {
            let pid = i32::try_from(std::process::id()).unwrap();
            // SAFETY: the integer queries name this process.
            let (group, session) = unsafe { (libc::getpgrp(), libc::getsid(0)) };
            println!(
                "sampled-head-image={} topology={pid}:{group}:{session}",
                std::env::current_exe().unwrap().display(),
            );
            return;
        }
        Ok("controller") => {
            let root = std::env::current_dir().expect("current directory");
            panic!(
                "production sample handoff returned: {:?}",
                sample(&root, &root.join("unused-sample-result"))
            );
        }
        _ => {}
    }

    let mut scratch =
        aterm_uds::deadline::KeptUntilRan::new(aterm_tempfile::tempdir().expect("scratch").keep());
    let name = std::env::current_exe()
        .expect("test binary")
        .file_name()
        .expect("test binary name")
        .to_owned();
    let controller = scratch.path().join("controller").join(&name);
    let candidate = scratch.path().join("candidate").join(&name);
    fs::create_dir_all(controller.parent().unwrap()).unwrap();
    fs::create_dir_all(candidate.parent().unwrap()).unwrap();
    let source = std::env::current_exe().expect("test binary");
    aterm_uds::deadline::ad_hoc_copy(&source, &controller);
    aterm_uds::deadline::ad_hoc_copy(&source, &candidate);
    let mut command = sample_command(&controller);
    command
        .args(["--exact", SAMPLE_EXEC_TEST_NAME, "--nocapture"])
        .env(SAMPLE_EXEC_TEST_MODE, "controller")
        .env(SAMPLE_EXEC_TEST_CANDIDATE, &candidate);
    let output =
        aterm_uds::deadline::output_until(&mut command, Instant::now() + Duration::from_secs(60))
            .expect("the exec proof must finish before the hang detector");
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let proof = format!("sampled-head-image={}", candidate.display());
    assert!(text.contains(&proof), "{text}");
    let topology = text
        .split("topology=")
        .nth(1)
        .and_then(|rest| rest.lines().next())
        .expect("candidate topology proof");
    let ids: Vec<_> = topology.split(':').collect();
    assert_eq!(ids.len(), 3, "{text}");
    assert_eq!(ids[0], ids[1], "the exec changed process group: {text}");
    assert_eq!(ids[0], ids[2], "the exec changed session: {text}");
    scratch.ran();
    assert!(output.status.success(), "{text}");
    assert!(
        !text.contains(&format!("sampled-head-image={}", controller.display())),
        "the stale controller image handled sample-ready: {text}"
    );
}
#[test]
fn a_wrapped_background_candidate_rehomes_before_it_claims() {
    const CHILD: &str = "ATERM_VERIFY_BACKGROUND_ENTRY_TEST_CHILD";
    const SCRATCH: &str = "ATERM_VERIFY_BACKGROUND_ENTRY_TEST_SCRATCH";
    const TEST: &str = "watch::tests::a_wrapped_background_candidate_rehomes_before_it_claims";
    if std::env::var_os(CHILD).is_none() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env(SCRATCH, scratch.path());
        let output = aterm_uds::deadline::output_until(
            &mut command,
            Instant::now() + Duration::from_secs(60),
        )
        .expect("the wrapped candidate must finish before the hang detector");
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{text}");
        assert!(
            text.contains("background candidate owns its group"),
            "{text}"
        );
        return;
    }

    let args = ["--exact", TEST, "--nocapture"].map(std::ffi::OsString::from);
    let inline_background = [std::ffi::OsString::from("--background=1")];
    // The production path is tethered. A libtest binary has no tether entry verb, so this
    // subprocess exercises the same setsid birth without routing it through that verb;
    // tether's identical setsid composition is covered by `a_sample_has_a_real_session...`.
    if let Some(code) =
        background_entry_with(&args, background_requested(&inline_background), false)
    {
        std::process::exit(code);
    }
    // SAFETY: getpgrp has no pointer arguments and only reads this process's group.
    assert_eq!(unsafe { libc::getpgrp() }, std::process::id() as i32);
    let ledger = aterm_capacity::Ledger::at(
        PathBuf::from(std::env::var_os(SCRATCH).expect("scratch path")).join("ledger"),
    );
    let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(8, 6, 2));
    let claim = aterm_capacity::admit_with(
        &ledger,
        &machine,
        aterm_capacity::Need::new("background entry control").cores(1),
        aterm_capacity::Admit::Preemptible,
        aterm_capacity::Pace::default(),
        &mut |_| {},
    )
    .expect("a self-led candidate can own a Background claim");
    assert_eq!(claim.record().need.rank, aterm_capacity::Rank::Background);
    println!("background candidate owns its group");
}
#[test]
fn a_sample_has_a_real_session_as_well_as_its_own_group() {
    let topology = |mut command: aterm_uds::spawnfd::Command| {
        let mut child = command.arg("60").spawn().unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        // SAFETY: these integer queries name this test's own unreaped child.
        let (group, session) = unsafe { (libc::getpgid(pid), libc::getsid(pid)) };
        child.kill().unwrap();
        child.wait().unwrap();
        (pid, group, session)
    };
    let (pid, group, session) = topology(sample_command(Path::new("/bin/sleep")));
    assert_eq!((group, session), (pid, pid));
    let mut group_only = aterm_uds::spawnfd::command("/bin/sleep");
    group_only.process_group(0);
    let (pid, group, session) = topology(group_only);
    assert_eq!(group, pid);
    assert_ne!(
        session, pid,
        "a group alone leaves launchd's session inherited"
    );
}
#[test]
fn attribution_resolves_package_aliases_and_submodule_pins() {
    let metadata: aterm_json::Value = aterm_json::from_str(r#"{"packages":[{"name":"alias","manifest_path":"/repo/crates/actual/Cargo.toml","dependencies":[]}]}"#).unwrap();
    let graph = metadata_graph(Path::new("/repo"), &metadata).unwrap();
    let roots = BTreeSet::from(["crates/alias".into()]);
    assert_eq!(
        resolve_roots(Path::new("/repo"), &roots, &metadata, &graph).unwrap(),
        BTreeSet::from(["crates/actual".into()])
    );
    assert!(
        resolve_roots(
            Path::new("/repo"),
            &BTreeSet::from(["crates/unknown".into()]),
            &metadata,
            &graph
        )
        .is_err()
    );
    let paths = BTreeSet::from(["vendor/bus/crates/client".into(), "vendor/bus-other".into()]);
    assert_eq!(
        gitlink_paths(&paths, "160000 commit abc\tvendor/bus\0"),
        BTreeSet::from(["vendor/bus".into(), "vendor/bus-other".into()])
    );
}
#[test]
fn attribution_queries_real_metadata_without_a_compilation_lane() {
    let root = std::fs::canonicalize(aterm_tempfile::manifest_dir().join("../..")).unwrap();
    let links = git(&root, &["ls-tree", "-rz", "--full-tree", "HEAD"]).unwrap();
    let paths = item_paths(
        &root,
        &BTreeSet::from(["crates/aterm-verify".into()]),
        false,
        &links,
    )
    .unwrap();
    assert_eq!(paths, BTreeSet::from(["crates/aterm-verify".into()]));
}
#[test]
fn sampled_verifier_and_attribution_drop_outer_targo_authority() {
    const AUTHORITY: &str = "TRUST_TARGO_NESTED_UNVERIFIED_ENDPOINT";
    const MODE: &str = "ATERM_VERIFY_WATCH_TARGO_AUTHORITY_TEST";
    const TEST: &str = "watch::tests::sampled_verifier_and_attribution_drop_outer_targo_authority";

    match std::env::var(MODE).as_deref() {
        Ok("after-scrub") => {
            assert!(std::env::var_os(AUTHORITY).is_none());
            return;
        }
        Ok("inherited") => {
            assert!(std::env::var_os(AUTHORITY).is_some());
            assert!(sample_removed_env().iter().any(|name| name == AUTHORITY));
            assert!(!inherited_build_env("TRUST_STAGE2_BIN"));

            let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
            command
                .args(["--exact", TEST, "--nocapture"])
                .env(MODE, "after-scrub");
            scrub(&mut command);
            let output = aterm_uds::deadline::output_until(
                &mut command,
                Instant::now() + Duration::from_secs(60),
            )
            .expect("the scrubbed attribution child must finish before the hang detector");
            assert!(output.status.success());
            return;
        }
        _ => {}
    }

    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture"])
        .env(MODE, "inherited")
        .env(AUTHORITY, "/outer/targo/unverified.sock");
    let output =
        aterm_uds::deadline::output_until(&mut command, Instant::now() + Duration::from_secs(60))
            .expect("the inherited-authority probe must finish before the hang detector");
    assert!(output.status.success());
}
#[test]
fn attribution_preserves_a_nested_crate_at_its_git_pin() {
    let root = std::fs::canonicalize(aterm_tempfile::manifest_dir().join("../..")).unwrap();
    let links = git(&root, &["ls-tree", "-rz", "--full-tree", "HEAD"]).unwrap();
    let evidence = row(
        "tree",
        Some(1),
        "FAIL -p astream-effects at /fleet/other-checkout/vendor/astream/crates/astream-effects/src/lib.rs:12",
    );
    let roots = item_crates(&evidence, &links);
    assert_eq!(
        roots,
        BTreeSet::from([
            "crates/astream-effects".into(),
            "vendor/astream/crates/astream-effects".into()
        ])
    );
    assert_eq!(
        item_paths(&root, &roots, false, &links).unwrap(),
        BTreeSet::from(["vendor/astream".into()])
    );
}
#[test]
fn attribution_keeps_a_git_pin_removed_by_the_fix() {
    let dir = aterm_tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]).unwrap();
    git(dir.path(), &["config", "user.name", "Andrew Yates"]).unwrap();
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    )
    .unwrap();
    fs::write(dir.path().join("item"), "seed").unwrap();
    git(dir.path(), &["add", "item"]).unwrap();
    git(dir.path(), &["commit", "-m", "seed"]).unwrap();
    let target = git(dir.path(), &["rev-parse", "HEAD"]).unwrap();
    git(
        dir.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &target,
            "vendor/retired",
        ],
    )
    .unwrap();
    git(dir.path(), &["commit", "-m", "pin"]).unwrap();
    let pinned = git(dir.path(), &["rev-parse", "HEAD"]).unwrap();
    git(
        dir.path(),
        &["update-index", "--force-remove", "vendor/retired"],
    )
    .unwrap();
    git(dir.path(), &["commit", "-m", "remove pin"]).unwrap();
    let links = transition_gitlinks(dir.path(), &pinned).unwrap();
    assert_eq!(
        item_crates(
            &row(
                "tree",
                Some(1),
                "error at /fleet/old/vendor/retired/crates/client/src/lib.rs"
            ),
            &links
        ),
        BTreeSet::from(["vendor/retired/crates/client".into()])
    );
}
#[test]
fn stopping_a_sample_first_allows_its_signal_handler_to_clean_up() {
    use std::os::unix::process::ExitStatusExt as _;
    let mut child = aterm_uds::spawnfd::command(aterm_uds::os_path!("sleep"))
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    stop_sample(&mut child).unwrap();
    assert_eq!(
        child.wait().unwrap().signal(),
        Some(libc::SIGTERM),
        "direct SIGKILL would prevent the worker from retiring its nested groups"
    );
}
#[test]
fn failed_renewals_preserve_only_confirmed_time_and_ownership_loss_stops() {
    let start = Instant::now();
    let at = |seconds| start + Duration::from_secs(seconds);
    let mut lease = Renewal::new(at(120), 1120, start);
    assert!(
        lease
            .finish(
                Err(PublishError::Unavailable("network".into())),
                at(30),
                1150,
                at(31),
                1031
            )
            .is_ok()
    );
    assert_eq!(lease.last_failure.as_deref(), Some("network"));
    assert_eq!(lease.expires, at(120));
    assert_eq!(lease.epoch_until, 1120);
    assert_eq!(lease.next, at(36));
    assert_eq!(lease.deadline(at(87), 1087), at(90));
    assert!(
        !lease.valid(at(31), 1121),
        "sleep or a forward wall-clock jump cannot prolong ownership"
    );
    assert!(
        !lease.valid(at(121), 900),
        "a backward wall-clock jump cannot prolong ownership"
    );
    assert!(lease.finish(Ok(()), at(60), 1180, at(61), 1061).is_ok());
    assert_eq!(
        lease.last_failure.as_deref(),
        Some("network"),
        "a later success retains the last failed transaction for diagnostics"
    );
    assert_eq!(lease.expires, at(180));
    assert_eq!(lease.cutoff(), at(150));
    assert!(
        lease
            .finish(Err(PublishError::Lost), at(80), 1200, at(81), 1081)
            .is_err()
    );
    assert!(
        lease
            .finish(
                Err(PublishError::Unavailable("network".into())),
                at(145),
                1265,
                at(150),
                1150
            )
            .is_err()
    );
    assert_eq!(lease.expires, at(180));
}

#[test]
fn renewal_uses_the_owned_lease_budget_instead_of_an_unrelated_timeout() {
    let start = Instant::now();
    let at = |seconds| start + Duration::from_secs(seconds);
    let mut lease = Renewal::new(at(120), 1120, start);
    // A six-second fetch/append/push fits the owned lease. The old five-second cap
    // rejected it even though sixty seconds remained before cleanup had to begin.
    assert_eq!(lease.deadline(at(30), 1030), at(90));
    assert!(at(36) < lease.deadline(at(30), 1030));
    lease.finish(Ok(()), at(30), 1150, at(36), 1036).unwrap();
    assert_eq!(lease.expires, at(150));
    assert_eq!(lease.epoch_until, 1150);
    assert_eq!(lease.deadline(at(87), 1140), at(87));
    assert_eq!(lease.deadline(at(119), 1000), at(120));
}

#[test]
fn workspace_seed_survives_source_removal_without_contacting_remote() {
    let dir = aterm_tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main", "source"]).unwrap();
    let source = dir.path().join("source");
    git(&source, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(&source, &["config", "user.email", "test@example.invalid"]).unwrap();
    fs::write(source.join("item"), "independent objects").unwrap();
    git(&source, &["add", "item"]).unwrap();
    git(&source, &["commit", "-m", "seed"]).unwrap();
    // A network-first clone fails against this deliberately unavailable remote.
    git(
        &source,
        &["remote", "add", "origin", "https://invalid.invalid/absent"],
    )
    .unwrap();
    let opts = Options {
        workspace: dir.path().join("lane"),
        remote: "origin".into(),
        branch: "main".into(),
        once: false,
        interval: Duration::from_secs(60),
    };
    let root = prepare_workspace(&opts, &source).unwrap();
    fs::remove_dir_all(&source).unwrap();
    assert_eq!(
        git(&root, &["show", "HEAD:item"]).unwrap(),
        "independent objects"
    );
    assert_eq!(
        git(&root, &["remote", "get-url", "origin"]).unwrap(),
        "https://invalid.invalid/absent"
    );
    assert!(!root.join(".git/objects/info/alternates").exists());
}

#[test]
fn attribution_names_non_merge_suspects_and_only_an_unambiguous_fix() {
    let dir = aterm_tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--bare", "remote"]).unwrap();
    git(dir.path(), &["init", "-b", "main", "source"]).unwrap();
    let root = dir.path().join("source");
    git(&root, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(&root, &["config", "user.email", "test@example.invalid"]).unwrap();
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            dir.path().join("remote").to_str().unwrap(),
        ],
    )
    .unwrap();
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"trust\"\n",
    )
    .unwrap();
    fs::write(root.join("trust.lock"), "fixture\n").unwrap();
    git(&root, &["add", "rust-toolchain.toml", "trust.lock"]).unwrap();
    let commit = |name: &str, text: &str| {
        fs::write(root.join("item"), text).unwrap();
        git(&root, &["add", "item"]).unwrap();
        git(&root, &["commit", "-m", name]).unwrap();
        git(&root, &["rev-parse", "HEAD"]).unwrap()
    };
    let record = |commit: &str, state: &str, updated| Record {
        commit: commit.into(),
        tree: git(&root, &["rev-parse", &format!("{commit}^{{tree}}")]).unwrap(),
        host: "test".into(),
        verifier: commit.into(),
        controller: String::new(),
        inputs: "test".into(),
        token: commit.into(),
        state: state.into(),
        cause: String::new(),
        started: updated,
        updated,
        lease_until: 0,
        rows: vec![row(
            "tree",
            Some(if state == "red" { 1 } else { 0 }),
            "whole-workspace item",
        )],
    };
    let base = commit("base", "good");
    assert!(
        judged::append(
            &root,
            "origin",
            None,
            &record(&base, "pass", 30),
            judged::deadline()
        )
        .unwrap()
    );
    git(&root, &["checkout", "-b", "unrelated", &base]).unwrap();
    let unrelated = commit("unrelated pass", "side");
    let head = judged::head(&root, "origin", judged::deadline())
        .unwrap()
        .unwrap();
    assert!(
        judged::append(
            &root,
            "origin",
            Some(&head),
            &record(&unrelated, "pass", 100),
            judged::deadline()
        )
        .unwrap()
    );
    git(&root, &["checkout", "main"]).unwrap();
    let nearest_green = commit("newer green", "still good");
    let head = judged::head(&root, "origin", judged::deadline())
        .unwrap()
        .unwrap();
    assert!(
        judged::append(
            &root,
            "origin",
            Some(&head),
            &record(&nearest_green, "pass", 20),
            judged::deadline()
        )
        .unwrap()
    );
    let red = commit("red", "bad");
    let mut red_record = record(&red, "red", 40);
    let sample_result = root.join(".git/test-sample.toml");
    write_sampled_result(&root, &sample_result, red_record.rows.clone()).unwrap();
    let sampled: Sample =
        aterm_toml::from_str(&fs::read_to_string(&sample_result).expect("sampled-head result"))
            .unwrap();
    assert_eq!(sampled.state, "red");
    assert_eq!(sampled.verifier, red);
    assert!(sampled.inputs.contains("trust.lock="), "{}", sampled.inputs);
    red_record.inputs = sampled.inputs;
    red_record.rows = sampled.rows;
    assert_eq!(
        red_record.rows[0].suspects.as_slice(),
        std::slice::from_ref(&red),
        "publication time and an unrelated pass cannot move the range before the nearest green"
    );
    let head = judged::head(&root, "origin", judged::deadline())
        .unwrap()
        .unwrap();
    assert!(
        judged::append(
            &root,
            "origin",
            Some(&head),
            &red_record,
            judged::deadline()
        )
        .unwrap()
    );
    let fix = commit("fix", "good again");
    let fixed_tree = git(&root, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let merge = git(
        &root,
        &[
            "commit-tree",
            &fixed_tree,
            "-p",
            &fix,
            "-p",
            &base,
            "-m",
            "merge",
        ],
    )
    .unwrap();
    let mut passing = record(&merge, "pass", 50);
    attribute(&root, &passing.commit, &passing.tree, &mut passing.rows).unwrap();
    assert_eq!(
        passing.rows[0].fixed_by.as_deref(),
        Some(fix.as_str()),
        "the landing merge is not a second suspect"
    );
    let extra = commit("another change", "good with another edit");
    let mut ambiguous = record(&extra, "pass", 60);
    attribute(
        &root,
        &ambiguous.commit,
        &ambiguous.tree,
        &mut ambiguous.rows,
    )
    .unwrap();
    assert!(
        ambiguous.rows[0].fixed_by.is_none(),
        "two non-merge changes cannot be attributed to one fix"
    );

    git(&root, &["checkout", "-b", "attribution-left", &base]).unwrap();
    let left = commit("left green", "left good");
    let head = judged::head(&root, "origin", judged::deadline())
        .unwrap()
        .unwrap();
    assert!(
        judged::append(
            &root,
            "origin",
            Some(&head),
            &record(&left, "pass", 70),
            judged::deadline()
        )
        .unwrap()
    );
    git(&root, &["checkout", "-b", "attribution-right", &base]).unwrap();
    let right = commit("right green", "right good");
    let head = judged::head(&root, "origin", judged::deadline())
        .unwrap()
        .unwrap();
    assert!(
        judged::append(
            &root,
            "origin",
            Some(&head),
            &record(&right, "pass", 80),
            judged::deadline()
        )
        .unwrap()
    );
    let right_tree = git(&root, &["rev-parse", &format!("{right}^{{tree}}")]).unwrap();
    let joined = git(
        &root,
        &[
            "commit-tree",
            &right_tree,
            "-p",
            &left,
            "-p",
            &right,
            "-m",
            "join incomparable greens",
        ],
    )
    .unwrap();
    git(&root, &["checkout", "--detach", &joined]).unwrap();
    let ambiguous_red = commit("red after both greens", "bad after join");
    let mut ambiguous_red_record = record(&ambiguous_red, "red", 90);
    attribute(
        &root,
        &ambiguous_red_record.commit,
        &ambiguous_red_record.tree,
        &mut ambiguous_red_record.rows,
    )
    .unwrap();
    assert!(
        ambiguous_red_record.rows[0].suspects.is_empty(),
        "incomparable last-green ancestors define no honest single suspect range"
    );
}
#[test]
fn ordinary_work_delays_a_start_but_only_release_stops_a_running_sample() {
    let mut row = aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::Holder,
        pid: 1,
        start_ms: 1,
        seq: 0,
        checkout: "/sample".into(),
        need: aterm_capacity::Need::new("fixture"),
        timing: aterm_capacity::TimingEvidence::default(),
        held: None,
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    };
    assert!(!pressure_requires_stop(
        Path::new("/sample"),
        std::slice::from_ref(&row),
        SamplePhase::Starting,
    ));
    row.checkout = "/other".into();
    assert!(pressure_requires_stop(
        Path::new("/sample"),
        std::slice::from_ref(&row),
        SamplePhase::Starting,
    ));
    assert!(!pressure_requires_stop(
        Path::new("/sample"),
        std::slice::from_ref(&row),
        SamplePhase::Running,
    ));
    row.held = Some(aterm_capacity::Held {
        admitted_ms: 1,
        cores: 0,
        alone: false,
    });
    assert!(!pressure_requires_stop(
        Path::new("/sample"),
        std::slice::from_ref(&row),
        SamplePhase::Starting,
    ));
    row.need.rank = aterm_capacity::Rank::Release;
    for phase in [SamplePhase::Starting, SamplePhase::Running] {
        assert!(pressure_requires_stop(
            Path::new("/sample"),
            &[row.clone()],
            phase
        ));
    }
}
#[test]
fn the_sampled_verifier_is_built_once_then_reused_without_an_outer_slot() {
    let ready = SampleExec::ready(
        Path::new("/repo"),
        Path::new("/repo/.git/result"),
        Path::new("/trust/bin"),
        std::ffi::OsStr::new("/trust/bin:/usr/bin"),
    );
    let candidate = candidate_path(Path::new("/repo"));
    assert_eq!(ready.program, candidate);
    assert_eq!(
        ready.args,
        ["watch", "--sample-ready", "/repo/.git/result"].map(std::ffi::OsString::from)
    );
    let build = candidate_build_command(
        Path::new("/repo"),
        Path::new("/trust/bin/targo"),
        Path::new("/trust/bin"),
        std::ffi::OsStr::new("/trust/bin:/usr/bin"),
        7,
        "claim",
    );
    assert_eq!(
        build.argv(),
        [
            "/trust/bin/targo",
            "--unverified",
            "build",
            "--locked",
            "--profile",
            "test",
            "-p",
            "aterm-verify",
            "--bin",
            "aterm-verify",
        ]
        .map(str::to_owned)
    );
    let command = tree_command(
        Path::new("/repo"),
        &candidate,
        &["release", "--tree", "--background"],
        Path::new("/trust/bin"),
        std::ffi::OsStr::new("/trust/bin:/usr/bin"),
    );
    assert_eq!(
        command.argv(),
        [
            format!(
                "/repo/.ubd/target.noindex/debug/aterm-verify{}",
                std::env::consts::EXE_SUFFIX
            ),
            "release".to_owned(),
            "--tree".to_owned(),
            "--background".to_owned(),
        ]
    );
    for forbidden in [
        "BUILD_SLOT_HELD",
        "CARGO_BUILD_JOBS",
        aterm_capacity::COVER_ENV,
        aterm_capacity::DISK_COVER_ENV,
        "ATERM_RELEASE_*",
    ] {
        assert!(
            command.envs.iter().all(|(name, _)| name != forbidden),
            "the direct verifier must own {forbidden}, not inherit an outer claim"
        );
        assert!(
            sample_removed_env().iter().any(|name| name == forbidden),
            "the sample must remove inherited {forbidden}"
        );
    }
}
#[test]
fn a_stage_waits_for_memory_without_holding_foreground_cores() {
    use aterm_capacity::{Admit, Fixed, Ledger, Need, Pace, Reading};
    use aterm_spec::wait::{MUST_HAPPEN, until_within};

    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = std::sync::Arc::new(Fixed::new(Reading::of_gib(48, 40, 14)));
    let memory = aterm_capacity::admit_with(
        &ledger,
        &*machine,
        Need::new("ordinary memory holder").mem_gib(30),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("the foreground memory claim");

    std::thread::scope(|scope| {
        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let waiting_ledger = ledger.clone();
        let waiting_machine = std::sync::Arc::clone(&machine);
        let waiter = scope.spawn(move || {
            let claim = aterm_capacity::admit_with(
                &waiting_ledger,
                &*waiting_machine,
                stage_need("inputs", 14),
                // The production caller is a dedicated process-group leader and requests
                // Preemptible. This scratch ledger exercises the same atomic Need under Wait
                // so the unit-test harness never authorizes cancellation of its shared group.
                Admit::Wait,
                Pace {
                    poll: Duration::from_millis(5),
                    ..Pace::default()
                },
                &mut |_| {},
            )
            .expect("the background stage claim");
            admitted_tx.send(claim.cores()).expect("reported admission");
            release_rx.recv().expect("released by the test");
        });

        let waiting = until_within(MUST_HAPPEN, "the stage's one atomic wait", || {
            ledger
                .list()
                .ok()?
                .into_iter()
                .find(|record| record.need.what == "verify watch inputs")
        });
        assert_eq!(waiting.need.mem, 26 << 30);
        assert_eq!(waiting.need.cores, 14);
        assert!(waiting.need.covers);
        assert_eq!(waiting.need.rank, aterm_capacity::Rank::Ordinary);
        assert!(
            waiting.held.is_none(),
            "a memory wait must retain no Background cores: {waiting:?}"
        );

        drop(memory);
        assert_eq!(
            until_within(MUST_HAPPEN, "the unblocked stage admission", || {
                admitted_rx.try_recv().ok()
            }),
            14
        );
        release_tx.send(()).expect("release the stage claim");
        waiter.join().expect("the stage waiter");
    });
}
#[test]
fn stage_evidence_keeps_real_failures_without_summary_or_blocked_aliases() {
    use crate::checks::Check;
    use crate::ladder::Report;

    for count in [1, crate::report::FINDINGS_SHOWN + 2] {
        let mut expected: Vec<_> = (0..count).map(|i| format!("compile-target-{i}")).collect();
        expected.sort();
        let mut build = Report::new("build");
        for id in &expected {
            build.fail(id);
        }
        let mut blocked = Report::new("tests");
        blocked.cannot_run("tests-after-build");
        let reports = [&build, &blocked];
        for seconds in [2.0, 900.0] {
            let result = crate::report::judge(
                Check::Test,
                &reports,
                seconds,
                "--workspace",
                "rerun-tests".into(),
                None,
            );
            let mut text = String::new();
            for (index, report) in reports.iter().enumerate() {
                text.push_str(&crate::report::stage_event(
                    index,
                    Some(Check::Test),
                    report,
                    Duration::ZERO,
                    Duration::ZERO,
                    None,
                ));
            }
            text.push_str(&crate::report::check_event(&result, &reports));
            text.push_str(&crate::report::render(&result, 8));
            let captured = row("tree", Some(1), &text);
            assert_eq!(captured.outcome, "red");
            assert_eq!(captured.red_items, expected);
            assert_eq!(captured.decisions.len(), count + 1);
            assert_eq!(
                captured.decisions.last(),
                Some(&Decision {
                    id: "tests-after-build".into(),
                    outcome: "NOT RUN".into(),
                })
            );
            let unavailable = row("tree", Some(3), &text);
            assert_eq!(unavailable.outcome, "could-not-run");
            assert!(unavailable.red_items.is_empty());
        }
    }
}

#[test]
fn partial_or_unavailable_samples_never_become_green() {
    let pass = row("tree", Some(0), "ok tests");
    assert_eq!(verdict(std::slice::from_ref(&pass)), "could-not-run");
    assert_eq!(verdict(&vec![pass; 4]), "could-not-run");
    let rows = [
        "inputs",
        "stable-x86_64",
        "tree",
        "export",
        "cut",
        "inputs-end",
    ]
    .map(|id| row(id, Some(0), "ok"));
    assert_eq!(verdict(&rows), "pass");
    assert_eq!(verdict(&[row("compat", Some(1), "error[E0502]")]), "red");
    assert_eq!(
        verdict(&[row("tree", Some(3), "NOT RUN compiler")]),
        "could-not-run"
    );
    assert!(
        row("tree", Some(3), "FAIL infrastructure")
            .red_items
            .is_empty()
    );
    assert_eq!(
        row("tree", Some(1), "  FAIL -p agh --lib jq_all").red_items,
        ["-p agh --lib jq_all"]
    );
}
#[test]
fn the_controller_copies_the_sampled_heads_complete_result_without_recomputing_it() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let result = dir.path().join("sample.toml");
    let mut rows: Vec<Row> = [
        "inputs",
        "stable-x86_64",
        "tree",
        "export",
        "cut",
        "inputs-end",
    ]
    .map(|id| row(id, Some(0), "ok"))
    .into();
    rows[0].suspects = vec!["sampled-suspect".into()];
    rows[1].fixed_by = Some("sampled-fix".into());
    let mut record = Record {
        commit: "commit".into(),
        tree: "tree".into(),
        host: "host".into(),
        verifier: "old-controller".into(),
        controller: "sha256=controller-image".into(),
        inputs: String::new(),
        token: "token".into(),
        state: "judging".into(),
        cause: String::new(),
        started: 1,
        updated: 1,
        lease_until: 2,
        rows: Vec::new(),
    };
    write_result(
        &result,
        &Sample {
            // Deliberately disagree with the old controller's `verdict(&rows)` result. A
            // mutation that recomputes here turns this pass and is caught.
            state: "red".into(),
            verifier: "sampled-head-verifier".into(),
            inputs: "sampled-head-inputs".into(),
            rows,
        },
    )
    .expect("serialized sampled-head result");
    let text = fs::read_to_string(result).expect("read sampled-head result");
    accept_sample_result(&text, &mut record).expect("accepted sampled-head result");
    assert_eq!(record.state, "red");
    assert_eq!(record.verifier, "sampled-head-verifier");
    let portable: Record = aterm_toml::from_str(&aterm_toml::to_string(&record).unwrap()).unwrap();
    assert_eq!(portable.controller, "sha256=controller-image");
    assert_eq!(record.inputs, "sampled-head-inputs");
    assert_eq!(record.rows.len(), 6);
    assert_eq!(record.rows[0].suspects, ["sampled-suspect"]);
    assert_eq!(record.rows[1].fixed_by.as_deref(), Some("sampled-fix"));
}
#[test]
fn bootstrap_results_keep_code_red_but_claim_no_sampled_identity_or_attribution() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = dir.path();
    git(root, &["init", "-b", "main"]).unwrap();
    git(root, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(root, &["config", "user.email", "test@example.invalid"]).unwrap();
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"trust\"\n",
    )
    .unwrap();
    fs::write(root.join("trust.lock"), "fixture\n").unwrap();
    git(root, &["add", "rust-toolchain.toml", "trust.lock"]).unwrap();
    git(root, &["commit", "-m", "fixture"]).unwrap();

    let mut evidence = candidate_failure_row(&crate::exec::Run {
        case_observations: None,
        code: Some(101),
        output: "could not compile sampled verifier\nFAIL candidate".into(),
        ..crate::exec::Run::default()
    });
    evidence.suspects = vec!["stale-suspect".into()];
    evidence.fixed_by = Some("stale-fix".into());
    evidence.decisions.push(Decision {
        id: "stale-decision".into(),
        outcome: "red".into(),
    });
    let result = root.join("bootstrap-result.toml");
    write_bootstrap_result(root, &result, evidence).unwrap();
    let sample: Sample = aterm_toml::from_str(&fs::read_to_string(result).unwrap()).unwrap();
    assert_eq!(sample.state, "red");
    assert!(sample.verifier.is_empty());
    assert_eq!(sample.rows.len(), 1);
    assert_eq!(sample.rows[0].id, "bootstrap");
    assert_eq!(sample.rows[0].outcome, "red");
    assert_eq!(sample.rows[0].red_items, ["candidate"]);
    assert!(sample.rows[0].suspects.is_empty());
    assert!(sample.rows[0].fixed_by.is_none());
    assert!(sample.rows[0].decisions.is_empty());

    let unavailable = candidate_failure_row(&crate::exec::Run {
        case_observations: None,
        spawn_error: Some("candidate compiler vanished".into()),
        output: "could not compile sampled verifier".into(),
        code: Some(101),
        ..crate::exec::Run::default()
    });
    let unavailable_result = root.join("bootstrap-unavailable.toml");
    write_bootstrap_result(root, &unavailable_result, unavailable).unwrap();
    let unavailable: Sample =
        aterm_toml::from_str(&fs::read_to_string(unavailable_result).unwrap()).unwrap();
    assert_eq!(unavailable.state, "could-not-run");
    assert!(unavailable.verifier.is_empty());
    assert_eq!(unavailable.rows[0].outcome, "could-not-run");
    assert!(unavailable.rows[0].red_items.is_empty());
}
#[test]
fn changed_or_missing_input_seals_cannot_qualify_a_sample() {
    let first = "a".repeat(64);
    assert!(seals_match(&format!("inputs: measured\n{first}\n"), &first));
    assert!(!seals_match(&first, &"b".repeat(64)));
    assert!(!seals_match("no seal", "no seal"));
}

#[test]
fn fresh_workspace_is_clean_and_an_existing_checkout_is_preserved() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-b", "main", "source"]).unwrap();
    let source = root.join("source");
    git(&source, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(&source, &["config", "user.email", "test@example.invalid"]).unwrap();
    fs::write(source.join("tracked"), "original").unwrap();
    git(&source, &["add", "tracked"]).unwrap();
    git(&source, &["commit", "-m", "base"]).unwrap();
    git(
        &source,
        &["remote", "add", "origin", source.to_str().unwrap()],
    )
    .unwrap();
    let mut opts = Options {
        workspace: root.join("lane"),
        remote: "origin".into(),
        branch: "main".into(),
        once: true,
        interval: Duration::from_secs(1),
    };
    let physical = prepare_workspace(&opts, &source).unwrap();
    assert!(
        git(&opts.workspace, &["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );
    let alias = root.join("lane-alias");
    std::os::unix::fs::symlink(&opts.workspace, &alias).unwrap();
    opts.workspace = alias;
    assert_eq!(
        prepare_workspace(&opts, &source).unwrap(),
        physical,
        "all children and dependency attribution receive the same physical workspace"
    );
    opts.workspace = source.clone();
    fs::write(source.join("tracked"), "person's work").unwrap();
    assert!(prepare_workspace(&opts, &source).is_err());
    assert_eq!(
        fs::read_to_string(source.join("tracked")).unwrap(),
        "person's work"
    );
}
#[test]
fn suspects_include_transitive_workspace_dependencies_but_not_unrelated_crates() {
    let graph = BTreeMap::from([
        (
            "a".into(),
            ("crates/a".into(), BTreeSet::from(["b".into()])),
        ),
        (
            "b".into(),
            ("crates/b".into(), BTreeSet::from(["c".into()])),
        ),
        ("c".into(), ("crates/c".into(), BTreeSet::new())),
        (
            "unrelated".into(),
            ("crates/unrelated".into(), BTreeSet::new()),
        ),
    ]);
    let roots = item_crates(&row("tree", Some(1), "FAIL -p a --lib item"), "");
    assert_eq!(
        dependency_closure(&roots, &graph),
        BTreeSet::from(["crates/a".into(), "crates/b".into(), "crates/c".into()])
    );
    let roots = item_crates(
        &row("compat", Some(1), "error at crates/b/src/lib.rs:12"),
        "",
    );
    assert_eq!(roots, BTreeSet::from(["crates/b".into()]));
    let metadata = aterm_json::json!({"packages": [
        {"name": "alias", "manifest_path": "/repo/crates/a/Cargo.toml", "dependencies": [{"name": "renamed", "path": "/repo/crates/b"}, {"name": "external", "path": null}]},
        {"name": "actual", "manifest_path": "/repo/crates/b/Cargo.toml", "dependencies": [{"name": "c", "path": "/repo/crates/c"}]},
        {"name": "c", "manifest_path": "/repo/crates/c/Cargo.toml", "dependencies": [{"name":"bus", "path":"/repo/vendor/bus/crates/client"}]},
        {"name": "unrelated", "manifest_path": "/repo/crates/unrelated/Cargo.toml", "dependencies": []}
    ]});
    let graph = metadata_graph(Path::new("/repo"), &metadata).unwrap();
    assert_eq!(
        dependency_closure(&BTreeSet::from(["crates/a".into()]), &graph),
        BTreeSet::from([
            "crates/a".into(),
            "crates/b".into(),
            "crates/c".into(),
            "vendor/bus/crates/client".into()
        ])
    );
}

#[test]
fn signal_provenance_survives_watch_messages_and_the_tail_limit() {
    let mut output = (0..32)
        .map(|n| format!("watch stage={n} done\n"))
        .collect::<String>();
    let signals = [
        "signal=15 sender_pid=42 sender_uid=501 executable=/sender argv=[stop]",
        "child-exit child_pid=51 signal=9 parent_pid=50 sender=unavailable-from-wait-status",
    ];
    for line in signals {
        output.push_str(line);
        output.push('\n');
    }
    output.push_str(
        &(0..80)
            .map(|n| format!("NOT RUN sibling {n}\n"))
            .collect::<String>(),
    );
    let recorded = row("tree", None, &output);
    for line in signals {
        assert!(recorded.log.contains(line), "{}", recorded.log);
    }
    let control = row("tree", Some(0), "completed");
    assert!(!control.log.contains("sender_pid"));
}
