// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wall watch on Windows: the doorbell and its wait, a move told by hand, the sleep's
//! measure, and the real registration with the power manager. A real suspend cannot be asked
//! for from a test: the registration's callback is told by hand
//! (`aterm_sysprobe::power::windows::Listening::deliver`), which runs what the power manager's
//! thread would.

use super::super::{Moved, WallWatch};
use super::*;
use aterm_sysprobe::power::windows::{
    PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
};

/// A wait for something that must happen: a hang detector, never a latency budget.
const MUST: Duration = Duration::from_secs(60);

fn rung(bells: &[&Doorbell]) -> Vec<usize> {
    any(bells, Some(Instant::now() + MUST)).expect("wait")
}

fn rung_now(bells: &[&Doorbell]) -> Vec<usize> {
    any(bells, Some(Instant::now())).expect("wait")
}

#[test]
fn a_doorbell_stays_rung_until_cleared_and_a_wait_names_every_rung_one() {
    let (first, second, third) = (
        Doorbell::new().expect("event"),
        Doorbell::new().expect("event"),
        Doorbell::new().expect("event"),
    );
    let bells = [&first, &second, &third];
    assert!(rung_now(&bells).is_empty(), "none rung: the deadline came");
    assert!(!second.clear(), "not rung: nothing to take");
    third.ring();
    second.ring();
    second.ring();
    assert_eq!(rung(&bells), [1, 2], "both rung ones, by one wait");
    assert_eq!(rung(&bells), [1, 2], "a wait takes no ring");
    assert!(second.clear(), "rung since the last clear");
    assert!(!second.clear(), "and taken once, however often it was rung");
    assert_eq!(rung(&bells), [2]);
    assert!(third.clear());
    assert!(rung_now(&bells).is_empty());
}

#[test]
fn a_wait_refuses_no_bell_and_a_ring_from_another_thread_ends_one_with_no_deadline() {
    assert!(any(&[], None).is_err(), "nothing to wait on");
    let bell = std::sync::Arc::new(Doorbell::new().expect("event"));
    let ringer = std::sync::Arc::clone(&bell);
    let thread = std::thread::spawn(move || ringer.ring());
    assert_eq!(any(&[&bell], None).expect("wait"), [0]);
    thread.join().expect("the ringer");
}

#[test]
fn a_told_move_rings_the_watch_and_is_taken_once() {
    let watch = WallWatch::by_hand().expect("doorbell");
    assert!(rung_now(&[watch.bell()]).is_empty(), "nothing told");
    watch.tell(Moved {
        stepped: true,
        ..Moved::default()
    });
    assert_eq!(rung(&[watch.bell()]), [0]);
    let moved = watch.take();
    assert!(moved.stepped && !moved.woke && moved.any());
    assert!(!watch.take().any(), "taken once");
    assert!(rung_now(&[watch.bell()]).is_empty(), "the take cleared it");
}

#[test]
fn a_sleep_is_the_walls_run_past_the_awake_clock() {
    let wall = SystemTime::now();
    let awake = Duration::from_secs(1_000);
    assert_eq!(
        slept_between(
            (awake, wall),
            (
                awake + Duration::from_secs(2),
                wall + Duration::from_secs(3_602)
            )
        ),
        Some(Duration::from_secs(3_600))
    );
    assert_eq!(
        slept_between((awake, wall), (awake + Duration::from_secs(5), wall)),
        Some(Duration::ZERO),
        "a wall that ran no further than the awake clock: no sleep, never a negative one"
    );
    assert_eq!(
        slept_between((awake, wall + Duration::from_secs(10)), (awake, wall)),
        None,
        "a wall set back says nothing"
    );
}

/// The real registration: Windows takes it, announces nothing at the start, and a resume its
/// callback hears rings the watch as a wake. A suspend heard first has the resume measure the
/// sleep (none here: both clocks ran together); a resume with no suspend before it is unmeasured,
/// and the second word of a resume a person caused is not a second wake.
#[test]
fn this_system_announces_its_wakes_and_a_resume_reaches_the_watcher() {
    const { assert!(!super::super::STEPS_ANNOUNCED) };
    let watch = WallWatch::start().expect("Windows announces a suspend and a resume");
    assert!(watch.steps.is_none(), "no step is announced here");
    let listening = watch._power.as_ref().expect("registered");
    assert!(rung_now(&[watch.bell()]).is_empty());
    assert!(!watch.take().any(), "nothing moved at the start");

    listening.deliver(PBT_APMSUSPEND);
    assert!(
        rung_now(&[watch.bell()]).is_empty(),
        "a suspend is noted, not announced"
    );
    listening.deliver(PBT_APMRESUMEAUTOMATIC);
    assert_eq!(rung(&[watch.bell()]), [0], "the resume rings");
    let moved = watch.take();
    assert!(moved.woke && !moved.stepped);
    let slept = moved.slept.expect("a suspend was heard before the resume");
    assert!(
        slept < Duration::from_secs(30),
        "nothing slept between the two: {slept:?}"
    );

    listening.deliver(PBT_APMRESUMESUSPEND);
    assert!(
        rung_now(&[watch.bell()]).is_empty(),
        "the second resume word is no second wake"
    );
    listening.deliver(PBT_APMRESUMEAUTOMATIC);
    assert_eq!(rung(&[watch.bell()]), [0]);
    let moved = watch.take();
    assert!(moved.woke);
    assert_eq!(moved.slept, None, "no suspend before it: unmeasured");
}
