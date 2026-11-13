// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn state(node: &str, at: u64) -> State {
    State {
        line: crate::line::Line::start("0.156.0", "base", 1),
        holder: Holder {
            node: node.into(),
            host: node.into(),
            sid: node.into(),
            pid: 1,
            boot: "boot".into(),
            start: 1,
        },
        step: "verify".into(),
        boundary: "test-binary-finished".into(),
        at,
        p95_s: 0,
        rows: Vec::new(),
    }
}

/// The exact holder survives native exec, and the authenticated pre-B1 parent may cross the
/// one child-process deployment boundary. NEGATIVE CONTROLS: without local continuation or
/// the independently live parent proof, and with another node, session or boot, the flag
/// grants no fleet-line authority.
#[test]
fn an_exec_or_authenticated_pre_b1_parent_continues_the_fleet_holder() {
    let previous = state("m3", 1).holder;
    let no_parent = |_: &Holder| false;
    assert!(continues_holder_with(
        true, &previous, &previous, &no_parent
    ));
    assert!(!continues_holder_with(false, &previous, &previous, &|_| {
        true
    }));

    let mut child = previous.clone();
    child.pid += 1;
    child.start += 1;
    let parent = |holder: &Holder| holder.pid == previous.pid && holder.start == previous.start;
    assert!(continues_holder_with(true, &previous, &child, &parent));
    assert!(!continues_holder_with(true, &previous, &child, &no_parent));
    for field in ["node", "host", "sid", "boot"] {
        let mut changed = child.clone();
        match field {
            "node" => changed.node.push('x'),
            "host" => changed.host.push('x'),
            "sid" => changed.sid.push('x'),
            _ => changed.boot.push('x'),
        }
        assert!(
            !continues_holder_with(true, &previous, &changed, &parent),
            "changed {field}"
        );
    }
}

#[test]
fn stalled_holder_is_adopted_but_live_progress_and_network_faults_are_not() {
    let old = state("m3", 1);
    assert!(adoptable(
        &old,
        &Observation::Live(Box::new(old.clone())),
        1802
    ));
    // Negative control: only the boundary changes, not a heartbeat or ref age.
    let live = state("m3", 1801);
    assert!(!adoptable(
        &old,
        &Observation::Live(Box::new(live.clone())),
        1802
    ));
    assert!(
        !adoptable(&live, &Observation::Live(Box::new(old.clone())), 1802),
        "a cached fleet read cannot overrule a fresh ref boundary"
    );
    // An unread host is neither dead nor alive: the claim's own age decides, at twice the bound.
    assert!(!adoptable(&old, &Observation::Unknown, 1802));
    assert!(!adoptable(&old, &Observation::Unknown, 7201));
    assert!(adoptable(&old, &Observation::Unknown, 7202));
    assert!(adoptable(&old, &Observation::Dead, 2));
    let mut long_step = old.clone();
    long_step.p95_s = 2000;
    assert!(!adoptable(
        &old,
        &Observation::Live(Box::new(long_step.clone())),
        4001
    ));
    assert!(adoptable(
        &old,
        &Observation::Live(Box::new(long_step.clone())),
        4002
    ));
    // A step with a long recorded p95 keeps an unread holder's lease for four times it.
    assert!(!adoptable(&long_step, &Observation::Unknown, 8001));
    assert!(adoptable(&long_step, &Observation::Unknown, 8002));
}

#[test]
fn a_supersession_takes_the_line_from_any_holder_and_a_contender_does_not() {
    let fresh = state("m9", 1000);
    let live = Observation::Live(Box::new(fresh.clone()));
    // The 0.199.0 incident: a holder no contender could read, and one that was live and working.
    for observed in [&Observation::Unknown, &live] {
        assert!(!may_replace(Take::Contend, false, &fresh, observed, 1001));
        assert!(may_replace(Take::Supersede, false, &fresh, observed, 1001));
    }
    // Continuing is the holder's own identity, proved separately, never the caller's word.
    assert!(!may_replace(Take::Continue, false, &fresh, &live, 1001));
    assert!(may_replace(Take::Continue, true, &fresh, &live, 1001));
    // A contender still adopts on evidence alone.
    assert!(may_replace(
        Take::Contend,
        false,
        &fresh,
        &Observation::Dead,
        1001
    ));
}

