// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[path = "codex_rotation_complete_tests.rs"]
mod complete;

#[test]
fn a_current_terminal_refusal_ends_rotation_but_stale_and_transient_results_retry() {
    let now = Instant::now();
    for why in [
        "source-unbound",
        "source-missing",
        "source-unmanaged",
        "not-walled",
    ] {
        assert!(
            matches!(
                progress_after_observation(Err(why), true, now),
                Progress::NotNeeded
            ),
            "{why}"
        );
        assert!(
            matches!(progress_after_observation(Err(why), false, now), Progress::Retry(at) if at > now),
            "stale {why}"
        );
    }
    for result in [Err("roster-busy"), Err("probe-panicked"), Ok(None)] {
        assert!(
            matches!(progress_after_observation(result, true, now), Progress::Retry(at) if at > now)
        );
    }
}

fn job(proof: &crate::self_claim::Proven) -> Preparation {
    Preparation {
        sid: "rotation-test".into(),
        intent: Intent {
            purpose: renewal::Purpose::WeeklyQuota,
            thread: "00000000-0000-0000-0000-000000000001".into(),
            wall: "00000000-0000-0000-0000-000000000002".into(),
            identity: renewal::Identity::from_native("source", "user"),
            limit: None,
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
        human_seq: 3,
        starts: 1,
        progress: Mutex::new(Progress::Retry(Instant::now())),
    }
}

#[test]
fn codex_rotation_probe_cannot_follow_a_replacement_process_wall_or_person_turn() {
    let proof = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]).proven;
    let job = job(&proof);
    assert!(job.matches(&job.intent, &job.process, &proof, 3, 1));
    let mut intent = job.intent.clone();
    intent.wall = "00000000-0000-0000-0000-000000000003".into();
    assert!(!job.matches(&intent, &job.process, &proof, 3, 1));
    intent = job.intent.clone();
    intent.identity = renewal::Identity::from_native("replacement", "user");
    assert!(!job.matches(&intent, &job.process, &proof, 3, 1));
    let mut process = job.process.clone();
    process.birth += 1;
    assert!(!job.matches(&job.intent, &process, &proof, 3, 1));
    process = job.process.clone();
    process.image.inode += 1;
    assert!(!job.matches(&job.intent, &process, &proof, 3, 1));
    assert!(!job.matches(&job.intent, &job.process, &proof, 4, 1));
    assert!(!job.matches(&job.intent, &job.process, &proof, 3, 2));
}

#[test]
fn codex_rotation_index_retains_source_intent_across_candidate_changes() {
    let proof = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]).proven;
    let mut job = job(&proof);
    let original = index(&job).unwrap();
    // A destination is deliberately not in the key: a lost Ready response
    // must find its original receipt before another candidate can be chosen.
    *job.progress.lock().unwrap() = Progress::NotNeeded;
    assert_eq!(index(&job).unwrap(), original);
    job.intent.wall = "00000000-0000-0000-0000-000000000003".into();
    assert_ne!(index(&job).unwrap(), original);
    job.intent.wall = "00000000-0000-0000-0000-000000000002".into();
    job.process.birth += 1;
    assert_ne!(index(&job).unwrap(), original);
}

/// A selection as a read at `observed` leaves it. Its plan is the pool's own answer for a
/// session aterm's harness supervises, which moves nothing and reads no file
/// ([`accounts_pool::native_quota_plan`]): these tests publish through a closure, and nothing in
/// them commits the plan.
fn selection(observed: Instant) -> Selection {
    let nowhere = Path::new("/rotation-test");
    let seat = accounts_pool::Seat {
        config_dir: nowhere,
        state: nowhere,
        home: nowhere,
        now: 0,
    };
    let session = accounts_pool::Session {
        cli: accounts::Cli::Codex,
        sid: "rotation-test",
        supervisor: accounts::Supervisor::Aterm,
        env: &[],
        seen: &aterm_agent::harness::observe::Observation::Unread,
        sample: None,
        point: accounts::Point::TurnEnd,
        mode: accounts::Mode::Off,
        relaunch: relaunch::PoolRelaunch {
            dialect: aterm_agent::harness::upgrade::Dialect::Zsh,
            cd: None,
            exe: Path::new("/codex"),
            flags: &[],
            resume: None,
            home: None,
        },
    };
    Selection {
        proposal: pool::Proposal {
            plan: accounts_pool::native_quota_plan(&seat, &session, |_| ()),
            source: accounts::Row::default(),
            destination: accounts::Row::default(),
            source_identity: renewal::Identity::from_native("source", "user"),
            destination_identity: renewal::Identity::from_native("target", "user"),
            old_auth: "/old/auth.json".into(),
            new_auth: "/new/auth.json".into(),
        },
        observed,
        receipt: None,
        publish_at: None,
    }
}

