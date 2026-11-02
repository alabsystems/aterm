// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::fsx;

#[test]
fn a_later_expired_push_cannot_replace_a_pending_table_or_acknowledge_it() {
    for wait in [Duration::ZERO, Duration::from_millis(100)] {
        let pusher = Pusher::default();
        let board = pusher.0.clone();
        let _serving = board.serving();
        let first = pusher.clone();
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let caller = thread::spawn(move || first.push_until([1; 32], deadline));
        // Observe registration without letting the runtime take the first table yet.
        loop {
            if board.desk().ask.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "first push never registered");
            thread::sleep(Duration::from_millis(1));
        }
        let expired = pusher.push_until([2; 32], Instant::now() + wait);
        let taken = board.take().expect("the original pending push");
        board.settle(taken.id, Pushed::Reached);
        let first_result = caller.join().expect("first caller");
        assert_eq!(expired, Pushed::TimedOut);
        assert_eq!(
            taken.table, [1; 32],
            "an expired caller replaced the pending table"
        );
        assert_eq!(first_result, Pushed::Reached);
    }
}

fn next_push(board: &Board, deadline: Instant) -> Ask {
    loop {
        if let Some(ask) = board.take() {
            return ask;
        }
        assert!(Instant::now() < deadline, "push never registered");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn concurrent_pushes_get_their_own_result_and_ignore_stale_completion() {
    let pusher = Pusher::default();
    let board = pusher.0.clone();
    let _serving = board.serving();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let first = pusher.clone();
    let first_caller = thread::spawn(move || first.push_until([1; 32], deadline));
    let first = next_push(&board, deadline);
    let second = pusher.clone();
    let second_caller = thread::spawn(move || second.push_until([2; 32], deadline));
    board.settle(first.id, Pushed::Reached);
    assert_eq!(first_caller.join().unwrap(), Pushed::Reached);
    let second = next_push(&board, deadline);
    assert_eq!(second.table, [2; 32]);
    board.settle(first.id, Pushed::TimedOut);
    let priority = *board.first();
    board.settle(second.id, Pushed::NoPeers);
    assert_eq!(second_caller.join().unwrap(), Pushed::NoPeers);
    assert_eq!(
        priority,
        Some([2; 32]),
        "stale completion cleared the current table"
    );
}

#[test]
fn an_abandoned_push_round_releases_its_priority_after_a_later_caller_expires() {
    let board = Board::default();
    let _serving = board.serving();
    {
        let mut desk = board.desk();
        desk.asked = 1;
        desk.ask = Some(Ask {
            id: 1,
            table: [1; 32],
            deadline: Instant::now(),
        });
    }
    let first = board.take().unwrap();
    // Caller one expired while its round ran. Caller two then registered and expired
    // before the owner took its request. Only round one's priority remains active.
    board.desk().asked = 2;
    board.settle(first.id, Pushed::Reached);
    assert_eq!(
        *board.first(),
        None,
        "the abandoned round left its table prioritized"
    );
    assert!(
        board.desk().settled.is_none(),
        "round one cannot answer caller two"
    );
}

#[test]
fn stopping_repair_wakes_every_push_caller() {
    let pusher = Pusher::default();
    let board = pusher.0.clone();
    let serving = board.serving();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let first = pusher.clone();
    let first_caller = thread::spawn(move || first.push_until([1; 32], deadline));
    next_push(&board, deadline);
    let second_caller = thread::spawn(move || pusher.push_until([2; 32], deadline));
    drop(serving);
    assert_eq!(first_caller.join().unwrap(), Pushed::NotServing);
    assert_eq!(second_caller.join().unwrap(), Pushed::NotServing);
}

#[test]
fn a_rapid_restart_cancels_unfinished_pushes_and_keeps_completed_results() {
    for stage in 0..3 {
        let pusher = Pusher::default();
        let board = pusher.0.clone();
        let serving = board.serving();
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let first = pusher.clone();
        let caller = thread::spawn(move || first.push_until([1; 32], deadline));
        loop {
            if board.desk().ask.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "push never registered");
            thread::sleep(Duration::from_millis(1));
        }
        let taken = (stage > 0).then(|| board.take().unwrap());
        // Hold the desk across the production stop/start transitions so the waiting
        // caller cannot observe the brief false value between service generations.
        {
            let mut desk = board.desk();
            if stage == 2 {
                // The owner had already completed this request before it stopped.
                desk.settled = Some((taken.unwrap().id, Pushed::Reached));
            }
            desk.stop();
            *board.first() = None;
            desk.start();
        }
        board.changed.notify_all();
        let result = caller.join().unwrap();
        assert_eq!(
            result,
            if stage == 2 {
                Pushed::Reached
            } else {
                Pushed::NotServing
            },
            "the push outcome crossed a service generation"
        );
        let next = thread::spawn(move || pusher.push_until([2; 32], deadline));
        let ask = next_push(&board, deadline);
        board.settle(ask.id, Pushed::Reached);
        assert_eq!(next.join().unwrap(), Pushed::Reached);
        drop(serving);
        assert!(
            board.desk().serving,
            "an old guard stopped the replacement service"
        );
    }
}

#[test]
fn diagnostics_keep_live_policy_and_owner_lease_separate_from_the_observer() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let home = aterm_tempfile::Builder::new()
        .prefix("rdg-")
        .tempdir_in("/tmp")
        .unwrap();
    let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let paths = lab.paths[0].clone();
    let config = home.path().join("aterm.toml");
    fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    let observer_home = aterm_tempfile::Builder::new()
        .prefix("runtime-diagnostic-")
        .tempdir_in("/tmp")
        .unwrap();
    let observer_config = observer_home.path().join("aterm.toml");
    fsx::write_atomic(&observer_config, b"[commons]\nenabled=true\n", 0o600).unwrap();
    let live_observer = Observer::open(&config, ObservationHandle::default()).unwrap();
    let lease = owner::Lease::acquire(&paths.dir.join("commons"))
        .unwrap()
        .unwrap();
    let recovery = paths.dir.join("commons/writer-generation");
    fsx::ensure_private_dir(&recovery).unwrap();
    let selection = recovery.join("selection");
    fsx::write_atomic(&selection, b"incomplete recovery", 0o600).unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let (observe, wait) = statuses();
    let runtime = Runtime::start_mode_with_sources(
        paths.clone(),
        config.clone(),
        (observer_config.clone(), false),
        observe,
        Network {
            opens: Some(opens.clone()),
            stand_in: Some(Arc::new(AtomicBool::new(true))),
            ..Network::default()
        },
        false,
        |_| {},
    )
    .unwrap();
    wait(Status::Disabled);
    assert!(!crate::network_state::history_exists(&observer_config));
    fsx::write_atomic(&config, b"[commons]\nenabled=true\n", 0o600).unwrap();
    wait(Status::Waiting);
    assert!(crate::network_state::history_exists(&observer_config));
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    runtime.stop();
    assert_eq!(std::fs::read(&selection).unwrap(), b"incomplete recovery");
    assert!(!recovery.join("home").exists());

    // The ordinary recovery pass sees the incomplete selection and refuses;
    // a copied-policy observation must not run that persistent transition.
    let (observe, wait) = statuses();
    let runtime = Runtime::start_mode_with_sources(
        paths,
        config,
        (observer_config, true),
        observe,
        Network {
            opens: Some(opens.clone()),
            stand_in: Some(Arc::new(AtomicBool::new(true))),
            ..Network::default()
        },
        false,
        |_| {},
    )
    .unwrap();
    wait(Status::Refused("writer recovery"));
    runtime.stop();
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&selection).unwrap(), b"incomplete recovery");
    drop((lease, live_observer));
}

