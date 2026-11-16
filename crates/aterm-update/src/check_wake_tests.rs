// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_spec::wait::{MUST_HAPPEN, until_within};
use std::cell::Cell;

const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(24 * 3600);
/// A leg cap of its own for the tests of `wait_legs`' arithmetic, which judge how a cap applies,
/// not [`BLIND_LEG`]'s value.
const CAP: Duration = Duration::from_secs(60);

/// `wait_legs` over a simulated budget whose legs each run exactly as long as asked: the legs it
/// took, and how it ended.
fn simulated(total: Duration, cap: Option<Duration>) -> (Vec<Duration>, Waited) {
    let elapsed = Cell::new(Duration::ZERO);
    let mut legs = Vec::new();
    let waited = wait_legs(
        total,
        || elapsed.get(),
        || true,
        || cap,
        |leg| {
            legs.push(leg);
            elapsed.set(elapsed.get() + leg);
            Waited::Elapsed
        },
    );
    (legs, waited)
}

/// The legs `signal`'s waiters entered, once there are at least `n`.
fn legs_entered(signal: &WakeSignal, n: usize) -> Vec<Duration> {
    until_within(MUST_HAPPEN, "a waiter enters its leg", || {
        let legs = signal.legs();
        (legs.len() >= n).then_some(legs)
    })
}

/// A day-long wait on `signal` on a thread of its own, answered on the returned channel.
fn waiter(
    signal: &'static WakeSignal,
    keep: impl Fn() -> bool + Send + 'static,
) -> std::sync::mpsc::Receiver<Waited> {
    let seen = signal.posts();
    let (done, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(sleep_watching_for_wake(signal, seen, DAY, keep));
    });
    answer
}

/// MUTANT: legs capped at [`BLIND_LEG`] whatever the hearing (the old `WAKE_SLICE`) take 720 legs
/// for the hour.
#[test]
fn an_armed_wait_enters_the_condvar_once_for_its_whole_budget() {
    let (legs, waited) = simulated(HOUR, None);
    assert_eq!(waited, Waited::Elapsed);
    assert_eq!(legs, [HOUR]);
}

/// MUTANT: a blind wait with no cap reads the clocks only at its deadline (one leg); a leg's wake
/// not returned at once would run out the budget.
#[test]
fn a_blind_wait_reads_the_clocks_every_blind_leg_and_ends_on_a_wake() {
    let (legs, waited) = simulated(HOUR, Some(BLIND_LEG));
    assert_eq!(waited, Waited::Elapsed);
    assert_eq!(legs.len() as u64, HOUR.as_secs() / BLIND_LEG.as_secs());
    assert!(legs.iter().all(|leg| *leg == BLIND_LEG));
    let elapsed = Cell::new(Duration::ZERO);
    let mut taken = 0;
    let slept = Waited::Woke(2 * HOUR);
    let waited = wait_legs(
        HOUR,
        || elapsed.get(),
        || true,
        || Some(BLIND_LEG),
        |leg| {
            taken += 1;
            elapsed.set(elapsed.get() + leg);
            if taken == 3 { slept } else { Waited::Elapsed }
        },
    );
    assert_eq!((waited, taken), (slept, 3));
}

/// MUTANT: a leg sized from a count of legs rather than from the one monotonic start (a 40 s or
/// 60 s second leg), or a zero leg taken once the budget is spent.
#[test]
fn scheduler_delays_consume_the_wait_budget_instead_of_extending_it() {
    let elapsed = Cell::new(Duration::ZERO);
    let mut legs = Vec::new();
    let result = wait_legs(
        Duration::from_secs(100),
        || elapsed.get(),
        || true,
        || Some(CAP),
        |leg| {
            legs.push(leg);
            // The first sixty-second leg resumes seventeen seconds late on both clocks.
            elapsed.set(
                elapsed.get()
                    + if legs.len() == 1 {
                        leg + Duration::from_secs(17)
                    } else {
                        leg
                    },
            );
            Waited::Elapsed
        },
    );
    assert_eq!(result, Waited::Elapsed);
    assert_eq!(legs, [CAP, Duration::from_secs(23)]);
    assert_eq!(elapsed.get(), Duration::from_secs(100));
}

