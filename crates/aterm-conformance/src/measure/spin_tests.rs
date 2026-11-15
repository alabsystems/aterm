// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CLAUDE: SpinRow = SpinRow {
    scene: SpinScene::ClaudeShaped,
    recording: true,
    window: Duration::from_secs(10),
    max_arms: 100,
    max_timer_wakes: Some(12),
    max_owner_fires: &[],
    needle_owed: false,
    max_wakes: 400,
    max_frames: None,
    min_hidden_parked: None,
};

const PET: SpinRow = SpinRow {
    scene: SpinScene::PetAsleep,
    max_timer_wakes: None,
    max_owner_fires: &[(EFFECT_LANE, 0)],
    needle_owed: true,
    max_wakes: 200,
    ..CLAUDE
};

const fn counts(arms: u64, timer_wakes: Option<u64>, wakes: Option<u64>) -> Counts<'static> {
    Counts {
        arms,
        timer_wakes,
        owner_fires: &[],
        needle: NeedleCounts {
            fires: Some(0),
            sweeps: Some(0),
            owed: Some(0),
            carried: Some(0),
        },
        wakes,
        frames: None,
        hidden_parked: None,
    }
}

/// A row that bounds its frames fails one frame past the bound and passes at it, and an
/// unreported frame count passes no bound; a row without one passes any count.
#[test]
fn a_bounded_frame_count_fails_one_past_its_bound() {
    let resting = SpinRow {
        scene: SpinScene::Resting,
        max_frames: Some(6),
        ..CLAUDE
    };
    let at = |frames| Counts {
        frames,
        ..counts(0, Some(5), Some(200))
    };
    assert_eq!(judge(&resting, at(Some(6))), ShapeVerdict::Pass);
    assert_eq!(
        judge(&resting, at(Some(7))),
        ShapeVerdict::Fail,
        "a frame past the bound, under every other"
    );
    assert_eq!(judge(&resting, at(None)), ShapeVerdict::Fail);
    assert_eq!(
        judge(&CLAUDE, at(Some(10_000))),
        ShapeVerdict::Pass,
        "an unbounded row"
    );
}

/// A hidden row fails one frame past its bound, and passes only where at least its fewest
/// redraws parked, a parked count unreported (a build without the gate) passing none; a row
/// that asks for none passes whatever parked.
#[test]
fn a_hidden_row_fails_a_frame_and_passes_only_on_the_redraws_it_parked() {
    let hidden = SpinRow {
        scene: SpinScene::Hidden,
        max_frames: Some(0),
        min_hidden_parked: Some(5),
        ..CLAUDE
    };
    let at = |frames, hidden_parked| Counts {
        frames,
        hidden_parked,
        ..counts(0, Some(5), Some(200))
    };
    assert_eq!(judge(&hidden, at(Some(0), Some(5))), ShapeVerdict::Pass);
    assert_eq!(
        judge(&hidden, at(Some(1), Some(10))),
        ShapeVerdict::Fail,
        "one frame of a minimised window"
    );
    assert_eq!(
        judge(&hidden, at(Some(0), Some(4))),
        ShapeVerdict::Fail,
        "output that never reached the window"
    );
    assert_eq!(
        judge(&hidden, at(Some(0), None)),
        ShapeVerdict::Fail,
        "a build that reports no parked redraw"
    );
    assert_eq!(
        judge(&CLAUDE, at(None, None)),
        ShapeVerdict::Pass,
        "a row that asks for no parked redraw"
    );
}

/// A build whose `trail status` reports no `sight=` (one from before the field) leaves the
/// hidden row unanswered, so a bundle this checkout did not build is named rather than refused;
/// one that reports it, wherever its window is, can take it.
#[test]
fn a_build_without_the_sight_field_leaves_the_hidden_row_unanswered() {
    let at = |sight: &str| format!("OK 1\ntrail focused=true selection=front {sight}");
    assert_eq!(
        hidden_unanswered(&at("resting=false")),
        Some("ctl trail status does not report sight")
    );
    for sight in ["sight=visible", "sight=covered", "sight=off-glass"] {
        assert_eq!(hidden_unanswered(&at(sight)), None, "{sight}");
    }
}

