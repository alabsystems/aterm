// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CAP: usize = 256 * 1024;

/// K1: a burst followed by 200 ms of quiet keyframes once, and an idle session never.
#[test]
fn a_quiet_burst_keyframes_once_and_an_idle_session_never() {
    let mut p = KeyframePolicy::new(CAP, 900, 7);
    assert!(!p.dirty(), "an idle session blocks with no timeout");
    assert_eq!(p.on_quiet(10_000).keyframe, None, "idle: no keyframe, ever");
    assert_eq!(p.after_slice(1_000, 100, 0).keyframe, None);
    assert!(p.dirty());
    assert_eq!(p.on_quiet(1_150).keyframe, None, "still inside the burst");
    assert_eq!(p.on_quiet(1_200).keyframe, Some(Reason::Quiescence));
    p.keyframe_written(1_200);
    assert!(!p.dirty());
    assert_eq!(p.on_quiet(5_000).keyframe, None, "once per burst");
}

/// K2: a session streaming steadily keyframes once a second, never more often.
#[test]
fn a_stream_keyframes_once_a_second() {
    let mut p = KeyframePolicy::new(CAP, 0, 7);
    let mut keyframes = Vec::new();
    for ms in (10..=3_000).step_by(10) {
        if let Some(reason) = p.after_slice(ms, 10, 0).keyframe {
            assert_eq!(reason, Reason::Periodic);
            keyframes.push(ms);
            p.keyframe_written(ms);
        }
    }
    assert_eq!(keyframes, vec![1_000, 2_000, 3_000]);
}

/// K3: a flood that fills half the ring keyframes with the small bridge, at most
/// ten times a second.
#[test]
fn a_flood_keyframes_under_pressure_at_most_ten_times_a_second() {
    let mut p = KeyframePolicy::new(CAP, 0, 7);
    let half = (CAP / 2) as u64;
    assert_eq!(
        p.after_slice(50, half, 0).keyframe,
        None,
        "inside the 100 ms gap"
    );
    let d = p.after_slice(100, half, 0);
    assert_eq!(d.keyframe, Some(Reason::TailPressure));
    assert_eq!(Reason::TailPressure.bridge_cap(), PRESSURE_BRIDGE_CAP);
    p.keyframe_written(100);
    let mut count = 0;
    for ms in (105..=1_100).step_by(5) {
        if p.after_slice(ms, half, 0).keyframe.is_some() {
            count += 1;
            p.keyframe_written(ms);
        }
    }
    assert_eq!(count, 10, "ten a second");
}

/// K4: a clear and a config change force a keyframe at once, whatever the rate;
/// the keyframe settles them.
#[test]
fn a_clear_or_a_config_change_forces_a_keyframe() {
    let mut p = KeyframePolicy::new(CAP, 0, 7);
    p.note_config(9);
    p.note_clear();
    assert_eq!(
        p.on_quiet(0).keyframe,
        Some(Reason::Clear),
        "the clear first"
    );
    p.keyframe_written(0);
    p.note_config(7);
    p.keyframe_written(0);
    p.note_clear();
    assert_eq!(p.after_slice(1, 0, 0).keyframe, Some(Reason::Clear));
    p.keyframe_written(1);
    assert_eq!(p.after_slice(2, 0, 0).keyframe, None);
    p.note_config(8);
    assert_eq!(p.on_quiet(3).keyframe, Some(Reason::Forced), "even quiet");
    p.keyframe_written(3);
    assert_eq!(p.after_slice(4, 0, 0).keyframe, None);
    p.note_config(8);
    assert_eq!(
        p.after_slice(5, 0, 0).keyframe,
        None,
        "the same config is no change"
    );
}