/// MUTANT: a leg taken when the budget is already spent, or a first leg's wake not returned.
#[test]
fn an_expired_budget_does_not_wait_and_a_wake_still_ends_the_wait() {
    assert_eq!(
        wait_legs(
            Duration::from_secs(20),
            || Duration::from_secs(21),
            || true,
            || None,
            |_| panic!("budget already expired")
        ),
        Waited::Elapsed
    );
    let mut calls = 0;
    let woke = Waited::Woke(Duration::from_secs(150));
    assert_eq!(
        wait_legs(
            Duration::from_secs(20),
            || Duration::ZERO,
            || true,
            || Some(CAP),
            |leg| {
                calls += 1;
                assert_eq!(leg, Duration::from_secs(20));
                woke
            }
        ),
        woke
    );
    assert_eq!(calls, 1);
}

/// MUTANT: ownership read only before a leg (a retired waiter whose leg saw a post returns Woke),
/// or a leg entered with ownership already lost.
#[test]
fn lost_ownership_cancels_before_waiting_and_before_reporting_a_wake() {
    assert_eq!(
        wait_legs(
            DAY,
            || Duration::ZERO,
            || false,
            || None,
            |_| panic!("retired wait slept")
        ),
        Waited::Cancelled
    );
    let current = Cell::new(true);
    let mut calls = 0;
    let result = wait_legs(
        DAY,
        || Duration::ZERO,
        || current.get(),
        || None,
        |_| {
            calls += 1;
            current.set(false);
            Waited::Woke(Duration::ZERO)
        },
    );
    assert_eq!(result, Waited::Cancelled);
    assert_eq!(calls, 1);
}

/// MUTANT: the gap read against a fixed threshold rather than the leg's own monotonic run (a leg
/// late on both clocks reads as a sleep); the threshold off by one; a wall clock set back read as
/// a wake; the on-return reading dropped (a sleep no post announced reads as Elapsed); a post not
/// read as a wake.
#[test]
fn a_leg_late_on_both_clocks_is_no_wake() {
    let leg = BLIND_LEG;
    let late = leg + Duration::from_secs(100);
    assert_eq!(classify(false, late, late), Waited::Elapsed);
    assert_eq!(classify(false, leg, leg), Waited::Elapsed);
    assert_eq!(
        classify(false, leg, leg + SLEEP_GAP - Duration::from_millis(1)),
        Waited::Elapsed,
        "just under the threshold is no wake"
    );
    assert_eq!(
        classify(false, leg, leg + SLEEP_GAP),
        Waited::Woke(SLEEP_GAP)
    );
    assert_eq!(
        classify(false, leg, leg + 2 * HOUR),
        Waited::Woke(2 * HOUR),
        "a sleep nothing posted is read off the clocks"
    );
    assert_eq!(
        classify(false, leg, Duration::ZERO),
        Waited::Elapsed,
        "a wall clock set back is no wake"
    );
    assert_eq!(
        classify(true, leg, Duration::ZERO),
        Waited::Woke(Duration::ZERO)
    );
    assert_eq!(
        classify(true, Duration::from_secs(5), Duration::from_secs(7)),
        Waited::Woke(Duration::from_secs(2))
    );
}

