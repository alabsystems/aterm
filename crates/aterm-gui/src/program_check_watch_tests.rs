// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::program_check::STUCK_AFTER_MS;

const D: Daemon = Daemon {
    pid: 690,
    start_s: 1_791_000_000,
    start_us: 1,
};
const MIN: u64 = 60_000;

fn on(daemon: Daemon, workers: u64, at_limit: bool) -> Look {
    Look::Read(Reading {
        daemon,
        workers,
        at_limit,
        limit: Some(90),
    })
}

fn reading(workers: u64, at_limit: bool) -> Look {
    on(D, workers, at_limit)
}

fn word(note: Option<&HostNote>) -> &'static str {
    match note {
        None => "-",
        Some(HostNote::ProgramCheckStuck { .. }) => "stuck",
        Some(HostNote::ProgramCheckUnstuck) => "unstuck",
        Some(_) => "other",
    }
}

fn words(looks: &[(u64, Look)]) -> Vec<&'static str> {
    let mut board = Board::default();
    looks
        .iter()
        .map(|(at, look)| word(step(&mut board, *at, *look).as_ref()))
        .collect()
}

#[test]
fn the_window_hears_the_first_answer_the_stuck_edge_and_the_end() {
    let mut board = Board::default();
    let notes: Vec<Option<HostNote>> = [
        (0, reading(6, false)),
        (MIN, reading(6, false)),
        (2 * MIN, reading(95, true)),
        (2 * MIN + STUCK_AFTER_MS, reading(95, true)),
        (3 * MIN + STUCK_AFTER_MS, reading(95, true)),
        (4 * MIN + STUCK_AFTER_MS, Look::Unread),
        (5 * MIN + STUCK_AFTER_MS, reading(4, false)),
        (6 * MIN + STUCK_AFTER_MS, reading(4, false)),
    ]
    .into_iter()
    .map(|(at, look)| step(&mut board, at, look))
    .collect();
    let said: Vec<&str> = notes.iter().map(Option::as_ref).map(word).collect();
    assert_eq!(
        said,
        [
            "unstuck", "-", "-", "stuck", "stuck", "stuck", "unstuck", "-"
        ]
    );
    for told in &notes[3..6] {
        assert_eq!(
            *told,
            Some(HostNote::ProgramCheckStuck {
                workers: 95,
                limit: Some(90),
            })
        );
    }
    assert_eq!(
        words(&[
            (0, reading(95, true)),
            (STUCK_AFTER_MS - 1, reading(95, true)),
            (STUCK_AFTER_MS, reading(5, false)),
        ]),
        ["-", "-", "unstuck"]
    );
}

/// Replacing or stopping a stuck daemon clears the alert.
#[test]
fn the_alert_ends_when_the_stuck_daemon_does() {
    let again = Daemon {
        pid: 489,
        start_s: 1_792_597_850,
        start_us: 7,
    };
    assert_eq!(
        words(&[
            (0, reading(95, true)),
            (STUCK_AFTER_MS, reading(95, true)),
            (STUCK_AFTER_MS + MIN, on(again, 95, true)),
            (2 * STUCK_AFTER_MS + MIN, on(again, 95, true)),
            (2 * STUCK_AFTER_MS + 2 * MIN, Look::NoneRunning),
            (2 * STUCK_AFTER_MS + 3 * MIN, Look::NoneRunning),
        ]),
        ["-", "stuck", "unstuck", "stuck", "unstuck", "-"]
    );
}

#[test]
fn the_status_fields_say_dash_until_read() {
    let b = Board::default();
    assert_eq!(b.judge.state().as_str(), "-");
    let s = status_fields();
    assert!(s.starts_with("syspolicyd="), "{s}");
    assert!(s.contains(" syspolicyd_workers="), "{s}");
    assert!(s.contains(" syspolicyd_full_ms="), "{s}");
}