#[test]
fn activity_without_a_completed_row_cannot_extend_the_progress_bound() {
    assert!(completed_row("compiler still busy").is_none());
    assert!(completed_row(r#"{"event":"stage-started","stage":"compile"}"#).is_none());
    assert!(
        completed_row(r#"{"event":"test-binary-finished","test":"db","outcome":"FAIL"}"#).is_some()
    );
    assert_eq!(
        p95(
            "1\t0.155.0\tverify\t2000\n2\t0.155.0\tbuild\t9000",
            "verify"
        ),
        2000
    );
}

#[test]
fn only_the_private_events_stream_advances_progress_and_partial_rows_wait() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let event = r#"{"event":"check-finished","check":"lint","outcome":"PASS"}"#;
    let output = dir.path().join("06-verify.log");
    std::fs::write(output, format!("{event}\n")).unwrap();
    let events = dir.path().join("06-verify.events");
    std::fs::write(&events, event).unwrap();
    let mut rows = Rows::default();
    // Negative controls: output that resembles a row, and an incomplete private row.
    assert!(rows.read(dir.path()).is_empty());
    std::fs::write(&events, format!("{event}\n")).unwrap();
    assert_eq!(rows.read(dir.path()).len(), 1);
    assert!(
        rows.read(dir.path()).is_empty(),
        "a poll is not another boundary"
    );
}

#[test]
fn reported_failed_cases_survive_remote_history_without_inventing_names() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cases = [
        Some(aterm_json::json!("first::case")),
        Some(aterm_json::json!("second::case")),
        None,
        Some(aterm_json::Value::Null),
        Some(aterm_json::json!({"unsupported": "name"})),
    ];
    let mut events = String::new();
    for case in &cases {
        let mut event = aterm_json::json!({
            "event": "test-case-failed", "test": "-p sample --lib",
            "outcome": "FAIL", "detail": "its block decides its binary a FAIL",
        });
        if let Some(case) = case {
            event
                .as_object_mut()
                .unwrap()
                .insert("case".into(), case.clone());
        }
        events.push_str(&format!("verify-event: {event}\n"));
    }
    events.push_str(
        "verify-event: {\"event\":\"test-case-failed\",\"test\":\"-p sample --lib\",\"case\":\"spared\",\"outcome\":\"NOT RUN\"}\n",
    );
    std::fs::write(dir.path().join("verify.events"), events).unwrap();
    let mut original = state("holder", 123);
    original.rows = Rows::default().read(dir.path());
    assert_eq!(original.rows.len(), 6);
    let encoded = aterm_toml::to_string(&original).unwrap();
    let decoded = parse_state(&encoded, &reference("0.156.0").unwrap()).unwrap();
    assert_eq!(decoded, original);
    let red = red_items(&decoded);
    let red: Vec<aterm_json::Value> = red
        .split("; ")
        .map(|row| aterm_json::from_str(row).unwrap())
        .collect();
    assert_eq!(red.len(), 5);
    assert_eq!(red[0]["case"].as_str(), Some("first::case"));
    assert_eq!(red[1]["case"].as_str(), Some("second::case"));
    assert_ne!(red[0], red[1]);
    assert!(red[2..].iter().all(|row| row["case"].is_null()));
    assert!(
        red.iter()
            .all(|row| row["outcome"].as_str() == Some("FAIL"))
    );
}

#[test]
fn reported_stage_failures_are_red_before_the_aggregate_check_finishes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut failed = aterm_verify::Report::new("finished stage with sibling pending");
    failed.fail("reported failure");
    failed.cannot_run("unavailable sibling work");
    let mut passed = aterm_verify::Report::new("passing stage");
    passed.pass("checked");
    passed.raw("error: raw text alone is not a verdict");
    let mut unavailable = aterm_verify::Report::new("unavailable stage");
    unavailable.cannot_run("not executed");
    let mut events = String::new();
    for report in [&failed, &passed, &unavailable] {
        events.push_str(&aterm_verify::report::stage_event(
            0,
            Some(aterm_verify::checks::Check::Test),
            report,
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
            None,
        ));
    }
    events.push_str("verify-event: {\"event\":\"stage-finished\",\"decisions\":[{\"outcome\":\"FAIL\",\"detail\":12}]}\n");
    std::fs::write(dir.path().join("verify.events"), events).unwrap();
    let mut original = state("holder", 123);
    original.rows = Rows::default().read(dir.path());
    assert_eq!(original.rows.len(), 4);
    let decoded = parse_state(
        &aterm_toml::to_string(&original).unwrap(),
        &reference("0.156.0").unwrap(),
    )
    .unwrap();
    assert_eq!(decoded, original);
    let red: aterm_json::Value = aterm_json::from_str(&red_items(&decoded)).unwrap();
    assert_eq!(red["event"].as_str(), Some("stage-finished"));
    assert!(red["outcome"].is_null());
    assert_eq!(red["decisions"][0]["outcome"].as_str(), Some("FAIL"));
    assert_eq!(
        red["decisions"][0]["detail"].as_str(),
        Some("reported failure")
    );
    assert_eq!(red["decisions"][1]["outcome"].as_str(), Some("NOT RUN"));
    // An unsupported neighbor cannot erase a separately reported failure.
    let mixed = completed_row(
        r#"{"event":"stage-finished","decisions":[null,{"outcome":"FAIL","detail":"observed"}]}"#,
    )
    .unwrap();
    let mut state = original;
    state.rows = vec![mixed];
    assert!(!red_items(&state).is_empty());
}

