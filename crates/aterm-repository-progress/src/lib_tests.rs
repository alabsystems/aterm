// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn outcomes_roundtrip_with_stage_usability_and_unknown_retry_then_clear_on_new_activity() {
    let mut row = Progress::new("repo", "/home/person/repo".into());
    row.usable = true;
    row.placed = true;
    row.enter(Phase::Fetching);
    row.fail(FailureKind::Network);
    let decoded: Progress = aterm_json::from_slice(&aterm_json::to_vec(&row).unwrap()).unwrap();
    assert_eq!(decoded, row);
    assert_eq!(
        row.outcome,
        Some(OutcomeDetail {
            stage: Phase::Fetching,
            reason: OutcomeReason::Failed(FailureKind::Network),
            retry: RetryDisposition::Unrecorded,
        })
    );
    assert!(row.usable && row.placed);
    assert_eq!(row.history.last().unwrap().phase, Phase::Failed);
    row.enter(Phase::Checking);
    assert!(row.outcome.is_none());
    row.defer(PreservationReason::LocalChanges);
    assert_eq!(
        row.outcome.as_ref().unwrap().retry,
        RetryDisposition::AfterChanges
    );
    assert!(row.usable && row.placed);
}

#[cfg(unix)]
#[test]
fn completed_snapshots_with_or_without_outcome_survive_owner_exit_unchanged() {
    for detailed in [false, true] {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let mut row = Progress::new("repo", "/home/person/repo".into());
        row.pid = u32::MAX;
        row.birth = None;
        row.enter(Phase::Verifying);
        if detailed {
            row.fail(FailureKind::Local);
        } else {
            row.enter(Phase::Failed);
        }
        let mut value = aterm_json::to_value(&row).unwrap();
        if !detailed {
            value.as_object_mut().unwrap().remove("outcome");
        }
        aterm_uds::atomic_file::write(
            &dir.path().join(format!("{}.json", row.attempt)),
            &aterm_json::to_vec(&value).unwrap(),
            0o600,
        )
        .unwrap();
        assert_eq!(read_from(dir.path()), vec![row]);
    }
}

#[cfg(unix)]
#[test]
fn a_new_attempt_does_not_inherit_an_earlier_failure_reason() {
    let mut old = Progress::new("repo", "/home/person/repo".into());
    old.started_ms = 10;
    old.enter(Phase::Connecting);
    old.fail(FailureKind::Network);
    let mut current = Progress::new("repo", old.destination.clone());
    current.started_ms = 20;
    current.enter(Phase::Checking);
    let rows = read_rows([old, current.clone()]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempt, current.attempt);
    assert!(rows[0].outcome.is_none());
    assert_eq!(rows[0].history, current.history);
    assert_eq!(rows[0].previous[0].phase, Phase::Failed);
}

#[test]
fn reasons_have_no_unbounded_diagnostic_text_boundary() {
    // A credential-bearing diagnostic cannot be decoded into the typed reason field.
    let diagnostic = [
        "https://",
        "fixture-user:",
        "fixture-value",
        "@example.invalid/repo",
    ]
    .concat();
    let mut row = Progress::new("repo", "/home/person/repo".into());
    row.enter(Phase::Connecting);
    row.fail(FailureKind::Network);
    let mut value = aterm_json::to_value(&row).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .get_mut("outcome")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("reason".into(), aterm_json::json!(diagnostic));
    assert!(aterm_json::from_value::<Progress>(value).is_err());
    let bytes = aterm_json::to_string(&row).unwrap();
    assert!(!bytes.contains("fixture-value"));
    let wording = row.outcome.unwrap().reason.label();
    assert!(wording.len() <= 128 && !wording.chars().any(char::is_control));
    assert!(!wording.contains("://"));
}

#[test]
fn git_stages_reset_percent_and_reject_unrecognized_output() {
    let mut p = Progress::new("repo", PathBuf::from("/home/person/repo"));
    assert!(p.git_line("Receiving objects:  75% (3/4), 1.50 GiB | 8.20 MiB/s"));
    assert_eq!(p.percent, Some(75));
    assert_eq!(p.received.as_deref(), Some("1.50 GiB"));
    p.enter(Phase::Verifying);
    assert_eq!(p.percent, None);
    assert!(!p.git_line("fatal: private diagnostic"));
    assert!(!p.git_line("Receiving objects: 101%"));
    assert!(p.git_line("Resolving deltas: 50% (2/4)"));
    assert_eq!(p.phase, Phase::Resolving);
    assert_eq!(p.received.as_deref(), Some("1.50 GiB"));
}