/// How many tries an owner makes in ten virtual minutes of one persistent refusal, each try
/// taking `each` before it is refused, under `wait` (the wait after each try).
fn tries_in_ten_minutes(each: Duration, mut wait: impl FnMut() -> Duration) -> usize {
    let mut elapsed = Duration::ZERO;
    let mut tries = 0;
    while elapsed < Duration::from_secs(600) {
        tries += 1;
        elapsed += each + wait();
    }
    tries
}

/// Real control-writer and custody contention use the runtime's failure adapter,
/// then its retry scheduler. Virtual attempts require no wall-clock delay.
#[test]
fn local_writer_contention_keeps_a_bounded_retry_instead_of_failure_backoff() {
    let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let held = crate::table::Session::open(&bootstrap).unwrap();
    let mut refusals = Refusals::new(false);
    let mut waits = Vec::new();
    for _ in 0..8 {
        let error = Endpoint::from_bootstrap_until(&bootstrap, Instant::now())
            .err()
            .expect("the other local session owns the control journal");
        let status = refusal("control endpoint")(error);
        assert_eq!(status, Status::Retrying("control endpoint"));
        waits.push(refusals.after(Some(status), None));
    }
    assert_eq!(
        waits,
        vec![POLL; 8],
        "temporary writer contention must not accumulate the permanent-failure backoff"
    );
    drop(held);
    assert!(Endpoint::from_bootstrap(&bootstrap).unwrap().is_some());

    let custody = aterm_uds::private_file::PrivateLock::take(
        &lab.paths[0].dir,
        "commons.lock",
        Duration::ZERO,
        "held",
    )
    .unwrap();
    let error = Bootstrap::required_with_journal_until(&lab.paths[0], Instant::now())
        .err()
        .expect("the other custody observation still owns its lock");
    let status = refusal("secrets custody")(error);
    assert_eq!(status, Status::Retrying("secrets custody"));
    assert_eq!(refusals.after(Some(status), None), POLL);
    drop(custody);
}

#[test]
fn unclassified_io_and_error_words_keep_persistent_refusal_backoff() {
    let statuses = [
        refusal("transport")(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            crate::BUSY,
        )),
        refusal("transport")(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "network timeout",
        )),
        refusal("transport")(crate::Error::new(crate::BUSY)),
    ];
    let mut refusals = Refusals::new(false);
    let waits: Vec<_> = statuses
        .into_iter()
        .map(|status| {
            assert_eq!(status, Status::Refused("transport"));
            refusals.after(Some(status), None)
        })
        .collect();
    let base = TICK * 2;
    assert_eq!(waits, [base, base * 2, base * 4]);
}

/// A window's owner tries a persistent refusal (local configuration or trust state) again at a
/// doubling wait, up to [`REFUSED_MAX`], not every 200 ms: each try rebuilds custody, an endpoint
/// and a transport. The background owner keeps [`POLL`], so it yields to a window that opens
/// within one. NEGATIVE CONTROL: the fixed wait the owner kept before (`TICK * 2` after every
/// refusal) makes 3000 tries of an instant refusal in ten minutes and 250 of one refused at its
/// first poll; the doubling makes at most 27 of either.
#[test]
fn a_persistent_refusal_backs_off_to_thirty_seconds_and_a_served_run_resets_it() {
    let base = TICK * 2;
    for each in [Duration::ZERO, POLL + base] {
        let mut refusals = Refusals::new(false);
        let doubling = tries_in_ten_minutes(each, || {
            refusals.after(Some(Status::Refused("generation guard")), None)
        });
        let fixed = tries_in_ten_minutes(each, || base);
        assert!(doubling <= 27, "{doubling} tries after {each:?} each");
        assert!(fixed >= 200, "the fixed control made {fixed} tries");
    }

    let mut refusals = Refusals::new(false);
    let refused = Some(Status::Refused("custody"));
    let waits: Vec<_> = (0..10).map(|_| refusals.after(refused, None)).collect();
    assert_eq!(waits[0], base, "the first refusal is tried again as before");
    assert_eq!(waits[1], base * 2);
    assert_eq!(waits[2], base * 4);
    assert_eq!(*waits.last().unwrap(), REFUSED_MAX);
    // A try that served briefly before it was refused (an owner refused at each poll) keeps the
    // doubling; one that served for the longest wait, or one that ended to rebuild, resets it.
    assert_eq!(
        refusals.after(refused, Some(POLL)),
        REFUSED_MAX,
        "a refusal at the first poll reset the backoff"
    );
    assert_eq!(refusals.after(refused, Some(REFUSED_MAX)), base);
    assert_eq!(refusals.after(refused, None), base * 2);
    assert_eq!(refusals.after(None, None), base);
    assert_eq!(refusals.after(refused, None), base);
    // An offline outcome keeps the mode's wait and the streak.
    assert_eq!(refusals.after(Some(Status::Offline), None), base);
    assert_eq!(refusals.after(refused, None), base * 2);
    let mut background = Refusals::new(true);
    assert!(
        (0..6).all(|_| background.after(refused, None) == POLL),
        "the background owner backed off"
    );
}