/// The wait classifies each leg no post ended by that leg's own two clocks: a wall clock two hours
/// past the monotonic run over one blind leg is a sleep nothing posted, a leg late by the same on
/// both clocks is none, and a wall clock set back over the leg is none.
/// MUTANT: the leg's requested length passed for its monotonic run (the old slice reading: the
/// late leg reads as a sleep), the monotonic run passed for both clocks or every leg no post ended
/// read as Elapsed (the two-hour sleep is never read), or a wall clock set back read as a long run.
#[test]
fn the_wait_reads_each_leg_by_its_own_two_clocks() {
    const LEG: Duration = Duration::from_millis(1);
    let mono_from = Instant::now();
    let wall_from = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    // A wait of one blind leg whose clocks read their start until a leg is entered, and
    // `mono_run` and `wall_after` once one was.
    let wait = |mono_run: Duration, wall_after: SystemTime| {
        let signal = WakeSignal::new();
        let entered = || !signal.legs().is_empty();
        let waited = wait_reading(
            &signal,
            0,
            LEG,
            || true,
            || {
                if entered() {
                    mono_from + mono_run
                } else {
                    mono_from
                }
            },
            || if entered() { wall_after } else { wall_from },
        );
        assert_eq!(signal.legs(), [LEG], "one blind leg of what remains");
        waited
    };
    assert_eq!(
        wait(LEG, wall_from + LEG + 2 * HOUR),
        Waited::Woke(2 * HOUR),
        "a sleep nothing posted is read off the leg's clocks"
    );
    let late = Duration::from_secs(100);
    assert_eq!(
        wait(late, wall_from + late),
        Waited::Elapsed,
        "a leg late on both clocks is no sleep"
    );
    assert_eq!(
        wait(LEG, wall_from - 2 * HOUR),
        Waited::Elapsed,
        "a wall clock set back over the leg is no sleep"
    );
}

/// A waiter inside a day-long leg on an armed signal is retired by the watchdog: the supersede
/// nudges the bound wake and the wait ends Cancelled with no post.
/// MUTANT: `supersede` not nudging (the waiter sleeps out its day: the hang detector trips); a
/// nudge written as a post (the post count moves); a cap on an armed leg (its first leg is short).
#[test]
fn a_supersede_inside_a_day_long_wait_cancels_it_without_a_post() {
    static WATCH: crate::checker_watch::CheckerWatch = crate::checker_watch::CheckerWatch::new();
    static WAKE: WakeSignal = WakeSignal::new();
    WATCH.wake_on_retire(&WAKE);
    assert!(WAKE.arm());
    let generation = WATCH.register(1_000);
    let answer = waiter(&WAKE, move || WATCH.is_current(generation));
    let legs = legs_entered(&WAKE, 1);
    assert!(
        legs[0] > BLIND_LEG,
        "an armed wait takes one long leg: {legs:?}"
    );
    assert!(WATCH.supersede(generation).is_some());
    assert_eq!(
        answer.recv_timeout(MUST_HAPPEN).ok(),
        Some(Waited::Cancelled),
        "a retired waiter must leave its day-long leg"
    );
    assert_eq!(WAKE.posts(), 0, "retirement is not a wake");
}

/// A virtual machine's guest never arms a long leg, whatever hears it: its waits read the clocks
/// every [`BLIND_LEG`]. A test build's own probe answers no guest.
/// MUTANT: `arm` honoured in a guest (one long leg).
#[test]
fn a_guest_never_takes_a_long_leg() {
    fn guest() -> bool {
        true
    }
    static GUEST: WakeSignal = WakeSignal::probing(guest);
    assert!(!hypervisor_guest(), "the test arm reads no machine");
    assert!(!GUEST.arm(), "a guest refuses the long leg");
    assert!(!GUEST.hears_sleep());
    let answer = waiter(&GUEST, || true);
    assert_eq!(legs_entered(&GUEST, 1)[0], BLIND_LEG);
    GUEST.post();
    assert!(matches!(
        answer.recv_timeout(MUST_HAPPEN).ok(),
        Some(Waited::Woke(_))
    ));
    let host = WakeSignal::new();
    assert!(host.arm() && host.hears_sleep(), "outside a guest it arms");
}

