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
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
#[cfg(any(target_os = "macos", target_os = "linux"))]
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

/// U2: each of adoption's gates names its own miss, and a spare that passes
/// them all is taken out of the slot. Every refusal leaves the spare where it
/// was (the cold path runs instead).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn each_adoption_gate_names_its_miss_and_a_spare_that_passes_them_is_taken() {
    let wid = WindowId(0);
    let mut app = app_in("/tmp");
    // A zsh's spare, whatever the host's own login shell is: it parks, and a
    // parked one may be retargeted (`App::spare_retargets`).
    app.session_factory.shell_override = Some("/bin/zsh".to_owned());
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
    // G5: the folder, exactly, for a parked spare that may not `cd` at its
    // release (a system zshenv ran before its hold).
    app.spares
        .shared
        .pre_hold
        .store(PRE_HOLD_ZSH, Ordering::Release);
    assert_eq!(
        try_with(&mut app, &SpawnAsk::new(Some("/private"), None), grid),
        Err("folder")
    );
    app.spares.slot.as_mut().unwrap().cwd = Some("/var".into());
    assert_eq!(try_with(&mut app, &plain, grid), Err("folder"));
    app.spares.slot.as_mut().unwrap().cwd = Some("/tmp".into());
    app.spares.shared.pre_hold.store(0, Ordering::Release);
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
    // A warm spare ran its rc in its own folder: never another one.
    app.spares.slot.as_mut().unwrap().cwd = Some("/var".into());
    assert_eq!(try_with(&mut app, &plain, grid), Err("folder"));
    assert_eq!(
        try_with(&mut app, &SpawnAsk::new(Some("/usr"), None), grid),
        Err("folder")
    );
    app.spares.slot.as_mut().unwrap().cwd = Some("/tmp".into());
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
    // A parked spare in another folder passes here, for the pane's folder or
    // one named: the spawner retargets it or names the miss
    // (`AdoptCheck::verdict`).
    app.spares.slot = Some(parked_slot(&app, id + 2, Some("/var")));
    assert!(try_with(&mut app, &plain, grid).is_ok());
    app.spares.slot = Some(parked_slot(&app, id + 3, Some("/var")));
    assert!(try_with(&mut app, &SpawnAsk::new(Some("/usr"), None), grid).is_ok());
}

/// A spare whose startup files have run and whose escape scan waits out its
/// window (Verifying) is taken as a warming one is, through the warm tiers'
/// gates; one still spawning is not. Its verdict, coming after, acts on no
/// other spare. NEGATIVE CONTROL: with Verifying refused as `booked` again
/// (before, a tab asked for in the grace after the mark went cold), the
/// adoption fails.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_spare_verifying_its_run_is_taken_as_a_warming_one_is() {
    let wid = WindowId(0);
    let mut app = app_in("/tmp");
    app.session_factory.shell_override = Some("/bin/zsh".to_owned());
    let grid = app.spawn_grid(wid);
    let plain = SpawnAsk::new(None, None);
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
    // The next spare verifies in its turn; the adopted one's verdict leaves it.
    app.spares.slot = Some(parked_slot(&app, id + 1, Some("/tmp")));
    app.spares.slot.as_mut().unwrap().state = SlotState::Verifying;
    app.on_spare_note(id, SpareNote::Verified(Verdict::Backoff("escaped")));
    assert_eq!(
        app.spares.slot.as_ref().map(|s| (s.id, s.state)),
        Some((id + 1, SlotState::Verifying))
    );
}

/// U3: a booking waits for a person, the boot grace, the settle after input,
/// the replacement spacing, the hourly budget and a pressure episode; a
/// session not at its prompt waits for its output (a held pane books in its
/// folder: live L13). A promotion waits for idle input and a stable folder,
/// and stops for the budget, a pass, the power state and the gate.
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
    // A system file zsh reads before its hold (Ubuntu's /etc/zsh/zshenv), or
    // the files not read yet (the production mirror before the thread, which
    // the first tick starts, has looked): parking waits for the gate, and
    // books once it is eligible. NEGATIVE CONTROL: bash's file alone does not
    // hold a zsh spare.
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
    // An eligible generation admits every shell with a spare hold — fish
    // too, whose generation stamps the startup roots its own record names
    // (R7) — and never one with none. Neither these program names nor this
    // check executes a shell.
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

/// A parked spare's spawn looks for the system files again: one may have
/// appeared since the thread looked (zsh installed and made the shell while
/// aterm runs), which the booking, reading the mirror, could not know. A zsh
/// spare is not forked while its global zshenv now runs before the hold with
/// no eligible gate, and the mirror takes what was found, so the next booking
/// waits for the gate. NEGATIVE CONTROLS: an eligible gate, a bash spare (its
/// own file absent) and a fish one (no parked hold) are forked, and a file
/// gone again clears the mirror.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_parked_spawn_looks_for_the_system_files_again() {
    use aterm_core::shell_integration::ShellType;
    // The thread found none at its start.
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

