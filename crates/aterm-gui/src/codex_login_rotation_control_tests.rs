// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::codex_app_login::RefusalReason;

#[cfg(unix)]
#[path = "codex_login_rotation_job_tests.rs"]
mod whole_job;

fn job(proof: &crate::self_claim::Proven, human_seq: u64) -> Preparation {
    Preparation {
        sid: "login-rotation-test".into(),
        intent: Intent {
            purpose: renewal::Purpose::LoginRotation,
            thread: "00000000-0000-0000-0000-000000000001".into(),
            wall: "00000000-0000-0000-0000-000000000002".into(),
            identity: renewal::Identity::from_native("source", "user"),
        },
        process: Process {
            pid: proof.pid,
            birth: 10,
            parent: 42,
            group: proof.pid,
            session: 42,
            image: Image {
                path: b"/codex".to_vec(),
                device: 1,
                inode: 2,
            },
        },
        start: proof.start.clone(),
        human_seq,
        starts: 1,
        progress: Mutex::new(Progress::Retry(Instant::now())),
    }
}

fn row(label: &str) -> accounts::Row {
    accounts::Row {
        label: label.into(),
        ..accounts::Row::default()
    }
}

fn lost_move() -> login::LoginMove {
    login::LoginMove {
        source: row("main"),
        destination: row("work"),
        source_identity: renewal::Identity::from_native("source", "user"),
        destination_identity: renewal::Identity::from_native("target", "user"),
        old_auth: "/home/.codex/auth.json".into(),
        new_auth: "/logins/work/auth.json".into(),
        generation: 0,
        reason: RefusalReason::Expired,
    }
}

/// A STOP IS THE JOB'S ANSWER ONLY WHILE THE JOB IS THE SESSION'S, AND A MOVE IS KEPT WITH ITS KEY
/// WHICHEVER JOB READ IT: a stop, whichever the decision names, is answered as not-needed by the
/// current job and asked again after the retry by a stale one; a wait is asked again no sooner
/// than the retry and not before the refusal on record settles; an error is asked again after the
/// retry. A move leaves the job to ask again at once, current or not, and is kept with the key's
/// reads for that ask to publish: the decision spent the key's one pass over the candidates, so
/// dropping a stale job's move would leave its replacement nothing to select. Nothing but a move
/// is kept.
#[test]
fn a_lost_login_decision_stops_or_waits_for_the_current_job_and_a_move_is_kept_for_its_key() {
    let now = Instant::now();
    let at = (now, 1_000);
    let retry = now + Duration::from_millis(RETRY_MS);
    let moved = || Ok(login::LoginDecision::Move(Box::new(lost_move())));
    for current in [true, false] {
        assert!(
            matches!(
                progress_after_observation("s", moved(), current, at),
                Progress::Retry(when) if when == now
            ),
            "current={current}: the next ask publishes"
        );
    }
    let mut kept = Reads::default();
    keep_selection("s", &mut kept, &moved(), now);
    assert!(matches!(
        &kept.selected,
        Some(selected) if selected.next == lost_move()
            && selected.observed == now
            && selected.publish_at.is_none()
    ));
    let stopped = Ok(login::LoginDecision::NotNeeded(
        login::LostLoginEnd::Refused(RefusalReason::Reused),
    ));
    assert!(matches!(
        progress_after_observation("s", stopped.clone(), true, at),
        Progress::NotNeeded
    ));
    // Every stop the decision names is answered not-needed, never selected and never asked again.
    for end in [
        login::LostLoginEnd::Off,
        login::LostLoginEnd::NoRow,
        login::LostLoginEnd::NamesAnother,
        login::LostLoginEnd::Held,
        login::LostLoginEnd::PinnedElsewhere,
        login::LostLoginEnd::Pinned,
        login::LostLoginEnd::RowRenewed,
        login::LostLoginEnd::Unrecorded,
        login::LostLoginEnd::Refused(RefusalReason::Invalidated),
        login::LostLoginEnd::Refused(RefusalReason::InvalidGrant),
        login::LostLoginEnd::Refused(RefusalReason::of_code("account_deactivated")),
        login::LostLoginEnd::Paced,
        login::LostLoginEnd::LoginWorks,
        login::LostLoginEnd::Unconfirmed,
        login::LostLoginEnd::NoTarget,
        login::LostLoginEnd::Spent,
    ] {
        let decided = Ok(login::LoginDecision::NotNeeded(end));
        let mut kept = Reads::default();
        keep_selection("s", &mut kept, &decided, now);
        assert!(kept.selected.is_none(), "{end}");
        assert!(
            matches!(
                progress_after_observation("s", decided, true, at),
                Progress::NotNeeded
            ),
            "{end}"
        );
    }
    assert!(matches!(
        progress_after_observation("s", stopped, false, at),
        Progress::Retry(when) if when == retry
    ));
    let settles = Ok(login::LoginDecision::Wait { until: Some(1_060) });
    assert!(matches!(
        progress_after_observation("s", settles, true, at),
        Progress::Retry(when) if when == now + Duration::from_secs(60)
    ));
    for soon in [
        Ok(login::LoginDecision::Wait { until: Some(1_001) }),
        Ok(login::LoginDecision::Wait { until: Some(900) }),
        Ok(login::LoginDecision::Wait { until: None }),
        Err("probe-panicked"),
        Err("roster"),
    ] {
        let mut kept = Reads::default();
        keep_selection("s", &mut kept, &soon, now);
        assert!(kept.selected.is_none());
        assert!(matches!(
            progress_after_observation("s", soon, true, at),
            Progress::Retry(when) if when == retry
        ));
    }
}

