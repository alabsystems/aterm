// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{DRY_RUN_CAPTURE_WINDOW, DryRunCapture, FREEZE_RUNG0_CEILING, FreezeSeed};
use crate::native_updater_service::ApplyMode;
use std::time::Duration;

fn caps() -> crate::seamless::WireCaps {
    crate::seamless::WireCaps::current()
}

/// A desk with history to carry, through the real parser.
fn desk_with_history(
    app: &crate::App,
) -> std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>> {
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut terminal = crate::term_lock(&term);
    for n in 0..400 {
        terminal.process(format!("line {n}: the history a capture carries\r\n").as_bytes());
    }
    drop(terminal);
    term
}

/// The dry run times the park's own capture and parks nothing: the reader stays
/// attached, no attempt record exists, the engine is free and unchanged, and the
/// measurement covers every handed session.
#[test]
fn a_dry_run_times_the_parks_own_capture_and_parks_nothing() {
    let mut app = crate::App::headless_for_test();
    let term = desk_with_history(&app);
    let (scrollback, cursor) = {
        let terminal = crate::term_lock(&term);
        (terminal.grid().scrollback_lines(), terminal.cursor())
    };
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .reader_join = Some(std::thread::spawn(|| {}));
    let handed = app.handoff_live_sessions().len();

    let dry = app.dry_run_handoff_capture(caps());
    let (DryRunCapture::Measured { took, sessions } | DryRunCapture::AtLeast { took, sessions }) =
        dry
    else {
        panic!("a quiet desk at parser ground is measured: {dry:?}");
    };
    assert_eq!(sessions, handed, "every handed session was captured");
    assert!(took > Duration::ZERO);

    assert!(
        app.pool.sessions[&0].session.reader_join.is_some(),
        "no reader was parked (`park_reader` takes this handle)"
    );
    assert!(app.pending_update_handoff.is_none() && app.update_handoff_prelaunch.is_none());
    let terminal = term.try_lock().expect("the engine is free again");
    assert_eq!(terminal.grid().scrollback_lines(), scrollback);
    assert_eq!(terminal.cursor(), cursor, "and read, not written");
}

/// A dry run that runs out of its window saturates the rung: an engine lock held
/// for the whole window ends it as `EngineBusy`, and 1.5 × the window is the
/// ceiling, so an untimeable desk gets the 80 ms rung 0.
#[test]
fn a_dry_run_that_runs_out_of_its_window_saturates_the_first_rung() {
    let mut app = crate::App::headless_for_test();
    let term = desk_with_history(&app);
    let held = crate::term_lock(&term);
    let dry = app.dry_run_handoff_capture(caps());
    drop(held);
    let DryRunCapture::AtLeast { took, sessions } = dry else {
        panic!("an engine held for the whole window is not measured: {dry:?}");
    };
    assert!(took >= DRY_RUN_CAPTURE_WINDOW, "{took:?}");
    assert_eq!(sessions, app.handoff_live_sessions().len());
    assert_eq!(
        FreezeSeed::DryRun {
            capture: took,
            sessions,
            last_park: None,
        }
        .rung0(),
        FREEZE_RUNG0_CEILING
    );
}