/// A hidden take decides only on a window that reads `sight=off-glass`, and on no window event
/// in its window.
#[test]
fn a_hidden_take_decides_only_off_every_glass_and_with_no_window_event() {
    let at = |sight: &str| format!("OK 1\ntrail focused=false selection=front {sight}");
    assert_eq!(hidden_unready(&at("sight=off-glass")), None);
    for (sight, off) in [
        ("sight=covered", "sight=covered"),
        ("sight=visible", "sight=visible"),
        ("", "sight=?"),
    ] {
        assert_eq!(hidden_unready(&at(sight)).as_deref(), Some(off), "{sight}");
    }
    assert!(os_events_unidle(SpinScene::Hidden, 1).is_some(), "a show");
    assert_eq!(os_events_unidle(SpinScene::Hidden, 0), None);
    assert_eq!(rest_unready(SpinScene::Hidden, &at("resting=true")), None);
}

/// A resting take decides only on a window that reads `resting=true`; the awake control
/// refuses one that rests and takes one whose build says nothing of rest, and every pet scene
/// refuses a take whose window took any event.
#[test]
fn a_rest_take_decides_only_at_rest_and_the_awake_control_only_awake() {
    let at = |resting: &str| format!("OK 1\ntrail focused=true selection=front {resting}");
    assert_eq!(rest_unready(SpinScene::Resting, &at("resting=true")), None);
    assert_eq!(
        rest_unready(SpinScene::Resting, &at("resting=false")).as_deref(),
        Some("resting=false")
    );
    assert_eq!(
        rest_unready(SpinScene::Resting, &at("")).as_deref(),
        Some("resting=?")
    );
    assert_eq!(
        rest_unready(SpinScene::PetAsleep, &at("resting=false")),
        None
    );
    assert_eq!(
        rest_unready(SpinScene::PetAsleep, &at("")),
        None,
        "an older build"
    );
    assert_eq!(
        rest_unready(SpinScene::PetAsleep, &at("resting=true")).as_deref(),
        Some("resting=true")
    );
    assert_eq!(rest_unready(SpinScene::Windowed, &at("resting=true")), None);
    assert_eq!(
        rest_unready(SpinScene::WindowedRewound, &at("resting=true")),
        None
    );
    assert!(os_events_unidle(SpinScene::Resting, 1).is_some());
    assert_eq!(os_events_unidle(SpinScene::Resting, 0), None);
}

/// The take's footprint summary: the median, the extremes, and the samples within the floor
/// band of the least.
#[test]
fn a_takes_footprint_reads_its_median_and_its_share_at_the_floor() {
    let mib = |m: u64| m << 20;
    let footprints = [mib(280), mib(100), mib(110), mib(281), mib(105)];
    let s = Sampled::of(&footprints, 7).expect("samples");
    assert_eq!(
        (s.samples, s.min, s.max, s.p50, s.at_floor),
        (5, mib(100), mib(281), mib(110), 3)
    );
    assert!((s.floor_share() - 0.6).abs() < 1e-9, "{s:?}");
    assert_eq!(s.cpu_ns, 7);
    assert_eq!(Sampled::of(&[], 0), None);
    assert!(s.to_string().contains("footprint_p50_mib=110.0 "), "{s}");
}

/// A pet take whose needle fired `fires` times against the frames its window's readings owed
/// and what the episode in flight as it opened had left, among `wakes` loop wakes.
const fn needle(fires: u64, (owed, carried): (u64, u64), wakes: u64) -> Counts<'static> {
    Counts {
        owner_fires: &[(EFFECT_LANE, Some(0))],
        needle: NeedleCounts {
            fires: Some(fires),
            sweeps: None,
            owed: Some(owed),
            carried: Some(carried),
        },
        ..counts(0, None, Some(wakes))
    }
}

