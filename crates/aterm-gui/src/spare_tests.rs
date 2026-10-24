// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless unit tests for the warm spare shell.

use super::*;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::tab_spawn::SpawnAsk;
use std::sync::Mutex as StdMutex;

const T: u64 = 1_800_000_000;

fn gate_file(dir: &aterm_tempfile::TempDir) -> PathBuf {
    dir.path().join("spare-gate-v1.tsv")
}

/// Three clean runs open a generation; failures reset or back off; the gate file round-trips.
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

    book.fail_at(other, Verdict::Deterministic("sched"), T);
    for _ in 0..5 {
        book.sample(other, Sample::Clean { rc_ms: 10 });
    }
    assert!(!book.eligible_at(other, T + 365 * 24 * 3600));
    assert_eq!(book.report(other), "ineligible:sched");

    let mut back = GateBook::at(Some(gate_file(&dir)));
    assert!(back.eligible_at(g, T + 4000 + 24 * 3600));
    assert!(!back.eligible_at(other, T + 365 * 24 * 3600));
    assert_eq!(back.t_ready(g), Duration::from_millis(1200));
}

/// Build a parked stub [`Slot`] for conformance testing.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn parked_slot_for(app: &App, id: u64, cwd: Option<&str>) -> Slot {
    parked_slot(app, id, cwd)
}

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

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn app_in(cwd: &str) -> App {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let s = crate::stub_session(app.next_session_id);
    crate::term_lock(&s.term).process(format!("\x1b]7;file://localhost{cwd}\x07").as_bytes());
    app.push_stub_tab(wid, s);
    app
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
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

/// Each adoption gate reports its miss reason and leaves the spare in place on refusal.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn each_adoption_gate_names_its_miss_and_a_spare_that_passes_them_is_taken() {
    let wid = WindowId(0);
    let mut app = app_in("/tmp");
    app.session_factory.shell_override = Some("/bin/zsh".to_owned());
    let grid = app.spawn_grid(wid);
    let plain = SpawnAsk::new(None);
    let id = app.next_session_id;
    app.next_session_id += 1;
    let try_with = |app: &mut App, ask: &SpawnAsk, grid: Grid| {
        app.try_adopt_spare(wid, Some(wid), grid, ask).map(|_| ())
    };

    assert_eq!(try_with(&mut app, &plain, grid), Err("none"));
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));
    app.spares.slot.as_mut().unwrap().state = SlotState::Booked;
    assert_eq!(try_with(&mut app, &plain, grid), Err("booked"));
    app.spares.slot.as_mut().unwrap().state = SlotState::Parked;
    app.config.spare_shell = Some("off".into());
    assert_eq!(try_with(&mut app, &plain, grid), Err("off"));
    app.config.spare_shell = None;
    for ask in [
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
    app.spares.slot.as_mut().unwrap().fingerprint ^= 1;
    assert_eq!(try_with(&mut app, &plain, grid), Err("factory"));
    app.spares.slot.as_mut().unwrap().fingerprint ^= 1;
    app.spares
        .shared
        .pre_hold
        .store(PRE_HOLD_ZSH, Ordering::Release);
    assert_eq!(
        try_with(&mut app, &SpawnAsk::new(Some("/private")), grid),
        Err("folder")
    );
    app.spares.slot.as_mut().unwrap().cwd = Some("/var".into());
    assert_eq!(try_with(&mut app, &plain, grid), Err("folder"));
    app.spares.slot.as_mut().unwrap().cwd = Some("/tmp".into());
    app.spares.shared.pre_hold.store(0, Ordering::Release);
    app.spares.slot.as_mut().unwrap().since = Instant::now().checked_sub(PARKED_MAX_AGE).unwrap();
    assert_eq!(try_with(&mut app, &plain, grid), Err("age"));
    app.spares.slot.as_mut().unwrap().since = Instant::now();
    app.spares.slot.as_ref().unwrap().link.taint(taint::BELL);
    assert_eq!(try_with(&mut app, &plain, grid), Err("taint"));
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));
    app.spares.slot.as_ref().unwrap().link.decide(Wake::Exit {
        session: id,
        window: wid,
    });
    assert_eq!(try_with(&mut app, &plain, grid), Err("process"));
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));

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
    app.spares.slot.as_mut().unwrap().cwd = Some("/var".into());
    assert_eq!(try_with(&mut app, &plain, grid), Err("folder"));
    assert_eq!(
        try_with(&mut app, &SpawnAsk::new(Some("/usr")), grid),
        Err("folder")
    );
    app.spares.slot.as_mut().unwrap().cwd = Some("/tmp".into());
    let (record, _transfer) = crate::app_update_handoff::dry_run_capture_tests::prelaunched(
        1,
        crate::native_updater_service::ApplyMode::Automatic,
        crate::app_update_handoff::FreezeSeed::Default,
    );
    app.update_handoff_prelaunch = Some(record);
    assert_eq!(try_with(&mut app, &plain, grid), Err("update"));
    app.update_handoff_prelaunch = None;
    assert!(app.spares.slot.is_some(), "every refusal leaves the spare");

    let (slot, session) = app
        .try_adopt_spare(wid, Some(wid), grid, &plain)
        .expect("adopted");
    assert_eq!((slot.id, session.id), (id, id));
    assert!(app.spares.slot.is_none());
    app.spares.slot = Some(parked_slot(&app, id + 1, Some("/tmp")));
    assert!(try_with(&mut app, &plain, other_grid).is_ok());
    app.spares.slot = Some(parked_slot(&app, id + 2, Some("/var")));
    assert!(try_with(&mut app, &plain, grid).is_ok());
    app.spares.slot = Some(parked_slot(&app, id + 3, Some("/var")));
    assert!(try_with(&mut app, &SpawnAsk::new(Some("/usr")), grid).is_ok());
}

/// A `Verifying` spare is adoptable through the warm gates while a `Spawned` one is not.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_spare_verifying_its_run_is_taken_as_a_warming_one_is() {
    let wid = WindowId(0);
    let mut app = app_in("/tmp");
    app.session_factory.shell_override = Some("/bin/zsh".to_owned());
    let grid = app.spawn_grid(wid);
    let plain = SpawnAsk::new(None);
    let id = app.next_session_id;
    app.next_session_id += 2;
    app.spares.shared.generation.store(7, Ordering::Release);
    app.spares.shared.eligible.store(true, Ordering::Release);
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));
    app.spares.slot.as_mut().unwrap().state = SlotState::Spawned;
    let try_with = |app: &mut App| {
        app.try_adopt_spare(wid, Some(wid), grid, &plain)
            .map(|_| ())
    };
    assert_eq!(try_with(&mut app), Err("booked"), "still spawning");
    app.spares.slot.as_mut().unwrap().state = SlotState::Verifying;
    app.spares.shared.eligible.store(false, Ordering::Release);
    assert_eq!(try_with(&mut app), Err("gate"), "a warm tier's gate");
    app.spares.shared.eligible.store(true, Ordering::Release);
    app.spares.shared.generation.store(8, Ordering::Release);
    assert_eq!(try_with(&mut app), Err("stale"), "a warm tier's generation");
    app.spares.shared.generation.store(7, Ordering::Release);
    let (slot, session) = app
        .try_adopt_spare(wid, Some(wid), grid, &plain)
        .expect("adopted while it verifies");
    assert_eq!(
        (slot.id, session.id, slot.state),
        (id, id, SlotState::Verifying)
    );
    app.spares.slot = Some(parked_slot(&app, id + 1, Some("/tmp")));
    app.spares.slot.as_mut().unwrap().state = SlotState::Verifying;
    app.on_spare_note(id, SpareNote::Verified(Verdict::Backoff("escaped")));
    assert_eq!(
        app.spares.slot.as_ref().map(|s| (s.id, s.state)),
        Some((id + 1, SlotState::Verifying))
    );
}

/// Booking and promotion enforce person presence, grace periods, budgets, and pressure holds.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn booking_and_promotion_wait_for_every_condition() {
    let mut app = spare_app_in("/tmp");
    let inputs = app.spare_gen_inputs();
    let now = Instant::now();
    assert_eq!(
        app.spare_book_check(now, &inputs),
        BookCheck::No("no-person")
    );
    app.config.spare_shell_needs_person = Some(false);
    app.spares.boot_at = Some(now);
    assert_eq!(
        app.spare_book_check(now, &inputs),
        BookCheck::Wait(now + BOOT_GRACE)
    );
    let later = now + BOOT_GRACE;
    app.spares.person.set(Some(later));
    assert_eq!(
        app.spare_book_check(later, &inputs),
        BookCheck::Wait(later + SETTLE)
    );
    let t = later + SETTLE;
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
    for bits in [PRE_HOLD_ZSH, Shared::unread().pre_hold.into_inner()] {
        app.spares.shared.pre_hold.store(bits, Ordering::Release);
        assert_eq!(app.spare_book_check(t, &inputs), BookCheck::No("system-rc"));
        app.spares.shared.eligible.store(true, Ordering::Release);
        assert!(matches!(
            app.spare_book_check(t, &inputs),
            BookCheck::Go { .. }
        ));
        app.spares.shared.eligible.store(false, Ordering::Release);
    }
    app.spares
        .shared
        .pre_hold
        .store(PRE_HOLD_BASH, Ordering::Release);
    assert!(matches!(
        app.spare_book_check(t, &inputs),
        BookCheck::Go { .. }
    ));
    app.spares.shared.pre_hold.store(0, Ordering::Release);
    app.spares.last_replace = Some(t);
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::Wait(t + REPLACE_MIN)
    );
    app.spares.last_replace = None;
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
    app.spares.pressure_at = Some(t);
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::Wait(t + PRESSURE_HOLD)
    );
    app.spares.pressure_at = None;
    app.spares.parked_failed = Some(parked_key(&inputs));
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::No("parked-failed")
    );
    app.spares.parked_failed = None;
    app.session_factory.temporal_recording = true;
    assert_eq!(app.spare_book_check(t, &inputs), BookCheck::No("factory"));
    app.session_factory.temporal_recording = false;

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
    let previous_shell = app.session_factory.shell_override.clone();
    for (program, admitted) in [
        ("/bin/zsh", true),
        ("/bin/bash", true),
        ("/opt/homebrew/bin/fish", true),
        ("/bin/sh", false),
    ] {
        app.session_factory.shell_override = Some(program.to_owned());
        let mut candidate = inputs.clone();
        candidate.shell = crate::spawn::detect_spawn_shell(Some(program));
        candidate.shell_path = program.into();
        candidate.fingerprint = app.session_factory.spawn_fingerprint();
        assert_eq!(app.spare_factory_ok(), admitted, "{program}");
        let decision = app.spare_book_check(u, &candidate);
        if admitted {
            assert!(
                matches!(decision, BookCheck::Go { .. }),
                "{program}: {decision:?}"
            );
        } else {
            assert_eq!(decision, BookCheck::No("factory"), "{program}");
        }
    }
    app.session_factory.shell_override = previous_shell;
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

/// Parked spawn re-checks system rc files that run before the hold.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_parked_spawn_looks_for_the_system_files_again() {
    use aterm_core::shell_integration::ShellType;
    let shared = Shared::default();
    let zshenv_now = |f: &str| f == "/etc/zsh/zshenv";
    assert!(parked_spawn_runs_unseen(
        &shared,
        ShellType::Zsh,
        zshenv_now
    ));
    assert!(shared.ran_before_hold(ShellType::Zsh));
    assert!(!shared.ran_before_hold(ShellType::Bash));
    assert!(!parked_spawn_runs_unseen(
        &shared,
        ShellType::Bash,
        zshenv_now
    ));
    assert!(!parked_spawn_runs_unseen(
        &shared,
        ShellType::Fish,
        zshenv_now
    ));
    shared.eligible.store(true, Ordering::Release);
    assert!(!parked_spawn_runs_unseen(
        &shared,
        ShellType::Zsh,
        zshenv_now
    ));
    shared.eligible.store(false, Ordering::Release);
    assert!(!parked_spawn_runs_unseen(&shared, ShellType::Zsh, |_| {
        false
    }));
    assert!(!shared.ran_before_hold(ShellType::Zsh));
}

