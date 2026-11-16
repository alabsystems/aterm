// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeMap;

#[test]
fn a_pending_status_keeps_exact_controller_selectors_when_the_host_is_clipped() {
    let node = "1234567890abcdef1234567890abcdef";
    let session = "tqn5r6e0148";
    let status = pending_status(&"長".repeat(DESCRIPTION_CHARS), Some(node), Some(session));
    assert_eq!(status.state, "pending");
    assert!(status.description.contains(&format!("node={node}")));
    assert!(status.description.contains(&format!("session={session}")));
    assert_eq!(status.description.chars().count(), DESCRIPTION_CHARS);
    assert!(status.description.ends_with('…'));
    assert!(!status.verdict());
    for (node, session) in [(None, None), (Some("not a node"), Some("not a session"))] {
        let status = pending_status("m45", node, session);
        assert!(!status.description.contains("node="));
        assert!(!status.description.contains("session="));
        assert!(status.description.contains("m45"));
    }
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
    let stopped = stopped_sample(
        std::process::ExitStatus::from_raw(75 << 8),
        &stopped_run("bootstrap", &ran, false),
    );
    assert_eq!(stopped.state, "could-not-run");
    let log = &stopped.rows[0].log;
    assert!(log.contains("exit=75"), "{log}");
    assert!(log.contains("watch bootstrap stopped:"), "{log}");
    assert!(log.contains("kind=environment"), "{log}");
    assert!(log.contains(crate::exec::ENOSPC_SENTENCE), "{log}");
}

#[test]
fn a_planted_signal_is_named_in_the_pass_that_could_not_run() {
    const CHILD: &str = "ATERM_VERIFY_CAUSE_SIGNAL_CHILD";
    const TEST: &str = "watch::tests::a_planted_signal_is_named_in_the_pass_that_could_not_run";
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
    let stopped = stopped_sample(status, "planted signal control");
    assert_eq!(stopped.state, "could-not-run");
    assert!(stopped.rows[0].red_items.is_empty());
    let log = stopped.rows[0].log.clone();
    assert!(
        log.starts_with(&format!("pass signal={};", libc::SIGKILL)),
        "{log}"
    );
    assert!(log.contains("planted signal control"));
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
                "production pass handoff returned: {:?}",
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
        "the stale controller image ran the pass: {text}"
    );
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

/// A DECLINED START OR A FAILED TURN IS LOGGED ONCE PER REASON, and a pass that ran leaves no
/// line here (its verdict is the commit status) and makes the next reason news again.
#[test]
fn a_declined_start_is_logged_once_per_reason() {
    let declined = |why: &str| Ok(Start::Declined(why.into()));
    let mut last = None;
    assert_eq!(
        log_line(&mut last, &declined("judged")).as_deref(),
        Some("verify watch: no pass started: judged")
    );
    assert_eq!(log_line(&mut last, &declined("judged")), None);
    assert_eq!(
        log_line(&mut last, &declined("c0ffee has the follower's failure")).as_deref(),
        Some("verify watch: no pass started: c0ffee has the follower's failure")
    );
    assert_eq!(log_line(&mut last, &Ok(Start::Judged)), None);
    assert_eq!(
        log_line(&mut last, &declined("c0ffee has the follower's failure")).as_deref(),
        Some("verify watch: no pass started: c0ffee has the follower's failure"),
        "a pass ran in between, so the reason is news again"
    );
    // A refusal GitHub repeats at every turn (a token without commit status access) is one
    // line, not one a minute.
    assert_eq!(
        log_line(&mut last, &Err("GitHub 403".into())).as_deref(),
        Some("verify watch: pass could not run: GitHub 403")
    );
    assert_eq!(log_line(&mut last, &Err("GitHub 403".into())), None);
    assert_eq!(
        log_line(&mut last, &Err("GitHub away".into())).as_deref(),
        Some("verify watch: pass could not run: GitHub away")
    );
}

fn opts(workspace: PathBuf, once: bool) -> Options {
    Options {
        workspace,
        remote: "origin".into(),
        branch: "main".into(),
        once,
        interval: Duration::from_secs(60),
        repair_cwd: Some(PathBuf::from("/")),
    }
}

/// THE DEFAULT REPAIR FOLDER IS THE MAIN CHECKOUT, never the worktree that installed the
/// follower, which is removed once its branch lands. NEGATIVE CONTROL: a named folder wins.
#[test]
fn the_default_repair_folder_is_the_main_checkout_not_the_installing_worktree() {
    let dir = aterm_tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main", "main"]).unwrap();
    let main = fs::canonicalize(dir.path().join("main")).unwrap();
    git(&main, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(&main, &["config", "user.email", "test@example.invalid"]).unwrap();
    git(&main, &["commit", "--allow-empty", "-m", "seed"]).unwrap();
    let worktree = dir.path().join("worktree");
    git(
        &main,
        &["worktree", "add", "-b", "w", worktree.to_str().unwrap()],
    )
    .unwrap();
    let mut options = opts(dir.path().join("lane"), false);
    options.repair_cwd = None;
    assert_eq!(options.repair_cwd(&worktree).unwrap(), main);
    assert_eq!(options.repair_cwd(&main).unwrap(), main);
    options.repair_cwd = Some(worktree.clone());
    assert_eq!(options.repair_cwd(&main).unwrap(), worktree);
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
    let root = prepare_workspace(&opts(dir.path().join("lane"), false), &source).unwrap();
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
    let mut opts = opts(root.join("lane"), true);
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
        "every child receives the same physical workspace"
    );
    opts.workspace = source.clone();
    fs::write(source.join("tracked"), "person's work").unwrap();
    assert!(prepare_workspace(&opts, &source).is_err());
    assert_eq!(
        fs::read_to_string(source.join("tracked")).unwrap(),
        "person's work"
    );
}