/// Every row writes its recording either way, after its scene's lines and before any table,
/// so it records or not as it says whatever the shipped default is; a Rewind view's scenes
/// need the recording arm.
#[test]
fn a_rows_head_sets_its_recording_either_way_and_a_rewound_scene_needs_it_on() {
    for scene in [
        SpinScene::ClaudeShaped,
        SpinScene::Idle,
        SpinScene::Rewound,
        SpinScene::WindowedRewound,
        SpinScene::Windowed,
        SpinScene::PetAsleep,
        SpinScene::Resting,
        SpinScene::Hidden,
    ] {
        // Every scene the enum has: a new one fails to compile here until it joins the list.
        match scene {
            SpinScene::ClaudeShaped
            | SpinScene::Idle
            | SpinScene::Rewound
            | SpinScene::WindowedRewound
            | SpinScene::Windowed
            | SpinScene::PetAsleep
            | SpinScene::Resting
            | SpinScene::Hidden => {}
        }
        for recording in [true, false] {
            let row = SpinRow {
                scene,
                recording,
                ..CLAUDE
            };
            let head = head(&row);
            let last = head.lines().last().unwrap_or_default();
            assert_eq!(
                last,
                format!("temporal_recording = {recording}"),
                "{scene:?}: {head}"
            );
            assert_eq!(
                head.matches("temporal_recording").count(),
                1,
                "{scene:?}: {head}"
            );
            assert!(!head.contains('['), "no table opens in a spin head: {head}");
            assert_eq!(
                row.validate().is_ok(),
                recording || !scene.rewound(),
                "{scene:?} recording={recording}"
            );
        }
    }
    assert!(SpinScene::WindowedRewound.windowed() && SpinScene::WindowedRewound.rewound());
    assert!(!SpinScene::Rewound.windowed() && SpinScene::Rewound.rewound());
    assert!(!SpinScene::WindowedRewound.pet());
    assert!(
        SpinScene::WindowedRewound
            .config()
            .contains("motion = \"reduced\"")
    );
    assert_eq!(
        SpinScene::WindowedRewound.config(),
        SpinScene::Windowed.config(),
        "the window half is the bare window's"
    );
    // The view half takes the bare window's event allowance, not a pet's none.
    assert_eq!(os_events_unidle(SpinScene::WindowedRewound, 2), None);
    assert!(os_events_unidle(SpinScene::WindowedRewound, MAX_OS_EVENTS + 1).is_some());
}

/// The windowed Rewound row bounds the blink lane at 0: a focused window that blinks its
/// caret over a rewound pane fails it under every other bound (the red: the `is_rewound` arm
/// of the blink deadline taken out, which blinked it 19 times in 10 s on 2026-10-27), as a
/// blink lane the metrics leave out does.
#[test]
fn a_blinking_caret_over_a_rewound_window_fails_its_row() {
    let row = SpinRow {
        scene: SpinScene::WindowedRewound,
        max_timer_wakes: Some(20),
        max_owner_fires: &[(BLINK_LANE, 0)],
        max_wakes: 200,
        ..CLAUDE
    };
    let still = [(BLINK_LANE, Some(0))];
    let quiet = Counts {
        owner_fires: &still,
        ..counts(0, Some(10), Some(60))
    };
    assert_eq!(judge(&row, quiet), ShapeVerdict::Pass);
    for blinks in [Some(1), Some(20), None] {
        let fired = [(BLINK_LANE, blinks)];
        assert_eq!(
            judge(
                &row,
                Counts {
                    owner_fires: &fired,
                    ..counts(0, Some(10), Some(60))
                }
            ),
            ShapeVerdict::Fail,
            "{blinks:?} blink fires"
        );
    }
    let line = "OK timer_wakes=29 deadline_fires_by_owner=blink:19,system_vitals:10";
    assert_eq!(owner_fires(line, BLINK_LANE), Some(19));
}

#[test]
fn a_healthy_window_passes_and_any_witness_fails_it() {
    assert_eq!(
        judge(&CLAUDE, counts(0, Some(5), Some(200))),
        ShapeVerdict::Pass
    );
    assert_eq!(
        judge(&CLAUDE, counts(101, Some(5), Some(200))),
        ShapeVerdict::Fail,
        "the spin itself"
    );
    assert_eq!(
        judge(&CLAUDE, counts(0, Some(21), Some(200))),
        ShapeVerdict::Fail,
        "the clamped poll"
    );
    assert_eq!(
        judge(&CLAUDE, counts(0, Some(5), Some(401))),
        ShapeVerdict::Fail,
        "a loop that wakes itself: no arm, no timer"
    );
}