/// The receipt a publication of [`selection`] answers with.
fn receipt() -> Receipt {
    Receipt {
        destination: Some(renewal::Identity::from_native("target", "user")),
        id: "rotation-test-receipt".into(),
        intent: Intent {
            purpose: renewal::Purpose::WeeklyQuota,
            thread: "00000000-0000-0000-0000-000000000001".into(),
            wall: "00000000-0000-0000-0000-000000000002".into(),
            identity: renewal::Identity::from_native("source", "user"),
            limit: None,
        },
        sid: "rotation-test".into(),
        old_pid: 4242,
        old_birth: 10,
        claim_revision: 1,
        human_seq: 3,
        old_auth: "/old/auth.json".into(),
        new_auth: "/new/auth.json".into(),
        generation: 1,
        row_stamp: String::new(),
        native_prepare: String::new(),
        expires_ms: u64::MAX,
        phase: Phase::Ready,
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: false,
        candidate: None,
        current_attempt: None,
        revoked: false,
    }
}

/// A PUBLICATION THAT KEEPS FAILING IS TRIED AGAIN, AND NOTHING IS READ FOR IT: a session asks
/// every five seconds of a selection whose publication fails each time. Each ask that is due
/// publishes once and is answered `waiting`, an ask before the next is due publishes nothing,
/// and the selection stays as it was read: ten publications in its first 45 seconds and no read
/// of any account. At 50 seconds, ten short of the observation's end, the selection is dropped
/// unpublished, and only the ask after that starts a read.
#[test]
fn codex_rotation_failing_publication_is_retried_without_a_read_and_lapses_in_time_to_commit() {
    let observed = Instant::now();
    let progress = Mutex::new(Progress::Selected(Box::new(selection(observed))));
    let (published, read) = (std::cell::Cell::new(0_u64), std::cell::Cell::new(0_u64));
    let ask_at = |after: Duration| {
        ask(
            &progress,
            observed + after,
            |_| {
                published.set(published.get() + 1);
                Err("writer-busy")
            },
            || read.set(read.get() + 1),
        )
    };
    for attempt in 0..10 {
        let due = Duration::from_secs(5 * attempt);
        assert_eq!(ask_at(due), WAITING, "{due:?}");
        assert_eq!(published.get(), attempt + 1, "{due:?}");
        assert_eq!(ask_at(due + Duration::from_millis(4_999)), WAITING);
        assert_eq!(published.get(), attempt + 1, "not due again within {due:?}");
        assert_eq!(read.get(), 0, "{due:?}");
        assert!(
            matches!(
                &*progress.lock().unwrap(),
                Progress::Selected(kept) if kept.observed == observed && kept.receipt.is_none()
            ),
            "{due:?}: the selection is kept as it was read"
        );
    }
    let lapsed = Duration::from_secs(50);
    assert_eq!(ask_at(lapsed), WAITING);
    assert_eq!((published.get(), read.get()), (10, 0));
    assert!(matches!(
        *progress.lock().unwrap(),
        Progress::Retry(at) if at == observed + lapsed
    ));
    assert_eq!(ask_at(Duration::from_secs(55)), WAITING);
    assert_eq!((published.get(), read.get()), (10, 1));
    assert!(matches!(*progress.lock().unwrap(), Progress::Reading));
}

/// A PUBLICATION THAT FAILED ONCE IS PUBLISHED AT THE NEXT ASK: the receipt writer is busy at
/// the first ask and free at the one five seconds later, which is answered with the receipt.
/// The selection is the one that was read, at the time it was read, and now holds the receipt
/// `commit_pool` compares the session's commit with. No account was read for it.
#[test]
fn codex_rotation_publication_that_fails_once_is_published_at_the_next_ask() {
    let observed = Instant::now();
    let progress = Mutex::new(Progress::Selected(Box::new(selection(observed))));
    let read = std::cell::Cell::new(0_u64);
    let receipt = receipt();
    assert_eq!(
        ask(
            &progress,
            observed,
            |_| Err("writer-busy"),
            || read.set(read.get() + 1)
        ),
        WAITING
    );
    let reply = ask(
        &progress,
        observed + Duration::from_secs(5),
        |_| Ok(receipt.clone()),
        || read.set(read.get() + 1),
    );
    assert_eq!(reply, receipt.reply());
    assert!(
        reply.starts_with("OK codex-rotation state=ready id=rotation-test-receipt "),
        "{reply}"
    );
    assert!(matches!(
        &*progress.lock().unwrap(),
        Progress::Selected(kept)
            if kept.observed == observed
                && kept.receipt.as_ref() == Some(&receipt)
                && kept.publish_at.is_none()
    ));
    assert_eq!(read.get(), 0);
}