/// The seed end to end, over the shipping ledger writers: an automatic attempt's
/// dry run seeds rung 0 (never below the last landed park) and is kept in the
/// ledger; an explicit apply spends no dry run; a dry run that measured nothing
/// (a parser mid-sequence over live output) falls back to the ledger.
#[test]
fn the_first_rung_is_seeded_from_the_dry_run_and_kept_for_the_next_update() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let term = desk_with_history(&app);

    let (seed, kept) = app.seed_first_freeze_rung(ApplyMode::Immediate, caps());
    assert_eq!(
        seed,
        FreezeSeed::Default,
        "an explicit apply spends no dry run"
    );
    assert!(kept.is_none());

    // A park that landed earlier in 30 ms, the part of the freeze the dry run cannot time:
    // the rung is never budgeted below it.
    super::keep_landed_park(Duration::from_millis(30))
        .expect("the park cost is kept")
        .join()
        .expect("the ledger writer finished");
    let (seed, kept) = app.seed_first_freeze_rung(ApplyMode::Automatic, caps());
    let FreezeSeed::DryRun {
        capture,
        sessions,
        last_park,
    } = seed
    else {
        panic!("a quiet desk seeds from its own dry run: {seed:?}");
    };
    assert_eq!(sessions, app.handoff_live_sessions().len());
    assert_eq!(
        seed.rung0(),
        super::freeze_rung0_from_capture(capture.max(last_park.unwrap_or_default()))
    );
    kept.expect("the measurement is kept")
        .join()
        .expect("the ledger writer finished");
    let prior = aterm_update::handoff_capture_prior();
    if cfg!(target_os = "macos") {
        assert_eq!(last_park, Some(Duration::from_millis(30)));
        assert!(
            seed.rung0() >= Duration::from_millis(45),
            "a 30 ms landed park buys at least 45 ms: {seed}"
        );
        let prior = prior.expect("the ledger holds the measurement");
        assert_eq!(
            prior.capture_us,
            u64::try_from(capture.as_micros()).expect("fits").max(1)
        );
        assert_eq!(
            u128::from(prior.freeze_seed_ms),
            seed.rung0().as_millis(),
            "and the rung it seeded"
        );
        assert_eq!(prior.build, crate::running_build_number());
        assert_eq!(
            (prior.park_us, prior.park_build),
            (30_000, crate::running_build_number())
        );
    } else {
        assert_eq!(last_park, None, "only macOS keeps an apply-lane ledger");
        assert!(prior.is_none(), "only macOS keeps an apply-lane ledger");
    }

    // The parser parks mid-sequence with the rest of the sequence queued: the dry run cannot
    // say how long a capture takes. The pair is close-on-exec from birth, so a child another
    // test spawns never keeps the slave open and a closed slave reads as hung up.
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    // A hooked DCS query over queued output, the one partial sequence no carry
    // holds, so the capture misses.
    crate::term_lock(&term).process(b"\x1bP$qm");
    // SAFETY: a bounded write of a live slice to a test-owned descriptor.
    let wrote = unsafe { libc::write(slave, b"\x1b\\".as_ptr().cast(), 2) };
    assert_eq!(wrote, 2);
    assert!(matches!(
        app.dry_run_handoff_capture(caps()),
        DryRunCapture::Unmeasured(_)
    ));
    let (fallback, kept) = app.seed_first_freeze_rung(ApplyMode::Automatic, caps());
    assert!(kept.is_none(), "nothing measured, nothing kept");
    assert_eq!(
        fallback,
        FreezeSeed::from_prior(aterm_update::handoff_capture_prior().as_ref())
    );
    if cfg!(target_os = "macos") {
        assert_eq!(
            fallback.rung0(),
            seed.rung0(),
            "the fallback is the measurement the ledger kept"
        );
    }
    // SAFETY: closing the two test-owned descriptors exactly once.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = -1;
}

/// A prelaunched attempt as `prelaunch_out_of_band_handoff` records it,
/// carrying `freeze_seed`, and the receiver its park sends the capture to.
pub(crate) fn prelaunched(
    attempt_id: u64,
    mode: ApplyMode,
    freeze_seed: FreezeSeed,
) -> (
    crate::HandoffPrelaunch,
    std::sync::mpsc::Receiver<super::HandoffTransferJob>,
) {
    let (transfer, transferred) = std::sync::mpsc::sync_channel(1);
    let record = crate::HandoffPrelaunch {
        attempt_id,
        nonce: "0123456789abcdef0123456789abcdef".to_string(),
        mode,
        apply_attempt: None,
        same_image: Some(super::SameImageHandoff::DebugSeam),
        target_build: 7,
        target_commit: String::new(),
        cancel: std::sync::mpsc::sync_channel(1).0,
        stand_down: std::sync::mpsc::sync_channel(1).0,
        stand_down_ack: std::sync::mpsc::sync_channel(1).0,
        transfer,
        arbiter: crate::HandoffAttemptArbiter::new(),
        launched_at: std::time::Instant::now(),
        dialled: None,
        park_retry_at: None,
        park_misses: 0,
        park_mid_sequence_reparks: 0,
        freeze_seed,
        land_waits: 0,
        last_wait: None,
        stood_down: false,
        teardown: crate::DeferredHandoffTeardown::None,
        revoked_by_activity: false,
        history_export: None,
    };
    (record, transferred)
}

/// Seeding a prelaunched attempt stores the seed on that attempt, so the park's
/// budget is the seeded ladder, not the 20 ms default.
#[test]
fn seeding_stores_the_first_rung_on_the_prelaunched_attempt() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let _term = desk_with_history(&app);
    let (record, _transferred) = prelaunched(2, ApplyMode::Automatic, FreezeSeed::Default);
    app.update_handoff_prelaunch = Some(record);
    let (seed, kept) = app.seed_first_freeze_rung(ApplyMode::Automatic, caps());
    if let Some(writer) = kept {
        writer.join().expect("the ledger writer finished");
    }
    assert!(
        matches!(seed, FreezeSeed::DryRun { .. }),
        "a quiet desk seeds from its own dry run: {seed:?}"
    );
    assert_eq!(
        app.prelaunched_park_budget(),
        Some((0, seed.rung0(), seed)),
        "the park reads the seed the attempt was given"
    );
}

