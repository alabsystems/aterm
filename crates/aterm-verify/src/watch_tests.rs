// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The bound these tests give the journal operations they set a scene with: a hang guard of
/// minutes, not the journal's own 60 s wait for a remote ([`judged::deadline`]), which a loaded
/// Mac's fixture can outlive with nothing wrong in the code (2026-11-13, `judged`'s own tests).
fn journal_bound() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(10 * 60)
}

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
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        Duration::from_secs(60),
        "the sampled head image exec proof",
    )
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
/// A CLAIM IS RENEWED AT EACH STAGE END AND AT LEAST EVERY TEN MINUTES, and a renewal the
/// journal did not take is tried again a minute on: sooner than the next ten minutes, and not at
/// every look. Another attempt's hold is asked after at the usual pace. What failed stays for
/// the attempt's last record, whatever succeeds later.
#[test]
fn a_claim_is_renewed_at_each_stage_end_and_every_ten_minutes_and_a_failure_is_tried_again_sooner()
{
    let start = Instant::now();
    let at = |seconds| start + Duration::from_secs(seconds);
    let mut renewal = Renewal::new(start);
    assert!(
        renewal.due(start, 0),
        "the first renewal carries the sampled inputs"
    );
    renewal.renewed(start, 0);
    assert!(!renewal.due(at(judged::RENEW_EVERY - 1), 0));
    assert!(renewal.due(at(judged::RENEW_EVERY), 0));
    assert!(renewal.due(at(1), 1), "a stage ended");
    renewal.renewed(at(1), 1);
    assert!(!renewal.due(at(judged::RENEW_EVERY), 1));
    assert_eq!(renewal.evidence(), None);

    renewal.failed(at(700), 1, &PublishError::Unavailable("network".into()));
    assert!(!renewal.due(at(700 + RENEW_RETRY - 1), 1));
    assert!(renewal.due(at(700 + RENEW_RETRY), 1));
    assert!(
        renewal.due(at(701), 2),
        "a stage end is a renewal's time while renewals fail, too"
    );
    renewal.failed(
        at(701),
        2,
        &PublishError::Displaced("judging by two".into()),
    );
    assert!(!renewal.due(at(701 + RENEW_RETRY), 2));
    assert!(renewal.due(at(701 + judged::RENEW_EVERY), 2));
    renewal.renewed(at(701 + judged::RENEW_EVERY), 2);
    let evidence = renewal.evidence().expect("what failed");
    assert!(
        evidence.contains("failed: 2") && evidence.contains("judging by two"),
        "{evidence}"
    );
}

/// A STAGE END IS A STAGE LOG WRITTEN SINCE THE SAMPLE BEGAN. An earlier sample's log, the
/// sample's own output and the service's log are none.
#[test]
fn a_stage_end_is_a_stage_log_written_since_the_sample_began() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let git_dir = dir.path().join(".git");
    fs::create_dir(&git_dir).unwrap();
    let began = std::time::SystemTime::now();
    let write = |name: &str, written: std::time::SystemTime| {
        let file = File::create(git_dir.join(name)).unwrap();
        file.set_modified(written).unwrap();
    };
    let earlier = began - Duration::from_secs(3_600);
    let later = began + Duration::from_secs(1);
    assert_eq!(stage_ends(dir.path(), began), 0);
    write("watch-inputs.log", earlier);
    write("watch-sample.log", later);
    write("watch-service.log", later);
    write("watch-result.toml", later);
    assert_eq!(stage_ends(dir.path(), began), 0);
    write("watch-candidate.log", later);
    assert_eq!(stage_ends(dir.path(), began), 1);
    write("watch-inputs.log", later);
    write("watch-tree.log", later);
    assert_eq!(stage_ends(dir.path(), began), 3);
}