/// `status` names the daemon the last look read and how long it has run, by the wall clock
/// its start is on; `-` before a read, once none runs, or with the clock before its start.
/// A look that reads nothing keeps the pid and gives no age, since no look says it still runs.
/// `syspolicyd_age_ms=` is how long ago the last look was taken, whatever it read.
#[test]
fn status_names_the_syspolicyd_the_last_look_read_and_how_long_it_has_run() {
    let half_hour = D.started_us() + 1_800_000_000;
    let mut b = Board::default();
    let unread = fields(&b, 0, half_hour);
    assert!(
        unread.ends_with(" syspolicyd_pid=- syspolicyd_up_ms=- syspolicyd_age_ms=-"),
        "{unread}"
    );
    step(&mut b, MIN, reading(6, false));
    assert_eq!(
        fields(&b, MIN, half_hour),
        "syspolicyd=ok syspolicyd_workers=6/90 syspolicyd_full_ms=- syspolicyd_pid=690 \
         syspolicyd_up_ms=1800000 syspolicyd_age_ms=0"
    );
    let hours = fields(&b, MIN + 3_600_000, half_hour);
    assert!(
        hours.starts_with("syspolicyd=ok ") && hours.ends_with(" syspolicyd_age_ms=3600000"),
        "a look that stands at rest says how old it is: {hours}"
    );
    let early = fields(&b, MIN, D.started_us() - 1);
    assert!(
        early.contains(" syspolicyd_pid=690 syspolicyd_up_ms=- "),
        "CONTROL: a clock before the start reads no age: {early}"
    );
    step(&mut b, 2 * MIN, Look::Unread);
    let kept = fields(&b, 2 * MIN, half_hour);
    assert!(
        kept.ends_with(" syspolicyd_pid=690 syspolicyd_up_ms=- syspolicyd_age_ms=0"),
        "a look that read nothing leaves the last daemon named, and no age, since no look \
         says it still runs: {kept}"
    );
    step(&mut b, 3 * MIN, reading(6, false));
    let again = fields(&b, 3 * MIN, half_hour);
    assert!(
        again.ends_with(" syspolicyd_pid=690 syspolicyd_up_ms=1800000 syspolicyd_age_ms=0"),
        "CONTROL: the next look that reads it gives its age again: {again}"
    );
    step(&mut b, 4 * MIN, Look::NoneRunning);
    let gone = fields(&b, 4 * MIN, half_hour);
    assert!(
        gone.ends_with(" syspolicyd_pid=- syspolicyd_up_ms=- syspolicyd_age_ms=0"),
        "CONTROL: none running names none: {gone}"
    );
}

/// The stranded judge gets the board only while its last look read a daemon, at most two
/// looks (two `DISK_TICK`s, 120 s) ago, with how long the looks have read that daemon on a
/// clock that stops while the Mac sleeps: from the first look that read it, which a look that
/// reads nothing keeps and one that finds none running, or another daemon, starts over.
#[test]
fn the_stranded_judge_reads_the_board_only_while_its_last_look_read_it() {
    let mut b = Board::default();
    assert_eq!(check_of(&b, 0), None, "nothing read");
    let at = 5 * MIN;
    step(&mut b, at, reading(6, false));
    let answering = |awake_ms| {
        Some(RunningCheck {
            daemon: D,
            pool_full: false,
            awake_ms,
        })
    };
    assert_eq!(check_of(&b, at), answering(0));
    assert_eq!(
        check_of(&b, at + 120_000),
        answering(120_000),
        "two looks old"
    );
    assert_eq!(check_of(&b, at + 120_001), None, "older than two looks");
    step(&mut b, at + MIN, Look::Unread);
    assert_eq!(check_of(&b, at + MIN), None, "the last look read nothing");
    let said = fields(&b, at + MIN, D.started_us());
    assert!(
        said.contains(" syspolicyd_workers=6/90 "),
        "CONTROL: status still says the last reading: {said}"
    );
    step(&mut b, at + 2 * MIN, stood_in(reading(2, false), true));
    assert_eq!(
        check_of(&b, at + 2 * MIN),
        Some(RunningCheck {
            daemon: D,
            pool_full: true,
            awake_ms: 2 * MIN,
        }),
        "the full stand-in reads full, the daemon read awake since the first look, a look \
         that read nothing between"
    );
    step(&mut b, at + 3 * MIN, Look::NoneRunning);
    assert_eq!(check_of(&b, at + 3 * MIN), None, "none running");
    step(&mut b, at + 4 * MIN, reading(6, false));
    step(&mut b, at + 5 * MIN, reading(6, false));
    assert_eq!(
        check_of(&b, at + 5 * MIN + 120_000),
        Some(RunningCheck {
            daemon: D,
            pool_full: false,
            awake_ms: MIN + 120_000,
        }),
        "each look that reads it moves the read time on, and one after none ran reads it \
         from that look"
    );
    let again = Daemon {
        pid: 11291,
        start_s: D.start_s + 3_600,
        start_us: 0,
    };
    step(&mut b, at + 6 * MIN, on(again, 6, false));
    assert_eq!(
        check_of(&b, at + 7 * MIN).map(|c| (c.daemon.pid, c.awake_ms)),
        Some((11291, MIN)),
        "CONTROL: another daemon is read awake from its own first look"
    );
}