/// The launched lane's next park budget climbs with the attempt's own misses:
/// rung 0 is the dry run's seed, and each miss buys a strictly wider rung.
#[test]
fn the_launched_lanes_park_budget_climbs_its_attempts_seeded_ladder() {
    let mut app = crate::App::headless_for_test();
    assert!(app.prelaunched_park_budget().is_none());
    let seed = FreezeSeed::DryRun {
        capture: Duration::from_millis(30),
        sessions: 1,
        last_park: None,
    };
    let (record, _transferred) = prelaunched(1, ApplyMode::Automatic, seed);
    app.update_handoff_prelaunch = Some(record);
    let mut last = None;
    for misses in 0_u8..3 {
        app.update_handoff_prelaunch
            .as_mut()
            .expect("prelaunched")
            .park_misses = misses;
        let (prior, budget, carried) = app.prelaunched_park_budget().expect("prelaunched");
        assert_eq!((prior, carried), (misses, seed), "miss {misses}");
        if misses == 0 {
            assert_eq!(budget, seed.rung0(), "rung 0 is the seed's");
        }
        assert!(
            last.is_none_or(|last| budget > last),
            "miss {misses}: {budget:?} after {last:?} must be a wider rung"
        );
        last = Some(budget);
    }
}

/// A landed park (the launched lane's real park over a real PTY) writes its cost into the
/// ledger's landed-park slot, the floor for the next update's first rung.
#[test]
fn a_landed_park_keeps_its_cost_for_the_next_updates_first_rung() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    // A sentinel no real park costs, written and joined first, so the wait below is for this
    // park's write.
    super::keep_landed_park(Duration::from_micros(1))
        .expect("the sentinel is kept")
        .join()
        .expect("the ledger writer finished");
    let mut app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    let (record, transferred) = prelaunched(3, ApplyMode::Immediate, FreezeSeed::Default);
    app.update_handoff_prelaunch = Some(record);

    app.on_update_handoff_awaiting_park(3, Some(4242), None);
    let transfer = transferred
        .try_recv()
        .expect("the park landed and handed its capture over");
    assert!(
        app.pending_update_handoff.is_some(),
        "the attempt is parked"
    );
    if cfg!(target_os = "macos") {
        // The writer runs detached (the park never waits on the ledger's
        // file lock): wait for it to land, the causal event.
        super::ledger_writers::wait_until_landed();
        let kept = aterm_update::handoff_capture_prior().expect("the sentinel is held");
        assert_ne!(
            kept.park_us, 1,
            "the landed park's cost never reached the ledger"
        );
        assert!(
            u128::from(kept.park_us) < super::park_budget_for_test::HANG_DETECTOR.as_micros(),
            "a park that landed inside its budget: {kept:?}"
        );
        assert_eq!(kept.park_build, crate::running_build_number());
    } else {
        assert!(
            aterm_update::handoff_capture_prior().is_none(),
            "only macOS keeps an apply-lane ledger"
        );
    }

    // The capture's descriptors are duplicates it owns; the session's are
    // the test's, closed once, here.
    drop(transfer);
    app.pending_update_handoff = None;
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = -1;
    // SAFETY: closing the two test-owned descriptors exactly once.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// Session 0 on a real, quiet, close-on-exec PTY, as the park needs it (a
/// stub's `-1` master reads as activity). Returns `(master, slave)`,
/// which [`release_pty`] hands back.
fn quiet_pty(app: &mut crate::App) -> (i32, i32) {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    (master, slave)
}

fn release_pty(app: &mut crate::App, (master, slave): (i32, i32)) {
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = -1;
    // SAFETY: closing the two test-owned descriptors exactly once.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// Publishes a passed pre-verification of `ticket`'s artifact carrying `policy`
/// (`None`: no policy file), as the handoff worker does, and returns the policy
/// the park will read.
fn publish(
    app: &crate::App,
    ticket: &crate::native_updater_service::ApplyAttemptTicket,
    policy: Option<&str>,
) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
    use aterm_update_core::handoff_policy::{PolicyFile, PolicyRead};
    let read = policy.map_or(PolicyRead::Absent, |text| {
        PolicyRead::Parsed(PolicyFile::parse(text).expect("a schema-1 policy"))
    });
    app.publish_preverified_pass_for_test(ticket, &read)
}

/// `extra` more live sessions (ids 1..=extra) beside session 0, each owning a duplicate of
/// session 0's quiet master, registered and pooled as spawned tabs are.
fn add_sessions_on(app: &mut crate::App, master: i32, extra: u64) {
    for id in 1..=extra {
        // SAFETY: `master` is the test's open PTY master; `F_DUPFD_CLOEXEC` returns a new
        // descriptor this session owns, close-on-exec from birth.
        let dup = unsafe { libc::fcntl(master, libc::F_DUPFD_CLOEXEC, 0) };
        assert!(dup >= 0, "dup the quiet master");
        let mut session = crate::stub_session(id);
        session.master = dup;
        crate::App::register_session(&app.store, &session, None);
        app.pool.insert(session);
        app.next_session_id = app.next_session_id.max(id + 1);
    }
}

/// Publishes a pass of `ticket`'s artifact declaring the chunked grant, as the
/// arm-time pre-verification caches it.
fn publish_chunked_pass(
    app: &crate::App,
    ticket: &crate::native_updater_service::ApplyAttemptTicket,
) {
    super::PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(ticket))
        .expect("an attempt names its artifact")
        .publish_pass(
            &aterm_update::HandoffCandidateFacts {
                policy: aterm_update_core::handoff_policy::PolicyRead::Absent,
                grant_chunks: true,
            },
            crate::running_build_number(),
            "the test candidate",
        );
}

