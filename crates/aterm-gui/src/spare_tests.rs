// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The warm spare's logic, headless (`docs/DESIGN-warm-spare-2026-10-16.md`
//! §13, tests U1–U14): the learned gate, adoption's gates, the booking and
//! promotion checks, the factory fingerprint, the reader's swallowing post,
//! taint, the cast's rebase, the spawner's adopt check and the backstop. Real
//! shells and the real `aterm-spare` thread are the live suite's
//! (`crates/aterm/tests/spare_live_headless.rs`); the shells' own holds are
//! `aterm-shell-integration`'s `tests_spare.rs`.

use super::*;
#[cfg(target_os = "macos")]
use crate::tab_spawn::SpawnAsk;
use std::sync::Mutex as StdMutex;

/// A unix second far from the epoch's edges.
const T: u64 = 1_800_000_000;

fn gate_file(dir: &aterm_tempfile::TempDir) -> PathBuf {
    dir.path().join("spare-gate-v1.tsv")
}

/// U1: three clean visible runs open a generation; an unclean one resets the
/// streak and an inconclusive one leaves it; a deterministic failure lasts until
/// the generation changes; a backoff failure holds an hour, then a day for a
/// second within a day; `T_READY` follows the longest run; the file round-trips.
#[test]
fn the_gate_opens_on_three_clean_runs_and_backs_off_on_failure() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let mut book = GateBook::at(Some(gate_file(&dir)));
    let (g, other) = (0xA1, 0xB2);
    assert!(!book.eligible_at(g, T));
    assert_eq!(book.report(g), "learning 0/3");
    for rc_ms in [40, 90] {
        book.sample(g, Sample::Clean { rc_ms });
    }
    book.sample(g, Sample::Inconclusive);
    assert!(
        !book.eligible_at(g, T),
        "two clean, one inconclusive: not yet"
    );
    book.sample(g, Sample::Unclean);
    assert_eq!(
        book.report(g),
        "learning 0/3",
        "an unclean run resets the streak"
    );
    for rc_ms in [40, 300, 60] {
        book.sample(g, Sample::Clean { rc_ms });
    }
    assert!(book.eligible_at(g, T), "three clean in a row");
    assert_eq!(book.report(g), "eligible");
    assert_eq!(
        book.t_ready(g),
        Duration::from_millis(1200),
        "4 × the longest run"
    );
    assert!(
        !book.eligible_at(other, T),
        "another generation learns on its own"
    );

    // A backoff failure: an hour; a second within a day: a day.
    book.fail_at(g, Verdict::Backoff("rc_timeout"), T);
    for _ in 0..3 {
        book.sample(g, Sample::Clean { rc_ms: 40 });
    }
    assert!(!book.eligible_at(g, T + 3599), "held for the hour");
    assert!(book.eligible_at(g, T + 3600));
    book.fail_at(g, Verdict::Backoff("gone"), T + 4000);
    for _ in 0..3 {
        book.sample(g, Sample::Clean { rc_ms: 40 });
    }
    assert!(
        !book.eligible_at(g, T + 4000 + 3600),
        "a second failure holds a day"
    );
    assert!(book.eligible_at(g, T + 4000 + 24 * 3600));

    // Deterministic: never again for this generation.
    book.fail_at(other, Verdict::Deterministic("sched"), T);
    for _ in 0..5 {
        book.sample(other, Sample::Clean { rc_ms: 10 });
    }
    assert!(!book.eligible_at(other, T + 365 * 24 * 3600));
    assert_eq!(book.report(other), "ineligible:sched");

    // The file: what a later process reads back.
    let mut back = GateBook::at(Some(gate_file(&dir)));
    assert!(back.eligible_at(g, T + 4000 + 24 * 3600));
    assert!(!back.eligible_at(other, T + 365 * 24 * 3600));
    assert_eq!(back.t_ready(g), Duration::from_millis(1200));
}

/// [`parked_slot`] for the conformance bind.
#[cfg(target_os = "macos")]
pub(super) fn parked_slot_for(app: &App, id: u64, cwd: Option<&str>) -> Slot {
    parked_slot(app, id, cwd)
}

/// A stub spare in window 0 of `app`, parked, in `cwd`, at the window's grid,
/// spawned by the app's own factory now.
fn parked_slot(app: &App, id: u64, cwd: Option<&str>) -> Slot {
    let wid = WindowId(0);
    let now = Instant::now();
    Slot {
        id,
        state: SlotState::Parked,
        link: Arc::new(SpareLink::new(id, wid, None)),
        session: Some(crate::stub_session(id)),
        cwd: cwd.map(str::to_owned),
        grid: app.spawn_grid(wid),
        window: wid,
        fingerprint: app.session_factory.spawn_fingerprint(),
        colors: app.engine_color_fanouts,
        booked: now,
        since: now,
        generation: 7,
        t_ready: Duration::from_secs(4),
        inputs: app.spare_gen_inputs(),
    }
}

/// An app with one tab whose focused pane reported `cwd` (OSC 7).
#[cfg(target_os = "macos")]
fn app_in(cwd: &str) -> App {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let s = crate::stub_session(app.next_session_id);
    crate::term_lock(&s.term).process(format!("\x1b]7;file://localhost{cwd}\x07").as_bytes());
    app.push_stub_tab(wid, s);
    app
}

/// [`app_in`] whose factory's shells can wait as spares (an integrated zsh;
/// the test App's own factory integrates nothing, and books none).
#[cfg(target_os = "macos")]
fn spare_app_in(cwd: &str) -> App {
    let mut app = app_in(cwd);
    app.session_factory.integrate = true;
    app.session_factory.shell_override = Some("/bin/zsh".to_owned());
    assert!(
        app.spare_factory_ok(),
        "an integrated zsh can wait as a spare"
    );
    app
}

/// U2: each of adoption's gates names its own miss, and a spare that passes
/// them all is taken out of the slot. Every refusal leaves the spare where it
/// was (the cold path runs instead).
#[cfg(target_os = "macos")]
#[test]
fn each_adoption_gate_names_its_miss_and_a_spare_that_passes_them_is_taken() {
    let wid = WindowId(0);
    let mut app = app_in("/tmp");
    let grid = app.spawn_grid(wid);
    let plain = SpawnAsk::new(None, None);
    let id = app.next_session_id;
    app.next_session_id += 1;
    let try_with = |app: &mut App, ask: &SpawnAsk, grid: Grid| {
        app.try_adopt_spare(wid, Some(wid), grid, ask).map(|_| ())
    };

    // G3: nothing to adopt, or one still booked.
    assert_eq!(try_with(&mut app, &plain, grid), Err("none"));
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));
    app.spares.slot.as_mut().unwrap().state = SlotState::Booked;
    assert_eq!(try_with(&mut app, &plain, grid), Err("booked"));
    app.spares.slot.as_mut().unwrap().state = SlotState::Parked;
    // G1: the kill switch.
    app.config.spare_shell = Some("off".into());
    assert_eq!(try_with(&mut app, &plain, grid), Err("off"));
    app.config.spare_shell = None;
    // G2: only a plain ask.
    for ask in [
        SpawnAsk::new(None, Some("agent")),
        SpawnAsk {
            identity_prep: crate::tab_spawn::IdentityPrep::Create,
            ..SpawnAsk::default()
        },
        SpawnAsk {
            observe: Some(aterm_session::SessionId::generate()),
            ..SpawnAsk::default()
        },
        SpawnAsk {
            theme: Some(crate::session_theme::SessionTheme {
                selector: "Dracula".into(),
                origin: crate::session_theme::SessionThemeOrigin::Spawn,
            }),
            ..SpawnAsk::default()
        },
    ] {
        assert_eq!(try_with(&mut app, &ask, grid), Err("ask"));
    }
    // G4: another factory.
    app.spares.slot.as_mut().unwrap().fingerprint ^= 1;
    assert_eq!(try_with(&mut app, &plain, grid), Err("factory"));
    app.spares.slot.as_mut().unwrap().fingerprint ^= 1;
    // G5: the folder, exactly.
    assert_eq!(
        try_with(&mut app, &SpawnAsk::new(Some("/private"), None), grid),
        Err("folder")
    );
    app.spares.slot.as_mut().unwrap().cwd = Some("/var".into());
    assert_eq!(try_with(&mut app, &plain, grid), Err("folder"));
    app.spares.slot.as_mut().unwrap().cwd = Some("/tmp".into());
    // G7: age.
    app.spares.slot.as_mut().unwrap().since = Instant::now().checked_sub(PARKED_MAX_AGE).unwrap();
    assert_eq!(try_with(&mut app, &plain, grid), Err("age"));
    app.spares.slot.as_mut().unwrap().since = Instant::now();
    // Taint: whatever the program asked of the host before anyone owned it.
    app.spares.slot.as_ref().unwrap().link.taint(taint::BELL);
    assert_eq!(try_with(&mut app, &plain, grid), Err("taint"));
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));
    // G9: its shell exited.
    app.spares.slot.as_ref().unwrap().link.decide(Wake::Exit {
        session: id,
        window: wid,
    });
    assert_eq!(try_with(&mut app, &plain, grid), Err("process"));
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));

    // WARM tiers: the colours, the grid, the generation and the gate too.
    app.spares.slot.as_mut().unwrap().state = SlotState::Ready;
    app.spares.shared.generation.store(7, Ordering::Release);
    app.spares.shared.eligible.store(true, Ordering::Release);
    app.spares.slot.as_mut().unwrap().colors ^= 1;
    assert_eq!(try_with(&mut app, &plain, grid), Err("colors"));
    app.spares.slot.as_mut().unwrap().colors ^= 1;
    {
        let slot = app.spares.slot.as_ref().unwrap();
        crate::term_lock(&slot.session.as_ref().unwrap().term).process(b"motd\r\n");
    }
    let other_grid = (grid.0 + 1, grid.1, grid.2);
    assert_eq!(try_with(&mut app, &plain, other_grid), Err("grid"));
    app.spares.shared.generation.store(8, Ordering::Release);
    assert_eq!(try_with(&mut app, &plain, grid), Err("stale"));
    app.spares.shared.generation.store(7, Ordering::Release);
    app.spares.shared.eligible.store(false, Ordering::Release);
    assert_eq!(try_with(&mut app, &plain, grid), Err("gate"));
    app.spares.shared.eligible.store(true, Ordering::Release);
    // G10: an update in flight.
    let (record, _transfer) = crate::app_update_handoff::dry_run_capture_tests::prelaunched(
        1,
        crate::native_updater_service::ApplyMode::Automatic,
        crate::app_update_handoff::FreezeSeed::Default,
    );
    app.update_handoff_prelaunch = Some(record);
    assert_eq!(try_with(&mut app, &plain, grid), Err("update"));
    app.update_handoff_prelaunch = None;
    assert!(app.spares.slot.is_some(), "every refusal leaves the spare");

    // Every gate passes: taken.
    let (slot, session) = app
        .try_adopt_spare(wid, Some(wid), grid, &plain)
        .expect("adopted");
    assert_eq!((slot.id, session.id), (id, id));
    assert!(app.spares.slot.is_none());
    // A parked spare lands at any grid (resized before its release).
    app.spares.slot = Some(parked_slot(&app, id + 1, Some("/tmp")));
    assert!(try_with(&mut app, &plain, other_grid).is_ok());
}