/// The owner loop waits as [`Refusals`] says, at its gate and after a refused try. A window's
/// owner refused by an unreadable configuration before its first LAN use, and one refused by an
/// incomplete writer recovery past it, wait 200, 400, 800 and 1600 ms between tries; the
/// background owner waits [`POLL`] each time. NEGATIVE CONTROL: the fixed `TICK * 2` the loop
/// kept before after each refusal, at its gate or past it, reads four waits of 200 ms.
#[test]
fn the_owner_loop_waits_out_a_persistent_refusal_at_a_doubling_interval() {
    let home = aterm_tempfile::tempdir().unwrap();
    let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let paths = lab.paths[0].clone();
    let recovery = paths.dir.join("commons/writer-generation");
    fsx::ensure_private_dir(&recovery).unwrap();
    fsx::write_atomic(&recovery.join("selection"), b"incomplete recovery", 0o600).unwrap();
    let unreadable = home.path().join("unreadable/aterm.toml");
    fsx::ensure_private_dir(unreadable.parent().unwrap()).unwrap();
    fsx::write_atomic(&unreadable, b"[commons\n", 0o600).unwrap();
    let enabled = home.path().join("enabled/aterm.toml");
    fsx::ensure_private_dir(enabled.parent().unwrap()).unwrap();
    fsx::write_atomic(&enabled, b"[commons]\nenabled=true\n", 0o600).unwrap();
    let doubling = [TICK * 2, TICK * 4, TICK * 8, TICK * 16];
    for (config, background, refused, expected) in [
        (&unreadable, false, "configuration", &doubling[..]),
        (&enabled, false, "writer recovery", &doubling[..]),
        (&enabled, true, "writer recovery", &[POLL, POLL][..]),
    ] {
        let retries = Arc::new(Mutex::new(Vec::new()));
        let (observe, wait) = statuses();
        let runtime = Runtime::start_mode_with_sources(
            paths.clone(),
            config.clone(),
            (config.clone(), true),
            observe,
            Network {
                retries: Some(retries.clone()),
                ..Network::default()
            },
            background,
            |_| {},
        )
        .unwrap();
        wait(Status::Refused(refused));
        let until = Instant::now() + Duration::from_secs(60);
        while retries.lock().unwrap().len() < expected.len() {
            assert!(
                Instant::now() < until,
                "{refused}: {:?}",
                retries.lock().unwrap()
            );
            thread::sleep(TICK / 10);
        }
        runtime.stop();
        let retries = retries.lock().unwrap();
        assert_eq!(
            retries[..expected.len()],
            *expected,
            "{refused}, background {background}"
        );
    }
}

#[test]
fn delayed_worker_completion_does_not_refresh_old_authentication() {
    let root = aterm_tempfile::tempdir().unwrap();
    let handle = crate::network_state::ObservationHandle::default();
    let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
    let authenticated = Instant::now();
    let completed = authenticated + Duration::from_secs(60);
    let at_ms = contact_time(61_000, completed, authenticated);
    assert_eq!(at_ms, 1000);
    observer.contact(
        MachineIdentity {
            machine_id: [1; 16],
            incarnation: [2; 16],
            label: None,
        },
        at_ms,
        authenticated,
    );
    observer.observe(Visibility::Active, 0, "ok", 61_000, completed);
    assert_eq!(
        handle.snapshot().peers[0].reachability,
        aterm_types::network_state::Reachability::LastSeen
    );
    assert_eq!(
        contact_time(500, completed, authenticated),
        0,
        "invalid wall mapping is unknown, not a fresh timestamp"
    );
}

#[test]
fn missing_configuration_is_default_on_but_never_bootstraps_a_vault() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    assert!(enabled(&config).unwrap());
    let (send, receive) = std::sync::mpsc::channel();
    let runtime = Runtime::start(paths.clone(), config.clone(), LanGate::open(), move |s| {
        let _ = send.send(s);
    })
    .unwrap();
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(60)).unwrap(),
        Status::Dormant
    );
    let observation = runtime.network_observation();
    runtime.stop();
    assert_eq!(observation.snapshot().visibility, Visibility::Unavailable);
    assert!(!paths.dir.exists());
    fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    assert!(!enabled(&config).unwrap());
    for source in ["[commons]\nenabled='false'", "commons=false", "[commons"] {
        fsx::write_atomic(&config, source.as_bytes(), 0o600).unwrap();
        assert!(enabled(&config).is_err());
    }
    fsx::write_atomic(&config, b"[commons]\n", 0o600).unwrap();
    assert!(enabled(&config).unwrap());
}

#[test]
fn running_host_observes_exclusion_and_reenabling_without_restart() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    let (send, receive) = std::sync::mpsc::channel();
    let runtime = Runtime::start(
        paths.clone(),
        config.clone(),
        LanGate::open(),
        move |status| {
            let _ = send.send(status);
        },
    )
    .unwrap();
    let wait = |expected| {
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(60)).unwrap(),
            expected
        )
    };
    wait(Status::Dormant);
    fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    wait(Status::Disabled);
    fsx::write_atomic(&config, b"[commons]\nenabled=true\n", 0o600).unwrap();
    wait(Status::Dormant);
    runtime.stop();
    assert!(!paths.dir.exists());
}

