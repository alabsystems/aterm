// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::fsx;

/// A descriptor can become ready after the zero-time poll but before the pacing floor.
/// The native wait's deadline is an upper bound, not a minimum sleep.
#[test]
fn a_notification_during_wait_preserves_the_tick_floor() {
    let start = Instant::now();
    for arrival in [Duration::ZERO, TICK / 2, TICK, TICK * 2] {
        let now = std::cell::Cell::new(start);
        rest_ready(
            start + TICK,
            start + POLL,
            || Ok(false),
            |_| {
                now.set(start + arrival);
                Ok(())
            },
            |duration| now.set(now.get() + duration),
            || now.get(),
        );
        assert_eq!(now.get(), start + arrival.max(TICK));
    }
}

#[test]
fn a_later_expired_push_cannot_replace_a_pending_table_or_acknowledge_it() {
    for wait in [Duration::ZERO, Duration::from_millis(100)] {
        let pusher = Pusher::default();
        let board = pusher.0.clone();
        let _serving = board.serving(None);
        let first = pusher.clone();
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let caller = thread::spawn(move || first.push_until([1; 32], deadline));
        // Observe registration without letting the runtime take the first table yet.
        loop {
            if board.desk.lock_or_recover().ask.is_some() {
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

/// A neighbour's acknowledgement on the mesh's relay is a reached push at once: no ask reaches
/// the owner and no round runs. NEGATIVE CONTROL: a relay that answers `None` hands the push to
/// the rounds, where the owner takes the ask and its answer (here no peer) is the caller's, as
/// before the mesh.
#[test]
fn a_relay_ack_reaches_a_push_with_no_round() {
    for acked in [true, false] {
        let pusher = Pusher::default();
        let board = pusher.0.clone();
        let _serving = board.serving(None);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let heard = asked.clone();
        pusher.relay(Arc::new(move |table, _deadline| {
            heard.lock().unwrap().push(table);
            acked.then_some(Pushed::Reached)
        }));
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let caller = pusher.clone();
        let caller = thread::spawn(move || caller.push_until([3; 32], deadline));
        if acked {
            assert_eq!(caller.join().unwrap(), Pushed::Reached);
            let desk = board.desk.lock_or_recover();
            assert_eq!(desk.asked, 0, "a relay ack asked the owner for a round");
            assert!(desk.ask.is_none() && desk.active.is_none());
        } else {
            let ask = next_push(&board, deadline);
            assert_eq!(ask.table, [3; 32]);
            board.settle(ask.id, Pushed::NoPeers);
            assert_eq!(caller.join().unwrap(), Pushed::NoPeers);
        }
        assert_eq!(
            *asked.lock().unwrap(),
            vec![[3; 32]],
            "the relay is asked first"
        );
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

/// A push asked of a serving owner rings the owner's doorbell ([`Board::serving`]), so the owner,
/// asleep until something it waits on happens, takes the ask at once: the doorbell polls ready
/// once the ask is made, and the ask it announced is there to take. A ring that never comes
/// reaches the hang detector's minute.
///
/// NEGATIVE CONTROL (2026-11-09): with `push_by_rounds` leaving the bell unrung, the wait reached
/// the minute (`a push rang the owner's doorbell`).
#[test]
fn a_push_rings_its_serving_owners_doorbell() {
    let pusher = Pusher::default();
    let board = pusher.0.clone();
    let bell = Arc::new(aterm_uds::ready::Doorbell::new().unwrap());
    let serving = board.serving(Some(bell.clone()));
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let caller = thread::spawn(move || pusher.push_until([3; 32], deadline));
    assert_eq!(
        aterm_uds::ready::any(&[bell.fd()], Some(deadline)).unwrap(),
        [0],
        "a push rang the owner's doorbell"
    );
    assert!(bell.clear());
    let ask = board.take().expect("the ask the ring announced");
    assert_eq!(ask.table, [3; 32]);
    board.settle(ask.id, Pushed::Reached);
    assert_eq!(caller.join().unwrap(), Pushed::Reached);
    drop(serving);
    assert!(
        board.bell.lock_or_recover().is_none(),
        "an owner that stopped serving keeps no doorbell"
    );
}

#[test]
fn concurrent_pushes_get_their_own_result_and_ignore_stale_completion() {
    let pusher = Pusher::default();
    let board = pusher.0.clone();
    let _serving = board.serving(None);
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
    let priority = *board.first.lock_or_recover();
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
    let _serving = board.serving(None);
    {
        let mut desk = board.desk.lock_or_recover();
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
    board.desk.lock_or_recover().asked = 2;
    board.settle(first.id, Pushed::Reached);
    assert_eq!(
        *board.first.lock_or_recover(),
        None,
        "the abandoned round left its table prioritized"
    );
    assert!(
        board.desk.lock_or_recover().settled.is_none(),
        "round one cannot answer caller two"
    );
}

#[test]
fn stopping_repair_wakes_every_push_caller() {
    let pusher = Pusher::default();
    let board = pusher.0.clone();
    let serving = board.serving(None);
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
        let serving = board.serving(None);
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let first = pusher.clone();
        let caller = thread::spawn(move || first.push_until([1; 32], deadline));
        loop {
            if board.desk.lock_or_recover().ask.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "push never registered");
            thread::sleep(Duration::from_millis(1));
        }
        let taken = (stage > 0).then(|| board.take().unwrap());
        // Hold the desk across the production stop/start transitions so the waiting
        // caller cannot observe the brief false value between service generations.
        {
            let mut desk = board.desk.lock_or_recover();
            if stage == 2 {
                // The owner had already completed this request before it stopped.
                desk.settled = Some((taken.unwrap().id, Pushed::Reached));
            }
            desk.stop();
            *board.first.lock_or_recover() = None;
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
            board.desk.lock_or_recover().serving,
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
    wait(refused_at("writer recovery"));
    runtime.stop();
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&selection).unwrap(), b"incomplete recovery");
    drop((lease, live_observer));
}

/// The wait `refusals` gives after a try that ended with `status`, having served for `served`
/// ([`Refusals::after`]).
fn wait_after(
    refusals: &mut Refusals,
    mut status: Option<Status>,
    served: Option<Duration>,
) -> Duration {
    refusals.after(status.as_mut(), served)
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
        waits.push(wait_after(&mut refusals, Some(status), None));
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
    assert_eq!(wait_after(&mut refusals, Some(status), None), POLL);
    drop(custody);
}

/// A vault writer held while the control endpoint revalidates its custody is local contention,
/// as it is to a table session: the owner retries after [`POLL`], and the endpoint's error keeps
/// the error custody gave. NEGATIVE CONTROL: the endpoint's generic adapter for a peer's evidence
/// dropped both, so this owner backed off as refused and warned `control endpoint` with no reason.
#[test]
fn a_vault_writer_at_endpoint_revalidation_is_contention_with_its_error() {
    let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let vault = aterm_uds::private_file::PrivateLock::take(
        &lab.paths[0].dir,
        ".lock",
        Duration::ZERO,
        "held",
    )
    .unwrap();
    let custody = bootstrap
        .revalidate_custody()
        .expect_err("a held vault writer is no current observation");
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let error = Endpoint::from_bootstrap_until(&bootstrap, deadline)
        .err()
        .expect("custody revalidation refuses while the vault writer holds");
    assert!(error.is_busy(), "{error}");
    assert!(
        error.to_string().contains(&custody.to_string()),
        "{error} lost {custody}"
    );
    let mut refusals = Refusals::new(false);
    let status = refusal("control endpoint")(error);
    assert_eq!(status, Status::Retrying("control endpoint"));
    assert_eq!(wait_after(&mut refusals, Some(status), None), POLL);
    drop(vault);
    assert!(
        Endpoint::from_bootstrap_until(&bootstrap, deadline)
            .unwrap()
            .is_some()
    );
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
            assert!(
                matches!(&status, Status::Refused(refusal) if refusal.step == "transport"),
                "{status:?}"
            );
            wait_after(&mut refusals, Some(status), None)
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
            wait_after(&mut refusals, Some(refused_at("generation guard")), None)
        });
        let fixed = tries_in_ten_minutes(each, || base);
        assert!(doubling <= 27, "{doubling} tries after {each:?} each");
        assert!(fixed >= 200, "the fixed control made {fixed} tries");
    }

    let mut refusals = Refusals::new(false);
    let refused = || Some(refused_at("custody"));
    let waits: Vec<_> = (0..10)
        .map(|_| wait_after(&mut refusals, refused(), None))
        .collect();
    assert_eq!(waits[0], base, "the first refusal is tried again as before");
    assert_eq!(waits[1], base * 2);
    assert_eq!(waits[2], base * 4);
    assert_eq!(*waits.last().unwrap(), REFUSED_MAX);
    // A try that served briefly before it was refused (an owner refused at each poll) keeps the
    // doubling; one that served for the longest wait, or one that ended to rebuild, resets it.
    assert_eq!(
        wait_after(&mut refusals, refused(), Some(POLL)),
        REFUSED_MAX,
        "a refusal at the first poll reset the backoff"
    );
    assert_eq!(
        wait_after(&mut refusals, refused(), Some(REFUSED_MAX)),
        base
    );
    assert_eq!(wait_after(&mut refusals, refused(), None), base * 2);
    assert_eq!(wait_after(&mut refusals, None, None), base);
    assert_eq!(wait_after(&mut refusals, refused(), None), base);
    // An offline outcome keeps the mode's wait and the streak.
    assert_eq!(wait_after(&mut refusals, Some(Status::Offline), None), base);
    assert_eq!(wait_after(&mut refusals, refused(), None), base * 2);
    let mut background = Refusals::new(true);
    assert!(
        (0..6).all(|_| wait_after(&mut background, refused(), None) == POLL),
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
        wait(refused_at(refused));
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

/// A WAITING OWNER PUBLISHES AGAIN BETWEEN THE SLICES OF A LONG WAIT, so its observation stays
/// current while it lives: a wait of four slices publishes three times, one of a slice or less
/// never, and a set stop ends the wait with nothing more published. The counts are exact
/// whatever the load, since each slice is slept in full. Negative control: a wait slept in one
/// piece publishes nothing, and a refused window's observation ages past what its relay serves.
#[test]
fn a_long_wait_publishes_again_between_its_slices() {
    let every = Duration::from_millis(5);
    let count = |stop: &Stop, wait: Duration| {
        let mut published = 0;
        sleep_sliced(stop, wait, every, || published += 1);
        published
    };
    let stop = Stop::default();
    assert_eq!(count(&stop, every * 4), 3);
    assert_eq!(count(&stop, every * 3 + every / 2), 3);
    assert_eq!(count(&stop, every), 0);
    assert_eq!(count(&stop, Duration::ZERO), 0);
    stop.set();
    assert_eq!(count(&stop, every * 4), 0);
    // The owner's longest wait is past the window a reader holds a publication to, and its
    // slices are well inside it.
    assert!(REFUSED_MAX > Duration::from_millis(aterm_types::network_state::REACHABLE_MS));
    assert!(REPUBLISH * 2 <= Duration::from_millis(aterm_types::network_state::REACHABLE_MS));
}

/// A refusal carries the step that refused and the error that step gave, from either error type a
/// step fails with, and so does the refusal of a configuration that could not be read; local
/// contention is still a retry. NEGATIVE CONTROL: the adapter that kept only the step gave two
/// different errors one status, so a host could log only the step.
#[test]
fn a_refusal_carries_the_error_its_step_gave() {
    let refused = |step, error: &str| {
        Status::Refused(Refusal {
            step,
            error: error.into(),
            standing: false,
        })
    };
    assert_eq!(
        refusal("transport")(crate::Error::new("first")),
        refused("transport", "first")
    );
    assert_eq!(
        refusal("transport")(std::io::Error::other("second")),
        refused("transport", "second")
    );
    assert_ne!(
        refusal("transport")(crate::Error::new("first")),
        refusal("transport")(crate::Error::new("second")),
    );
    assert_eq!(
        refusal("transport")(crate::Error::busy()),
        Status::Retrying("transport")
    );
    let home = aterm_tempfile::tempdir().unwrap();
    let unreadable = home.path().join("aterm.toml");
    fsx::write_atomic(&unreadable, b"[commons\n", 0o600).unwrap();
    let error = enabled(&unreadable).expect_err("an unreadable configuration");
    assert_eq!(
        enabled(&unreadable).map_err(refusal("configuration")),
        Err(refused("configuration", &error.to_string()))
    );
}

/// A streak of refusals is one standing refusal while its cause holds ([`Refusals`]). An owner
/// refused at each poll serves briefly, and its tries pass through other statuses, between two
/// refusals: each after the first stands. A refusal at another step or with another error is
/// news, and so is the first cause when it returns; a try that served for [`REFUSED_MAX`], one
/// that ended to rebuild, or a disabled Commons lifted the refusal, so the next is news. Both modes
/// mark alike.
/// NEGATIVE CONTROL: with no refusal marked standing, every refusal of an owner refused at each
/// poll is news, and its host warned of each.
#[test]
fn a_refusal_stands_until_its_cause_changes_or_lifts() {
    let refused = |step, error: &str| {
        Some(Status::Refused(Refusal {
            step,
            error: error.into(),
            standing: false,
        }))
    };
    let custody = || refused("custody", "Commons immutable custody changed");
    let lost = || {
        refused(
            "custody",
            "Commons established history is missing or not private",
        )
    };
    let lease = || refused("owner lease", "Commons owner lease was replaced");
    for background in [false, true] {
        let mut refusals = Refusals::new(background);
        let mut stands = |mut status: Option<Status>, served: Option<Duration>| {
            refusals.after(status.as_mut(), served);
            match status {
                Some(Status::Refused(refusal)) => Some(refusal.standing),
                _ => None,
            }
        };
        let news = Some(false);
        let standing = Some(true);
        assert_eq!(
            stands(custody(), None),
            news,
            "the first refusal of a streak"
        );
        assert_eq!(
            stands(custody(), Some(POLL)),
            standing,
            "refused at its first poll"
        );
        for between in [
            Status::Offline,
            Status::Pending,
            Status::Retrying("control endpoint"),
            Status::Waiting,
            Status::LanHeld,
        ] {
            assert_eq!(stands(Some(between), None), None);
            assert_eq!(
                stands(custody(), Some(POLL)),
                standing,
                "background {background}"
            );
        }
        assert_eq!(stands(lease(), None), news, "another step");
        assert_eq!(stands(custody(), None), news, "the first cause returned");
        assert_eq!(stands(lost(), None), news, "the same step, another error");
        assert_eq!(stands(lost(), None), standing);
        assert_eq!(
            stands(lost(), Some(REFUSED_MAX)),
            news,
            "served the longest wait"
        );
        assert_eq!(stands(None, None), None);
        assert_eq!(stands(lost(), None), news, "ended to rebuild");
        assert_eq!(stands(lost(), None), standing);
        assert_eq!(stands(Some(Status::Disabled), None), None);
        assert_eq!(stands(lost(), None), news, "Commons was switched off");
        assert_eq!(stands(lost(), None), standing);
    }
}

/// An owner's refusal reaches its host with the error its step gave, once per standing cause. A
/// window's owner and the background owner, refused at their transport's open, report it as news;
/// after tries that waited on another owner's lease, they report the same refusal again as
/// standing, which their host logs at debug: of all they report, one refusal is news
/// ([`Status::news`], what their host warns of). NEGATIVE CONTROL: an owner that marked no refusal
/// standing reported the refusal after the wait as it reported the first, a second warning of one
/// standing cause, the re-post its hosts made whenever its tries passed through another status.
#[test]
fn an_owner_reports_a_standing_refusal_once_with_its_error() {
    for background in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
        let paths = lab.paths[0].clone();
        let config = home.path().join("aterm.toml");
        fsx::write_atomic(&config, b"[commons]\nenabled=true\n", 0o600).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let runtime = Runtime::start_mode_with_sources(
            paths.clone(),
            config.clone(),
            (config.clone(), true),
            move |status| {
                let _ = send.send(status);
            },
            Network {
                open_fails: true,
                ..Network::default()
            },
            background,
            |_| {},
        )
        .unwrap();
        let reported = std::cell::RefCell::new(Vec::new());
        let next = || {
            let status = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
            reported.borrow_mut().push(status.clone());
            status
        };
        let open_refused = |standing| Refusal {
            step: "transport",
            error: crate::Error::new(OPEN_FAILS).to_string(),
            standing,
        };
        let transport = |standing| Status::Refused(open_refused(standing));
        assert_eq!(next(), transport(false), "background {background}");
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let lease = loop {
            if let Some(lease) = owner::Lease::acquire(&paths.dir.join("commons")).unwrap() {
                break lease;
            }
            assert!(Instant::now() < until, "the owner kept its lease");
            thread::sleep(TICK / 10);
        };
        // A try refused before the test held the lease already stood.
        loop {
            match next() {
                Status::Waiting => break,
                status => assert_eq!(status, transport(true), "background {background}"),
            }
        }
        drop(lease);
        assert_eq!(next(), transport(true), "background {background}");
        runtime.stop();
        let mut reported = reported.into_inner();
        reported.extend(receive.try_iter());
        let news: Vec<_> = reported.iter().filter_map(Status::news).collect();
        assert_eq!(
            news,
            [&open_refused(false)],
            "background {background}: {reported:?}"
        );
    }
}

/// A dial scheduler that ends with none to follow it at once leaves no promise standing: a
/// serving generation that fails (here, with Commons turned off) and presence-only serving,
/// which dials nothing, each count every earlier contact's lease from the contact, two minutes
/// at most ([`unscheduled`]). A peer contacted 200 s before a later generation's first look
/// therefore reads LastSeen there, not Reachable on what the ended scheduler promised it.
///
/// NEGATIVE CONTROL (run here): with `UNSCHEDULED_KEEPS`, each reads Reachable.
#[test]
fn a_scheduler_that_ends_with_none_to_follow_leaves_no_promise_standing() {
    use aterm_types::network_state::Reachability;
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::locate_for_home(home.path());
    let off = home.path().join("off.toml");
    fsx::write_atomic(&off, b"[commons]\nenabled=false\n", 0o600).unwrap();
    let on = home.path().join("on.toml");
    fsx::write_atomic(&on, b"[commons]\nenabled=true\n", 0o600).unwrap();
    for keeps in [false, true] {
        for presence in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let handle = ObservationHandle::default();
            let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
            let start = Instant::now();
            let identity = MachineIdentity {
                machine_id: [31; 16],
                incarnation: [6; 16],
                label: None,
            };
            observer.contact(identity, 1000, start, Duration::from_secs(660));
            // Commons answers nothing here and opens no socket: the serve ends as it starts.
            let network = Network {
                isolated: Some(auto_sync_tests::Attachment::new(0)),
                open_fails: true,
                ..Network::default()
            };
            let stop = Stop::default();
            UNSCHEDULED_KEEPS.with(|flag| flag.set(keeps));
            let ended = if presence {
                serve_presence(
                    (&paths, &on),
                    None,
                    &stop,
                    &mut |_| {},
                    &mut observer,
                    &network,
                )
            } else {
                serve(
                    &paths,
                    &off,
                    &stop,
                    &mut |_| {},
                    &mut observer,
                    &network,
                    &|_| {},
                )
            };
            UNSCHEDULED_KEEPS.with(|flag| flag.set(false));
            let status = if presence {
                Status::Offline
            } else {
                Status::Disabled
            };
            assert_eq!(ended, Err(status), "presence {presence}");
            // The next generation starts 200 s after the contact and looks at once.
            let later = start + Duration::from_secs(200);
            observer.curtail(later, Since::Now);
            observer.observe(Visibility::Active, 0, "test", 201_000, later);
            assert_eq!(
                handle.snapshot().peers[0].reachability,
                if keeps {
                    Reachability::Reachable
                } else {
                    Reachability::LastSeen
                },
                "presence {presence}, keeps {keeps}"
            );
        }
    }
}

#[test]
fn delayed_worker_completion_does_not_refresh_old_authentication() {
    let root = aterm_tempfile::tempdir().unwrap();
    let handle = crate::network_state::ObservationHandle::default();
    let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
    let authenticated = Instant::now();
    // The round ends as long after it authenticated as its contact was promised the next one:
    // counted from the authentication the promise has run out, counted from the completion it
    // would have all of it left.
    let promise = discovery::Hints::promise();
    let completed = authenticated + promise;
    let completed_ms = 1000 + promise.as_millis() as u64;
    let at_ms = contact_time(completed_ms, completed, authenticated);
    assert_eq!(at_ms, 1000);
    observer.contact(
        MachineIdentity {
            machine_id: [1; 16],
            incarnation: [2; 16],
            label: None,
        },
        at_ms,
        authenticated,
        promise,
    );
    observer.observe(Visibility::Active, 0, "ok", completed_ms, completed);
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

/// A serving loop reads a sleep where the wall clock moved on by more than ten seconds over
/// what the steady clock counted between two ticks, and only there: clocks in step, a loop
/// that was slow while both ran, a wall clock set back and one that cannot be read tell none,
/// and a sleep is told once, with what the wall clock counted over the steady one.
#[test]
fn a_serving_loop_reads_a_sleep_where_the_wall_clock_outran_the_steady_one() {
    let start = Instant::now();
    let mut wake = Wake::default();
    let hour = 3_600_000;
    for (second, wall_ms, woke, what) in [
        (0, Some(1_000_000), None, "the first tick"),
        (2, Some(1_002_000), None, "in step"),
        (4, Some(1_014_000), None, "ten seconds ahead and no more"),
        (6, Some(1_026_001), Some(10_001), "over ten seconds ahead"),
        (8, Some(1_028_001), None, "told once"),
        (10, Some(900_000), None, "set back"),
        (
            3610,
            Some(900_000 + hour),
            None,
            "a slow loop: both clocks ran",
        ),
        (3612, None, None, "no wall clock"),
        (3614, Some(99_000_000), None, "nothing to compare with"),
        (
            3616,
            Some(99_000_000 + 8 * hour),
            Some(8 * hour - 2000),
            "a night's sleep",
        ),
    ] {
        assert_eq!(
            wake.woke(start + Duration::from_secs(second), wall_ms),
            woke.map(Duration::from_millis),
            "{what}"
        );
    }
}

#[test]
fn the_hearing_ledger_keeps_a_version_at_its_round_and_the_contact_that_came_after() {
    use aterm_secrets::commons::{
        hearing::{Entry, Version},
        journal::Trust,
    };
    let mut ledger = Ledger::new(Trust {
        network: [7; 16],
        authority: [11; 32],
        genesis: [13; 32],
    });
    let start = Instant::now();
    let after = |s| start + Duration::from_secs(s);
    let now = after(5);
    // A crossed dial: the round that traded versions authenticated 5s ago, the loser 1s ago,
    // refused before versions were traded. Seconds apart, so neither time can pass for the other.
    let crossed = transport::Contact {
        actor: [7; 48],
        at: after(4),
        lease: discovery::Hints::promise(),
        said: Said::Version(Version::own()),
        said_at: after(0),
        clock: None,
        clock_at: after(4),
    };
    // A peer whose rounds all ended before versions were traded.
    let silent = transport::Contact {
        actor: [8; 48],
        at: after(3),
        lease: discovery::Hints::promise(),
        said: Said::Unknown,
        said_at: after(3),
        clock: None,
        clock_at: after(3),
    };
    let contacts = [crossed, silent];
    assert!(record_contacts(&mut ledger, &contacts, 100_000, now));
    assert_eq!(
        ledger.entry(&crossed.actor),
        Some(&Entry {
            at_ms: 99_000,
            version: Some(Version::own()),
            said_ms: 95_000,
        })
    );
    assert_eq!(
        ledger.entry(&silent.actor),
        Some(&Entry {
            at_ms: 98_000,
            version: None,
            said_ms: 0,
        })
    );
    // The same contacts again add no member and change no version.
    assert!(!record_contacts(&mut ledger, &contacts, 100_000, now));
}

/// What a round's trade of version reports leaves in this Mac's store reaches its contact
/// ledger at the next hearing tick, with no round between the two Macs: the member the
/// checkpoint gate held as unheard is counted from the time it signed, in the ledger and in
/// `<commons>/heard`, and that is no contact, so the watch does not start (the kept file's watch
/// line stays `0 0`, where a tick with a round of that member's own starts it). The first tick
/// gives the store its roster, signs this Mac's own report into it and keeps it in
/// `<commons>/reports`; a hearing opened afterwards reads that report back and signs no other.
///
/// NEGATIVE CONTROL: with `Hearing::hear_relayed` recording each report as a contact
/// (`Ledger::record` at the signed time, in place of `Ledger::relay`), the watch starts:
/// `` left: Some("1794984691439 1794984691458") right: Some("0 0") ``.
#[test]
fn relayed_reports_reach_the_ledger_at_the_hearing_tick_without_counting_as_contact() {
    use aterm_secrets::commons::{
        hearing::{Entry, Floor, Hold, RELAYS, Reports, Version, in_place},
        roster::Role,
    };
    // The floor a member that reports `RELAYS` clears: the test judges the relay, not where
    // a deployed fence such as `CHECKPOINTS` stands.
    const RELAYED: Floor = in_place(Some(RELAYS));
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    // Mac 2 runs the tick; Mac 1 is the member it has no round with.
    let (signer, local) = (lab.bootstrap(0), lab.bootstrap(1));
    let member = signer.request().unwrap().binding().clone();
    let own = local.request().unwrap().binding().clone();
    let reports = Arc::new(Mutex::new(Reports::new(local.trust(), &own)));
    let mut hearing = Hearing::open(&local, &own, reports.clone());
    let tick = |hearing: &mut Hearing, contacts: &[transport::Contact]| {
        let resolved = std::cell::OnceCell::new();
        hearing.tick(&lab.paths[1], &local, &resolved, contacts, Instant::now());
    };
    let watch = || {
        let kept = std::fs::read_to_string(local.heard_file()).unwrap();
        kept.lines().nth(2).map(str::to_owned)
    };
    let unheard = vec![(member.actor(), Hold::Unheard)];

    tick(&mut hearing, &[]);
    let now_ms = aterm_time::unix_millis().unwrap();
    let (kept, _) = reports.lock().unwrap().since(0);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].actor(), kept[0].version()),
        (own.actor(), Version::own())
    );
    assert_eq!(
        hearing.ledger.holding_below([&member], now_ms, RELAYED),
        unheard
    );

    // A round with some other peer leaves Mac 1's report in the store.
    let wire = signer
        .device()
        .with_seed(|seed| {
            hearing::report_at(seed, local.trust().network, &member.actor(), RELAYS, now_ms)
        })
        .unwrap();
    {
        let mut store = reports.lock().unwrap();
        let binding = store.screen(&wire, now_ms).unwrap();
        assert!(store.accept(hearing::relayed(&wire, &binding, now_ms).unwrap(), now_ms));
    }
    tick(&mut hearing, &[]);
    let heard = Entry {
        at_ms: now_ms,
        version: Some(RELAYS),
        said_ms: now_ms,
    };
    assert_eq!(hearing.ledger.entry(&member.actor()), Some(&heard));
    assert!(
        hearing
            .ledger
            .holding_below([&member], now_ms, RELAYED)
            .is_empty()
    );
    let read = Ledger::read(&local);
    assert_eq!(read.entry(&member.actor()), Some(&heard));
    assert_eq!(watch().as_deref(), Some("0 0"));

    // A hearing opened afterwards reads this Mac's own report from the file and keeps it.
    let again = Arc::new(Mutex::new(Reports::new(local.trust(), &own)));
    let mut reopened = Hearing::open(&local, &own, again.clone());
    tick(&mut reopened, &[]);
    let (read_back, _) = again.lock().unwrap().since(0);
    assert_eq!(read_back, kept);

    // A round with Mac 1 itself, which the relayed report was not, starts the watch, and what
    // that round reported replaces what the relayed report said.
    let at = Instant::now();
    let contact = transport::Contact {
        actor: member.actor(),
        at,
        lease: discovery::Hints::promise(),
        said: Said::Version(Version::own()),
        said_at: at,
        clock: None,
        clock_at: at,
    };
    tick(&mut hearing, &[contact]);
    assert_ne!(watch().as_deref(), Some("0 0"));
    assert_eq!(
        hearing.ledger.entry(&member.actor()).unwrap().version,
        Some(Version::own())
    );
}