#[test]
fn a_bounded_wake_count_that_was_not_reported_is_not_a_pass() {
    assert_eq!(
        judge(&CLAUDE, counts(0, None, Some(200))),
        ShapeVerdict::Fail
    );
    assert_eq!(judge(&CLAUDE, counts(0, Some(5), None)), ShapeVerdict::Fail);
    let idle = SpinRow {
        scene: SpinScene::Idle,
        max_timer_wakes: None,
        ..CLAUDE
    };
    assert_eq!(judge(&idle, counts(0, None, Some(200))), ShapeVerdict::Pass);
    assert_eq!(judge(&idle, counts(0, None, None)), ShapeVerdict::Fail);
}

#[test]
fn a_sleeping_pets_lane_fails_its_row_on_one_fire_or_none_reported() {
    let calm = [(EFFECT_LANE, Some(0))];
    let quiet = Counts {
        owner_fires: &calm,
        ..counts(0, Some(20), Some(80))
    };
    assert_eq!(judge(&PET, quiet), ShapeVerdict::Pass);
    let pet = [(EFFECT_LANE, Some(1))];
    assert_eq!(
        judge(
            &PET,
            Counts {
                owner_fires: &pet,
                ..quiet
            }
        ),
        ShapeVerdict::Fail,
        "a pet that fires in deep sleep, under every other bound"
    );
    let unread = [(EFFECT_LANE, None)];
    assert_eq!(
        judge(
            &PET,
            Counts {
                owner_fires: &unread,
                ..quiet
            }
        ),
        ShapeVerdict::Fail
    );
    assert_eq!(
        judge(&CLAUDE, counts(0, Some(5), Some(200))),
        ShapeVerdict::Pass,
        "a row that bounds no lane passes whatever the lane did"
    );
}

/// The pet row bounds the dial's needle by the frames its readings owed, which the Mac's
/// load sets: a busy take passes where its readings owed its fires, as does an episode in
/// flight as the window opened, and the wakes the needle's fires take are its own. A needle
/// that fires one frame more than its readings owed fails under every other bound, as a wake
/// beyond the needle's own does, and as an unreported count does; the sweeps alone are a
/// record, not a bound.
#[test]
fn the_pet_row_bounds_the_needle_by_the_frames_its_readings_owed() {
    for (what, c, verdict) in [
        ("a quiet take", needle(0, (0, 0), 92), ShapeVerdict::Pass),
        (
            "a quiet take's drift",
            needle(3, (3, 0), 98),
            ShapeVerdict::Pass,
        ),
        (
            "a busy Mac's take",
            needle(46, (51, 0), 184),
            ShapeVerdict::Pass,
        ),
        (
            "an episode in flight",
            needle(46, (30, 16), 184),
            ShapeVerdict::Pass,
        ),
        (
            "one frame past",
            needle(47, (30, 16), 186),
            ShapeVerdict::Fail,
        ),
        (
            "a needle that owes nothing",
            needle(1, (0, 0), 94),
            ShapeVerdict::Fail,
        ),
        (
            "its own two wakes a frame",
            needle(20, (20, 0), 240),
            ShapeVerdict::Pass,
        ),
        (
            "one wake beyond them",
            needle(20, (20, 0), 241),
            ShapeVerdict::Fail,
        ),
    ] {
        assert_eq!(judge(&PET, c), verdict, "{what}");
    }
    let busy = needle(46, (51, 0), 184);
    for unread in [
        NeedleCounts {
            fires: None,
            ..busy.needle
        },
        NeedleCounts {
            owed: None,
            ..busy.needle
        },
        NeedleCounts {
            carried: None,
            ..busy.needle
        },
    ] {
        assert_eq!(
            judge(
                &PET,
                Counts {
                    needle: unread,
                    ..busy
                }
            ),
            ShapeVerdict::Fail,
            "{unread:?}"
        );
    }
    assert_eq!(
        judge(
            &CLAUDE,
            Counts {
                needle: NeedleCounts {
                    fires: Some(100),
                    owed: Some(100),
                    ..busy.needle
                },
                ..counts(0, Some(5), Some(401))
            }
        ),
        ShapeVerdict::Fail,
        "a row that bounds no needle leaves none of its wakes out"
    );
    let opened = "OK past_deadline_arms=3 deadline_fires_by_owner=deck_clock:2 \
                  needle_sweeps=1 needle_frames_owed=8 needle_episode_left=6";
    let ended = "OK past_deadline_arms=3 deadline_fires_by_owner=stats_live:30,deck_clock:11 \
                 needle_sweeps=2 needle_frames_owed=20 needle_episode_left=0";
    assert_eq!(
        needle_counts(opened, ended),
        NeedleCounts {
            fires: Some(9),
            sweeps: Some(1),
            owed: Some(12),
            carried: Some(6),
        },
        "what was left as the window opened, the rest between its edges"
    );
    assert_eq!(
        needle_counts("OK", ended),
        NeedleCounts::default(),
        "an opening read that reports nothing leaves every count unreported"
    );
    assert_eq!(
        fires_between("OK deadline_fires_by_owner=none", ended, EFFECT_LANE),
        Some(0)
    );
    let line = "OK timer_wakes=46 deadline_fires_by_owner=stats_live:30,deck_clock:4,\
                system_vitals:10 wakes_by_variant=none";
    assert_eq!(owner_fires(line, NEEDLE_LANE), Some(4));
    assert_eq!(owner_fires(line, EFFECT_LANE), Some(0));
}