/// A spare whose spawn found its folder would fall back forked nothing: its
/// booking goes back to the hourly budget, and that folder books no spare
/// again until the wanted folder moves, so a held pane's folder that is
/// another machine's costs one check per move, never one per keystroke.
/// NEGATIVE CONTROL: any other failed spawn keeps its booking and bars no
/// folder.
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
    // Booked for /tmp, as `spare_book` books it, and its spawn refused.
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
    // The pane's folder moves: booked there.
    crate::term_lock(&app.pool.get(focused).unwrap().term)
        .process(b"\x1b]7;file://localhost/usr\x07");
    app.spare_follow(t);
    assert_eq!(app.spare_book_check(t, &inputs), go("/usr"));
    // Another failure: counted against the budget, no folder barred.
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

/// U3b: a person seen once is enough for a PARKED spare, through an absence
/// longer than the person's window (the first Cmd-T back adopts it); WARM
/// waits for a person now.
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

/// The FIFO sweep: a FIFO whose owner is gone (a reaped pid, or a pid now
/// another process's) is removed with its link; a live owner's (this
/// process's), one whose owner could not read its own birth (`0`) while that
/// pid lives, a name that names no owner, and an owner-named entry that is no
/// FIFO are left. The release of a shell blocked on it is live L12's.
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
/// and its shell still hold; a parked one asked for in another folder is told
/// to `cd` there at its release, to the folder's kernel path, when it may. A
/// spawn naming no folder is judged in the factory's (`-d`), as it starts.
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
    // The rc moved: stale.
    std::fs::write(home.path().join(".zshrc"), "PS1='$ '\nalias x=y\n").unwrap();
    assert_eq!(
        check(None, Some(generation)).verdict(&session, None, None),
        Err("stale")
    );
    // Its folder would fall back now. Unix only: off unix no folder is judged
    // before the spawn (`spawn_folder::start_fault`), the spawn inherits.
    #[cfg(unix)]
    {
        let gone = "/nonexistent/aterm-spare";
        assert_eq!(
            check(Some(gone), None).verdict(&session, Some(gone), None),
            Err("folder")
        );
        // Another folder: a spare that may not `cd` misses; a parked one may,
        // never to a folder the cold spawn would fall back from.
        assert_eq!(
            check(Some("/tmp"), None).verdict(&session, Some("/usr"), None),
            Err("folder")
        );
        assert_eq!(
            parked("/tmp").verdict(&session, Some(gone), None),
            Err("folder")
        );
    }
    // The kernel's path: what a cold shell's `getcwd()` names there. macOS
    // and Linux: elsewhere no spare parks, and nothing is retargeted.
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
        // A spawn that names no folder starts in the factory's (`-d`), the
        // spare's own spawn too: a spare spawned there is in place, and a
        // parked one elsewhere is told that folder, not the home folder.
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
    // Its shell is gone.
    link.decide(Wake::Exit {
        session: 3,
        window: WindowId(0),
    });
    assert_eq!(
        check(None, None).verdict(&session, None, None),
        Err("process")
    );
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
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    // Stand-ins of our own, whose environment the scan can read, run in
    // place: never copies in `dir`, which goes when the test ends, while
    // macOS may still assess a first exec (`aterm_uds::deadline::stand_in`).
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
    // Own the unreaped child's fresh group before any readiness wait or
    // assertion. If capture fails, its still-parented descendants cannot escape.
    let mut owned = OwnedEscapeFixture {
        child: Some(child),
        daemon: None,
    };
    let shell_pid = owned.child.as_ref().unwrap().id() as i32;
    // Read at its spawn, as `spawn_session` does: its `exec` once its
    // child is gone takes a later version than the daemon's launcher has.
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
    let caught = escape_scan(
        shell_pid,
        shell_version,
        "s-0123456789abcdef0123",
        born,
        born,
    );
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

/// A positive zombie state establishes completed exit for this scan; the
/// flag that merely begins exit, a failed query, a partial record and an
/// empty answer (a reaped pid — settled by a later listing, not this read)
/// remain uncertain. These are bytes the real process-table reader passes
/// to its decoder.
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
/// are `Clean`; a stale daemon that cannot be read again, yet is still listed
/// (a later listing says so), leaves the scan `Unknown`, never `Clean`; the
/// same tree read live is `Escaped`.
#[cfg(target_os = "macos")]
#[test]
fn a_daemon_read_before_its_launcher_exited_is_read_again() {
    let sid = FIXTURE_SID;
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
        (Scan::Unknown, 2),
        "a stale record that cannot be read again, its pid still listed"
    );
    assert_eq!(read_live, Scan::Escaped, "the same tree read live");
}

/// The session every escape fixture's processes name in their environment
/// (`ATERM_PARENT_SESSION_ID`), and the one the scans here judge.
#[cfg(target_os = "macos")]
const FIXTURE_SID: &str = "s-0123456789abcdef0123";