/// Mac 2 of a lab of two writers, for its hearing tick: its store of kept reports, and Mac 1
/// as the member it hears of.
struct Listening {
    lab: crate::table::tests::Lab,
    local: Bootstrap,
    own: aterm_secrets::commons::binding::Binding,
    member: aterm_secrets::commons::binding::Binding,
    reports: Arc<Mutex<hearing::Reports>>,
}

impl Listening {
    fn new() -> Self {
        let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer; 2]);
        let local = lab.bootstrap(1);
        let own = local.request().unwrap().binding().clone();
        let member = lab.bootstrap(0).request().unwrap().binding().clone();
        let reports = Arc::new(Mutex::new(hearing::Reports::new(local.trust(), &own)));
        Self {
            lab,
            local,
            own,
            member,
            reports,
        }
    }

    fn hearing(&self) -> Hearing {
        Hearing::open(&self.local, &self.own, self.reports.clone())
    }

    /// One tick of `hearing` at `now`, with `contacts` as the rounds since the last.
    fn tick(&self, hearing: &mut Hearing, contacts: &[transport::Contact], now: Instant) {
        let resolved = std::cell::OnceCell::new();
        hearing.tick(&self.lab.paths[1], &self.local, &resolved, contacts, now);
    }

    /// Mac 1's report of `version` stamped `signed_ms`, left in the store as by a round with
    /// some other peer.
    fn hand(&self, version: hearing::Version, signed_ms: u64) {
        let now_ms = aterm_time::unix_millis().unwrap();
        let wire = self
            .lab
            .bootstrap(0)
            .device()
            .with_seed(|seed| {
                hearing::report_at(
                    seed,
                    self.local.trust().network,
                    &self.member.actor(),
                    version,
                    signed_ms,
                )
            })
            .unwrap();
        let mut store = self.reports.lock().unwrap();
        let binding = store.screen(&wire, now_ms).unwrap();
        assert!(store.accept(hearing::relayed(&wire, &binding, now_ms).unwrap(), now_ms));
    }

    /// A round with Mac 1 that ended at `at` and reported `version`.
    fn contact(&self, version: hearing::Version, at: Instant) -> transport::Contact {
        transport::Contact {
            actor: self.member.actor(),
            at,
            lease: discovery::Hints::promise(),
            said: Said::Version(version),
            said_at: at,
            clock: None,
            clock_at: at,
        }
    }

    /// The members whose reports `<commons>/reports` holds, as a store that starts now reads it.
    fn filed(&self) -> std::collections::BTreeSet<astream_wire::sync::Actor> {
        let now_ms = aterm_time::unix_millis().unwrap();
        let mut read = hearing::Reports::new(self.local.trust(), &self.own);
        read.set_roster([&self.member, &self.own], now_ms);
        read.load(&self.local, now_ms);
        let (kept, _) = read.since(0);
        kept.iter().map(hearing::Relayed::actor).collect()
    }
}

