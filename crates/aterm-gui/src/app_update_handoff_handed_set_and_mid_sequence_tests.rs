// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    CaptureFailure, ParkAttempt, ParkGate, ParkGateFacts, classify_capture_failure,
    handed_sessions_only, handoff_masters_closed, handoff_masters_have_activity,
    prelaunch_hold_cap, prelaunch_park_admitted, stamp_fg_holders, stamp_journal_keys,
    stamp_render_debts,
};
use crate::native_update_auto_intent::{ActivityFacts, ApplyPhase};
use crate::native_updater_service::ApplyMode;

/// A PTY pair with both ends marked close-on-exec.
fn openpty() -> (i32, i32) {
    aterm_pty::open_pty_pair().expect("open_pty_pair")
}

fn write_all(fd: i32, bytes: &[u8]) {
    // SAFETY: a bounded write of a live slice to a test-owned descriptor.
    let wrote = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(usize::try_from(wrote).ok(), Some(bytes.len()), "write");
}

/// Drain all queued bytes from `master` until the PTY is quiet.
fn drain(master: i32) {
    let entry = [(0u64, master, 0i32)];
    let mut buffer = [0u8; 4096];
    while handoff_masters_have_activity(&entry) {
        // SAFETY: a bounded read into a stack buffer from a readable master.
        let read = unsafe { libc::read(master, buffer.as_mut_ptr().cast(), buffer.len()) };
        assert!(read > 0, "a readable master reads");
    }
}

/// Capture parked screens for the live handed set under current wire capabilities.
fn capture(app: &mut crate::App) -> Result<Vec<u64>, CaptureFailure> {
    capture_for(app, crate::seamless::WireCaps::current())
}

/// [`capture`] for a successor with `caps`.
fn capture_for(
    app: &mut crate::App,
    caps: crate::seamless::WireCaps,
) -> Result<Vec<u64>, CaptureFailure> {
    let live = app.handoff_live_sessions();
    let window = crate::app_update_handoff::park_budget_for_test::deadline_budget(
        std::time::Duration::from_secs(30),
    );
    app.capture_parked_screens(
        &live,
        std::time::Instant::now() + window,
        window.as_millis(),
        window / 2,
        caps,
        aterm_update_core::handoff_policy::CarryCeiling::Full,
    )
    .map(|parked| parked.screens.iter().map(|(id, _)| *id).collect())
}

/// On a hop to a successor without parser carry, a partial sequence over queued output misses the park while stalled or boundary states are carried.
#[test]
fn an_older_successor_misses_a_split_csi_over_queued_output() {
    let running = 200;
    let older = crate::seamless::WireCaps::for_hop(running, running - 1);
    assert!(!older.carries_parser(), "PRECONDITION: an older hop");
    assert!(crate::seamless::WireCaps::for_hop(running, running).carries_parser());
    let mut app = crate::App::headless_for_test();
    let (master, slave) = openpty();
    set_master(&mut app, 0, master);
    let term = app.pool.get(0).expect("session 0").term.clone();
    crate::term_lock(&term).process(b"$ make\r\n\x1b[38;5;19");
    write_all(slave, b"6mcompiling\r\n");
    let failure = capture_for(&mut app, older).expect_err("a split CSI over queued output");
    assert!(
        matches!(failure, CaptureFailure::MidSequence { local_id: 0, .. }),
        "{failure:?}"
    );
    assert!(
        matches!(classify_capture_failure(&failure), ParkAttempt::Missed(_)),
        "timing: re-parked on the next rung, never a refusal"
    );
    assert_eq!(
        capture(&mut app),
        Ok(vec![0]),
        "the same parser is carried whole to a successor that reads it"
    );

    drain(master);
    assert_eq!(capture_for(&mut app, older), Ok(vec![0]));
    crate::term_lock(&term).process(b"6m");
    write_all(slave, b"compiling\r\n");
    assert_eq!(capture_for(&mut app, older), Ok(vec![0]));

    set_master(&mut app, 0, -1);
    drop(app);
    // Close master first so closing slave does not block on macOS tty drain.
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
}

fn set_master(app: &mut crate::App, id: u64, master: i32) {
    app.pool
        .sessions
        .get_mut(&id)
        .expect("the session is pooled")
        .session
        .master = master;
}