/// The escape fixtures' shell and `sleep`: ad hoc signed stand-ins of
/// `/bin/bash` and `/bin/sleep` ([`aterm_uds::deadline::stand_in`]), run in
/// place, whose environment another process can read. Darwin 27 withholds a
/// platform binary's, and a scan counts an orphan whose environment it cannot
/// read, so a fixture of plain `/bin/sh` and `/bin/sleep` left orphans that
/// any other run's scan on the host counted as that run's escape: the live
/// spare suite (`spare_live_headless`) beside two loops of `spare::` read
/// COULD NOT RUN in 14 of 160 module runs (load 131-220), each on such a
/// `sleep` (9) or the `bash` forked for one (5). Each process a fixture makes
/// names [`FIXTURE_SID`] instead, which these scans count and no other run's
/// does: 0 of 160 beside 865 runs of `spare::` (load 112-243).
#[cfg(target_os = "macos")]
fn fixture_shell() -> PathBuf {
    aterm_uds::deadline::stand_in(std::path::Path::new("/bin/bash"))
}

/// See [`fixture_shell`].
#[cfg(target_os = "macos")]
fn fixture_sleep() -> PathBuf {
    aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sleep"))
}

/// A fixture's process: `program` naming [`FIXTURE_SID`], its stdio null.
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

    /// Until `pid` is neither readable as `birth`, nor a zombie, nor named by
    /// a whole listing of our uid: reaped. A process still exiting reads as
    /// nothing and is no zombie yet, but every listing names it until its exit
    /// is done (a scan then reads it unsettled, `Unknown`: measured 10 of 667
    /// runs of `spare::` beside the live spare suite; none of 1449 once this
    /// waits, which outwaited such an exit 46 times).
    fn gone(&self, pid: i32, birth: u64) {
        // SAFETY: `getuid` is a side-effect-free getter.
        let uid = unsafe { libc::getuid() };
        while bsdinfo_uniq(pid).is_some_and(|i| i.p_uniqueid == birth)
            || exited(pid)
            || !uid_pids(uid).is_some_and(|pids| !pids.contains(&pid))
        {
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
    scan_with_first_read_since(shell_pid, None, target, record)
}

/// [`scan_with_first_read`] for a shell whose pid version at its spawn was
/// `shell_version`.
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

/// [`escape_scan_with_owner`] of `shell_pid` over the first listing `listed`,
/// in the window of `window`'s birth. A pid's first reads are its records in
/// `first`, in order (`None`: unreadable), every later one the kernel's; a
/// later listing is the kernel's, of `later` alone; `owner` answers the
/// environment check (`None`: the production read).
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

/// A pid of ours, reaped: it reads as nothing and is in no later listing.
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

/// `info` as read with `pid`, parent `ppid` and unique id `uniqueid`.
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

/// A double fork caught mid-exit: its middle process (adopted, in the window)
/// read alive, then exited before its session was read, and its daemon was read
/// still naming it. That daemon is launchd's now, so the scan is incomplete,
/// and the rescan reads it as the escape it is (before: neither was judged an
/// escape, and the scan read `Clean`). The same with the middle's pid already
/// another process's at its confirm read. But a middle process whose recorded
/// children are gone too (a pipeline it waited for) orphaned nothing: `Clean`.
/// A daemon read as nothing at that check, yet listed and no zombie, may live:
/// `Unknown`. So may one below a gone child of the middle process (a triple
/// fork whose two middles exited): `Unknown`. A live child in a session older
/// than the shell is no process of the run (this test's own process, read as
/// the middle's child): `Clean`. The daemon is real (adopted); the middle
/// process is a real pid of ours, reaped (or, replaced, a live process of
/// another birth), its first read the record it had alive. NEGATIVE CONTROL:
/// with the stale-parent clause mutated out of either branch, its straddled
/// scan reads `Clean`; counting a gone child, the pipeline's reads `Unknown`;
/// with the unreadable-yet-listed arm mutated out the unread daemon's reads
/// `Clean`, as the triple fork's does with the walk below a gone child; with
/// older sessions counted, the stranger's reads `Unknown`.
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
    // The middle's pid read again as another process (the shell's child).
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
    // The middle's only recorded child was reaped before it exited.
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
    // The daemon, read under the middle, unreadable when read again.
    let unread = scan(&[
        (daemon_pid, Some(stale)),
        (middle_pid, Some(middle)),
        (daemon_pid, None),
    ]);
    // A triple fork: the middle's child exited too, the daemon read under it.
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
    // The middle's live child is this process, in a session older than the
    // shell's (its own, listed with it).
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

/// A record naming a parent gone since the listing that is gone itself when
/// read again (a parent and its last child exiting together, a busy host's
/// churn) orphaned nothing when nothing recorded below it lives: the scan is
/// whole. Measured: 240 runs of the live spare module beside three more loops
/// of it (load 130-246) failed 15 of 3600 tests on an `Inconclusive` visible
/// run (the gate left learning) or a verification's `scan-unknown`, and each
/// of their scans read `Unknown` on such stale records alone; with this rule,
/// 720 more runs failed none that way. One with a live process recorded
/// below it still leaves the scan `Unknown`. The parent and the child
/// are real pids of ours, reaped; the live process below is a real orphan.
/// NEGATIVE CONTROL: on the code before, the lone child's scan read `Unknown`.
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

/// The same straddle where the middle process left the shell's session and
/// passed its confirm read, but its environment could not be read and it was
/// gone at the read after: its daemon, read naming it, is launchd's. The
/// middle is a live process of ours (another adopted orphan) read as nothing at
/// its third read and in no later listing. NEGATIVE CONTROL: with the
/// stale-parent clause mutated out of that branch, the scan reads `Clean`.
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

/// The shell's child, in its session, read under a parent that has exited by
/// its confirm read (one the startup files left running before the run's
/// window, `( daemon & )`): the same process under another parent now is
/// launchd's, adopted in the shell's session, and the scan is incomplete. And
/// the straddle with the middle the shell's own: its child read naming it, in
/// the shell's session, is launchd's too. The child is real; each parent is a
/// reaped pid read as it lived. NEGATIVE CONTROL: with the changed-parent
/// clause mutated out, the first reads `Clean`. (The second needs either
/// clause: with the stale-parent one mutated out it still reads `Unknown`.)
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

/// A daemon whose line to the shell runs through a live process of another
/// user (a set-user-id program still running, as `sudo` is: not in this uid's
/// listing) is still the shell's, and out of its session it escaped. Below one
/// launchd adopted that is the run's (`sudo -b`) it is an orphan of the run
/// too. Below one launchd itself started (a daemon of root's, at boot), or one
/// born before the shell (an orphan of ours made before it), it is no orphan
/// of the run: the hop is judged by the clause a direct orphan is, whose
/// launcher half `a_process_launchd_started_is_no_orphan` pins. Below one that
/// cannot be read (gone) the line ends, as before.
/// The daemon is real (its own session); its first read names as parent the
/// shell's own child or another adopted orphan of ours, each left out of the
/// listing as another user's would be, a real daemon of root's, or a reaped
/// pid, and so does its confirm read (a daemon under a live parent keeps it).
/// NEGATIVE CONTROL: with the walk through other users' processes mutated
/// out, the first reads `Clean`; with the adopted-hop clause mutated out, the
/// second does; with that clause dropped from the hop (counted whatever its
/// birth or launcher), the third reads `Escaped`; with its birth half mutated
/// out, the fourth does.
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
    // Read as below `parent` at its first read and at its confirm read.
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

/// A scan lists only once the window it judges has ended (the mark plus the
/// grace), as a sample is due, so a verification at its hold waits; and a
/// sample's runs at most once per [`SCAN_EVERY`]. NEGATIVE CONTROL: without the
/// wait (before, a verification listed as soon as the rate limit let it), the
/// first scan lists at the mark, before a daemon the startup files fork in the
/// grace.
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

/// A verification's scan is not spaced from the scan before it (a sample's,
/// or the last spare's): its spare, and the thread that forks the next one,
/// wait on it. Its job reaches the scan (the
/// generation it was booked for, a clean posture), the last scan a moment ago
/// (a stamp ahead of now is inside any spacing); pid 42 is a boot-time
/// process, none of ours on a host whose tests run as a person, but whatever
/// the scan reads of it, the verdict is the scan's. NEGATIVE CONTROLS: with the verification waiting
/// out the spacing again, its verdict never comes (the hang detector); with its
/// scan spaced, it is rate-limited and the earlier scan stays the last.
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

/// An orphan whose launcher took its pid version before the shell did is none
/// of the run's: another terminal's tab, left to launchd as that terminal quit
/// while this run's startup files ran (one such orphan every 50 ms failed live
/// L4 five times in five: every boot run read `Unclean`). One the shell or a later
/// process launched is still an escape, and so is any orphan when the shell's
/// version at its spawn is unknown. The orphan is real (its launcher younger
/// than the shell); the older launcher is its record with an older original
/// parent. NEGATIVE CONTROL: without the launcher-version clause the older
/// launcher's orphan reads `Escaped`.
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

/// `<sys/proc_info.h>`: `pbi_flags` says the process has exec'd (`P_EXEC`).
#[cfg(target_os = "macos")]
const PROC_FLAG_EXEC: u32 = 0x4000;

/// When a [`TerminalSession`]'s orphan execs its `sleep`.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum OrphanExec {
    /// At once, while the terminal still has its leader: it keeps
    /// `P_CONTROLT`.
    WhileLed,
    /// Only once [`TerminalSession::exec_now`] says so, after
    /// [`TerminalSession::end_leader`]: as the `$(...)` child of a tab's zsh
    /// whose tab closed before its exec. It loses `P_CONTROLT`.
    AfterLeader,
}