/// The park holds the pool to the dialled claim before it freezes: a cached pass
/// says the candidate declares the chunked grant, but the held successor claimed
/// `ATRZ1C`. `transfer` would refuse 63 sessions to that claim, so the park must
/// refuse them first, with every reader live and nothing captured.
#[test]
fn a_park_refuses_a_desk_the_dialled_claim_cannot_carry_before_freezing() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let pty = quiet_pty(&mut app);
    add_sessions_on(&mut app, pty.0, 62);
    assert_eq!(app.handoff_live_sessions().len(), 63, "PRECONDITION");
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ab".repeat(32));
    publish_chunked_pass(&app, &ticket);
    assert!(
        app.park_policy(Some(&ticket)).grant_chunks,
        "PRECONDITION — the cached pass declares the chunked grant"
    );
    let (mut record, transferred) = prelaunched(21, ApplyMode::Immediate, FreezeSeed::Default);
    record.apply_attempt = Some(ticket);
    app.update_handoff_prelaunch = Some(record);

    app.update_handoff_prelaunch
        .as_mut()
        .expect("prelaunched")
        .dialled = Some(crate::DialledSuccessor {
        pid: Some(4242),
        // What an `ATRZ1C` claim carries.
        grant_limit: Some(62),
        at: std::time::Instant::now(),
    });
    let attempt = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    let super::ParkAttempt::Failed(stand_down) = attempt else {
        panic!("the park must refuse a desk the claim cannot carry: {attempt:?}");
    };
    assert_eq!(
        stand_down.outcome,
        crate::UpdateHandoffOutcome::ProducerFailed
    );
    assert!(transferred.try_recv().is_err(), "nothing was captured");
    assert!(app.pending_update_handoff.is_none(), "no reader was parked");
    app.update_handoff_prelaunch = None;
    app.pool.sessions.retain(|id, _| *id == 0);
    release_pty(&mut app, pty);
}

/// A fork has no descriptor-message limit: when the launched lane falls back at runtime
/// (`fork_after_park`, no dialer and no grant limit) with 63 tabs, the park is not refused and
/// the fork gets every session by inheritance. The park's deadline is the test park's
/// one-minute hang detector (`park_budget_for_test`), since a 63-session park in a loaded
/// debug build can miss the quarter-second rung.
#[test]
fn a_fork_after_park_is_not_held_to_the_rendezvous_session_limit() {
    // A landed park starts a detached ledger writer.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let pty = quiet_pty(&mut app);
    add_sessions_on(&mut app, pty.0, 62);
    assert_eq!(app.handoff_live_sessions().len(), 63, "PRECONDITION");
    let (record, transferred) = prelaunched(22, ApplyMode::Immediate, FreezeSeed::Default);
    app.update_handoff_prelaunch = Some(record);

    app.on_update_handoff_awaiting_park(22, None, None);
    assert!(
        app.update_handoff_prelaunch
            .as_ref()
            .is_some_and(|prelaunch| !prelaunch.stood_down),
        "a fork passes its 63 masters by inheritance; no descriptor message bounds it"
    );
    let transfer = transferred
        .try_recv()
        .expect("the park got past the limit check, landed and handed the fork its capture");
    assert_eq!(transfer.capture.live.len(), 63, "every session is handed");
    drop(transfer);
    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    app.pool.sessions.retain(|id, _| *id == 0);
    release_pty(&mut app, pty);
}