#[test]
fn progress_reader_discards_large_lines_without_rereading_partial_bytes() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("06-verify.events");
    let event = r#"{"event":"check-finished","check":"lint","outcome":"PASS"}"#;
    let mut reader = Rows::default();
    // Complete arbitrary output is bounded too, even though it is not JSON.
    std::fs::write(
        &path,
        format!("{}\n{event}\n", "x".repeat(PROGRESS_LINE_BYTES * 3)),
    )
    .unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert!(reader.read(dir.path()).is_empty());

    std::fs::write(&path, "x".repeat(PROGRESS_LINE_BYTES * 2)).unwrap();
    assert!(reader.read(dir.path()).is_empty());
    let cursor = reader.0.get(&path).unwrap();
    let consumed = cursor.offset;
    assert_eq!(consumed, (PROGRESS_LINE_BYTES * 2) as u64);
    assert!(cursor.partial.is_empty());
    assert!(cursor.discarding);
    assert!(reader.read(dir.path()).is_empty());
    assert_eq!(reader.0[&path].offset, consumed);
    // A valid-looking suffix of the oversized record must not become a row.
    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    write!(append, "{event}\n{event}\n").unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert!(!reader.0[&path].discarding);
    assert!(reader.0[&path].partial.is_empty());
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
fn progress_reader_keeps_bounded_fragments_and_seeds_old_completed_rows() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.log");
    let event =
        "verify-event: {\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"PASS\"}\n";
    std::fs::write(&path, event).unwrap();
    let mut reader = Rows::default();
    reader.seed(dir.path());
    assert!(reader.read(dir.path()).is_empty());
    let split = event.len() / 2;
    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    append.write_all(&event.as_bytes()[..split]).unwrap();
    assert!(reader.read(dir.path()).is_empty());
    assert_eq!(reader.0[&path].partial, event.as_bytes()[..split]);
    assert_eq!(reader.0[&path].offset, (event.len() + split) as u64);
    append.write_all(&event.as_bytes()[split..]).unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