#[test]
fn running_generation_withdraws_after_same_envelope_rebinds_to_another_source() {
    use aterm_secrets::{b64, client, transport::Source};

    let home = aterm_tempfile::Builder::new()
        .prefix("commons-source-")
        .tempdir_in("/tmp")
        .unwrap();
    let paths = Paths::for_home(home.path());
    let source = home.path().join("team.vault");
    client::create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let original = commons::cached(&paths).unwrap().unwrap();
    let config = home.path().join("aterm.toml");
    let rebound = home.path().join("rebound");
    fsx::ensure_private_dir(&rebound).unwrap();
    let target = rebound.join("team.vault");
    // Published envelopes are encrypted 0644 files; use transport's bounded
    // public-file reader rather than the private custody-file reader.
    let bytes = fsx::read_public(&source, aterm_secrets::envelope::MAX_ENVELOPE)
        .unwrap()
        .unwrap();
    fsx::write_atomic(&target, bytes.as_slice(), 0o600).unwrap();
    let stop = Arc::new(Stop::default());
    let (finished, completion) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let mut history = Observer::open(&config, ObservationHandle::default()).unwrap();
            let mut rebound = false;
            let mut report = |_: Status| {
                if rebound {
                    return;
                }
                let configured = client::config(&paths).unwrap().unwrap();
                client::join(
                    &paths,
                    &client::JoinOptions {
                        source: format!(
                            "{}#{}",
                            target.display(),
                            b64::encode_padded(&configured.signer.unwrap())
                        ),
                        host: Some(configured.host),
                        interactive: false,
                        wait: Duration::ZERO,
                        confirm: &|_| false,
                        password: client::JoinPassword::Given(None),
                        fetched: None,
                        requested: &|| {},
                        say: &|_| {},
                    },
                )
                .unwrap();
                let current = commons::cached(&paths).unwrap().unwrap();
                assert_eq!(original.sequence(), current.sequence());
                assert_eq!(original.digest(), current.digest());
                assert_eq!(original.recipient(), current.recipient());
                assert_eq!(original.control(), current.control());
                assert!(!original.same_observation(&current));
                assert_eq!(
                    client::config(&paths).unwrap().unwrap().source,
                    Source::Path(target.clone())
                );
                // This callback runs on the owner thread. The next generation
                // poll therefore sees the fully rebound, available source,
                // never join's transient unavailable/materialization states.
                rebound = true;
            };
            let result = serve(
                &paths,
                &config,
                &stop,
                &mut report,
                &mut history,
                &Network::default(),
                &|_| {},
            );
            assert!(rebound);
            finished.send(result).unwrap();
        });
        let result = completion.recv_timeout(Duration::from_secs(60));
        stop.request();
        worker.join().unwrap();
        assert_eq!(
            result.expect("the old source generation must withdraw"),
            Ok(())
        );
    });
}

fn lease_scenario(action: &str) -> bool {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("commons");
    let lease = owner::Lease::acquire(&dir).unwrap().unwrap();
    match action {
        "Acquire" => lease.recheck().is_ok(),
        "Contend" => owner::Lease::acquire(&dir).unwrap().is_some(),
        "Transfer" => {
            drop(lease);
            owner::Lease::acquire(&dir).unwrap().is_some()
        }
        "Replace" => {
            std::fs::rename(dir.join("runtime.lock"), dir.join("old.lock")).unwrap();
            fsx::write_new(&dir.join("runtime.lock"), b"", 0o600).unwrap();
            lease.recheck().is_ok()
        }
        "Remove" => {
            std::fs::remove_file(dir.join("runtime.lock")).unwrap();
            lease.recheck().is_ok()
        }
        _ => panic!("unknown lease action"),
    }
}

#[test]
fn actual_owner_lease_conforms_and_duplicate_owner_mutant_is_detected() {
    let model = aterm_models_db::transport_owner_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for (n, action) in ["Acquire", "Contend", "Transfer", "Replace", "Remove"]
        .iter()
        .enumerate()
    {
        let actual = lease_transition(&initial, action);
        assert_eq!(actual["scenario"], n as i64 + 1);
        assert!(
            model.successors(action, &initial).contains(&actual),
            "{action}: {actual:?}"
        );
    }
    let mut mutant = initial.clone();
    mutant.insert("scenario", 2);
    mutant.insert("usable", 1);
    assert!(!model.successors("Contend", &initial).contains(&mutant));
    assert!(!model.check_invariant("OnlyExclusiveIntactOwner", &mutant));
}

fn lease_transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
    let scenario = match action {
        "Acquire" => 1,
        "Contend" => 2,
        "Transfer" => 3,
        "Replace" => 4,
        "Remove" => 5,
        _ => panic!("unknown lease action"),
    };
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("usable", i64::from(lease_scenario(action)));
    next
}

/// A gate whose turn the test sets, sending each look the owner takes.
fn test_gate(turn: &Arc<Mutex<LanTurn>>) -> (LanGate, std::sync::mpsc::Receiver<LanTurn>) {
    let (look, looks) = std::sync::mpsc::channel();
    let turn = turn.clone();
    let gate = LanGate::asking(move || {
        let now = *turn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = look.send(now);
        now
    });
    (gate, looks)
}

/// Each status the owner reports, in order, and a one-minute hang detector to wait for one.
fn statuses() -> (impl Fn(Status) + Send + 'static, impl Fn(Status)) {
    let (send, receive) = std::sync::mpsc::channel();
    let receive = Mutex::new(receive);
    (
        move |status| {
            let _ = send.send(status);
        },
        move |expected| {
            assert_eq!(
                receive
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap(),
                expected
            );
        },
    )
}

const HELD: &str = "Waits for the test to hand it the LAN";

