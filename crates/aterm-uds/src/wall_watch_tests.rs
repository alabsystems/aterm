// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Instant;

#[cfg(unix)]
const MUST: Duration = Duration::from_secs(60);

#[cfg(unix)]
fn ready(watch: &WallWatch) -> bool {
    !crate::ready::any(&watch.fds(), Some(Instant::now() + MUST))
        .expect("poll")
        .is_empty()
}

/// Windows' twin, on its doorbell, is `wall_watch_win_tests.rs`'s.
#[cfg(unix)]
#[test]
fn a_told_move_polls_ready_and_is_taken_once() {
    let watch = WallWatch::by_hand().expect("doorbell");
    assert!(
        crate::ready::any_now(&watch.fds())
            .expect("poll")
            .is_empty(),
        "nothing told: nothing ready"
    );
    watch.tell(Moved {
        stepped: true,
        ..Moved::default()
    });
    assert!(ready(&watch));
    let moved = watch.take();
    assert!(moved.stepped && !moved.woke && moved.any());
    assert!(!watch.take().any(), "taken once");
    assert!(
        crate::ready::any_now(&watch.fds())
            .expect("poll")
            .is_empty()
    );
}

#[test]
fn a_wake_after_a_will_sleep_measures_the_sleep_on_both_clocks() {
    let heard = Heard::new().expect("doorbell");
    let mono = Instant::now();
    let wall = SystemTime::now();
    heard.will_sleep(mono, wall);
    heard.woke(
        mono + Duration::from_secs(2),
        wall + Duration::from_secs(3_602),
    );
    let moved = heard.take();
    assert!(moved.woke && !moved.stepped);
    assert_eq!(moved.slept, Some(Duration::from_secs(3_600)));
    heard.woke(mono, wall);
    assert_eq!(
        heard.take().slept,
        None,
        "no will-sleep before it: unmeasured"
    );
    assert_eq!(
        slept((mono, wall + Duration::from_secs(10)), (mono, wall)),
        None,
        "a wall set back says nothing"
    );
}

/// The real libnotify plumbing, on a name of this test's own (posting the system's
/// `clock_set` would tell every other process on the Mac that the clock moved).
#[cfg(target_os = "macos")]
#[test]
fn a_libnotify_post_polls_ready_and_reads_as_a_step() {
    let name = std::ffi::CString::new(format!("com.aterm.test.wall-watch.{}", std::process::id()))
        .expect("name");
    let steps = imp::Steps::on(&name).expect("registered");
    assert!(!steps.take(), "nothing posted yet");
    assert!(imp::Steps::post(&name), "posted");
    let ready = crate::ready::any(&[steps.fd()], Some(Instant::now() + MUST)).expect("poll");
    assert_eq!(ready, vec![0], "the post polls ready");
    assert!(steps.take(), "the post reads as a step");
    assert!(!steps.take(), "and only once");
}

/// A watch that hears the system's step announcement alone hears a sleep only where a resume is
/// announced as a step (Linux's timer). macOS's `clock_set` says nothing of a sleep the clock
/// was not set across, so the owner of such a watch keeps reading its own clocks, and a watch the
/// test tells by hand counts as hearing each sleep it is told.
/// NEGATIVE CONTROL: `hears_sleep` read as "either half registered", which is what
/// [`WallWatch::start`] answers `Some` for, reads true for the steps-only watch on macOS.
#[test]
fn a_watch_of_steps_alone_hears_a_sleep_only_where_a_resume_is_a_step() {
    match WallWatch::steps_only() {
        Some(watch) => assert_eq!(watch.hears_sleep(), cfg!(target_os = "linux")),
        None => assert!(
            !cfg!(any(target_os = "macos", target_os = "linux")),
            "macOS and Linux announce a step"
        ),
    }
    assert!(WallWatch::by_hand().expect("doorbell").hears_sleep());
}

#[cfg(target_os = "macos")]
#[test]
fn this_mac_announces_its_steps_and_wakes() {
    let watch = WallWatch::start().expect("macOS announces both");
    assert!(watch.steps.is_some(), "clock_set registered");
    assert!(watch.hears_sleep(), "the power half registered");
    assert_eq!(watch.fds().len(), 2);
    assert!(!watch.take().any(), "nothing moved at start");
}