/// A wait nothing tells of this process's sleeps reads its clocks at least every 5 s, as every
/// wait did before one-leg waits, so a wake nothing posted still reaches a check within 8 s with
/// the 3 s settle: a host that never asked (a macOS terminal session, the only checker on a Mac
/// reached over ssh), one whose hearer is still starting, and one whose hearer could not start or
/// stopped (a window or node job whose power registration was refused).
/// MUTANT: a minute-long blind leg (a wake nothing posted is noticed up to 63 s late), or a long
/// leg taken while the hearer is still starting or after it stopped.
#[test]
fn a_wait_nothing_tells_of_a_sleep_reads_its_clocks_every_five_seconds() {
    const OLD_SLICE: Duration = Duration::from_secs(5);
    static WAKE: WakeSignal = WakeSignal::new();
    let first_leg = |state: &str| {
        let before = WAKE.legs().len();
        let answer = waiter(&WAKE, || true);
        let leg = legs_entered(&WAKE, before + 1)[before];
        WAKE.post();
        assert!(
            matches!(answer.recv_timeout(MUST_HAPPEN).ok(), Some(Waited::Woke(_))),
            "{state}: a post ends the wait"
        );
        assert!(
            leg <= OLD_SLICE,
            "{state}: a wait nothing tells of a sleep took a {leg:?} leg"
        );
    };
    first_leg("never asked");
    #[cfg(unix)]
    {
        WAKE.hearing();
        first_leg("a hearer starting");
    }
    WAKE.disarm();
    first_leg("a hearer stopped");
    assert!(
        WAKE.legs().iter().all(|leg| *leg <= OLD_SLICE),
        "{:?}",
        WAKE.legs()
    );
}

/// A told wake reaches a day-long wait through the wall relay: `hear_wakes_from` arms the signal
/// once its watch hears a sleep, and the wake after a will-sleep is posted.
/// MUTANT: a relay that never posts, or one that posts only on steps (the hang detector trips).
#[cfg(unix)]
#[test]
fn a_told_wake_ends_a_day_long_wait_through_the_wall_relay() {
    use aterm_uds::wall_watch::{Moved, WallWatch};
    use std::sync::Arc;
    static WAKE: WakeSignal = WakeSignal::new();
    let watch = Arc::new(WallWatch::by_hand().expect("doorbell"));
    let source = Arc::clone(&watch);
    assert!(hear_wakes_from(&WAKE, move || Some(source), wall_thread));
    until_within(MUST_HAPPEN, "the relay arms the wait", || {
        WAKE.hears_sleep().then_some(())
    });
    let answer = waiter(&WAKE, || true);
    assert!(legs_entered(&WAKE, 1)[0] > BLIND_LEG);
    watch.tell(Moved {
        woke: true,
        slept: Some(3 * HOUR),
        stepped: false,
    });
    assert!(matches!(
        answer.recv_timeout(MUST_HAPPEN).ok(),
        Some(Waited::Woke(_))
    ));
}

/// A wake with no will-sleep before it (an idle sleep that did not come) posts nothing; a step and
/// a measured sleep post one each.
/// MUTANT: a relay posting on every `woke` (each `kIOMessageSystemWillNotSleep` would reset the
/// backoff), one blind to steps, or one blind to wakes.
#[cfg(unix)]
#[test]
fn a_wake_with_no_sleep_before_it_posts_nothing() {
    use aterm_uds::wall_watch::{Moved, WallWatch};
    let signal = WakeSignal::new();
    let watch = WallWatch::by_hand().expect("doorbell");
    let tell_and_answer = |moved: Moved| {
        watch.tell(moved);
        answer(&signal, hear(&watch).expect("poll"));
        signal.posts()
    };
    assert_eq!(
        tell_and_answer(Moved {
            woke: true,
            slept: None,
            stepped: false
        }),
        0
    );
    assert_eq!(
        tell_and_answer(Moved {
            stepped: true,
            ..Moved::default()
        }),
        1
    );
    assert_eq!(
        tell_and_answer(Moved {
            woke: true,
            slept: Some(HOUR),
            stepped: false
        }),
        2
    );
}

