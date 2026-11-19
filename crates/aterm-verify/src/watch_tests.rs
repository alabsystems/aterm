// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeMap;

/// A CLAIM NAMES ITS CONTROLLER BY EXACT SELECTORS: that it is the follower's, its image and
/// process, and the node and session it runs in where those are well formed. NEGATIVE CONTROL:
/// a malformed node or session is left out, never written as given.
#[test]
fn a_claim_names_its_controller_by_exact_selectors() {
    let node = "1234567890abcdef1234567890abcdef";
    let session = "tqn5r6e0148";
    assert_eq!(
        controller_identity("abc", 7, Some(node), Some(session)),
        format!("follower sha256=abc pid=7 node={node} session={session}")
    );
    for (node, session) in [(None, None), (Some("not a node"), Some("not a session"))] {
        assert_eq!(
            controller_identity("abc", 7, node, session),
            "follower sha256=abc pid=7"
        );
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
    assert_eq!(
        stopped.state, "cancelled",
        "a pass that was stopped did not break"
    );
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
/// line here (its verdict is in the journal) and makes the next reason news again.
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
        log_line(&mut last, &declined("c0ffee: red on m9")).as_deref(),
        Some("verify watch: no pass started: c0ffee: red on m9")
    );
    assert_eq!(log_line(&mut last, &Ok(Start::Judged)), None);
    assert_eq!(
        log_line(&mut last, &declined("c0ffee: red on m9")).as_deref(),
        Some("verify watch: no pass started: c0ffee: red on m9"),
        "a pass ran in between, so the reason is news again"
    );
    // A refusal the journal repeats at every turn is one line, not one a minute.
    assert_eq!(
        log_line(&mut last, &Err("journal away".into())).as_deref(),
        Some("verify watch: pass could not run: journal away")
    );
    assert_eq!(log_line(&mut last, &Err("journal away".into())), None);
    assert_eq!(
        log_line(&mut last, &Err("journal back".into())).as_deref(),
        Some("verify watch: pass could not run: journal back")
    );
}

/// WHAT HOLDS A TIP IS SAID IN WORDS THAT DO NOT CHANGE FROM TURN TO TURN, so [`log_line`]
/// writes it once: a verdict, a claim with its lease's end, this Mac's own attempt that ended
/// with none and the time it rests to. NEGATIVE CONTROL: a reason counting seconds from now
/// would differ at each turn.
#[test]
fn a_declined_start_says_what_holds_the_tip() {
    let mut held = filed("c", "judging", None);
    held.lease_until = 2200;
    assert_eq!(
        declined("tip", &held),
        "tip: judging on m9, its lease to 2200"
    );
    held.state = "red".into();
    held.rows = red_sample("deadline::tests::a")
        .rows
        .iter()
        .map(journal_row)
        .collect();
    assert_eq!(
        declined("tip", &held),
        "tip: red on m9: 1 red: aterm-uds --lib deadline::tests::a"
    );
    held.state = "could-not-run".into();
    held.updated = 200;
    assert_eq!(
        declined("tip", &held),
        format!(
            "tip: this Mac's attempt ended could-not-run at 200; it is not claimed again before {}",
            200 + judged::RETRY_AFTER
        )
    );
}