/// A SELECTION IS PUBLISHED, NEVER READ AGAIN: a publication that keeps failing (the sign-in lock
/// busy, the claim's revision moving) is tried again no sooner than the retry, with no read, and
/// once the selection is [`OBSERVATION_MS`] old the job answers not-needed.
#[test]
fn a_failing_publication_retries_only_the_publication_then_lapses() {
    let now = Instant::now();
    let mut selected = Selection {
        next: lost_move(),
        observed: now,
        publish_at: None,
    };
    let tries = std::cell::Cell::new(0);
    let failing = || {
        tries.set(tries.get() + 1);
        Err("signin-busy")
    };
    assert_eq!(
        publication(&mut selected, now, failing).as_deref(),
        Some(WAITING)
    );
    assert_eq!(tries.get(), 1);
    assert_eq!(
        publication(&mut selected, now + Duration::from_millis(10), failing).as_deref(),
        Some(WAITING)
    );
    assert_eq!(tries.get(), 1, "not before the retry");
    let later = now + Duration::from_millis(RETRY_MS);
    assert_eq!(
        publication(&mut selected, later, failing).as_deref(),
        Some(WAITING)
    );
    assert_eq!(tries.get(), 2);
    assert_eq!(
        publication(
            &mut selected,
            now + Duration::from_millis(OBSERVATION_MS),
            failing
        ),
        None,
        "lapsed"
    );
    assert_eq!(tries.get(), 2, "a lapsed selection is not published");
}

/// WHAT A LOSS READ OUTLIVES ITS JOB AND THE OTHER WALL TURNS ASKED IN BETWEEN: a job a person's
/// input replaced on the same process and wall turn spends the same reads; another wall turn of
/// the process has reads of its own, and the first wall turn asked again afterwards finds its own
/// as it left them (spent, and with the move it selected), where one slot per session would have
/// admitted each read again. Another process (a hand-back's relaunch) starts anew, and the
/// earlier process's reads are over.
#[test]
fn what_a_loss_read_is_kept_per_process_and_wall_turn_across_jobs_and_other_walls() {
    let handle = crate::session_store::test_handle_on(8947, -1);
    let ctx = Arc::clone(&handle.ctx);
    let proof = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]).proven;
    let first = job(&proof, 3);
    let kept = reads(&ctx, &first).unwrap();
    keep_selection(
        "s",
        &mut kept.lock().unwrap(),
        &Ok(login::LoginDecision::Move(Box::new(lost_move()))),
        Instant::now(),
    );
    let replaced = job(&proof, 4);
    assert!(Arc::ptr_eq(&reads(&ctx, &replaced).unwrap(), &kept));

    let mut next_wall = job(&proof, 4);
    next_wall.intent.wall = "00000000-0000-0000-0000-000000000003".into();
    let other = reads(&ctx, &next_wall).unwrap();
    assert!(!Arc::ptr_eq(&other, &kept));
    assert!(other.lock().unwrap().selected.is_none());
    let mut next_thread = job(&proof, 4);
    next_thread.intent.thread = "00000000-0000-0000-0000-000000000009".into();
    assert!(!Arc::ptr_eq(&reads(&ctx, &next_thread).unwrap(), &kept));
    let again = reads(&ctx, &first).unwrap();
    assert!(
        Arc::ptr_eq(&again, &kept),
        "the first wall turn, asked again after two others, keeps what it read"
    );
    assert!(again.lock().unwrap().selected.is_some());
    assert!(Arc::ptr_eq(&reads(&ctx, &next_wall).unwrap(), &other));

    let mut relaunched = job(&proof, 4);
    relaunched.process.birth += 1;
    let fresh = reads(&ctx, &relaunched).unwrap();
    assert!(!Arc::ptr_eq(&fresh, &kept));
    assert_eq!(fresh.lock().unwrap().ledger, login::ReadLedger::default());
    assert!(fresh.lock().unwrap().selected.is_none());
    assert!(
        !Arc::ptr_eq(&reads(&ctx, &first).unwrap(), &kept),
        "the earlier process's reads are over"
    );
}