/// A spawn whose folder would fall back refunds its budget slot and bars that folder until it changes.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_folder_a_spare_would_fall_back_from_books_none_until_it_moves() {
    let mut app = spare_app_in("/tmp");
    app.config.spare_shell_needs_person = Some(false);
    app.spares.captured = Some(Vec::new());
    let inputs = app.spare_gen_inputs();
    let now = Instant::now();
    app.spares.boot_at = Some(now);
    let t = now + BOOT_GRACE;
    let focused = app.focused_session_id(WindowId(0)).expect("a tab");
    crate::term_lock(&app.pool.get(focused).unwrap().term).process(b"\x1b]133;A\x07");
    let go = |cwd: &str| BookCheck::Go {
        window: WindowId(0),
        cwd: Some(cwd.into()),
    };
    let booked = |app: &mut App, id: u64| {
        let mut slot = parked_slot(app, id, Some("/tmp"));
        slot.state = SlotState::Booked;
        slot.session = None;
        slot.booked = t;
        app.spares.parked_spawns.push_back(t);
        app.spares.slot = Some(slot);
    };
    app.spare_follow(t);
    assert_eq!(app.spare_book_check(t, &inputs), go("/tmp"));
    booked(&mut app, 60);
    app.on_spare_spawned(SpareSpawned {
        id: 60,
        session: Err(std::io::Error::other(FolderFallsBack)),
    });
    assert!(app.spares.slot.is_none());
    assert!(
        app.spares.parked_spawns.is_empty(),
        "given back to the budget"
    );
    assert_eq!(app.spare_book_check(t, &inputs), BookCheck::No("folder"));
    app.spare_follow(t);
    assert_eq!(
        app.spare_book_check(t, &inputs),
        BookCheck::No("folder"),
        "the same folder at the next tick"
    );
    crate::term_lock(&app.pool.get(focused).unwrap().term)
        .process(b"\x1b]7;file://localhost/usr\x07");
    app.spare_follow(t);
    assert_eq!(app.spare_book_check(t, &inputs), go("/usr"));
    booked(&mut app, 61);
    app.on_spare_spawned(SpareSpawned {
        id: 61,
        session: Err(std::io::Error::other(
            "fork: resource temporarily unavailable",
        )),
    });
    assert_eq!(
        app.spares.parked_spawns.len(),
        1,
        "a spawn that failed counts"
    );
    assert_eq!(app.spare_book_check(t, &inputs), go("/usr"));
}

/// A parked spare persists across user absence while promotion to warm requires recent presence.
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
    let back = seen + PERSON_WINDOW + Duration::from_secs(60);
    assert_eq!(
        app.spare_book_check(back, &inputs),
        BookCheck::Go {
            window: WindowId(0),
            cwd: Some("/tmp".into())
        },
        "a parked spare is booked after the absence"
    );
    let id = app.next_session_id;
    app.spares.slot = Some(parked_slot(&app, id, Some("/tmp")));
    app.spares.slot.as_mut().unwrap().since = seen;
    app.spares.wanted = Some((WindowId(0), Some("/tmp".into()), seen));
    app.spares.power = || false;
    app.spares.shared.generation.store(9, Ordering::Release);
    app.spares.shared.eligible.store(true, Ordering::Release);
    assert_eq!(app.spare_promote_check(back), BookCheck::No("no-person"));
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

/// Window focus counts as user presence.
#[cfg(any(target_os = "macos", target_os = "linux"))]
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

/// `spawn_fingerprint` changes with shell-shaping fields and ignores terminal colors.
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

fn captured_link(id: u64) -> (Arc<SpareLink>, Arc<StdMutex<Vec<String>>>) {
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let post: crate::spawn::WakePost = Arc::new(move |wake| {
        sink.lock().unwrap().push(format!("{wake:?}"));
        true
    });
    (Arc::new(SpareLink::new(id, WindowId(0), Some(post))), seen)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn a_start_seen_of(kind: StartKind) -> StartSeen {
    a_start_seen(kind)
}

fn a_start_seen(kind: StartKind) -> StartSeen {
    StartSeen {
        at: Instant::now(),
        mark: StartMark {
            kind,
            rc_dir: Some("/h".into()),
            roots: None,
            flags: Some(aterm_core::terminal::StartFlags::default()),
        },
        input: None,
        fg_pgrp: -1,
        image: None,
        alt_screen: false,
    }
}

/// `SpareLink` swallows wakes before adoption and forwards re-stamped wakes once live.
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

/// A refused spare output post clears the output latch.
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

/// Pre-adoption host effects taint the spare without reaching the host.
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

/// Adopting a spare rebases its recording timestamps to the adoption instant.
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

/// `AdoptCheck` validates generation, folder, and live shell state before handoff.
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
    let generation = rc_generation(&inputs, &Reported::default());
    let link = Arc::new(SpareLink::new(3, WindowId(0), None));
    let session = crate::stub_session(3);
    let check = |cwd: Option<&str>, generation: Option<u64>| AdoptCheck {
        cwd: cwd.map(str::to_owned),
        generation: generation.map(|g| (inputs.clone(), g)),
        link: Arc::clone(&link),
        kill: false,
        retarget: false,
    };
    #[cfg(unix)]
    let parked = |cwd: &str| AdoptCheck {
        retarget: true,
        ..check(Some(cwd), None)
    };
    assert_eq!(
        check(None, Some(generation)).verdict(&session, None, None),
        Ok(None)
    );
    assert_eq!(
        check(Some("/tmp"), None).verdict(&session, Some("/tmp"), None),
        Ok(None)
    );
    std::fs::write(home.path().join(".zshrc"), "PS1='$ '\nalias x=y\n").unwrap();
    assert_eq!(
        check(None, Some(generation)).verdict(&session, None, None),
        Err("stale")
    );
    #[cfg(unix)]
    {
        let gone = "/nonexistent/aterm-spare";
        assert_eq!(
            check(Some(gone), None).verdict(&session, Some(gone), None),
            Err("folder")
        );
        assert_eq!(
            check(Some("/tmp"), None).verdict(&session, Some("/usr"), None),
            Err("folder")
        );
        assert_eq!(
            parked("/tmp").verdict(&session, Some(gone), None),
            Err("folder")
        );
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        assert_eq!(
            parked("/tmp").verdict(&session, Some("/usr"), None),
            Ok(Some("/usr".to_owned()))
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            parked("/usr").verdict(&session, Some("/tmp"), None),
            Ok(Some("/private/tmp".to_owned())),
            "through the root's link"
        );
        assert_eq!(
            check(None, None).verdict(&session, None, Some("/usr")),
            Ok(None)
        );
        assert_eq!(
            check(Some("/usr"), None).verdict(&session, None, Some("/usr")),
            Ok(None)
        );
        assert_eq!(
            parked("/tmp").verdict(&session, None, Some("/usr")),
            Ok(Some("/usr".to_owned()))
        );
        assert_eq!(
            AdoptCheck {
                retarget: true,
                ..check(None, None)
            }
            .verdict(&session, Some("/usr"), Some("/tmp")),
            Ok(Some("/usr".to_owned())),
            "a spare in the factory's folder, asked for another"
        );
    }
    link.decide(Wake::Exit {
        session: 3,
        window: WindowId(0),
    });
    assert_eq!(
        check(None, None).verdict(&session, None, None),
        Err("process")
    );
}

/// The event-loop backstop swallows wakes for unowned spares and passes all others.
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
    assert!(!app.spares.swallows(id + 1));
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
    app.retire_spare("test", None);
    assert!(app.spares.slot.is_none());
    assert!(app.spares.swallows(id), "a tombstone");
}

/// macOS escape scan detects an orphaned daemon left behind in the run window.
#[cfg(target_os = "macos")]
#[test]
fn the_escape_scan_catches_a_daemon_the_session_left_behind() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let (shell, sleep) = (fixture_shell(), fixture_sleep());
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
        .env(aterm_types::domain::ENV_PARENT_SESSION_ID, FIXTURE_SID)
        .current_dir(dir.path())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn");
    let mut owned = OwnedEscapeFixture {
        child: Some(child),
        daemon: None,
    };
    let shell_pid = owned.child.as_ref().unwrap().id() as i32;
    let shell_version = bsdinfo_uniq(shell_pid).map(|info| info.p_idversion);
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
    let caught = escape_scan(
        shell_pid,
        shell_version,
        "s-0123456789abcdef0123",
        born,
        born,
    );
    let window_before = escape_scan_among(
        shell_pid,
        "s-0123456789abcdef0123",
        std::time::UNIX_EPOCH,
        std::time::UNIX_EPOCH,
        |_| Some(vec![shell_pid, daemon_pid]),
        bsdinfo_uniq,
    );
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

/// `proc_pidinfo` decoder treats only a full zombie record as a completed exit.
#[cfg(target_os = "macos")]
#[test]
fn a_zombie_establishes_a_completed_exit_and_an_empty_answer_does_not() {
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
    assert!(
        !exited_observation(0, &zombie, 0),
        "a query that answered with no record proves no teardown: a reaped \
         pid is settled by a later listing, not by this read"
    );
    let exiting = record(2, 0x2000);
    assert!(
        !exited_observation(0, &exiting, exiting.len()),
        "beginning exit does not establish released resources"
    );
}

/// Incomplete escape scans retry up to `SCAN_ATTEMPTS` before returning `Unknown`.
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

/// An unreadable process born before the shell is ignored on the first listing.
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

/// A pid reaped mid-scan is settled by a follow-up listing.
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

/// A daemon read while its launcher was exiting is re-read so the orphan is caught.
#[cfg(target_os = "macos")]
#[test]
fn a_daemon_read_before_its_launcher_exited_is_read_again() {
    let sid = FIXTURE_SID;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let null = aterm_uds::spawnfd::Stdio::null;
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
    let launcher = fixture_command(&fixture_shell())
        .arg("-c")
        .arg(r#""$1" 600 & printf '%s\n' "$!" > "$2"; exec "$1" 600"#)
        .arg("launcher")
        .arg(fixture_sleep())
        .arg(&mark)
        .current_dir(dir.path())
        .process_group(0)
        .spawn()
        .expect("spawn the launcher");
    let launcher_pid = launcher.id() as i32;
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
        (Scan::Unknown, 2),
        "a stale record that cannot be read again, its pid still listed"
    );
    assert_eq!(read_live, Scan::Escaped, "the same tree read live");
}

#[cfg(target_os = "macos")]
const FIXTURE_SID: &str = "s-0123456789abcdef0123";

/// Ad-hoc signed shell and sleep stand-ins whose environment macOS allows reading.
#[cfg(target_os = "macos")]
fn fixture_shell() -> PathBuf {
    aterm_uds::deadline::stand_in(std::path::Path::new("/bin/bash"))
}

#[cfg(target_os = "macos")]
fn fixture_sleep() -> PathBuf {
    aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sleep"))
}