fn opts(workspace: PathBuf, once: bool) -> Options {
    Options {
        workspace,
        remote: "origin".into(),
        branch: "main".into(),
        once,
        interval: Duration::from_secs(60),
        repair: false,
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

/// A scripted world: the workers started with where each stands, those asked after and those
/// retired, and what a one-shot caller was handed.
#[derive(Default)]
struct Scripted {
    caller_write_error: Option<String>,
    caller_repairs: Vec<String>,
    spawned: Vec<String>,
    workers: BTreeMap<String, Worker>,
    worker_reads: Vec<String>,
    retired: Vec<String>,
}

impl World for Scripted {
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

/// The follower's record on `commit`: `state`, carrying `repair`'s custody.
fn filed(commit: &str, state: &str, repair: Option<&Repair>) -> Record {
    let mut record = Record {
        commit: commit.into(),
        tree: format!("tree-of-{commit}"),
        host: "m9".into(),
        verifier: String::new(),
        controller: String::new(),
        inputs: String::new(),
        token: format!("token-of-{commit}"),
        state: state.into(),
        cause: String::new(),
        started: 100,
        updated: 100,
        lease_until: 0,
        judge: judged::FOLLOWER.into(),
        repair: String::new(),
        repair_assignment: String::new(),
        repair_started: 0,
        rows: Vec::new(),
    };
    if let Some(repair) = repair {
        repair.put(&mut record);
    }
    record
}

const ROOT: &str = "/follower/checkout";

/// [`conclude`] `sample` into a fresh claim of `tip`, the persistent follower's way.
fn persistent(
    world: &mut Scripted,
    worker: &mut Option<Repair>,
    tip: &str,
    before: &[Record],
    complete: bool,
    sample: &Sample,
) -> Record {
    let mut record = filed(tip, "judging", None);
    let earlier = walk(before, complete);
    conclude(
        world,
        &mut record,
        &earlier,
        sample,
        RepairMode::Persistent(worker),
        Path::new(ROOT),
        1000,
    )
    .unwrap();
    record
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
        Worker::Starting
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

/// A RED PASS FILES A RED THAT NAMES THE RED TEST AND STARTS AT MOST ONE REPAIR WORKER: a later
/// red pass while that worker works starts none and names the same worker, a restarted
/// follower finds it in the red it filed, and only once it has ended does a red start another,
/// the ended one retired. The worker is told to bisect between the red tip and the green one
/// over every parent. NEGATIVE CONTROL: the green before the reds carries no worker and starts
/// none.
#[test]
fn a_red_pass_files_a_red_naming_the_test_and_starts_at_most_one_repair_worker() {
    let mut world = Scripted::default();
    let mut worker = None;
    let sample = red_sample("deadline::tests::a");
    let green = filed("green", "pass", None);
    let first = persistent(
        &mut world,
        &mut worker,
        "r1",
        &[green.clone()],
        true,
        &sample,
    );
    assert_eq!(first.state, "red");
    assert_eq!(first.reds(), ["aterm-uds --lib deadline::tests::a"]);
    assert_eq!(
        first.rows.len(),
        3,
        "every row is filed, the green ones too"
    );
    assert_eq!(first.repair, "t0000000001");
    assert_eq!(first.repair_started, 1000);
    assert_eq!(
        first.repair_assignment,
        assignment(Some("green"), &sample.rows)
    );
    assert_eq!(world.spawned.len(), 1);
    assert!(world.spawned[0].contains("`git bisect start r1 green`"));
    assert!(world.spawned[0].contains("- tree: aterm-uds --lib deadline::tests::a"));
    assert!(world.spawned[0].contains("Could not finish: <why>"));
    assert!(!world.spawned[0].contains("bisect start --first-parent"));

    let before = [first.clone(), green.clone()];
    let second = persistent(&mut world, &mut worker, "r2", &before, true, &sample);
    assert_eq!(world.spawned.len(), 1, "the worker still works");
    assert_eq!(
        (second.repair.as_str(), second.repair_started),
        ("t0000000001", 1000)
    );

    let mut restarted = None;
    let before = [second, first, green];
    let third = persistent(&mut world, &mut restarted, "r3", &before, true, &sample);
    assert_eq!(world.spawned.len(), 1, "the journal names the worker");
    assert_eq!(restarted, Repair::of(&third));
    assert!(world.retired.is_empty());

    world.workers.insert("t0000000001".into(), Worker::Ended);
    let fourth = persistent(&mut world, &mut restarted, "r4", &before, true, &sample);
    assert_eq!(world.retired, ["t0000000001"]);
    assert_eq!(world.spawned.len(), 2);
    assert_eq!(fourth.repair, "t0000000002");
}

/// A WORKER THAT COULD NOT FINISH KEEPS THE SAME REDS, AND NEW REDS START ANOTHER: a fresh
/// worker on the reds that stopped the last would meet what stopped it. The reds it was given
/// are the hash its verdict carries, so the same red names after a later green are a new
/// assignment. NEGATIVE CONTROLS: a worker with no recorded assignment keeps its place whatever
/// the reds (nothing says what it tried); one that still works keeps it across a green.
#[test]
fn a_worker_that_could_not_finish_keeps_its_reds_and_new_reds_start_another() {
    let mut world = Scripted::default();
    let mut worker = None;
    let sample = red_sample("deadline::tests::a");
    let green = filed("g1", "pass", None);
    let first = persistent(
        &mut world,
        &mut worker,
        "r1",
        &[green.clone()],
        true,
        &sample,
    );
    world.workers.insert(first.repair.clone(), Worker::Failed);
    let before = [first.clone(), green.clone()];
    let same = persistent(&mut world, &mut worker, "r2", &before, true, &sample);
    assert_eq!(same.repair, first.repair);
    assert_eq!((world.spawned.len(), world.retired.len()), (1, 0));

    // A later green carries the custody, and the same red names after it are a new assignment.
    let mut restarted = None;
    let passed = Sample {
        state: "pass".into(),
        rows: vec![row("tree", Some(0), "ok")],
    };
    let g2 = persistent(&mut world, &mut restarted, "g2", &before, true, &passed);
    assert_eq!(
        (g2.state.as_str(), g2.repair.as_str()),
        ("pass", "t0000000001")
    );
    assert_eq!(world.worker_reads.len(), 1, "a pass asks after no worker");
    let after = [g2.clone(), first.clone(), green.clone()];
    let found = walk(&after, false);
    assert_eq!(found.green.as_deref(), Some("g2"));
    assert_eq!(found.repair, Repair::of(&first));
    assert!(
        found.complete,
        "a green closes the frontier, whatever lies before it"
    );
    let again = persistent(&mut world, &mut restarted, "r3", &after, true, &sample);
    assert_eq!(world.retired, ["t0000000001"]);
    assert_eq!(again.repair, "t0000000002");
    assert_ne!(again.repair_assignment, first.repair_assignment);

    // New reds at the same green start another too.
    world.workers.insert("t0000000002".into(), Worker::Failed);
    let other = red_sample("deadline::tests::b");
    let before = [again.clone(), g2.clone()];
    let new = persistent(&mut world, &mut restarted, "r4", &before, true, &other);
    assert_eq!(new.repair, "t0000000003");

    // Unknown assignment: custody holds, and nothing claims the worker tried these reds.
    let mut unknown = new.clone();
    unknown.repair_assignment = "not a hash".into();
    world.workers.insert("t0000000003".into(), Worker::Failed);
    let mut fresh = None;
    let before = [unknown, g2.clone()];
    let held = persistent(&mut world, &mut fresh, "r5", &before, true, &sample);
    assert_eq!(held.repair, "t0000000003");
    assert_eq!(held.repair_assignment, "");
    assert_eq!(world.spawned.len(), 3);

    // A worker that still works keeps its place across a green.
    world.workers.insert("t0000000003".into(), Worker::Working);
    let crossed = persistent(&mut world, &mut fresh, "r6", &[g2], true, &other);
    assert_eq!(crossed.repair, "t0000000003");
    assert_eq!(world.spawned.len(), 3);
}

/// CUSTODY THAT COULD NOT BE READ STARTS NO SECOND WORKER: where the journal or the history was
/// not read whole and no green closes the frontier, a worker no record read names may still
/// work, and an ended one that is named is kept, not replaced. NEGATIVE CONTROL: the same red
/// with the custody read whole starts one.
#[test]
fn an_unread_journal_starts_no_second_worker() {
    let mut world = Scripted::default();
    let sample = red_sample("deadline::tests::a");
    let mut worker = None;
    let unread = persistent(&mut world, &mut worker, "r1", &[], false, &sample);
    assert_eq!((unread.state.as_str(), unread.repair.as_str()), ("red", ""));
    assert!(world.spawned.is_empty());
    assert_eq!(Before::default(), walk(&[], false), "an unread journal");

    let ended = Repair {
        sid: "t0000000009".into(),
        assignment: None,
        started: 5,
    };
    let older = filed("r0", "red", Some(&ended));
    let kept = persistent(&mut world, &mut worker, "r2", &[older], false, &sample);
    assert_eq!(kept.repair, "t0000000009");
    assert!(world.spawned.is_empty() && world.retired.is_empty());

    let mut fresh = None;
    let read = persistent(&mut world, &mut fresh, "r3", &[], true, &sample);
    assert_eq!(read.repair, "t0000000001");
}

/// A ONE-SHOT RED RETURNS CONTEXT TO ITS CALLER AND MANAGES NO WORKER: it asks after none,
/// starts none, retires none, and its red carries no custody, so the worker an older red names
/// is still the one the next persistent follower finds beyond it. Its context is bounded, and
/// a caller that cannot be written to still leaves the red whole for the journal. NEGATIVE
/// CONTROL: the caller's context carries no worker protocol.
#[test]
fn a_one_shot_red_returns_bounded_context_and_keeps_older_custody() {
    let mut world = Scripted::default();
    let working = Repair {
        sid: "t0000000007".into(),
        assignment: Some("ab".repeat(32)),
        started: 5,
    };
    let before = [
        filed("r1", "red", Some(&working)),
        filed("green", "pass", None),
    ];
    let mut sample = red_sample("deadline::tests::a");
    sample.rows[1].red_items = (0..4000)
        .map(|n| format!("{n} {}", "長".repeat(40)))
        .collect();
    let mut record = filed("r2", "judging", None);
    let earlier = walk(&before, true);
    let mode = RepairMode::Caller;
    conclude(
        &mut world,
        &mut record,
        &earlier,
        &sample,
        mode,
        Path::new(ROOT),
        9,
    )
    .unwrap();
    assert_eq!((record.state.as_str(), record.repair.as_str()), ("red", ""));
    assert_eq!(record.reds().len(), 4000, "the journal keeps every red");
    assert!(world.worker_reads.is_empty() && world.spawned.is_empty());
    assert!(world.retired.is_empty());
    let [context] = world.caller_repairs.as_slice() else {
        panic!("one context, got {}", world.caller_repairs.len());
    };
    assert!(context.starts_with("Use your harness's native subagents"));
    assert!(context.contains("`git bisect start r2 green`"));
    assert!(context.contains("The oldest red tip since then is r1"));
    assert!(context.contains("More red details remain in this pass's row logs."));
    assert!(!context.contains("Could not finish"));
    assert!(
        context.len() < MAX_REPAIR_CONTEXT_BYTES + 256,
        "{}",
        context.len()
    );

    let after = [record, before[0].clone(), before[1].clone()];
    let found = walk(&after, true);
    assert_eq!(found.repair, Some(working));
    assert_eq!(found.oldest_red.as_deref(), Some("r1"));

    world.caller_write_error = Some("stdout closed".into());
    let mut unwritten = filed("r3", "judging", None);
    let small = red_sample("deadline::tests::a");
    let mode = RepairMode::Caller;
    let written = conclude(
        &mut world,
        &mut unwritten,
        &found,
        &small,
        mode,
        Path::new(ROOT),
        9,
    );
    assert_eq!(written, Err("caller repair output: stdout closed".into()));
    assert_eq!(unwritten.state, "red");
    assert_eq!(unwritten.reds(), ["aterm-uds --lib deadline::tests::a"]);
}

/// ONLY A WELL-FORMED RECORD ESTABLISHES CUSTODY: a session that is no tab names no worker, and
/// an assignment that is no hash leaves the worker's assignment unknown, never guessed.
#[test]
fn custody_is_read_from_a_records_fields_and_never_guessed() {
    let mut record = filed("r1", "red", None);
    assert_eq!(Repair::of(&record), None);
    record.repair = "repair the tree".into();
    assert_eq!(Repair::of(&record), None);
    record.repair = "t0000000001".into();
    record.repair_assignment = "AB".repeat(32);
    record.repair_started = 7;
    let unknown = Repair::of(&record).unwrap();
    assert_eq!((unknown.assignment.as_deref(), unknown.started), (None, 7));
    record.repair_assignment = "ab".repeat(32);
    let known = Repair::of(&record).unwrap();
    assert_eq!(known.assignment, Some("ab".repeat(32)));
    let mut copy = filed("r2", "red", None);
    known.put(&mut copy);
    assert_eq!(Repair::of(&copy), Some(known));
}

/// A GREEN PASS AND A PASS THAT COULD NOT RUN FILE THEIR OWN WORDS AND START NO WORKER: a pass
/// with every row, a pass that could not run with the row that stopped it as its cause.
#[test]
fn a_green_pass_and_a_pass_that_could_not_run_start_no_worker() {
    let mut world = Scripted::default();
    let mut worker = None;
    let passed = Sample {
        state: "pass".into(),
        rows: vec![row("inputs", Some(0), "ok"), row("tree", Some(0), "ok")],
    };
    let green = persistent(&mut world, &mut worker, "g", &[], true, &passed);
    assert_eq!(green.summary(), "pass on m9: inputs tree");
    assert_eq!(green.repair, "");
    let unavailable = Sample {
        state: "could-not-run".into(),
        rows: vec![
            row("inputs", Some(0), "ok"),
            could_not_run("tree", "first\ncompiler lease: held\n".into()),
        ],
    };
    let ended = persistent(&mut world, &mut worker, "u", &[], true, &unavailable);
    assert_eq!(ended.state, "could-not-run");
    assert_eq!(ended.cause, "could not run tree: compiler lease: held");
    assert_eq!(
        ended.summary(),
        "could-not-run on m9: could not run tree: compiler lease: held"
    );
    assert!(world.worker_reads.is_empty() && world.spawned.is_empty());
    assert!(!ended.occupied(1), "the tip is judged again");
}

/// A repository with a committer, in a new folder `name` of `dir`.
fn repository(dir: &Path, name: &str, extra: &[&str]) -> PathBuf {
    let mut init = vec!["init", "--quiet", "-b", "main"];
    init.extend_from_slice(extra);
    init.push(name);
    git(dir, &init).unwrap();
    let root = dir.join(name);
    git(&root, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(&root, &["config", "user.email", "test@example.invalid"]).unwrap();
    git(&root, &["config", "maintenance.auto", "false"]).unwrap();
    root
}

/// Commit a file `name` in `root`: the commit.
fn landed(root: &Path, name: &str) -> String {
    fs::write(root.join(name), name).unwrap();
    git(root, &["add", name]).unwrap();
    git(root, &["commit", "--quiet", "-m", name]).unwrap();
    git(root, &["rev-parse", "HEAD"]).unwrap()
}

/// THE TIP SEQUENCE IS THE JOURNAL'S VERDICTS ORDERED BY ANCESTRY OVER EVERY PARENT. A landing
/// merges main into its branch and pushes the merge: its first parent is the branch, its second
/// the tip before it. Here t0 was judged green and t1 red with a worker, and t2 is such a merge
/// of t1. The sequence before t2 is t1 then t0, whatever order the journal lists them in, and
/// it leaves out the tip itself, a claim, a record that is not the follower's and a record on a
/// commit that is no ancestor. NEGATIVE CONTROL: t1 is not on t2's first-parent chain, and the
/// same records ordered by that chain lose it: the walk then reads t0 as the green with no red
/// since and no worker, and a second worker would start beside the first.
#[test]
fn the_tip_sequence_is_ancestry_over_every_parent_not_the_first_parent_chain() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = repository(dir.path(), "main", &[]);
    let t0 = landed(&root, "t0");
    git(&root, &["checkout", "--quiet", "-b", "work"]).unwrap();
    let branch = landed(&root, "work");
    git(&root, &["checkout", "--quiet", "main"]).unwrap();
    let t1 = landed(&root, "t1");
    git(&root, &["checkout", "--quiet", "work"]).unwrap();
    git(
        &root,
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "Merge main into work",
            "main",
        ],
    )
    .unwrap();
    let t2 = git(&root, &["rev-parse", "HEAD"]).unwrap();
    let chain = git(&root, &["rev-list", "--first-parent", &t2]).unwrap();
    assert_eq!(chain.lines().collect::<Vec<_>>(), [&t2, &branch, &t0]);

    let working = Repair {
        sid: "t0000000001".into(),
        assignment: Some("ab".repeat(32)),
        started: 5,
    };
    let mut lane = filed(&branch, "pass", None);
    lane.judge = String::new();
    let records = vec![
        filed(&t0, "pass", None),
        filed(&t2, "red", None),
        filed("f00d", "pass", None),
        lane,
        filed(&branch, "judging", None),
        filed(&t1, "red", Some(&working)),
    ];
    let listed = git(&root, &["rev-list", "--topo-order", &t2]).unwrap();
    let before = sequence(records.clone(), &t2, &listed);
    let commits: Vec<&str> = before.iter().map(|record| record.commit.as_str()).collect();
    assert_eq!(commits, [&t1, &t0]);
    let found = walk(&before, true);
    assert_eq!(found.green.as_deref(), Some(t0.as_str()));
    assert_eq!(found.oldest_red.as_deref(), Some(t1.as_str()));
    assert_eq!(found.repair, Some(working));

    let lost = walk(&sequence(records, &t2, &chain), true);
    assert_eq!(lost.green.as_deref(), Some(t0.as_str()));
    assert_eq!((lost.oldest_red, lost.repair), (None, None));
}

/// A judgment's wait, looked at `looks` more times, thirty seconds apart by its own clock, and
/// then ended clean, unless `signal` reaches its controller first. No real time passes.
struct Hours {
    at: Instant,
    epoch: u64,
    looks: u64,
    signal: Option<i32>,
    stopped: bool,
}

impl Hours {
    fn of(looks: u64) -> Self {
        Self {
            at: Instant::now(),
            epoch: 10_000,
            looks,
            signal: None,
            stopped: false,
        }
    }
}

impl Attended for Hours {
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
        self.signal
    }

    fn now(&self) -> (Instant, u64) {
        (self.at, self.epoch)
    }

    fn pause(&mut self) {
        self.at += Duration::from_secs(30);
        self.epoch += 30;
    }
}

/// A CLAIM IS RENEWED EVERY TEN MINUTES WHILE ITS PASS RUNS, AND A RENEWAL THE JOURNAL DID NOT
/// TAKE IS TRIED AGAIN A MINUTE LATER AND ENDS NOTHING. An hour's pass renews five times, each
/// moving the lease twenty minutes past its own time, so one renewal may fail before the lease
/// lapses. A renewal displaced by another attempt waits the whole ten minutes. The failures
/// are counted for the attempt's last record. NEGATIVE CONTROLS: a claim renewed at every look
/// would write 119 journal commits an hour; a controller's signal stops the pass and renews
/// nothing more.
#[test]
fn a_claim_is_renewed_every_ten_minutes_and_a_failed_renewal_is_tried_again_sooner() {
    let hour = 120;
    let mut record = filed("c", "judging", None);
    let mut renewed = Vec::new();
    let (ended, evidence) = attend(&mut record, &mut Hours::of(hour), &mut |record, epoch| {
        renewed.push((epoch, record.lease_until));
        Ok(())
    })
    .unwrap();
    assert!(matches!(ended, Ended::Exited(status) if status.success()));
    assert_eq!(evidence, None);
    let every: Vec<_> = [10_600, 11_200, 11_800, 12_400, 13_000]
        .map(|at| (at, at + judged::LEASE))
        .into();
    assert_eq!(renewed, every);
    assert_eq!(record.lease_until, 13_000 + judged::LEASE);

    let mut tried = Vec::new();
    let (_, evidence) = attend(&mut record, &mut Hours::of(hour), &mut |_, epoch| {
        tried.push(epoch);
        match tried.len() {
            1 => Err(Unfiled::Unavailable("journal away".into())),
            3 => Err(Unfiled::Displaced("judging by m45".into())),
            _ => Ok(()),
        }
    })
    .unwrap();
    assert_eq!(tried, [10_600, 10_660, 11_260, 11_860, 12_460, 13_060]);
    assert_eq!(
        evidence.as_deref(),
        Some(
            "claim renewals that failed: 2; the last: another attempt holds this tree: judging by m45"
        )
    );

    let mut signalled = Hours::of(hour);
    signalled.signal = Some(15);
    let (ended, _) = attend(&mut record, &mut signalled, &mut |_, _| {
        panic!("a stopped pass renews nothing")
    })
    .unwrap();
    assert!(matches!(ended, Ended::Interrupted(15)));
    assert!(signalled.stopped);
}

/// A main of one commit in a bare remote, and a follower's clone of it for each of `followers`:
/// the folder that holds them, main's tip, its tree, and the clones.
fn followed(followers: &[&str]) -> (aterm_tempfile::TempDir, String, String, Vec<PathBuf>) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let remote = repository(dir.path(), "remote", &["--bare"]);
    let source = repository(dir.path(), "source", &[]);
    let tip = landed(&source, "tip");
    let tree = git(&source, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let url = remote.to_str().unwrap();
    git(&source, &["push", "--quiet", url, "main"]).unwrap();
    let roots = followers
        .iter()
        .map(|name| {
            let root = repository(dir.path(), name, &[]);
            git(&root, &["remote", "add", "origin", url]).unwrap();
            root
        })
        .collect();
    (dir, tip, tree, roots)
}

/// What `tree`'s slot holds in the journal `root` follows.
fn slot(root: &Path, tree: &str) -> Option<Record> {
    let head = judged::head(root, ORIGIN, judged::deadline()).unwrap();
    judged::record(root, head.as_deref(), tree, judged::deadline()).unwrap()
}

/// A pass no declined follower may start.
fn no_pass(_: &Path, _: &mut Record) -> Result<(Judged, Option<String>), String> {
    panic!("a follower that was declined judges nothing")
}

/// A PASS FILES `judging` AND THEN ITS VERDICT AT THE TIP'S TREE'S SLOT, AND A SECOND FOLLOWER
/// ON THE SAME TIP IS DECLINED WHILE THAT LEASE IS LIVE. While the first follower's pass runs,
/// the slot holds its claim with a twenty-minute lease, marked the follower's; a second
/// follower one second before the lease's end starts nothing and writes nothing. The pass ends
/// green: the slot holds `pass` with the rows, the journal took exactly those two commits, the
/// checkout keeps no record, and the tip is judged for every later follower. NEGATIVE CONTROL:
/// the second follower's turn would panic had it started a pass.
#[test]
fn a_pass_files_judging_then_its_verdict_and_a_second_follower_is_declined() {
    let (_dir, tip, tree, roots) = followed(&["one", "two"]);
    let two = opts(roots[1].clone(), true);
    let mut second = None;
    let mut pass = |root: &Path, record: &mut Record| {
        let held = slot(root, &tree).unwrap();
        assert_eq!(held, *record, "the claim is in the journal before the pass");
        assert_eq!(
            (held.state.as_str(), held.judge.as_str()),
            ("judging", "follower")
        );
        assert_eq!(held.lease_until, 1000 + judged::LEASE);
        let mut world = Scripted::default();
        let late = || 1000 + judged::LEASE - 1;
        second = Some(attempt_with(
            &two,
            &mut world,
            &mut None,
            &late,
            &|| None,
            &mut no_pass,
        ));
        assert_eq!(slot(root, &tree), Some(held));
        let rows = vec![row("inputs", Some(0), "ok"), row("tree", Some(0), "ok")];
        let state = "pass".into();
        Ok((Judged::Sample(Sample { state, rows }), None))
    };
    let mut world = Scripted::default();
    let one = opts(roots[0].clone(), true);
    let judged = attempt_with(&one, &mut world, &mut None, &|| 1000, &|| None, &mut pass);
    assert_eq!(judged, Ok(Start::Judged));
    let why = format!(
        "{tip}: judging on {}, its lease to {}",
        host(),
        1000 + judged::LEASE
    );
    assert_eq!(second, Some(Ok(Start::Declined(why))));

    let filed = slot(&roots[0], &tree).unwrap();
    assert_eq!(filed.summary(), format!("pass on {}: inputs tree", host()));
    assert_eq!(
        (filed.commit.as_str(), filed.lease_until),
        (tip.as_str(), 0)
    );
    let head = judged::head(&roots[0], ORIGIN, judged::deadline())
        .unwrap()
        .unwrap();
    assert_eq!(
        git(&roots[0], &["log", "--format=%s", &head]).unwrap(),
        format!("verify watch: pass {tip}\nverify watch: judging {tip}")
    );
    assert!(!saved(&roots[0]).exists());
    let later = attempt_with(
        &two,
        &mut world,
        &mut None,
        &|| 99_999,
        &|| None,
        &mut no_pass,
    );
    let why = format!("{tip}: {}", filed.summary());
    assert_eq!(later, Ok(Start::Declined(why)));
    assert!(world.caller_repairs.is_empty() && world.spawned.is_empty());
}

/// ANOTHER MAC'S CLAIM KEEPS THIS FOLLOWER OUT WHILE ITS LEASE IS LIVE, AND IS TAKEN ONCE IT
/// LAPSED. m45 claimed the tip at 1000 and went away. One second before its lease ends this
/// follower starts nothing; at its end it claims the tip, judges it red, and files the red
/// with its rows in the slot, in place of the lapsed claim. Being one-shot it hands the repair
/// context to its caller and starts no agent session. NEGATIVE CONTROL: the first turn is the
/// same follower one second earlier.
#[test]
fn another_macs_claim_is_taken_only_after_its_lease_lapsed() {
    let (_dir, tip, tree, roots) = followed(&["one"]);
    let root = &roots[0];
    git(root, &["fetch", "--quiet", ORIGIN, "main"]).unwrap();
    let mut gone = filed(&tip, "judging", None);
    gone.tree.clone_from(&tree);
    gone.host = "m45".into();
    gone.lease_until = 1000 + judged::LEASE;
    assert_eq!(
        judged::claim(root, ORIGIN, &gone, 1000, judged::deadline()),
        Ok(None)
    );
    let one = opts(root.clone(), true);
    let mut world = Scripted::default();
    let live = || 1000 + judged::LEASE - 1;
    let early = attempt_with(&one, &mut world, &mut None, &live, &|| None, &mut no_pass);
    let why = format!(
        "{tip}: judging on m45, its lease to {}",
        1000 + judged::LEASE
    );
    assert_eq!(early, Ok(Start::Declined(why)));
    assert_eq!(slot(root, &tree), Some(gone.clone()));

    let mut red =
        |_: &Path, _: &mut Record| Ok((Judged::Sample(red_sample("deadline::tests::a")), None));
    let lapsed = || 1000 + judged::LEASE;
    let taken = attempt_with(&one, &mut world, &mut None, &lapsed, &|| None, &mut red);
    assert_eq!(taken, Ok(Start::Judged));
    let filed = slot(root, &tree).unwrap();
    assert_eq!(filed.state, "red");
    assert_eq!(filed.host, host());
    assert_ne!(filed.token, gone.token);
    assert_eq!(filed.reds(), ["aterm-uds --lib deadline::tests::a"]);
    assert_eq!(filed.rows.len(), 3);
    assert_eq!(filed.repair, "");
    assert_eq!(world.caller_repairs.len(), 1);
    assert!(world.caller_repairs[0].contains(&format!("`git bisect start {tip}`")));
    assert!(world.spawned.is_empty() && world.worker_reads.is_empty());
}

/// WHAT AN ATTEMPT LEFT REACHES THE JOURNAL BEFORE ANYTHING NEW IS CLAIMED. A follower that
/// ended while judging left its claim in the journal and in its checkout: the next loop closes
/// it as could-not-run, and this Mac then leaves the tip alone for [`judged::RETRY_AFTER`]. A
/// last record whose filing failed stays in the checkout, and is filed once the journal can be
/// reached. NEGATIVE CONTROLS: the claim is closed by its own token, not by a new attempt's;
/// the unreachable journal leaves the kept record where it was.
#[test]
fn a_claim_its_follower_left_is_closed_and_a_last_record_is_filed_later() {
    let (dir, tip, tree, roots) = followed(&["one"]);
    let root = &roots[0];
    let mut claim = filed(&tip, "judging", None);
    claim.tree.clone_from(&tree);
    claim.host = host();
    claim.lease_until = 1000 + judged::LEASE;
    assert_eq!(
        judged::claim(root, ORIGIN, &claim, 1000, judged::deadline()),
        Ok(None)
    );
    save(root, &claim).unwrap();
    settle(root, 1500).unwrap();
    let closed = slot(root, &tree).unwrap();
    assert_eq!(
        (closed.state.as_str(), &closed.token),
        ("could-not-run", &claim.token)
    );
    assert_eq!(
        closed.cause,
        "the follower ended before this attempt had a last record"
    );
    assert!(!saved(root).exists());
    let one = opts(root.clone(), true);
    let rest = || 1500 + judged::RETRY_AFTER - 1;
    let rested = attempt_with(
        &one,
        &mut Scripted::default(),
        &mut None,
        &rest,
        &|| None,
        &mut no_pass,
    );
    let why = format!(
        "{tip}: this Mac's attempt ended could-not-run at 1500; it is not claimed again before {}",
        1500 + judged::RETRY_AFTER
    );
    assert_eq!(rested, Ok(Start::Declined(why)));

    let mut last = filed("0ther", "red", None);
    last.updated = 2000;
    save(root, &last).unwrap();
    let url = git(root, &["remote", "get-url", ORIGIN]).unwrap();
    let away = dir.path().join("away");
    git(root, &["remote", "set-url", ORIGIN, away.to_str().unwrap()]).unwrap();
    assert!(settle(root, 2000).is_err());
    assert!(saved(root).exists(), "kept until the journal holds it");
    git(root, &["remote", "set-url", ORIGIN, &url]).unwrap();
    settle(root, 2100).unwrap();
    assert_eq!(slot(root, "tree-of-0ther"), Some(last));
    assert!(!saved(root).exists());
}

/// NO CODE PATH OF THE FOLLOWER CALLS GITHUB'S STATUSES API, OR ANY GITHUB API: its sources
/// name no statuses path and start no agh API call; the journal is its one store, reached
/// through the verifier's remote transport. The vault's development token may not write a
/// commit status (403), which is why a follower that posted them judged nothing. NEGATIVE
/// CONTROL: the follower before this named both `commits/{sha}/statuses` and
/// `statuses/{sha}` and ran `agh api`.
#[test]
fn no_code_path_of_the_follower_reads_or_posts_a_commit_status() {
    for (name, source) in [
        ("watch.rs", include_str!("watch.rs")),
        ("watch_service.rs", include_str!("watch_service.rs")),
        ("judged.rs", include_str!("judged.rs")),
    ] {
        for call in [
            "/statuses",
            "statuses/",
            "Tool::Agh",
            "\"api\"",
            "aterm/follower",
        ] {
            assert!(!source.contains(call), "{name} names {call}");
        }
    }
}

/// WITH REPAIRS OFF A RED IS FILED AND NOTHING OPENS, ASKS AFTER OR ENDS AN AGENT SESSION. A
/// persistent follower is off unless `--repair worker` was given. Its red carries the rows and
/// no custody, though an older verdict names a worker that has ended: that worker is not
/// retired, not asked after and not replaced, and its custody stays in the journal for a
/// follower that is asked to repair. A green carries that custody on. NEGATIVE CONTROL: the
/// same red from a follower with `--repair worker` retires the ended worker and starts one.
#[test]
fn with_repairs_off_a_red_is_filed_and_no_session_is_started() {
    let dir = PathBuf::from("/w");
    let mut off = opts(dir.clone(), false);
    assert!(matches!(off.repair_mode(&mut None), RepairMode::Off));
    off.repair = true;
    assert!(matches!(
        off.repair_mode(&mut None),
        RepairMode::Persistent(_)
    ));
    off.once = true;
    assert!(matches!(off.repair_mode(&mut None), RepairMode::Caller));

    let ended = Repair {
        sid: "t0000000009".into(),
        assignment: Some("ab".repeat(32)),
        started: 5,
    };
    let before = [filed("r1", "red", Some(&ended)), filed("g", "pass", None)];
    let earlier = walk(&before, true);
    let sample = red_sample("deadline::tests::a");
    let mut world = Scripted::default();
    let mut record = filed("r2", "judging", None);
    let root = Path::new(ROOT);
    conclude(
        &mut world,
        &mut record,
        &earlier,
        &sample,
        RepairMode::Off,
        root,
        9,
    )
    .unwrap();
    assert_eq!((record.state.as_str(), record.repair.as_str()), ("red", ""));
    assert_eq!(record.reds(), ["aterm-uds --lib deadline::tests::a"]);
    assert_eq!(record.rows.len(), 3);
    assert!(world.spawned.is_empty() && world.retired.is_empty());
    assert!(world.worker_reads.is_empty() && world.caller_repairs.is_empty());
    let after = [record, before[0].clone(), before[1].clone()];
    assert_eq!(walk(&after, true).repair, Some(ended.clone()));

    let passed = Sample {
        state: "pass".into(),
        rows: vec![row("tree", Some(0), "ok")],
    };
    let mut green = filed("g2", "judging", None);
    let earlier = walk(&after, true);
    conclude(
        &mut world,
        &mut green,
        &earlier,
        &passed,
        RepairMode::Off,
        root,
        9,
    )
    .unwrap();
    assert_eq!(Repair::of(&green), Some(ended));
    assert!(world.worker_reads.is_empty());

    let mut worker = None;
    let asked = persistent(&mut world, &mut worker, "r3", &after, true, &sample);
    assert_eq!(world.retired, ["t0000000009"]);
    assert_eq!(asked.repair, "t0000000001");
}

/// A WORKER STALLED AT ITS START HOLDS THE REPAIR PLACE FOR FIFTEEN MINUTES, NOT FOR EVER. One
/// second inside that time from its start a red keeps it; at the fifteenth minute it is
/// retired and another started. NEGATIVE CONTROLS: a worker that works keeps its place however
/// long ago it started, and so does one stalled where it resumes by itself.
#[test]
fn a_worker_stalled_at_its_start_holds_the_place_for_a_bounded_time() {
    let sample = red_sample("deadline::tests::a");
    let started = Repair {
        sid: "t0000000009".into(),
        assignment: None,
        started: 1000,
    };
    let before = [filed("r1", "red", Some(&started))];
    let earlier = walk(&before, true);
    let red_at = |world: &mut Scripted, epoch: u64| {
        let mut record = filed("r2", "judging", None);
        let mut worker = None;
        let mode = RepairMode::Persistent(&mut worker);
        conclude(
            world,
            &mut record,
            &earlier,
            &sample,
            mode,
            Path::new(ROOT),
            epoch,
        )
        .unwrap();
        record.repair
    };
    let mut world = Scripted::default();
    world.workers.insert("t0000000009".into(), Worker::Starting);
    assert_eq!(red_at(&mut world, 1000 + STARTUP_STALL - 1), "t0000000009");
    assert!(world.spawned.is_empty() && world.retired.is_empty());
    assert_eq!(red_at(&mut world, 1000 + STARTUP_STALL), "t0000000001");
    assert_eq!(world.retired, ["t0000000009"]);

    let mut world = Scripted::default();
    world.workers.insert("t0000000009".into(), Worker::Working);
    assert_eq!(
        red_at(&mut world, 1000 + 100 * STARTUP_STALL),
        "t0000000009"
    );
    assert!(world.spawned.is_empty() && world.retired.is_empty());
}

/// One claim in the capacity ledger at `rank`.
fn capacity_claim(rank: aterm_capacity::Rank, held: bool) -> aterm_capacity::Record {
    aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::Holder,
        pid: 41,
        start_ms: 1,
        laid_at: None,
        seq: 1,
        checkout: "/release/aterm".into(),
        need: aterm_capacity::Need::new("ship release").rank(rank).mem(1),
        timing: aterm_capacity::TimingEvidence::default(),
        held: held.then_some(aterm_capacity::Held {
            admitted_ms: 2,
            cores: 0,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

/// A RELEASE GOES FIRST: NO PASS STARTS UNDER A RELEASE CLAIM, AND NOTHING IS CLAIMED. While
/// the capacity ledger holds a Release claim, waiting or admitted, or cannot be read, a turn
/// fetches nothing, files no claim in the journal and judges nothing; ordinary and background
/// claims stop no start. NEGATIVE CONTROLS: the same turn with no release claims the tip; an
/// unread ledger is never read as an empty one.
#[test]
fn a_release_claim_starts_no_pass_and_nothing_is_claimed() {
    use aterm_capacity::Rank;
    let why = "Release claim pid=41 what=\"ship release\" checkout=\"/release/aterm\"";
    for held in [false, true] {
        let ledger = vec![
            capacity_claim(Rank::Ordinary, true),
            capacity_claim(Rank::Release, held),
        ];
        assert_eq!(release_holds(Some(Ok(ledger))).as_deref(), Some(why));
    }
    let others = vec![
        capacity_claim(Rank::Ordinary, true),
        capacity_claim(Rank::Background, true),
    ];
    assert_eq!(release_holds(Some(Ok(others))), None);
    assert_eq!(release_holds(Some(Ok(Vec::new()))), None);
    let unread = release_holds(Some(Err(std::io::Error::other("locked")))).unwrap();
    assert_eq!(unread, "the capacity ledger could not be read: locked");
    assert!(release_holds(None).is_some());

    let (_dir, _tip, tree, roots) = followed(&["one"]);
    let one = opts(roots[0].clone(), true);
    let mut world = Scripted::default();
    let release = || Some(why.to_owned());
    let turn = attempt_with(
        &one,
        &mut world,
        &mut None,
        &|| 1000,
        &release,
        &mut no_pass,
    );
    assert_eq!(
        turn,
        Ok(Start::Declined(format!("a release goes first: {why}")))
    );
    assert_eq!(
        judged::head(&roots[0], ORIGIN, judged::deadline()),
        Ok(None)
    );
    let mut pass = |_: &Path, _: &mut Record| {
        let rows = vec![row("tree", Some(0), "ok")];
        let state = "pass".into();
        Ok((Judged::Sample(Sample { state, rows }), None))
    };
    let turn = attempt_with(&one, &mut world, &mut None, &|| 1000, &|| None, &mut pass);
    assert_eq!(turn, Ok(Start::Judged));
    assert_eq!(slot(&roots[0], &tree).unwrap().state, "pass");
}

/// A PASS HOLDS ONE BACKGROUND CLAIM THAT COVERS ITS ROWS' BUILDS, AND A STOPPED ADMISSION IS A
/// STOP. The claim is background work's (a release ends its holder's group; ordinary work is
/// served first), covers the builds below it with 26 GiB, and asks one core. An admission that
/// a release or an interrupt stopped ends the pass with no row and no error, and a pass that
/// was stopped is filed cancelled, never as one that could not run or as a verdict. NEGATIVE
/// CONTROLS: a ledger that failed is an error, not a stop; a pass that broke (exit 3) could
/// not run.
#[test]
fn a_pass_holds_one_background_claim_and_a_stopped_pass_is_cancelled() {
    use std::os::unix::process::ExitStatusExt as _;
    let need = pass_need();
    assert_eq!(need.rank, aterm_capacity::Rank::Background);
    assert!(need.covers);
    assert_eq!((need.mem, need.cores), (26 << 30, 1));
    let stopped = aterm_capacity::Error::Stopped {
        signal: None,
        short: None,
    };
    assert!(matches!(pass_claim(Err(stopped)), Ok(None)));
    let failed = aterm_capacity::Error::Ledger(std::io::Error::other("no ledger"));
    let why = pass_claim(Err(failed)).map(drop).unwrap_err();
    assert!(why.starts_with("capacity: "), "{why}");

    let preempted = stopped_sample(std::process::ExitStatus::from_raw(75 << 8), "row=tree");
    assert_eq!(preempted.state, "cancelled");
    let mut world = Scripted::default();
    let mut worker = None;
    let record = persistent(&mut world, &mut worker, "c", &[], true, &preempted);
    assert_eq!(record.state, "cancelled");
    assert!(!record.verdict() && !record.occupied(1));
    assert!(record.resting("m9", record.updated + judged::RETRY_AFTER - 1));
    let broke = stopped_sample(std::process::ExitStatus::from_raw(3 << 8), "row=tree");
    assert_eq!(broke.state, "could-not-run");
    let full = "watch row=tree stopped: code=Some(75) kind=environment condition=\"no space\"";
    let failed = stopped_sample(std::process::ExitStatus::from_raw(75 << 8), full);
    assert_eq!(failed.state, "could-not-run", "a full disk is no stop");
}

/// A GIT STEP THAT MOVES OBJECTS IS BOUNDED BY PROGRESS, NOT BY A WALL CLOCK, AND A SEED THAT
/// WAS STOPPED HALF WAY IS MADE AGAIN. A step that spins on its CPU, starting no process, until
/// the test tells it to stop three stall windows in ends well, since its tree's CPU time
/// advanced the whole time; the seed clone took 38.8 to 67.8 s under a 60 s wall bound. What a
/// stopped seed left beside the workspace is cleared, and the workspace appears whole, with its
/// owner's mark, or not at all. NEGATIVE CONTROLS: a step parked in `sleep` for
/// [`aterm_spec::wait::PARKED_SECS`] is ended by the stall window, whose error names that
/// window, so a wall bound that ends it instead fails; the window must be judged before the
/// park ends; a folder at the workspace's own path that this tool did not make is still
/// refused and kept.
#[test]
fn a_seed_waits_by_progress_and_a_half_made_seed_is_made_again() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = repository(dir.path(), "source", &[]);
    landed(&source, "seed");
    git(
        &source,
        &["remote", "add", "origin", "https://invalid.invalid/absent"],
    )
    .unwrap();
    let window = Duration::from_secs(1);
    let told = dir.path().join("told");
    let busy = format!(
        "alias.work=!while [ ! -e '{}' ]; do :; done; echo worked",
        told.display()
    );
    let worked = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(window * 3);
            fs::write(&told, "").unwrap();
        });
        git_working_within(&source, &["-c", &busy, "work"], window)
    });
    assert_eq!(worked.as_deref(), Ok("worked"));
    let parked = format!("alias.work=!exec sleep {}", aterm_spec::wait::PARKED_SECS);
    let idle = git_working_within(&source, &["-c", &parked, "work"], window).unwrap_err();
    assert!(
        idle.contains(&format!("progress for {window:?} while")),
        "{idle}"
    );
    let failed = git_working(&source, &["rev-parse", "--verify", "absent"]).unwrap_err();
    assert!(
        failed.starts_with("git rev-parse --verify absent: "),
        "{failed}"
    );

    let workspace = dir.path().join("lane");
    let seed = seeding(&workspace).unwrap();
    assert_eq!(seed, dir.path().join("lane.seeding"));
    fs::create_dir_all(seed.join(".git")).unwrap();
    fs::write(seed.join("half"), "a clone that was stopped").unwrap();
    let root = prepare_workspace(&opts(workspace.clone(), false), &source).unwrap();
    assert_eq!(git(&root, &["show", "HEAD:seed"]).unwrap(), "seed");
    assert!(!root.join("half").exists() && !seed.exists());
    assert_eq!(
        fs::read_to_string(root.join(".git/verify-watch-owner")).unwrap(),
        "https://invalid.invalid/absent"
    );

    let foreign = dir.path().join("theirs");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("work"), "kept").unwrap();
    let refused = prepare_workspace(&opts(foreign.clone(), false), &source).unwrap_err();
    assert!(refused.contains("not created by verify watch"), "{refused}");
    assert_eq!(fs::read_to_string(foreign.join("work")).unwrap(), "kept");
}
