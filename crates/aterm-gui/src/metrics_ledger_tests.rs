// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_timer_wakes_inherit_the_selected_deadline_owner() {
    for kind in [
        EventWakeKind::None,
        EventWakeKind::Init,
        EventWakeKind::Poll,
        EventWakeKind::WaitCancelled,
    ] {
        assert_eq!(
            wake_owner(kind, DeadlineOwner::Predictor),
            DeadlineOwner::None,
            "{kind:?} is not caused by the interrupted deadline"
        );
    }
    assert_eq!(
        wake_owner(EventWakeKind::Timer, DeadlineOwner::PresentRetry),
        DeadlineOwner::PresentRetry
    );
}

#[test]
fn arms_are_attributed_to_the_owner_that_armed_them() {
    let m = Metrics::new();
    let now = Instant::now();
    let future = now + Duration::from_millis(5);
    let stale = now - Duration::from_millis(1);
    let passed = |deadline: Option<Instant>| Heal {
        armed: deadline,
        floor: false,
        streak: false,
    };
    let sched = &m.window.sched;
    let ledger = |owner: DeadlineOwner| {
        (
            sched.arms_by_owner.get(owner.index()),
            sched.past_arms_by_owner.get(owner.index()),
            sched.streak_heals_by_owner.get(owner.index()),
        )
    };

    m.note_deadline(
        DeadlineOwner::SessionStatus,
        Some(stale),
        now,
        passed(Some(stale)),
    );
    m.note_deadline(
        DeadlineOwner::Blink,
        Some(future),
        now,
        passed(Some(future)),
    );
    m.note_deadline(
        DeadlineOwner::SessionStatus,
        Some(stale),
        now,
        Heal {
            armed: Some(now + Duration::from_millis(250)),
            floor: true,
            streak: true,
        },
    );

    assert_eq!(
        ledger(DeadlineOwner::SessionStatus),
        (2, 2, 1),
        "both stale arms and the streak heal are booked to the owner that armed them"
    );
    assert_eq!(
        ledger(DeadlineOwner::Blink),
        (1, 0, 0),
        "a healthy arm counts as an arm and NOT as a past arm"
    );
    assert_eq!(sched.past_arms.get(), 2);
    assert_eq!(sched.stale_arm_heals.get(), 1);
    assert_eq!(
        m.live.last_deadline_late.load(Ordering::Relaxed),
        1_000_000,
        "the lateness is the requested deadline's, not the clamp's"
    );

    let text = inert_text(&m, 0);
    assert_eq!(
        text_field(&text, "deadline_arms_by_owner"),
        Some("blink:1/0,session_status:2/2"),
        "{text}"
    );
    assert_eq!(
        text_field(&text, "past_arm_streak_heals"),
        Some("session_status:1"),
        "the streak ledger NAMES the producer it clamped: {text}"
    );
}

#[test]
fn only_unbounded_present_stoppages_taint_the_on_glass_ledger() {
    for reason in [
        PresentDropReason::None,
        PresentDropReason::GpuReconfigured,
        PresentDropReason::GpuTimeout,
        PresentDropReason::GpuValidation,
        PresentDropReason::CpuResize,
        PresentDropReason::CpuAcquire,
        PresentDropReason::CpuCommit,
        PresentDropReason::TargetMismatch,
        PresentDropReason::Virtual,
        PresentDropReason::CpuRaster,
    ] {
        assert!(
            !drop_taints_present_latency(reason, false),
            "{} retries; the stall it caused is REAL and stays on glass",
            reason.as_str()
        );
        assert!(
            drop_taints_present_latency(reason, true),
            "{} parked: nothing reaches glass until an external stimulus",
            reason.as_str()
        );
    }
    assert!(
        drop_taints_present_latency(PresentDropReason::GpuOccluded, false),
        "occluded glass is the canonical unwatched interval"
    );
}

#[test]
fn the_taint_window_covers_live_captures_and_the_episode_tail() {
    assert!(
        !tainted_at(false, 1_000, 0, 0),
        "no episode, no capture: the sample is aterm's own"
    );
    assert!(
        tainted_at(false, 1_000, 2_000, 0),
        "inside the episode tail"
    );
    assert!(
        !tainted_at(false, 2_000, 2_000, 0),
        "the tail is exclusive: the boundary sample is clean again"
    );
    assert!(
        tainted_at(true, 1_000, 0, 0),
        "a present a capture forced taints with no episode and no tail"
    );
    assert!(
        tainted_at(false, u64::MAX, 0, 1),
        "a LIVE capture taints regardless of the clock — the recorder is              pacing presents right now"
    );
}

#[test]
fn capture_episodes_nest_and_never_underflow() {
    let m = Metrics::new();
    let active = |m: &Metrics| m.live.capture_depth.load(Ordering::Relaxed) != 0;
    assert!(!active(&m), "no capture is running");
    m.note_capture_episode(true);
    assert!(active(&m));
    m.note_capture_episode(true);
    m.note_capture_episode(false);
    assert!(
        active(&m),
        "one close does not end two overlapping captures"
    );
    m.note_capture_episode(false);
    assert!(!active(&m), "the last close ends the episode");
    m.note_capture_episode(false);
    assert!(
        !active(&m),
        "an unpaired close saturates at zero instead of wrapping"
    );
    assert_eq!(
        m.window.present.capture_episodes.get(),
        2,
        "each opening is one episode"
    );
    assert!(
        PresentClock::read(&m, false).tainted,
        "the closing tail is still armed"
    );
}