#[cfg(target_os = "macos")]
fn fixture_command(program: &std::path::Path) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .env(aterm_types::domain::ENV_PARENT_SESSION_ID, FIXTURE_SID)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    command
}

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

    fn gone(&self, pid: i32, birth: u64) {
        // SAFETY: `getuid` is a side-effect-free getter.
        let uid = unsafe { libc::getuid() };
        let reaped = || match bsdinfo_uniq(pid) {
            Some(info) => info.p_uniqueid != birth,
            None => !exited(pid) && uid_pids(uid).is_some_and(|pids| !pids.contains(&pid)),
        };
        while !reaped() {
            assert!(Instant::now() < self.give_up, "{pid} was never reaped");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(target_os = "macos")]
fn kill_birth(pid: i32, birth: u64) {
    if bsdinfo_uniq(pid).is_some_and(|i| i.p_uniqueid == birth) {
        // SAFETY: `pid` matches the captured birth.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

#[cfg(target_os = "macos")]
fn born_at(info: &BsdInfoWithUniqId) -> std::time::SystemTime {
    std::time::UNIX_EPOCH
        + Duration::from_secs(info.pbsd.pbi_start_tvsec)
        + Duration::from_micros(info.pbsd.pbi_start_tvusec)
}

#[cfg(target_os = "macos")]
fn session_shell(
    dir: &std::path::Path,
    waits: &FixtureWaits,
) -> (OwnedEscapeFixture, i32, i32, BsdInfoWithUniqId) {
    let mark = dir.join("child-pid");
    let shell = fixture_command(&fixture_shell())
        .arg("-c")
        .arg(r#""$1" 600 & printf '%s\n' "$!" > "$2"; wait; exec "$1" 600"#)
        .arg("shell")
        .arg(fixture_sleep())
        .arg(&mark)
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

#[cfg(target_os = "macos")]
fn adopted_orphan(
    dir: &std::path::Path,
    waits: &FixtureWaits,
) -> (OwnedEscapeFixture, i32, BsdInfoWithUniqId) {
    let mark = dir.join("orphan-pid");
    let launcher = fixture_command(&fixture_shell())
        .arg("-c")
        .arg(r#""$1" 600 & printf '%s\n' "$!" > "$2"; exec "$1" 600"#)
        .arg("launcher")
        .arg(fixture_sleep())
        .arg(&mark)
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

#[cfg(target_os = "macos")]
fn scan_with_first_read(shell_pid: i32, target: i32, record: BsdInfoWithUniqId) -> Scan {
    scan_with_first_read_since(shell_pid, None, target, record)
}

#[cfg(target_os = "macos")]
fn scan_with_first_read_since(
    shell_pid: i32,
    shell_version: Option<i32>,
    target: i32,
    record: BsdInfoWithUniqId,
) -> Scan {
    let mut first_read = true;
    let mut first_listing = true;
    escape_scan_since(
        shell_pid,
        shell_version,
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

/// Processes that exit before their session or environment is read do not count as escapes.
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

#[cfg(target_os = "macos")]
fn scan_reads(
    shell_pid: i32,
    window: &BsdInfoWithUniqId,
    listed: &[i32],
    later: &[i32],
    first: &[(i32, Option<BsdInfoWithUniqId>)],
    owner: Option<&dyn Fn(i32) -> Option<bool>>,
) -> Scan {
    let mut unread = first.to_vec();
    let mut first_listing = true;
    escape_scan_with_owner(
        shell_pid,
        None,
        "s-0123456789abcdef0123",
        born_at(window),
        born_at(window),
        |uid| {
            if std::mem::take(&mut first_listing) {
                Some(listed.to_vec())
            } else {
                uid_pids(uid).map(|pids| pids.into_iter().filter(|p| later.contains(p)).collect())
            }
        },
        |p| match unread.iter().position(|(q, _)| *q == p) {
            Some(at) => unread.remove(at).1,
            None => bsdinfo_uniq(p),
        },
        |pid, sid| match owner {
            Some(owner) => owner(pid),
            None => environment_names(pid, sid),
        },
        stdio_dead,
    )
}

#[cfg(target_os = "macos")]
fn reaped_pid() -> i32 {
    let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn a child to reap");
    child.wait().expect("reap it");
    child.id() as i32
}

#[cfg(target_os = "macos")]
fn record_as(info: &BsdInfoWithUniqId, pid: i32, ppid: i32, uniqueid: u64) -> BsdInfoWithUniqId {
    BsdInfoWithUniqId {
        pbsd: libc::proc_bsdinfo {
            pbi_pid: pid as u32,
            pbi_ppid: ppid as u32,
            ..info.pbsd
        },
        p_uniqueid: uniqueid,
        ..*info
    }
}

/// Double-fork daemons whose middle process exits mid-scan are re-read and caught.
#[cfg(target_os = "macos")]
#[test]
fn a_daemon_read_under_a_parent_gone_since_its_read_is_read_again() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let (_shell, shell_pid, child_pid, _) = session_shell(dir.path(), &waits);
    let (mut owned, daemon_pid, adopted) = adopted_orphan(dir.path(), &waits);
    let middle_pid = reaped_pid();
    let middle = record_as(&adopted, middle_pid, 1, adopted.p_uniqueid - 1);
    let stale = record_as(&adopted, daemon_pid, middle_pid, adopted.p_uniqueid);
    let ours = [shell_pid, daemon_pid, middle_pid];
    let straddle = [(daemon_pid, Some(stale)), (middle_pid, Some(middle))];
    let scan = |first: &[(i32, Option<BsdInfoWithUniqId>)]| {
        scan_reads(shell_pid, &adopted, &ours, &ours, first, None)
    };
    let straddled = scan(&straddle);
    let mut attempts = 0;
    let rescanned = rescan_while_unknown(SCAN_ATTEMPTS, || {
        attempts += 1;
        scan(if attempts == 1 { &straddle } else { &[] })
    });
    let replaced_middle = record_as(&adopted, child_pid, 1, adopted.p_uniqueid - 1);
    let replaced = scan_reads(
        shell_pid,
        &adopted,
        &[shell_pid, daemon_pid, child_pid],
        &[shell_pid, daemon_pid, child_pid],
        &[
            (
                daemon_pid,
                Some(record_as(
                    &adopted,
                    daemon_pid,
                    child_pid,
                    adopted.p_uniqueid,
                )),
            ),
            (child_pid, Some(replaced_middle)),
        ],
        None,
    );
    let piped_pid = reaped_pid();
    let piped = record_as(&adopted, piped_pid, middle_pid, adopted.p_uniqueid + 1);
    let pipeline = scan_reads(
        shell_pid,
        &adopted,
        &[shell_pid, middle_pid, piped_pid],
        &[shell_pid],
        &[(middle_pid, Some(middle)), (piped_pid, Some(piped))],
        None,
    );
    let unread = scan(&[
        (daemon_pid, Some(stale)),
        (middle_pid, Some(middle)),
        (daemon_pid, None),
    ]);
    let second_pid = reaped_pid();
    let second = record_as(&adopted, second_pid, middle_pid, adopted.p_uniqueid - 1);
    let first = record_as(&adopted, middle_pid, 1, adopted.p_uniqueid - 2);
    let triple = scan_reads(
        shell_pid,
        &adopted,
        &[shell_pid, middle_pid, second_pid, daemon_pid],
        &ours,
        &[
            (
                daemon_pid,
                Some(record_as(
                    &adopted,
                    daemon_pid,
                    second_pid,
                    adopted.p_uniqueid,
                )),
            ),
            (second_pid, Some(second)),
            (middle_pid, Some(first)),
        ],
        None,
    );
    let here = std::process::id() as i32;
    let me = bsdinfo_uniq(here).expect("this process's record");
    let stranger = scan_reads(
        shell_pid,
        &adopted,
        &[shell_pid, middle_pid, here],
        &[shell_pid, here],
        &[
            (middle_pid, Some(middle)),
            (here, Some(record_as(&me, here, middle_pid, me.p_uniqueid))),
        ],
        None,
    );
    owned
        .retire_owned_escape_fixture()
        .expect("retire the daemon");
    assert_eq!(straddled, Scan::Unknown, "the daemon read stale");
    assert_eq!(
        (rescanned, attempts),
        (Scan::Escaped, 2),
        "the rescan reads the daemon as launchd's"
    );
    assert_eq!(replaced, Scan::Unknown, "the middle's pid since replaced");
    assert_eq!(pipeline, Scan::Clean, "a middle whose child is gone too");
    assert_eq!(unread, Scan::Unknown, "a listed child read as nothing");
    assert_eq!(triple, Scan::Unknown, "the daemon below two gone middles");
    assert_eq!(
        stranger,
        Scan::Clean,
        "a live child in a session older than the shell"
    );
}

/// A child that exits together with its gone parent and leaves no live descendants is clean.
#[cfg(target_os = "macos")]
#[test]
fn a_child_gone_with_its_gone_parent_is_no_stale_record() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let (_shell, shell_pid, _, _) = session_shell(dir.path(), &waits);
    let (mut owned, daemon_pid, adopted) = adopted_orphan(dir.path(), &waits);
    let parent_pid = reaped_pid();
    let child_pid = reaped_pid();
    let child = record_as(&adopted, child_pid, parent_pid, adopted.p_uniqueid - 1);
    let alone = scan_reads(
        shell_pid,
        &adopted,
        &[shell_pid, parent_pid, child_pid],
        &[shell_pid],
        &[(parent_pid, None), (child_pid, Some(child))],
        None,
    );
    let below = scan_reads(
        shell_pid,
        &adopted,
        &[shell_pid, parent_pid, child_pid, daemon_pid],
        &[shell_pid, daemon_pid],
        &[
            (parent_pid, None),
            (child_pid, Some(child)),
            (
                daemon_pid,
                Some(record_as(
                    &adopted,
                    daemon_pid,
                    child_pid,
                    adopted.p_uniqueid,
                )),
            ),
        ],
        None,
    );
    owned
        .retire_owned_escape_fixture()
        .expect("retire the daemon");
    assert_eq!(alone, Scan::Clean, "a child gone with its gone parent");
    assert_eq!(
        below,
        Scan::Unknown,
        "the same child with a live process recorded below it"
    );
}

/// A daemon whose parent disappears before its environment read triggers a rescan.
#[cfg(target_os = "macos")]
#[test]
fn a_daemon_under_a_parent_gone_at_its_environment_read_is_read_again() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let (_shell, shell_pid, _, _) = session_shell(dir.path(), &waits);
    let (mut owned_daemon, daemon_pid, daemon) = adopted_orphan(dir.path(), &waits);
    let middle_dir = dir.path().join("middle");
    std::fs::create_dir(&middle_dir).expect("the middle's folder");
    let (mut owned_middle, middle_pid, middle) = adopted_orphan(&middle_dir, &waits);
    let window = record_as(&middle, middle_pid, 1, middle.p_uniqueid);
    let middle_record = BsdInfoWithUniqId {
        pbsd: libc::proc_bsdinfo {
            pbi_start_tvsec: daemon.pbsd.pbi_start_tvsec,
            pbi_start_tvusec: daemon.pbsd.pbi_start_tvusec,
            ..window.pbsd
        },
        ..window
    };
    let verdict = scan_reads(
        shell_pid,
        &daemon,
        &[shell_pid, daemon_pid, middle_pid],
        &[shell_pid, daemon_pid],
        &[
            (
                daemon_pid,
                Some(record_as(
                    &daemon,
                    daemon_pid,
                    middle_pid,
                    daemon.p_uniqueid,
                )),
            ),
            (middle_pid, Some(middle_record)),
            (middle_pid, Some(middle_record)),
            (middle_pid, None),
        ],
        Some(&|pid| (pid != middle_pid).then_some(false)),
    );
    owned_middle
        .retire_owned_escape_fixture()
        .expect("retire the middle");
    owned_daemon
        .retire_owned_escape_fixture()
        .expect("retire the daemon");
    assert_eq!(verdict, Scan::Unknown, "the daemon read stale");
}

/// A shell child whose parent exits before confirmation is treated as an incomplete scan.
#[cfg(target_os = "macos")]
#[test]
fn a_child_of_the_shell_under_a_parent_gone_since_its_read_is_read_again() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let (mut owned, shell_pid, child_pid, child) = session_shell(dir.path(), &waits);
    let parent_pid = reaped_pid();
    let before = BsdInfoWithUniqId {
        pbsd: libc::proc_bsdinfo {
            pbi_start_tvsec: child.pbsd.pbi_start_tvsec - 60,
            ..child.pbsd
        },
        ..child
    };
    let ours = [shell_pid, child_pid, parent_pid];
    let first = |parent: BsdInfoWithUniqId| {
        [
            (
                child_pid,
                Some(record_as(&child, child_pid, parent_pid, child.p_uniqueid)),
            ),
            (parent_pid, Some(parent)),
        ]
    };
    let left_before = record_as(&before, parent_pid, shell_pid, child.p_uniqueid - 1);
    let moved = scan_reads(shell_pid, &child, &ours, &ours, &first(left_before), None);
    let middle = record_as(&child, parent_pid, shell_pid, child.p_uniqueid - 1);
    let straddled = scan_reads(shell_pid, &child, &ours, &ours, &first(middle), None);
    let read_whole = scan_reads(shell_pid, &child, &[shell_pid, child_pid], &ours, &[], None);
    owned
        .retire_owned_escape_fixture()
        .expect("retire the shell");
    assert_eq!(moved, Scan::Unknown, "the same child under another parent");
    assert_eq!(straddled, Scan::Unknown, "the shell's own middle gone");
    assert_eq!(read_whole, Scan::Clean, "the control: the child as it is");
}

/// The escape scan traces ancestry through processes owned by other users.
#[cfg(target_os = "macos")]
#[test]
fn a_daemon_below_another_users_process_is_still_the_shells() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let older_dir = dir.path().join("older");
    std::fs::create_dir(&older_dir).expect("the older hop's folder");
    let (mut owned_older, older_pid, _) = adopted_orphan(&older_dir, &waits);
    let (_shell, shell_pid, child_pid, _) = session_shell(dir.path(), &waits);
    let (mut owned, daemon_pid, adopted) = adopted_orphan(dir.path(), &waits);
    let hop_dir = dir.path().join("hop");
    std::fs::create_dir(&hop_dir).expect("the hop's folder");
    let (mut owned_hop, hop_pid, _) = adopted_orphan(&hop_dir, &waits);
    let below = |parent: i32| {
        let record = Some(record_as(&adopted, daemon_pid, parent, adopted.p_uniqueid));
        let ours = [shell_pid, daemon_pid];
        let reads = [(daemon_pid, record), (daemon_pid, record)];
        scan_reads(shell_pid, &adopted, &ours, &ours, &reads, None)
    };
    let through = below(child_pid);
    let adopted_hop = below(hop_pid);
    let launchd = launchd_idversion().expect("launchd's pid version");
    let started = uid_pids(0)
        .expect("root's processes")
        .into_iter()
        .filter(|&p| p > 1 && short_parent(p) == Some(1))
        .filter_map(|p| uniq_identifier(p).map(|i| (p, i)))
        .filter(|(_, i)| i.p_orig_ppidversion == launchd)
        .min_by_key(|(_, i)| i.p_uniqueid)
        .expect("a daemon launchd started")
        .0;
    let boot_hop = below(started);
    let older_hop = below(older_pid);
    let unread = below(reaped_pid());
    owned_hop
        .retire_owned_escape_fixture()
        .expect("retire the hop");
    owned_older
        .retire_owned_escape_fixture()
        .expect("retire the older hop");
    owned
        .retire_owned_escape_fixture()
        .expect("retire the daemon");
    assert_eq!(
        through,
        Scan::Escaped,
        "through another user's live process"
    );
    assert_eq!(adopted_hop, Scan::Escaped, "below the run's orphan");
    assert_eq!(boot_hop, Scan::Clean, "below a daemon launchd started");
    assert_eq!(
        older_hop,
        Scan::Clean,
        "below an orphan older than the shell"
    );
    assert_eq!(unread, Scan::Clean, "below a parent gone: the line ends");
}

/// Escape scans wait until the grace window ends and rate-limit sample scans.
#[test]
fn a_scan_lists_after_its_window() {
    let mark = Instant::now();
    let mut last = None;
    let mut listed = None;
    let first = rate_limited(mark, &mut last, true, || {
        listed = Some(Instant::now());
        Scan::Unknown
    });
    let listed = listed.expect("the first scan runs");
    let mut again = false;
    let second = rate_limited(mark, &mut last, true, || {
        again = true;
        Scan::Unknown
    });
    assert_eq!(first, Scan::Unknown);
    assert!(listed >= mark + SCAN_GRACE, "listed inside the window");
    assert_eq!((second, again), (Scan::RateLimited, false), "rate-limited");
}

/// Warm verification scans are not rate-limited by prior scans.
#[test]
fn a_verification_is_not_spaced_from_the_scan_before_it() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let inputs = GenInputs {
        shell: aterm_core::shell_integration::ShellType::Zsh,
        shell_path: dir.path().join("shell"),
        home: dir.path().into(),
        launch_zdotdir: dir.path().into(),
        fingerprint: 1,
        containment: 1,
    };
    let generation = rc_generation(&inputs, &Reported::default());
    let witness = Arc::new(Witness::default());
    witness
        .shell
        .set((42, Some(PathBuf::from("/owned/shell"))))
        .unwrap();
    let mut seen = a_start_seen(StartKind::Spare);
    seen.fg_pgrp = 42;
    seen.image = Some(PathBuf::from("/owned/shell"));
    witness.start_seen.set(seen).unwrap();
    let job = VerifyJob {
        id: 94,
        witness,
        sid: "s-0123456789abcdef0123".into(),
        inputs,
        generation,
        run_at: Instant::now(),
        reply: Arc::new(|_| true),
    };
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut last = Some(Instant::now() + Duration::from_secs(3600));
        let verdict = verify(&job, &mut last, &Shared::default());
        let _ = tx.send((verdict, last));
    });
    let (verdict, last) = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the verification waited out the last scan's spacing");
    assert!(
        matches!(
            verdict,
            Verdict::Ok | Verdict::Backoff("escaped" | "scan-unknown")
        ),
        "it reached the scan: {verdict:?}"
    );
    assert!(
        last.is_some_and(|at| at <= Instant::now()),
        "its scan is the last one now"
    );
}

/// Processes started directly by `launchd` are not counted as run orphans.
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

/// Orphans whose launcher predates the shell pid version are not counted as escapes.
#[cfg(target_os = "macos")]
#[test]
fn an_orphan_whose_launcher_predates_the_shell_is_no_escape() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let (_shell, shell_pid, _, _) = session_shell(dir.path(), &waits);
    let version = bsdinfo_uniq(shell_pid)
        .expect("the shell stand-in")
        .p_idversion;
    let (mut owned, orphan_pid, adopted) = adopted_orphan(dir.path(), &waits);
    assert!(
        launched_before(version, adopted.p_orig_ppidversion),
        "the launcher was spawned after the shell"
    );
    let launched_by = |launcher: i32| BsdInfoWithUniqId {
        p_orig_ppidversion: launcher,
        ..adopted
    };
    let younger = scan_with_first_read_since(shell_pid, Some(version), orphan_pid, adopted);
    let by_the_shell =
        scan_with_first_read_since(shell_pid, Some(version), orphan_pid, launched_by(version));
    let older = scan_with_first_read_since(
        shell_pid,
        Some(version),
        orphan_pid,
        launched_by(version.wrapping_sub(1)),
    );
    let unknown = scan_with_first_read_since(
        shell_pid,
        None,
        orphan_pid,
        launched_by(version.wrapping_sub(1)),
    );
    owned
        .retire_owned_escape_fixture()
        .expect("retire the orphan");
    assert_eq!(younger, Scan::Escaped, "launched after the shell");
    assert_eq!(by_the_shell, Scan::Escaped, "launched by the shell");
    assert_eq!(older, Scan::Clean, "launched before the shell");
    assert_eq!(unknown, Scan::Escaped, "the shell's version unknown");
    assert!(
        launched_before(i32::MAX, i32::MIN),
        "older across the counter's wrap"
    );
    assert!(!launched_before(i32::MIN, i32::MAX));
}

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
        let leader = fixture_command(&fixture_shell())
            .arg("-c")
            .arg(
                r#"while [ ! -e "$3" ]; do "$1" 0.01; done
                ( "$1" 600 & printf '%s\n' "$!" > "$2" )
                exec "$1" 600"#,
            )
            .arg(name)
            .arg(fixture_sleep())
            .arg(&mark)
            .arg(&go)
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