/// macOS asks its Local Network question at an app's first LAN use, which System State's step is
/// meant to make (`docs/DESIGN-system-state-2026-10-21.md` §6). A held gate keeps the owner
/// before its observer (no history) and its transport (no socket), in a window's runtime and in
/// `aterm node serve`'s, and the owner publishes the wait; once the gate opens, the owner observes
/// and opens its transport (presence reports Dormant only after that open).
#[test]
fn a_held_gate_opens_no_observer_and_no_socket_until_it_opens() {
    for background in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        let config = home.path().join("aterm.toml");
        let turn = Arc::new(Mutex::new(LanTurn::Held(HELD)));
        let (lan, looks) = test_gate(&turn);
        let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let network = Network {
            lan,
            opens: Some(opens.clone()),
            ..Network::default()
        };
        let (observe, wait) = statuses();
        let runtime = if background {
            Runtime::start_background_on(paths.clone(), config.clone(), observe, network)
        } else {
            Runtime::start_on(paths.clone(), config.clone(), observe, network)
        }
        .unwrap();
        wait(Status::LanHeld);
        for _ in 0..2 {
            assert_eq!(
                looks.recv_timeout(Duration::from_secs(60)).unwrap(),
                LanTurn::Held(HELD)
            );
        }
        assert_eq!(
            opens.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "background {background}: no transport while held"
        );
        let observation = runtime.network_observation().snapshot();
        assert_eq!(observation.visibility, Visibility::Waiting);
        assert_eq!(observation.detail, HELD);
        assert!(
            !crate::network_state::history_exists(&config),
            "background {background}: no observer while held"
        );
        *turn.lock().unwrap() = LanTurn::Open("the test handed it the LAN");
        wait(Status::Dormant);
        assert!(opens.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        assert!(crate::network_state::history_exists(&config));
        runtime.stop();
        assert!(!paths.dir.exists());
    }
}

/// `[commons] enabled = false` reads Disabled at once, without asking the gate (nothing is
/// waited for), and opens no observer; enabled again, the gate holds and then opens.
#[test]
fn a_disabled_commons_reads_disabled_without_asking_its_gate() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    let turn = Arc::new(Mutex::new(LanTurn::Held(HELD)));
    let (lan, looks) = test_gate(&turn);
    let (observe, wait) = statuses();
    let runtime = Runtime::start_on(
        paths.clone(),
        config.clone(),
        observe,
        Network {
            lan,
            ..Network::default()
        },
    )
    .unwrap();
    wait(Status::Disabled);
    assert!(looks.try_recv().is_err(), "a disabled Commons asks nothing");
    assert_eq!(
        runtime.network_observation().snapshot().visibility,
        Visibility::Disabled
    );
    assert!(!crate::network_state::history_exists(&config));
    fsx::write_atomic(&config, b"[commons]\nenabled=true\n", 0o600).unwrap();
    wait(Status::LanHeld);
    assert_eq!(
        looks.recv_timeout(Duration::from_secs(60)).unwrap(),
        LanTurn::Held(HELD)
    );
    *turn.lock().unwrap() = LanTurn::Open("the test handed it the LAN");
    wait(Status::Dormant);
    runtime.stop();
}

/// `aterm node serve` yields to a window before its gate: while a window claims Commons it
/// waits without a look, and only once the window is gone does its own gate hold it.
#[test]
fn a_background_owner_yields_to_a_window_before_asking_its_gate() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    let claim = owner::WindowClaim::acquire(&crate::network_state::directory(&config)).unwrap();
    let turn = Arc::new(Mutex::new(LanTurn::Held(HELD)));
    let (lan, looks) = test_gate(&turn);
    let (observe, wait) = statuses();
    let runtime = Runtime::start_background_on(
        paths.clone(),
        config.clone(),
        observe,
        Network {
            lan,
            ..Network::default()
        },
    )
    .unwrap();
    wait(Status::Waiting);
    assert!(looks.try_recv().is_err(), "a window owns Commons: no look");
    drop(claim);
    wait(Status::LanHeld);
    assert_eq!(
        looks.recv_timeout(Duration::from_secs(60)).unwrap(),
        LanTurn::Held(HELD)
    );
    assert!(!crate::network_state::history_exists(&config));
    runtime.stop();
}

/// A disabled Commons and a held gate leave the owner with no observer, and so with no relay of
/// its observation; it serves its wait on a relay of its own instead, so `aterm db
/// network-state` in another process hears Disabled or the gate's reason live, as it heard an
/// open owner's Disabled. Once the gate opens, the observer's relay answers in its place.
#[test]
fn a_held_or_disabled_owner_serves_its_wait_to_other_local_processes() {
    // Darwin limits Unix socket paths to 103 bytes: keep the observer's sockets in a short root.
    let home = aterm_tempfile::Builder::new()
        .prefix("commons-held-")
        .tempdir_in("/tmp")
        .unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    let turn = Arc::new(Mutex::new(LanTurn::Held(HELD)));
    let (lan, _looks) = test_gate(&turn);
    let (observe, wait) = statuses();
    let runtime = Runtime::start_on(
        paths.clone(),
        config.clone(),
        observe,
        Network {
            lan,
            ..Network::default()
        },
    )
    .unwrap();
    let live = || {
        crate::network_state::read_live_publication(&config)
            .expect("the owner's wait is served live")
            .observation()
            .clone()
    };
    wait(Status::Disabled);
    let disabled = live();
    assert_eq!(disabled.visibility, Visibility::Disabled);
    assert_eq!(disabled.detail, "Network visibility disabled");
    assert_eq!(disabled.via_local_process, Some(std::process::id()));
    assert!(!crate::network_state::history_exists(&config));
    fsx::write_atomic(&config, b"[commons]\nenabled=true\n", 0o600).unwrap();
    wait(Status::LanHeld);
    let held = live();
    assert_eq!(held.visibility, Visibility::Waiting);
    assert_eq!(held.detail, HELD);
    assert_eq!(
        crate::network_state::read_live_observation(&config)
            .expect("a process with no runtime of its own hears the owner's wait")
            .visibility,
        Visibility::Waiting
    );
    assert!(!crate::network_state::history_exists(&config));
    *turn.lock().unwrap() = LanTurn::Open("the test handed it the LAN");
    wait(Status::Dormant);
    let sockets = crate::network_state::directory(&config);
    assert!(
        !sockets.join("waiting.sock").exists(),
        "an open owner serves no wait"
    );
    assert!(
        sockets.join("observer.sock").exists(),
        "the observer's relay serves in its place"
    );
    runtime.stop();
}