/// K5: a deferred keyframe is retried at the next slice end until it is written, and
/// meanwhile the session is parked: the parse thread's wait blocks instead of retrying
/// on every quiet, since only new bytes move the parser out of what refused the
/// capture.
#[test]
fn a_deferred_keyframe_is_retried_at_the_next_slice_end() {
    let mut p = KeyframePolicy::new(CAP, 0, 7);
    p.after_slice(10, 0, 0);
    assert_eq!(p.on_quiet(210).keyframe, Some(Reason::Quiescence));
    p.keyframe_deferred(Reason::Quiescence);
    assert!(
        !p.dirty(),
        "parked: an idle session inside a query wakes zero times"
    );
    assert_eq!(p.after_slice(220, 0, 0).keyframe, Some(Reason::Quiescence));
    assert!(p.dirty(), "the slice unparked it");
    p.keyframe_deferred(Reason::Quiescence);
    p.note_resize(230, false);
    assert!(p.dirty(), "a resize unparks it");
    assert_eq!(p.on_quiet(430).keyframe, Some(Reason::Quiescence));
    p.keyframe_written(430);
    assert_eq!(p.after_slice(440, 0, 0).keyframe, None);
    p.keyframe_retry(Reason::Quiescence);
    assert!(p.dirty(), "a retry for size stays awake");
    assert_eq!(p.on_quiet(450).keyframe, Some(Reason::Quiescence));
}

/// K6: a verified mark keyframes when the budget allows; inside the budget it waits
/// for the next keyframe due (the writer counts its marks meanwhile).
#[test]
fn a_verified_mark_keyframes_when_the_budget_allows() {
    let mut p = KeyframePolicy::new(CAP, 0, 7);
    assert_eq!(p.after_slice(150, 0, 1).keyframe, Some(Reason::Mark));
    p.keyframe_written(150);
    assert_eq!(
        p.after_slice(160, 0, 2).keyframe,
        None,
        "inside the budget: no keyframe"
    );
    assert_eq!(p.on_quiet(360).keyframe, Some(Reason::Quiescence));
}

/// K7: a width change or a rows grow makes a keyframe due at the next slice end the pressure gap
/// allows, with the small bridge.
#[test]
fn a_regrid_keyframes_soon_after() {
    let mut p = KeyframePolicy::new(CAP, 0, 7);
    p.note_resize(1_000, true);
    assert_eq!(p.after_slice(1_010, 10, 0).keyframe, Some(Reason::Regrid));
    assert_eq!(Reason::Regrid.bridge_cap(), PRESSURE_BRIDGE_CAP);
    p.keyframe_written(1_010);
    p.note_resize(1_020, true);
    assert_eq!(p.after_slice(1_030, 10, 0).keyframe, None, "inside the gap");
    assert_eq!(p.after_slice(1_110, 10, 0).keyframe, Some(Reason::Regrid));
    p.keyframe_written(1_110);
    p.note_resize(1_120, false);
    assert_eq!(
        p.after_slice(1_300, 10, 0).keyframe,
        None,
        "a shrink: no K7"
    );
}

/// A resize with no output makes the session dirty, so a quiet keyframe follows at
/// the new geometry.
#[test]
fn a_resize_with_no_output_is_keyframed_when_quiet() {
    let mut p = KeyframePolicy::new(CAP, 0, 7);
    p.note_resize(1_000, false);
    assert!(p.dirty());
    assert_eq!(p.on_quiet(1_100).keyframe, None);
    assert_eq!(p.on_quiet(1_200).keyframe, Some(Reason::Quiescence));
}

/// K4 for a renumbering with no output (an import attached older history): owed at
/// once on a clean session, quiet or not, with the normal bridge, and settled by the
/// keyframe it forced.
#[test]
fn a_renumbering_with_no_output_forces_a_keyframe() {
    let mut p = KeyframePolicy::new(CAP, 0, 7);
    assert!(!p.dirty());
    p.note_renumbered();
    assert_eq!(p.on_quiet(5).keyframe, Some(Reason::Renumbered));
    assert_eq!(Reason::Renumbered.bridge_cap(), BRIDGE_CAP);
    p.keyframe_written(5);
    assert_eq!(p.on_quiet(10_000).keyframe, None, "settled");
}