/// A launchd-limited producer makes room for its own park. A window started from the Dock,
/// Finder or a login item runs at launchd's soft 256 descriptors: about 30 of its own and
/// three per session, so a 63-session desk leaves 37 where the park's duplicates need 63. At
/// exactly that headroom the park is refused (the control); the attempt's raise
/// (`raise_descriptor_limit_for_park`) makes room while new tabs keep the launcher's limit.
/// Toward an older target no raise is made, and an older successor starts at launchd's limit
/// unless handed a duplicate only the raise fit.
#[cfg(target_os = "macos")]
#[test]
fn a_launchd_limited_producer_makes_room_to_park_a_chunked_desk() {
    use std::os::fd::FromRawFd as _;
    const LAUNCHD_SOFT_NOFILE: u64 = 256;
    const SESSIONS: u64 = 63;
    const WINDOW_BASE_NOFILE: u64 = 30;
    // The running build a target is compared with; only the comparison matters.
    const THIS_BUILD: u64 = 7;

    if !crate::control_auth::enter_low_nofile_test_child(1024) {
        return;
    }
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let pty = quiet_pty(&mut app);
    add_sessions_on(&mut app, pty.0, SESSIONS - 1);
    assert_eq!(
        app.handoff_live_sessions().len(),
        usize::try_from(SESSIONS).expect("a count"),
        "PRECONDITION"
    );
    // Each session's wake pipe, as every spawned session holds one.
    let wake_pipes: Vec<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> = (0..SESSIONS)
        .map(|_| {
            let mut fds = [-1i32; 2];
            // SAFETY: a valid two-slot out-array for pipe(2).
            assert_eq!(
                unsafe { libc::pipe(fds.as_mut_ptr()) },
                0,
                "PRECONDITION: the desk fits"
            );
            // SAFETY: pipe(2) returned two fresh descriptors owned here.
            unsafe {
                (
                    std::os::fd::OwnedFd::from_raw_fd(fds[0]),
                    std::os::fd::OwnedFd::from_raw_fd(fds[1]),
                )
            }
        })
        .collect();
    // Launchd's headroom, not its number: a window's ~30 descriptors of its own (measured) and
    // this desk leave `256 - 30 - 3 × 63` = 37. The soft limit is set to leave exactly that
    // here, since this binary's own table is smaller than a window's and, under a std without
    // SP-1, a child of a busy test run inherits what other tests held open across its spawn.
    let headroom = LAUNCHD_SOFT_NOFILE - WINDOW_BASE_NOFILE - 3 * SESSIONS;
    let listed: Vec<i32> = std::fs::read_dir("/dev/fd")
        .expect("the descriptor directory")
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    let open: Vec<u64> = listed
        .into_iter()
        // SAFETY: F_GETFD on any number only asks; the listing's own
        // descriptor is closed by now and answers EBADF.
        .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .filter_map(|fd| u64::try_from(fd).ok())
        .collect();
    let launchd_soft = (0..)
        .find(|limit: &u64| {
            let used = open.iter().filter(|fd| *fd < limit).count();
            limit - u64::try_from(used).expect("a count") == headroom
        })
        .expect("a limit with that headroom");
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource id and a writable out-parameter.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
        0
    );
    let launchd = libc::rlimit {
        rlim_cur: launchd_soft,
        rlim_max: limit.rlim_max,
    };
    // SAFETY: a child-only change that keeps the hard limit as it was.
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const launchd) },
        0
    );
    // A window at launchd's limit holds no descriptor at or above it: whatever a busy test run
    // leaked into this child up there is marked close-on-exec, up to the kernel's maximum, so
    // no successor below can be handed it.
    for fd in i32::try_from(launchd_soft).expect("a descriptor number")
        ..aterm_uds::spawnfd::descriptor_bound()
    {
        // SAFETY: F_GETFD/F_SETFD on any number only reads or sets its
        // close-on-exec flag; a number that is not open answers -1.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
    let (mut record, transferred) = prelaunched(23, ApplyMode::Immediate, FreezeSeed::Default);
    record.dialled = Some(crate::DialledSuccessor {
        pid: Some(4242),
        // What an `ATRZ2C` claim carries.
        grant_limit: Some(crate::handoff_rendezvous::grant_session_limit(true)),
        at: std::time::Instant::now(),
    });
    app.update_handoff_prelaunch = Some(record);

    let refused = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    let super::ParkAttempt::Failed(stand_down) = refused else {
        panic!("CONTROL: at launchd's limit the duplicate runs out: {refused:?}");
    };
    assert_eq!(
        stand_down.outcome,
        crate::UpdateHandoffOutcome::ProducerFailed
    );
    assert!(transferred.try_recv().is_err(), "CONTROL: nothing captured");

    // Toward an older target (the fork lane's alone, whose child inherits this limit and may
    // not read the launcher's limit it is told) no raise is made, the shells' record stays
    // empty, and the park still fails.
    let soft_nofile = || {
        let mut now = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: a valid resource id and a writable out-parameter.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut now) },
            0
        );
        now.rlim_cur
    };
    crate::handoff_rendezvous::raise_descriptor_limit_for_park(THIS_BUILD - 1, THIS_BUILD);
    assert_eq!(
        soft_nofile(),
        launchd_soft,
        "an older target leaves the limit"
    );
    assert_eq!(
        crate::spawn::daily_driver_limits().open_files,
        None,
        "…and records nothing for the shells"
    );
    let still = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    let super::ParkAttempt::Failed(stand_down) = still else {
        panic!("an older target's park fails as before: {still:?}");
    };
    assert_eq!(
        stand_down.outcome,
        crate::UpdateHandoffOutcome::ProducerFailed
    );
    assert!(transferred.try_recv().is_err(), "nothing captured");

    // The same build (a same-image re-exec), and so every newer one, reads the told limit: the
    // raise is made and the duplicate has room for every master. Whether the park then lands
    // inside its 250 ms rung is the clock's question; a miss there is a re-park.
    crate::handoff_rendezvous::raise_descriptor_limit_for_park(THIS_BUILD, THIS_BUILD);
    assert!(soft_nofile() > launchd_soft, "the park's raise is made");
    let (reserved, adoption) = super::reserve_child_only_masters(&app.handoff_live_sessions())
        .expect("the raise makes room for every duplicate");
    assert_eq!(
        reserved.len(),
        usize::try_from(SESSIONS).expect("a count"),
        "every session is reserved"
    );
    assert_eq!(
        crate::spawn::daily_driver_limits().open_files,
        Some(launchd_soft),
        "a tab opened afterwards still gets the launcher's limit"
    );

    // …and since nothing lowers the raised limit again, a later attempt toward an older build
    // starts its successor at the limit held before the raise (`start_older_image_at_its_limit`),
    // which its stand-in's `ulimit -n` reads.
    let successor_soft = |target_build: u64, handed: &[i32]| -> u64 {
        // std on purpose: the shipped hook under test installs a `pre_exec`.
        let mut successor = std::process::Command::new("/bin/sh");
        successor.args(["-c", "ulimit -n"]);
        crate::handoff_rendezvous::start_older_image_at_its_limit(
            &mut successor,
            target_build,
            THIS_BUILD,
            handed,
        );
        let out = successor.output().expect("run the successor's stand-in");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("a limit")
    };
    let raised = soft_nofile();
    assert_eq!(
        successor_soft(THIS_BUILD - 1, &[]),
        launchd_soft,
        "an older successor starts at launchd's limit, not the {raised} the park left"
    );
    assert_eq!(
        successor_soft(THIS_BUILD, &[]),
        raised,
        "a same-or-newer one inherits the room, and reads the told limit"
    );
    // A successor handed this desk's duplicates, which outgrew launchd's limit, keeps the room
    // it is handed them in: its strip would not reach them under launchd's.
    let above: Vec<i32> = adoption
        .iter()
        .map(|(_, master, _)| *master)
        .filter(|master| u64::try_from(*master).is_ok_and(|master| master >= launchd_soft))
        .collect();
    assert!(
        !above.is_empty(),
        "PRECONDITION: the duplicates outgrew launchd's limit"
    );
    assert_eq!(successor_soft(THIS_BUILD - 1, &above), raised);

    drop(reserved);
    drop(wake_pipes);
    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    app.pool.sessions.retain(|id, _| *id == 0);
    release_pty(&mut app, pty);
}