/// The `full` stand-in drives a healthy reading to stuck, and `live` clears it.
#[test]
fn the_full_stand_in_drives_the_alert_and_live_ends_it() {
    let healthy = reading(2, false);
    assert_eq!(
        stood_in(healthy, true),
        on(D, 90, true),
        "the daemon kept, the flag set, its threads raised to the limit"
    );
    assert_eq!(stood_in(reading(95, true), true), reading(95, true));
    assert_eq!(
        stood_in(healthy, false),
        healthy,
        "CONTROL: live reads as read"
    );
    let no_limit = Look::Read(Reading {
        daemon: D,
        workers: 3,
        at_limit: false,
        limit: None,
    });
    assert_eq!(
        stood_in(no_limit, true),
        Look::Read(Reading {
            daemon: D,
            workers: 3,
            at_limit: true,
            limit: None,
        })
    );
    assert_eq!(stood_in(Look::NoneRunning, true), Look::NoneRunning);
    assert_eq!(stood_in(Look::Unread, true), Look::Unread);
    assert_eq!(
        words(&[
            (0, stood_in(healthy, false)),
            (MIN, stood_in(healthy, true)),
            (MIN + STUCK_AFTER_MS, stood_in(healthy, true)),
            (2 * MIN + STUCK_AFTER_MS, stood_in(healthy, false)),
        ]),
        ["unstuck", "-", "stuck", "unstuck"]
    );
}

/// STARTS ARE HELD ONLY WHILE THE CHECK READS FULL OR STUCK ([`starts_held`]): a board that
/// `program-check full`'s stand-in drives to full, and on to stuck, holds them. NEGATIVE
/// CONTROL: a board nothing has read, and one a live reading left answering, hold none.
#[test]
fn starts_held_reads_full_and_stuck_only() {
    let board = Mutex::new(Board::new());
    assert!(!starts_held(&board), "CONTROL: unread holds nothing");
    let healthy = reading(2, false);
    let at = |ms: u64, full: bool| {
        step(
            &mut board.lock().unwrap_or_else(PoisonError::into_inner),
            ms,
            stood_in(healthy, full),
        );
    };
    at(0, false);
    assert!(!starts_held(&board), "CONTROL: answering holds nothing");
    at(MIN, true);
    assert!(starts_held(&board), "full");
    at(MIN + STUCK_AFTER_MS, true);
    assert!(
        matches!(board.lock().unwrap().judge.state(), State::Stuck { .. }) && starts_held(&board),
        "stuck"
    );
    at(2 * MIN + STUCK_AFTER_MS, false);
    assert!(!starts_held(&board), "live again: answering");
}