#[test]
fn an_owners_fires_are_read_from_its_pair_and_an_absent_owner_fired_none() {
    let line = "OK timer_wakes=29 deadline_fires_by_owner=blink:19,cursor_effect:44,\
                system_vitals:10 wakes_by_variant=none";
    assert_eq!(owner_fires(line, EFFECT_LANE), Some(44));
    assert_eq!(owner_fires(line, "blink"), Some(19));
    assert_eq!(
        owner_fires(line, "cursor"),
        Some(0),
        "a prefix is not an owner"
    );
    assert_eq!(
        owner_fires("OK deadline_fires_by_owner=none", EFFECT_LANE),
        Some(0)
    );
    assert_eq!(owner_fires("OK timer_wakes=0", EFFECT_LANE), None);
}

#[test]
fn a_pet_take_decides_only_for_a_focused_windows_sleeping_pet_at_full_motion() {
    let ready = "OK 1\ntrail style=\"rainbow\" resolved=rainbow-kitty focused=true \
                 motion=full motion_stage=full pet_active=true pet_action=sleep \
                 pet_deep=true pet_pose=sleep_0";
    assert_eq!(pet_unready(ready), None);
    for (from, to, off) in [
        ("resolved=rainbow-kitty", "resolved=comet", "resolved=comet"),
        ("focused=true", "focused=false", "focused=false"),
        (
            "motion_stage=full",
            "motion_stage=reduced",
            "motion_stage=reduced",
        ),
        ("pet_active=true", "pet_active=false", "pet_active=false"),
        ("pet_action=sleep", "pet_action=loaf", "pet_action=loaf"),
        (" pet_action=sleep", "", "pet_action=?"),
        ("pet_deep=true", "pet_deep=false", "pet_deep=false"),
    ] {
        assert_eq!(
            pet_unready(&ready.replacen(from, to, 1)).as_deref(),
            Some(off),
            "{from} -> {to:?}"
        );
    }
}

#[test]
fn a_pet_take_decides_only_with_no_window_event_its_pet_would_answer() {
    assert!(
        os_events_unidle(SpinScene::PetAsleep, 2).is_some(),
        "a focus lost and regained fades the pet back in"
    );
    assert!(
        os_events_unidle(SpinScene::PetAsleep, 1).is_some(),
        "one event is any"
    );
    assert_eq!(os_events_unidle(SpinScene::PetAsleep, 0), None);
    // CONTROL: the bare windowed scene keeps its allowance.
    assert_eq!(os_events_unidle(SpinScene::Windowed, 2), None);
    assert!(os_events_unidle(SpinScene::Windowed, MAX_OS_EVENTS + 1).is_some());
}

#[test]
fn a_pet_takes_record_names_each_field_and_marks_one_the_reply_lacks() {
    let reply = "OK 1\ntrail style=\"rainbow\" resolved=rainbow-kitty focused=true \
                 motion=full motion_stage=full pet_active=true pet_action=sleep";
    assert_eq!(
        pet_fields(reply),
        "resolved=rainbow-kitty focused=true motion=full motion_stage=full shed=? \
         pet_active=true pet_action=sleep pet_deep=? pet_pose=? pet_reason=? pet_event_seq=? \
         resting=?"
    );
}