/// The OS's Local Network permission, withheld, granted, withdrawn and granted again, as the
/// isolated attachment's flag stands for it: a refused app's LAN sends fail (EHOSTUNREACH), so
/// while the flag is off the runtime's own refresh and tick drop each link whose beacon fails, the
/// receiver's listener with it, and the observation reads Unavailable; once it is on, the next
/// refresh opens the link and repair catches up.
#[test]
fn lan_permission_changes_close_and_reopen_the_runtimes_transport() {
    use super::auto_sync_tests::{Attachment, wait_network};
    use crate::{accounts::proposals::Proposals, database::Change};
    use aterm_net::lan::{Connection, Multicast, Scope as LinkScope};
    use aterm_secrets::commons::roster::Role;
    use std::{net::Ipv4Addr, sync::atomic::Ordering};

    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    let change = |key: &str| Change {
        collection: "accounts".into(),
        key: key.into(),
        value: Some(aterm_json::json!({"provider":"anthropic","method":"oauth"})),
    };
    let first = source
        .write([31; 16], vec![change("before-permission")])
        .unwrap();
    assert!(!lab.paths[1].dir.join("commons/tables").exists());
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-permission-")
        .tempdir_in("/tmp")
        .unwrap();
    let source_observer = observers.path().join("0");
    let receiver_observer = observers.path().join("1");
    fsx::ensure_private_dir(&source_observer).unwrap();
    fsx::ensure_private_dir(&receiver_observer).unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    let (send, receive) = std::sync::mpsc::channel();
    let sender = Runtime::start_on_with_reports(
        lab.paths[0].clone(),
        source_observer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(Attachment::new(port)),
            ..Network::default()
        },
        move |report| {
            let _ = send.send(report);
        },
    )
    .unwrap();
    let attachment = Attachment::new(port);
    attachment.lan_allowed.store(false, Ordering::Release);
    let runtime = Runtime::start_on(
        lab.paths[1].clone(),
        receiver_observer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(attachment.clone()),
            ..Network::default()
        },
    )
    .unwrap();
    let observation = runtime.network_observation();
    wait_network(&observation, "permission holds discovery", |state| {
        state.visibility == Visibility::Unavailable
    });
    attachment.lan_allowed.store(true, Ordering::Release);
    attachment.wait_for(
        table.digest(),
        1,
        "grant discovers the first application table",
    );
    let received = Proposals::at(lab.paths[1].clone());
    let before_pause = received.snapshot().unwrap();
    assert_eq!(before_pause.operations.len(), 1);
    assert_eq!(before_pause.operations[0].header.id, first.id);
    assert_eq!(before_pause.cells, source.snapshot().unwrap().cells);

    // Learn and positively probe the receiver's real listener before withdrawal.
    // The isolated discovery group contains only these two native owners.
    let deadline = Instant::now() + Duration::from_secs(60);
    let listening = loop {
        let report = receive
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("sender reported the receiver's advertised endpoint");
        if let Some(target) = report.target
            && Connection::connect(
                target,
                LinkScope::Loopback,
                deadline.saturating_duration_since(Instant::now()),
            )
            .is_ok()
        {
            break target;
        }
    };
    attachment.lan_allowed.store(false, Ordering::Release);
    wait_network(
        &observation,
        "revocation withdraws the transport",
        |state| state.visibility == Visibility::Unavailable,
    );
    assert!(
        Connection::connect(listening, LinkScope::Loopback, Duration::from_secs(60)).is_err(),
        "revoked receiver still accepts a native TCP connection"
    );
    let second = source
        .write([32; 16], vec![change("while-permission-held")])
        .unwrap();
    assert_eq!(received.snapshot().unwrap().operations.len(), 1);
    attachment.lan_allowed.store(true, Ordering::Release);
    attachment.wait_for(
        table.digest(),
        2,
        "re-grant catches up the operation written offline",
    );
    runtime.stop();
    sender.stop();
    let actual = received.snapshot().unwrap();
    let expected = source.snapshot().unwrap();
    assert_eq!(actual.cells, expected.cells);
    assert_eq!(actual.operations.len(), 2);
    assert_eq!(actual.withheld, 0);
    assert!(
        actual
            .operations
            .iter()
            .any(|operation| operation.header.id == second.id)
    );
}

/// A refusal the host's records hear is information, never a closed LAN: the records may be stale
/// (on macOS only a look at the ALab vault's address or System State's step writes them, and a
/// home's Macs and apps share them). The OS refuses an app the LAN by failing its sends, not by
/// taking its addresses away, and the isolated attachment's flag does the same to its link's
/// beacons: the transport's link fails and it reads Unavailable. While the records hear the
/// refusal, the observation says the OS refused and where to lift it; the plain detail comes back
/// when they stop hearing it; and the OS's grant brings the transport up at its next refresh while
/// the records still hear the refusal.
#[test]
fn a_heard_refusal_says_so_and_never_closes_the_lan() {
    use super::auto_sync_tests::{Attachment, wait_network};
    use aterm_net::lan::{Multicast, Scope as LinkScope};
    use std::{
        net::Ipv4Addr,
        sync::atomic::{AtomicBool, Ordering},
    };

    let home = aterm_tempfile::Builder::new()
        .prefix("commons-refusal-")
        .tempdir_in("/tmp")
        .unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let attachment = Attachment::new(reservation.port());
    attachment.lan_allowed.store(false, Ordering::Release);
    let heard = Arc::new(AtomicBool::new(true));
    let refused = heard.clone();
    let runtime = Runtime::start_on(
        Paths::for_home(home.path()),
        home.path().join("aterm.toml"),
        |_| {},
        Network {
            lan: LanGate::open().with_refusal(move || refused.load(Ordering::Acquire)),
            isolated: Some(attachment.clone()),
            ..Network::default()
        },
    )
    .unwrap();
    let observation = runtime.network_observation();
    let refusal = concat_refused!("; Commons control unavailable");
    wait_network(&observation, "a heard refusal says so", |state| {
        state.visibility == Visibility::Unavailable && state.detail == refusal
    });
    heard.store(false, Ordering::Release);
    wait_network(&observation, "no refusal heard", |state| {
        state.visibility == Visibility::Unavailable
            && state.detail.starts_with(
                "Local network sockets unavailable; Commons control unavailable: discovery beacon",
            )
    });
    heard.store(true, Ordering::Release);
    wait_network(&observation, "the refusal heard again", |state| {
        state.visibility == Visibility::Unavailable && state.detail == refusal
    });
    attachment.lan_allowed.store(true, Ordering::Release);
    wait_network(
        &observation,
        "the OS's grant opens the LAN while the records still hear the refusal",
        |state| {
            state.visibility == Visibility::Active
                && state.detail == "Discovery only; Commons control unavailable"
        },
    );
    runtime.stop();
}