/// A SELECTION TOO OLD TO COMMIT IS NOT PUBLISHED: a receipt is committed in the step after the
/// one that reads it, and a commit is refused once the observation is 60 seconds old. A
/// selection a millisecond short of 50 seconds old is still published. One 50 seconds old is
/// not: its ask is answered `waiting`, leaves the job to read again and starts no read itself.
#[test]
fn codex_rotation_selection_too_old_to_commit_is_not_published() {
    let observed = Instant::now();
    let receipt = receipt();
    for (age, publishes) in [(49_999, true), (50_000, false)] {
        let now = observed + Duration::from_millis(age);
        let mut progress = Progress::Selected(Box::new(selection(observed)));
        let mut published = false;
        let (reply, reads) = answer(&mut progress, now, |_| {
            published = true;
            Ok(receipt.clone())
        });
        assert_eq!(published, publishes, "{age} ms");
        assert!(!reads, "{age} ms");
        if publishes {
            assert_eq!(reply, receipt.reply());
            assert!(matches!(&progress, Progress::Selected(kept) if kept.receipt.is_some()));
        } else {
            assert_eq!(reply, WAITING);
            assert!(matches!(progress, Progress::Retry(at) if at == now));
        }
    }
}

/// The read of an ask that must start none.
fn unread() {
    panic!("this ask starts no read")
}

/// The publication of an ask that has no selection to publish.
fn unpublished(_: &Selection) -> Result<Receipt, &'static str> {
    panic!("this ask publishes nothing")
}

/// ONLY A RETRY THAT IS DUE STARTS A READ, AND IT STARTS ONE: an ask a millisecond before its
/// job's retry is due reads nothing and leaves the retry when it was. The ask at that time runs
/// the read once, with the progress already `Reading` and its lock free for the read to write
/// its result or its failure to start, and is answered `waiting`. No ask while that read is out
/// starts another, five seconds or an hour later. A job whose answer is `not-needed` is
/// answered that at any age, stays so and reads nothing: no ask turns it into a retry. None of
/// these asks publishes.
#[test]
fn codex_rotation_ask_starts_a_read_only_from_a_retry_that_is_due() {
    let due = Instant::now() + Duration::from_millis(RETRY_MS);
    let progress = Mutex::new(Progress::Retry(due));
    assert_eq!(
        ask(
            &progress,
            due - Duration::from_millis(1),
            unpublished,
            unread
        ),
        WAITING
    );
    assert!(matches!(*progress.lock().unwrap(), Progress::Retry(at) if at == due));
    let reads = std::cell::Cell::new(0_u32);
    let read = || {
        reads.set(reads.get() + 1);
        assert!(
            matches!(progress.try_lock().as_deref(), Ok(Progress::Reading)),
            "the read runs with the progress unlocked and already reading"
        );
    };
    assert_eq!(ask(&progress, due, unpublished, read), WAITING);
    assert_eq!(reads.get(), 1, "the ask that is due runs the read");
    for later in [RETRY_MS, 3_600_000] {
        let now = due + Duration::from_millis(later);
        assert_eq!(ask(&progress, now, unpublished, read), WAITING);
        assert_eq!(reads.get(), 1, "one read at a time, {later} ms on");
        assert!(matches!(*progress.lock().unwrap(), Progress::Reading));
    }

    let ended = Mutex::new(Progress::NotNeeded);
    for later in [0, RETRY_MS, OBSERVATION_MS, 7 * 86_400_000] {
        let now = due + Duration::from_millis(later);
        assert_eq!(
            ask(&ended, now, unpublished, unread),
            "OK codex-rotation state=not-needed\n",
            "{later} ms on"
        );
        assert!(matches!(*ended.lock().unwrap(), Progress::NotNeeded));
    }
}

/// How long a test waits for a job's worker: far longer than a read that ends at its first
/// check takes.
const PATIENCE: Duration = Duration::from_secs(60);