/// A parser parked inside a hooked DCS query (`DECRQSS`) over queued output misses the park, whereas quiet PTYs, boundaries, split CSIs, and tmux passthrough wrappers are carried.
#[test]
fn a_parser_parked_mid_sequence_over_queued_output_misses_the_park() {
    let mut app = crate::App::headless_for_test();
    let (master, slave) = openpty();
    set_master(&mut app, 0, master);
    let term = app.pool.get(0).expect("session 0").term.clone();
    crate::term_lock(&term).process(b"$ make\r\n\x1b[38;5;19");
    write_all(slave, b"6mcompiling\r\n");
    assert_eq!(
        capture(&mut app),
        Ok(vec![0]),
        "a split CSI over queued output is carried whole"
    );
    drain(master);
    crate::term_lock(&term).process(b"6m\x1bPtmu");
    write_all(slave, b"x;\x1b\x1b]2;inner\x07\x1b\\");
    assert_eq!(
        capture(&mut app),
        Ok(vec![0]),
        "a tmux passthrough wrapper over queued output is carried: nothing is lost"
    );
    drain(master);
    crate::term_lock(&term).process(b"x;\x1b\x1b]2;inner\x07\x1b\\\x1bP$qm");
    write_all(slave, b"\x1b\\");

    let failure = capture(&mut app).expect_err("a hooked DCS query over queued output");
    assert_eq!(
        failure,
        CaptureFailure::MidSequence {
            local_id: 0,
            state: "DcsPassthrough"
        }
    );
    assert!(
        matches!(classify_capture_failure(&failure), ParkAttempt::Missed(_)),
        "timing: re-parked on the next rung, never a refusal"
    );

    drain(master);
    assert_eq!(
        capture(&mut app),
        Ok(vec![0]),
        "a stalled sequence is carried"
    );

    crate::term_lock(&term).process(b"\x1b\\");
    write_all(slave, b"more output\r\n");
    assert_eq!(
        capture(&mut app),
        Ok(vec![0]),
        "a boundary over a busy PTY is carried"
    );

    set_master(&mut app, 0, -1);
    drop(app);
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
}

/// An over-cap OSC or started APC over queued output misses the park (a picture only while a free
/// re-park remains), whereas short OSCs and DCS headers are carried.
#[test]
fn a_carry_that_only_swallows_the_queued_tail_misses_the_park() {
    let mut app = crate::App::headless_for_test();
    let (master, slave) = openpty();
    set_master(&mut app, 0, master);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut clipboard = b"\x1b]52;c;".to_vec();
    clipboard.extend(std::iter::repeat_n(b'A', 5000));
    let mut image = b"\x1b]1337;File=inline=1:".to_vec();
    image.extend(std::iter::repeat_n(b'A', 5000));
    for free_repark in [false, true] {
        app.update_handoff_prelaunch = free_repark.then(|| {
            let (mut prelaunch, ..) =
                super::late_park_record_tests::prelaunch_record(1, ApplyMode::AutomaticPastGrace);
            prelaunch.park_mid_sequence_reparks = super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS - 1;
            prelaunch
        });
        for (at, parked, queued, state, picture) in [
            (
                "a kitty-graphics APC",
                &b"\x1b_Ga=T,f=100;"[..],
                &b"iVBORw0KGgo=\x1b\\"[..],
                "SosPmApcString",
                true,
            ),
            (
                "an OSC 1337 image over the carry's cap",
                &image[..],
                &b"QUFBQQ==\x07"[..],
                "OscString",
                true,
            ),
            (
                "an OSC 52 over the carry's cap",
                &clipboard[..],
                &b"QUFBQQ==\x07"[..],
                "OscString",
                false,
            ),
            (
                "a hooked sixel string",
                &b"\x1bPq#0;2;0;0;0"[..],
                &b"#0!10~-\x1b\\"[..],
                "DcsPassthrough",
                true,
            ),
        ] {
            crate::term_lock(&term).process(parked);
            write_all(slave, queued);
            let expected = if picture && !free_repark {
                Ok(vec![0])
            } else {
                Err(CaptureFailure::MidSequence { local_id: 0, state })
            };
            assert_eq!(
                capture(&mut app),
                expected,
                "{at} over queued output (a free re-park left: {free_repark})"
            );
            drain(master);
            assert_eq!(capture(&mut app), Ok(vec![0]), "{at}, stalled, is carried");
            crate::term_lock(&term).process(b"\x18");
        }
    }
    app.update_handoff_prelaunch = None;
    for (at, parked, queued) in [
        ("a short OSC", &b"\x1b]0;a ti"[..], &b"tle\x07"[..]),
        (
            "a DCS header",
            &b"\x1bP1;2"[..],
            &b"q#0;2;0;0;0#0!10~-\x1b\\"[..],
        ),
    ] {
        crate::term_lock(&term).process(parked);
        write_all(slave, queued);
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "{at} over queued output is carried whole"
        );
        drain(master);
        crate::term_lock(&term).process(b"\x18");
    }

    set_master(&mut app, 0, -1);
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