/// A DECLINED START IS LOGGED ONCE PER REASON, a failure each time, and a sample that ran
/// leaves no line here (its record is the journal's).
#[test]
fn a_declined_start_is_logged_once_per_reason() {
    let declined = |why: &str| Ok(Start::Declined(why.into()));
    let mut last = None;
    assert_eq!(
        log_line(&mut last, &declined("busy")).as_deref(),
        Some("verify watch: no sample started: busy")
    );
    assert_eq!(log_line(&mut last, &declined("busy")), None);
    assert_eq!(
        log_line(&mut last, &declined("c0ffee is red by m1")).as_deref(),
        Some("verify watch: no sample started: c0ffee is red by m1")
    );
    assert_eq!(log_line(&mut last, &Ok(Start::Judged)), None);
    assert_eq!(
        log_line(&mut last, &declined("c0ffee is red by m1")).as_deref(),
        Some("verify watch: no sample started: c0ffee is red by m1"),
        "a sample ran in between, so the reason is news again"
    );
    for _ in 0..2 {
        assert_eq!(
            log_line(&mut last, &Err("journal away".into())).as_deref(),
            Some("verify watch: sample could not run: journal away")
        );
    }
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
            journal_bound()
        )
        .unwrap()
    );
    git(&root, &["checkout", "-b", "unrelated", &base]).unwrap();
    let unrelated = commit("unrelated pass", "side");
    let head = judged::head(&root, "origin", journal_bound())
        .unwrap()
        .unwrap();
    assert!(
        judged::append(
            &root,
            "origin",
            Some(&head),
            &record(&unrelated, "pass", 100),
            journal_bound()
        )
        .unwrap()
    );
    git(&root, &["checkout", "main"]).unwrap();
    let nearest_green = commit("newer green", "still good");
    let head = judged::head(&root, "origin", journal_bound())
        .unwrap()
        .unwrap();
    assert!(
        judged::append(
            &root,
            "origin",
            Some(&head),
            &record(&nearest_green, "pass", 20),
            journal_bound()
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
    let head = judged::head(&root, "origin", journal_bound())
        .unwrap()
        .unwrap();
    assert!(judged::append(&root, "origin", Some(&head), &red_record, journal_bound()).unwrap());
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
    let head = judged::head(&root, "origin", journal_bound())
        .unwrap()
        .unwrap();
    assert!(
        judged::append(
            &root,
            "origin",
            Some(&head),
            &record(&left, "pass", 70),
            journal_bound()
        )
        .unwrap()
    );
    git(&root, &["checkout", "-b", "attribution-right", &base]).unwrap();
    let right = commit("right green", "right good");
    let head = judged::head(&root, "origin", journal_bound())
        .unwrap()
        .unwrap();
    assert!(
        judged::append(
            &root,
            "origin",
            Some(&head),
            &record(&right, "pass", 80),
            journal_bound()
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
fn a_closure_holds_what_the_lock_links_through_a_patch() {
    // As `targo metadata --no-deps` words a workspace: its members and their path dependencies.
    // No manifest names by path what the root manifest patches in, all of it outside the
    // workspace; only the lock says which member links which.
    let registry = "registry+https://example.invalid/index";
    let mirror = "registry+https://example.invalid/mirror";
    let described = Described {
        metadata: aterm_json::json!({"packages": [
            {"name": "a", "manifest_path": "/repo/crates/a/Cargo.toml", "dependencies": [
                {"name": "b", "path": "/repo/crates/b"},
                {"name": "reg", "path": null},
                {"name": "shim", "path": null}
            ]},
            {"name": "b", "manifest_path": "/repo/crates/b/Cargo.toml", "dependencies": []},
            {"name": "c", "manifest_path": "/repo/crates/c/Cargo.toml", "dependencies": []},
            {"name": "ghost", "manifest_path": "/repo/crates/ghost/Cargo.toml", "dependencies": []}
        ]}),
        manifest: aterm_toml::from_str(
            "[patch.crates-io]\n\
             shim = { path = \"./vendor/shim-1\" }\n\
             shim-2 = { package = \"shim\", path = \"vendor/shim-2\" }\n\
             deep = { path = \"vendor/deep\" }\n\
             unused = { path = \"vendor/unused\" }\n\
             elsewhere = { git = \"https://example.invalid/elsewhere\" }\n\
             [patch.\"https://example.invalid/forked\"]\n\
             forked = { path = \"vendor/forked\" }\n",
        )
        .unwrap(),
        lock: aterm_toml::from_str(&format!(
            "version = 4\n\
             [[package]]\nname = \"a\"\nversion = \"0.1.0\"\n\
             dependencies = [\"b\", \"reg\", \"shim 1.0.0\"]\n\
             [[package]]\nname = \"b\"\nversion = \"0.1.0\"\n\
             dependencies = [\"forked\", \"twin 1.0.0 ({mirror})\"]\n\
             [[package]]\nname = \"c\"\nversion = \"0.1.0\"\n\
             dependencies = [\"shim 1.0.0\", \"unused\"]\n\
             [[package]]\nname = \"deep\"\nversion = \"0.1.0\"\n\
             [[package]]\nname = \"forked\"\nversion = \"0.3.0\"\n\
             [[package]]\nname = \"reg\"\nversion = \"1.2.0\"\nsource = \"{registry}\"\n\
             checksum = \"of-reg\"\ndependencies = [\"shim 2.0.0\"]\n\
             [[package]]\nname = \"shim\"\nversion = \"1.0.0\"\n\
             [[package]]\nname = \"shim\"\nversion = \"2.0.0\"\ndependencies = [\"deep\"]\n\
             [[package]]\nname = \"twin\"\nversion = \"1.0.0\"\nsource = \"{registry}\"\n\
             checksum = \"of-twin\"\n\
             [[package]]\nname = \"twin\"\nversion = \"1.0.0\"\nsource = \"{mirror}\"\n\
             checksum = \"of-mirrored-twin\"\n\
             [[package]]\nname = \"unused\"\nversion = \"1.0.0\"\n"
        ))
        .unwrap(),
    };
    let closure = |crate_dir: &str, dependencies| {
        item_closure(
            Path::new("/repo"),
            &BTreeSet::from([crate_dir.to_owned()]),
            dependencies,
            "",
            &|| Ok(&described),
        )
    };
    let set = |members: &[&str]| -> BTreeSet<String> {
        members.iter().map(|member| (*member).to_owned()).collect()
    };
    assert_eq!(
        closure("crates/a", true).unwrap(),
        Closure {
            paths: set(&[
                "crates/a",
                "crates/b",
                "vendor/deep",
                "vendor/forked",
                "vendor/shim-1",
                "vendor/shim-2",
            ]),
            registry: set(&[
                &format!("reg 1.2.0 {registry} of-reg"),
                &format!("twin 1.0.0 {mirror} of-mirrored-twin"),
            ]),
        },
        "a patched crate a member names by version, one a registry package between them names, \
         and one a path dependency names; of two registry packages of one name and version, \
         the one the lock's dependency names"
    );
    assert_eq!(
        closure("crates/c", true).unwrap(),
        Closure {
            paths: set(&[
                "crates/c",
                "vendor/shim-1",
                "vendor/shim-2",
                "vendor/unused"
            ]),
            registry: BTreeSet::new(),
        },
        "both directories of the name `shim` are its, and only version 2.0.0 links `deep`"
    );
    assert_eq!(
        closure("crates/a", false).unwrap(),
        Closure {
            paths: set(&["crates/a"]),
            registry: BTreeSet::new(),
        }
    );
    assert_eq!(
        closure("crates/ghost", true).unwrap_err(),
        "Cargo.lock holds no package at crates/ghost",
        "a crate the lock does not hold has no closure to compare"
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

/// A lane journal on a scratch bare remote, and one checkout per named host with it as `origin`.
fn lane(hosts: &[&str]) -> (aterm_tempfile::TempDir, Vec<PathBuf>) {
    let dir = aterm_tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet", "--bare", "journal"]).unwrap();
    let journal = dir.path().join("journal");
    let roots = hosts
        .iter()
        .map(|host| {
            git(dir.path(), &["init", "--quiet", "-b", "main", host]).unwrap();
            let root = dir.path().join(host);
            git(&root, &["config", "user.name", "Andrew Yates"]).unwrap();
            git(&root, &["config", "user.email", "test@example.invalid"]).unwrap();
            git(
                &root,
                &["remote", "add", "origin", journal.to_str().unwrap()],
            )
            .unwrap();
            root
        })
        .collect();
    (dir, roots)
}

/// `host`'s claim of `tree`, made at `at`.
fn claim(host: &str, tree: &str, token: &str, at: u64) -> Record {
    Record {
        commit: format!("commit-of-{tree}"),
        tree: tree.into(),
        host: host.into(),
        verifier: String::new(),
        controller: String::new(),
        inputs: String::new(),
        token: token.into(),
        state: "judging".into(),
        cause: String::new(),
        started: at,
        updated: at,
        lease_until: at + judged::LEASE,
        rows: Vec::new(),
    }
}

/// A sample that is looked at `looks` more times, thirty seconds apart by its own clock, and then
/// exits clean. Nothing signals its controller, and no real time passes.
struct Scripted {
    at: Instant,
    epoch: u64,
    looks: u64,
    stopped: bool,
}

impl Attended for Scripted {
    fn exited(&mut self) -> Result<Option<std::process::ExitStatus>, String> {
        use std::os::unix::process::ExitStatusExt as _;
        if self.looks == 0 {
            return Ok(Some(std::process::ExitStatus::from_raw(0)));
        }
        self.looks -= 1;
        Ok(None)
    }

    fn stop(&mut self) -> Result<(), String> {
        self.stopped = true;
        Ok(())
    }

    fn signal(&self) -> Option<i32> {
        None
    }

    fn now(&self) -> (Instant, u64) {
        (self.at, self.epoch)
    }

    fn stages(&mut self) -> usize {
        0
    }

    fn pause(&mut self) {
        self.at += Duration::from_secs(30);
        self.epoch += 30;
    }
}

/// A JUDGMENT IS WORTH MORE THAN ITS LEASE, AND ITS LAST RECORD REACHES THE JOURNAL ONCE. Over a
/// journal on a scratch remote and a scripted clock: (1) with the journal away for two leases'
/// time, every renewal fails and the sample is neither stopped nor failed; (2) its terminal
/// record, which cannot be published either, is kept in the checkout, and the next loop appends
/// it at its tree's slot, lapsed lease and all, and forgets the copy; (3) a loop after that, and
/// a copy that outlived its own publication, append nothing; (4) where another host claimed the
/// tree after the lapse and its red or its live claim holds the slot, the late record is filed
/// beside that host's, whichever finishes first, and a red in the slot is not replaced by the
/// late pass; (5) a claim whose controller ended while judging is closed as could-not-run by the
/// next loop. NEGATIVE CONTROLS: stop the sample when a renewal fails, as the 120 s lease did,
/// and (1) fails; let nothing read the kept record, and (2) fails; append a kept record without
/// asking the journal for it, and (3) fails; file a late record at the slot, and (4) loses the
/// other host's.
#[test]
fn a_judgment_outlives_failed_renewals_and_its_terminal_record_is_published_later() {
    let (dir, roots) = lane(&["one", "two"]);
    let [one, two] = roots.as_slice() else {
        unreachable!("two hosts were asked for");
    };
    let journal = dir.path().join("journal");
    let away = dir.path().join("away");
    let reach = |root: &Path, there: bool| {
        let url = if there { &journal } else { &away };
        git(
            root,
            &["remote", "set-url", "origin", url.to_str().unwrap()],
        )
        .unwrap();
    };
    let head = |root: &Path| judged::head(root, "origin", judged::deadline()).unwrap();
    let slot = |root: &Path, tree: &str| {
        judged::record(root, head(root).as_deref(), tree, judged::deadline()).unwrap()
    };
    let all =
        |root: &Path| judged::records(root, head(root).as_deref(), judged::deadline()).unwrap();
    let append = |root: &Path, record: &Record| {
        assert!(
            judged::append(
                root,
                "origin",
                head(root).as_deref(),
                record,
                judged::deadline()
            )
            .unwrap()
        );
    };
    let kept = |root: &Path| -> Record {
        aterm_toml::from_str(&fs::read_to_string(saved(root)).expect("the kept record")).unwrap()
    };
    let red = |record: &mut Record| {
        record.state = "red".into();
        record.rows = vec![row("tree", Some(1), "FAIL the item")];
    };
    let began = 1_800_000_000;

    // (1) Host one claims a tree and keeps the claim, as an attempt does. Then the journal is
    // away for the whole judgment.
    let mut first = claim("one", "t1", "first", began);
    append(one, &first);
    save(one, &first).unwrap();
    let claimed = head(one);
    reach(one, false);
    let mut sample = Scripted {
        at: Instant::now(),
        epoch: began,
        looks: 2 * judged::LEASE / 30 + 1,
        stopped: false,
    };
    let ended = attend(one, &mut first, &mut sample);
    assert!(
        !sample.stopped,
        "a renewal that failed ended the judgment: {ended:?}"
    );
    assert!(
        matches!(&ended, Ok((Ended::Exited(status), Some(_))) if status.success()),
        "the sample runs to its own end, and the failed renewals are named: {ended:?}"
    );
    let ended_at = sample.epoch;
    assert!(
        ended_at > began + 2 * judged::LEASE,
        "control: the lease lapsed while the sample ran"
    );

    // (2) The verdict cannot be published while the journal is away: it is kept.
    red(&mut first);
    assert!(
        conclude(one, &mut first, ended_at).is_err(),
        "control: the journal is away"
    );
    assert_eq!(kept(one), first);
    assert_eq!((first.state.as_str(), first.lease_until), ("red", 0));
    reach(one, true);
    assert_eq!(head(one), claimed, "nothing reached the journal meanwhile");
    assert!(
        slot(one, "t1").is_some_and(|held| held.state == "judging" && !held.occupied(ended_at)),
        "control: the journal holds the claim, and its lease has lapsed"
    );
    // The next loop publishes it and forgets the copy.
    settle(one, ended_at + 60).unwrap();
    assert!(!saved(one).exists());
    assert_eq!(slot(one, "t1"), Some(first.clone()));
    let published = head(one);
    assert_ne!(published, claimed);

    // (3) Never twice: the loop after has nothing to publish, and a copy that outlived its own
    // publication (the controller ended between the push and the removal) is only forgotten.
    settle(one, ended_at + 120).unwrap();
    assert_eq!(head(one), published);
    save(one, &first).unwrap();
    settle(one, ended_at + 180).unwrap();
    assert_eq!(head(one), published, "the journal already held it");
    assert!(!saved(one).exists());

    // (4a) Two hosts judge one tree after a lapse; the other host's red is in the slot first.
    let at = ended_at + 1_000;
    let mut late = claim("one", "t2", "late", at);
    append(one, &late);
    save(one, &late).unwrap();
    reach(one, false);
    let lapsed = at + judged::LEASE + 1;
    assert!(
        slot(two, "t2").is_some_and(|held| !held.occupied(lapsed)),
        "control: host one's lease lapsed, so host two may claim the tree"
    );
    let mut other = claim("two", "t2", "other", lapsed);
    append(two, &other);
    red(&mut other);
    conclude(two, &mut other, lapsed + 600).unwrap();
    assert_eq!(slot(two, "t2"), Some(other.clone()));
    late.state = "pass".into();
    assert!(conclude(one, &mut late, lapsed + 700).is_err());
    reach(one, true);
    settle(one, lapsed + 760).unwrap();
    assert!(!saved(one).exists());
    assert_eq!(
        slot(one, "t2"),
        Some(other.clone()),
        "a red is never replaced by a later pass"
    );
    let records = all(one);
    assert!(
        records.contains(&late) && records.contains(&other),
        "two hosts that judged one tree leave two records: {records:?}"
    );
    let published = head(one);
    save(one, &late).unwrap();
    settle(one, lapsed + 820).unwrap();
    assert_eq!(
        head(one),
        published,
        "a record beside the slot is not filed twice"
    );

    // (4b) The same, with the other host still judging under a live claim when the late record
    // arrives: its claim is left alone, and its own verdict then takes the slot.
    let at = lapsed + 2_000;
    let mut late = claim("one", "t3", "late3", at);
    append(one, &late);
    let lapsed = at + judged::LEASE + 1;
    let mut other = claim("two", "t3", "other3", lapsed);
    append(two, &other);
    red(&mut late);
    conclude(one, &mut late, lapsed + 60).unwrap();
    assert_eq!(
        slot(one, "t3"),
        Some(other.clone()),
        "a live claim of another host is not replaced"
    );
    other.state = "pass".into();
    conclude(two, &mut other, lapsed + 120).unwrap();
    assert_eq!(slot(two, "t3"), Some(other.clone()));
    let records = all(two);
    assert!(
        records.contains(&late) && records.contains(&other),
        "{records:?}"
    );

    // (5) A controller that ended while judging left its claim in the checkout and the
    // journal: the next loop closes it.
    let at = lapsed + 2_000;
    let orphan = claim("one", "t4", "orphan", at);
    append(one, &orphan);
    save(one, &orphan).unwrap();
    settle(one, at + 90).unwrap();
    assert!(!saved(one).exists());
    let closed = slot(one, "t4").expect("the closed claim");
    assert_eq!(
        (
            closed.token.as_str(),
            closed.state.as_str(),
            closed.lease_until
        ),
        ("orphan", "could-not-run", 0)
    );
    assert_eq!(closed.updated, at + 90);
}

/// A TREE WITH A VERDICT IS NOT CLAIMED AGAIN, WHEREVER THE VERDICT IS FILED. Host one's red is
/// filed beside the live claim host two made after one's lease lapsed; host two's attempt then
/// ends cancelled, which leaves the slot to any new claim. The red beside it still keeps a new
/// claim out. CONTROLS: the same tree before the red is held by nothing once the claim on it
/// lapses, and after it by the live claim alone until that claim ends. NEGATIVE CONTROL: ask
/// the slot alone, as a claim did, and the tree is judged again after its red.
#[test]
fn a_tree_judged_beside_its_slot_is_not_claimed_again() {
    let (_dir, roots) = lane(&["one", "two"]);
    let [one, two] = roots.as_slice() else {
        unreachable!("two hosts were asked for");
    };
    let head = |root: &Path| judged::head(root, "origin", judged::deadline()).unwrap();
    let append = |root: &Path, record: &Record| {
        assert!(
            judged::append(
                root,
                "origin",
                head(root).as_deref(),
                record,
                judged::deadline()
            )
            .unwrap()
        );
    };
    let held_at = |root: &Path, epoch: u64| {
        holder(root, head(root).as_deref(), "t", epoch, judged::deadline())
            .unwrap()
            .map(|held| (held.token, held.state))
    };
    let began = 1_800_000_000;
    let mut late = claim("one", "t", "late", began);
    append(one, &late);
    let lapsed = began + judged::LEASE + 1;
    assert_eq!(
        held_at(two, began + 1),
        Some(("late".into(), "judging".into()))
    );
    assert_eq!(
        held_at(two, lapsed),
        None,
        "control: a lapsed claim holds nothing"
    );
    let mut other = claim("two", "t", "other", lapsed);
    append(two, &other);
    late.state = "red".into();
    late.rows = vec![row("tree", Some(1), "FAIL the item")];
    conclude(one, &mut late, lapsed + 60).unwrap();
    assert_eq!(
        held_at(one, lapsed + 61),
        Some(("other".into(), "judging".into())),
        "control: the live claim holds the slot, and the red is beside it"
    );
    other.state = "cancelled".into();
    conclude(two, &mut other, lapsed + 120).unwrap();
    assert_eq!(
        judged::record(two, head(two).as_deref(), "t", judged::deadline())
            .unwrap()
            .map(|held| held.occupied(lapsed + 121)),
        Some(false),
        "control: the slot itself is open to a new claim"
    );
    assert_eq!(
        held_at(two, lapsed + 121),
        Some(("late".into(), "red".into())),
        "a red beside the slot keeps the tree from being judged again"
    );
}