#[test]
fn chunks_and_carriage_returns_preserve_stage_transitions() {
    let mut p = Progress::new("repo", PathBuf::from("/home/person/repo"));
    let mut parser = GitProgress::default();
    assert!(!parser.feed(b"Receiving obj", &mut p));
    assert!(parser.feed(b"ects: 9% (9/100), 20 MiB | 2 MiB/s\rResolving del", &mut p));
    assert_eq!(p.percent, Some(9));
    assert!(!parser.feed(b"tas: 50% (1/2)", &mut p));
    assert!(parser.finish(&mut p));
    assert_eq!(p.phase, Phase::Resolving);
    assert_eq!(p.percent, Some(50));
    assert!(!parser.feed(&vec![b'x'; 8192], &mut p));
    assert!(!parser.finish(&mut p));
    assert_eq!(p.percent, Some(50));
}

#[test]
fn fetch_progress_preserves_usability_and_failure_preserves_placement() {
    let mut p = Progress::new("repo", PathBuf::from("/home/person/repo"));
    p.usable = true;
    p.placed = true;
    assert!(p.git_line("Receiving objects: 20% (1/5), 1 MiB | 2 MiB/s"));
    assert_eq!(p.phase, Phase::Fetching);
    p.enter(Phase::Failed);
    assert!(p.usable && p.placed);
    assert_eq!(p.percent, None);
}

#[cfg(unix)]
#[test]
fn abandoned_disk_wait_is_interrupted_without_rewriting_completed_outcomes() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let mut waiting = Progress::new("waiting", "/home/person/waiting".into());
    waiting.pid = u32::MAX;
    waiting.birth = None;
    waiting.usable = true;
    waiting.placed = true;
    waiting.enter(Phase::WaitingForSpace);
    let mut ready = Progress::new("ready", "/home/person/ready".into());
    ready.pid = u32::MAX;
    ready.birth = None;
    ready.enter(Phase::Ready);
    for row in [&waiting, &ready] {
        aterm_uds::atomic_file::write(
            &dir.path().join(format!("{}.json", row.attempt)),
            &aterm_json::to_vec(row).unwrap(),
            0o600,
        )
        .unwrap();
    }
    let rows = read_from(dir.path());
    assert_eq!(rows.len(), 2);
    let abandoned = rows.iter().find(|row| row.name == "waiting").unwrap();
    assert_eq!(abandoned.phase, Phase::Interrupted);
    assert!(abandoned.usable && abandoned.placed);
    assert_eq!(abandoned.updated_ms, waiting.updated_ms);
    assert_eq!(
        abandoned.outcome,
        Some(OutcomeDetail {
            stage: Phase::WaitingForSpace,
            reason: OutcomeReason::ReporterExited,
            retry: RetryDisposition::Unrecorded,
        })
    );
    assert_eq!(
        abandoned.history.last().unwrap().phase,
        Phase::WaitingForSpace
    );
    assert_eq!(
        rows.iter().find(|row| row.name == "ready").unwrap().phase,
        Phase::Ready
    );
}

#[cfg(unix)]
#[test]
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn pruning_reused_pids_bounds_history_and_preserves_live_or_unknown_owners() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let mut current = Progress::new("repo", "/home/person/repo".into());
    let birth = current
        .birth
        .expect("this host reads its own process birth");
    current.started_ms = 100;
    current.enter(Phase::Ready);
    let save = |row: &Progress| {
        let path = dir.path().join(format!("{}.json", row.attempt));
        aterm_uds::atomic_file::write(&path, &aterm_json::to_vec(row).unwrap(), 0o600).unwrap();
        path
    };
    let mut abandoned = Vec::new();
    for started_ms in 1..=5 {
        let mut row = Progress::new("repo", current.destination.clone());
        row.started_ms = started_ms;
        row.birth = Some(birth.wrapping_add(1));
        row.enter(Phase::Downloading);
        abandoned.push(save(&row));
    }
    let mut live = Progress::new("repo", current.destination.clone());
    live.started_ms = 10;
    live.enter(Phase::Downloading);
    let live_path = save(&live);
    let mut unknown = Progress::new("repo", current.destination.clone());
    unknown.started_ms = 11;
    unknown.birth = None;
    unknown.enter(Phase::Downloading);
    let unknown_path = save(&unknown);
    let current_path = save(&current);

    prune(dir.path(), &current);

    assert!(!abandoned[0].exists());
    assert!(!abandoned[1].exists());
    assert!(abandoned[2..].iter().all(|path| path.exists()));
    assert!(live_path.exists());
    assert!(unknown_path.exists());
    assert!(current_path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 6);
}