/// What a round with a member said stands against a relayed report of that member stamped
/// minutes later, when both reach the same hearing tick: the tick records its rounds before it
/// counts relayed reports. Mac 1's clock runs nine minutes fast; it signed a release, went back
/// to a build that neither reads checkpoints nor stopped writing secret files, and a round two
/// seconds before the tick heard that build. Its earlier report, left in the store by another
/// peer, does not replace what the round said, and the checkpoint gate holds.
///
/// NEGATIVE CONTROL: with `Hearing::tick` counting relayed reports before it records its
/// rounds, the report sets the version at the tick's time and the round, two seconds earlier,
/// is dropped: `` left: Some(Version(0, 207, 0)) right: Some(Version(0, 202, 0)) ``.
#[test]
fn a_round_with_a_member_stands_against_its_relayed_report_in_the_same_tick() {
    use hearing::{Floor, Hold, RELAYS, Version, in_place};
    // The floor a member that reports `RELAYS` clears: the test judges the relay, not where
    // a deployed fence such as `CHECKPOINTS` stands.
    const RELAYED: Floor = in_place(Some(RELAYS));
    let lab = Listening::new();
    let mut hearing = lab.hearing();
    lab.tick(&mut hearing, &[], Instant::now());

    let older = Version(0, 202, 0);
    let now_ms = aterm_time::unix_millis().unwrap();
    lab.hand(RELAYS, now_ms + 9 * 60 * 1000);
    let now = Instant::now();
    let heard = now.checked_sub(Duration::from_secs(2)).unwrap();
    lab.tick(&mut hearing, &[lab.contact(older, heard)], now);

    let actor = lab.member.actor();
    assert_eq!(hearing.ledger.entry(&actor).unwrap().version, Some(older));
    let now_ms = aterm_time::unix_millis().unwrap();
    assert_eq!(
        hearing.ledger.holding_below([&lab.member], now_ms, RELAYED),
        vec![(actor, Hold::Older(Some(older)))]
    );
    assert!(!hearing.ledger.writes_no_secret_file(&lab.member, now_ms));
}