/// Another terminal's session, owned: its leader leads a fresh PTY, as a
/// tab's shell does, and leaves one background process to launchd at once (a
/// double fork), which ignores the hangup and so lives on through its leader's
/// exit. The leader is this process's child, killed and reaped by
/// [`Self::end_leader`] or at drop; the orphan is retired by its birth. Its
/// programs are the fixtures' stand-ins, naming [`FIXTURE_SID`].
#[cfg(target_os = "macos")]
struct TerminalSession {
    leader: Option<i32>,
    master: i32,
    orphan: OwnedEscapeFixture,
    /// The FIFO an [`OrphanExec::AfterLeader`] orphan reads its go from.
    go: std::path::PathBuf,
}

#[cfg(target_os = "macos")]
impl TerminalSession {
    /// The session, and its orphan's pid and record once launchd adopted it
    /// (and, [`OrphanExec::WhileLed`], once it exec'd).
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
        // SAFETY: a test process; the trusted-launcher contract trivially holds.
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
        // Adopted, and (`WhileLed`) its `sleep` exec'd while the terminal
        // still has its leader: an exec after the leader's exit drops
        // `P_CONTROLT` (measured on Darwin 27: 0 of 30 such orphans kept it,
        // 30 of 30 that exec'd before), and the forked shell can be adopted
        // before it execs.
        let adopted = session.orphan_once(waits, |info| {
            info.pbsd.pbi_ppid == 1
                && (exec == OrphanExec::AfterLeader || info.pbsd.pbi_flags & PROC_FLAG_EXEC != 0)
        });
        (session, pid, adopted)
    }

    /// The orphan's record once `holds` of it, under the hang detector.
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

    /// An [`OrphanExec::AfterLeader`] orphan's go: it execs its `sleep` now.
    /// Its record once it has.
    fn exec_now(&self, waits: &FixtureWaits) -> BsdInfoWithUniqId {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        // The orphan waits in its open of the read end from its birth, and a
        // reader waiting there counts: a non-blocking open for writing then
        // succeeds, where it fails `ENXIO` with no reader. A blocking open
        // would wait for ever on an orphan that died; this is a hang detector.
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

    /// Kill and reap the leader: its session outlives it in its orphan.
    fn end_leader(&mut self) {
        if let Some(leader) = self.leader.take() {
            // SAFETY: this process's own unreaped child, so the pid is still it.
            unsafe { libc::kill(leader, libc::SIGKILL) };
            let mut status = 0;
            // SAFETY: reaps exactly that child.
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

/// An orphan born on another terminal, outside the shell's session, whose
/// leader is not shown to be the run's, is none of the run's: as the child an
/// interactive zsh forks for a startup file's `$(...)` (`/etc/zshrc`'s
/// `locale LC_CTYPE`) is when that zsh's tab closes mid-startup and launchd
/// holds the child for its last moment. Measured: the live gate tests read a
/// visible run `Unclean` under load with no unclean-run line; with the escape
/// named in the log, 240 runs of the live spare module beside three more
/// loops of it (load 60-150) read one, in L3: a `zsh` with the terminal flag
/// (`0x1400090`), adopted in the session of a leader gone and unlisted. The
/// run's own processes leave its session only by `setsid`, which drops the
/// terminal for good. The processes are real; the orphan ignores the hangup,
/// so it holds still in the state the scan read that `zsh` in, and the
/// environment's answer is Darwin 27's for a platform binary (withheld, so
/// counted against the run).
/// NEGATIVE CONTROL: the same orphan read without its terminal flag and with
/// its stdio live (the run's `setsid` daemon whose launcher exited, on the
/// shell's terminal) is `Escaped`, and so is the orphan
/// while its live leader is read as under the shell (a terminal the run
/// opened). On the code before, the gone leader's orphan read `Escaped`.
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
    // The run's `setsid` daemon holds the shell's terminal, live while the
    // shell lives.
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

/// An orphan born on another terminal that exec'd only after that terminal's
/// leader left has lost `P_CONTROLT`, yet still holds its standard
/// descriptors on the terminal its leader's exit revoked: none of the run's.
/// As the `locale` a tab's zsh forks for `/etc/zshrc`'s `$(locale LC_CTYPE)`
/// and execs once its tab has closed: the live spare suite read one as L3's
/// escape in 960 module runs (`flags=0x21404010`, its session's leader gone
/// and unlisted), and this orphan reads those flags and `ENOENT` for each of
/// its descriptors 0, 1 and 2. The processes are real: the orphan waits on a
/// FIFO for its go, given after the leader is killed and reaped.
/// NEGATIVE CONTROL: the same orphan with its descriptors read as live (the
/// run's `setsid` daemon on the shell's terminal) or unreadable is `Escaped`;
/// on the scan before the descriptors were read, this orphan read `Escaped`.
#[cfg(target_os = "macos")]
#[test]
fn an_orphan_that_execd_after_its_terminals_leader_left_is_no_escape() {
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
        Scan::Clean,
        "an orphan on another terminal, exec'd after its leader left"
    );
    assert_eq!(
        live,
        Scan::Escaped,
        "the same orphan with its stdio live (the run's `setsid` daemon)"
    );
    assert_eq!(unread, Scan::Escaped, "the same orphan, its stdio unread");
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
// A PARKED hold is a zsh/bash wrapper's, on macOS and Linux (`has_parked_hold`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_update_retires_the_spare_and_a_spare_is_never_activity() {
    let mut app = App::headless_for_test();
    let id = 50;
    let mut slot = parked_slot(&app, id, None);
    slot.state = SlotState::Booked;
    // A parked shell's spare, whatever the host's own login shell is.
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

/// A promotion writes `run` only on a stamp of the rc generation taken after
/// it asked (`Job::Freshen`, answered on the `aterm-spare` thread): a mirror
/// that still says eligible for a generation an edit has since moved never
/// runs the edited startup files unseen. Negative control: without the
/// freshen, the first look writes `run` on the stale mirror.
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
// A PARKED hold is a zsh/bash wrapper's, on macOS and Linux (`has_parked_hold`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_start_record_before_the_spawn_lands_retires_the_spare() {
    let mut app = App::headless_for_test();
    let id = 51;
    let mut slot = parked_slot(&app, id, None);
    slot.state = SlotState::Booked;
    slot.session = None;
    // A parked shell's spare, whatever the host's own login shell is.
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

/// A fish spare (no PARKED hold) lands Warming, its startup files running from
/// the spawn: its start record, even one seen before the spawn's completion
/// landed, is judged as on arrival — a `spare` record sends it to Verifying,
/// and nothing is held against the script set — and the escape scan it asks
/// for covers the run from the booking, ahead of the fork, never from the
/// completion (a window opened there missed what the startup files forked
/// before the main loop read it). The same record from a parked shell is the
/// negative control: it ran past its hold, and spares go off.
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
        // Booked (and so forked) a while before its completion lands.
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

/// A write that lands in a spare's input queue before its adoption (an rc's
/// query, answered by its reader) fires the input watch into the swallowing
/// post; the watch fires once per episode until its probe re-arms it, so the
/// adoption re-arms it (and wakes it for the byte still unread), and the
/// adopted tab's next write wakes the loop as a cold tab's does. Negative
/// control: without the adoption, the next write wakes nothing. macOS only:
/// the input watch is armed only where its probe reads a tty's queue
/// (`input_stall::watch_input`).
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
        retarget: Arc::default(),
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
    let g0 = rc_generation(&inputs, &Reported::default());
    assert_eq!(
        rc_generation(&inputs, &Reported::default()),
        g0,
        "an untouched tree"
    );
    // The history file is not a startup file.
    std::fs::write(h.join(".zsh_history"), ": 1:0;ls\n").unwrap();
    assert_eq!(
        rc_generation(&inputs, &Reported::default()),
        g0,
        "history moves nothing"
    );
    // A hook edited in place: a longer line, its folder's mtime unmoved.
    let dir_mtime = std::fs::metadata(&hooks).unwrap().modified().unwrap();
    std::fs::write(hooks.join("path.zsh"), "export PATH=/a:/b:$PATH\n").unwrap();
    assert_eq!(
        std::fs::metadata(&hooks).unwrap().modified().unwrap(),
        dir_mtime,
        "an in-place edit leaves the folder's mtime"
    );
    let g1 = rc_generation(&inputs, &Reported::default());
    assert_ne!(g1, g0, "a hook edited in place moves it");
    // A hook added.
    std::fs::write(hooks.join("more.zsh"), "alias l=ls\n").unwrap();
    let g2 = rc_generation(&inputs, &Reported::default());
    assert_ne!(g2, g1, "a hook added moves it");
    // A `.zshenv` that points ZDOTDIR elsewhere: that folder's files count.
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

/// A fish generation (R7) stamps the folders its start record names: the
/// user's (`$__fish_config_dir`), the sysconf one and every vendor conf.d one,
/// an inherited `$XDG_DATA_DIRS` entry's included. A file added to a vendor
/// conf.d or the sysconf conf.d, a sysconf `config.fish` written, an entry
/// edited in place: each moves it, so a spare warmed before is stale. Controls:
/// an untouched tree stamps the same twice, a folder no record named (the
/// binary prefix's vendor conf.d, the old guess) moves nothing, and a
/// generation with no roots named is not the one with them.
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

/// A fish run teaches its generation only when its start record named the
/// startup roots the generation stamps (R7): with none, what a spare would
/// run unseen is unknown, and the run is unclean before any process scan.
/// Controls: the same fish record naming its roots, and a zsh record (which
/// names none), both go on as far as the injected scan limit.
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
        // The future stamp withholds the scan (as above): no latency assertion.
        judge(&obs, &seen, &mut Some(now + Duration::from_secs(3600)))
    };
    assert_eq!(judged(ShellType::Fish, None), Sample::Unclean);
    assert_eq!(
        judged(ShellType::Fish, Some(vec!["/etc/fish".into()])),
        Sample::Inconclusive
    );
    assert_eq!(judged(ShellType::Zsh, None), Sample::Inconclusive);
}

/// A fish spare ran the startup files of the roots its own record names; its
/// generation holds only if it was stamped over those: a record naming other
/// roots, or none, backs off as `roots`. Control: the same roots go on to the
/// generation guard (`stale` here, its booked generation being 0).
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

// ─── The Linux escape scan (R6) ───

/// A `/proc` table a test writes: each pid's record as a function of how many
/// settling pauses have passed, each pid's environment verdict, the listing.
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

/// A live process of uid 501 with parent `ppid`, in session `session`, born
/// at tick `start`.
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

/// The fixture's shell: pid 10, leading its own session, born at tick 100;
/// the run's window is ticks 100-200, and pid 5 (aterm's parent, say a
/// `systemd --user`) is the one reaper besides init.
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

/// A `/proc` table whose records change read by read: each pid's reads in
/// order (the last repeats; an empty list or none is gone), and each pid's
/// environment verdict. The shell is pid 10 as in [`scan_fixture`].
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

/// [`linux_escape_scan`] of shell 10 (window ticks 100-200, reapers 1 and 5)
/// over `procs`, each pid's reads in order, listed in that order.
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

/// The Linux scan's parents read again (2026-10-19 review): `/proc` is read in
/// pid order, so a parent read alive can begin to exit before its child's
/// read, which still names it (`( daemon & )`: the subshell 20 exits at once,
/// its daemon 30 stays in the shell's session). Every record in the window has
/// its parent read once more, and one now gone is settled: the daemon is
/// init's, in the shell's session, the run's. And a candidate whose
/// environment cannot be read, gone at its re-read, leaves a live child read
/// naming it stale: the scan is incomplete. Only a live one: a child it reaped
/// before it exited (a pipeline) orphaned nothing; one below a child gone too
/// (a triple fork) counts. The parent's pid read again as another birth is
/// gone too, and so is a middle the settle found reaped (a double fork). A
/// parent the listing never named (another user's under `hidepid=2`) is not
/// read again: the line ends there, as before. NEGATIVE
/// CONTROLS: the same subshell alive, or reading as nothing in any case but
/// the live child, is `Clean`; with the parents' second read mutated out the
/// first case reads `Clean`, with the gone candidate's children unread the
/// third does, and counting a gone child the fourth reads `Unknown`; without
/// the walk below a gone child the triple fork reads `Clean`, without the
/// birth check the new birth does, with the reaped middle only removed the
/// double fork does, and reading the unnamed parent again leaves the last
/// `Unknown`.
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
    // 20, adopted in the window and in its own session, cannot have its
    // environment read and is gone at the read after; 30, its child.
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
    // 30 read alive at the parents' read, gone after; 40 below it lives.
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
    // The subshell's pid read again is a new child of the shell's.
    let reborn = vec![Some(subshell), Some(proc_at(10, 10, 160))];
    assert_eq!(
        scan_linux_reads(vec![(20, reborn), (30, adopted.clone())], &[]),
        Scan::Escaped,
        "the daemon of a subshell whose pid is another's now"
    );
    // A double fork: 20 exiting, its child 25 (its own session) reaped at the
    // settle, the daemon 30 read under 25, then under init.
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
    // 20 is in no listing and reads as nothing.
    assert_eq!(
        scan_linux_reads(
            vec![(30, vec![Some(proc_at(20, 30, 150))])],
            &[(30, Some(true))]
        ),
        Scan::Clean,
        "below a parent the listing never named"
    );
}