/// Orphans from sessions or leaders predating the shell are not counted as escapes.
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

#[cfg(target_os = "macos")]
const PROC_FLAG_EXEC: u32 = 0x4000;

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum OrphanExec {
    WhileLed,
    AfterLeader,
}

#[cfg(target_os = "macos")]
struct TerminalSession {
    leader: Option<i32>,
    master: i32,
    orphan: OwnedEscapeFixture,
    go: std::path::PathBuf,
}

#[cfg(target_os = "macos")]
impl TerminalSession {
    fn new(
        dir: &std::path::Path,
        waits: &FixtureWaits,
        exec: OrphanExec,
    ) -> (Self, i32, BsdInfoWithUniqId) {
        let mark = dir.join("terminal-orphan-pid");
        let go = dir.join("terminal-orphan-go");
        let script = match exec {
            OrphanExec::WhileLed => {
                r#"trap '' HUP; ( "$1" 600 & printf '%s\n' "$!" > "$2" ); exec "$1" 600"#
            }
            OrphanExec::AfterLeader => {
                r#"trap '' HUP; ( ( read -r _ < "$3"; exec "$1" 600 ) & printf '%s\n' "$!" > "$2" ); exec "$1" 600"#
            }
        };
        if exec == OrphanExec::AfterLeader {
            let c = std::ffi::CString::new(go.as_os_str().as_encoded_bytes()).expect("a path");
            // SAFETY: `c` is a NUL-terminated path that lives across the call.
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "the go FIFO");
        }
        let argv: Vec<std::ffi::OsString> = vec![
            fixture_shell().into_os_string(),
            "-c".into(),
            script.into(),
            "leader".into(),
            fixture_sleep().into_os_string(),
            mark.clone().into_os_string(),
            go.clone().into_os_string(),
        ];
        // SAFETY: test process; the trusted-launcher contract holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let spawned = aterm_pty::spawn_shell_with_pid_cell_px(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[(
                aterm_types::domain::ENV_PARENT_SESSION_ID.to_owned(),
                FIXTURE_SID.to_owned(),
            )],
            None,
            None,
            None,
            Some(&argv),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
            None,
        )
        .expect("spawn the terminal's leader");
        let mut session = Self {
            leader: Some(spawned.pid),
            master: spawned.master,
            orphan: OwnedEscapeFixture {
                child: None,
                daemon: None,
            },
            go,
        };
        let pid = waits.pid_in(&mark);
        let birth = bsdinfo_uniq(pid).expect("the orphan-to-be").p_uniqueid;
        session.orphan.daemon = Some((pid, birth));
        let adopted = session.orphan_once(waits, |info| {
            info.pbsd.pbi_ppid == 1
                && (exec == OrphanExec::AfterLeader || info.pbsd.pbi_flags & PROC_FLAG_EXEC != 0)
        });
        (session, pid, adopted)
    }

    fn orphan_once(
        &self,
        waits: &FixtureWaits,
        holds: impl Fn(&BsdInfoWithUniqId) -> bool,
    ) -> BsdInfoWithUniqId {
        let (pid, birth) = self.orphan.daemon.expect("the orphan's birth");
        loop {
            if let Some(info) = bsdinfo_uniq(pid)
                && info.p_uniqueid == birth
                && holds(&info)
            {
                return info;
            }
            assert!(
                Instant::now() < waits.give_up,
                "the terminal's orphan never reached the state waited for"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn exec_now(&self, waits: &FixtureWaits) -> BsdInfoWithUniqId {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut go = loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.go)
            {
                Ok(go) => break go,
                Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {}
                Err(e) => panic!("open the orphan's go: {e}"),
            }
            assert!(
                Instant::now() < waits.give_up,
                "the terminal's orphan never opened its go"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        go.write_all(b"go\n").expect("tell the orphan to exec");
        drop(go);
        self.orphan_once(waits, |info| info.pbsd.pbi_flags & PROC_FLAG_EXEC != 0)
    }

    fn end_leader(&mut self) {
        if let Some(leader) = self.leader.take() {
            // SAFETY: this process's own unreaped child, so the pid is still it.
            unsafe { libc::kill(leader, libc::SIGKILL) };
            let mut status = 0;
            // SAFETY: reaps `leader`.
            let reaped = unsafe { libc::waitpid(leader, &mut status, 0) };
            assert_eq!(reaped, leader, "reap the terminal's leader");
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.end_leader();
        aterm_pty::close_fd(self.master);
    }
}

/// Orphans retaining a controlling terminal from a foreign session are not counted as escapes.
#[cfg(target_os = "macos")]
#[test]
fn an_orphan_born_on_another_terminal_is_no_escape() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let (_shell, shell_pid, _, _) = session_shell(dir.path(), &waits);
    let version = bsdinfo_uniq(shell_pid)
        .expect("the shell stand-in")
        .p_idversion;
    let (mut terminal, orphan, adopted) =
        TerminalSession::new(dir.path(), &waits, OrphanExec::WhileLed);
    let leader = terminal.leader.expect("the leader lives");
    let scan_with = |listed: &[i32],
                     read: &dyn Fn(i32) -> Option<BsdInfoWithUniqId>,
                     stdio: &dyn Fn(i32) -> Option<bool>| {
        escape_scan_with_owner(
            shell_pid,
            Some(version),
            FIXTURE_SID,
            born_at(&adopted),
            born_at(&adopted),
            |_| Some(listed.to_vec()),
            read,
            |_, _| Some(true),
            stdio,
        )
    };
    let scan = |listed: &[i32], read: &dyn Fn(i32) -> Option<BsdInfoWithUniqId>| {
        scan_with(listed, read, &stdio_dead)
    };
    let leader_under_the_shell = scan(&[shell_pid, orphan, leader], &|p| {
        bsdinfo_uniq(p).map(|mut i| {
            if p == leader {
                i.pbsd.pbi_ppid = shell_pid as u32;
            }
            i
        })
    });
    terminal.end_leader();
    // SAFETY: `getsid` reads the process table.
    let sessions = unsafe { (libc::getsid(orphan), libc::getsid(leader)) };
    let leader_gone = scan(&[shell_pid, orphan], &bsdinfo_uniq);
    let born_on_none = scan_with(
        &[shell_pid, orphan],
        &|p| {
            bsdinfo_uniq(p).map(|mut i| {
                if p == orphan {
                    i.pbsd.pbi_flags &= !PROC_FLAG_CONTROLT;
                }
                i
            })
        },
        &|_| Some(false),
    );
    let still = bsdinfo_uniq(orphan);
    terminal
        .orphan
        .retire_owned_escape_fixture()
        .expect("retire the orphan");
    assert!(
        adopted.pbsd.pbi_flags & PROC_FLAG_CONTROLT != 0,
        "born on the terminal: {:#x}",
        adopted.pbsd.pbi_flags
    );
    assert_eq!(
        sessions,
        (leader, -1),
        "the orphan stays in the session its gone leader led"
    );
    assert!(
        still.is_some_and(|i| i.pbsd.pbi_ppid == 1 && i.pbsd.pbi_flags & PROC_FLAG_CONTROLT != 0),
        "adopted, its terminal flag kept through its leader's exit"
    );
    assert_eq!(
        leader_under_the_shell,
        Scan::Escaped,
        "an orphan of a terminal the run opened, its leader under the shell"
    );
    assert_eq!(
        leader_gone,
        Scan::Clean,
        "an orphan born on another terminal, its leader gone"
    );
    assert_eq!(
        born_on_none,
        Scan::Escaped,
        "the same orphan born on no terminal (the run's `setsid` daemon)"
    );
}

/// Dead stdio without a controlling terminal or foreign leader does not clear an orphan.
#[cfg(target_os = "macos")]
#[test]
fn a_dead_stdio_orphan_is_not_cleared_without_terminal_origin() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let waits = FixtureWaits::new();
    let (_shell, shell_pid, _, _) = session_shell(dir.path(), &waits);
    let version = bsdinfo_uniq(shell_pid)
        .expect("the shell stand-in")
        .p_idversion;
    let (mut terminal, orphan, adopted) =
        TerminalSession::new(dir.path(), &waits, OrphanExec::AfterLeader);
    let leader = terminal.leader.expect("the leader lives");
    terminal.end_leader();
    let execd = terminal.exec_now(&waits);
    let scan = |stdio: &dyn Fn(i32) -> Option<bool>| {
        escape_scan_with_owner(
            shell_pid,
            Some(version),
            FIXTURE_SID,
            born_at(&execd),
            born_at(&execd),
            |_| Some(vec![shell_pid, orphan]),
            bsdinfo_uniq,
            |_, _| Some(true),
            stdio,
        )
    };
    let revoked = scan(&stdio_dead);
    let live = scan(&|_| Some(false));
    let unread = scan(&|_| None);
    let read = stdio_dead(orphan);
    let shell_read = stdio_dead(shell_pid);
    // SAFETY: `getsid` reads the process table.
    let session = unsafe { libc::getsid(orphan) };
    terminal
        .orphan
        .retire_owned_escape_fixture()
        .expect("retire the orphan");
    assert!(
        adopted.pbsd.pbi_flags & PROC_FLAG_CONTROLT != 0,
        "born on the terminal: {:#x}",
        adopted.pbsd.pbi_flags
    );
    assert_eq!(
        (
            execd.pbsd.pbi_ppid,
            execd.pbsd.pbi_flags & (PROC_FLAG_CONTROLT | PROC_FLAG_EXEC),
            session,
        ),
        (1, PROC_FLAG_EXEC, leader),
        "adopted, exec'd after its leader left, its terminal flag gone ({:#x}), \
         in the session its leader led",
        execd.pbsd.pbi_flags
    );
    assert_eq!(read, Some(true), "its stdio on the revoked terminal");
    assert_eq!(
        shell_read,
        Some(false),
        "the shell's stdio, `/dev/null`, is live"
    );
    assert_eq!(
        revoked,
        Scan::Escaped,
        "a dead vnode alone cannot prove this owned-or-unreadable orphan foreign"
    );
    assert_eq!(
        live,
        Scan::Escaped,
        "the same orphan with its stdio live (the run's `setsid` daemon)"
    );
    assert_eq!(unread, Scan::Escaped, "the same orphan, its stdio unread");
}

/// A missing process identity during late confirmation keeps the scan `Unknown`.
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
            None,
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
            stdio_dead,
        )
    };
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
            // SAFETY: `pid` matches the fixture's captured process birth.
            if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    errors.push(format!("kill the captured daemon: {error}"));
                }
            }
        }
        if let Some(child) = self.child.as_mut() {
            // SAFETY: `child` is unreaped and still owns its process group.
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

/// Applying an update retires the spare, and spare lifecycle events do not count as user activity.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_update_retires_the_spare_and_a_spare_is_never_activity() {
    let mut app = App::headless_for_test();
    let id = 50;
    let mut slot = parked_slot(&app, id, None);
    slot.state = SlotState::Booked;
    slot.inputs.shell = aterm_core::shell_integration::ShellType::Zsh;
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

/// Promotion to warm requires a fresh `Job::Freshen` stamp of the rc generation.
#[cfg(any(target_os = "macos", target_os = "linux"))]
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

    app.spare_promote(at);
    let n = asked(&app).expect("a freshen asked");
    assert_eq!(state(&app), Some(SlotState::Parked));
    assert_eq!(read(), "", "no `run` before a fresh stamp");
    app.spare_promote(at);
    assert_eq!(asked(&app), Some(n));
    assert_eq!(read(), "");
    shared.generation.store(8, Ordering::Release);
    shared.eligible.store(false, Ordering::Release);
    shared.fresh.store(n, Ordering::Release);
    app.spare_promote(at);
    assert_eq!(state(&app), Some(SlotState::Parked));
    assert_eq!(read(), "", "the edited generation never runs unseen");
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

/// A spawned spare that misses `PARKED_DEADLINE` is retired and disables parked spares for that shell.
#[test]
fn a_spawned_spare_unparked_at_its_deadline_is_retired_and_a_younger_one_kept() {
    let mut app = App::headless_for_test();
    let mut slot = parked_slot(&app, 61, None);
    slot.state = SlotState::Spawned;
    let (since, key) = (slot.since, parked_key(&slot.inputs));
    app.spares.slot = Some(slot);
    app.spares.captured = Some(Vec::new());
    app.spares.tick_at.set(None);

    app.spare_age(
        since + PARKED_DEADLINE - Duration::from_millis(1),
        SpareMode::Auto,
    );
    assert_eq!(
        app.spares.slot.as_ref().map(|s| s.state),
        Some(SlotState::Spawned),
        "a millisecond short of the deadline it is still spawning"
    );
    assert_eq!(app.spares.parked_failed, None);
    assert_eq!(
        app.spares.tick_at.get(),
        Some(since + PARKED_DEADLINE),
        "the tick is armed for the deadline"
    );

    app.spare_age(since + PARKED_DEADLINE, SpareMode::Auto);
    assert!(app.spares.slot.is_none(), "no parked mark: retired");
    assert_eq!(app.spares.parked_failed, Some(key), "and spares are off");
    assert_eq!(app.spares.discards, vec![("no_park", 1)]);
}

/// A start record arriving while `Booked` retires a parked-capable spare.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_start_record_before_the_spawn_lands_retires_the_spare() {
    let mut app = App::headless_for_test();
    let id = 51;
    let mut slot = parked_slot(&app, id, None);
    slot.state = SlotState::Booked;
    slot.session = None;
    slot.inputs.shell = aterm_core::shell_integration::ShellType::Zsh;
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

/// A fish spare lands in `Warming` and verifies from its booking timestamp.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_fish_spare_lands_warming_and_judges_an_early_start_record() {
    for (shell, warm) in [
        (aterm_core::shell_integration::ShellType::Fish, true),
        (aterm_core::shell_integration::ShellType::Zsh, false),
    ] {
        let mut app = App::headless_for_test();
        let id = 52;
        let mut slot = parked_slot(&app, id, None);
        slot.state = SlotState::Booked;
        slot.session = None;
        slot.inputs.shell = shell;
        let booked = Instant::now() - Duration::from_millis(250);
        slot.booked = booked;
        slot.since = booked;
        let link = Arc::clone(&slot.link);
        app.spares.slot = Some(slot);
        app.spares.captured = Some(Vec::new());
        let _ = link.witness.start_seen.set(a_start_seen(StartKind::Spare));
        app.on_spare_spawned(SpareSpawned {
            id,
            session: Ok(crate::stub_session(id)),
        });
        if warm {
            assert_eq!(
                app.spares.slot.as_ref().map(|s| s.state),
                Some(SlotState::Verifying),
                "{shell:?}: landed Warming, its record judged at once"
            );
            assert_eq!(app.spares.parked_failed, None);
            let run_at = app.spares.captured.as_ref().and_then(|jobs| {
                jobs.iter().find_map(|job| match job {
                    Job::Verify(v) => Some(v.run_at),
                    _ => None,
                })
            });
            assert_eq!(
                run_at,
                Some(booked),
                "{shell:?}: the escape scan covers the run from its booking"
            );
        } else {
            assert!(app.spares.slot.is_none(), "{shell:?}: ran past its hold");
            assert!(app.spares.parked_failed.is_some());
        }
    }
}