/// What the store took since `<commons>/reports` was last written is written when the hearing
/// ends, as it does when its serving loop does. The first tick writes this Mac's own report;
/// a report a round then brings is counted at the next tick, which writes no file inside the
/// hour; the file holds it once the hearing is dropped, so the store that starts next keeps it
/// and offers it on.
///
/// NEGATIVE CONTROL: with `Hearing`'s `Drop` writing nothing, the file still holds only this
/// Mac's own report after the hearing ended: `` assertion `left == right` failed `` with a
/// left set of one actor and a right set of two.
#[test]
fn reports_taken_since_the_last_write_are_kept_when_the_hearing_ends() {
    use std::collections::BTreeSet;
    let lab = Listening::new();
    let (own, member) = (lab.own.actor(), lab.member.actor());
    let mut hearing = lab.hearing();
    lab.tick(&mut hearing, &[], Instant::now());
    assert_eq!(lab.filed(), BTreeSet::from([own]));

    lab.hand(hearing::RELAYS, aterm_time::unix_millis().unwrap());
    lab.tick(&mut hearing, &[], Instant::now());
    assert!(hearing.ledger.entry(&member).is_some());
    assert_eq!(lab.filed(), BTreeSet::from([own]));

    drop(hearing);
    assert_eq!(lab.filed(), BTreeSet::from([own, member]));
}