/// U3: a booking waits for a person, the boot grace, the settle after input,
/// the replacement spacing, the hourly budget and a pressure episode; a held
/// pane books nothing; a session not at its prompt waits for its output. A
/// promotion waits for idle input and a stable folder, and stops for the
/// budget, a pass, the power state and the gate.
#[cfg(target_os = "macos")]
#[test]
fn booking_and_promotion_wait_for_every_condition() {
    let mut app = spare_app_in("/tmp");
    let inputs = app.spare_gen_inputs();
    let now = Instant::now();
    // A person is required by default.
    assert_eq!(
        app.spare_book_check(now, &inputs),
        BookCheck::No("no-person")
    );
    app.config.spare_shell_needs_person = Some(false);
    // The boot grace.
    app.spares.boot_at = Some(now);
    assert_eq!(
        app.spare_book_check(now, &inputs),
        BookCheck::Wait(now + BOOT_GRACE)
    );
    let later = now + BOOT_GRACE;
    // A person typed: the settle after it.
    app.spares.person.set(Some(later));
    assert_eq!(
        app.spare_book_check(later, &inputs),
        BookCheck::Wait(later + SETTLE)
    );
    let t = later + SETTLE;
    // The focused session has not drawn its prompt: its output looks again.
    let focused = app.focused_session_id(WindowId(0));
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::NotAtPrompt(focused)
    );
    let s = app.pool.get(focused.unwrap()).unwrap();
    crate::term_lock(&s.term).process(b"\x1b]133;A\x07");
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::Go {
            window: WindowId(0),
            cwd: Some("/tmp".into())
        }
    );
    // The replacement spacing.
    app.spares.last_replace = Some(t);
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::Wait(t + REPLACE_MIN)
    );
    app.spares.last_replace = None;
    // The hourly budget.
    for i in 0..PARKED_BUDGET {
        app.spares
            .parked_spawns
            .push_back(t - Duration::from_secs(60) + Duration::from_millis(i as u64));
    }
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::Wait(t - Duration::from_secs(60) + HOUR)
    );
    app.spares.parked_spawns.clear();
    // A pressure episode.
    app.spares.pressure_at = Some(t);
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::Wait(t + PRESSURE_HOLD)
    );
    app.spares.pressure_at = None;
    // A parked spare that ran past its hold: none until the script set or shell changes.
    app.spares.parked_failed = Some(parked_key(&inputs));
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::No("parked-failed")
    );
    app.spares.parked_failed = None;
    // `-e`, and temporal recording: never.
    app.session_factory.temporal_recording = true;
    assert_eq!(app.spare_book_check(t, &inputs), BookCheck::No("factory"));
    app.session_factory.temporal_recording = false;

    // Promotion.
    let id = app.next_session_id;
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));
    app.spares.slot.as_mut().unwrap().since = t;
    app.spares.wanted = Some((WindowId(0), Some("/tmp".into()), t));
    app.spares.power = || false;
    assert_eq!(app.spare_promote_check(t), BookCheck::No("gate"));
    app.spares.shared.generation.store(9, Ordering::Release);
    app.spares.shared.eligible.store(true, Ordering::Release);
    assert_eq!(
        app.spare_promote_check(t),
        BookCheck::Wait(t + PROMOTE_FOLDER_STABLE),
        "the folder holds 3 s, the input settles 1 s"
    );
    let u = t + PROMOTE_FOLDER_STABLE;
    assert!(matches!(app.spare_promote_check(u), BookCheck::Go { .. }));
    for _ in 0..WARM_BUDGET {
        app.spares.warm_discards.push_back(u);
    }
    assert_eq!(app.spare_promote_check(u), BookCheck::No("budget"));
    app.spares.warm_discards.clear();
    app.spares.pass_active = true;
    assert_eq!(app.spare_promote_check(u), BookCheck::No("pass"));
    app.spares.pass_active = false;
    app.spares.power = || true;
    assert_eq!(app.spare_promote_check(u), BookCheck::No("power"));
    app.spares.power = || false;
    app.config.spare_shell = Some("parked".into());
    assert_eq!(app.spare_promote_check(u), BookCheck::No("mode"));
}

/// U3b: a person seen once is enough for a PARKED spare, through an absence
/// longer than the person's window (the first Cmd-T back adopts it); WARM
/// waits for a person now.
#[cfg(target_os = "macos")]
#[test]
fn a_parked_spare_waits_out_an_absence_and_only_warm_needs_a_person_now() {
    let mut app = spare_app_in("/tmp");
    let inputs = app.spare_gen_inputs();
    let seen = Instant::now();
    app.spares.boot_at = Some(seen);
    let focused = app.focused_session_id(WindowId(0)).expect("a tab");
    let s = app.pool.get(focused).unwrap();
    crate::term_lock(&s.term).process(b"\x1b]133;A\x07");
    app.spares.note_person(seen);
    // Eleven minutes without a gesture.
    let back = seen + PERSON_WINDOW + Duration::from_secs(60);
    assert_eq!(
        app.spare_book_check(back, &inputs),
        BookCheck::Go {
            window: WindowId(0),
            cwd: Some("/tmp".into())
        },
        "a parked spare is booked after the absence"
    );
    // The parked spare, its folder settled and the gate open: not promoted
    // while nobody is here.
    let id = app.next_session_id;
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));
    app.spares.slot.as_mut().unwrap().since = seen;
    app.spares.wanted = Some((WindowId(0), Some("/tmp".into()), seen));
    app.spares.power = || false;
    app.spares.shared.generation.store(9, Ordering::Release);
    app.spares.shared.eligible.store(true, Ordering::Release);
    assert_eq!(app.spare_promote_check(back), BookCheck::No("no-person"));
    // The person is back: WARM once their input settles.
    app.spares.note_person(back);
    assert_eq!(
        app.spare_promote_check(back),
        BookCheck::Wait(back + PROMOTE_IDLE)
    );
    assert!(matches!(
        app.spare_promote_check(back + PROMOTE_IDLE),
        BookCheck::Go { .. }
    ));
}

/// U3c: a window coming to the front is a person arriving, as a gesture is.
#[cfg(target_os = "macos")]
#[test]
fn a_window_coming_to_the_front_is_a_person() {
    let mut app = app_in("/tmp");
    assert_eq!(app.spares.person.get(), None, "nobody yet");
    app.window_focus_event(WindowId(0), false);
    assert_eq!(
        app.spares.person.get(),
        None,
        "a window leaving the front is not"
    );
    let before = Instant::now();
    app.window_focus_event(WindowId(0), true);
    assert!(
        app.spares.person.get().is_some_and(|at| at >= before),
        "the focus stamped a person"
    );
}

/// The FIFO sweep: a FIFO whose owner is gone (a reaped pid, or a pid now
/// another process's) is removed with its link; a live owner's (this
/// process's), one whose owner could not read its own birth (`0`) while that
/// pid lives, a name that names no owner, and an owner-named entry that is no
/// FIFO are left. The release of a shell blocked on it is live L12's.
#[cfg(target_os = "macos")]
#[test]
fn the_fifo_sweep_removes_only_a_dead_owners_fifo_and_its_link() {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let at = |name: &str| dir.path().join(name);
    let mkfifo = |name: &str| {
        let c = std::ffi::CString::new(at(name).as_os_str().as_bytes()).expect("path");
        // SAFETY: `c` is a NUL-terminated path that lives across the call.
        assert_eq!(
            unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
            0,
            "mkfifo {name}"
        );
    };
    let link = |to: &str, sid: &str| std::os::unix::fs::symlink(to, at(sid)).expect("link");
    // A process this test started and read, then killed and reaped.
    let mut gone = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .spawn()
        .expect("sleep");
    let gone_pid = i32::try_from(gone.id()).expect("pid");
    let gone_birth = aterm_pty::process_birth(gone_pid).expect("its birth");
    gone.kill().expect("kill");
    gone.wait().expect("reap");
    let me = i32::try_from(std::process::id()).expect("pid");
    let my_birth = aterm_pty::process_birth(me).expect("this process's birth");

    let dead = format!("{gone_pid}-{gone_birth}-s-0a");
    mkfifo(&dead);
    link(&dead, "s-0a");
    let reused = format!("{me}-{}-s-0b", my_birth + 1);
    mkfifo(&reused);
    let live = format!("{me}-{my_birth}-s-0c");
    mkfifo(&live);
    link(&live, "s-0c");
    mkfifo("s-0d");
    let not_fifo = format!("{gone_pid}-{gone_birth}-s-0e");
    std::fs::write(at(&not_fifo), b"").expect("file");
    // A birth unread at the naming proves nothing while its pid lives.
    let unread = format!("{me}-0-s-0f");
    mkfifo(&unread);
    mkfifo(&format!("{gone_pid}-0-s-10"));

    sweep_dead_fifos(dir.path());
    let mut left: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    let mut kept = vec![live, "s-0c".to_owned(), "s-0d".to_owned(), not_fifo, unread];
    kept.sort();
    assert_eq!(left, kept);
}

/// U5: the factory fingerprint moves with every field that shapes a shell, and
/// not with the colours.
#[test]
fn the_spawn_fingerprint_moves_with_what_shapes_a_shell_and_not_with_colours() {
    let app = App::headless_for_test();
    let base = app.session_factory.clone();
    let fp = base.spawn_fingerprint();
    type Change = Box<dyn Fn(&mut crate::spawn::SessionFactory)>;
    let moved: [(&str, Change); 8] = [
        (
            "env_add",
            Box::new(|f| f.env_add.push(("X".into(), "1".into()))),
        ),
        (
            "exec_command",
            Box::new(|f| f.exec_command = Some(vec!["vi".into()])),
        ),
        (
            "shell_override",
            Box::new(|f| f.shell_override = Some("/bin/sh".into())),
        ),
        (
            "shell_args",
            Box::new(|f| f.shell_args = Some(vec!["-l".into()])),
        ),
        ("cwd", Box::new(|f| f.cwd = Some("/tmp".into()))),
        (
            "sandbox_wrap",
            Box::new(|f| f.sandbox_wrap = Some("(deny)".into())),
        ),
        ("integrate", Box::new(|f| f.integrate = !f.integrate)),
        (
            "temporal_recording",
            Box::new(|f| f.temporal_recording = !f.temporal_recording),
        ),
    ];
    for (field, change) in moved {
        let mut f = base.clone();
        change(&mut f);
        assert_ne!(f.spawn_fingerprint(), fp, "{field} must move it");
    }
    let mut colours = base.clone();
    colours.terminal_config = None;
    assert_eq!(colours.spawn_fingerprint(), fp, "colours do not move it");
}

/// A link whose posts are captured.
fn captured_link(id: u64) -> (Arc<SpareLink>, Arc<StdMutex<Vec<String>>>) {
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let post: crate::spawn::WakePost = Arc::new(move |wake| {
        sink.lock().unwrap().push(format!("{wake:?}"));
        true
    });
    (Arc::new(SpareLink::new(id, WindowId(0), Some(post))), seen)
}