/// A RED IS NAMED BY PACKAGE, TARGET AND TEST, never by the shard filter its binary ran under:
/// a case's own event and the check's finding for it are one red, and a binary that failed
/// with no case is named by its binary and why.
#[test]
fn reds_are_named_by_package_target_and_test_without_a_shard_filter() {
    let event = |value: aterm_json::Value| format!("verify-event: {value}\n");
    let mut text = String::new();
    text.push_str(&event(aterm_json::json!({
        "event": "test-case-failed", "outcome": "FAIL",
        "test": "-p aterm-gui --lib -- settings::", "case": "settings::tests::a_row_saves",
    })));
    text.push_str(&event(aterm_json::json!({
        "event": "test-binary-finished", "outcome": "FAIL",
        "test": "-p aterm-gui --lib -- settings::",
        "failures": [{"case": "settings::tests::a_row_saves"}],
    })));
    text.push_str(&event(aterm_json::json!({
        "event": "test-binary-finished", "outcome": "FAIL",
        "test": "-p aterm-uds --test spawn -- --skip slow::", "failures": [],
        "detail": "hung: no output for 60 s",
    })));
    text.push_str(&event(aterm_json::json!({
        "event": "check-finished", "outcome": "FAIL", "check": "test",
        "findings": ["-p aterm-gui --lib -- settings:: -- settings::tests::a_row_saves"],
    })));
    text.push_str(&event(aterm_json::json!({
        "event": "check-finished", "outcome": "FAIL", "check": "census",
        "findings": ["inline-test-module: crates/x/src/lib.rs"],
    })));
    let red = row("tree", Some(1), &text);
    assert_eq!(red.outcome, "red");
    assert_eq!(
        red.red_items,
        [
            "aterm-gui --lib settings::tests::a_row_saves",
            "aterm-uds --test spawn: hung: no output for 60 s",
            "census: inline-test-module: crates/x/src/lib.rs",
        ]
    );
    assert!(
        red.red_items.iter().all(|name| !name.contains(" -- ")),
        "a shard filter is no red's name: {:?}",
        red.red_items
    );
    let unavailable = row("tree", Some(3), &text);
    assert_eq!(unavailable.outcome, "could-not-run");
    assert!(unavailable.red_items.is_empty());
}

#[test]
fn partial_or_unavailable_passes_never_become_green_and_a_red_row_stops_none() {
    let ids: Vec<&str> = planned(false).iter().map(|(id, _)| *id).collect();
    assert!(!ids.contains(&"appkit"), "no desk, no appkit row");
    assert!(ids.contains(&"cells"));
    let with_desk: Vec<&str> = planned(true).iter().map(|(id, _)| *id).collect();
    assert_eq!(with_desk.len(), ids.len() + 1);
    assert!(with_desk.contains(&"appkit"));
    let pass = row("tree", Some(0), "ok tests");
    assert_eq!(verdict(std::slice::from_ref(&pass), &ids), "could-not-run");
    let rows: Vec<Row> = ids.iter().map(|id| row(id, Some(0), "ok")).collect();
    assert_eq!(verdict(&rows, &ids), "pass");
    assert_eq!(
        verdict(&rows, &with_desk),
        "could-not-run",
        "a desk Mac's pass without its appkit row is no green"
    );
    let mut mixed = rows.clone();
    mixed[2] = row("tree", Some(1), "  FAIL -p agh --lib jq_all");
    mixed[3] = row("cells", Some(3), "NOT RUN compiler");
    assert_eq!(verdict(&mixed, &ids), "red");
    assert_eq!(mixed[2].red_items, ["-p agh --lib jq_all"]);
    assert!(
        row("tree", Some(3), "FAIL infrastructure")
            .red_items
            .is_empty()
    );
}

#[test]
fn bootstrap_results_keep_code_red_and_an_unavailable_build_could_not_run() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let evidence = candidate_failure_row(&crate::exec::Run {
        case_observations: None,
        code: Some(101),
        output: "could not compile sampled verifier\nFAIL candidate".into(),
        ..crate::exec::Run::default()
    });
    let result = dir.path().join("bootstrap-result.toml");
    write_bootstrap_result(&result, evidence).unwrap();
    let sample: Sample = aterm_toml::from_str(&fs::read_to_string(result).unwrap()).unwrap();
    assert_eq!(sample.state, "red");
    assert_eq!(sample.rows.len(), 1);
    assert_eq!(sample.rows[0].id, "bootstrap");
    assert_eq!(sample.rows[0].red_items, ["candidate"]);

    let unavailable = candidate_failure_row(&crate::exec::Run {
        case_observations: None,
        spawn_error: Some("candidate compiler vanished".into()),
        output: "could not compile sampled verifier".into(),
        code: Some(101),
        ..crate::exec::Run::default()
    });
    let unavailable_result = dir.path().join("bootstrap-unavailable.toml");
    write_bootstrap_result(&unavailable_result, unavailable).unwrap();
    let unavailable: Sample =
        aterm_toml::from_str(&fs::read_to_string(unavailable_result).unwrap()).unwrap();
    assert_eq!(unavailable.state, "could-not-run");
    assert_eq!(unavailable.rows[0].outcome, "could-not-run");
    assert!(unavailable.rows[0].red_items.is_empty());
}

#[test]
fn changed_or_missing_input_seals_cannot_qualify_a_pass() {
    let first = "a".repeat(64);
    assert!(seals_match(&format!("inputs: measured\n{first}\n"), &first));
    assert!(!seals_match(&first, &"b".repeat(64)));
    assert!(!seals_match("no seal", "no seal"));
}