/// While the hearing runs, `<commons>/reports` is written at most once in
/// [`hearing::PERSIST`], however many [`KEEP`] ticks find the store changed: a report taken
/// after the first tick's write is not in the file at the next `KEEP` tick, nor at one a
/// millisecond short of the hour, and is in it at the `KEEP` tick after that, past the hour.
///
/// NEGATIVE CONTROL: with `Hearing::keep_reports` writing at every `KEEP` tick that finds a
/// change, the file holds the report ten minutes after the first write:
/// `` assertion `left == right` failed: a KEEP tick inside the hour `` with a left set of two
/// actors and a right set of one.
#[test]
fn the_kept_reports_file_is_written_at_most_once_an_hour_while_the_hearing_runs() {
    use std::collections::BTreeSet;
    let lab = Listening::new();
    let (own, member) = (lab.own.actor(), lab.member.actor());
    let hour = Duration::from_millis(hearing::PERSIST);
    let mut hearing = lab.hearing();
    let start = Instant::now();
    lab.tick(&mut hearing, &[], start);
    assert_eq!(lab.filed(), BTreeSet::from([own]));

    lab.hand(hearing::RELAYS, aterm_time::unix_millis().unwrap());
    lab.tick(&mut hearing, &[], start + KEEP);
    assert_eq!(
        lab.filed(),
        BTreeSet::from([own]),
        "a KEEP tick inside the hour"
    );
    let short = start + hour - Duration::from_millis(1);
    lab.tick(&mut hearing, &[], short);
    assert_eq!(lab.filed(), BTreeSet::from([own]));
    lab.tick(&mut hearing, &[], short + KEEP);
    assert_eq!(lab.filed(), BTreeSet::from([own, member]));
}