/// Adopting a spare re-arms the PTY input watch.
#[cfg(target_os = "macos")]
#[test]
fn an_adopted_spares_input_watch_is_armed_again() {
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
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
    sink.write_frame_nonparking(b"\x1b[?1;2c").expect("reply");
    assert_eq!(watches(), 0, "swallowed while it is nobody's");
    sink.write_frame_nonparking(b"x").expect("a second write");
    assert_eq!(watches(), 0, "fired once: the watch waits for a re-arm");
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
        retarget: Arc::default(),
    });
    app.spares.captured = Some(Vec::new());
    app.adopt_live(id, wid);
    assert_eq!(
        watches(),
        1,
        "re-armed and woken for the bytes still unread"
    );
    // SAFETY: both fds belong to this test's `raw_pty_pair`.
    unsafe {
        libc::close(slave);
    }
}

/// `rc_generation` changes with startup files and hooks, and ignores history files.
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
    let g0 = rc_generation(&inputs, &Reported::default());
    assert_eq!(
        rc_generation(&inputs, &Reported::default()),
        g0,
        "an untouched tree"
    );
    std::fs::write(h.join(".zsh_history"), ": 1:0;ls\n").unwrap();
    assert_eq!(
        rc_generation(&inputs, &Reported::default()),
        g0,
        "history moves nothing"
    );
    let dir_mtime = std::fs::metadata(&hooks).unwrap().modified().unwrap();
    std::fs::write(hooks.join("path.zsh"), "export PATH=/a:/b:$PATH\n").unwrap();
    assert_eq!(
        std::fs::metadata(&hooks).unwrap().modified().unwrap(),
        dir_mtime,
        "an in-place edit leaves the folder's mtime"
    );
    let g1 = rc_generation(&inputs, &Reported::default());
    assert_ne!(g1, g0, "a hook edited in place moves it");
    std::fs::write(hooks.join("more.zsh"), "alias l=ls\n").unwrap();
    let g2 = rc_generation(&inputs, &Reported::default());
    assert_ne!(g2, g1, "a hook added moves it");
    let z = h.join("zdot");
    std::fs::create_dir_all(&z).unwrap();
    std::fs::write(z.join(".zshrc"), "PS1='% '\n").unwrap();
    let in_z_dir = Reported {
        rc_dir: Some(z.clone()),
        roots: None,
    };
    let in_z = rc_generation(&inputs, &in_z_dir);
    assert_ne!(in_z, g2, "the final rc folder is part of it");
    std::fs::write(z.join(".zshrc"), "PS1='%% '\nalias x=y\n").unwrap();
    assert_ne!(
        rc_generation(&inputs, &in_z_dir),
        in_z,
        "and its files are stamped"
    );
}