/// The Linux scan's line to the shell runs through another user's process
/// (2026-10-19 review): a daemon below a live `sudo` under the shell, out of
/// its session, escaped; below one the run left to a reaper (`sudo -b`) it is
/// an orphan of the run. Before, other users' records were read and dropped,
/// the walk stopped at them, and both read `Clean`. Out of the shell's session
/// only an environment naming the session says: this uid's login below a
/// `sshd -i` systemd started in the window, its environment withheld, is no
/// escape, while a process below a set-user-id daemon of the run (`screen
/// -dm`) whose environment names the session is. A record of such a process
/// naming a parent exiting is settled as
/// one of ours is (the first `sudo -b` caught exiting), and so is one of ours
/// naming it exiting. NEGATIVE CONTROLS: the same set-user-id process born
/// before the window, under init, is no orphan of the run (`Clean`); with the
/// walk through other users' records mutated out the first reads `Clean`, and
/// with the reaper clause the second; with a withheld environment counted
/// below one out of the session the `sshd` login reads `Escaped`, and with
/// such a hop no orphan at all the `screen` daemon reads `Clean`; settling only records of ours, the caught `sudo -b`
/// reads `Clean`, and so does the daemon below an exiting `sudo` when a parent
/// of another user's is never leaving.
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
    // `sshd -i` (20, its own session) and the login's process of this uid.
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
    // The first `sudo -b` (20) exiting; its child (25) read under it until it
    // is gone, then under init; the daemon (30) below that child.
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
    // A `sudo` under the shell (20) exiting; the daemon read under it, then
    // under init.
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