/// The store gets the current roster at any tick that has a contact, not only every [`KEEP`]:
/// a round is how a new member is first met, and its report is wanted from then. A store whose
/// roster fell behind the journal's (here it lacks Mac 1) keeps that roster through a tick with
/// no contact inside `KEEP`, and has the journal's after a tick with one.
///
/// NEGATIVE CONTROL: with `Hearing::tick` giving the store its roster only when `KEEP` is due,
/// the store still lacks the member after the tick with a contact: `` left: [false] right:
/// [true] ``.
#[test]
fn a_tick_with_a_contact_gives_the_store_the_current_roster() {
    let lab = Listening::new();
    let mut hearing = lab.hearing();
    lab.tick(&mut hearing, &[], Instant::now());
    let now_ms = aterm_time::unix_millis().unwrap();
    let wanted = || {
        let offered = [(hearing::tag(&lab.member.actor()), now_ms)];
        lab.reports.lock().unwrap().want(&offered, now_ms)
    };
    assert_eq!(wanted(), [true]);
    lab.reports.lock().unwrap().set_roster([&lab.own], now_ms);
    assert_eq!(wanted(), [false]);

    lab.tick(&mut hearing, &[], Instant::now());
    assert_eq!(wanted(), [false]);
    let at = Instant::now();
    lab.tick(
        &mut hearing,
        &[lab.contact(hearing::Version::own(), at)],
        at,
    );
    assert_eq!(wanted(), [true]);
}

#[test]
fn missing_configuration_is_default_on_but_never_bootstraps_a_vault() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    assert!(enabled(&config).unwrap());
    let (send, receive) = std::sync::mpsc::channel();
    let runtime = Runtime::start(paths.clone(), config.clone(), move |s| {
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

/// A process keeps its runtime in a static, which nothing drops: it stops the runtime through a
/// shared reference as it ends, and the owner thread's observer then closes the network history.
#[test]
fn a_shared_runtime_shuts_down_and_closes_its_network_history() {
    static RUNTIME: std::sync::OnceLock<Runtime> = std::sync::OnceLock::new();
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    let (send, receive) = std::sync::mpsc::channel();
    let started = Runtime::start(paths, config.clone(), move |status| {
        let _ = send.send(status);
    })
    .unwrap();
    assert!(RUNTIME.set(started).is_ok());
    let runtime = RUNTIME.get().unwrap();
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(60)).unwrap(),
        Status::Dormant
    );
    // The wait is a hang detector: the owner thread ends as soon as it reads the request.
    assert!(runtime.shutdown(Duration::from_secs(60)));
    assert_eq!(
        runtime.network_observation().snapshot().detail,
        "Network observation stopped"
    );
    // Its observer observed, so it wrote; its drop closed what it wrote.
    let history = crate::network_state::directory(&config).join("projection.json");
    assert!(history.is_file());
    assert!(
        crate::network_state::read_closed_history(&config)
            .unwrap()
            .is_some()
    );
    // Asked again, there is nothing left to stop.
    assert!(runtime.shutdown(Duration::ZERO));
}

#[test]
fn running_host_observes_exclusion_and_reenabling_without_restart() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    let (send, receive) = std::sync::mpsc::channel();
    let runtime = Runtime::start(paths.clone(), config.clone(), move |status| {
        let _ = send.send(status);
    })
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
        stop.set();
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

/// Each status the owner reports, in order, and a one-minute hang detector to wait for one. A
/// refusal is waited for by its step and whether it stands ([`refused_at`]), not its error's words.
fn statuses() -> (impl Fn(Status) + Send + 'static, impl Fn(Status)) {
    let (send, receive) = std::sync::mpsc::channel();
    let receive = Mutex::new(receive);
    (
        move |status| {
            let _ = send.send(status);
        },
        move |expected| {
            let status = receive
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
            assert_eq!(
                without_error(status.clone()),
                without_error(expected),
                "{status:?}"
            );
        },
    )
}

/// `status` with a refusal's error left out.
fn without_error(status: Status) -> Status {
    match status {
        Status::Refused(refusal) => Status::Refused(Refusal {
            error: String::new(),
            ..refusal
        }),
        status => status,
    }
}

/// The first refusal of a streak at `step`, its error left out ([`statuses`]).
fn refused_at(step: &'static str) -> Status {
    Status::Refused(Refusal {
        step,
        error: String::new(),
        standing: false,
    })
}

/// A configuration's refusal stands as any try's does ([`Refusals`]). `aterm node serve`, refused
/// by an unreadable configuration and then waiting on a window that claimed Commons, reports the
/// same refusal as standing once the window is gone, and as news again only after Commons was
/// switched off. NEGATIVE CONTROL: an owner that marked a copy of its status and reported the
/// unmarked one made each refusal after a window's claim news, a warning every cycle; an owner
/// whose cause outlived a disabled Commons reported the refusal after it as standing.
#[test]
fn a_configuration_refusal_stands_across_a_windows_claim_until_commons_is_switched_off() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    fsx::write_atomic(&config, b"[commons\n", 0o600).unwrap();
    let error = enabled(&config)
        .expect_err("an unreadable configuration")
        .to_string();
    let (send, receive) = std::sync::mpsc::channel();
    let runtime = Runtime::start_background_on(
        paths,
        config.clone(),
        move |status| {
            let _ = send.send(status);
        },
        Network::default(),
    )
    .unwrap();
    let next = || receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    let configuration = |standing| {
        Status::Refused(Refusal {
            step: "configuration",
            error: error.clone(),
            standing,
        })
    };
    // Statuses until `last`, each of which the standing refusal may precede.
    let until = |last: Status| loop {
        match next() {
            status if status == last => break,
            status => assert_eq!(status, configuration(true), "before {last:?}"),
        }
    };
    assert_eq!(next(), configuration(false));
    #[cfg(target_os = "macos")]
    assert_eq!(
        crate::network_state::directory(&config),
        aterm_uds::quiet_state::destination(&config.with_file_name("network-state"))
    );
    let claim = owner::WindowClaim::acquire(&config).unwrap();
    assert!(owner::window_claimed(&config));
    until(Status::Waiting);
    drop(claim);
    assert_eq!(
        next(),
        configuration(true),
        "a window's claim lifted the refusal"
    );
    fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    until(Status::Disabled);
    fsx::write_atomic(&config, b"[commons\n", 0o600).unwrap();
    assert_eq!(next(), configuration(false), "Commons was switched off");
    runtime.stop();
}