/// A held stand-in holds its tests' lock for its whole life, so a second test's stand-in
/// waits for it rather than replacing its pid, and its drop empties no other test's. (The
/// pid is no process's, so no other test's `held_now` reads it.)
#[cfg(target_os = "macos")]
#[test]
fn a_held_stand_in_keeps_its_tests_lock_while_it_lives() {
    let held = HeldStandIn::of(u32::MAX);
    assert!(
        STAND_IN_TESTS.try_lock().is_err(),
        "a stand-in another test can replace while this one lives"
    );
    assert_eq!(held_stand_in(), Some(u32::MAX));
    drop(held);
}

#[test]
fn the_verb_reads_bare_and_stands_in_only_where_a_look_reads() {
    let any = |_| true;
    assert_eq!(parse("", false, false, any), Ok(Ask::Read));
    assert_eq!(parse("  ", true, true, any), Ok(Ask::Read));
    assert_eq!(parse("full", true, true, any), Ok(Ask::Pool(true)));
    assert_eq!(parse(" live ", true, true, any), Ok(Ask::Pool(false)));
    assert_eq!(parse("stuck", true, true, any), Err(USAGE));
    assert_eq!(parse("full live", true, true, any), Err(USAGE));
    assert!(parse("full", false, true, any).is_err_and(|e| e.contains("development build")));
    assert!(parse("live", true, false, any).is_err_and(|e| e.contains("runs no stuck check")));
}

/// `held_now` holds no program the kernel reads as past its start (this process, which runs
/// this test) and none left unread (pid 0, which is never read; pid 1, launchd, root's).
#[test]
fn held_now_holds_no_program_the_kernel_reads_started_or_cannot_read() {
    assert!(
        !held_now(std::process::id()),
        "this test runs, so it is past its start"
    );
    for unread in [0, 1] {
        assert!(
            !held_now(unread),
            "pid {unread}: no process of this user's, so unread and not held"
        );
    }
}

/// The held stand-in needs a development build and a pid of a live process of this user
/// (the only kind a start is read for, so any other would count nowhere); `held -` needs no
/// pid. This process is one of this user's; pid 1 (launchd, root's) is not.
#[test]
fn the_held_stand_in_needs_a_development_build_and_a_pid() {
    let any = |_| true;
    let only = |mine: u32| move |pid| pid == mine;
    assert_eq!(
        parse("held 25311", true, true, only(25311)),
        Ok(Ask::Held(Some(25311)))
    );
    assert_eq!(
        parse(" held  - ", true, true, |_| false),
        Ok(Ask::Held(None))
    );
    for refused in ["held", "held gh", "held 0", "held -1", "held 7 8"] {
        assert_eq!(parse(refused, true, true, any), Err(USAGE), "{refused:?}");
    }
    assert!(parse("held 25311", false, true, any).is_err_and(|e| e.contains("development build")));
    assert!(parse("held -", true, false, any).is_err_and(|e| e.contains("runs no stuck check")));
    assert_eq!(
        parse("held 25311", true, true, only(7)),
        Err(NOT_OURS),
        "a pid no live process of this user holds"
    );
    #[cfg(target_vendor = "apple")]
    {
        assert!(ours(std::process::id()), "this process is this user's");
        assert!(
            aterm_uds::peer::our_uid() == 0 || !ours(1),
            "launchd is root's"
        );
    }
}