/// The runtime asks its gate's host right before its first LAN use ([`LanGate::before_use`]),
/// since the OS's question comes at that use, not when the gate opened (an open owner may make
/// none for a while). A deferred use is [`Status::LanHeld`], with the host's reason kept for the
/// owner's observation; only loopback candidates ask nothing; and once its use is kept the runtime
/// asks no more.
#[test]
fn the_first_lan_use_asks_its_host_right_before_it() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    const BUSY: &str = "Waits for another macOS question of aterm's to end";
    let (asks, busy) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(true)),
    );
    let network = Network {
        lan: LanGate::open().before_use({
            let (asks, busy) = (asks.clone(), busy.clone());
            move || {
                asks.fetch_add(1, Ordering::SeqCst);
                busy.load(Ordering::SeqCst).then_some(BUSY)
            }
        }),
        ..Network::default()
    };
    let lan: [std::net::SocketAddr; 1] = ["192.168.1.20:0".parse().unwrap()];
    assert_eq!(network.may_use(&["127.0.0.1:0".parse().unwrap()]), Ok(()));
    assert_eq!(asks.load(Ordering::SeqCst), 0, "loopback is no LAN use");
    assert_eq!(network.may_use(&lan), Err(Status::LanHeld), "deferred");
    assert_eq!(
        network.waits.get(),
        BUSY,
        "the host's reason, for the observation"
    );
    busy.store(false, Ordering::SeqCst);
    assert_eq!(network.may_use(&lan), Ok(()), "the host lets it go");
    assert_eq!(asks.load(Ordering::SeqCst), 2);
    network.note_use(&lan, true);
    assert_eq!(network.may_use(&lan), Ok(()));
    assert_eq!(asks.load(Ordering::SeqCst), 2, "a kept use asks no more");
}

/// A runtime's LAN use is its host's to keep ([`LanGate::keeping_use`]), never a record per home:
/// the OS asks each app on each Mac, and a terminal session lane's use is the question of the app
/// that holds its terminal, not aterm.app's. A runtime with a keeper runs it at its first
/// non-loopback open or refresh, and again at the next while it could not keep it; a loopback one
/// runs none. So an earlier aterm used the LAN here (`network_state::lan_used`) only as an aterm
/// with no gate's history says: a gated owner's history, and its LAN use, read as no use. A
/// history with no record is one an aterm with no gate (0.145.0) left: it reads as used, before and
/// after a gated owner opens there.
#[test]
fn a_lan_use_is_the_hosts_to_keep_never_the_homes() {
    use crate::network_state::{history_exists, lan_used};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    assert!(!lan_used(&config), "nothing ran here");
    let (observe, wait) = statuses();
    let runtime =
        Runtime::start_on(paths.clone(), config.clone(), observe, Network::default()).unwrap();
    wait(Status::Dormant);
    runtime.stop();
    assert!(history_exists(&config), "the owner opened its observer");
    assert!(!lan_used(&config), "a gated owner's history is no use");
    let lan: [std::net::SocketAddr; 1] = ["192.168.1.20:0".parse().unwrap()];
    Network::default().note_use(&lan, true);
    assert!(
        !lan_used(&config),
        "a runtime with no keeper (a session lane's) keeps no use for the home"
    );
    let (runs, kept) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
    );
    let network = Network {
        lan: LanGate::open().keeping_use({
            let (runs, kept) = (runs.clone(), kept.clone());
            move || {
                runs.fetch_add(1, Ordering::SeqCst);
                kept.load(Ordering::SeqCst)
            }
        }),
        ..Network::default()
    };
    network.note_use(&["127.0.0.1:0".parse().unwrap()], true);
    assert_eq!(runs.load(Ordering::SeqCst), 0, "loopback is no LAN use");
    network.note_use(&lan, true);
    assert_eq!(runs.load(Ordering::SeqCst), 1, "a LAN address opened");
    kept.store(true, Ordering::SeqCst);
    network.note_use(&lan, true);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "not kept: tried at the next"
    );
    network.note_use(&lan, true);
    assert_eq!(runs.load(Ordering::SeqCst), 2, "kept once for the runtime");
    assert!(!lan_used(&config), "the host kept it, not the home");

    let legacy = aterm_tempfile::tempdir().unwrap();
    let config = legacy.path().join("aterm.toml");
    drop(Observer::open(&config, ObservationHandle::default()).unwrap());
    assert!(lan_used(&config), "an ungated aterm's history");
    let (observe, wait) = statuses();
    let runtime = Runtime::start_on(
        Paths::for_home(legacy.path()),
        config.clone(),
        observe,
        Network::default(),
    )
    .unwrap();
    wait(Status::Dormant);
    runtime.stop();
    assert!(
        lan_used(&config),
        "a gated owner keeps the ungated aterm's use"
    );
}