/// A network that delivers none of the runtime's sends (EHOSTUNREACH), then delivers, stops and
/// delivers again, as the isolated attachment's flag stands for it: while the flag is off the
/// runtime's own refresh and tick drop each link whose beacon fails, the receiver's listener with
/// it, and the observation reads Unavailable; once it is on, the next refresh opens the link and
/// repair catches up.
#[test]
fn undelivered_sends_close_and_reopen_the_runtimes_transport() {
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
        .write([31; 16], vec![change("before-delivery")])
        .unwrap();
    assert!(!lab.paths[1].dir.join("commons/tables").exists());
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-delivery-")
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
    attachment.delivering.store(false, Ordering::Release);
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
    wait_network(&observation, "no delivery holds discovery", |state| {
        state.visibility == Visibility::Unavailable
    });
    attachment.delivering.store(true, Ordering::Release);
    attachment.wait_for(
        table.digest(),
        1,
        "delivery discovers the first application table",
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
    attachment.delivering.store(false, Ordering::Release);
    wait_network(
        &observation,
        "no delivery withdraws the transport",
        |state| state.visibility == Visibility::Unavailable,
    );
    assert!(
        Connection::connect(listening, LinkScope::Loopback, Duration::from_secs(60)).is_err(),
        "a withdrawn receiver still accepts a native TCP connection"
    );
    let second = source
        .write([32; 16], vec![change("while-undelivered")])
        .unwrap();
    assert_eq!(received.snapshot().unwrap().operations.len(), 1);
    attachment.delivering.store(true, Ordering::Release);
    attachment.wait_for(
        table.digest(),
        2,
        "delivery again catches up the operation written offline",
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

/// The runtime's first LAN use waits for the LAN broker, which on macOS makes every LAN socket of
/// aterm's: while it does not answer the use is [`Status::LanHeld`], with what it waits for kept
/// for the owner's observation; only loopback candidates ask nothing; and once it answered the
/// runtime asks no more.
#[test]
fn the_first_lan_use_waits_for_the_lan_broker() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    const ABSENT: &str = "Waits for the test's LAN broker";
    let (asks, absent) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(true)),
    );
    let network = Network {
        broker: Some(Arc::new({
            let (asks, absent) = (asks.clone(), absent.clone());
            move || {
                asks.fetch_add(1, Ordering::SeqCst);
                if absent.load(Ordering::SeqCst) {
                    Err(ABSENT)
                } else {
                    Ok(())
                }
            }
        })),
        ..Network::default()
    };
    let lan: [std::net::SocketAddr; 1] = ["192.168.1.20:0".parse().unwrap()];
    assert_eq!(network.may_use(&["127.0.0.1:0".parse().unwrap()]), Ok(()));
    assert_eq!(network.may_use(&[]), Ok(()));
    assert_eq!(asks.load(Ordering::SeqCst), 0, "loopback is no LAN use");
    assert_eq!(network.may_use(&lan), Err(Status::LanHeld), "no broker");
    assert_eq!(network.waits.get(), ABSENT, "for the observation");
    absent.store(false, Ordering::SeqCst);
    assert_eq!(network.may_use(&lan), Ok(()), "the broker answered");
    absent.store(true, Ordering::SeqCst);
    assert_eq!(network.may_use(&lan), Ok(()));
    assert_eq!(asks.load(Ordering::SeqCst), 2, "an answer is asked no more");
}

/// An owner whose LAN broker does not answer opens no transport and observes the wait; once the
/// broker answers, its next try opens the transport, in discovery and in repair.
#[test]
fn an_owner_opens_its_transport_once_the_broker_answers() {
    use aterm_secrets::commons::roster::Role;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    const ABSENT: &str = "Waits for the test's LAN broker";
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    for repair in [false, true] {
        let home = aterm_tempfile::Builder::new()
            .prefix("commons-broker-")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = if repair {
            lab.paths[0].clone()
        } else {
            Paths::for_home(home.path())
        };
        let answers = Arc::new(AtomicBool::new(false));
        let opens = Arc::new(AtomicUsize::new(0));
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::start_on(
            paths,
            home.path().join("aterm.toml"),
            {
                let statuses = statuses.clone();
                move |status| statuses.lock().unwrap().push(status)
            },
            Network {
                stand_in: Some(Arc::new(AtomicBool::new(true))),
                opens: Some(opens.clone()),
                broker: Some(Arc::new({
                    let answers = answers.clone();
                    move || {
                        if answers.load(Ordering::SeqCst) {
                            Ok(())
                        } else {
                            Err(ABSENT)
                        }
                    }
                })),
                ..Network::default()
            },
        )
        .unwrap();
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !statuses.lock().unwrap().contains(&Status::LanHeld) {
            assert!(
                Instant::now() < until,
                "repair {repair}: never waited for the broker"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let observation = runtime.network_observation().snapshot();
        assert_eq!(
            observation.visibility,
            Visibility::Waiting,
            "repair {repair}"
        );
        assert_eq!(observation.detail, ABSENT, "repair {repair}");
        assert_eq!(
            opens.load(Ordering::SeqCst),
            0,
            "repair {repair}: no transport"
        );
        answers.store(true, Ordering::SeqCst);
        while opens.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < until,
                "repair {repair}: no transport opened"
            );
            thread::sleep(Duration::from_millis(10));
        }
        runtime.stop();
    }
}

/// A debug build's private attachment is one loopback address and never reads the LAN, so the
/// broker is never asked; an invalid setting refuses rather than falling back to the LAN; and with
/// none the candidates are the LAN's IPv4 addresses, an unreadable LAN reading offline.
#[test]
fn a_private_attachment_is_loopback_alone_and_never_reads_the_lan() {
    let loopback: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let lan: std::net::SocketAddr = "192.168.1.5:0".parse().unwrap();
    assert_eq!(
        usable_addresses(Ok(Some(49152)), || panic!("read the LAN")),
        Ok(vec![loopback])
    );
    assert!(matches!(
        usable_addresses(Err(crate::Error::new("unguarded")), || panic!("read the LAN")),
        Err(Status::Refused(refusal)) if refusal.step == "isolated attachment"
    ));
    assert_eq!(
        usable_addresses(Ok(None), || Ok(vec![lan, "[fe80::1]:0".parse().unwrap()])),
        Ok(vec![lan])
    );
    assert_eq!(
        usable_addresses(Ok(None), || Err(std::io::Error::other("down"))),
        Err(Status::Offline)
    );
}