/// With no watch the signal stays blind and a wait reads its clocks every [`BLIND_LEG`]; a watch
/// of steps alone arms it only where a resume is a step (Linux); a watch that hears sleeps arms it.
/// MUTANT: arming without a source (a host whose power registration failed would sleep a whole
/// leg past a wake), arming on a steps-only watch on macOS, or a blind wait with no cap.
#[cfg(unix)]
#[test]
fn no_wall_watch_leaves_the_wait_blind() {
    use aterm_uds::wall_watch::WallWatch;
    use std::sync::Arc;
    static NONE: WakeSignal = WakeSignal::new();
    static STEPS: WakeSignal = WakeSignal::new();
    static HEARS: WakeSignal = WakeSignal::new();
    let answered = |signal: &WakeSignal| {
        until_within(MUST_HAPPEN, "the hearer answers", || {
            (signal.hearer.load(Ordering::SeqCst) != STARTING).then_some(())
        });
    };
    assert!(hear_wakes_from(&NONE, || None, wall_thread));
    answered(&NONE);
    assert!(!NONE.hears_sleep());
    let answer = waiter(&NONE, || true);
    assert_eq!(legs_entered(&NONE, 1)[0], BLIND_LEG);
    NONE.post();
    assert!(matches!(
        answer.recv_timeout(MUST_HAPPEN).ok(),
        Some(Waited::Woke(_))
    ));
    assert!(hear_wakes_from(
        &STEPS,
        || WallWatch::steps_only().map(Arc::new),
        wall_thread
    ));
    answered(&STEPS);
    assert_eq!(STEPS.hears_sleep(), cfg!(target_os = "linux"));
    assert!(hear_wakes_from(
        &HEARS,
        || Some(Arc::new(WallWatch::by_hand().expect("doorbell"))),
        wall_thread
    ));
    answered(&HEARS);
    assert!(HEARS.hears_sleep());
}

/// A thread that did not start never made the watch and leaves the waits blind.
/// MUTANT: the watch started, and the signal armed, on the caller before the spawn (a refused
/// spawn would leave waits long with nothing to post).
#[cfg(unix)]
#[test]
fn a_refused_spawn_never_starts_the_watch_and_leaves_the_wait_blind() {
    use std::sync::atomic::AtomicBool;
    static WAKE: WakeSignal = WakeSignal::new();
    static STARTED: AtomicBool = AtomicBool::new(false);
    assert!(!hear_wakes_from(
        &WAKE,
        || {
            STARTED.store(true, Ordering::SeqCst);
            Some(std::sync::Arc::new(
                aterm_uds::wall_watch::WallWatch::by_hand().expect("doorbell"),
            ))
        },
        |_| Err(std::io::Error::other("refused"))
    ));
    assert!(
        !STARTED.load(Ordering::SeqCst),
        "the watch starts on its thread"
    );
    assert!(!WAKE.hears_sleep());
    assert_eq!(WAKE.hearer.load(Ordering::SeqCst), BLIND);
}

/// A relay whose source fails disarms the signal and nudges a waiter inside its day-long leg onto
/// a blind leg at once.
/// MUTANT: `disarm` not nudging, or a failing relay not disarming (the waiter stays in its day:
/// the hang detector trips); a disarmed wait with no cap.
#[cfg(unix)]
#[test]
fn a_failed_relay_disarms_a_day_long_wait_back_to_blind_legs() {
    static WAKE: WakeSignal = WakeSignal::new();
    assert!(WAKE.arm());
    let answer = waiter(&WAKE, || true);
    assert!(legs_entered(&WAKE, 1)[0] > BLIND_LEG);
    relay(&WAKE, || Err(std::io::Error::other("poll failed")));
    assert!(!WAKE.hears_sleep());
    assert_eq!(legs_entered(&WAKE, 2)[1], BLIND_LEG);
    WAKE.post();
    assert!(matches!(
        answer.recv_timeout(MUST_HAPPEN).ok(),
        Some(Waited::Woke(_))
    ));
}