/// ONE PROCESS IS SERVED A BOUNDED NUMBER OF LOST LOGINS: each wall turn it is asked for keeps its
/// own reads, up to [`LEDGERS`] of them; a further one is given none (the job answers not-needed),
/// and none already kept is dropped to make room, so no loss's reads are ever admitted again.
/// CONTROL: a relaunched process is served again.
#[test]
fn one_process_is_served_a_bounded_number_of_lost_logins() {
    let handle = crate::session_store::test_handle_on(8948, -1);
    let ctx = Arc::clone(&handle.ctx);
    let proof = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]).proven;
    let on_wall = |wall: usize| {
        let mut job = job(&proof, 3);
        job.intent.wall = format!("00000000-0000-0000-0000-{wall:012}");
        job
    };
    let kept: Vec<_> = (0..LEDGERS)
        .map(|wall| reads(&ctx, &on_wall(wall)).expect("served"))
        .collect();
    assert!(
        reads(&ctx, &on_wall(LEDGERS)).is_none(),
        "one loss too many"
    );
    for (wall, kept) in kept.iter().enumerate() {
        assert!(
            Arc::ptr_eq(&reads(&ctx, &on_wall(wall)).unwrap(), kept),
            "wall {wall} keeps its reads"
        );
    }
    let mut relaunched = on_wall(LEDGERS);
    relaunched.process.birth += 1;
    assert!(reads(&ctx, &relaunched).is_some(), "CONTROL");
}

/// THE JOB CANNOT FOLLOW ANOTHER PROCESS, WALL OR PERSON'S TURN, AND ITS INDEX IS ITS OWN: the
/// index names the login rotation's purpose and does not change with its candidates.
#[test]
fn a_login_rotation_job_is_bound_to_its_process_wall_and_turn() {
    let proof = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]).proven;
    let job = job(&proof, 3);
    assert!(job.matches(&job.intent, &job.process, &proof, 3, 1));
    let mut intent = job.intent.clone();
    intent.wall = "00000000-0000-0000-0000-000000000003".into();
    assert!(!job.matches(&intent, &job.process, &proof, 3, 1));
    intent = job.intent.clone();
    intent.purpose = renewal::Purpose::WeeklyQuota;
    assert!(!job.matches(&intent, &job.process, &proof, 3, 1));
    assert!(!job.matches(&job.intent, &job.process, &proof, 4, 1));
    let original = index(&job).unwrap();
    *job.progress.lock().unwrap() = Progress::NotNeeded;
    assert_eq!(index(&job).unwrap(), original);
    let quota = Intent {
        purpose: renewal::Purpose::WeeklyQuota,
        ..job.intent.clone()
    };
    assert_ne!(
        renewal::IndexKey::moved(&quota, job.process.pid, job.process.birth, &job.start).unwrap(),
        original,
        "a usage-limit handoff on the same wall is indexed apart"
    );
}

/// THE VERB TAKES THE ROTATION'S FOUR KEYS AND ANSWERS UNDER ITS OWN PREFIX: a fifth field, an
/// unknown key, a key missing or a login renewal's keys are refused as an invalid request before
/// any claim is read, and every reply of the renewal lane is renamed to `codex-login-rotation`.
#[test]
fn a_login_rotation_request_is_the_rotations_four_keys_under_its_own_prefix() {
    let handle = crate::session_store::test_handle_on(8967, -1);
    let ctx = Arc::clone(&handle.ctx);
    let source = renewal::Identity::from_native("source", "user");
    let keys = format!(
        "thread=019a4b54-5fa1-7123-8123-123456789abc \
         turn=019a4b54-5fa2-7123-8123-123456789abc source_account={} source_user={}",
        source.account, source.user
    );
    for line in [
        format!("prepare {keys} target_account=target"),
        format!("prepare {keys} extra"),
        format!(
            "prepare thread=019a4b54-5fa1-7123-8123-123456789abc source_account={} \
             source_user={}",
            source.account, source.user
        ),
        format!(
            "prepare thread=019a4b54-5fa1-7123-8123-123456789abc \
             wall=019a4b54-5fa2-7123-8123-123456789abc account={} user={}",
            source.account, source.user
        ),
        "status id=nope".to_string(),
    ] {
        assert_eq!(
            super::super::serve_login_rotation(&ctx, -1, &line),
            "ERR codex-login-rotation invalid request\n",
            "{line}"
        );
    }
    assert_eq!(
        super::super::serve_login_rotation(&ctx, -1, &format!("prepare {keys}")),
        "ERR codex-login-rotation denied\n",
        "a well-formed request from no claimed process"
    );
    assert_eq!(
        super::super::renamed(
            "OK codex-renewal state=waiting\n".into(),
            "codex-login-rotation"
        ),
        "OK codex-login-rotation state=waiting\n"
    );
    assert_eq!(
        super::super::renamed(WAITING.into(), "codex-login-rotation"),
        WAITING,
        "the job's own replies already carry the prefix"
    );
}