/// THE WATCH LOOKS ONLY WHILE THERE IS SOMETHING TO JUDGE ([`Cadence`]): its first look a tick
/// after it starts; after an answer, a look that read nothing, or none running, it rests with no
/// timeout; while the pool reads full or stuck it looks each tick; a ring looks at once when the
/// last look is a tick old or more, else a tick after it, and never puts off a look already
/// due. NEGATIVE CONTROL: the full run's looks, which only the cadence brings.
#[test]
fn the_watch_looks_only_while_there_is_something_to_judge() {
    let tick = tick_ms();
    assert_eq!(tick, MIN, "the production tick");
    let mut c = Cadence::started(0);
    assert_eq!(
        c.wait_ms(0),
        Some(MIN),
        "the first look a tick after the start"
    );
    c.looked(MIN, State::Answering);
    assert_eq!(c.wait_ms(MIN), None, "an answer rests");
    assert_eq!(c.wait_ms(100 * MIN), None, "and stays at rest however long");
    c.rung(MIN + 10_000);
    assert_eq!(
        c.wait_ms(MIN + 10_000),
        Some(MIN - 10_000),
        "a ring inside a tick of the last look waits for the tick"
    );
    c.rung(MIN + 20_000);
    assert_eq!(
        c.wait_ms(MIN + 20_000),
        Some(MIN - 20_000),
        "rings do not stack"
    );
    c.looked(2 * MIN, State::Full { since_ms: 2 * MIN });
    assert_eq!(
        c.wait_ms(2 * MIN),
        Some(MIN),
        "CONTROL: full looks each tick"
    );
    c.looked(3 * MIN, State::Stuck { since_ms: 2 * MIN });
    assert_eq!(
        c.wait_ms(3 * MIN),
        Some(MIN),
        "CONTROL: stuck looks each tick"
    );
    c.rung(3 * MIN + 1);
    assert_eq!(
        c.wait_ms(3 * MIN + 1),
        Some(MIN - 1),
        "a ring never puts off the tick's own look"
    );
    c.looked(4 * MIN, State::Answering);
    assert_eq!(c.wait_ms(4 * MIN), None, "the pool's end rests again");
    c.rung(10 * MIN);
    assert_eq!(
        c.wait_ms(10 * MIN),
        Some(0),
        "a ring after a tick looks at once"
    );
    c.looked(10 * MIN, State::Unread);
    assert_eq!(c.wait_ms(10 * MIN), None, "a look that read nothing rests");
    let mut never = Cadence::started(0);
    never.rung(5);
    assert_eq!(
        never.wait_ms(5),
        Some(0),
        "a ring before any look looks at once"
    );
}

/// THE JOBS PASS RINGS THE WATCH ONLY WHILE A PROGRAM WAITS ([`read_for`]), and is then handed
/// only a fresh look; with none waiting it is handed the look that stands, however old, and
/// rings nothing. An ask rings only when the last look is a tick old or more ([`ask_on`]).
#[test]
fn the_jobs_pass_and_an_ask_ring_the_watch_only_when_it_has_something_to_look_for() {
    let board = Mutex::new(Board::new());
    let bell = Bell::new();
    let rings = || bell.rings.load(Ordering::SeqCst);
    ask_on(&board, &bell, 0);
    assert_eq!(rings(), 1, "an ask before any look rings");
    step(
        &mut board.lock().unwrap_or_else(PoisonError::into_inner),
        MIN,
        reading(6, false),
    );
    ask_on(&board, &bell, MIN + 59_999);
    assert_eq!(
        rings(),
        1,
        "CONTROL: an ask inside a tick of the look rings nothing"
    );
    ask_on(&board, &bell, 2 * MIN);
    assert_eq!(rings(), 2, "an ask a tick after the look rings");
    let late = 10 * MIN;
    let standing = read_for(&board, &bell, late, false);
    assert_eq!(
        rings(),
        2,
        "CONTROL: a pass with none waiting rings nothing"
    );
    assert_eq!(
        standing.map(|c| (c.daemon, c.pool_full)),
        Some((D, false)),
        "and reads the look that stands"
    );
    assert_eq!(
        read_for(&board, &bell, late, true),
        None,
        "a pass with one waiting is handed no stale look"
    );
    assert_eq!(rings(), 3, "and rings the watch");
    step(
        &mut board.lock().unwrap_or_else(PoisonError::into_inner),
        late,
        reading(6, false),
    );
    assert_eq!(
        read_for(&board, &bell, late + 1, true).map(|c| c.daemon),
        Some(D),
        "the look the ring brought is handed"
    );
}