/// Held-pane capture projects at most `MAX_HELD_PANES` exited panes and stops at `held_capture_deadline` (a quarter of the budget before the park deadline).
#[test]
fn the_held_capture_stops_at_the_held_caps_and_at_its_own_deadline() {
    let mut app = crate::App::headless_for_test();
    let (master, slave) = openpty();
    set_master(&mut app, 0, master);
    let line = "held output ".repeat(24);
    for id in 1..=70_u64 {
        let mut terminal = aterm_core::terminal::Terminal::new(120, 300);
        for n in 0..120 {
            terminal.process(format!("{n:03} {line}\r\n").as_bytes());
        }
        let mut session = crate::stub_session(id);
        session.term = std::sync::Arc::new(std::sync::Mutex::new(terminal));
        crate::App::register_session(&app.store, &session, None);
        app.store
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_state(id, crate::session_store::SessionState::Exited);
        app.pool.insert(session);
        app.next_session_id = app.next_session_id.max(id + 1);
    }
    let live = app.handoff_live_sessions();
    assert_eq!(live.len(), 1, "PRECONDITION: only session 0 is handed");

    let window = std::time::Duration::from_secs(120);
    let parked = app
        .capture_parked_screens(
            &live,
            std::time::Instant::now() + window,
            window.as_millis(),
            window / 2,
            crate::seamless::WireCaps::current(),
            aterm_update_core::handoff_policy::CarryCeiling::Full,
        )
        .expect("the park lands");
    assert_eq!(
        parked.held.len(),
        crate::seamless::MAX_HELD_PANES,
        "no pane past the carry's cap is projected inside the freeze"
    );
    assert!(
        app.capture_held_screens(
            &live,
            std::time::Instant::now(),
            &mut 0,
            crate::seamless::WireCaps::current(),
            aterm_update_core::handoff_policy::CarryCeiling::Full,
            &mut Vec::new(),
        )
        .is_empty(),
        "a held capture whose deadline has come projects no pane"
    );

    let now = std::time::Instant::now();
    let comfort = std::time::Duration::from_millis(200);
    let deadline = now + comfort;
    assert_eq!(
        super::held_capture_deadline(deadline, now, comfort),
        Some(deadline - comfort / 2),
        "the held capture stops a quarter of the budget short of the park's deadline"
    );
    assert_eq!(
        super::held_capture_deadline(deadline, now + std::time::Duration::from_nanos(1), comfort),
        None,
        "with less than half the budget left no held pane is captured"
    );

    app.pool.sessions.retain(|id, _| *id == 0);
    set_master(&mut app, 0, -1);
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

/// An open OSC 8 link or pending VT52 `ESC Y` address over queued output is carried to a current successor and misses on an older hop.
#[test]
fn an_open_link_or_a_vt52_address_over_queued_output_misses_only_an_older_hop() {
    let running = 200;
    let older = crate::seamless::WireCaps::for_hop(running, running - 1);
    assert!(!older.carries_output_state(), "PRECONDITION: an older hop");
    assert!(crate::seamless::WireCaps::for_hop(running, running).carries_output_state());
    let mut app = crate::App::headless_for_test();
    let (master, slave) = openpty();
    set_master(&mut app, 0, master);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let (prelaunch, ..) =
        super::late_park_record_tests::prelaunch_record(1, ApplyMode::AutomaticPastGrace);
    app.update_handoff_prelaunch = Some(prelaunch);
    for (at, parked, queued, reset, state) in [
        (
            "an open OSC 8 link",
            &b"\x1b]8;;https://example.com\x07src/ma"[..],
            &b"in.rs\x1b]8;;\x07\r\n"[..],
            &b"\x1b]8;;\x07"[..],
            "Osc8HyperlinkOpen",
        ),
        (
            "a VT52 ESC Y",
            &b"\x1b[?2l\x1bY"[..],
            &b"\x22\x24"[..],
            &b"\x22\x24\x1b<"[..],
            "Vt52CursorAddress",
        ),
    ] {
        crate::term_lock(&term).process(parked);
        write_all(slave, queued);
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "{at} over queued output is carried to a successor that continues it"
        );
        assert_eq!(
            capture_for(&mut app, older),
            Err(CaptureFailure::MidSequence { local_id: 0, state }),
            "{at} over queued output misses an older successor's park"
        );
        drain(master);
        assert_eq!(
            capture_for(&mut app, older),
            Ok(vec![0]),
            "{at}, stalled, is carried"
        );
        crate::term_lock(&term).process(reset);
    }
    crate::term_lock(&term).process(b"\x1b]8;;https://example.com\x07a\x1b]8;;\x07");
    write_all(slave, b"more output\r\n");
    assert_eq!(
        capture_for(&mut app, older),
        Ok(vec![0]),
        "a closed link over queued output is carried"
    );
    drain(master);
    app.update_handoff_prelaunch = None;

    set_master(&mut app, 0, -1);
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

/// On an older hop, an unclosed OSC 8 link is carried once free mid-sequence re-parks are exhausted (or on the fork lane), while a VT52 address still misses.
#[test]
fn an_open_link_past_the_free_reparks_is_carried_not_missed() {
    let running = 200;
    let older = crate::seamless::WireCaps::for_hop(running, running - 1);
    let mut app = crate::App::headless_for_test();
    let (master, slave) = openpty();
    set_master(&mut app, 0, master);
    let term = app.pool.get(0).expect("session 0").term.clone();
    crate::term_lock(&term).process(b"\x1b]8;;https://example.com/rg\x07src/ma");
    write_all(slave, b"compiling crate 1 of 400\r\n");
    assert_eq!(
        capture_for(&mut app, older),
        Ok(vec![0]),
        "the fork lane carries it"
    );
    let (mut prelaunch, ..) =
        super::late_park_record_tests::prelaunch_record(1, ApplyMode::AutomaticPastGrace);
    prelaunch.park_mid_sequence_reparks = super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS - 1;
    app.update_handoff_prelaunch = Some(prelaunch);
    assert_eq!(
        capture_for(&mut app, older),
        Err(CaptureFailure::MidSequence {
            local_id: 0,
            state: "Osc8HyperlinkOpen"
        }),
        "a free re-park is left: the close may be at the next boundary"
    );
    if let Some(prelaunch) = app.update_handoff_prelaunch.as_mut() {
        prelaunch.park_mid_sequence_reparks = super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS;
    }
    assert_eq!(
        capture_for(&mut app, older),
        Ok(vec![0]),
        "the free re-parks are spent: carried, not a miss the ladder charges"
    );
    crate::term_lock(&term).process(b"\x1b]8;;\x07\x1b[?2l\x1bY");
    write_all(slave, b"\x22\x24");
    assert_eq!(
        capture_for(&mut app, older),
        Err(CaptureFailure::MidSequence {
            local_id: 0,
            state: "Vt52CursorAddress"
        })
    );
    drain(master);
    app.update_handoff_prelaunch = None;

    set_master(&mut app, 0, -1);
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

fn calm_land(live: &[(u64, i32, i32)]) -> ParkGateFacts {
    ParkGateFacts {
        mode: ApplyMode::AutomaticPastGrace,
        phase: ApplyPhase::Land,
        activity: ActivityFacts {
            quiet: true,
            hands_off_keys: true,
            output_quiet: true,
            focused: true,
            consent_warmup: false,
            harness_restored_pending: false,
        },
        masters_quiet: !handoff_masters_have_activity(live),
        masters_alive: !handoff_masters_closed(live),
        land_waits: u8::MAX,
        land_gate_relaxed: false,
        held_for: std::time::Duration::ZERO,
        recording: false,
    }
}

/// An exited `--hold` pane is excluded from the handed set so its hung-up master does not block the park gate, capture, or manifest.
#[test]
fn an_exited_held_pane_is_not_handed_and_does_not_hold_the_park() {
    let mut app = crate::App::headless_for_test();
    app.hold = true;
    let (exited_master, exited_slave) = openpty();
    write_all(exited_slave, b"build finished\r\n");
    // Nonblocking close flushes unread output without waiting for macOS tty drain.
    aterm_pty::set_nonblocking(exited_slave, true).expect("nonblocking slave");
    aterm_pty::close_fd(exited_slave);
    let probe = [(1u64, exited_master, 0i32)];
    // Wait for the slave hang-up in case a concurrent test forked a child before close-on-exec.
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !handoff_masters_closed(&probe) {
        assert!(
            std::time::Instant::now() < deadline,
            "the exited slave hung up"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let held = crate::stub_session(1);
    held.child_reaped
        .store(true, std::sync::atomic::Ordering::Release);
    let mut held = held;
    held.master = exited_master;
    let _ = app.insert_logical_window(held, 24, 80);
    app.store
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .set_state(1, crate::session_store::SessionState::Exited);

    let whole: Vec<(u64, i32, i32)> = app
        .pool
        .iter()
        .map(|session| (session.id, session.master, session.pid))
        .collect();
    assert!(
        matches!(
            prelaunch_park_admitted(
                calm_land(&whole),
                prelaunch_hold_cap(ApplyMode::AutomaticPastGrace)
            ),
            ParkGate::Wait(_)
        ),
        "CONTROL: the whole pool, as both lanes handed it, holds the park even at Land \
         past every bound"
    );

    let live = app.handoff_live_sessions();
    assert_eq!(
        live.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(),
        vec![0],
        "the exited pane is not handed"
    );
    assert_eq!(
        prelaunch_park_admitted(
            calm_land(&live),
            prelaunch_hold_cap(ApplyMode::AutomaticPastGrace)
        ),
        ParkGate::Park,
        "and the handed set parks"
    );
    assert_eq!(
        capture(&mut app),
        Ok(vec![0]),
        "the capture carries the live set"
    );
    let manifest = handed_sessions_only(
        crate::session_store::SessionHandoff::from_store(
            &app.store
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ),
        &live,
    );
    assert!(
        manifest.sessions.iter().all(|record| record.local_id != 1),
        "the manifest names only handed sessions"
    );

    set_master(&mut app, 1, -1);
    drop(app);
    aterm_pty::close_fd(exited_master);
}

/// A `CarryCeiling::Repaint` session still carries its `fg_holder` in the manifest so the successor can restore terminal modes.
#[test]
fn a_repaint_rung_session_still_carries_its_foreground_holder() {
    let mut app = crate::App::headless_for_test();
    let session = app.pool.get(0).expect("session 0");
    *crate::term_lock(&session.term) = aterm_core::terminal::Terminal::new(24, 80);
    crate::term_lock(&session.term).process(b"$ vim notes.txt\r\n");
    session
        .fg_holder
        .store(4242, std::sync::atomic::Ordering::Release);

    let live = app.handoff_live_sessions();
    let window = crate::app_update_handoff::park_budget_for_test::deadline_budget(
        std::time::Duration::from_secs(30),
    );
    let parked = app
        .capture_parked_screens(
            &live,
            std::time::Instant::now() + window,
            window.as_millis(),
            window / 2,
            crate::seamless::WireCaps::current(),
            aterm_update_core::handoff_policy::CarryCeiling::Repaint,
        )
        .unwrap_or_else(|failure| panic!("the desk parks: {failure}"));
    assert_eq!(parked.repaint, vec![0], "carried blank");
    assert_eq!(parked.carries.len(), 1, "its ledger and rows still travel");
    assert!(
        !parked.carries[0].carries_differ(),
        "a blank carry goes without the differ's state"
    );
    assert_eq!(
        parked.fg_holders,
        vec![(0, 4242)],
        "but its foreground holder is read all the same"
    );

    let manifest = stamp_fg_holders(
        handed_sessions_only(
            crate::session_store::SessionHandoff::from_store(
                &app.store
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ),
            &live,
        ),
        &parked.fg_holders,
    );
    assert_eq!(manifest.sessions.len(), 1);
    assert_eq!(
        manifest.sessions[0].fg_holder,
        Some(4242),
        "and rides the manifest record"
    );
    let wire = manifest.to_toml().expect("serialize");
    let read = crate::session_store::SessionHandoff::from_toml(&wire).expect("parse the manifest");
    assert_eq!(read.sessions[0].fg_holder, Some(4242), "round trip");

    let none = stamp_fg_holders(manifest.clone(), &[(0, 0)]);
    assert_eq!(none.sessions[0].fg_holder, None);
    assert!(!none.to_toml().expect("serialize").contains("fg_holder"));
    let absent = stamp_fg_holders(manifest, &[(7, 4242)]);
    assert_eq!(absent.sessions[0].fg_holder, None, "another session's");
}

/// `stamp_journal_keys` attaches a crash-journal key only to its matching session record.
#[test]
fn a_journal_key_rides_the_record_it_names_and_no_other() {
    let app = crate::App::headless_for_test();
    assert!(app.journal_keys().is_empty(), "nothing taken over");
    let manifest = crate::session_store::SessionHandoff::from_store(
        &app.store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    assert!(!manifest.sessions.is_empty(), "PRECONDITION: a session");
    let none = stamp_journal_keys(manifest.clone(), &app.journal_keys());
    assert!(
        none.sessions
            .iter()
            .all(|record| record.journal_key.is_none())
    );
    assert!(!none.to_toml().expect("serialize").contains("journal_key"));
    let id = manifest.sessions[0].local_id;
    let keyed = stamp_journal_keys(manifest.clone(), &[(id, (4141, 7))]);
    assert_eq!(keyed.sessions[0].journal_key.as_deref(), Some("4141:7"));
    let other = stamp_journal_keys(manifest, &[(id + 1000, (4141, 7))]);
    assert_eq!(other.sessions[0].journal_key, None, "another session's");
}

/// `stamp_render_debts` records `render_debt` only for alternate-screen sessions with non-zero debt carried under `CarryCeiling::Full`.
#[test]
fn a_session_that_owes_rows_carries_its_render_debt_and_the_others_carry_none() {
    let window = crate::app_update_handoff::park_budget_for_test::deadline_budget(
        std::time::Duration::from_secs(30),
    );
    let capture = |app: &mut crate::App, ceiling| {
        let live = app.handoff_live_sessions();
        let parked = app
            .capture_parked_screens(
                &live,
                std::time::Instant::now() + window,
                window.as_millis(),
                window / 2,
                crate::seamless::WireCaps::current(),
                ceiling,
            )
            .unwrap_or_else(|failure| panic!("the desk parks: {failure}"));
        (live, parked)
    };
    let owing = || {
        let mut t = aterm_core::terminal::Terminal::new(12, 20);
        t.process(b"\x1b[?1049h\x1b[2J");
        for r in 0..12 {
            t.process(format!("\x1b[{};1HL{r}", r + 1).as_bytes());
        }
        t.process(b"\x1b[10;3H");
        t.resize(11, 20);
        t.process(b"\x1b[?1004h");
        t.resize(12, 20);
        assert_eq!(
            t.render_debt(),
            aterm_core::terminal::RenderDebt::of_rows(-1, 0),
            "the flap left a residue"
        );
        t
    };
    let mut app = crate::App::headless_for_test();
    *crate::term_lock(&app.pool.get(0).expect("session 0").term) = owing();
    let (live, parked) = capture(
        &mut app,
        aterm_update_core::handoff_policy::CarryCeiling::Full,
    );
    assert_eq!(
        parked.render_debts,
        vec![(0, aterm_core::terminal::RenderDebt::of_rows(-1, 0))]
    );
    let manifest = stamp_render_debts(
        handed_sessions_only(
            crate::session_store::SessionHandoff::from_store(
                &app.store
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ),
            &live,
        ),
        &parked.render_debts,
    );
    assert_eq!(manifest.sessions[0].render_debt.as_deref(), Some("-1,0"));
    let wire = manifest.to_toml().expect("serialize");
    assert!(wire.contains("render_debt = \"-1,0\""), "{wire}");
    let read = crate::session_store::SessionHandoff::from_toml(&wire).expect("parse");
    assert_eq!(
        read.sessions[0].render_debt.as_deref(),
        Some("-1,0"),
        "round trip"
    );

    let cut = aterm_core::terminal::RenderDebt {
        shift: -1,
        pushed: 0,
        cut: 2,
    };
    let wire = stamp_render_debts(manifest.clone(), &[(0, cut)])
        .to_toml()
        .expect("serialize");
    assert!(wire.contains("render_debt = \"-1,0\""), "{wire}");
    assert!(wire.contains("render_debt_cut = 2"), "{wire}");
    let read = crate::session_store::SessionHandoff::from_toml(&wire).expect("parse");
    assert_eq!(
        read.sessions[0].carried_render_debt(),
        Some(cut),
        "round trip"
    );

    *crate::term_lock(&app.pool.get(0).expect("session 0").term) = {
        let mut t = aterm_core::terminal::Terminal::new(12, 20);
        t.process(b"\x1b[?1049h\x1b[2J\x1b[3;1Hcomposer");
        t
    };
    let (_, parked) = capture(
        &mut app,
        aterm_update_core::handoff_policy::CarryCeiling::Full,
    );
    assert!(parked.render_debts.is_empty(), "{:?}", parked.render_debts);
    let none = stamp_render_debts(manifest.clone(), &parked.render_debts);
    assert_eq!(none.sessions[0].render_debt, None);
    assert!(!none.to_toml().expect("serialize").contains("render_debt"));

    *crate::term_lock(&app.pool.get(0).expect("session 0").term) = owing();
    let (_, parked) = capture(
        &mut app,
        aterm_update_core::handoff_policy::CarryCeiling::Repaint,
    );
    assert_eq!(parked.repaint, vec![0], "carried blank");
    assert!(parked.render_debts.is_empty(), "{:?}", parked.render_debts);

    *crate::term_lock(&app.pool.get(0).expect("session 0").term) = {
        let mut t = aterm_core::terminal::Terminal::new(12, 20);
        t.process(b"$ ls\r\n");
        t.resize(11, 20);
        t.resize(12, 20);
        t
    };
    let (_, parked) = capture(
        &mut app,
        aterm_update_core::handoff_policy::CarryCeiling::Full,
    );
    assert!(parked.render_debts.is_empty());
    let other = stamp_render_debts(
        manifest,
        &[(7, aterm_core::terminal::RenderDebt::of_rows(-1, 0))],
    );
    assert_eq!(other.sessions[0].render_debt, None);
}

/// Park capture exports each handed session's log sidecar for capable successors and fences local `log` reads until `rollback_overlap` lifts the fence.
#[test]
fn the_park_hands_its_logs_to_the_export_and_a_stand_down_lifts_the_fence() {
    let mut app = crate::App::headless_for_test();
    let ctx = std::sync::Arc::clone(&app.pool.get(0).expect("session 0").ctx);
    let written = |n: usize| {
        let mut tl = ctx.timeline.lock().unwrap();
        let now = tl.slog().now();
        for _ in 0..n {
            tl.slog_mut().human_input(true, now);
        }
        tl.slog().log().last_seq()
    };
    let fenced = written(5);
    let live = app.handoff_live_sessions();
    let window = crate::app_update_handoff::park_budget_for_test::deadline_budget(
        std::time::Duration::from_secs(30),
    );
    let parked = app
        .capture_parked_screens(
            &live,
            std::time::Instant::now() + window,
            window.as_millis(),
            window / 2,
            crate::seamless::WireCaps::current(),
            aterm_update_core::handoff_policy::CarryCeiling::Full,
        )
        .unwrap_or_else(|failure| panic!("the desk parks: {failure}"));
    assert_eq!(parked.carries.len(), live.len(), "every handed session");
    let ceiling = crate::control::turn_ids_for_handoff();
    let build = crate::running_build_number();
    for (_, bytes) in crate::handoff_carry::export_for_target(&parked.carries, ceiling, build - 1) {
        let carry = crate::handoff_carry::decode(&bytes).expect("a sidecar");
        assert!(carry.log.is_none(), "no member for an older successor");
    }
    let controls = crate::handoff_carry::export_for_target(&parked.carries, ceiling, build);
    assert_eq!(controls.len(), live.len());
    for (id, bytes) in &controls {
        let mut carry = crate::handoff_carry::decode(bytes).expect("a sidecar");
        let lines = carry
            .take_log()
            .unwrap_or_else(|| panic!("session {id}'s log rides its sidecar"));
        assert!(lines[0].starts_with("slog-carry v=1 "), "{}", lines[0]);
        assert!(
            lines[0].contains(&format!(" last={fenced} ")),
            "the log as the take found it: {}",
            lines[0]
        );
    }
    let after = written(3);
    assert!(after > fenced);
    let reply = crate::control::cmd_log(&ctx, "");
    assert!(
        reply.contains(&format!(" as_of={fenced} ")),
        "fenced until the update commits or stands down: {reply}"
    );
    app.rollback_overlap(None, &[]);
    let reply = crate::control::cmd_log(&ctx, "");
    assert!(
        reply.contains(&format!(" as_of={after} ")),
        "the stand-down lifted the fence: {reply}"
    );
}