/// `prepare` ASKS ITS JOB AT THE PRESENT TIME, WITH THE LANE'S OWN READ AND PUBLICATION: the
/// session's first ask makes its job and starts the job's worker, whose result is written to
/// that job. Here the read ends at its first check (this host names no configuration, so no
/// roster is opened and no account is read), and it does not stand, because the session is
/// held: the job retries five seconds after this ask or later. Every later ask is the same
/// job's. One before a retry is due leaves the retry when it was. One of a job with no rotation
/// coming is answered `not-needed` and leaves it so. One of a job with a selection publishes
/// it; the publication is refused, since the asking process proved no claim here, and the
/// selection is kept as it was read, to be published again five seconds after this ask.
#[test]
fn codex_rotation_prepare_asks_its_job_now_with_the_lanes_read_and_publication() {
    let handle = crate::session_store::test_handle_on(9131, -1);
    let ctx = Arc::clone(&handle.ctx);
    // Under a hold the worker's check ends before it looks for the asking process's record.
    ctx.fabric
        .seed_holds(crate::fabric::parse_holds(["local mine"]), &ctx.timeline);
    assert!(ctx.fabric.hold().is_some());
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let mut env = Env::from_process().unwrap();
    env.home = Some(root.join("home"));
    env.state = root.join("state");
    env.config = None;
    let pid = std::process::id();
    let proof = crate::self_claim::Proven {
        pid,
        start: aterm_agent::harness::restart_stamp::kernel_start(pid)
            .expect("this process's kernel start"),
    };
    let intent = receipt().intent;
    let copy = self_claimed::Copy {
        lifetime: None,
        pid,
        start: proof.start.clone(),
        home: root.join("home/.codex"),
        seen: self_claimed::Seen::Removed,
        resume: Ok(self_claimed::Resume {
            argv: vec!["/codex".into(), "resume".into(), intent.thread.clone()],
            session: intent.thread.clone(),
            codex_home: None,
            codex_auth_path: None,
            cwd: None,
        }),
    };
    let current = aterm_json::json!({"owed": {"rotation": {"starts": 1}}});
    let ask_the_lane = || prepare(&ctx, &env, &proof, &copy, &current, intent.clone());
    let job_of_the_session = || {
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .rotation_prepare
            .clone()
    };
    assert!(job_of_the_session().is_none());
    let retry = Duration::from_millis(RETRY_MS);

    let before = Instant::now();
    assert_eq!(ask_the_lane(), WAITING);
    let job = job_of_the_session().expect("the first ask makes the job");
    let since = Instant::now();
    while matches!(*job.progress.lock().unwrap(), Progress::Reading) {
        assert!(
            since.elapsed() < PATIENCE,
            "hang: the read the first ask starts never wrote its result"
        );
        aterm_uds::deadline::poll_yield();
    }
    assert!(
        matches!(*job.progress.lock().unwrap(), Progress::Retry(at) if at >= before + retry),
        "the first ask's read ran, and its result is tried again five seconds on: {:?}",
        job.progress.lock().unwrap()
    );

    let far = Instant::now() + Duration::from_secs(3600);
    *job.progress.lock().unwrap() = Progress::Retry(far);
    assert_eq!(ask_the_lane(), WAITING);
    assert!(matches!(*job.progress.lock().unwrap(), Progress::Retry(at) if at == far));

    *job.progress.lock().unwrap() = Progress::NotNeeded;
    assert_eq!(ask_the_lane(), "OK codex-rotation state=not-needed\n");
    assert!(matches!(*job.progress.lock().unwrap(), Progress::NotNeeded));

    let observed = Instant::now();
    *job.progress.lock().unwrap() = Progress::Selected(Box::new(selection(observed)));
    let before = Instant::now();
    assert_eq!(ask_the_lane(), WAITING);
    let after = Instant::now();
    assert!(
        matches!(
            &*job.progress.lock().unwrap(),
            Progress::Selected(kept)
                if kept.observed == observed
                    && kept.receipt.is_none()
                    && kept
                        .publish_at
                        .is_some_and(|at| before + retry <= at && at <= after + retry)
        ),
        "the refused publication is due again five seconds after its ask: {:?}",
        job.progress.lock().unwrap()
    );
    assert!(
        job_of_the_session().is_some_and(|last| Arc::ptr_eq(&last, &job)),
        "every ask was the same job's"
    );
}