/// The watch's thread parks on its bell and a ring brings a look, folded into its board: a
/// clock frozen at a tick and a ring take the first look; the answer rests the watch; a ring
/// ten ticks on takes the next. Each wait is a hang detector.
#[test]
fn a_ring_brings_the_resting_watch_a_look() {
    use std::sync::atomic::AtomicU64;
    use std::sync::mpsc;
    static BOARD_T: Mutex<Board> = Mutex::new(Board::new());
    static BELL_T: Bell = Bell::new();
    static NOW: AtomicU64 = AtomicU64::new(0);
    let (took, looks) = mpsc::channel::<u64>();
    let took = Mutex::new(took);
    std::thread::spawn(move || {
        watch(
            &BOARD_T,
            &BELL_T,
            || NOW.load(Ordering::SeqCst),
            || {
                let _ = took
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .send(NOW.load(Ordering::SeqCst));
                reading(6, false)
            },
            &|_| {},
        );
    });
    let hang = aterm_spec::wait::MUST_HAPPEN;
    NOW.store(MIN, Ordering::SeqCst);
    BELL_T.ring();
    assert_eq!(looks.recv_timeout(hang), Ok(MIN), "the first look");
    let folded = Instant::now();
    while BOARD_T
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .looked_ms
        != Some(MIN)
    {
        assert!(
            folded.elapsed() < hang,
            "the first look was never folded in"
        );
        aterm_uds::deadline::poll_yield();
    }
    NOW.store(11 * MIN, Ordering::SeqCst);
    BELL_T.ring();
    assert_eq!(
        looks.recv_timeout(hang),
        Ok(11 * MIN),
        "a ring at rest looks"
    );
    assert_eq!(
        BOARD_T
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .judge
            .state(),
        State::Answering,
        "each look is folded into the board"
    );
}

/// The watch's cost at rest: its thread on a board of its own, the live look (one setuid `ps`
/// each, counted), over `ATERM_MEASURE_REST_SECS` (190 by default, three ticks) on an answering
/// pool with nothing waiting, read by `proc_pid_rusage` for this whole test process (the
/// measuring thread's own sleep included). `ATERM_MEASURE_ARM=before` runs the loop this
/// replaced (sleep a tick, look) for the comparison. A manual measurement, never a timing gate
/// (ATERM-KI-1105-20).
#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual idle measurement of the program check's watch at rest, not a timing gate"]
fn program_check_rest_wakeups() {
    use std::sync::atomic::AtomicU64;
    static BOARD_M: Mutex<Board> = Mutex::new(Board::new());
    static BELL_M: Bell = Bell::new();
    static LOOKS: AtomicU64 = AtomicU64::new(0);
    fn wakeups() -> u64 {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: a V4 buffer of the right size for this process's own usage.
        let rc = unsafe {
            libc::proc_pid_rusage(
                i32::try_from(std::process::id()).expect("a pid"),
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast(),
            )
        };
        assert_eq!(rc, 0);
        // SAFETY: proc_pid_rusage filled it.
        unsafe { info.assume_init() }.ri_interrupt_wkups
    }
    fn live() -> Look {
        LOOKS.fetch_add(1, Ordering::SeqCst);
        aterm_uds::program_check::look(aterm_uds::program_check::PS_HANG)
    }
    let span = Duration::from_secs(
        std::env::var("ATERM_MEASURE_REST_SECS")
            .ok()
            .and_then(|secs| secs.parse().ok())
            .unwrap_or(190),
    );
    let before = std::env::var("ATERM_MEASURE_ARM").is_ok_and(|arm| arm == "before");
    std::thread::spawn(move || {
        if before {
            loop {
                std::thread::sleep(every());
                let look = live();
                step(
                    &mut BOARD_M.lock().unwrap_or_else(PoisonError::into_inner),
                    now_ms(),
                    look,
                );
            }
        }
        watch(&BOARD_M, &BELL_M, now_ms, live, &|_| {});
    });
    let wake0 = wakeups();
    std::thread::sleep(span);
    let wake1 = wakeups();
    eprintln!(
        "program check at rest ({}): span_s={} looks={} interrupt_wkups={} state={}",
        if before { "before" } else { "after" },
        span.as_secs(),
        LOOKS.load(Ordering::SeqCst),
        wake1 - wake0,
        BOARD_M
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .judge
            .state()
            .as_str()
    );
}