/// Fish `rc_generation` stamps the startup roots reported by the start mark.
#[test]
fn a_fish_generation_stamps_the_startup_roots_its_record_names() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let r = root.path();
    let user = r.join("cfg").join("fish");
    let sysconf = r.join("sysconf");
    let inherited = r.join("x d;s").join("fish").join("vendor_conf.d");
    let own = r.join("data").join("fish").join("vendor_conf.d");
    let guessed = r
        .join("prefix")
        .join("share")
        .join("fish")
        .join("vendor_conf.d");
    for dir in [
        user.join("conf.d"),
        sysconf.join("conf.d"),
        inherited.clone(),
        own.clone(),
        guessed.clone(),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(own.join("tool.fish"), "set -g a 1\n").unwrap();
    let inputs = GenInputs {
        shell: aterm_core::shell_integration::ShellType::Fish,
        shell_path: r.join("prefix").join("bin").join("fish"),
        home: r.join("home"),
        launch_zdotdir: r.join("home"),
        fingerprint: 1,
        containment: 2,
    };
    let reported = Reported {
        rc_dir: Some(user.clone()),
        roots: Some(vec![sysconf.clone(), own.clone(), inherited.clone()]),
    };
    let g = || rc_generation(&inputs, &reported);
    let g0 = g();
    assert_eq!(g(), g0, "an untouched tree");
    assert_ne!(
        rc_generation(
            &inputs,
            &Reported {
                roots: None,
                ..reported.clone()
            }
        ),
        g0,
        "no roots named is another generation"
    );
    std::fs::write(guessed.join("unread.fish"), "set -g b 1\n").unwrap();
    assert_eq!(g(), g0, "a folder no record named moves nothing");
    let mut last = g0;
    let mut moved = |what: &str| {
        let now = g();
        assert_ne!(now, last, "{what} moves it");
        last = now;
    };
    std::fs::write(inherited.join("late.fish"), "set -g c 1\n").unwrap();
    moved("a file added to an inherited vendor conf.d");
    std::fs::write(sysconf.join("conf.d").join("late.fish"), "set -g d 1\n").unwrap();
    moved("a file added to the sysconf conf.d");
    std::fs::write(sysconf.join("config.fish"), "set -g e 1\n").unwrap();
    moved("a sysconf config.fish written");
    let dir_mtime = std::fs::metadata(&own).unwrap().modified().unwrap();
    std::fs::write(own.join("tool.fish"), "set -g a 1; set -g f 2\n").unwrap();
    assert_eq!(
        std::fs::metadata(&own).unwrap().modified().unwrap(),
        dir_mtime
    );
    moved("a vendor entry edited in place");
    std::fs::write(user.join("conf.d").join("mine.fish"), "set -g g 1\n").unwrap();
    moved("a file added to the user's conf.d");
    std::fs::create_dir_all(user.join("themes")).unwrap();
    std::fs::write(
        user.join("themes").join("default.theme"),
        "fish_color_command blue\n",
    )
    .unwrap();
    moved("a default.theme added to the user's themes folder");
}

/// A fish visible run without reported startup roots is marked unclean.
#[test]
fn a_fish_run_whose_record_names_no_roots_teaches_nothing_clean() {
    use aterm_core::shell_integration::ShellType;
    let session = crate::stub_session(92);
    let input = session.ctx.sink.driven_input();
    let now = Instant::now();
    let judged = |shell: ShellType, roots: Option<Vec<String>>| {
        let witness = Arc::new(Witness::default());
        witness
            .shell
            .set((42, Some(PathBuf::from("/owned/shell"))))
            .unwrap();
        let obs = Observation {
            witness,
            sid: "s-0123456789abcdef0123".into(),
            started_at: now,
            input0: Some(input),
            host_effects0: 0,
            early_control0: 0,
            inputs: GenInputs {
                shell,
                shell_path: PathBuf::new(),
                home: PathBuf::new(),
                launch_zdotdir: PathBuf::new(),
                fingerprint: 1,
                containment: 1,
            },
        };
        let mut seen = a_start_seen(StartKind::Cold);
        seen.mark.roots = roots;
        seen.at = now;
        seen.input = Some(input);
        seen.fg_pgrp = 42;
        seen.image = Some(PathBuf::from("/owned/shell"));
        judge(&obs, &seen, &mut Some(now + Duration::from_secs(3600)))
    };
    assert_eq!(judged(ShellType::Fish, None), Sample::Unclean);
    assert_eq!(
        judged(ShellType::Fish, Some(vec!["/etc/fish".into()])),
        Sample::Inconclusive
    );
    assert_eq!(judged(ShellType::Zsh, None), Sample::Inconclusive);
}

/// Warm fish verification rejects start marks whose roots differ from the stamped generation.
#[test]
fn a_fish_spare_holds_only_for_the_roots_its_generation_stamped() {
    use aterm_core::shell_integration::ShellType;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let stamped = vec![PathBuf::from("/etc/fish"), dir.path().join("vendor_conf.d")];
    let verdict = |shell: ShellType, roots: Option<Vec<PathBuf>>| {
        let witness = Arc::new(Witness::default());
        witness
            .shell
            .set((42, Some(PathBuf::from("/owned/shell"))))
            .unwrap();
        let mut seen = a_start_seen(StartKind::Spare);
        seen.mark.roots = roots.map(|r| r.iter().map(|p| p.display().to_string()).collect());
        seen.fg_pgrp = 42;
        seen.image = Some(PathBuf::from("/owned/shell"));
        witness.start_seen.set(seen).unwrap();
        let shared = Shared::default();
        *shared.reported.lock().unwrap() = Reported {
            rc_dir: None,
            roots: Some(stamped.clone()),
        };
        verify(
            &VerifyJob {
                id: 93,
                witness,
                sid: "s-0123456789abcdef0123".into(),
                inputs: GenInputs {
                    shell,
                    shell_path: dir.path().join("fish"),
                    home: dir.path().into(),
                    launch_zdotdir: dir.path().into(),
                    fingerprint: 1,
                    containment: 1,
                },
                generation: 0,
                run_at: Instant::now(),
                reply: Arc::new(|_| true),
            },
            &mut None,
            &shared,
        )
    };
    assert_eq!(
        verdict(ShellType::Fish, Some(vec![PathBuf::from("/etc/fish")])),
        Verdict::Backoff("roots")
    );
    assert_eq!(verdict(ShellType::Fish, None), Verdict::Backoff("roots"));
    assert_eq!(
        verdict(ShellType::Fish, Some(stamped.clone())),
        Verdict::Backoff("stale")
    );
    assert_eq!(verdict(ShellType::Zsh, None), Verdict::Backoff("stale"));
}

/// Malformed gate snapshots are rejected.
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

/// Oversized gate snapshots are rejected.
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

/// Gate persistence refuses to overwrite an existing staging file.
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