#[cfg(unix)]
#[test]
fn same_millisecond_cleanup_keeps_newest_history_and_preserves_later_attempts() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let make = |attempt: &str, phase| {
        let mut row = Progress::new("repo", "/home/person/repo".into());
        row.started_ms = 10;
        row.attempt = attempt.into();
        row.enter(phase);
        row.usable = phase == Phase::Ready;
        row
    };
    let save = |row: &Progress| {
        let path = dir.path().join(format!("{}.json", row.attempt));
        aterm_uds::atomic_file::write(&path, &aterm_json::to_vec(row).unwrap(), 0o600).unwrap();
        path
    };
    let newer = make("attempt-06", Phase::Ready);
    let newer_path = save(&newer);
    let current = make("attempt-05", Phase::Failed);
    let current_path = save(&current);
    let mut older = Vec::new();
    // Publication order is deliberately the opposite of attempt order.
    for index in (0..5).rev() {
        older.push(save(&make(&format!("attempt-{index:02}"), Phase::Failed)));
    }
    assert_eq!(read_from(dir.path())[0].attempt, newer.attempt);

    prune(dir.path(), &current);

    assert!(older[..3].iter().all(|path| path.exists()));
    assert!(older[3..].iter().all(|path| !path.exists()));
    assert!(current_path.exists());
    assert!(newer_path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 5);
    let rows = read_from(dir.path());
    assert_eq!(rows[0].attempt, newer.attempt);
    assert!(rows[0].usable);
}

#[cfg(unix)]
#[test]
fn tied_outcomes_keep_the_same_newest_history_in_every_arrival_order() {
    let mut rows = Vec::new();
    for (index, phase) in [
        Phase::Ready,
        Phase::Failed,
        Phase::Deferred,
        Phase::Interrupted,
        Phase::Ready,
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = Progress::new("repo", "/home/person/repo".into());
        row.attempt = format!("attempt-{index}");
        row.started_ms = if index == 4 { 20 } else { 10 };
        row.enter(phase);
        row.updated_ms = 100;
        rows.push(row);
    }
    for cached in [false, true] {
        if cached {
            rows[4].previous.push(Transition {
                at_ms: 9999,
                phase: Phase::Queued,
            });
        }
        for order in [[4, 3, 2, 1, 0], [0, 1, 2, 3, 4], [2, 4, 0, 3, 1]] {
            let observed = read_rows(order.map(|index| rows[index].clone()));
            assert_eq!(observed.len(), 1);
            assert_eq!(observed[0].attempt, "attempt-4");
            assert_eq!(
                observed[0]
                    .previous
                    .iter()
                    .map(|event| event.phase)
                    .collect::<Vec<_>>(),
                [Phase::Failed, Phase::Deferred, Phase::Interrupted],
                "arrival order {order:?} must not decide which outcomes survive"
            );
        }
    }
    // An older attempt may finish later without becoming the current attempt.
    rows[0].updated_ms = 200;
    let observed = read_rows(rows);
    assert_eq!(observed[0].attempt, "attempt-4");
    assert_eq!(
        observed[0]
            .previous
            .iter()
            .map(|event| event.phase)
            .collect::<Vec<_>>(),
        [Phase::Deferred, Phase::Interrupted, Phase::Ready]
    );
}

#[cfg(unix)]
#[test]
fn later_attempt_wins_and_owner_loss_keeps_the_previous_failure() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let mut first = Progress::new("repo", PathBuf::from("/home/person/repo"));
    first.started_ms = 10;
    first.enter(Phase::Failed);
    let mut second = Progress::new("repo", first.destination.clone());
    second.started_ms = 20;
    second.pid = u32::MAX;
    second.birth = None;
    second.enter(Phase::Downloading);
    // Delayed publication of the old attempt must not replace the new one.
    for row in [&second, &first] {
        let bytes = aterm_json::to_vec(row).unwrap();
        aterm_uds::atomic_file::write(
            &dir.path().join(format!("{}.json", row.attempt)),
            &bytes,
            0o600,
        )
        .unwrap();
    }
    let rows = read_from(dir.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempt, second.attempt);
    assert_eq!(rows[0].phase, Phase::Interrupted);
    assert!(!rows[0].usable);
    assert_eq!(rows[0].previous.last().unwrap().phase, Phase::Failed);
}