/// The scrollback sidecars in `dir`.
fn sidecars(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.to_string_lossy().ends_with(".hist"))
                .collect()
        })
        .unwrap_or_default()
}

/// The launched lane sends no scrollback under a policy that carries none: its history export
/// starts before the policy can be read, so under `carry = "visible"` or `"repaint"` the park
/// stops it, removes its sidecar and hands the worker `HistoryPlan::Withheld`, and the join
/// counts every line as the policy's. Control: with no policy the sidecar is named.
#[test]
fn a_policy_without_scrollback_withholds_the_launched_lanes_history_carry() {
    use crate::handoff_history::{Fallback, HistoryExporter, HistoryPlan};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let nonce = "0123456789abcdef0123456789abcdef";
    for (attempt_id, policy) in [
        (11_u64, None),
        (12, Some("schema = 1\ncarry = \"visible\"\n")),
        (13, Some("schema = 1\ncarry = \"repaint\"\n")),
    ] {
        let at = format!("policy {policy:?}");
        let withheld = policy.is_some();
        let mut app = crate::App::headless_for_test();
        let term = desk_with_history(&app);
        let seen = crate::term_lock(&term).history_fence().lines();
        assert!(seen > 0, "{at}: PRECONDITION — the tab holds scrollback");
        let pty = quiet_pty(&mut app);
        let dir = std::env::temp_dir().join(format!(
            "aterm-withheld-history-{}-{attempt_id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");

        // The export the launch started, its first pass on disk.
        let mut export =
            HistoryExporter::start(dir.clone(), vec![(0, std::sync::Arc::clone(&term))], true)
                .expect("the export starts");
        assert!(
            export.await_caught_up(aterm_spec::wait::MUST_HAPPEN),
            "{at}: its first pass finishes"
        );
        assert_eq!(
            sidecars(&dir).len(),
            1,
            "{at}: PRECONDITION — its sidecar is on disk"
        );

        let ticket =
            crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ab".repeat(32));
        let read = publish(&app, &ticket, policy);
        assert_eq!(
            read.is_some_and(|read| !read.carry_ceiling().carries_scrollback()),
            withheld,
            "{at}: PRECONDITION — the park reads this policy"
        );
        let (mut record, transferred) =
            prelaunched(attempt_id, ApplyMode::Immediate, FreezeSeed::Default);
        record.apply_attempt = Some(ticket);
        record.history_export = Some(export);
        app.update_handoff_prelaunch = Some(record);

        // The landed park writes its cost on a detached thread; write a sentinel
        // first so the wait below is for this park's write.
        super::keep_landed_park(Duration::from_micros(1))
            .expect("the sentinel is kept")
            .join()
            .expect("the ledger writer finished");
        app.on_update_handoff_awaiting_park(attempt_id, Some(4242), None);
        let transfer = transferred
            .try_recv()
            .unwrap_or_else(|_| panic!("{at}: the park landed and handed its capture over"));
        if cfg!(target_os = "macos") {
            super::ledger_writers::wait_until_landed();
            assert!(
                aterm_update::handoff_capture_prior().is_some_and(|prior| prior.park_us != 1),
                "{at}: the landed park's cost never reached the ledger"
            );
        }
        let mut capture = transfer.capture;
        assert_eq!(
            matches!(capture.history, HistoryPlan::Withheld),
            withheld,
            "{at}: the plan the park hands the worker ({:?})",
            capture.history
        );
        if withheld {
            // The export is stopped at the park and its files go with it (bounded: its worker
            // may finish its last look just after the park's own bounded wait).
            let patience = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while !sidecars(&dir).is_empty() {
                assert!(
                    std::time::Instant::now() < patience,
                    "{at}: the withheld export's sidecar was never removed"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        // The worker's join, as `prepare_outgoing_artifacts` runs it.
        let verdicts = crate::handoff_history::stamp_manifest(
            &mut capture.manifest,
            &capture.screens,
            &capture.history_heads,
            std::mem::replace(&mut capture.history, HistoryPlan::Unexported).results(&dir),
            &dir,
            nonce,
        );
        let record = capture
            .manifest
            .sessions
            .iter()
            .find(|record| record.local_id == 0)
            .expect("session 0 is handed");
        let (_, joined) = verdicts
            .iter()
            .find(|(local_id, _)| *local_id == 0)
            .expect("session 0 is joined");
        if withheld {
            assert_eq!(record.history, None, "{at}: NO SIDECAR IS NAMED");
            assert!(sidecars(&dir).is_empty(), "{at}: and none is on disk");
            assert_eq!(
                (record.history_dropped, record.history_lost),
                (seen, seen),
                "{at}: every line the park saw is counted as left behind"
            );
            assert_eq!(
                joined.fallback,
                Some(Fallback::Withheld),
                "{at}: said as the policy's"
            );
            assert!(
                record.history_withheld,
                "{at}: and the record tells the successor so"
            );
        } else {
            assert!(
                record.history.is_some(),
                "{at}: CONTROL — with no policy the sidecar is named"
            );
            assert_eq!(
                record.history_dropped, 0,
                "{at}: and nothing is left behind"
            );
            assert!(!record.history_withheld, "{at}: nor withheld");
        }

        drop(capture);
        app.pending_update_handoff = None;
        release_pty(&mut app, pty);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Holds `term`'s engine from another thread past the widest freeze rung, returning once held,
/// so a park on the real rung (`park_budget_for_test::real`) misses with `EngineBusy` and its
/// rollback waits the hold out.
pub(super) fn hold_engine_past_every_rung(
    term: &std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
) -> std::thread::JoinHandle<()> {
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let term = std::sync::Arc::clone(term);
    let holder = std::thread::spawn(move || {
        let guard = crate::term_lock(&term);
        held_tx.send(()).expect("the test waits");
        std::thread::sleep(Duration::from_millis(1500));
        drop(guard);
    });
    held_rx.recv().expect("the engine is held");
    holder
}

/// A missed park resumes the history export it paused: after a miss (engine held past the
/// deadline) the tab prints more than 256 lines before the re-park, and the whole history still
/// crosses through the second capture's join.
#[test]
fn a_missed_park_resumes_the_history_export_for_the_repark() {
    use crate::handoff_history::{HistoryExporter, HistoryPlan};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let nonce = "0123456789abcdef0123456789abcdef";
    let mut app = crate::App::headless_for_test();
    let term = app.pool.get(0).expect("session 0").term.clone();
    for n in 0..2000 {
        crate::term_lock(&term)
            .process(format!("line {n}: the history a capture carries\r\n").as_bytes());
    }
    let pty = quiet_pty(&mut app);
    let dir = std::env::temp_dir().join(format!("aterm-repark-history-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let mut export =
        HistoryExporter::start(dir.clone(), vec![(0, std::sync::Arc::clone(&term))], true)
            .expect("the export starts");
    assert!(export.await_caught_up(aterm_spec::wait::MUST_HAPPEN));
    let (mut record, transferred) = prelaunched(31, ApplyMode::Immediate, FreezeSeed::Default);
    record.history_export = Some(export);
    record.dialled = Some(crate::DialledSuccessor {
        pid: Some(4242),
        grant_limit: None,
        at: std::time::Instant::now(),
    });
    app.update_handoff_prelaunch = Some(record);

    // The first park misses: the engine is held past every rung's deadline, on the real rung
    // (a test park's default deadline would wait it out).
    let real = super::park_budget_for_test::real();
    let holder = hold_engine_past_every_rung(&term);
    let first = std::time::Instant::now();
    app.try_park_for_prelaunched_successor(first);
    holder.join().expect("the holder finished");
    drop(real);
    assert!(
        transferred.try_recv().is_err(),
        "PRECONDITION — the park missed"
    );
    let prelaunch = app.update_handoff_prelaunch.as_ref().expect("still held");
    assert_eq!(
        prelaunch.park_misses, 1,
        "PRECONDITION — one miss, re-parked"
    );
    let retry_at = prelaunch.park_retry_at.expect("a re-park is scheduled");

    // The tab prints past the screen carry's lines through the retry delay; the resumed
    // export follows it.
    for n in 2000..2600 {
        crate::term_lock(&term)
            .process(format!("line {n}: printed during the retry delay\r\n").as_bytes());
    }
    std::thread::sleep(crate::handoff_history::FOLLOW_EVERY * 4);

    super::keep_landed_park(Duration::from_micros(1))
        .expect("the sentinel is kept")
        .join()
        .expect("the ledger writer finished");
    app.try_park_for_prelaunched_successor(retry_at + Duration::from_millis(1));
    let transfer = transferred.try_recv().expect("the re-park landed");
    if cfg!(target_os = "macos") {
        super::ledger_writers::wait_until_landed();
        assert!(
            aterm_update::handoff_capture_prior().is_some_and(|prior| prior.park_us != 1),
            "the landed park's cost never reached the ledger"
        );
    }
    let mut capture = transfer.capture;
    let seen = capture
        .history_heads
        .iter()
        .find(|head| head.local_id == 0)
        .expect("session 0's head")
        .fence
        .lines();
    let verdicts = crate::handoff_history::stamp_manifest(
        &mut capture.manifest,
        &capture.screens,
        &capture.history_heads,
        std::mem::replace(&mut capture.history, HistoryPlan::Unexported).results(&dir),
        &dir,
        nonce,
    );
    let (_, joined) = verdicts
        .iter()
        .find(|(local_id, _)| *local_id == 0)
        .expect("session 0 is joined");
    let record = capture
        .manifest
        .sessions
        .iter()
        .find(|record| record.local_id == 0)
        .expect("session 0 is handed");
    assert_eq!(joined.fallback, None, "the export meets the re-park's head");
    assert!(joined.take > 0, "the sidecar carries the older history");
    assert_eq!(
        record.history_dropped, 0,
        "none of the {seen} lines the re-park saw is left behind"
    );

    drop(capture);
    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    release_pty(&mut app, pty);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The dry run captures under the successor's policy. With aterm-core's
/// `checkpoint.carry_projection` fault point armed (every non-blank grid projection panics):
///
/// * with the policy not read yet, no dry run is taken (`Unmeasured`) and the seed falls back
///   to the ledger;
/// * under `carry = "repaint"`, the dry run is measured, projects nothing, and seeds the rung;
/// * control: under a pass with no policy (`full`), the dry run panics at the fault point.
#[test]
fn the_dry_run_captures_under_the_successors_policy_and_not_before_it_is_read() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let _term = desk_with_history(&app);
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ef".repeat(32));
    let (mut record, _transferred) = prelaunched(21, ApplyMode::Automatic, FreezeSeed::Default);
    record.apply_attempt = Some(ticket.clone());
    app.update_handoff_prelaunch = Some(record);
    let armed = |app: &mut crate::App| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            aterm_core::fault::with_armed("checkpoint.carry_projection", || {
                app.dry_run_handoff_capture(caps())
            })
        }))
    };

    // Unread: nothing is captured, so nothing projects.
    let unread = armed(&mut app).expect("an unread policy runs no capture");
    assert!(matches!(unread, DryRunCapture::Unmeasured(_)), "{unread:?}");
    let (fallback, kept) = app.seed_first_freeze_rung(ApplyMode::Automatic, caps());
    assert!(kept.is_none(), "nothing measured, nothing kept");
    assert_eq!(
        fallback,
        FreezeSeed::from_prior(aterm_update::handoff_capture_prior().as_ref()),
        "the seed falls back to the ledger"
    );

    // Repaint: measured, and never projected.
    publish(&app, &ticket, Some("schema = 1\ncarry = \"repaint\"\n"))
        .expect("the repaint policy applies to every build");
    let repaint = armed(&mut app).expect("a repaint policy's dry run never projects a grid");
    assert!(
        matches!(
            repaint,
            DryRunCapture::Measured { sessions: 1.., .. }
                | DryRunCapture::AtLeast { sessions: 1.., .. }
        ),
        "{repaint:?}"
    );
    let (seed, kept) = aterm_core::fault::with_armed("checkpoint.carry_projection", || {
        app.seed_first_freeze_rung(ApplyMode::Automatic, caps())
    });
    if let Some(writer) = kept {
        writer.join().expect("the ledger writer finished");
    }
    assert!(
        matches!(seed, FreezeSeed::DryRun { .. }),
        "the rung is seeded from the policy's own capture: {seed:?}"
    );

    // Control, last (a panic inside the capture poisons the engine's lock): a pass with no
    // policy is `full`, and that capture projects.
    assert_eq!(publish(&app, &ticket, None), None);
    assert!(
        armed(&mut app).is_err(),
        "the fault point must fire where a Full capture projects, or this test proves nothing"
    );
}