#[test]
fn signal_provenance_survives_watch_messages_and_the_tail_limit() {
    let mut output = (0..32)
        .map(|n| format!("watch row={n} done\n"))
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

/// A scripted world: statuses by commit (newest first), the commits whose read fails, the
/// statuses posted, the workers started with where each stands, and those retired.
#[derive(Default)]
struct Scripted {
    statuses: BTreeMap<String, Vec<Status>>,
    unreadable: Vec<String>,
    posted: Vec<(String, Status)>,
    post_attempts: usize,
    post_error: Option<String>,
    caller_write_error: Option<String>,
    caller_repairs: Vec<String>,
    spawned: Vec<String>,
    workers: BTreeMap<String, Worker>,
    worker_reads: Vec<String>,
    retired: Vec<String>,
}

impl World for Scripted {
    fn statuses(&mut self, commit: &str) -> Result<Vec<Status>, String> {
        if self.unreadable.iter().any(|c| c == commit) {
            return Err("GitHub answered 502".into());
        }
        Ok(self.statuses.get(commit).cloned().unwrap_or_default())
    }

    fn post(&mut self, commit: &str, status: &Status) -> Result<(), String> {
        self.post_attempts += 1;
        if let Some(why) = &self.post_error {
            return Err(why.clone());
        }
        self.statuses
            .entry(commit.to_owned())
            .or_default()
            .insert(0, status.clone());
        self.posted.push((commit.to_owned(), status.clone()));
        Ok(())
    }

    fn caller_repair(&mut self, context: &str) -> Result<(), String> {
        self.caller_repairs.push(context.to_owned());
        self.caller_write_error.clone().map_or(Ok(()), Err)
    }

    fn worker(&mut self, sid: &str) -> Worker {
        self.worker_reads.push(sid.to_owned());
        self.workers.get(sid).copied().unwrap_or(Worker::Ended)
    }

    fn retire(&mut self, sid: &str) {
        self.retired.push(sid.to_owned());
    }

    fn spawn(&mut self, task: &str) -> Result<String, String> {
        let sid = format!("t{:010}", self.spawned.len() + 1);
        self.spawned.push(task.to_owned());
        self.workers.insert(sid.clone(), Worker::Working);
        Ok(sid)
    }
}

/// A WORKER'S PLACE IS READ FROM ITS TASK RECORD: it holds the repair place while it works or is
/// stalled where it resumes by itself, keeps its reds once it failed or is stalled where only a
/// mark moves it, and has ended otherwise. NEGATIVE CONTROL: a stall at sign-in is no work.
#[test]
fn a_worker_holds_the_repair_place_only_while_it_works() {
    let record = |rest: &str| format!("OK task rev=4 {rest} from=ctl since_ms=9 result_bytes=0");
    assert_eq!(task_state(&record("state=working")), Worker::Working);
    assert_eq!(
        task_state(&record("state=stalled why=startup")),
        Worker::Working
    );
    assert_eq!(
        task_state(&record("state=stalled why=held")),
        Worker::Working
    );
    assert_eq!(
        task_state(&record("state=stalled conclusion=failed why=signin")),
        Worker::Failed,
        "a stall at sign-in stays until a mark: it holds no place a new worker could take"
    );
    assert_eq!(
        task_state(&record(
            "state=failed conclusion=failed why=could-not-finish"
        )),
        Worker::Failed
    );
    assert_eq!(
        task_state(&record("state=done conclusion=finished why=landed")),
        Worker::Ended
    );
    assert_eq!(
        task_state(&record("state=exited conclusion=failed why=exit exit=1")),
        Worker::Ended
    );
    assert_eq!(task_state("OK task -"), Worker::Ended);
    assert_eq!(
        task_state("garbled"),
        Worker::Working,
        "unread is not ended"
    );
    assert_eq!(
        task_state(&record("state=future-terminal conclusion=finished")),
        Worker::Working
    );
}

fn red_sample(test: &str) -> Sample {
    let failed = format!(
        "verify-event: {}\n",
        aterm_json::json!({
            "event": "test-case-failed", "outcome": "FAIL",
            "test": "-p aterm-uds --lib -- deadline::", "case": test,
        })
    );
    Sample {
        state: "red".into(),
        rows: vec![
            row("inputs", Some(0), "ok"),
            row("tree", Some(1), &failed),
            row("cells", Some(0), "ok"),
        ],
    }
}

/// A RED PASS POSTS A FAILURE THAT NAMES THE RED TEST AND STARTS AT MOST ONE REPAIR WORKER: a
/// later red pass while that worker works starts none and names the same worker, a restarted
/// follower finds it in the failure it posted, and only once it has ended does a red start
/// another.
#[test]
fn a_red_pass_posts_a_failure_naming_the_red_test_and_starts_at_most_one_repair_worker() {
    let mut world = Scripted::default();
    world.statuses.insert(
        "green".into(),
        vec![Status::new("success", "pass on m9: inputs tree".into())],
    );
    let root = Path::new("/follower/checkout");
    let mut worker = None;
    let first = conclude(
        &mut world,
        "red1",
        &["landed".into(), "green".into(), "older".into()],
        true,
        &red_sample("deadline::tests::a_child_ends"),
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(first.state, "failure");
    assert_eq!(world.spawned.len(), 1, "a red starts one repair worker");
    let sid = worker.as_ref().expect("the worker").sid.clone();
    assert_eq!(
        first.repair().unwrap().assignment,
        Some(assignment(
            Some("green"),
            &red_sample("deadline::tests::a_child_ends").rows
        ))
    );
    assert!(
        first.description.contains(&format!("repair {sid}")),
        "{}",
        first.description
    );
    assert!(first.description.chars().count() <= DESCRIPTION_CHARS);
    let task = &world.spawned[0];
    assert!(task.contains("red1") && task.contains("green"), "{task}");
    assert!(
        task.contains("aterm-uds --lib deadline::tests::a_child_ends"),
        "{task}"
    );
    assert!(
        task.contains("git bisect start --first-parent red1 green"),
        "{task}"
    );
    assert!(task.contains("/follower/checkout"), "{task}");
    assert!(task.contains("Could not finish"), "{task}");
    assert!(
        !world.statuses.contains_key("landed"),
        "a commit the follower did not judge gets no status"
    );

    // A later red while that worker works: none started, the same one named.
    let second = conclude(
        &mut world,
        "red2",
        &["red1".into(), "landed".into(), "green".into()],
        true,
        &red_sample("deadline::tests::another"),
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 1, "a working repair worker is the one");
    assert!(second.description.contains(&format!("repair {sid}")));

    // A follower started again knows no worker; the failure it posted names the one at work.
    let mut restarted = None;
    conclude(
        &mut world,
        "red3",
        &[
            "red2".into(),
            "red1".into(),
            "landed".into(),
            "green".into(),
        ],
        true,
        &red_sample("deadline::tests::another"),
        RepairMode::Persistent(&mut restarted),
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 1, "the restarted follower starts none");
    assert_eq!(
        restarted.as_ref().map(|repair| repair.sid.as_str()),
        Some(sid.as_str())
    );
    let task_of_tip = repair_task(
        "red3",
        &walk(
            &mut world,
            &["red2".into(), "red1".into(), "green".into()],
            true,
        ),
        &red_sample("x").rows,
        root,
    );
    assert!(
        task_of_tip.contains("git bisect start --first-parent red3 green"),
        "the bisect runs from the tip these reds were seen on to the one known green: \
         {task_of_tip}"
    );
    assert!(task_of_tip.contains("oldest red tip since then is red1"));
    let no_green = repair_task(
        "red3",
        &walk(&mut world, &["red2".into(), "red1".into()], true),
        &red_sample("x").rows,
        root,
    );
    assert!(
        no_green.contains("`git bisect start --first-parent red3`,"),
        "no tip is taken as good without a green: {no_green}"
    );

    // Negative control: once that worker has ended, the next red retires it and starts another.
    world.workers.insert(sid.clone(), Worker::Ended);
    conclude(
        &mut world,
        "red4",
        &["red3".into(), "green".into()],
        true,
        &red_sample("deadline::tests::another"),
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 2);
    assert_ne!(
        worker.as_ref().map(|repair| repair.sid.as_str()),
        Some(sid.as_str())
    );
    assert_eq!(world.retired, [sid]);
}

/// A WORKER THAT COULD NOT FINISH KEEPS THE SAME REDS: the next red pass with those reds names it
/// and starts none, since a fresh worker would meet what stopped it; reds that changed retire it
/// and start another.
#[test]
fn a_worker_that_could_not_finish_keeps_the_same_reds_and_new_reds_start_another() {
    let mut world = Scripted::default();
    world.statuses.insert(
        "green".into(),
        vec![Status::new("success", "pass on m9: inputs tree".into())],
    );
    let root = Path::new("/follower/checkout");
    let mut worker = None;
    conclude(
        &mut world,
        "red1",
        &["green".into()],
        true,
        &red_sample("deadline::tests::a_child_ends"),
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    let sid = worker.as_ref().unwrap().sid.clone();
    world.workers.insert(sid.clone(), Worker::Failed);
    let mut restarted = None;
    let same = conclude(
        &mut world,
        "red2",
        &["red1".into(), "green".into()],
        true,
        &red_sample("deadline::tests::a_child_ends"),
        RepairMode::Persistent(&mut restarted),
        root,
    )
    .unwrap();
    assert_eq!(
        world.spawned.len(),
        1,
        "the same reds start no second worker"
    );
    assert!(same.description.contains(&format!("repair {sid}")));
    assert!(world.retired.is_empty());

    let other = conclude(
        &mut world,
        "red3",
        &["red2".into(), "red1".into(), "green".into()],
        true,
        &red_sample("deadline::tests::another"),
        RepairMode::Persistent(&mut restarted),
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 2, "new reds start a worker");
    assert_eq!(world.retired, std::slice::from_ref(&sid));
    assert!(!other.description.contains(&sid));
}

/// A WALK THAT COULD NOT READ BACK TO A WORKER OR A GREEN STARTS NONE: a worker it did not read
/// may still work, and two at once is what one place forbids. NEGATIVE CONTROL: the same red
/// with every status read starts one.
#[test]
fn an_unread_walk_starts_no_second_worker() {
    let mut world = Scripted::default();
    world.unreadable.push("red1".into());
    let root = Path::new("/follower/checkout");
    let mut worker = None;
    let posted = conclude(
        &mut world,
        "red2",
        &["red1".into(), "green".into()],
        true,
        &red_sample("deadline::tests::a_child_ends"),
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(posted.state, "failure");
    assert!(world.spawned.is_empty());
    assert!(!posted.description.contains("repair"));
    world.unreadable.clear();
    conclude(
        &mut world,
        "red2",
        &["red1".into(), "green".into()],
        true,
        &red_sample("deadline::tests::a_child_ends"),
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 1);
}

#[test]
fn a_one_shot_red_returns_context_without_managing_existing_workers() {
    let root = Path::new("/follower/checkout");
    let options = opts(root.to_owned(), true);
    let sample = red_sample("deadline::tests::caller_failure");
    let red = reds(&sample.rows).remove(0);
    for state in [Worker::Working, Worker::Failed, Worker::Ended] {
        for unread in [false, true] {
            let sid = "t0000000001";
            let mut world = Scripted::default();
            world.statuses.insert(
                "old".into(),
                vec![Status::new(
                    "failure",
                    format!("1 red, repair {sid}: older"),
                )],
            );
            world
                .statuses
                .insert("green".into(), vec![Status::new("success", "pass".into())]);
            world.workers.insert(sid.into(), state);
            if unread {
                world.unreadable.push("old".into());
            }
            let mut worker = Some(Repair {
                sid: sid.to_owned(),
                assignment: None,
            });
            let posted = conclude(
                &mut world,
                "caller-tip",
                &["old".into(), "green".into()],
                true,
                &sample,
                options.repair_mode(&mut worker),
                root,
            )
            .unwrap();
            assert_eq!(posted.state, "failure");
            assert_eq!(posted.worker(), None, "the caller assigns no worker");
            assert_eq!(worker.as_ref().map(|repair| repair.sid.as_str()), Some(sid));
            assert_eq!(world.caller_repairs.len(), 1);
            let context = &world.caller_repairs[0];
            for field in ["caller-tip", red.as_str(), root.to_str().unwrap()] {
                assert!(context.contains(field), "missing {field:?}");
            }
            if !unread {
                assert!(context.contains("green"));
            }
            assert!(world.worker_reads.is_empty());
            assert!(world.retired.is_empty());
            assert!(world.spawned.is_empty());
            assert_eq!(world.posted, [("caller-tip".into(), posted)]);
        }
    }
}

#[test]
fn a_caller_failure_preserves_older_worker_custody_and_its_original_reds() {
    let root = Path::new("/follower/checkout");
    for unread in [false, true] {
        for state in [Worker::Working, Worker::Failed] {
            for same_red in [false, true] {
                let mut world = Scripted::default();
                world
                    .statuses
                    .insert("green".into(), vec![Status::new("success", "pass".into())]);
                let mut worker = None;
                let original = red_sample("deadline::tests::original");
                conclude(
                    &mut world,
                    "original",
                    &["green".into()],
                    true,
                    &original,
                    RepairMode::Persistent(&mut worker),
                    root,
                )
                .unwrap();
                let sid = worker.as_ref().unwrap().sid.clone();
                world.workers.insert(sid.clone(), state);
                if unread {
                    world.unreadable.push("original".into());
                }
                let sample = if same_red {
                    original
                } else {
                    red_sample("deadline::tests::other")
                };
                conclude(
                    &mut world,
                    "caller",
                    &["original".into(), "green".into()],
                    true,
                    &sample,
                    RepairMode::Caller,
                    root,
                )
                .unwrap();
                assert_eq!(world.spawned.len(), 1);
                assert!(world.retired.is_empty());
                assert!(world.worker_reads.is_empty());
                world.unreadable.clear();
                let mut restarted = None;
                let posted = conclude(
                    &mut world,
                    "next",
                    &["caller".into(), "original".into(), "green".into()],
                    true,
                    &sample,
                    RepairMode::Persistent(&mut restarted),
                    root,
                )
                .unwrap();
                let replaced = state == Worker::Failed && !same_red;
                assert_eq!(world.spawned.len(), if replaced { 2 } else { 1 });
                assert_eq!(
                    world.retired,
                    if replaced { vec![sid.clone()] } else { vec![] }
                );
                assert_eq!(world.worker_reads.as_slice(), std::slice::from_ref(&sid));
                if replaced {
                    assert_ne!(posted.worker(), Some(sid.as_str()));
                } else {
                    assert_eq!(posted.worker(), Some(sid.as_str()));
                }
            }
        }
    }
}

#[test]
fn caller_context_is_bounded_and_survives_a_status_write_failure() {
    let root = Path::new("/follower/checkout");
    let mut world = Scripted {
        post_error: Some("GitHub answered 502".into()),
        ..Scripted::default()
    };
    let mut sample = red_sample("deadline::tests::multibyte_failure");
    let row = sample
        .rows
        .iter_mut()
        .find(|row| row.outcome == "red")
        .unwrap();
    row.red_items.push("𝄞".repeat(100_000));
    let error = conclude(
        &mut world,
        "red-tip",
        &[],
        true,
        &sample,
        RepairMode::Caller,
        root,
    )
    .unwrap_err();
    assert_eq!(error, "GitHub answered 502");
    assert_eq!(world.caller_repairs.len(), 1);
    let context = &world.caller_repairs[0];
    assert!(context.len() <= 64 * 1024);
    for field in [
        "red-tip",
        "deadline::tests::multibyte_failure",
        root.to_str().unwrap(),
        "git bisect",
        "targo --unverified verify land",
    ] {
        assert!(context.contains(field), "missing {field:?}");
    }
    assert!(!context.contains("Could not finish:") && !context.contains("FIX <"));
    assert!(world.posted.is_empty());
    assert!(world.worker_reads.is_empty());
    assert!(world.retired.is_empty());
    assert!(world.spawned.is_empty());
}

#[test]
fn caller_output_failure_never_prevents_the_final_status_attempt() {
    struct Closed {
        deferred: bool,
    }
    impl std::io::Write for Closed {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.deferred {
                Ok(bytes.len())
            } else {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }
    for deferred in [false, true] {
        assert_eq!(
            write_caller_repair(&mut Closed { deferred }, "repair context")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }
    for post_error in [None, Some("GitHub refused the status".to_owned())] {
        let mut world = Scripted {
            post_error: post_error.clone(),
            caller_write_error: Some("closed pipe".into()),
            ..Scripted::default()
        };
        let error = conclude(
            &mut world,
            "tip",
            &[],
            true,
            &red_sample("deadline::tests::failed"),
            RepairMode::Caller,
            Path::new("/follower/checkout"),
        )
        .unwrap_err();
        assert!(error.contains("closed pipe"));
        match post_error {
            Some(post) => assert!(
                error.starts_with(&post),
                "status error stays primary: {error}"
            ),
            None => {
                assert_eq!(world.posted.len(), 1);
                assert_eq!(world.posted[0].1.state, "failure");
                assert!(world.posted[0].1.verdict());
            }
        }
        assert_eq!(world.caller_repairs.len(), 1);
        assert_eq!(world.post_attempts, 1);
        assert!(world.worker_reads.is_empty());
        assert!(world.spawned.is_empty());
        assert!(world.retired.is_empty());
    }
}

#[test]
fn a_working_workers_assignment_survives_new_reds_and_a_failed_first_post() {
    let root = Path::new("/follower/checkout");
    for first_post_fails in [false, true] {
        for return_to_original in [false, true] {
            let mut world = Scripted::default();
            world
                .statuses
                .insert("green".into(), vec![Status::new("success", "pass".into())]);
            if first_post_fails {
                world.post_error = Some("GitHub refused A".into());
            }
            let a = red_sample("deadline::tests::a");
            let b = red_sample("deadline::tests::b");
            let mut worker = None;
            let first = conclude(
                &mut world,
                "a",
                &["green".into()],
                true,
                &a,
                RepairMode::Persistent(&mut worker),
                root,
            );
            assert_eq!(first.is_err(), first_post_fails);
            let original = worker.clone().unwrap();
            assert_eq!(
                original.assignment,
                Some(assignment(Some("green"), &a.rows))
            );
            world.post_error = None;
            let working_b = conclude(
                &mut world,
                "working-b",
                &["a".into(), "green".into()],
                true,
                &b,
                RepairMode::Persistent(&mut worker),
                root,
            )
            .unwrap();
            assert_eq!(working_b.repair(), Some(original.clone()));
            assert_eq!(world.spawned.len(), 1);
            world.workers.insert(original.sid.clone(), Worker::Failed);
            let mut restarted = None;
            let final_sample = if return_to_original { &a } else { &b };
            conclude(
                &mut world,
                "next",
                &["working-b".into(), "a".into(), "green".into()],
                true,
                final_sample,
                RepairMode::Persistent(&mut restarted),
                root,
            )
            .unwrap();
            assert_eq!(world.spawned.len(), if return_to_original { 1 } else { 2 });
            if return_to_original {
                assert_eq!(restarted, Some(original));
                assert!(world.retired.is_empty());
            } else {
                assert_eq!(world.retired, [original.sid]);
                assert_eq!(
                    restarted.unwrap().assignment,
                    Some(assignment(Some("green"), &b.rows))
                );
            }
        }
    }
}

#[test]
fn a_failed_repair_gets_a_new_assignment_after_a_green_frontier() {
    let mut world = Scripted::default();
    let root = Path::new("/follower/checkout");
    world.statuses.insert(
        "old-green".into(),
        vec![Status::new("success", "pass".into())],
    );
    let red = red_sample("deadline::tests::a");
    let green = Sample {
        state: "pass".into(),
        rows: vec![row("tree", Some(0), "ok")],
    };
    let mut worker = None;
    conclude(
        &mut world,
        "old-red",
        &["old-green".into()],
        true,
        &red,
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    let original = worker.clone().unwrap();
    world.workers.insert(original.sid.clone(), Worker::Failed);
    conclude(
        &mut world,
        "new-green",
        &["old-red".into(), "old-green".into()],
        true,
        &green,
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(worker, Some(original.clone()));
    conclude(
        &mut world,
        "new-red",
        &["new-green".into(), "old-red".into(), "old-green".into()],
        true,
        &red,
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 2);
    assert_eq!(world.retired, [original.sid]);
    assert_ne!(worker.unwrap().assignment, original.assignment);
}

#[test]
fn a_working_repair_crosses_a_green_frontier_until_it_fails_even_after_restart() {
    for restart in [false, true] {
        let mut world = Scripted::default();
        let root = Path::new("/follower/checkout");
        world.statuses.insert(
            "old-green".into(),
            vec![Status::new("success", "pass".into())],
        );
        let red = red_sample("deadline::tests::a");
        let green = Sample {
            state: "pass".into(),
            rows: vec![row("tree", Some(0), "ok")],
        };
        let mut worker = None;
        conclude(
            &mut world,
            "old-red",
            &["old-green".into()],
            true,
            &red,
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        let original = worker.clone().unwrap();
        conclude(
            &mut world,
            "new-green",
            &["old-red".into(), "old-green".into()],
            true,
            &green,
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        let held = conclude(
            &mut world,
            "working-red",
            &["new-green".into(), "old-red".into(), "old-green".into()],
            true,
            &red,
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        assert_eq!(held.repair(), Some(original.clone()));
        assert_eq!(world.spawned.len(), 1);
        assert!(world.retired.is_empty());
        world.workers.insert(original.sid.clone(), Worker::Failed);
        if restart {
            worker = None;
        }
        conclude(
            &mut world,
            "failed-red",
            &[
                "working-red".into(),
                "new-green".into(),
                "old-red".into(),
                "old-green".into(),
            ],
            true,
            &red,
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        assert_eq!(world.spawned.len(), 2, "restart={restart}");
        assert_eq!(world.retired, [original.sid]);
        assert_ne!(worker.unwrap().assignment, original.assignment);
    }
}

#[test]
fn a_green_pass_carries_repair_custody_across_an_immediate_restart() {
    for caller_green in [false, true] {
        for state in [Worker::Working, Worker::Failed] {
            let mut world = Scripted::default();
            let root = Path::new("/follower/checkout");
            world.statuses.insert(
                "old-green".into(),
                vec![Status::new("success", "pass".into())],
            );
            let red = red_sample("deadline::tests::a");
            let green = Sample {
                state: "pass".into(),
                rows: vec![row("tree", Some(0), "ok")],
            };
            let mut worker = None;
            conclude(
                &mut world,
                "old-red",
                &["old-green".into()],
                true,
                &red,
                RepairMode::Persistent(&mut worker),
                root,
            )
            .unwrap();
            let original = worker.clone().unwrap();
            world.workers.insert(original.sid.clone(), state);
            let mode = if caller_green {
                RepairMode::Caller
            } else {
                RepairMode::Persistent(&mut worker)
            };
            let passed = conclude(
                &mut world,
                "new-green",
                &["old-red".into(), "old-green".into()],
                true,
                &green,
                mode,
                root,
            )
            .unwrap();
            assert_eq!(passed.state, "success");
            assert!(world.worker_reads.is_empty());
            assert!(world.retired.is_empty());
            assert_eq!(world.spawned.len(), 1);
            let mut restarted = None;
            conclude(
                &mut world,
                "new-red",
                &["new-green".into(), "old-red".into(), "old-green".into()],
                true,
                &red,
                RepairMode::Persistent(&mut restarted),
                root,
            )
            .unwrap();
            assert_eq!(
                world.spawned.len(),
                if state == Worker::Working { 1 } else { 2 },
                "caller_green={caller_green}, state={state:?}"
            );
            assert_eq!(passed.repair(), Some(original.clone()));
            if state == Worker::Working {
                assert_eq!(restarted, Some(original));
                assert!(world.retired.is_empty());
            } else {
                assert_eq!(world.retired, [original.sid]);
                assert_ne!(restarted.unwrap().assignment, original.assignment);
            }
        }
    }
}

#[test]
fn a_failed_repair_keeps_its_assignment_at_the_same_green_frontier_after_restart() {
    for restart in [false, true] {
        let mut world = Scripted::default();
        let root = Path::new("/follower/checkout");
        world
            .statuses
            .insert("green".into(), vec![Status::new("success", "pass".into())]);
        let red = red_sample("deadline::tests::a");
        let mut worker = None;
        conclude(
            &mut world,
            "old-red",
            &["green".into()],
            true,
            &red,
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        let original = worker.clone().unwrap();
        world.workers.insert(original.sid.clone(), Worker::Failed);
        if restart {
            worker = None;
        }
        conclude(
            &mut world,
            "new-red",
            &["old-red".into(), "green".into()],
            true,
            &red,
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        assert_eq!(world.spawned.len(), 1);
        assert!(world.retired.is_empty());
        assert_eq!(worker, Some(original));
    }
}

#[test]
fn worker_assignment_hashes_canonical_full_row_and_red_keys() {
    let mut a = row("tree", Some(1), "failed");
    a.red_items = vec!["abc".into(), "x".into()];
    let mut reordered = a.clone();
    reordered.red_items = vec!["x".into(), "abc".into(), "abc".into()];
    assert_eq!(
        assignment(None, &[a.clone()]),
        assignment(None, &[reordered])
    );
    assert_ne!(
        assignment(None, &[a.clone()]),
        assignment(Some(""), &[a.clone()])
    );
    assert_ne!(
        assignment(Some("green-a"), &[a.clone()]),
        assignment(Some("green-b"), &[a.clone()])
    );
    let mut other_row = a.clone();
    other_row.id = "cells".into();
    assert_ne!(
        assignment(None, &[a.clone()]),
        assignment(None, &[other_row])
    );
    let mut long_a = a.clone();
    long_a.red_items = vec![format!("{}a", "𝄞".repeat(1000))];
    let mut long_b = long_a.clone();
    long_b.red_items[0].pop();
    long_b.red_items[0].push('b');
    assert_ne!(assignment(None, &[long_a]), assignment(None, &[long_b]));
    let mut left = a.clone();
    left.id = "ab".into();
    left.red_items = vec!["c".into()];
    let mut right = a;
    right.id = "a".into();
    right.red_items = vec!["bc".into()];
    assert_ne!(assignment(None, &[left]), assignment(None, &[right]));
}

#[test]
fn repair_headers_preserve_custody_and_never_infer_assignment_from_prose() {
    let hash = assignment(None, &red_sample("deadline::tests::a").rows);
    for sid in ["tqn5r6e0148", "s-3f7db22fd790a94538cd"] {
        for state in ["failure", "success"] {
            let status = Status::new(
                state,
                format!("repair {sid} assigned={hash}; {}", "detail".repeat(100)),
            );
            assert!(status.verdict());
            assert_eq!(status.description.chars().count(), DESCRIPTION_CHARS);
            assert_eq!(
                status.repair(),
                Some(Repair {
                    sid: sid.into(),
                    assignment: Some(hash.clone())
                })
            );
            for malformed in [
                "?".to_owned(),
                hash[..40].into(),
                hash.to_uppercase(),
                format!("{hash}x"),
            ] {
                let status = Status::new(state, format!("repair {sid} assigned={malformed}; red"));
                assert_eq!(
                    status.repair(),
                    Some(Repair {
                        sid: sid.into(),
                        assignment: None
                    })
                );
            }
            let truncated = Status::new(state, format!("repair {sid} assigned={hash}"));
            assert_eq!(truncated.repair().unwrap().assignment, None);
            for fake in [
                format!("1 red: test repair {sid} assigned={hash}; x"),
                format!("x red, repair {sid}: x"),
            ] {
                assert_eq!(Status::new(state, fake).repair(), None);
            }
        }
        let legacy = Status::new("failure", format!("1 red, repair {sid}: old"));
        assert_eq!(legacy.repair().unwrap().assignment, None);
        let legacy_success = Status::new("success", format!("1 red, repair {sid}: old"));
        assert_eq!(legacy_success.repair(), None);
        for state in ["pending", "error"] {
            assert_eq!(
                Status::new(state, format!("repair {sid} assigned={hash}; text")).repair(),
                None
            );
        }
    }
}

#[test]
fn an_unknown_worker_assignment_holds_custody_without_claiming_it_tried_new_reds() {
    let root = Path::new("/follower/checkout");
    let sid = "t0000000001";
    for old in [
        format!("1 red, repair {sid}: old"),
        format!("repair {sid} assigned=broken; old"),
    ] {
        let mut world = Scripted::default();
        world
            .statuses
            .insert("old".into(), vec![Status::new("failure", old)]);
        world.workers.insert(sid.into(), Worker::Failed);
        let mut worker = None;
        let posted = conclude(
            &mut world,
            "new",
            &["old".into()],
            true,
            &red_sample("deadline::tests::new"),
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        assert_eq!(
            posted.repair(),
            Some(Repair {
                sid: sid.into(),
                assignment: None
            })
        );
        assert!(world.spawned.is_empty());
        assert!(world.retired.is_empty());
    }
}

#[test]
fn an_unknown_failure_header_cannot_establish_worker_free_history() {
    let root = Path::new("/follower/checkout");
    let sid = "t0000000001";
    for description in [
        "",
        "unrecognized failure",
        "repair invalid assigned=broken; 1 red",
        "1 red, repair invalid: test",
    ] {
        let mut world = Scripted::default();
        world.workers.insert(sid.into(), Worker::Working);
        world.statuses.insert(
            "older".into(),
            vec![Status::new("failure", format!("1 red, repair {sid}: old"))],
        );
        world.statuses.insert(
            "bad".into(),
            vec![Status::new("failure", description.into())],
        );
        let sample = red_sample("deadline::tests::new");
        conclude(
            &mut world,
            "caller",
            &["bad".into(), "older".into()],
            true,
            &sample,
            RepairMode::Caller,
            root,
        )
        .unwrap();
        assert_eq!(world.caller_repairs.len(), 1);
        let mut worker = None;
        let posted = conclude(
            &mut world,
            "next",
            &["caller".into(), "bad".into(), "older".into()],
            true,
            &sample,
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        assert_eq!(posted.state, "failure");
        assert!(world.worker_reads.is_empty());
        assert!(world.retired.is_empty());
        assert!(world.spawned.is_empty());
    }
    // An anchored caller/no-worker header is evidence, rather than a malformed repair record.
    let mut world = Scripted::default();
    world.statuses.insert(
        "caller".into(),
        vec![Status::new("failure", "1 red: test".into())],
    );
    let mut worker = None;
    conclude(
        &mut world,
        "next",
        &["caller".into()],
        true,
        &red_sample("deadline::tests::new"),
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 1);
}

#[test]
fn a_bounded_unread_or_shallow_history_cannot_hide_a_live_worker() {
    let root = Path::new("/follower/checkout");
    let long = (0..WALK + 2)
        .map(|n| format!("commit-{n}\n"))
        .collect::<String>();
    for answer in [
        Ok((long, false)),
        Ok(("tip\nshallow-boundary\n".into(), true)),
        Err("git rev-list failed".into()),
    ] {
        let (before, complete) = first_parent_history(answer);
        assert!(!complete);
        assert!(before.len() <= WALK);
        let mut world = Scripted::default();
        let sid = "t0000000001";
        world.workers.insert(sid.into(), Worker::Working);
        world.statuses.insert(
            "older".into(),
            vec![Status::new(
                "failure",
                format!("1 red, repair {sid}: original"),
            )],
        );
        let sample = red_sample("deadline::tests::current");
        conclude(
            &mut world,
            "caller",
            &before,
            complete,
            &sample,
            RepairMode::Caller,
            root,
        )
        .unwrap();
        assert_eq!(world.caller_repairs.len(), 1);
        let mut worker = None;
        let posted = conclude(
            &mut world,
            "next",
            &before,
            complete,
            &sample,
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        assert_eq!(posted.state, "failure");
        assert!(world.worker_reads.is_empty());
        assert!(world.retired.is_empty());
        assert!(world.spawned.is_empty());
        assert_eq!(world.workers.get(sid), Some(&Worker::Working));
    }

    // A real root, or a fully read chain exactly at the bound, can establish no prior worker.
    for text in [
        "root\n".into(),
        (0..WALK + 1)
            .map(|n| format!("commit-{n}\n"))
            .collect::<String>(),
    ] {
        let (before, complete) = first_parent_history(Ok((text, false)));
        assert!(complete);
        let mut world = Scripted::default();
        let mut worker = None;
        conclude(
            &mut world,
            "tip",
            &before,
            complete,
            &red_sample("deadline::tests::current"),
            RepairMode::Persistent(&mut worker),
            root,
        )
        .unwrap();
        assert_eq!(world.spawned.len(), 1);
    }
}

#[test]
fn one_shot_passes_and_unavailable_checks_keep_their_verdicts_without_repair() {
    for state in ["pass", "could-not-run"] {
        let sample = Sample {
            state: state.into(),
            rows: vec![row(
                "tree",
                Some(if state == "pass" { 0 } else { 3 }),
                "verdict",
            )],
        };
        let mut world = Scripted::default();
        let mut worker = None;
        let posted = conclude(
            &mut world,
            "tip",
            &[],
            true,
            &sample,
            opts(PathBuf::from("/follower/checkout"), true).repair_mode(&mut worker),
            Path::new("/follower/checkout"),
        )
        .unwrap();
        assert_eq!(
            posted.state,
            if state == "pass" { "success" } else { "error" }
        );
        assert!(posted.verdict());
        assert!(world.caller_repairs.is_empty());
        assert!(world.worker_reads.is_empty());
        assert!(world.retired.is_empty());
        assert!(world.spawned.is_empty());
    }
}

/// A `verify` ROW THAT EXITED 0 WITH A SKIPPED CHECK COULD NOT RUN: appkit on a console locked
/// mid-pass is no green. NEGATIVE CONTROL: the same row with the check ok passes.
#[test]
fn a_verify_row_with_a_skipped_check_could_not_run() {
    let event = |outcome: &str| {
        format!(
            "verify-event: {}\n",
            aterm_json::json!({
                "event": "check-finished", "check": "appkit", "outcome": outcome,
                "seconds": 1.0, "detail": "", "rows": [], "rows_total": 1,
                "findings": [], "findings_total": 0, "rerun": "", "stopped": false,
            })
        )
    };
    let skip = event("skip");
    let mut skipped = row("appkit", Some(0), &skip);
    skip_is_no_pass(&mut skipped, &skip);
    assert_eq!(skipped.outcome, "could-not-run");
    assert!(
        skipped.log.ends_with("appkit skipped; a skip is no pass"),
        "{}",
        skipped.log
    );
    let ok = event("ok");
    let mut passed = row("appkit", Some(0), &ok);
    skip_is_no_pass(&mut passed, &ok);
    assert_eq!(passed.outcome, "pass");
}

#[test]
fn a_green_pass_and_a_pass_that_could_not_run_post_their_verdicts_and_start_no_worker() {
    let mut world = Scripted::default();
    let mut worker = None;
    let root = Path::new("/follower/checkout");
    let green = Sample {
        state: "pass".into(),
        rows: vec![row("inputs", Some(0), "ok"), row("tree", Some(0), "ok")],
    };
    let posted = conclude(
        &mut world,
        "tip",
        &[],
        true,
        &green,
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(posted.state, "success");
    assert!(
        posted.description.contains("inputs tree"),
        "{}",
        posted.description
    );
    let unavailable = Sample {
        state: "could-not-run".into(),
        rows: vec![could_not_run("follower", "git fetch: network".into())],
    };
    let posted = conclude(
        &mut world,
        "next",
        &[],
        true,
        &unavailable,
        RepairMode::Persistent(&mut worker),
        root,
    )
    .unwrap();
    assert_eq!(posted.state, "error");
    assert!(posted.description.contains("git fetch: network"));
    assert!(world.spawned.is_empty());
    assert!(
        posted.verdict(),
        "an error is a verdict: the tip is not judged again"
    );
    assert!(!Status::new("pending", "judging on m9".into()).verdict());
}

#[test]
fn a_description_fits_githubs_limit_and_names_its_worker() {
    let long = (0..40)
        .map(|n| format!("aterm-gui --lib settings::tests::case_{n}"))
        .collect::<Vec<_>>()
        .join("; ");
    let status = Status::new("failure", format!("40 red, repair tqn5r6e0148: {long}"));
    assert_eq!(status.description.chars().count(), DESCRIPTION_CHARS);
    assert!(status.description.ends_with('…'));
    assert_eq!(status.worker(), Some("tqn5r6e0148"));
    assert_eq!(
        Status::new("failure", "1 red, repair s-3f7db22fd790a94538cd: x".into()).worker(),
        Some("s-3f7db22fd790a94538cd"),
        "a worker started before the t form is still named"
    );
    assert_eq!(
        Status::new("failure", "1 red: x".into()).worker(),
        None,
        "a failure with no worker names none"
    );
}

/// Only the follower's own statuses are read, newest first as GitHub lists them. NEGATIVE
/// CONTROL: a reader that took every context would hand the release another tool's success.
#[test]
fn only_the_followers_context_is_read_from_a_commits_statuses() {
    let answer = aterm_json::json!([
        {"context": "ci/other", "state": "success", "description": "not ours"},
        {"context": CONTEXT, "state": "failure", "description": "1 red: x"},
        {"context": CONTEXT, "state": "pending", "description": "judging on m9"},
    ]);
    assert_eq!(
        follower_statuses(&answer),
        Some(vec![
            ("failure".to_owned(), "1 red: x".to_owned()),
            ("pending".to_owned(), "judging on m9".to_owned()),
        ])
    );
    assert_eq!(follower_statuses(&aterm_json::json!([])), Some(vec![]));
    assert_eq!(
        follower_statuses(&aterm_json::json!({"message": "Not Found"})),
        None
    );
}

#[test]
fn malformed_status_records_cannot_establish_the_absence_of_a_worker() {
    let own = aterm_json::json!({"context": CONTEXT, "state": "failure", "description": "1 red"});
    for malformed in [
        aterm_json::json!(null),
        aterm_json::json!("not a status"),
        aterm_json::json!({}),
        aterm_json::json!({"context": null}),
        aterm_json::json!({"context": ""}),
        aterm_json::json!({"context": CONTEXT, "state": "future", "description": "unknown"}),
        aterm_json::json!({"context": CONTEXT, "state": "failure"}),
        aterm_json::json!({"context": CONTEXT, "state": "failure", "description": null}),
        aterm_json::json!({"context": CONTEXT, "description": "missing state"}),
    ] {
        let answer = aterm_json::json!([own.clone(), malformed]);
        assert_eq!(follower_statuses(&answer), None, "{answer:?}");
    }
}

#[test]
fn all_returned_statuses_are_read_beyond_unrelated_page_sized_prefixes() {
    let mut rows = vec![
        aterm_json::json!({
            "context": "ci/other", "state": "success", "description": "not ours"
        });
        250
    ];
    rows.push(aterm_json::json!({
        "context": CONTEXT, "state": "failure",
        "description": "1 red, repair tqn5r6e0148: older worker"
    }));
    rows.push(
        aterm_json::json!({"context": CONTEXT, "state": "error", "description": "could not run"}),
    );
    let found = follower_statuses(&aterm_json::json!(rows)).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(
        Status::new(&found[0].0, found[0].1.clone()).worker(),
        Some("tqn5r6e0148")
    );
    assert_eq!(found[1], ("error".into(), "could not run".into()));
}