fn progress_seed_skips_dense_history_and_keeps_its_incomplete_tail() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("06-verify.events");
    let event = "{\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"PASS\"}\n";
    let next = "{\"event\":\"check-finished\",\"check\":\"doc\",\"outcome\":\"PASS\"}\n";
    let split = event.len() / 2;
    let mut file = std::fs::File::create(&path).unwrap();
    for _ in 0..10_000 {
        file.write_all(event.as_bytes()).unwrap();
    }
    file.write_all(&event.as_bytes()[..split]).unwrap();
    let mut reader = Rows::default();
    reader.seed(dir.path());
    assert_eq!(reader.0[&path].offset, file.metadata().unwrap().len());
    assert_eq!(reader.0[&path].partial, event.as_bytes()[..split]);
    assert!(reader.read(dir.path()).is_empty());
    file.write_all(&event.as_bytes()[split..]).unwrap();
    file.write_all(next.as_bytes()).unwrap();
    assert_eq!(
        reader.read(dir.path()),
        [completed_row(event).unwrap(), completed_row(next).unwrap()]
    );
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
fn progress_seed_keeps_oversize_discard_through_the_next_newline() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.log");
    std::fs::write(&path, "x".repeat(PROGRESS_LINE_BYTES * 2)).unwrap();
    let mut reader = Rows::default();
    reader.seed(dir.path());
    assert_eq!(reader.0[&path].offset, (PROGRESS_LINE_BYTES * 2) as u64);
    assert!(reader.0[&path].partial.is_empty());
    assert!(reader.0[&path].discarding);
    assert!(reader.read(dir.path()).is_empty());
    let event = "{\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"PASS\"}\n";
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    write!(file, "{event}{event}").unwrap();
    assert_eq!(reader.read(dir.path()), [completed_row(event).unwrap()]);
    assert!(!reader.0[&path].discarding);
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
fn progress_reader_never_joins_fragments_across_replacement_or_observed_truncation() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("06-verify.events");
    let event = r#"{"event":"check-finished","check":"lint","outcome":"PASS"}"#;
    let split = event.len() - 3;
    let mut reader = Rows::default();
    std::fs::write(&path, &event[..split]).unwrap();
    assert!(reader.read(dir.path()).is_empty());
    let old_identity = reader.0[&path].identity;
    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, format!("{}\n{event}\n", &event[split..])).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert_ne!(reader.0[&path].identity, old_identity);
    assert!(reader.read(dir.path()).is_empty());

    // Observe a shortened same-inode file while an old prefix is retained.
    std::fs::write(&path, "x".repeat(PROGRESS_LINE_BYTES / 2)).unwrap();
    assert!(reader.read(dir.path()).is_empty());
    let same_identity = reader.0[&path].identity;
    std::fs::write(&path, format!("{}\n{event}\n", &event[split..])).unwrap();
    assert_eq!(reader.read(dir.path()).len(), 1);
    assert_eq!(reader.0[&path].identity, same_identity);
    assert!(reader.read(dir.path()).is_empty());
}

