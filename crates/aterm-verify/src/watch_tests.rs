// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeMap;

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
    spawned: Vec<String>,
    workers: BTreeMap<String, Worker>,
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
        self.statuses
            .entry(commit.to_owned())
            .or_default()
            .insert(0, status.clone());
        self.posted.push((commit.to_owned(), status.clone()));
        Ok(())
    }

    fn worker(&mut self, sid: &str) -> Worker {
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
        &red_sample("deadline::tests::a_child_ends"),
        &mut worker,
        root,
    )
    .unwrap();
    assert_eq!(first.state, "failure");
    assert_eq!(world.spawned.len(), 1, "a red starts one repair worker");
    let sid = worker.clone().expect("the worker's sid");
    assert!(
        first
            .description
            .contains("aterm-uds --lib deadline::tests::a_child_ends"),
        "{}",
        first.description
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
        world.statuses.get("landed").is_none(),
        "a commit the follower did not judge gets no status"
    );

    // A later red while that worker works: none started, the same one named.
    let second = conclude(
        &mut world,
        "red2",
        &["red1".into(), "landed".into(), "green".into()],
        &red_sample("deadline::tests::another"),
        &mut worker,
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
        &red_sample("deadline::tests::another"),
        &mut restarted,
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 1, "the restarted follower starts none");
    assert_eq!(restarted.as_deref(), Some(sid.as_str()));
    let task_of_tip = repair_task(
        "red3",
        &walk(&mut world, &["red2".into(), "red1".into(), "green".into()]),
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
        &walk(&mut world, &["red2".into(), "red1".into()]),
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
        &red_sample("deadline::tests::another"),
        &mut worker,
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 2);
    assert_ne!(worker.as_deref(), Some(sid.as_str()));
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
        &red_sample("deadline::tests::a_child_ends"),
        &mut worker,
        root,
    )
    .unwrap();
    let sid = worker.clone().unwrap();
    world.workers.insert(sid.clone(), Worker::Failed);
    let mut restarted = None;
    let same = conclude(
        &mut world,
        "red2",
        &["red1".into(), "green".into()],
        &red_sample("deadline::tests::a_child_ends"),
        &mut restarted,
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
        &red_sample("deadline::tests::another"),
        &mut restarted,
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 2, "new reds start a worker");
    assert_eq!(world.retired, [sid.clone()]);
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
        &red_sample("deadline::tests::a_child_ends"),
        &mut worker,
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
        &red_sample("deadline::tests::a_child_ends"),
        &mut worker,
        root,
    )
    .unwrap();
    assert_eq!(world.spawned.len(), 1);
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
    let posted = conclude(&mut world, "tip", &[], &green, &mut worker, root).unwrap();
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
    let posted = conclude(&mut world, "next", &[], &unavailable, &mut worker, root).unwrap();
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