/// Following a gone candidate's stale children has a bound, and one too deep
/// to follow is no answer, never a `Clean` (2026-10-19 review): 300 gone
/// processes, each the child of the one before, the last with a live child.
/// NEGATIVE CONTROL: with the bound skipping instead (before), each reads
/// "nothing lives below".
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
        // Pids above macOS's 99999: none is ever a process.
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

/// The Linux scan's verdict on each kind of process in the run's window.
/// An orphan handed to a reaper (init, or a subreaper among aterm's
/// ancestors) is the run's when it is still in the shell's session, or when
/// its environment names the session or cannot be read while the same birth
/// lives; a process still under the shell counts only once it left the
/// session. NEGATIVE CONTROLS, each beside its positive: the same orphan
/// outside the window, naming another session, gone at its re-read, a zombie,
/// of another uid, in a session a live leader older than the shell holds, or
/// parented by a live process that is no reaper; the same child still in the
/// shell's session.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_linux_escape_scan_judges_each_kind_of_process_in_the_window() {
    use std::io::{Error, ErrorKind};
    // The actual proc adapters preserve read/list uncertainty before the
    // verdict logic sees it; none of these controls reads the host's /proc.
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
    // Initial candidate read, shell read and final shell read are all
    // required. An inaccessible or malformed record proves no disappearance.
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
    // An unreadable environment needs a successful same-birth re-read;
    // failure there cannot masquerade as a process that has exited.
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
    // Both reads in a settling pass preserve errors rather than deleting
    // the parent or child as though a missing entry had confirmed its exit.
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
    // An orphan still in the shell's session: the run's, whatever it holds.
    assert_eq!(
        scan_fixture(vec![(20, proc_at(1, 10, 150))], &[(20, Some(false))]).0,
        Scan::Escaped
    );
    // An orphan that left it (`setsid`), handed to init or to aterm's parent.
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
    // Unreadable (`ssh-agent` makes itself non-dumpable): the run's while it lives.
    assert_eq!(
        scan_fixture(vec![(20, proc_at(1, 20, 150))], &[(20, None)]).0,
        Scan::Escaped
    );
    // Outside the window, either side.
    assert_eq!(
        scan_fixture(vec![(20, proc_at(1, 20, 99))], &[(20, named)]).0,
        Scan::Clean
    );
    assert_eq!(
        scan_fixture(vec![(20, proc_at(1, 20, 201))], &[(20, named)]).0,
        Scan::Clean
    );
    // A zombie holds nothing; another uid's is not judged.
    let zombie = ProcRecord {
        ended: true,
        ..proc_at(1, 10, 150)
    };
    assert_eq!(scan_fixture(vec![(20, zombie)], &[]).0, Scan::Clean);
    // A daemon whose `main` returned through `pthread_exit` while its other
    // threads run: its leader reads as a zombie, or mid-exit, and the process
    // lives on (unreadable now: the leader's memory is gone).
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
        // NEGATIVE CONTROL: the group's last thread exiting holds nothing.
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
    // Still under the shell: a job of the run in its session, or a daemon
    // that left it.
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
    // A stranger's child: its live parent (30, born before the shell) is no
    // reaper, and it never was under the shell.
    assert_eq!(
        scan_fixture(
            vec![(30, proc_at(1, 30, 50)), (31, proc_at(30, 31, 150))],
            &[(31, named)]
        )
        .0,
        Scan::Clean
    );
    // An orphan in a session whose live leader is older than the shell is
    // another terminal's background job, even unreadable; one whose leader
    // came after the shell is not settled by that.
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