/// A LAN use is a LAN socket the transport bound ([`transport::Transport::bound`]), not an address
/// in its candidate list: an attachment can change between the read of its address and the bind,
/// whose re-check then refuses before any socket, so the OS asked nothing. Such a try is no use:
/// the keeper does not run, the gate's host hears the miss ([`LanGate::missing_use`]), and the next
/// try asks the host again; the try that binds is the use.
#[test]
fn a_try_that_bound_no_lan_socket_is_no_lan_use() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let count = || Arc::new(AtomicUsize::new(0));
    let (asks, keeps, misses) = (count(), count(), count());
    let network = Network {
        lan: LanGate::open()
            .before_use({
                let asks = asks.clone();
                move || {
                    asks.fetch_add(1, Ordering::SeqCst);
                    None
                }
            })
            .keeping_use({
                let keeps = keeps.clone();
                move || {
                    keeps.fetch_add(1, Ordering::SeqCst);
                    true
                }
            })
            .missing_use({
                let misses = misses.clone();
                move || {
                    misses.fetch_add(1, Ordering::SeqCst);
                }
            }),
        ..Network::default()
    };
    let seen = || {
        (
            asks.load(Ordering::SeqCst),
            keeps.load(Ordering::SeqCst),
            misses.load(Ordering::SeqCst),
        )
    };
    // TEST-NET-1 is no attachment of this machine's: every bind refuses before its socket.
    let lan: [std::net::SocketAddr; 1] = ["192.0.2.1:0".parse().unwrap()];
    assert_eq!(network.may_use(&lan), Ok(()));
    let mut transport = network.open(None, &lan).unwrap();
    assert!(!transport.bound(), "no attachment: every bind refused");
    assert_eq!(
        seen(),
        (1, 0, 1),
        "no LAN socket: no use, and its host hears the miss"
    );
    assert_eq!(network.may_use(&lan), Ok(()));
    network.refresh(&mut transport, &lan);
    assert_eq!(
        seen(),
        (2, 0, 2),
        "the next try asks its host again, and misses again"
    );
    let mut bound = transport::Transport::isolated(None, 0, None).unwrap();
    assert!(bound.bound(), "its loopback group bound");
    assert_eq!(network.may_use(&lan), Ok(()));
    network.refresh(&mut bound, &lan);
    assert_eq!(seen(), (3, 1, 2), "a bound socket is the use, kept");
    assert_eq!(network.may_use(&lan), Ok(()));
    assert_eq!(seen(), (3, 1, 2), "a kept use asks no more");

    let (lan_gate, seen) = counting_gate();
    let failing = Network {
        lan: lan_gate,
        open_fails: true,
        ..Network::default()
    };
    assert_eq!(failing.may_use(&lan), Ok(()));
    assert!(failing.open(None, &lan).is_err());
    assert_eq!(
        seen(),
        (1, 0, 1),
        "an open that failed made no socket: a miss, never a use"
    );
}

/// A gate whose host counts each use it readies, keeps and misses ([`LanGate::before_use`],
/// [`LanGate::keeping_use`], [`LanGate::missing_use`]), readying every one, and what it counted.
fn counting_gate() -> (LanGate, impl Fn() -> (usize, usize, usize)) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let count = || Arc::new(AtomicUsize::new(0));
    let (asks, keeps, misses) = (count(), count(), count());
    let gate = LanGate::open()
        .before_use({
            let asks = asks.clone();
            move || {
                asks.fetch_add(1, Ordering::SeqCst);
                None
            }
        })
        .keeping_use({
            let keeps = keeps.clone();
            move || {
                keeps.fetch_add(1, Ordering::SeqCst);
                true
            }
        })
        .missing_use({
            let misses = misses.clone();
            move || {
                misses.fetch_add(1, Ordering::SeqCst);
            }
        });
    (gate, move || {
        (
            asks.load(Ordering::SeqCst),
            keeps.load(Ordering::SeqCst),
            misses.load(Ordering::SeqCst),
        )
    })
}

/// Wait, with a one-minute hang detector, until `seen` (asks, keeps, misses) satisfies `done`.
fn until_seen(
    seen: &impl Fn() -> (usize, usize, usize),
    phase: &str,
    done: impl Fn((usize, usize, usize)) -> bool,
) {
    let until = Instant::now() + Duration::from_secs(60);
    while !done(seen()) {
        assert!(Instant::now() < until, "{phase}: {:?}", seen());
        thread::sleep(Duration::from_millis(10));
    }
}

/// Every LAN use the owner readies with its gate's host ([`LanGate::before_use`]) ends in exactly
/// one keep or one miss, at its open and at each poll's refresh, in discovery
/// (`serve_presence`) and in repair (`serve_mode`): a host that holds something for the use (a
/// dialog slot) hears every try that reached no LAN socket and lets it go, and the try that
/// reaches one is the use, kept. A try readied with nothing after it would leave the host holding
/// for good.
#[test]
fn every_lan_use_the_owner_readies_ends_in_a_keep_or_a_miss() {
    use aterm_secrets::commons::roster::Role;
    use std::sync::atomic::{AtomicBool, Ordering};
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    for repair in [false, true] {
        let home = aterm_tempfile::Builder::new()
            .prefix("commons-uses-")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = if repair {
            lab.paths[0].clone()
        } else {
            Paths::for_home(home.path())
        };
        let (lan, seen) = counting_gate();
        let up = Arc::new(AtomicBool::new(false));
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::start_on(
            paths,
            home.path().join("aterm.toml"),
            {
                let statuses = statuses.clone();
                move |status| statuses.lock().unwrap().push(status)
            },
            Network {
                lan,
                stand_in: Some(up.clone()),
                ..Network::default()
            },
        )
        .unwrap();
        until_seen(
            &seen,
            "the open and a poll's refresh missed",
            |(_, _, misses)| misses >= 2,
        );
        up.store(true, Ordering::Release);
        until_seen(&seen, "a refresh reached the LAN", |(_, keeps, _)| {
            keeps == 1
        });
        runtime.stop();
        let (asks, keeps, misses) = seen();
        assert_eq!(
            asks,
            keeps + misses,
            "repair {repair}: every use readied ended in one keep or miss"
        );
        assert_eq!(keeps, 1, "repair {repair}: the use, kept once");
        let statuses = statuses.lock().unwrap();
        assert_eq!(
            statuses
                .iter()
                .any(|status| matches!(status, Status::Active { .. } | Status::Pending)),
            repair,
            "repair {repair}: {statuses:?}"
        );
    }
}

/// With no IPv4 candidate (the runtime reads only IPv4 addresses) the runtime asks its gate's host
/// nothing and makes no use, so it hears no miss either: a slot the host took for the use goes
/// when its own wait runs out, not at once.
#[test]
fn no_candidate_asks_nothing_and_misses_nothing() {
    let (lan, seen) = counting_gate();
    let network = Network {
        lan,
        ..Network::default()
    };
    assert_eq!(network.may_use(&[]), Ok(()));
    let mut transport = network.open(None, &[]).unwrap();
    network.refresh(&mut transport, &[]);
    assert_eq!(seen(), (0, 0, 0), "no try, no miss");
}