thread_local! {
    /// The simulated clock [`serve_paced`] reads in its tests.
    static SIMULATED: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

fn simulated() -> Instant {
    SIMULATED
        .with(std::cell::Cell::get)
        .expect("the simulated clock is set")
}

fn advance(by: Duration) {
    SIMULATED.with(|now| now.set(Some(simulated() + by)));
}

/// A serving loop's wait ([`serve_paced`]) where nothing it waits on happens: the simulated clock
/// moves to `until`, or to `floor` where `until` came before it, as [`rest`] waits.
fn idle(_: &(), floor: Instant, until: Instant) {
    let to = until.max(floor);
    SIMULATED.with(|now| now.set(Some(to.max(simulated()))));
}

/// The polled rounds a serving loop ([`serve_paced`]) runs in a simulated minute whose rounds each
/// take `round`, and the ticks it runs in all, each tick keeping `next` as its deadline from its
/// start and nothing the loop waits on happening ([`idle`]). Counted, never timed.
fn rounds_in_a_minute(round: Duration, next: Option<Duration>) -> (usize, usize) {
    let start = Instant::now();
    SIMULATED.with(|now| now.set(Some(start)));
    let (mut rounds, mut ticks) = (0, 0);
    let served: std::result::Result<(), Status> = serve_paced(
        &mut (),
        LoopKind::Repair,
        simulated,
        || simulated() >= start + Duration::from_secs(60),
        idle,
        |(), now, due| {
            ticks += 1;
            // A loop whose clock no longer moves spins: the count ends it, as a hang detector.
            assert!(ticks <= 6_000, "the loop ticks without its clock moving");
            if due {
                rounds += 1;
                advance(round);
            }
            Ok(Flow::Serve(next.map(|next| now + next)))
        },
    );
    assert_eq!(served, Ok(()));
    (rounds, ticks)
}

/// A serving loop rests a whole [`POLL`] after each polled round, by the loop itself
/// ([`serve_paced`], which `serve_selected` and `serve_presence` run under), whatever the round
/// took: over a simulated minute whose ticks each keep a deadline a [`TICK`] on, instant rounds
/// run 29 times, one per [`POLL`], and rounds of 3 s (a busy vault waited for, a slow interface
/// read) 12 times, each 2 s after the last one ended. Counted on a simulated clock, never timed.
///
/// NEGATIVE CONTROLS (2026-11-06), each planted in `serve_paced`, the loop both serving functions
/// run, not in a helper they call: with the rest left out (`due` never moved), every tick after
/// the first [`POLL`] runs a round (`left: 580, right: 29`); with the next round due [`POLL`] after
/// the last one started (`due = now + POLL`, the loops' `poll = now + POLL` before), 3 s rounds
/// run back to back, one [`TICK`] apart (`left: 19, right: 12`).
#[test]
fn a_round_slower_than_the_poll_still_rests_after_it() {
    assert_eq!(rounds_in_a_minute(Duration::ZERO, Some(TICK)).0, 29);
    assert_eq!(rounds_in_a_minute(Duration::from_secs(3), Some(TICK)).0, 12);
}

/// A serving loop wakes only for what it waits on and the deadlines its ticks keep: over a
/// simulated minute where nothing happens, a loop whose ticks keep no deadline ticks once a
/// [`POLL`] (30 ticks, its rounds and its first), one whose ticks keep a deadline 500 ms on (a
/// dial's spacing) ticks at those deadlines, its rounds among them, and one whose ticks keep a deadline already
/// due (work a tick left) still ticks at most once a [`TICK`], never more often (600, the old
/// tick's count, not a spin). Counted on a simulated clock, never timed.
///
/// NEGATIVE CONTROLS (2026-11-09), each planted in `serve_paced`: with its wait's `until` a
/// [`TICK`] on whatever the tick kept (the 100 ms tick this loop had), the idle loop ticked 600
/// times (`left: (29, 600), right: (29, 30)`); with the floor the tick's start (`now`, not `now + TICK`), the
/// loop whose deadline is always due never let the clock move, and the tick count's hang
/// detector ended it (`the loop ticks without its clock moving`).
#[test]
fn an_idle_serving_loop_ticks_only_at_its_rounds_and_its_deadlines() {
    assert_eq!(rounds_in_a_minute(Duration::ZERO, None), (29, 30));
    assert_eq!(
        rounds_in_a_minute(Duration::ZERO, Some(Duration::from_millis(500))),
        (29, 120)
    );
    let (_, ticks) = rounds_in_a_minute(Duration::ZERO, Some(Duration::ZERO));
    assert_eq!(ticks, 600, "a deadline always due ticks once a TICK");
}

/// The words of the serving loops' ledger (`census::Loop`) the calling thread counted between
/// two of its readings, with how many of each.
fn loops_between(before: &census::Cadence, after: &census::Cadence) -> Vec<(&'static str, u64)> {
    let [.., (_, before)] = before.ledgers();
    let [.., (_, after)] = after.ledgers();
    after
        .into_iter()
        .filter_map(|(word, count)| {
            let was = before
                .iter()
                .find(|(seen, _)| *seen == word)
                .map_or(0, |(_, count)| *count);
            (count > was).then_some((word, count - was))
        })
        .collect()
}

/// A tick that ends a serving loop ends it there, with what it came to: [`Flow::End`] as
/// `Ok(())`, a refusal as itself, and no wait or tick after it: the round that ends it is the
/// second tick, after one idle wait. The census counts the loop's opening, by its kind, and why
/// it ended, once each (`census::Loop`); a loop whose stop is set ends as stopped.
///
/// NEGATIVE CONTROLS (2026-11-15), each planted in `serve_paced`: with every `Ok` end counted as
/// `vault`, the refresh case read `[("opened-serving", 1), ("vault", 1)]`; with a presence loop
/// counted as a serving one, it read `[("opened-serving", 1), ("refresh", 1)]`.
#[test]
fn a_tick_that_ends_the_loop_ends_it_at_once() {
    for (ends, outcome, end) in [
        (Ok(Flow::End(LoopEnd::Refresh)), Ok(()), "refresh"),
        (Ok(Flow::End(LoopEnd::Vault)), Ok(()), "vault"),
        (Ok(Flow::End(LoopEnd::Witnessed)), Ok(()), "witnessed"),
        (Err(Status::Disabled), Err(Status::Disabled), "refused"),
    ] {
        for (kind, opened) in [
            (LoopKind::Repair, "opened-serving"),
            (LoopKind::Presence, "opened-presence"),
        ] {
            let start = Instant::now();
            SIMULATED.with(|now| now.set(Some(start)));
            let mut ticks = 0;
            let mut waits = 0;
            let before = census::cadence_here();
            let served = serve_paced(
                &mut (),
                kind,
                simulated,
                || false,
                |state, floor, until| {
                    waits += 1;
                    idle(state, floor, until);
                },
                |(), _, due| {
                    ticks += 1;
                    if due {
                        ends.clone()
                    } else {
                        Ok(Flow::Serve(None))
                    }
                },
            );
            assert_eq!(served, outcome);
            assert_eq!((ticks, waits), (2, 1));
            assert_eq!(
                loops_between(&before, &census::cadence_here()),
                [(opened, 1), (end, 1)],
                "{kind:?} ending {end}"
            );
        }
    }
    let before = census::cadence_here();
    let served: std::result::Result<(), Status> = serve_paced(
        &mut (),
        LoopKind::Repair,
        simulated,
        || true,
        idle,
        |(), _, _| unreachable!("a stopped loop ticks never"),
    );
    assert_eq!(served, Ok(()));
    assert_eq!(
        loops_between(&before, &census::cadence_here()),
        [("opened-serving", 1), ("stopped", 1)]
    );
}