/// A record read while its parent was exiting still names that parent (the
/// kernel hands the children on only as the exit ends), so a daemon whose
/// launcher was exiting at the read looks still under the shell, in its
/// session: a job, not an escape. It is read again, with its parent, until it
/// names a live one; then it reads as the orphan it is. NEGATIVE CONTROL: a
/// parent that never finishes exiting leaves the scan `Unknown` (never
/// `Clean`) after every settling listing; a pid listed and reaped before its
/// read holds nothing and settles at once.
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
                // The launcher: exiting until a pause has passed, then a zombie.
                20 => Some(ProcRecord {
                    exiting: true,
                    ended: finishes && pauses > 0,
                    ..proc_at(10, 10, 150)
                }),
                // The daemon: handed to init once its launcher's exit ended.
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
    // A listed pid reaped before its read: nothing to settle.
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
    // The shell gone, or another process under its pid by the end: no verdict.
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

/// `/proc/<pid>/stat`'s fields, counted from the command name's last `)`
/// (a name may hold spaces and parentheses), with `PF_EXITING` read from the
/// flags, the group's thread count, and the zombie and dead states as ended. NEGATIVE CONTROL: a line
/// naming another pid, or cut short, is no record.
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

/// Linux: this process's own `/proc/self/stat` reads as the kernel says of it.
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

/// A [`ProcTable`] over the host's `/proc` that lists only a fixture's own
/// processes, so a verdict about its tree is never another test's churn.
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

/// Linux: the escape scan on an owned process tree, the macOS fixture's twin.
/// The fixture keeps its daemon parented until its exact pid and start are
/// captured, then releases that parent, and the kernel hands the daemon to
/// its reaper (init, or a child subreaper among this process's ancestors).
/// Its environment names the session, so the whole host's scan over the
/// run's window catches it. NEGATIVE CONTROL: the same tree in a window
/// before it is clean. Cleanup signals only the captured births.
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
                // SAFETY: a signal to the daemon this fixture made, its birth
                // checked just above.
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
    // SAFETY: a side-effect-free getter.
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