#[cfg(target_os = "macos")]
pub(super) fn a_start_seen_of(kind: StartKind) -> StartSeen {
    a_start_seen(kind)
}

fn a_start_seen(kind: StartKind) -> StartSeen {
    StartSeen {
        at: Instant::now(),
        mark: StartMark {
            kind,
            rc_dir: Some("/h".into()),
            flags: Some(aterm_core::terminal::StartFlags::default()),
        },
        input: None,
        fg_pgrp: -1,
        image: None,
        alt_screen: false,
    }
}

/// U6: before its adoption a spare's reader posts nothing the loop acts on —
/// an output wake answers `false` (the latch clears) after noting a parked or a
/// start mark once each, an exit becomes a note and never a `Wake::Exit` — and
/// once live every wake goes on, stamped with its owner's window.
#[test]
fn a_spare_reader_is_swallowed_until_live_then_forwarded_to_its_window() {
    let (link, seen) = captured_link(5);
    let output = || Wake::Output {
        session: 5,
        window: WindowId(0),
    };
    assert!(
        !link.decide(output()),
        "an output wake is refused: the latch clears"
    );
    assert!(seen.lock().unwrap().is_empty(), "nothing to note yet");
    let _ = link.witness.parked_seen.set(Instant::now());
    assert!(!link.decide(output()));
    assert!(!link.decide(output()));
    let _ = link.witness.start_seen.set(a_start_seen(StartKind::Spare));
    assert!(!link.decide(output()));
    assert!(link.decide(Wake::Ready {
        session: 5,
        window: WindowId(0),
    }));
    assert!(link.reader_started());
    link.decide(Wake::Exit {
        session: 5,
        window: WindowId(0),
    });
    assert!(link.exited());
    let notes = seen.lock().unwrap().clone();
    assert_eq!(
        notes,
        vec![
            "SpareNote { id: 5, note: Parked }".to_string(),
            "SpareNote { id: 5, note: AtStart }".to_string(),
            "SpareNote { id: 5, note: Gone }".to_string(),
        ],
        "each note once, and no Exit reaches the loop"
    );

    // Live in window 3: forwarded, re-stamped.
    let (link, seen) = captured_link(6);
    link.go_live(WindowId(3));
    assert!(link.decide(Wake::Output {
        session: 6,
        window: WindowId(0),
    }));
    link.decide(Wake::Exit {
        session: 6,
        window: WindowId(0),
    });
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![
            "Output { session: 6, window: WindowId(3) }".to_string(),
            "Exit { session: 6, window: WindowId(3) }".to_string(),
        ]
    );
}

/// U6's latch half: the documented headless path — a refused output post
/// clears the session's latch, so the reader's next chunk posts again, and no
/// repost is counted.
#[test]
fn a_refused_spare_output_post_clears_the_latch() {
    let (link, _) = captured_link(9);
    let latch = AtomicU64::new(0);
    let gate = crate::spawn::gated_output_wake_for_test(&latch, 1_000, || {
        link.decide(Wake::Output {
            session: 9,
            window: WindowId(0),
        })
    });
    assert_eq!(latch.load(Ordering::Relaxed), 0, "cleared, not stranded");
    assert_eq!(
        gate,
        crate::spawn::WakeGate::Coalesced,
        "a refused post counts as nothing posted, never a repost"
    );
}

/// U8: before its adoption a spare's host effects reach nothing and taint it:
/// the bell, a notification, an OSC 52 write; and an OSC 52 query is refused
/// (revoked), so the pasteboard is never read.
#[test]
fn a_spare_host_effect_only_taints_it() {
    let term = Arc::new(std::sync::Mutex::new(crate::spawn::new_live_terminal(
        24,
        80,
        Some(&aterm_core::config::TerminalConfig {
            allow_notifications: true,
            allow_osc52_query: true,
            ..aterm_core::config::TerminalConfig::default()
        }),
        None,
    )));
    let (link, seen) = captured_link(4);
    wire_spare_taint(&term, &link);
    {
        let mut t = crate::term_lock(&term);
        assert!(!t.modes().allow_osc52_query, "the query is revoked");
        t.process(b"\x07");
    }
    assert_eq!(link.tainted(), taint::BELL);
    crate::term_lock(&term).process(b"\x1b]9;hello\x07");
    assert_ne!(link.tainted() & taint::NOTIFY, 0, "a notification taints");
    crate::term_lock(&term)
        .authorize_clipboard_access(aterm_core::terminal::ClipboardAccess::Write);
    crate::term_lock(&term).process(b"\x1b]52;c;aGk=\x07");
    assert_ne!(
        link.tainted() & taint::CLIPBOARD,
        0,
        "a clipboard write taints"
    );
    assert!(seen.lock().unwrap().is_empty(), "nothing was delivered");
}

/// U9: an adopted spare's recording reads as a tab started at the adoption —
/// what it recorded before moves to t = 0, later events are offset by its age,
/// and order is kept.
#[test]
fn an_adopted_spares_cast_starts_at_the_adoption() {
    let mut rec = crate::cast::CastRecorder::new(80, 24);
    rec.record_output(Duration::from_millis(100), b"motd");
    rec.record_output(Duration::from_millis(200), b"rc");
    let at = rec.epoch() + Duration::from_millis(5_000);
    rec.adopt_spare(at);
    rec.record_output(Duration::from_millis(5_250), b"prompt");
    let cast = rec.to_asciicast();
    let lines: Vec<&str> = cast.lines().skip(1).collect();
    assert_eq!(lines.len(), 3, "{cast}");
    assert!(
        lines[0].starts_with("[0.000000, \"o\", \"motd\"]"),
        "{cast}"
    );
    assert!(lines[1].starts_with("[0.000000, \"o\", \"rc\"]"), "{cast}");
    assert!(
        lines[2].starts_with("[0.250000, \"o\", \"prompt\"]"),
        "{cast}"
    );
}

/// U11: the spawner hands a spare over only while its rc generation, its folder
/// and its shell still hold.
#[test]
fn the_spawner_hands_a_spare_over_only_while_it_is_still_valid() {
    let home = aterm_tempfile::tempdir().expect("tempdir");
    std::fs::write(home.path().join(".zshrc"), "PS1='> '\n").unwrap();
    let inputs = GenInputs {
        shell: aterm_core::shell_integration::ShellType::Zsh,
        shell_path: PathBuf::from("/bin/zsh"),
        home: home.path().to_path_buf(),
        launch_zdotdir: home.path().to_path_buf(),
        fingerprint: 1,
        containment: 2,
    };
    let generation = rc_generation(&inputs, None);
    let link = Arc::new(SpareLink::new(3, WindowId(0), None));
    let session = crate::stub_session(3);
    let check = |cwd: Option<&str>, generation: Option<u64>| AdoptCheck {
        cwd: cwd.map(str::to_owned),
        generation: generation.map(|g| (inputs.clone(), g)),
        link: Arc::clone(&link),
        kill: false,
    };
    assert_eq!(check(None, Some(generation)).miss(&session), None);
    assert_eq!(check(Some("/tmp"), None).miss(&session), None);
    // The rc moved: stale.
    std::fs::write(home.path().join(".zshrc"), "PS1='$ '\nalias x=y\n").unwrap();
    assert_eq!(check(None, Some(generation)).miss(&session), Some("stale"));
    // Its folder would fall back now.
    assert_eq!(
        check(Some("/nonexistent/aterm-spare"), None).miss(&session),
        Some("folder")
    );
    // Its shell is gone.
    link.decide(Wake::Exit {
        session: 3,
        window: WindowId(0),
    });
    assert_eq!(check(None, None).miss(&session), Some("process"));
}

/// U14: the backstop swallows exactly the wakes about a spare nobody owns —
/// the slot's, one in transit, a retired one's — and an output wake clears its
/// latch; a request that names a session, and any other session's wake, pass.
#[test]
fn the_backstop_swallows_only_a_spares_own_wakes() {
    let mut app = App::headless_for_test();
    let id = 40;
    app.spares.slot = Some(parked_slot(&app, id, None));
    for wake in [
        Wake::Output {
            session: id,
            window: WindowId(0),
        },
        Wake::Exit {
            session: id,
            window: WindowId(0),
        },
        Wake::Bell {
            session: id,
            window: WindowId(0),
        },
        Wake::Ready {
            session: id,
            window: WindowId(0),
        },
        Wake::InputWatch { session: id },
    ] {
        assert_eq!(wake.local_session(), Some(id), "{wake:?}");
        assert!(app.spares.swallows(id));
    }
    // Another session's exit passes.
    assert!(!app.spares.swallows(id + 1));
    // A request that names the spare's id is answered by its handler.
    let (tx, _rx) = std::sync::mpsc::channel();
    assert_eq!(
        Wake::CloseSession {
            session: id,
            by: crate::session_store::ExitActor::Human,
            reply: tx,
        }
        .local_session(),
        None
    );
    // The latch of a swallowed output wake is cleared.
    app.spares
        .slot
        .as_ref()
        .unwrap()
        .session
        .as_ref()
        .unwrap()
        .output_wake_pending
        .store(77, Ordering::Relaxed);
    app.on_spare_backstop(&Wake::Output {
        session: id,
        window: WindowId(0),
    });
    assert_eq!(
        app.spares
            .slot
            .as_ref()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .output_wake_pending
            .load(Ordering::Relaxed),
        0
    );
    // Retired: its late wakes are still swallowed.
    app.retire_spare("test", None);
    assert!(app.spares.slot.is_none());
    assert!(app.spares.swallows(id), "a tombstone");
}