#[test]
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn blocked_publication_cannot_hide_completed_progress_from_the_fleet() {
    const COMPLETIONS: usize = 16_384;
    let dir = aterm_tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    let event = aterm_json::json!({
        "event":"test-binary-finished", "test":"db", "outcome":"FAIL",
        "detail":"x".repeat(256),
    })
    .to_string();
    let recording = format!("{event}\n").repeat(COMPLETIONS);
    let log = logs.join("06-verify.events");
    std::fs::write(&log, &recording).unwrap();
    let mut old = state("m3", 1);
    old.holder.pid = std::process::id();
    (old.holder.boot, old.holder.start) =
        aterm_agent::release_progress::identity(old.holder.pid).unwrap();
    let local = Arc::new(Local {
        state: Mutex::new(old.clone()),
        path: dir
            .path()
            .join(aterm_agent::release_progress::DIRECTORY)
            .join("line.json"),
    });
    let lease = Lease {
        repo: dir.path().into(),
        name: "unused".into(),
        sha: "old".into(),
        state: old.clone(),
        local: local.path.clone(),
        previous_holder: None,
    };
    local.update(|state| state.step = "build".into()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let publishing = Arc::clone(&local);
    let writer = std::thread::spawn(move || {
        publish_progress_with(lease, &publishing, &rx, &mut |_| {
            entered_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap();
            Ok(true)
        })
    });
    tx.send(Request::Wake).unwrap();
    let entered = entered_rx.recv_timeout(std::time::Duration::from_secs(60));
    // This is the production sampler path, while the publisher is held by a channel.
    let sampled = local.sample(&mut Rows::default(), &logs);
    let observed = local_observation(&old, dir.path());
    let current = local.snapshot();
    let local_bytes = std::fs::read(&local.path);
    let recorded_bytes = std::fs::read(&log);
    // Retire the held publisher before any assertion, including I/O failures.
    let stopped = tx.send(Request::Stop);
    let released = release_tx.send(());
    let joined = writer.join();
    entered.unwrap();
    stopped.unwrap();
    released.unwrap();
    joined.unwrap();
    sampled.unwrap();
    let current = current.unwrap();
    let local_bytes = local_bytes.unwrap();
    let full_history = aterm_json::to_vec(&current).unwrap();
    assert!(full_history.len() > 4 * 1024 * 1024);
    assert!(local_bytes.len() < 4 * 1024 * 1024);
    assert_eq!(current.rows.len(), COMPLETIONS);
    let mut expected_local = current.clone();
    expected_local.rows.clear();
    let local_record: State = aterm_json::from_slice(&local_bytes).unwrap();
    assert_eq!(local_record, expected_local);
    assert_eq!(recorded_bytes.unwrap(), recording.as_bytes());
    // Remote serialization still carries every outcome, including red details.
    let remote_text = aterm_toml::to_string(&current).unwrap();
    let remote_record =
        parse_state(&remote_text, &reference(&current.line.version).unwrap()).unwrap();
    assert_eq!(remote_record, current);
    assert!(!red_items(&remote_record).is_empty());
    assert!(matches!(&observed, Observation::Live(live) if **live == expected_local));
    assert!(!adoptable(&old, &observed, now()));
    // Negative control: without the sampler's fresh host observation this same old
    // ref and live process would be judged stalled and adopted.
    assert!(adoptable(
        &old,
        &Observation::Live(Box::new(old.clone())),
        now()
    ));
    // Negative control: the previous full-history registry shape is omitted by
    // the real survey reader before its own rows projection can run.
    std::fs::write(&local.path, full_history).unwrap();
    assert!(matches!(
        local_observation(&old, dir.path()),
        Observation::Unknown
    ));
}

#[cfg(unix)]
fn repository(root: &Path, name: &str, origin: &Path) -> Network<'static> {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    let path = root.join(name);
    std::fs::create_dir(&path).unwrap();
    let git = GitCli::new(&path);
    for args in [
        vec!["init"],
        vec!["config", "user.name", "Andrew Yates"],
        vec!["config", "user.email", "test@example.invalid"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/fixture/app.git",
        ],
    ] {
        git_ok(&git, &args).unwrap();
    }
    Network {
        remote: crate::github_remote::claim_fixture(&path, origin),
        repo: path,
        stopped: &RUNNING,
    }
}