/// An unreadable shell process record makes the escape scan `Unknown`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_unreadable_shell_is_not_a_clean_escape_scan() {
    assert_ne!(
        escape_scan(
            -1,
            None,
            "s-0123456789abcdef0123",
            std::time::UNIX_EPOCH,
            std::time::UNIX_EPOCH
        ),
        Scan::Clean,
        "missing shell identity is not a clean scan"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct FixtureProcs {
    listing: Vec<i32>,
    records: Box<dyn Fn(i32, usize) -> Option<ProcRecord>>,
    owners: std::collections::HashMap<i32, Option<bool>>,
    pauses: usize,
    read_errors: std::collections::HashMap<(i32, usize), std::io::ErrorKind>,
    reads: std::collections::HashMap<i32, usize>,
    list_error: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ProcTable for FixtureProcs {
    fn list(&mut self) -> Option<Vec<i32>> {
        (!self.list_error).then(|| self.listing.clone())
    }

    fn read(&mut self, pid: i32) -> std::io::Result<Option<ProcRecord>> {
        let read = self.reads.entry(pid).or_default();
        *read += 1;
        if let Some(kind) = self.read_errors.get(&(pid, *read)) {
            return Err(std::io::Error::from(*kind));
        }
        Ok((self.records)(pid, self.pauses))
    }

    fn owner(&mut self, pid: i32, _sid: &str) -> Option<bool> {
        self.owners.get(&pid).copied().flatten()
    }

    fn pause(&mut self, _pause: Duration) {
        self.pauses += 1;
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn proc_at(ppid: i32, session: i32, start: u64) -> ProcRecord {
    ProcRecord {
        ppid,
        session,
        start,
        ended: false,
        exiting: false,
        threads: 1,
        uid: 501,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn scan_fixture(others: Vec<(i32, ProcRecord)>, owners: &[(i32, Option<bool>)]) -> (Scan, usize) {
    let mut table = FixtureProcs {
        read_errors: std::collections::HashMap::new(),
        reads: std::collections::HashMap::new(),
        list_error: false,
        listing: std::iter::once(10)
            .chain(others.iter().map(|(p, _)| *p))
            .collect(),
        records: Box::new(move |pid, _| {
            if pid == 10 {
                return Some(proc_at(9, 10, 100));
            }
            others.iter().find(|(p, _)| *p == pid).map(|(_, r)| *r)
        }),
        owners: owners.iter().copied().collect(),
        pauses: 0,
    };
    let verdict = linux_escape_scan(
        &mut table,
        10,
        "s-0123456789abcdef0123",
        (100, 200),
        &[1, 5],
        501,
    );
    (verdict, table.pauses)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ReadsProcs {
    listing: Vec<i32>,
    reads: std::collections::HashMap<i32, Vec<Option<ProcRecord>>>,
    owners: std::collections::HashMap<i32, Option<bool>>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ProcTable for ReadsProcs {
    fn list(&mut self) -> Option<Vec<i32>> {
        Some(self.listing.clone())
    }

    fn read(&mut self, pid: i32) -> std::io::Result<Option<ProcRecord>> {
        Ok(match self.reads.get_mut(&pid) {
            Some(seq) if seq.len() > 1 => seq.remove(0),
            Some(seq) => seq.first().copied().flatten(),
            None => None,
        })
    }

    fn owner(&mut self, pid: i32, _sid: &str) -> Option<bool> {
        self.owners.get(&pid).copied().flatten()
    }

    fn pause(&mut self, _pause: Duration) {}
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn scan_linux_reads(
    procs: Vec<(i32, Vec<Option<ProcRecord>>)>,
    owners: &[(i32, Option<bool>)],
) -> Scan {
    let mut table = ReadsProcs {
        listing: std::iter::once(10)
            .chain(procs.iter().map(|(p, _)| *p))
            .collect(),
        reads: std::iter::once((10, vec![Some(proc_at(9, 10, 100))]))
            .chain(procs)
            .collect(),
        owners: owners.iter().copied().collect(),
    };
    linux_escape_scan(
        &mut table,
        10,
        "s-0123456789abcdef0123",
        (100, 200),
        &[1, 5],
        501,
    )
}

/// Linux escape scan re-reads parent records to catch daemons whose parent exited mid-scan.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_linux_scan_reads_each_parent_again() {
    let subshell = proc_at(10, 10, 150);
    let gone_after_first = vec![Some(subshell), None];
    let daemon = |ppid: i32| proc_at(ppid, 10, 151);
    let adopted = vec![Some(daemon(20)), Some(daemon(1))];
    assert_eq!(
        scan_linux_reads(vec![(20, gone_after_first), (30, adopted.clone())], &[]),
        Scan::Escaped,
        "the daemon of a subshell gone since its read"
    );
    assert_eq!(
        scan_linux_reads(
            vec![(20, vec![Some(subshell)]), (30, vec![Some(daemon(20))])],
            &[]
        ),
        Scan::Clean,
        "the control: the subshell alive, the daemon still under the shell"
    );
    let orphan = proc_at(1, 20, 150);
    let child = proc_at(20, 20, 151);
    let orphan_reads = || vec![Some(orphan), Some(orphan), None];
    assert_eq!(
        scan_linux_reads(
            vec![(20, orphan_reads()), (30, vec![Some(child)])],
            &[(20, None)]
        ),
        Scan::Unknown,
        "a live child of a candidate gone at its environment read"
    );
    assert_eq!(
        scan_linux_reads(
            vec![(20, orphan_reads()), (30, vec![Some(child), None])],
            &[(20, None)]
        ),
        Scan::Clean,
        "a child the candidate reaped before it exited"
    );
    assert_eq!(
        scan_linux_reads(
            vec![
                (20, orphan_reads()),
                (30, vec![Some(child), Some(child), None]),
                (40, vec![Some(proc_at(30, 20, 152))]),
            ],
            &[(20, None)]
        ),
        Scan::Unknown,
        "a live process below a gone child of the candidate"
    );
    let reborn = vec![Some(subshell), Some(proc_at(10, 10, 160))];
    assert_eq!(
        scan_linux_reads(vec![(20, reborn), (30, adopted.clone())], &[]),
        Scan::Escaped,
        "the daemon of a subshell whose pid is another's now"
    );
    assert_eq!(
        scan_linux_reads(
            vec![
                (
                    20,
                    vec![
                        Some(ProcRecord {
                            exiting: true,
                            ..proc_at(10, 10, 150)
                        }),
                        None
                    ]
                ),
                (25, vec![Some(proc_at(20, 25, 151)), None]),
                (
                    30,
                    vec![Some(proc_at(25, 25, 152)), Some(proc_at(1, 25, 152))]
                ),
            ],
            &[(30, Some(true))]
        ),
        Scan::Escaped,
        "the daemon of a double fork whose middle the settle found reaped"
    );
    assert_eq!(
        scan_linux_reads(
            vec![(30, vec![Some(proc_at(20, 30, 150))])],
            &[(30, Some(true))]
        ),
        Scan::Clean,
        "below a parent the listing never named"
    );
}

/// Linux escape scan traces ancestry across other users' processes.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_linux_scan_walks_through_another_users_process() {
    let root = |ppid: i32, session: i32, start: u64| ProcRecord {
        uid: 0,
        ..proc_at(ppid, session, start)
    };
    let daemon = proc_at(20, 30, 150);
    let sudo_under = |ppid: i32, start: u64| {
        vec![
            (20, vec![Some(root(ppid, 10, start))]),
            (30, vec![Some(daemon)]),
        ]
    };
    assert_eq!(
        scan_linux_reads(sudo_under(10, 140), &[(30, Some(false))]),
        Scan::Escaped,
        "below a live sudo under the shell, out of its session"
    );
    assert_eq!(
        scan_linux_reads(sudo_under(1, 140), &[(30, Some(true))]),
        Scan::Escaped,
        "below a sudo the run left to init"
    );
    assert_eq!(
        scan_linux_reads(sudo_under(1, 50), &[(30, Some(true))]),
        Scan::Clean,
        "below a sudo older than the run"
    );
    let own_session = || {
        vec![
            (20, vec![Some(root(1, 20, 140))]),
            (30, vec![Some(proc_at(20, 20, 150))]),
        ]
    };
    assert_eq!(
        scan_linux_reads(own_session(), &[(30, None)]),
        Scan::Clean,
        "below a sshd systemd started"
    );
    assert_eq!(
        scan_linux_reads(own_session(), &[(30, Some(true))]),
        Scan::Escaped,
        "below a set-user-id daemon of the run in its own session"
    );
    let exiting = |r: ProcRecord| ProcRecord { exiting: true, ..r };
    let first_sudo = || {
        vec![
            Some(exiting(root(10, 10, 140))),
            Some(exiting(root(10, 10, 140))),
            None,
        ]
    };
    assert_eq!(
        scan_linux_reads(
            vec![
                (20, first_sudo()),
                (
                    25,
                    vec![
                        Some(root(20, 10, 141)),
                        Some(root(20, 10, 141)),
                        Some(root(1, 10, 141))
                    ]
                ),
                (30, vec![Some(proc_at(25, 10, 150))]),
            ],
            &[]
        ),
        Scan::Escaped,
        "below a sudo -b caught as its launcher exits"
    );
    assert_eq!(
        scan_linux_reads(
            vec![
                (20, first_sudo()),
                (
                    30,
                    vec![Some(proc_at(20, 10, 150)), Some(proc_at(1, 10, 150))]
                ),
            ],
            &[]
        ),
        Scan::Escaped,
        "the daemon of a sudo exiting"
    );
}

/// Exceeding the stale-child walk depth bound returns `Unknown` rather than `Clean`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn too_many_gone_below_a_candidate_to_follow_is_no_answer() {
    let mut procs = LinuxProcs {
        ours: (0..300)
            .map(|i| (1000 + i, proc_at(999 + i, 20, 150)))
            .chain(std::iter::once((2000, proc_at(1299, 20, 151))))
            .collect(),
        others: std::collections::HashMap::new(),
        gone: std::collections::HashSet::new(),
    };
    procs.gone.insert(999);
    let mut table = ReadsProcs {
        listing: Vec::new(),
        reads: std::iter::once((2000, vec![Some(proc_at(1299, 20, 151))])).collect(),
        owners: std::collections::HashMap::new(),
    };
    assert_eq!(procs.orphans_live(&mut table, 999), Ok(true), "Linux");
    #[cfg(target_os = "macos")]
    {
        let here = std::process::id() as i32;
        let me = bsdinfo_uniq(here).expect("this process's record");
        let infos: Vec<_> = (0..=300)
            .map(|i| {
                let pid = 100_001 + i;
                (pid, record_as(&me, pid, pid - 1, 1 + i as u64))
            })
            .collect();
        let live = infos[300].1;
        let mut read = |p: i32| (p == 100_301).then_some(live);
        let mut list = |_| Some(Vec::new());
        assert!(
            orphans_live(&infos, 100_000, &mut read, &mut list, 501, None),
            "macOS"
        );
    }
}

/// Linux escape scan classifies orphans, session children, zombies, and foreign processes.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_linux_escape_scan_judges_each_kind_of_process_in_the_window() {
    use std::io::{Error, ErrorKind};
    let line = "20 (helper) S 1 20 20 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 150 1000 5 0";
    assert_eq!(
        read_proc_record(20, || Ok(line.into()), || Ok(501)).unwrap(),
        Some(proc_at(1, 20, 150))
    );
    for kind in [ErrorKind::PermissionDenied, ErrorKind::InvalidData] {
        assert_eq!(
            read_proc_record(20, || Err(Error::from(kind)), || panic!("no uid read"))
                .unwrap_err()
                .kind(),
            kind
        );
        assert_eq!(
            read_proc_record(20, || Ok(line.into()), || Err(Error::from(kind)))
                .unwrap_err()
                .kind(),
            kind
        );
    }
    assert_eq!(
        read_proc_record(20, || Ok("malformed".into()), || Ok(501))
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(
        read_proc_record(
            20,
            || Err(Error::from(ErrorKind::NotFound)),
            || panic!("gone")
        )
        .unwrap(),
        None
    );
    assert_eq!(
        read_proc_record(
            20,
            || Ok(line.into()),
            || Err(Error::from(ErrorKind::NotFound))
        )
        .unwrap(),
        None
    );
    assert_eq!(
        listed_proc_pids(["10", "self", "20", "-1"].map(|s| Ok(s.into()))),
        Some(vec![10, 20])
    );
    assert_eq!(
        listed_proc_pids([
            Ok("10".into()),
            Err(Error::from(ErrorKind::PermissionDenied))
        ]),
        None
    );
    let fixture = || FixtureProcs {
        listing: vec![10, 20],
        records: Box::new(|pid, _| match pid {
            10 => Some(proc_at(9, 10, 100)),
            20 => Some(proc_at(1, 20, 150)),
            _ => None,
        }),
        owners: [(20, Some(false))].into_iter().collect(),
        pauses: 0,
        read_errors: std::collections::HashMap::new(),
        reads: std::collections::HashMap::new(),
        list_error: false,
    };
    let judge = |table: &mut FixtureProcs| {
        linux_escape_scan(
            table,
            10,
            "s-0123456789abcdef0123",
            (100, 200),
            &[1, 5],
            501,
        )
    };
    assert_eq!(judge(&mut fixture()), Scan::Clean);
    for (pid, read) in [(20, 1), (10, 1), (10, 3)] {
        for kind in [ErrorKind::PermissionDenied, ErrorKind::InvalidData] {
            let mut table = fixture();
            table.read_errors.insert((pid, read), kind);
            assert_eq!(judge(&mut table), Scan::Unknown);
        }
    }
    let mut gone = fixture();
    gone.records = Box::new(|pid, _| (pid == 10).then_some(proc_at(9, 10, 100)));
    assert_eq!(judge(&mut gone), Scan::Clean, "confirmed missing candidate");
    let mut unread_list = fixture();
    unread_list.list_error = true;
    assert_eq!(judge(&mut unread_list), Scan::Unknown);
    let mut owner = fixture();
    owner.owners.clear();
    owner
        .read_errors
        .insert((20, 2), ErrorKind::PermissionDenied);
    assert_eq!(judge(&mut owner), Scan::Unknown);
    let mut older_session = fixture();
    older_session.records = Box::new(|pid, _| match pid {
        10 => Some(proc_at(9, 10, 100)),
        20 => Some(proc_at(1, 30, 150)),
        _ => None,
    });
    older_session
        .read_errors
        .insert((30, 1), ErrorKind::PermissionDenied);
    assert_eq!(judge(&mut older_session), Scan::Unknown);
    for pid in [20, 21] {
        let mut settling = fixture();
        settling.listing.push(21);
        settling.records = Box::new(|pid, _| match pid {
            10 => Some(proc_at(9, 10, 100)),
            20 => Some(ProcRecord {
                exiting: true,
                ..proc_at(10, 10, 150)
            }),
            21 => Some(proc_at(20, 10, 160)),
            _ => None,
        });
        settling
            .read_errors
            .insert((pid, 2), ErrorKind::PermissionDenied);
        assert_eq!(judge(&mut settling), Scan::Unknown);
    }
    let mut ancestry = fixture();
    ancestry.records = Box::new(|pid, _| (pid == 5).then_some(proc_at(1, 5, 50)));
    assert_eq!(reapers(&mut ancestry, 5), Some(vec![1, 5]));
    ancestry
        .read_errors
        .insert((5, 2), ErrorKind::PermissionDenied);
    assert_eq!(reapers(&mut ancestry, 5), None, "unreadable ancestor");
    ancestry.records = Box::new(|_, _| None);
    assert_eq!(reapers(&mut ancestry, 5), None, "missing ancestor");
    ancestry.records = Box::new(|pid, _| Some(proc_at(pid, pid, 50)));
    assert_eq!(reapers(&mut ancestry, 5), None, "cyclic ancestry");
    ancestry.records = Box::new(|pid, _| Some(proc_at(pid + 1, pid, 50)));
    assert_eq!(
        reapers(&mut ancestry, 5),
        None,
        "incomplete capped ancestry"
    );
    let named = Some(true);
    assert_eq!(
        scan_fixture(vec![(20, proc_at(1, 10, 150))], &[(20, Some(false))]).0,
        Scan::Escaped
    );
    assert_eq!(
        scan_fixture(vec![(20, proc_at(1, 20, 150))], &[(20, named)]).0,
        Scan::Escaped
    );
    assert_eq!(
        scan_fixture(vec![(20, proc_at(5, 20, 150))], &[(20, named)]).0,
        Scan::Escaped
    );
    assert_eq!(
        scan_fixture(vec![(20, proc_at(5, 20, 150))], &[(20, Some(false))]).0,
        Scan::Clean
    );
    assert_eq!(
        scan_fixture(vec![(20, proc_at(1, 20, 150))], &[(20, None)]).0,
        Scan::Escaped
    );
    assert_eq!(
        scan_fixture(vec![(20, proc_at(1, 20, 99))], &[(20, named)]).0,
        Scan::Clean
    );
    assert_eq!(
        scan_fixture(vec![(20, proc_at(1, 20, 201))], &[(20, named)]).0,
        Scan::Clean
    );
    let zombie = ProcRecord {
        ended: true,
        ..proc_at(1, 10, 150)
    };
    assert_eq!(scan_fixture(vec![(20, zombie)], &[]).0, Scan::Clean);
    for (ended, exiting) in [(true, true), (false, true)] {
        let leader_gone = ProcRecord {
            ended,
            exiting,
            threads: 3,
            ..proc_at(1, 20, 150)
        };
        assert_eq!(
            scan_fixture(vec![(20, leader_gone)], &[(20, None)]).0,
            Scan::Escaped,
            "ended={ended} exiting={exiting}"
        );
        let last = ProcRecord {
            threads: 1,
            ..leader_gone
        };
        assert_eq!(scan_fixture(vec![(20, last)], &[(20, None)]).0, Scan::Clean);
    }
    let theirs = ProcRecord {
        uid: 0,
        ..proc_at(1, 10, 150)
    };
    assert_eq!(scan_fixture(vec![(20, theirs)], &[]).0, Scan::Clean);
    assert_eq!(
        scan_fixture(
            vec![(20, proc_at(10, 10, 150)), (21, proc_at(20, 10, 160))],
            &[]
        )
        .0,
        Scan::Clean
    );
    assert_eq!(
        scan_fixture(
            vec![(20, proc_at(10, 10, 150)), (21, proc_at(20, 21, 160))],
            &[]
        )
        .0,
        Scan::Escaped
    );
    assert_eq!(
        scan_fixture(
            vec![(30, proc_at(1, 30, 50)), (31, proc_at(30, 31, 150))],
            &[(31, named)]
        )
        .0,
        Scan::Clean
    );
    assert_eq!(
        scan_fixture(
            vec![(40, proc_at(1, 40, 50)), (41, proc_at(1, 40, 150))],
            &[(41, None)]
        )
        .0,
        Scan::Clean
    );
    assert_eq!(
        scan_fixture(
            vec![(40, proc_at(1, 40, 120)), (41, proc_at(1, 40, 150))],
            &[(40, Some(false)), (41, None)]
        )
        .0,
        Scan::Escaped
    );
}

/// Linux escape scan settles records whose parent is mid-exit (`PF_EXITING`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_daemon_read_while_its_launcher_exits_is_read_again() {
    let shell = proc_at(9, 10, 100);
    let run = |finishes: bool| {
        let mut table = FixtureProcs {
            read_errors: std::collections::HashMap::new(),
            reads: std::collections::HashMap::new(),
            list_error: false,
            listing: vec![10, 20, 21],
            records: Box::new(move |pid, pauses| match pid {
                10 => Some(shell),
                20 => Some(ProcRecord {
                    exiting: true,
                    ended: finishes && pauses > 0,
                    ..proc_at(10, 10, 150)
                }),
                21 => Some(proc_at(
                    if finishes && pauses > 0 { 1 } else { 20 },
                    10,
                    160,
                )),
                _ => None,
            }),
            owners: std::collections::HashMap::new(),
            pauses: 0,
        };
        let verdict = linux_escape_scan(
            &mut table,
            10,
            "s-0123456789abcdef0123",
            (100, 200),
            &[1],
            501,
        );
        (verdict, table.pauses)
    };
    assert_eq!(
        run(true),
        (Scan::Escaped, 1),
        "read again once the exit ended"
    );
    assert_eq!(
        run(false),
        (Scan::Unknown, SCAN_SETTLE_LISTINGS - 1),
        "a launcher still exiting leaves no verdict"
    );
    let mut reaped = FixtureProcs {
        read_errors: std::collections::HashMap::new(),
        reads: std::collections::HashMap::new(),
        list_error: false,
        listing: vec![10, 20],
        records: Box::new(move |pid, _| (pid == 10).then_some(shell)),
        owners: std::collections::HashMap::new(),
        pauses: 0,
    };
    let verdict = linux_escape_scan(
        &mut reaped,
        10,
        "s-0123456789abcdef0123",
        (100, 200),
        &[1],
        501,
    );
    assert_eq!((verdict, reaped.pauses), (Scan::Clean, 0));
    for end in [None, Some(proc_at(9, 10, 300))] {
        let reads = std::cell::Cell::new(0);
        let mut gone = FixtureProcs {
            read_errors: std::collections::HashMap::new(),
            reads: std::collections::HashMap::new(),
            list_error: false,
            listing: vec![10],
            records: Box::new(move |pid, _| {
                if pid != 10 {
                    return None;
                }
                reads.set(reads.get() + 1);
                if reads.get() == 1 { Some(shell) } else { end }
            }),
            owners: std::collections::HashMap::new(),
            pauses: 0,
        };
        let verdict = linux_escape_scan(
            &mut gone,
            10,
            "s-0123456789abcdef0123",
            (100, 200),
            &[1],
            501,
        );
        assert_eq!(verdict, Scan::Unknown, "{end:?}");
    }
}

/// `parse_proc_stat` parses `/proc/<pid>/stat` fields after the last `)`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_proc_stat_line_reads_as_its_record() {
    let line = |state: char, flags: u64| {
        format!(
            "4242 (we (ird) name) {state} 7 77 78 0 -1 {flags} 0 0 0 0 0 0 0 0 20 0 4 0 31337 1000 5 0"
        )
    };
    let rec = parse_proc_stat(&line('S', 0x0040_0100), 4242, 501).expect("a record");
    assert_eq!(
        rec,
        ProcRecord {
            ppid: 7,
            session: 78,
            start: 31337,
            ended: false,
            exiting: false,
            threads: 4,
            uid: 501,
        }
    );
    assert!(
        parse_proc_stat(&line('R', 0x0040_0104), 4242, 501)
            .unwrap()
            .exiting
    );
    for state in ['Z', 'X', 'x'] {
        assert!(
            parse_proc_stat(&line(state, 0), 4242, 501).unwrap().ended,
            "{state}"
        );
    }
    assert_eq!(
        parse_proc_stat(&line('S', 0), 4243, 501),
        None,
        "another pid"
    );
    assert_eq!(
        parse_proc_stat("4242 (cut) S 7 77 78", 4242, 501),
        None,
        "cut short"
    );
}

/// `/proc/self/stat` matches the kernel process ids.
#[cfg(target_os = "linux")]
#[test]
fn this_processs_own_proc_record_names_its_parent_session_and_start() {
    let me = i32::try_from(std::process::id()).expect("pid");
    let rec = Procfs
        .read(me)
        .expect("read this process")
        .expect("this process's record");
    assert_eq!(
        rec.ppid,
        i32::try_from(std::os::unix::process::parent_id()).unwrap()
    );
    // SAFETY: `getsid(0)` and `geteuid` read this process's own ids.
    assert_eq!(rec.session, unsafe { libc::getsid(0) });
    assert_eq!(rec.uid, unsafe { libc::geteuid() });
    let stat = std::fs::read_to_string("/proc/self/stat").expect("stat");
    let (_, start) = aterm_uds::process::procfs::stat_start(&stat, me).expect("start");
    assert_eq!(rec.start, start);
    assert!(!rec.ended && !rec.exiting);
}

#[cfg(target_os = "linux")]
struct AmongProcs(Vec<i32>);

#[cfg(target_os = "linux")]
impl ProcTable for AmongProcs {
    fn list(&mut self) -> Option<Vec<i32>> {
        Some(self.0.clone())
    }

    fn read(&mut self, pid: i32) -> std::io::Result<Option<ProcRecord>> {
        Procfs.read(pid)
    }

    fn owner(&mut self, pid: i32, sid: &str) -> Option<bool> {
        Procfs.owner(pid, sid)
    }

    fn pause(&mut self, pause: Duration) {
        Procfs.pause(pause);
    }
}

/// Linux escape scan detects a real orphaned daemon in the run window.
#[cfg(target_os = "linux")]
#[test]
fn the_linux_escape_scan_catches_a_daemon_the_session_left_behind() {
    const SID: &str = "s-0123456789abcdef0123";
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let mark = dir.path().join("daemon-pid");
    let release = dir.path().join("release-parent");
    let before = std::time::SystemTime::now();
    let child = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(
            r#"( "$1" 600 & printf '%s\n' "$!" > "$2";
                 while [ ! -f "$3" ]; do "$1" 0.01; done ) &
                wait
                exec "$1" 600"#,
        )
        .arg("escape-fixture")
        .arg("/bin/sleep")
        .arg(&mark)
        .arg(&release)
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .env("ATERM_PARENT_SESSION_ID", SID)
        .current_dir(dir.path())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn");
    struct Owned {
        child: aterm_uds::spawnfd::Child,
        daemon: Option<(i32, u64)>,
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            if let Some((pid, start)) = self.daemon
                && Procfs
                    .read(pid)
                    .is_ok_and(|r| r.is_some_and(|r| r.start == start && !r.ended))
            {
                // SAFETY: `pid` matches the fixture daemon's verified birth.
                let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    let mut owned = Owned {
        child,
        daemon: None,
    };
    let shell_pid = i32::try_from(owned.child.id()).expect("pid");
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
    let daemon = Procfs
        .read(daemon_pid)
        .expect("read daemon")
        .expect("the named fixture process");
    let parent = Procfs
        .read(daemon.ppid)
        .expect("read parent")
        .expect("its held parent");
    assert_eq!(parent.ppid, shell_pid, "our child's child");
    owned.daemon = Some((daemon_pid, daemon.start));
    std::fs::write(&release, b"release\n").expect("release its parent");
    let reapers = super::reapers(
        &mut Procfs,
        i32::try_from(std::os::unix::process::parent_id()).expect("parent pid"),
    )
    .expect("complete ancestor records");
    loop {
        let now = Procfs
            .read(daemon_pid)
            .expect("read daemon")
            .expect("the owned daemon is still live");
        assert_eq!(now.start, daemon.start, "the captured birth");
        if reapers.contains(&now.ppid) {
            break;
        }
        assert!(
            Instant::now() < give_up,
            "no reaper adopted the fixture: {now:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let after = std::time::SystemTime::now();
    let caught = escape_scan(shell_pid, None, SID, before, after);
    // SAFETY: side-effect-free getter.
    let uid = unsafe { libc::geteuid() };
    let window = super::boot_tick_window(before, after).expect("the boot clock");
    let among = |window| {
        linux_escape_scan(
            &mut AmongProcs(vec![shell_pid, daemon_pid]),
            shell_pid,
            SID,
            window,
            &reapers,
            uid,
        )
    };
    let caught_among = among(window);
    let window_before = among((0, 0));
    drop(owned);
    assert_eq!(caught, Scan::Escaped);
    assert_eq!(caught_among, Scan::Escaped);
    assert_eq!(
        window_before,
        Scan::Clean,
        "a window before this process tree"
    );
}

/// Visible-run judgment requires matching initial and current shell executable paths.
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
        rc_generation(&inputs, &Reported::default()),
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