/// The escape scan on an owned process tree: the fixture keeps the daemon
/// parented until its exact pid, birth and parent chain are captured, then
/// releases that parent and waits for launchd to adopt it. Cleanup never
/// searches the host for a process name.
#[cfg(target_os = "macos")]
#[test]
fn the_escape_scan_catches_a_daemon_the_session_left_behind() {
    if aterm_uds::deadline::trust_service_silent() {
        aterm_spec::host::could_not_run("the copied fixture binaries could not be signed");
    }
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let shell = dir.path().join("sh");
    let sleep = dir.path().join("sleep");
    for (from, to) in [("/bin/sh", &shell), ("/bin/sleep", &sleep)] {
        std::fs::copy(from, to).expect("copy the owned stand-in");
        aterm_uds::deadline::sign_ad_hoc(to);
    }
    let mark = dir.path().join("daemon-pid");
    let release = dir.path().join("release-parent");
    let child = aterm_uds::spawnfd::command(&shell)
        .arg("-c")
        .arg(
            r#"( "$1" 600 & printf '%s\n' "$!" > "$2";
                 while [ ! -f "$3" ]; do "$1" 0.01; done ) &
                wait
                exec "$1" 600"#,
        )
        .arg("escape-fixture")
        .arg(&sleep)
        .arg(&mark)
        .arg(&release)
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .env("ATERM_PARENT_SESSION_ID", "s-0123456789abcdef0123")
        .current_dir(dir.path())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn");
    // Own the unreaped child's fresh group before any readiness wait or
    // assertion. If capture fails, its still-parented descendants cannot escape.
    let mut owned = OwnedEscapeFixture {
        child: Some(child),
        daemon: None,
    };
    let shell_pid = owned.child.as_ref().unwrap().id() as i32;
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let daemon_pid = loop {
        if let Ok(line) = std::fs::read_to_string(&mark)
            && line.ends_with('\n')
            && let Ok(pid) = line.trim().parse::<i32>()
        {
            break pid;
        }
        assert!(
            Instant::now() < give_up,
            "the fixture never named its daemon"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let daemon = bsdinfo_uniq(daemon_pid).expect("the named fixture process");
    let parent = bsdinfo_uniq(daemon.pbsd.pbi_ppid as i32).expect("its held parent");
    assert_eq!(parent.pbsd.pbi_ppid, shell_pid as u32, "our child's child");
    owned.daemon = Some((daemon_pid, daemon.p_uniqueid));
    // Only a captured, still-parented daemon may be released to launchd.
    std::fs::write(&release, b"release\n").expect("release its parent");
    loop {
        let info = bsdinfo_uniq(daemon_pid).expect("the owned daemon is still live");
        assert_eq!(info.p_uniqueid, daemon.p_uniqueid, "the captured birth");
        if info.pbsd.pbi_ppid == 1 {
            break;
        }
        assert!(
            Instant::now() < give_up,
            "launchd never adopted the fixture"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let born = std::time::UNIX_EPOCH
        + Duration::from_secs(daemon.pbsd.pbi_start_tvsec)
        + Duration::from_micros(daemon.pbsd.pbi_start_tvusec);
    let caught = escape_scan(shell_pid, "s-0123456789abcdef0123", born, born);
    // The window before this tree is judged over the fixture's own live
    // processes: over the whole uid, any other process exiting while it is
    // listed (another test's, under a full suite) reads as an incomplete
    // scan, `Unknown`, not `Clean`.
    let window_before = escape_scan_among(
        shell_pid,
        "s-0123456789abcdef0123",
        std::time::UNIX_EPOCH,
        std::time::UNIX_EPOCH,
        |_| Some(vec![shell_pid, daemon_pid]),
        bsdinfo_uniq,
    );
    // Negative control: the same fixture in the run's window is caught.
    let caught_among = escape_scan_among(
        shell_pid,
        "s-0123456789abcdef0123",
        born,
        born,
        |_| Some(vec![shell_pid, daemon_pid]),
        bsdinfo_uniq,
    );
    owned
        .retire_owned_escape_fixture()
        .expect("reap the fixture child");
    let gone_by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while bsdinfo_uniq(daemon_pid).is_some_and(|i| i.p_uniqueid == daemon.p_uniqueid) {
        assert!(
            Instant::now() < gone_by,
            "the owned daemon was not collected"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(caught, Scan::Escaped);
    assert_eq!(caught_among, Scan::Escaped);
    assert_eq!(
        window_before,
        Scan::Clean,
        "a window before this process tree"
    );
}

/// Only a positive zombie state establishes completed exit for this scan.
/// An empty response and the flag that merely begins exit remain uncertain.
/// These are bytes the real process-table reader passes to its decoder.
#[cfg(target_os = "macos")]
#[test]
fn only_positive_zombie_state_establishes_a_completed_exit() {
    let record = |state: u8, flags: i32| {
        let mut bytes = [0u8; 37];
        bytes[32..36].copy_from_slice(&flags.to_ne_bytes());
        bytes[36] = state;
        bytes
    };
    let zombie = record(libc::SZOMB as u8, 0);
    let exiting_zombie = record(libc::SZOMB as u8, 0x2000);
    let live = record(2, 0);
    assert!(exited_observation(0, &zombie, zombie.len()), "zombie");
    assert!(
        exited_observation(0, &exiting_zombie, exiting_zombie.len()),
        "the observed zombie state decides, not its exit flag"
    );
    assert!(!exited_observation(-1, &[], 0), "failed empty query");
    assert!(
        !exited_observation(-1, &zombie, zombie.len()),
        "failed query"
    );
    assert!(!exited_observation(0, &live, live.len()), "live process");
    assert!(!exited_observation(0, &zombie, 36), "short response");
    assert!(
        !exited_observation(0, &zombie, 38),
        "invalid reported length"
    );
    let exiting = record(2, 0x2000);
    let uncertain = [
        exited_observation(0, &[], 0),
        exited_observation(0, &exiting, exiting.len()),
    ];
    assert_eq!(
        uncertain,
        [false, false],
        "empty output and beginning exit do not establish released resources"
    );
}

/// An incomplete scan is taken again, up to [`SCAN_ATTEMPTS`] listings: the
/// first verdict that is not `Unknown` stands, an escape included, and no
/// scan follows it. NEGATIVE CONTROL: one attempt reports the same
/// incomplete scan as `Unknown`, and a host that never completes one stays
/// `Unknown` after every attempt.
#[cfg(target_os = "macos")]
#[test]
fn an_incomplete_escape_scan_is_taken_again() {
    let run = |attempts: usize, verdicts: &[Scan]| {
        let mut taken = 0;
        let verdict = rescan_while_unknown(attempts, || {
            taken += 1;
            verdicts[taken - 1]
        });
        (verdict, taken)
    };
    assert_eq!(
        run(SCAN_ATTEMPTS, &[Scan::Unknown, Scan::Clean, Scan::Escaped]),
        (Scan::Clean, 2)
    );
    assert_eq!(
        run(SCAN_ATTEMPTS, &[Scan::Escaped, Scan::Clean, Scan::Clean]),
        (Scan::Escaped, 1)
    );
    assert_eq!(
        run(
            SCAN_ATTEMPTS,
            &[Scan::Unknown, Scan::Unknown, Scan::Escaped]
        ),
        (Scan::Escaped, 3)
    );
    assert_eq!(run(1, &[Scan::Unknown, Scan::Clean]), (Scan::Unknown, 1));
    assert_eq!(
        run(SCAN_ATTEMPTS, &[Scan::Unknown; SCAN_ATTEMPTS]),
        (Scan::Unknown, SCAN_ATTEMPTS)
    );
}

/// A pid the scan cannot read that the process table says was born before the
/// shell is none of the run's, so the first listing settles it: a host with a
/// process wedged mid-exit otherwise left every scan `Unknown`. NEGATIVE
/// CONTROL: `a_pid_reaped_mid_scan_is_settled_by_a_later_listing`'s reaped pid
/// has no record, and stays unsettled.
#[cfg(target_os = "macos")]
#[test]
fn an_unreadable_process_older_than_the_shell_is_none_of_the_runs() {
    let before_shell = std::time::SystemTime::now();
    let mut shell = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn an owned stand-in shell");
    let shell_pid = shell.id() as i32;
    let me = std::process::id() as i32;
    let older = born_before(me, before_shell);
    let younger = born_before(shell_pid, before_shell);
    let mut listings = 0;
    let verdict = escape_scan_among(
        shell_pid,
        "s-0123456789abcdef0123",
        std::time::UNIX_EPOCH,
        std::time::UNIX_EPOCH,
        |_| {
            listings += 1;
            Some(vec![shell_pid, me])
        },
        |p| if p == me { None } else { bsdinfo_uniq(p) },
    );
    let _ = shell.kill();
    let _ = shell.wait();
    assert!(older, "this test's process was born before the instant");
    assert!(
        !younger,
        "a process spawned after the instant was not born before it"
    );
    assert_eq!(
        (verdict, listings),
        (Scan::Clean, 1),
        "an unreadable process older than the shell settles in the first listing"
    );
}

/// A pid the first listing named that reads as nothing by the time it is read
/// (a busy host's process reaped mid-scan) is settled by a later listing that
/// no longer names it: a scan of a live shell stays clean, and the reaped pid
/// takes one later listing, not a whole new scan with the host's fresh churn.
/// NEGATIVE CONTROL: a later listing that still names the unreadable pid leaves
/// the scan incomplete (`Unknown`) after every settling listing, and a scan
/// whose later listing fails is `Unknown`.
#[cfg(target_os = "macos")]
#[test]
fn a_pid_reaped_mid_scan_is_settled_by_a_later_listing() {
    struct Owned(aterm_uds::spawnfd::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let spawn = |program: &str, args: &[&str]| {
        aterm_uds::spawnfd::command(program)
            .args(args)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn an owned stand-in")
    };
    let shell = Owned(spawn("/bin/sleep", &["600"]));
    let shell_pid = shell.0.id() as i32;
    let mut done = spawn("/usr/bin/true", &[]);
    done.wait().expect("reap the owned child");
    let reaped = done.id() as i32;
    assert!(
        bsdinfo_uniq(reaped).is_none() && !exited(reaped),
        "a reaped pid reads as nothing and is not a zombie"
    );
    let sid = "s-0123456789abcdef0123";
    let scan = |later: Option<Vec<i32>>| {
        let mut listings = 0;
        let verdict = escape_scan_among(
            shell_pid,
            sid,
            std::time::UNIX_EPOCH,
            std::time::UNIX_EPOCH,
            |_| {
                listings += 1;
                if listings == 1 {
                    Some(vec![shell_pid, reaped])
                } else {
                    later.clone()
                }
            },
            bsdinfo_uniq,
        );
        (verdict, listings)
    };
    assert_eq!(
        scan(Some(vec![shell_pid])),
        (Scan::Clean, 2),
        "a later listing that no longer names the reaped pid settles it"
    );
    assert_eq!(
        scan(Some(vec![shell_pid, reaped])),
        (Scan::Unknown, 1 + SCAN_SETTLE_LISTINGS),
        "a pid every later listing still names and nothing can read stays unsettled"
    );
    assert_eq!(
        scan(None),
        (Scan::Unknown, 2),
        "a failed later listing settles nothing"
    );
    let mut whole = 0;
    assert_eq!(
        escape_scan_among(
            shell_pid,
            sid,
            std::time::UNIX_EPOCH,
            std::time::UNIX_EPOCH,
            |_| {
                whole += 1;
                Some(vec![shell_pid])
            },
            bsdinfo_uniq,
        ),
        Scan::Clean,
        "a listing every pid of which reads takes no later one"
    );
    assert_eq!(whole, 1);
    drop(shell);
}

/// A daemon read while its launcher was still exiting names that launcher as
/// its parent, though XNU hands an exiting process's children to launchd
/// before it is a zombie. The scan reads every record that names a parent it
/// dropped again, so the escape stands: a launcher that is a zombie at its
/// read, and one reaped since the listing that a later listing settles. The
/// processes are real; only the race (the launcher in `P_WEXIT` between the
/// daemon's read and its own) is not left to the kernel's timing: the scan's
/// first read of the daemon is the daemon's real record from before its
/// launcher was killed. NEGATIVE CONTROL: without the second read both scans
/// are `Clean`; a stale daemon that cannot be read again leaves the scan
/// `Unknown`, never `Clean`; the same tree read live is `Escaped`.
#[cfg(target_os = "macos")]
#[test]
fn a_daemon_read_before_its_launcher_exited_is_read_again() {
    let sid = "s-0123456789abcdef0123";
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let null = aterm_uds::spawnfd::Stdio::null;
    // The shell stand-in is born first: an adopted daemon is younger than it.
    let shell = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .stdin(null())
        .stdout(null())
        .stderr(null())
        .process_group(0)
        .spawn()
        .expect("spawn the shell stand-in");
    let shell_pid = shell.id() as i32;
    let _shell = OwnedEscapeFixture {
        child: Some(shell),
        daemon: None,
    };
    let mark = dir.path().join("daemon-pid");
    let launcher = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(r#""$1" 600 & printf '%s\n' "$!" > "$2"; exec "$1" 600"#)
        .arg("launcher")
        .arg("/bin/sleep")
        .arg(&mark)
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .env("ATERM_PARENT_SESSION_ID", sid)
        .current_dir(dir.path())
        .stdin(null())
        .stdout(null())
        .stderr(null())
        .process_group(0)
        .spawn()
        .expect("spawn the launcher");
    let launcher_pid = launcher.id() as i32;
    // Own the launcher's fresh group (its daemon is in it) before any wait.
    let mut owned = OwnedEscapeFixture {
        child: Some(launcher),
        daemon: None,
    };
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let daemon_pid = loop {
        if let Ok(line) = std::fs::read_to_string(&mark)
            && line.ends_with('\n')
            && let Ok(pid) = line.trim().parse::<i32>()
        {
            break pid;
        }
        assert!(
            Instant::now() < give_up,
            "the launcher never named its daemon"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let stale = bsdinfo_uniq(daemon_pid).expect("the named daemon");
    assert_eq!(
        stale.pbsd.pbi_ppid, launcher_pid as u32,
        "held by its launcher"
    );
    owned.daemon = Some((daemon_pid, stale.p_uniqueid));
    let launcher_birth = bsdinfo_uniq(launcher_pid)
        .expect("the live launcher")
        .p_uniqueid;
    owned
        .child
        .as_mut()
        .expect("the owned launcher")
        .kill()
        .expect("kill the launcher");
    loop {
        let adopted = bsdinfo_uniq(daemon_pid)
            .is_some_and(|i| i.p_uniqueid == stale.p_uniqueid && i.pbsd.pbi_ppid == 1);
        if adopted && exited(launcher_pid) {
            break;
        }
        assert!(
            Instant::now() < give_up,
            "the launcher never became a zombie with its daemon adopted"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let born = std::time::UNIX_EPOCH
        + Duration::from_secs(stale.pbsd.pbi_start_tvsec)
        + Duration::from_micros(stale.pbsd.pbi_start_tvusec);
    // The launcher's pid reads only as the launcher (once reaped, a stranger
    // may be given it).
    let live =
        |p: i32| bsdinfo_uniq(p).filter(|i| p != launcher_pid || i.p_uniqueid == launcher_birth);
    let not_again = |p: i32| if p == daemon_pid { None } else { live(p) };
    let scan = |later: &[i32], read_after: &dyn Fn(i32) -> Option<BsdInfoWithUniqId>| {
        let mut listings = 0;
        let mut first_read = true;
        let verdict = escape_scan_among(
            shell_pid,
            sid,
            born,
            born,
            |_| {
                listings += 1;
                Some(if listings == 1 {
                    vec![shell_pid, daemon_pid, launcher_pid]
                } else {
                    later.to_vec()
                })
            },
            |p| {
                if p == daemon_pid && std::mem::take(&mut first_read) {
                    Some(stale)
                } else {
                    read_after(p)
                }
            },
        );
        (verdict, listings)
    };
    let tree = [shell_pid, daemon_pid, launcher_pid];
    let zombie_launcher = scan(&tree, &live);
    let zombie_not_again = scan(&tree, &not_again);
    let read_live = escape_scan_among(shell_pid, sid, born, born, |_| Some(tree.to_vec()), live);
    owned
        .child
        .take()
        .expect("the owned launcher")
        .wait()
        .expect("reap the launcher");
    assert!(
        live(launcher_pid).is_none() && !exited(launcher_pid),
        "a reaped launcher reads as nothing and is not a zombie"
    );
    let reaped_launcher = scan(&[shell_pid, daemon_pid], &live);
    owned
        .retire_owned_escape_fixture()
        .expect("retire the daemon");
    assert_eq!(
        zombie_launcher,
        (Scan::Escaped, 1),
        "a launcher that is a zombie at its read"
    );
    assert_eq!(
        reaped_launcher,
        (Scan::Escaped, 2),
        "a launcher reaped since the listing, settled by a later listing"
    );
    assert_eq!(
        zombie_not_again,
        (Scan::Unknown, 1),
        "a stale record that cannot be read again"
    );
    assert_eq!(read_live, Scan::Escaped, "the same tree read live");
}

/// Hang-detector waits shared by the escape fixtures below.
#[cfg(target_os = "macos")]
struct FixtureWaits {
    give_up: Instant,
}

#[cfg(target_os = "macos")]
impl FixtureWaits {
    fn new() -> Self {
        Self {
            give_up: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        }
    }

    /// The pid a fixture script wrote to `mark`.
    fn pid_in(&self, mark: &std::path::Path) -> i32 {
        loop {
            if let Ok(line) = std::fs::read_to_string(mark)
                && line.ends_with('\n')
                && let Ok(pid) = line.trim().parse::<i32>()
            {
                return pid;
            }
            assert!(
                Instant::now() < self.give_up,
                "{} was never named",
                mark.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Until `pid` is neither readable as `birth` nor a zombie: reaped.
    fn gone(&self, pid: i32, birth: u64) {
        while bsdinfo_uniq(pid).is_some_and(|i| i.p_uniqueid == birth) || exited(pid) {
            assert!(Instant::now() < self.give_up, "{pid} was never reaped");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// SIGKILL `pid` if it is still the process born as `birth`.
#[cfg(target_os = "macos")]
fn kill_birth(pid: i32, birth: u64) {
    if bsdinfo_uniq(pid).is_some_and(|i| i.p_uniqueid == birth) {
        // SAFETY: the captured birth, live a moment ago.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// When the process `info` records started.
#[cfg(target_os = "macos")]
fn born_at(info: &BsdInfoWithUniqId) -> std::time::SystemTime {
    std::time::UNIX_EPOCH
        + Duration::from_secs(info.pbsd.pbi_start_tvsec)
        + Duration::from_micros(info.pbsd.pbi_start_tvusec)
}

/// An owned shell stand-in that leads its own session, as a tab's shell does,
/// and one child of its own in that session: the custody, the shell's pid, and
/// the child's pid and record.
#[cfg(target_os = "macos")]
fn session_shell(
    dir: &std::path::Path,
    waits: &FixtureWaits,
) -> (OwnedEscapeFixture, i32, i32, BsdInfoWithUniqId) {
    let mark = dir.join("child-pid");
    let shell = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(r#""$1" 600 & printf '%s\n' "$!" > "$2"; wait; exec "$1" 600"#)
        .arg("shell")
        .arg("/bin/sleep")
        .arg(&mark)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .setsid(true)
        .spawn()
        .expect("spawn the shell stand-in");
    let shell_pid = shell.id() as i32;
    let owned = OwnedEscapeFixture {
        child: Some(shell),
        daemon: None,
    };
    let child_pid = waits.pid_in(&mark);
    let child = bsdinfo_uniq(child_pid).expect("the shell's child");
    assert_eq!(
        child.pbsd.pbi_ppid, shell_pid as u32,
        "the shell's own child"
    );
    (owned, shell_pid, child_pid, child)
}

/// An owned orphan launchd adopted, its launcher killed and reaped: the
/// custody, its pid, and its record once adopted. Its launcher made a session
/// of its own, younger than any shell stand-in made before it, as a daemon that
/// left the run's session does.
#[cfg(target_os = "macos")]
fn adopted_orphan(
    dir: &std::path::Path,
    waits: &FixtureWaits,
) -> (OwnedEscapeFixture, i32, BsdInfoWithUniqId) {
    let mark = dir.join("orphan-pid");
    let launcher = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(r#""$1" 600 & printf '%s\n' "$!" > "$2"; exec "$1" 600"#)
        .arg("launcher")
        .arg("/bin/sleep")
        .arg(&mark)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .setsid(true)
        .spawn()
        .expect("spawn the launcher");
    let mut owned = OwnedEscapeFixture {
        child: Some(launcher),
        daemon: None,
    };
    let pid = waits.pid_in(&mark);
    let birth = bsdinfo_uniq(pid).expect("the orphan-to-be").p_uniqueid;
    owned.daemon = Some((pid, birth));
    // Reaped here: once its orphan is gone too, its group is a zombie's alone.
    let mut launcher = owned.child.take().expect("the owned launcher");
    launcher.kill().expect("kill the launcher");
    launcher.wait().expect("reap the launcher");
    let adopted = loop {
        if let Some(info) = bsdinfo_uniq(pid)
            && info.p_uniqueid == birth
            && info.pbsd.pbi_ppid == 1
        {
            break info;
        }
        assert!(
            Instant::now() < waits.give_up,
            "launchd never adopted the orphan"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    (owned, pid, adopted)
}

/// [`escape_scan_among`] of `shell_pid` and `target` in `target`'s birth
/// window; `target`'s first read is `record`, every other read the kernel's.
#[cfg(target_os = "macos")]
fn scan_with_first_read(shell_pid: i32, target: i32, record: BsdInfoWithUniqId) -> Scan {
    let mut first_read = true;
    let mut first_listing = true;
    escape_scan_among(
        shell_pid,
        "s-0123456789abcdef0123",
        born_at(&record),
        born_at(&record),
        |uid| {
            if std::mem::take(&mut first_listing) {
                Some(vec![shell_pid, target])
            } else {
                uid_pids(uid).map(|pids| {
                    pids.into_iter()
                        .filter(|p| *p == shell_pid || *p == target)
                        .collect()
                })
            }
        },
        |p| {
            if p == target && std::mem::take(&mut first_read) {
                Some(record)
            } else {
                bsdinfo_uniq(p)
            }
        },
    )
}

/// A process the scan read in its window but that is gone by the time its
/// session and environment are read (a busy host's, or the shell's own
/// short-lived child) holds nothing and is no escape: a child of the shell
/// whose session can no longer be read did not leave it, and an orphan whose
/// environment can no longer be read is not the run's. The processes are real;
/// the scan's first read of each is its real record from while it lived.
/// NEGATIVE CONTROL: the same orphan alive is `Escaped`; with the presence
/// check mutated out both gone processes read `Escaped`.
#[cfg(target_os = "macos")]
#[test]
fn a_process_gone_since_its_read_is_no_escape() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let (_shell, shell_pid, child_pid, child) = session_shell(dir.path(), &waits);
    let (mut owned, orphan_pid, adopted) = adopted_orphan(dir.path(), &waits);
    let orphan_alive = scan_with_first_read(shell_pid, orphan_pid, adopted);
    kill_birth(orphan_pid, adopted.p_uniqueid);
    waits.gone(orphan_pid, adopted.p_uniqueid);
    let orphan_gone = scan_with_first_read(shell_pid, orphan_pid, adopted);
    kill_birth(child_pid, child.p_uniqueid);
    waits.gone(child_pid, child.p_uniqueid);
    let child_gone = scan_with_first_read(shell_pid, child_pid, child);
    owned
        .retire_owned_escape_fixture()
        .expect("retire the orphan");
    assert_eq!(orphan_alive, Scan::Escaped, "the orphan while it lives");
    assert_eq!(orphan_gone, Scan::Clean, "an orphan gone since its read");
    assert_eq!(
        child_gone,
        Scan::Clean,
        "a child of the shell gone since its read did not leave its session"
    );
}

/// A process launchd itself started in the run's window (a Spotlight worker
/// indexing a busy host's files: measured, 16 `mdworker_shared` in a minute
/// here, and the escape most runs under load were failed on) was never the
/// run's orphan, while an orphan launchd adopted still names its own
/// launcher as its original parent. The orphan is real; the scan's first read
/// of it is its record with launchd as its original parent. NEGATIVE CONTROL:
/// its real record reads `Escaped`; with the original-parent check mutated out
/// so does the launchd-started one.
#[cfg(target_os = "macos")]
#[test]
fn a_process_launchd_started_is_no_orphan() {
    let launchd = launchd_idversion().expect("launchd's pid version is readable");
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let (_shell, shell_pid, _, _) = session_shell(dir.path(), &waits);
    let (mut owned, orphan_pid, adopted) = adopted_orphan(dir.path(), &waits);
    assert_ne!(
        adopted.p_orig_ppidversion, launchd,
        "an adopted orphan keeps its launcher as its original parent"
    );
    let orphan = scan_with_first_read(shell_pid, orphan_pid, adopted);
    let launchd_started = scan_with_first_read(
        shell_pid,
        orphan_pid,
        BsdInfoWithUniqId {
            p_orig_ppidversion: launchd,
            ..adopted
        },
    );
    owned
        .retire_owned_escape_fixture()
        .expect("retire the orphan");
    assert_eq!(orphan, Scan::Escaped, "an orphan launchd adopted");
    assert_eq!(
        launchd_started,
        Scan::Clean,
        "a process launchd started itself"
    );
}

/// An owned session of its own, as a busy host's shell leads one: its leader
/// waits for `go`, then leaves one background job's process to launchd and
/// lives on (in the same process group, so its custody's group kill reaches
/// the orphan too).
#[cfg(target_os = "macos")]
struct ForeignSession {
    owned: OwnedEscapeFixture,
    leader: i32,
    go: std::path::PathBuf,
    mark: std::path::PathBuf,
}

#[cfg(target_os = "macos")]
impl ForeignSession {
    fn new(dir: &std::path::Path, name: &str) -> Self {
        let go = dir.join(format!("{name}-go"));
        let mark = dir.join(format!("{name}-orphan-pid"));
        let leader = aterm_uds::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg(
                r#"while [ ! -e "$3" ]; do "$1" 0.01; done
                ( "$1" 600 & printf '%s\n' "$!" > "$2" )
                exec "$1" 600"#,
            )
            .arg(name)
            .arg("/bin/sleep")
            .arg(&mark)
            .arg(&go)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .setsid(true)
            .spawn()
            .expect("spawn the session leader");
        let leader_pid = leader.id() as i32;
        Self {
            owned: OwnedEscapeFixture {
                child: Some(leader),
                daemon: None,
            },
            leader: leader_pid,
            go,
            mark,
        }
    }

    /// Release the leader's background job: the orphan's pid and its record
    /// once launchd adopted it.
    fn orphan(&mut self, waits: &FixtureWaits) -> (i32, BsdInfoWithUniqId) {
        std::fs::write(&self.go, b"go\n").expect("release the leader");
        let pid = waits.pid_in(&self.mark);
        let birth = bsdinfo_uniq(pid).expect("the orphan-to-be").p_uniqueid;
        self.owned.daemon = Some((pid, birth));
        loop {
            if let Some(info) = bsdinfo_uniq(pid)
                && info.p_uniqueid == birth
                && info.pbsd.pbi_ppid == 1
            {
                return (pid, info);
            }
            assert!(
                Instant::now() < waits.give_up,
                "launchd never adopted the background job"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// An orphan launchd adopted in the run's window from a session with a live
/// member born before the shell (a busy host's background job: measured, one
/// such orphan every 50 ms failed every gate test's boot run, 3 of 3 runs) is
/// not the run's. The run's processes are in the shell's session or one the
/// run made, and a session is entered only by birth from a member, so no
/// process older than the shell is in either. Nor is an orphan of a session
/// made after the shell whose leader lives under a process born before it
/// (another tab's: measured, a parallel instance's `( { sleep 8; } & )` failed
/// an_unclean_rc_never_opens_the_gate's boot run 2 of 180 times under load):
/// the run's own `setsid` leader is under the shell, or launchd's once its
/// parent is gone. The processes are real.
/// NEGATIVE CONTROL: the older session's orphan read with its older member left
/// out of the scan's records is `Escaped`; the younger session's orphan is
/// `Escaped` with its leader's parent unread, with its leader read as adopted by
/// launchd (the run's daemon whose launcher exited), and with that parent read
/// as born after the leader (a reused pid). With either rule mutated out, its
/// clean case is `Escaped` too.
#[cfg(target_os = "macos")]
#[test]
fn an_orphan_of_a_session_older_than_the_shell_is_no_escape() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let mut older = ForeignSession::new(dir.path(), "older");
    let (_shell, shell_pid, _, _) = session_shell(dir.path(), &waits);
    let mut younger = ForeignSession::new(dir.path(), "younger");
    let (old_orphan, old_record) = older.orphan(&waits);
    let (young_orphan, young_record) = younger.orphan(&waits);
    let birth = |pid: i32| bsdinfo_uniq(pid).map(|i| i.p_uniqueid);
    let births = (birth(older.leader), birth(shell_pid), birth(younger.leader));
    // SAFETY: `getsid` reads the process table.
    let sessions = unsafe { (libc::getsid(old_orphan), libc::getsid(young_orphan)) };
    let scan_reading = |listed: &[i32],
                        record: &BsdInfoWithUniqId,
                        read: &dyn Fn(i32) -> Option<BsdInfoWithUniqId>| {
        escape_scan_among(
            shell_pid,
            "s-0123456789abcdef0123",
            born_at(record),
            born_at(record),
            |_| Some(listed.to_vec()),
            read,
        )
    };
    let scan =
        |listed: &[i32], record: &BsdInfoWithUniqId| scan_reading(listed, record, &bsdinfo_uniq);
    let me = std::process::id() as i32;
    let leader_parent = bsdinfo_uniq(younger.leader).map(|i| i.pbsd.pbi_ppid as i32);
    let stranger = scan(&[shell_pid, old_orphan, older.leader], &old_record);
    let member_unseen = scan(&[shell_pid, old_orphan], &old_record);
    let young = scan(&[shell_pid, young_orphan, younger.leader], &young_record);
    let young_led_from_outside = scan(
        &[me, shell_pid, young_orphan, younger.leader],
        &young_record,
    );
    let leader_adopted = scan_reading(
        &[me, shell_pid, young_orphan, younger.leader],
        &young_record,
        &|p| {
            bsdinfo_uniq(p).map(|mut i| {
                if p == younger.leader {
                    i.pbsd.pbi_ppid = 1;
                }
                i
            })
        },
    );
    // This process's own parent is listed, so only the birth order ends the walk.
    // SAFETY: `getppid` reads this process's parent.
    let my_parent = unsafe { libc::getppid() };
    let parent_reused = scan_reading(
        &[my_parent, me, shell_pid, young_orphan, younger.leader],
        &young_record,
        &|p| {
            bsdinfo_uniq(p).map(|mut i| {
                if p == me {
                    i.p_uniqueid = u64::MAX;
                }
                i
            })
        },
    );
    for session in [&mut older, &mut younger] {
        session
            .owned
            .retire_owned_escape_fixture()
            .expect("retire the session");
    }
    let (Some(old_leader), Some(shell), Some(young_leader)) = births else {
        panic!("the fixture's processes were readable: {births:?}");
    };
    assert!(
        old_leader < shell && shell < young_leader,
        "one session older than the shell, one younger"
    );
    assert_eq!(
        sessions,
        (older.leader, younger.leader),
        "each orphan is in its leader's session"
    );
    assert_eq!(
        stranger,
        Scan::Clean,
        "an orphan of a session older than the shell"
    );
    assert_eq!(
        member_unseen,
        Scan::Escaped,
        "the same orphan with its older member unread"
    );
    assert_eq!(
        leader_parent,
        Some(me),
        "the younger session's leader is this test process's child"
    );
    assert_eq!(
        young,
        Scan::Escaped,
        "an orphan of a session younger than the shell, its leader's parent unread"
    );
    assert_eq!(
        young_led_from_outside,
        Scan::Clean,
        "an orphan of a younger session led from a process older than the shell"
    );
    assert_eq!(
        leader_adopted,
        Scan::Escaped,
        "the same orphan with its leader read as adopted by launchd"
    );
    assert_eq!(
        parent_reused,
        Scan::Escaped,
        "the same orphan with its leader's parent read as born after the leader"
    );
}

/// A late missing identity is not proof that a listed process disappeared.
/// The BSD records and ownership answers are modeled observations; only the
/// candidate pid is this test's own process, so the scan's session/table reads
/// never require a stand-in child, and no signal or cleanup authority is used.
#[cfg(target_os = "macos")]
#[test]
fn a_late_unreadable_escape_identity_keeps_the_scan_unknown() {
    const SHELL: i32 = i32::MAX;
    const SID: &str = "s-0123456789abcdef0123";
    let pid = i32::try_from(std::process::id()).expect("our pid fits");
    assert_ne!(pid, SHELL);
    let mut candidate = bsdinfo_uniq(pid).expect("our own process record");
    candidate.pbsd.pbi_ppid = 1;
    candidate.pbsd.pbi_start_tvsec = 2;
    candidate.pbsd.pbi_start_tvusec = 0;
    candidate.p_uniqueid = 20;
    candidate.p_orig_ppidversion = launchd_idversion().map_or(0, |v| v.wrapping_add(1));
    let mut shell = candidate;
    shell.pbsd.pbi_pid = SHELL as u32;
    shell.pbsd.pbi_start_tvsec = 1;
    shell.p_uniqueid = 10;
    let at = std::time::UNIX_EPOCH + Duration::from_secs(2);
    let scan = |late: usize, replacement: bool, owner: Option<bool>, later: Option<bool>| {
        let mut listings = 0;
        let mut candidate_reads = 0;
        escape_scan_with_owner(
            SHELL,
            SID,
            at,
            at,
            |_| {
                listings += 1;
                if listings == 1 || later == Some(true) {
                    Some(vec![SHELL, pid])
                } else {
                    later.map(|_| vec![SHELL])
                }
            },
            |p| {
                if p == SHELL {
                    return Some(shell);
                }
                if p != pid {
                    return None;
                }
                candidate_reads += 1;
                if candidate_reads == late {
                    replacement.then_some(BsdInfoWithUniqId {
                        p_uniqueid: 21,
                        ..candidate
                    })
                } else {
                    Some(candidate)
                }
            },
            |_, _| owner,
        )
    };
    // Ordinary ownership, foreign environment, known replacement and a
    // complete later absence are controls before either uncertainty witness.
    assert_eq!(scan(0, false, Some(true), Some(true)), Scan::Escaped);
    assert_eq!(scan(0, false, Some(false), Some(true)), Scan::Clean);
    assert_eq!(scan(0, false, None, Some(true)), Scan::Escaped);
    for late in [2, 3] {
        assert_eq!(scan(late, true, None, Some(true)), Scan::Clean);
        assert_eq!(scan(late, false, None, Some(false)), Scan::Clean);
    }
    let uncertain = [
        scan(2, false, None, Some(true)),
        scan(3, false, None, Some(true)),
        scan(2, false, None, None),
        scan(3, false, None, None),
    ];
    assert!(
        uncertain.iter().all(|&s| s == Scan::Unknown),
        "LATE_ESCAPE_IDENTITY_UNKNOWN_WAS_CLEAN: {uncertain:?}"
    );
}

/// The original unreaped Child owns this process group until its wait. The
/// orphan's separate custody names one captured birth, never a host-wide name.
#[cfg(target_os = "macos")]
struct OwnedEscapeFixture {
    child: Option<aterm_uds::spawnfd::Child>,
    daemon: Option<(i32, u64)>,
}

#[cfg(target_os = "macos")]
impl OwnedEscapeFixture {
    fn retire_owned_escape_fixture(&mut self) -> std::io::Result<()> {
        let mut errors = Vec::new();
        if let Some((pid, birth)) = self.daemon
            && bsdinfo_uniq(pid).is_some_and(|info| info.p_uniqueid == birth)
        {
            // SAFETY: exactly the fixture's captured process birth.
            if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    errors.push(format!("kill the captured daemon: {error}"));
                }
            }
        }
        if let Some(child) = self.child.as_mut() {
            // SAFETY: this unreaped Child still owns its freshly created group;
            // no other Child/try_wait can reap it or let its group id be reused.
            if unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    errors.push(format!("kill the owned group: {error}"));
                }
            }
            if let Err(error) = child.kill()
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                errors.push(format!("kill the owned child: {error}"));
            }
            let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        // The exact Child reached EOF and is reaped. Only now
                        // release its custody; a failed wait leaves it in self.
                        self.child.take();
                        break;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) => {
                        errors.push("the owned child did not exit by the hang detector".into());
                        break;
                    }
                    Err(error) => {
                        errors.push(format!("reap the owned child: {error}"));
                        break;
                    }
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(std::io::Error::other(errors.join("; ")))
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for OwnedEscapeFixture {
    fn drop(&mut self) {
        if let Err(error) = self.retire_owned_escape_fixture() {
            eprintln!("owned escape fixture cleanup failed: {error}");
        }
    }
}

/// U13: an update's apply retires the spare first (it is in no manifest), and
/// a spare's own life — its spawn landing, its parked note — never counts as
/// activity that would hold an update off. Negative control: the slot is
/// still there before the apply.
#[test]
fn an_update_retires_the_spare_and_a_spare_is_never_activity() {
    let mut app = App::headless_for_test();
    let id = 50;
    let mut slot = parked_slot(&app, id, None);
    slot.state = SlotState::Booked;
    let link = Arc::clone(&slot.link);
    slot.session = None;
    app.spares.slot = Some(slot);
    app.spares.captured = Some(Vec::new());
    let epoch = app.update_handoff_activity_epoch;
    app.on_spare_spawned(SpareSpawned {
        id,
        session: Ok(crate::stub_session(id)),
    });
    let _ = link.witness.parked_seen.set(Instant::now());
    app.on_spare_note(id, SpareNote::Parked);
    assert_eq!(
        app.spares.slot.as_ref().map(|s| s.state),
        Some(SlotState::Parked)
    );
    assert_eq!(
        app.update_handoff_activity_epoch, epoch,
        "a spare's life is not the person's activity"
    );
    let _ = app.apply_staged_update_now(
        crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0, 0),
        crate::native_updater_service::ApplyMode::Immediate,
        None,
        None,
    );
    assert!(app.spares.slot.is_none(), "retired before the update");
    assert!(app.spares.swallows(id), "and its late wakes swallowed");
}

/// A promotion writes `run` only on a stamp of the rc generation taken after
/// it asked (`Job::Freshen`, answered on the `aterm-spare` thread): a mirror
/// that still says eligible for a generation an edit has since moved never
/// runs the edited startup files unseen. Negative control: without the
/// freshen, the first look writes `run` on the stale mirror.
#[cfg(target_os = "macos")]
#[test]
fn a_promotion_runs_only_on_a_fresh_stamp_of_the_gate() {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut app = app_in("/tmp");
    app.spares.captured = Some(Vec::new());
    app.spares.power = || false;
    app.config.spare_shell_needs_person = Some(false);
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let fifo_at = dir.path().join("fifo");
    let id = app.next_session_id;
    app.next_session_id += 1;
    let slot = parked_slot(&app, id, Some("/tmp"));
    slot.link
        .witness
        .hold_fifo(SpareFifo::make_at(fifo_at.clone()).expect("fifo"));
    app.spares.slot = Some(slot);
    let shared = Arc::clone(&app.spares.shared);
    shared.generation.store(7, Ordering::Release);
    shared.eligible.store(true, Ordering::Release);
    // What the shell at its hold would read, without blocking.
    let mut shell_end = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo_at)
        .expect("the shell's end");
    let mut read = move || {
        let mut buf = [0u8; 16];
        shell_end.read(&mut buf).map_or_else(
            |_| String::new(),
            |n| String::from_utf8_lossy(&buf[..n]).into_owned(),
        )
    };
    let asked = |app: &App| {
        app.spares.captured.as_ref().and_then(|jobs| {
            jobs.iter().rev().find_map(|j| match j {
                Job::Freshen(n) => Some(*n),
                _ => None,
            })
        })
    };
    let state = |app: &App| app.spares.slot.as_ref().map(|s| s.state);
    let at = Instant::now() + Duration::from_secs(3_600);

    // The first look asks for a stamp and writes nothing.
    app.spare_promote(at);
    let n = asked(&app).expect("a freshen asked");
    assert_eq!(state(&app), Some(SlotState::Parked));
    assert_eq!(read(), "", "no `run` before a fresh stamp");
    // Asked and unanswered: the next look asks nothing more, writes nothing.
    app.spare_promote(at);
    assert_eq!(asked(&app), Some(n));
    assert_eq!(read(), "");
    // The stamp finds a startup file edited: a new generation, its gate shut.
    shared.generation.store(8, Ordering::Release);
    shared.eligible.store(false, Ordering::Release);
    shared.fresh.store(n, Ordering::Release);
    app.spare_promote(at);
    assert_eq!(state(&app), Some(SlotState::Parked));
    assert_eq!(read(), "", "the edited generation never runs unseen");
    // Its gate opens later: the old answer is stale, a new one is asked, and
    // only its answer runs the spare, for the new generation.
    shared.eligible.store(true, Ordering::Release);
    let later = at + FRESH_FOR;
    app.spare_promote(later);
    let m = asked(&app).expect("asked again");
    assert_ne!(m, n);
    assert_eq!(read(), "");
    shared.fresh.store(m, Ordering::Release);
    app.spare_promote(later);
    assert_eq!(state(&app), Some(SlotState::Warming));
    assert_eq!(app.spares.slot.as_ref().map(|s| s.generation), Some(8));
    assert_eq!(read(), "run\n");
}

/// A start record the reader saw before the spawn's completion landed (its
/// note found the slot still Booked): the shell ran past its parked hold, so
/// the spare goes and spares stay off for this script set and shell, exactly
/// as for a record seen once it is Parked. Negative control: the note alone,
/// while Booked, changes nothing.
#[test]
fn a_start_record_before_the_spawn_lands_retires_the_spare() {
    let mut app = App::headless_for_test();
    let id = 51;
    let mut slot = parked_slot(&app, id, None);
    slot.state = SlotState::Booked;
    slot.session = None;
    let link = Arc::clone(&slot.link);
    let key = parked_key(&slot.inputs);
    app.spares.slot = Some(slot);
    app.spares.captured = Some(Vec::new());
    let _ = link.witness.parked_seen.set(Instant::now());
    let _ = link.witness.start_seen.set(a_start_seen(StartKind::Cold));
    app.on_spare_note(id, SpareNote::AtStart);
    assert_eq!(
        app.spares.slot.as_ref().map(|s| s.state),
        Some(SlotState::Booked),
        "the note finds nothing to judge yet"
    );
    app.on_spare_spawned(SpareSpawned {
        id,
        session: Ok(crate::stub_session(id)),
    });
    assert!(app.spares.slot.is_none(), "it ran past its hold: retired");
    assert_eq!(app.spares.parked_failed, Some(key));
    assert!(app.spares.swallows(id));
}

/// A write that lands in a spare's input queue before its adoption (an rc's
/// query, answered by its reader) fires the input watch into the swallowing
/// post; the watch fires once per episode until its probe re-arms it, so the
/// adoption re-arms it (and wakes it for the byte still unread), and the
/// adopted tab's next write wakes the loop as a cold tab's does. Negative
/// control: without the adoption, the next write wakes nothing.
#[cfg(target_os = "macos")]
#[test]
fn an_adopted_spares_input_watch_is_armed_again() {
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    // The id the stub tab below is installed under.
    let id = app.next_session_id;
    let (master, slave) = crate::input_stall::tests::raw_pty_pair();
    let sink = Arc::new(aterm_session::sink::SinkWriter::new(master));
    let (link, seen) = captured_link(id);
    let post = link.post();
    assert!(!crate::input_stall::watch_input(&sink, move || {
        let _ = post(Wake::InputWatch { session: id });
    }));
    let watches = || {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|w| w.starts_with("InputWatch"))
            .count()
    };
    // Nobody's yet: the query reply fires the watch, and the post swallows it.
    sink.write_frame_nonparking(b"\x1b[?1;2c").expect("reply");
    assert_eq!(watches(), 0, "swallowed while it is nobody's");
    sink.write_frame_nonparking(b"x").expect("a second write");
    assert_eq!(watches(), 0, "fired once: the watch waits for a re-arm");
    // Adopted: the session lands and goes live.
    let mut session = crate::stub_session_with_sink(id, Arc::clone(&sink));
    Arc::get_mut(&mut session.ctx).expect("one ctx").witness = Arc::clone(&link.witness);
    app.push_stub_tab(wid, session);
    app.spares.transit.push(Transit {
        id,
        link: Arc::clone(&link),
        tier: SlotState::Parked,
        grid: app.spawn_grid(wid),
        age: Duration::ZERO,
        rc_ms: None,
        inputs: app.spare_gen_inputs(),
        miss: Arc::default(),
    });
    app.spares.captured = Some(Vec::new());
    app.adopt_live(id, wid);
    assert_eq!(
        watches(),
        1,
        "re-armed and woken for the bytes still unread"
    );
    // SAFETY: both ends are this test's, opened by `raw_pty_pair`.
    unsafe {
        libc::close(slave);
    }
}

/// The rc generation (design §7.1) moves with what a shell's start reads — a
/// startup file, a `shell.d` hook edited in place (its folder's mtime does
/// not move) or added, the rc folder a `.zshenv` points `ZDOTDIR` at — and
/// not with the history file, which the release re-reads instead. Negative
/// control: an untouched tree stamps the same twice.
#[test]
fn the_rc_generation_moves_with_startup_files_and_hooks_not_history() {
    let home = aterm_tempfile::tempdir().expect("tempdir");
    let h = home.path();
    std::fs::write(h.join(".zshrc"), "PS1='> '\n").unwrap();
    let hooks = h.join(".aterm").join("shell.d");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("path.zsh"), "export PATH=/a:$PATH\n").unwrap();
    let inputs = GenInputs {
        shell: aterm_core::shell_integration::ShellType::Zsh,
        shell_path: PathBuf::from("/bin/zsh"),
        home: h.to_path_buf(),
        launch_zdotdir: h.to_path_buf(),
        fingerprint: 1,
        containment: 2,
    };
    let g0 = rc_generation(&inputs, None);
    assert_eq!(rc_generation(&inputs, None), g0, "an untouched tree");
    // The history file is not a startup file.
    std::fs::write(h.join(".zsh_history"), ": 1:0;ls\n").unwrap();
    assert_eq!(rc_generation(&inputs, None), g0, "history moves nothing");
    // A hook edited in place: a longer line, its folder's mtime unmoved.
    let dir_mtime = std::fs::metadata(&hooks).unwrap().modified().unwrap();
    std::fs::write(hooks.join("path.zsh"), "export PATH=/a:/b:$PATH\n").unwrap();
    assert_eq!(
        std::fs::metadata(&hooks).unwrap().modified().unwrap(),
        dir_mtime,
        "an in-place edit leaves the folder's mtime"
    );
    let g1 = rc_generation(&inputs, None);
    assert_ne!(g1, g0, "a hook edited in place moves it");
    // A hook added.
    std::fs::write(hooks.join("more.zsh"), "alias l=ls\n").unwrap();
    let g2 = rc_generation(&inputs, None);
    assert_ne!(g2, g1, "a hook added moves it");
    // A `.zshenv` that points ZDOTDIR elsewhere: that folder's files count.
    let z = h.join("zdot");
    std::fs::create_dir_all(&z).unwrap();
    std::fs::write(z.join(".zshrc"), "PS1='% '\n").unwrap();
    let in_z = rc_generation(&inputs, Some(&z));
    assert_ne!(in_z, g2, "the final rc folder is part of it");
    std::fs::write(z.join(".zshrc"), "PS1='%% '\nalias x=y\n").unwrap();
    assert_ne!(
        rc_generation(&inputs, Some(&z)),
        in_z,
        "and its files are stamped"
    );
}

/// Corrupt disk state cannot partially authorize running startup files unseen.
#[test]
fn a_malformed_gate_snapshot_cannot_keep_an_eligible_record() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = gate_file(&dir);
    const GOOD: &str = "00000000000000a1\t3\t0\t\t1\t1800000000\t\n";
    std::fs::write(&path, GOOD).unwrap();
    assert!(GateBook::at(Some(path.clone())).eligible_at(0xa1, T));
    for text in [
        format!("{GOOD}broken\n"),
        GOOD.replace("\t0\t\t1", "\t0\t1,nope\t1"),
        format!("{GOOD}{GOOD}"),
        (0..=GATE_KEEP)
            .map(|n| GOOD.replace("00000000000000a1", &format!("{:016x}", 0xa1 + n)))
            .collect::<String>(),
    ] {
        std::fs::write(&path, &text).unwrap();
        let allowed = GateBook::at(Some(path.clone())).eligible_at(0xa1, T);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            text.as_bytes(),
            "a read preserves the private input"
        );
        assert!(!allowed, "a malformed snapshot kept eligibility");
    }
}

/// A valid prefix in an oversized file is not a valid persisted gate snapshot.
#[test]
fn an_oversized_gate_snapshot_cannot_authorize_warm_startup() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = gate_file(&dir);
    let good = b"00000000000000a1\t3\t0\t\t1\t1800000000\t\n";
    std::fs::write(&path, good).unwrap();
    assert!(GateBook::at(Some(path.clone())).eligible_at(0xa1, T));
    let mut large = good.to_vec();
    large.resize(128 * 1024 + 1, b'x');
    std::fs::write(&path, &large).unwrap();
    let allowed = GateBook::at(Some(path.clone())).eligible_at(0xa1, T);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        large,
        "a read preserves the oversized input"
    );
    assert!(!allowed, "oversized disk state authorized warm startup");
}

/// A stale staging entry is private data, not permission to truncate or remove it.
#[test]
fn persisting_a_gate_never_overwrites_an_existing_staging_entry() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = gate_file(&dir);
    let mut book = GateBook::at(Some(path.clone()));
    for _ in 0..3 {
        book.sample(0xa1, Sample::Clean { rc_ms: 1 });
    }
    assert!(GateBook::at(Some(path.clone())).eligible_at(0xa1, T));
    let before = std::fs::read(&path).unwrap();
    let stage = path.with_extension(format!("tmp.{}", std::process::id()));
    const PRIVATE: &[u8] = b"private staging bytes\n";
    std::fs::write(&stage, PRIVATE).unwrap();
    book.sample(0xa1, Sample::Unclean);
    assert_eq!(
        std::fs::read(&stage).ok(),
        Some(PRIVATE.to_vec()),
        "a collision stays untouched"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a refused stage publishes nothing"
    );
}

/// A kernel read that cannot identify the shell proves no clean process scan.
#[cfg(target_os = "macos")]
#[test]
fn an_unreadable_shell_is_not_a_clean_escape_scan() {
    assert_ne!(
        escape_scan(
            -1,
            "s-0123456789abcdef0123",
            std::time::UNIX_EPOCH,
            std::time::UNIX_EPOCH
        ),
        Scan::Clean,
        "missing shell identity is not a clean scan"
    );
}

/// Actual visible-run judgment: a matching image passes as far as an injected
/// rate limit; missing or unequal images must stop before any process scan.
#[test]
fn a_visible_run_needs_two_observed_matching_shell_images() {
    let session = crate::stub_session(91);
    let input = session.ctx.sink.driven_input();
    let now = Instant::now();
    let judged = |initial: Option<&str>, current: Option<&str>| {
        let witness = Arc::new(Witness::default());
        witness.shell.set((42, initial.map(PathBuf::from))).unwrap();
        let obs = Observation {
            witness,
            sid: "s-0123456789abcdef0123".into(),
            started_at: now,
            input0: Some(input),
            host_effects0: 0,
            early_control0: 0,
            inputs: GenInputs {
                shell: aterm_core::shell_integration::ShellType::Zsh,
                shell_path: PathBuf::new(),
                home: PathBuf::new(),
                launch_zdotdir: PathBuf::new(),
                fingerprint: 1,
                containment: 1,
            },
        };
        let mut seen = a_start_seen(StartKind::Cold);
        seen.at = now;
        seen.input = Some(input);
        seen.fg_pgrp = 42;
        seen.image = current.map(PathBuf::from);
        // The future stamp deterministically withholds the scan within this
        // test's minute detector; it is not a latency assertion or a sleep.
        judge(&obs, &seen, &mut Some(now + Duration::from_secs(3600)))
    };
    assert_eq!(
        judged(Some("/owned/shell"), Some("/owned/shell")),
        Sample::Inconclusive
    );
    assert_eq!(judged(Some("/owned/shell"), None), Sample::Unclean);
    assert_eq!(judged(None, Some("/owned/shell")), Sample::Unclean);
    assert_eq!(
        judged(Some("/owned/shell"), Some("/owned/other")),
        Sample::Unclean
    );
    assert_eq!(
        judged(None, None),
        Sample::Unclean,
        "absence cannot match a shell image"
    );
}

/// A private run stopped at the generation guard, so these posture tests use
/// real verify decisions without reading the host's process table.
fn verify_posture_only(
    home: &Path,
    flags: Option<aterm_core::terminal::StartFlags>,
    initial: Option<&str>,
    current: Option<&str>,
) -> Verdict {
    let witness = Arc::new(Witness::default());
    witness.shell.set((42, initial.map(PathBuf::from))).unwrap();
    let mut seen = a_start_seen(StartKind::Spare);
    seen.mark.flags = flags;
    seen.fg_pgrp = 42;
    seen.image = current.map(PathBuf::from);
    witness.start_seen.set(seen).unwrap();
    let inputs = GenInputs {
        shell: aterm_core::shell_integration::ShellType::Zsh,
        shell_path: home.join("shell"),
        home: home.into(),
        launch_zdotdir: home.into(),
        fingerprint: 1,
        containment: 1,
    };
    assert_ne!(
        rc_generation(&inputs, None),
        0,
        "the later generation refusal is reachable"
    );
    verify(
        &VerifyJob {
            id: 91,
            witness,
            sid: "s-0123456789abcdef0123".into(),
            inputs,
            generation: 0,
            run_at: Instant::now(),
            reply: Arc::new(|_| true),
        },
        &mut None,
        &Shared::default(),
    )
}

#[test]
fn warm_verification_requires_a_complete_flags_record() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let good = aterm_core::terminal::StartFlags::default();
    let seen = |flags| {
        verify_posture_only(
            dir.path(),
            flags,
            Some("/owned/shell"),
            Some("/owned/shell"),
        )
    };
    assert_eq!(seen(Some(good)), Verdict::Backoff("stale"));
    let busy = aterm_core::terminal::StartFlags { sched: 1, ..good };
    assert_eq!(seen(Some(busy)), Verdict::Deterministic("sched"));
    assert_eq!(
        seen(None),
        Verdict::Backoff("no-flags"),
        "missing flags cannot reach later admission"
    );
}

#[test]
fn warm_verification_needs_two_observed_matching_shell_images() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let seen = |initial, current| {
        verify_posture_only(
            dir.path(),
            Some(aterm_core::terminal::StartFlags::default()),
            initial,
            current,
        )
    };
    assert_eq!(
        seen(Some("/owned/shell"), Some("/owned/shell")),
        Verdict::Backoff("stale")
    );
    assert_eq!(
        seen(Some("/owned/shell"), None),
        Verdict::Backoff("not-the-shell")
    );
    assert_eq!(
        seen(None, Some("/owned/shell")),
        Verdict::Backoff("not-the-shell")
    );
    assert_eq!(
        seen(Some("/owned/shell"), Some("/owned/other")),
        Verdict::Backoff("not-the-shell")
    );
    assert_eq!(
        seen(None, None),
        Verdict::Backoff("not-the-shell"),
        "absence cannot match a warm shell image"
    );
}