#[cfg(unix)]
#[test]
fn native_line_claim_retries_inventory_with_fresh_policy_and_a_bound() {
    use std::sync::{OnceLock, Weak};
    for mode in [
        "forward",
        "twice",
        "early",
        "conflict",
        "rewind",
        "diverge",
        "bound",
        "every",
        "between",
        "between-ref",
        "policy",
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        std::fs::create_dir(&origin).unwrap();
        git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
        git_ok(
            &GitCli::new(&origin),
            &["config", "user.name", "Andrew Yates"],
        )
        .unwrap();
        git_ok(
            &GitCli::new(&origin),
            &["config", "user.email", "test@example.invalid"],
        )
        .unwrap();
        let mut writer = repository(dir.path(), "writer", &origin);
        let source = GitCli::new(&writer.repo);
        git_ok(&source, &["checkout", "-b", "main"]).unwrap();
        git_ok(&source, &["commit", "--allow-empty", "-m", "base"]).unwrap();
        let base = git_ok(&source, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_owned();
        git_ok(&source, &["commit", "--allow-empty", "-m", "tip"]).unwrap();
        let initial = git_ok(&source, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_owned();
        git_ok(&source, &["push", origin.to_str().unwrap(), "main"]).unwrap();
        let name = reference("0.156.0").unwrap();
        let rival = commit(&writer, "", &state("other", 124)).unwrap();
        git_ok(
            &GitCli::new(&origin),
            &[
                "fetch",
                "--no-write-fetch-head",
                writer.repo.to_str().unwrap(),
                &rival,
            ],
        )
        .unwrap();
        let slot = Arc::new(OnceLock::<Weak<agh::Fx>>::new());
        let observed_slot = slot.clone();
        let reads = Arc::new(Mutex::new(0_usize));
        let observed_reads = reads.clone();
        let bare = origin.clone();
        let observed_name = name.clone();
        let early = AtomicBool::new(false);
        let fx = Arc::new(
            agh::Fx::simulated(1_760_000_000).with_git_child_observer_for_tests(move |span| {
                let specs = observed_slot
                    .get()
                    .unwrap()
                    .upgrade()
                    .unwrap()
                    .children_run();
                let args = &specs[span.ordinal].args;
                let early_move = mode == "early"
                    && args.iter().any(|arg| arg == "--is-shallow-repository")
                    && !early.swap(true, Ordering::SeqCst);
                if !early_move
                    && (!args.iter().any(|arg| arg == "ls-remote")
                        || !args.iter().any(|arg| arg == observed_name.as_str())
                        || !args.iter().any(|arg| arg == "refs/heads/main"))
                {
                    return;
                }
                let mut reads = observed_reads.lock().unwrap();
                if !early_move {
                    *reads += 1;
                }
                let git = GitCli::new(&bare);
                if mode == "between-ref" && *reads == 2 {
                    git_ok(&git, &["update-ref", &observed_name, &rival]).unwrap();
                    return;
                }
                if mode == "between" && *reads == 2 {
                    git_ok(&git, &["update-ref", "refs/heads/main", &initial]).unwrap();
                    return;
                }
                if mode == "early" && !early_move {
                    return;
                }
                if !early_move
                    && *reads != 1
                    && mode != "every"
                    && !(mode == "twice" && *reads == 2)
                    && !(mode == "bound" && *reads % 2 == 1)
                {
                    return;
                }
                let current = git_ok(&git, &["rev-parse", "main"])
                    .unwrap()
                    .stdout_utf8()
                    .trim()
                    .to_owned();
                let tree = git_ok(&git, &["rev-parse", "main^{tree}"])
                    .unwrap()
                    .stdout_utf8()
                    .trim()
                    .to_owned();
                let message = if mode == "policy" {
                    let record = crate::line::Supersession {
                        previous: state("holder", 123).line,
                        replacement: crate::line::Line::start("0.156.0", &current, 2),
                        reason: "concurrent retirement".into(),
                    };
                    format!(
                        "land\n\nAterm-Line-Supersession: {}",
                        aterm_json::to_string(&record).unwrap()
                    )
                } else {
                    "land".into()
                };
                let args = if matches!(mode, "rewind" | "diverge") {
                    vec!["commit-tree", tree.as_str(), "-m", "unrelated"]
                } else {
                    vec![
                        "commit-tree",
                        tree.as_str(),
                        "-p",
                        current.as_str(),
                        "-m",
                        message.as_str(),
                    ]
                };
                let next = if mode == "rewind" {
                    base.clone()
                } else {
                    git_ok(&git, &args).unwrap().stdout_utf8().trim().to_owned()
                };
                let reference = if mode == "conflict" {
                    observed_name.as_str()
                } else {
                    "refs/heads/main"
                };
                git_ok(&git, &["update-ref", reference, &next]).unwrap();
            }),
        );
        assert!(slot.set(Arc::downgrade(&fx)).is_ok());
        writer.remote =
            crate::github_remote::claim_fixture_with_fx(&writer.repo, &origin, fx.clone());
        let mut attempts = 0;
        let mut expected_ref = None;
        let result = inventory_retry(|floor| {
            attempts += 1;
            let (main, _) = claim_main(
                &writer.remote,
                &writer.repo,
                &state("holder", 123).line,
                floor,
            )?;
            remote_expected(&writer, &name, expected_ref.as_deref())?;
            match replace_at_main(&writer, &name, "", &state("holder", 123), Some(&main)) {
                Write::Written(sha) => Ok(ClaimAttempt::Done(sha)),
                Write::InventoryChanged { main, why } => {
                    expected_ref = Some(String::new());
                    Ok(ClaimAttempt::InventoryChanged { main, why })
                }
                Write::Race => Err("conflict".into()),
                Write::Offline(why) => Err(why),
            }
        });
        if matches!(mode, "forward" | "early" | "twice") {
            let sha = result.unwrap();
            assert_eq!(
                git_ok(&GitCli::new(&origin), &["rev-parse", &name])
                    .unwrap()
                    .stdout_utf8()
                    .trim(),
                sha
            );
            assert_eq!(attempts, 2);
            assert_eq!(*reads.lock().unwrap(), if mode == "early" { 3 } else { 4 });
        } else {
            let why = result.unwrap_err();
            assert_eq!(
                attempts,
                match mode {
                    "bound" | "every" => 3,
                    "between" | "between-ref" | "policy" => 2,
                    _ => 1,
                },
                "{mode}: {why}"
            );
            if matches!(mode, "bound" | "every") {
                assert!(why.contains("outside the requested refs"));
            }
            if mode == "policy" {
                assert!(why.contains("superseded on main"));
            }
            if mode == "between-ref" {
                assert!(why.contains("conflict; side_effect=none"));
            }
            if mode == "between" {
                assert!(why.contains("did not advance fast-forward"), "{why}");
            }
            assert!(
                !fx.children_run()
                    .iter()
                    .any(|s| s.args.iter().any(|a| a == "push"))
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn native_fleet_status_reads_the_actual_claim_without_changing_its_holder() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
    let writer = repository(dir.path(), "writer", &origin);
    let name = reference("0.156.0").unwrap();
    let initial = state("original-holder", 123);
    let Write::Written(sha) = replace(&writer, &name, "", &initial) else {
        panic!("fixture claim must be written");
    };
    let reader = repository(dir.path(), "reader", &origin);
    let root = dir.path().join("reader");
    let remote = crate::github_remote::tests::local(&root, &origin);
    assert_eq!(
        status_rows(&remote).unwrap(),
        [
            "v0.156.0 original-holder original-holder step=verify boundary=test-binary-finished at=123 head=base"
        ]
    );
    assert_eq!(
        crate::ledger::rev_parse(&GitCli::new(&origin), &name).unwrap(),
        sha
    );
    assert!(
        git_ok(&reader, &["for-each-ref", "--format=%(refname)"])
            .unwrap()
            .stdout_utf8()
            .lines()
            .all(|name| name.starts_with("refs/agh/"))
    );
    assert!(!root.join(".git/FETCH_HEAD").exists());
    let mut wrong = initial;
    wrong.line.version = "0.155.0".into();
    assert!(matches!(
        replace(&writer, &name, &sha, &wrong),
        Write::Written(_)
    ));
    assert!(
        status_rows(&remote)
            .unwrap_err()
            .contains("version does not match")
    );
}

#[cfg(unix)]
#[test]
fn two_macs_have_one_holder_and_stalled_cut_is_not_lost() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
    let a = repository(dir.path(), "a", &origin);
    let b = repository(dir.path(), "b", &origin);
    // Before B6 each clone could acquire its own run claim for the same version.
    // These are the existing production local locks, not a model of them.
    let local_a = crate::release::RunClaim::take(
        &dir.path().join("a/running"),
        (101, "start-a"),
        dir.path(),
        &mut |_| None,
    )
    .unwrap();
    let local_b = crate::release::RunClaim::take(
        &dir.path().join("b/running"),
        (102, "start-b"),
        dir.path(),
        &mut |_| None,
    )
    .unwrap();
    let baseline_holders = [&local_a, &local_b].len();
    assert_eq!(baseline_holders, 2);
    let name = reference("0.156.0").unwrap();
    let initial = state("m3", 1);
    // Both Macs read the absent ref. Exactly one CAS can install a holder.
    let first = match replace(&a, &name, "", &initial) {
        Write::Written(sha) => sha,
        other => panic!("{other:?}"),
    };
    let mut holder = Lease {
        repo: dir.path().join("a"),
        name: name.clone(),
        sha: first.clone(),
        state: initial.clone(),
        local: dir.path().join("local.json"),
        previous_holder: None,
    };
    git_ok(
        &a,
        &[
            "remote",
            "set-url",
            "origin",
            dir.path().join("unavailable").to_str().unwrap(),
        ],
    )
    .unwrap();
    let stopped = AtomicBool::new(false);
    assert_eq!(
        holder.write(&stopped),
        Ok(false),
        "a transport fault is pending, never ownership loss"
    );
    assert_eq!(holder.sha, first);
    git_ok(
        &a,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/fixture/app.git",
        ],
    )
    .unwrap();
    assert_eq!(
        holder.write_with(&a),
        Ok(true),
        "the same candidate continues once transport returns"
    );
    let first = holder.sha.clone();
    assert_eq!(replace(&b, &name, "", &state("m22", 1)), Write::Race);
    let (old_sha, old) = remote(&b, &name).unwrap().unwrap();
    assert_eq!(old_sha, first);
    assert!(!adoptable(
        &old,
        &Observation::Live(Box::new(state("m3", 1801))),
        1802
    ));
    assert!(adoptable(
        &old,
        &Observation::Live(Box::new(old.clone())),
        1802
    ));
    let mut adopted = old.clone();
    adopted.holder = state("m22", 1802).holder;
    adopted.at = 1802;
    let new = match replace(&b, &name, &old_sha, &adopted) {
        Write::Written(sha) => sha,
        other => panic!("{other:?}"),
    };
    assert_eq!(replace(&a, &name, &first, &initial), Write::Race);
    assert!(
        holder.write_with(&a).is_err(),
        "positive ownership loss stops the old holder"
    );
    assert_eq!(
        remote(&a, &name).unwrap().unwrap().1.line,
        initial.line,
        "the exact candidate survives takeover"
    );
    // Negative control: removing the expected-old lease lets the stale holder overwrite
    // the adopter. The same stale write with the lease above was refused.
    let stale = commit(&a, &new, &initial).unwrap();
    git_ok(
        &a,
        &[
            "-c",
            "protocol.file.allow=always",
            "push",
            "--force",
            origin.to_str().unwrap(),
            &format!("{stale}:{name}"),
        ],
    )
    .unwrap();
    assert_eq!(remote(&b, &name).unwrap().unwrap().1.holder, initial.holder);
    println!(
        "B6 measured: before holders/version={baseline_holders}; after holders/version=1; unfenced stale overwrite=1; leased stale overwrite=0; stalled adoptions=1; cuts lost=0"
    );
}

#[cfg(unix)]
#[test]
fn a_stopped_or_retargeted_monitor_cannot_publish_a_claim() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git_ok(&GitCli::new(&origin), &["init", "--bare"]).unwrap();
    let initial = repository(dir.path(), "writer", &origin);
    let stopped = AtomicBool::new(true);
    let writer = Network {
        repo: initial.repo,
        remote: initial.remote,
        stopped: &stopped,
    };
    let name = reference("0.156.0").unwrap();
    assert!(matches!(
        replace(&writer, &name, "", &state("m3", 1)),
        Write::Offline(_)
    ));
    stopped.store(false, Ordering::SeqCst);
    git_ok(
        &writer,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/foreign/app.git",
        ],
    )
    .unwrap();
    assert!(matches!(
        replace(&writer, &name, "", &state("m3", 1)),
        Write::Offline(_)
    ));
    assert!(
        git_ok(&GitCli::new(&origin), &["for-each-ref"])
            .unwrap()
            .stdout
            .is_empty()
    );
}
