// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::measure::spin::{EFFECT_LANE, NEEDLE_LANE, SpinScene, head};

/// The bench's unattended row, as the decision reads it.
const ROW: SpinRow = SpinRow {
    scene: SpinScene::Unattended,
    recording: true,
    window: Duration::from_secs(60),
    max_arms: 100,
    max_timer_wakes: None,
    max_owner_fires: &[(EFFECT_LANE, 0), (NEEDLE_LANE, 0)],
    needle_owed: false,
    max_wakes: 180,
    max_frames: None,
    min_hidden_parked: None,
};

/// A focused, unattended window on glass whose pet sleeps deep, as `trail status` reads it.
const TRAIL: &str = "OK 1\ntrail style=\"rainbow\" resolved=rainbow-kitty focused=true \
                     motion=auto motion_stage=full shed=1.00 pet_active=true pet_action=sleep \
                     pet_deep=true word_icons=none resting=false attended=false sight=visible \
                     lane_term=none";

/// The opening `metrics` line, read just after `metrics reset`: the reads' own wakes, no fire.
const OPENED: &str = "OK frames=0 past_deadline_arms=0 event_wakes=2 timer_wakes=0 \
                      os_window_events=0 os_window_events_by_kind=none \
                      deadline_fires_by_owner=none";

/// A closing `metrics` line: `frames` frames, `fires` as `deadline_fires_by_owner` pairs (their
/// sum the timer wakes), `events` window events (focus changes) and `wakes` loop wakes since the
/// reset.
fn ended(frames: u64, fires: &str, events: u64, wakes: u64) -> String {
    let kinds = if events == 0 {
        "none".to_string()
    } else {
        format!("focus:{events}")
    };
    let timer_wakes: u64 = fires
        .split(',')
        .filter_map(|pair| pair.split_once(':')?.1.parse::<u64>().ok())
        .sum();
    format!(
        "OK frames={frames} past_deadline_arms=1 event_wakes={wakes} timer_wakes={timer_wakes} \
         os_window_events={events} os_window_events_by_kind={kinds} \
         deadline_fires_by_owner={fires}"
    )
}

/// A quiet window's close: 12 readings and the band's poll once, a frame each, and the crash
/// journal's timers.
fn quiet() -> String {
    ended(13, "system_vitals:13,crash_journal:4", 0, 40)
}

/// The decision of a 60 s window the bench's clock read as at most 60.15 s.
fn take_with(
    ended: &str,
    opened_trail: &str,
    ended_trail: &str,
) -> Result<UnattendedDecision, CouldNotRun> {
    decide(
        &ROW,
        &UnattendedReadings {
            at_least: Duration::from_secs(60),
            at_most: Duration::from_millis(60_150),
            opened: OPENED,
            ended,
            opened_trail,
            ended_trail,
        },
    )
}

fn take(ended: &str) -> Result<UnattendedDecision, CouldNotRun> {
    take_with(ended, TRAIL, TRAIL)
}

fn verdict(ended: &str) -> Disposition {
    take(ended).expect("a decided take").verdict
}

/// The counts of a window that fired the blink `blink` times, the live edge `live` times and
/// every owner `timer_wakes` times, as the bounds read them.
fn fired(blink: u64, live: u64, timer_wakes: u64) -> UnattendedCounts {
    UnattendedCounts {
        owner_fires: Vec::new(),
        live,
        blink,
        readings: 13,
        timer_wakes,
        frames: 0,
        wakes: 0,
        arms: 0,
    }
}

/// The bounds count the slots of each period the window can hold: in 60 s to 60.15 s, at least
/// 12 readings and the band's poll less the one fire another owner's wake may take, at most 13
/// readings and the band's 2 polls, a grey a reading slot and one more, and a frame a reading
/// slot and a poll slot; the blink adds a frame a fire of its own, up to its 114 slots, and
/// admits as late frames the flips other owners' timer wakes may carry, up to the slots left;
/// each flip and grey adds two wakes to the row's 180, and a carried flip one.
#[test]
fn the_bounds_count_the_slots_each_period_has_in_the_window() {
    let at_least = Duration::from_secs(60);
    let at_most = Duration::from_millis(60_150);
    let of = |counts: &UnattendedCounts| UnattendedBounds::of(&ROW, at_least, at_most, counts);
    assert_eq!(
        of(&fired(0, 0, 17)),
        UnattendedBounds {
            readings_min: 12,
            readings_max: 15,
            greys_max: 14,
            flips: 0,
            carried_max: 0,
            frames_max: 15,
            frames_late_max: 15,
            wakes_max: 180,
        }
    );
    assert_eq!(
        of(&fired(20, 0, 33)),
        UnattendedBounds {
            readings_min: 12,
            readings_max: 15,
            greys_max: 14,
            flips: 20,
            carried_max: 13,
            frames_max: 35,
            frames_late_max: 48,
            wakes_max: 233,
        },
        "a blink that ran for part of the window"
    );
    let all = of(&fired(113, 0, 130));
    assert_eq!(
        (
            all.flips,
            all.carried_max,
            all.frames_max,
            all.frames_late_max
        ),
        (113, 1, 128, 129),
        "the blink over the whole window: one slot left to carry"
    );
    let over = of(&fired(200, 0, 213));
    assert_eq!(
        (over.flips, over.carried_max, over.frames_max),
        (114, 0, 129),
        "a blink faster than its period earns no more than its slots"
    );
    let greyed = of(&fired(0, 3, 20));
    assert_eq!(
        (greyed.frames_max, greyed.frames_late_max, greyed.wakes_max),
        (15, 18, 186),
        "three greys, a frame and two wakes each"
    );
    assert_eq!(
        of(&fired(0, 40, 57)).frames_late_max,
        29,
        "greys admitted no further than greys_max"
    );
    assert_eq!(
        UnattendedBounds::of(&ROW, Duration::ZERO, Duration::ZERO, &fired(0, 0, 0)),
        UnattendedBounds {
            readings_min: 0,
            readings_max: 2,
            greys_max: 2,
            flips: 0,
            carried_max: 0,
            frames_max: 2,
            frames_late_max: 2,
            wakes_max: 180,
        },
        "a window of no length holds at most the slot it opens on"
    );
}

/// A quiet window passes: its readings on their cadence, a frame each, no lane the row bounds
/// fired and its live edge still, whether its blink clock ran or not, anywhere inside the
/// readings' fence; its report names its counts and its edges, and says nothing of why.
#[test]
fn a_quiet_unattended_window_passes() {
    let decision = take(&quiet()).expect("decided");
    assert_eq!(decision.verdict, Disposition::Pass, "{decision}");
    assert!(decision.why.is_empty(), "{decision}");
    assert_eq!(
        (
            decision.counts.readings,
            decision.counts.frames,
            decision.counts.blink,
            decision.counts.timer_wakes
        ),
        (13, 13, 0, 17)
    );
    let shown = decision.to_string();
    assert!(
        shown.contains("readings=13 readings_cadence=12..=15"),
        "{shown}"
    );
    assert!(
        shown.contains("opened=[focused=true attended=false sight=visible"),
        "{shown}"
    );
    assert!(!shown.contains("why="), "{shown}");
    assert_eq!(
        verdict(&ended(
            126,
            "blink:113,system_vitals:13,crash_journal:4",
            0,
            200
        )),
        Disposition::Pass,
        "the caret's blink at a reduced motion stage, a frame a flip"
    );
    for readings in [12, 15] {
        assert_eq!(
            verdict(&ended(11, &format!("system_vitals:{readings}"), 0, 40)),
            Disposition::Pass,
            "{readings} readings"
        );
    }
}

/// A deck or an effect that moves unattended fails by the lane's name, whatever else the window
/// did: one `deck_clock` fire, one cursor-effect fire, or a live edge that fired more than a
/// still deck's greys; a starved reading cadence beside it leaves the breach decided.
#[test]
fn a_deck_clock_fire_fails_the_take_by_name() {
    for lane in [NEEDLE_LANE, EFFECT_LANE] {
        let decision =
            take(&ended(13, &format!("{lane}:1,system_vitals:13"), 0, 41)).expect("decided");
        assert_eq!(decision.verdict, Disposition::Fail, "{lane}: {decision}");
        assert!(
            decision
                .why
                .contains(&format!("{lane} fired 1 times, at most 0")),
            "{lane}: {decision}"
        );
    }
    let moved = take(&ended(13, "stats_live:15,system_vitals:13", 0, 41)).expect("decided");
    assert_eq!(moved.verdict, Disposition::Fail, "{moved}");
    assert!(
        moved
            .why
            .contains("stats_live fired 15 times, past the 14 greys"),
        "{moved}"
    );
    let starved = take(&ended(5, "deck_clock:3,system_vitals:5", 0, 20)).expect("decided");
    assert_eq!(starved.verdict, Disposition::Fail, "{starved}");
    assert!(
        starved.why.contains("deck_clock fired 3 times"),
        "{starved}"
    );
}

/// A window whose readings fell behind their cadence is undecided, never a pass, however quiet
/// its other counts: ten readings in a minute, eleven (two slots missed beside the band's poll),
/// or none at all. A build that reports no fires leaves it unanswered.
#[test]
fn a_reading_cadence_that_fell_behind_is_undecided_not_a_pass() {
    for (what, fires) in [
        ("ten readings", "system_vitals:10"),
        ("two slots missed beside the poll", "system_vitals:11"),
        ("no reading", "crash_journal:4"),
    ] {
        let decision = take(&ended(10, fires, 0, 40)).expect("decided");
        assert_eq!(
            decision.verdict,
            Disposition::Unproved,
            "{what}: {decision}"
        );
        assert!(
            decision.why.starts_with("system_vitals fired ")
                && decision.why.contains("under the 12")
                && decision.why.contains("fell behind")
                && decision.why.ends_with("no evidence"),
            "{what}: {decision}"
        );
    }
    let unreported = "OK frames=10 past_deadline_arms=1 event_wakes=40 timer_wakes=10 \
                      os_window_events=0";
    assert_eq!(
        take(unreported),
        Err(CouldNotRun::Unanswered(
            "ctl metrics does not report deadline_fires_by_owner".into()
        ))
    );
}

/// More readings' fires than their asks and the band's polls take fail by the owner's name,
/// never undecided: a loop that falls behind fires fewer, so more is work under the readings'
/// owner, as of a band poll that stayed on its 1 s first look.
#[test]
fn readings_past_their_asks_fail_by_name() {
    for (fires, frames, wakes) in [(16, 13, 42), (73, 13, 85)] {
        let decision =
            take(&ended(frames, &format!("system_vitals:{fires}"), 0, wakes)).expect("decided");
        assert_eq!(decision.verdict, Disposition::Fail, "{fires}: {decision}");
        assert!(
            decision
                .why
                .contains(&format!("system_vitals fired {fires} times, past the 15"))
                && !decision.why.contains("fell behind"),
            "{fires}: {decision}"
        );
    }
}

/// A still deck's grey leaves the take undecided, never failed: one live-edge fire with the
/// readings on their cadence, its frame past what the readings ask, or as many greys as the
/// window's reading slots and one more. A build that reports no timer wakes leaves the take
/// unanswered.
#[test]
fn a_stale_grey_leaves_the_take_undecided() {
    for (what, frames, live) in [
        ("one grey", 14, 1),
        ("one grey and its frame past the readings'", 16, 1),
        ("a grey a reading slot and one more", 13, 14),
    ] {
        let decision = take(&ended(
            frames,
            &format!("stats_live:{live},system_vitals:13"),
            0,
            42,
        ))
        .expect("decided");
        assert_eq!(
            decision.verdict,
            Disposition::Unproved,
            "{what}: {decision}"
        );
        assert!(
            decision.why.contains(&format!(
                "stats_live fired {live} times: a still deck greys 15s"
            )),
            "{what}: {decision}"
        );
    }
    let unreported = "OK frames=13 past_deadline_arms=1 event_wakes=40 os_window_events=0 \
                      deadline_fires_by_owner=system_vitals:13";
    assert_eq!(
        take(unreported),
        Err(CouldNotRun::Unanswered(
            "ctl metrics does not report timer_wakes".into()
        ))
    );
}

/// Frames past what the blink's own fires, the readings and the band's polls ask fail by name
/// where nothing late explains them: one past with the blink off, one past what the blink's
/// fires and the one flip another wake may carry admit, a frame per reading on a doubled
/// cadence, and a blink of one fire that cannot explain the frames of a whole window. Frames that
/// only flips another owner's wake carried explain leave the take undecided: one past the
/// blink's own over the whole window, and a blink that ran for part of it beside two frames a
/// reading. A build that reports no frames leaves it unanswered.
#[test]
fn a_frame_count_past_its_bound_fails() {
    assert_eq!(
        verdict(&ended(15, "system_vitals:13", 0, 40)),
        Disposition::Pass
    );
    let past = take(&ended(16, "system_vitals:13", 0, 40)).expect("decided");
    assert_eq!(past.verdict, Disposition::Fail, "{past}");
    assert!(past.why.contains("frames=16, past the 15"), "{past}");
    assert_eq!(
        verdict(&ended(128, "blink:113,system_vitals:13", 0, 200)),
        Disposition::Pass
    );
    let carried = take(&ended(129, "blink:113,system_vitals:13", 0, 200)).expect("decided");
    assert_eq!(carried.verdict, Disposition::Unproved, "{carried}");
    assert!(
        carried.why.contains("frames=129, past the 128"),
        "{carried}"
    );
    let over = take(&ended(130, "blink:113,system_vitals:13", 0, 200)).expect("decided");
    assert_eq!(
        over.verdict,
        Disposition::Fail,
        "one frame past the blink's flips, the carried flip, the readings and the band: {over}"
    );
    assert!(over.why.contains("frames=130, past the 129"), "{over}");
    assert_eq!(
        verdict(&ended(44, "blink:20,system_vitals:13", 0, 200)),
        Disposition::Unproved,
        "a blink for part of the window beside two frames a reading"
    );
    let one_flip = take(&ended(60, "blink:1,system_vitals:13", 0, 200)).expect("decided");
    assert_eq!(one_flip.verdict, Disposition::Fail, "{one_flip}");
    assert!(
        one_flip.why.contains("frames=60, past the 29"),
        "{one_flip}"
    );
    let doubled = take(&ended(25, "system_vitals:25", 0, 80)).expect("decided");
    assert_eq!(doubled.verdict, Disposition::Fail, "{doubled}");
    assert!(doubled.why.contains("frames=25"), "{doubled}");
    let no_frames = "OK past_deadline_arms=1 event_wakes=40 timer_wakes=13 os_window_events=0 \
                     deadline_fires_by_owner=system_vitals:13";
    assert_eq!(
        take(no_frames),
        Err(CouldNotRun::Unanswered(
            "ctl metrics does not report frames".into()
        ))
    );
}

/// Loop wakes past the row's bound and what the window's flips and greys take fail, though no
/// frame came of them: a 100 ms poller under an owner the row names no bound for, and one wake
/// past the bound with and without the blink.
#[test]
fn a_frameless_poller_fails_on_its_wakes() {
    let poller = take(&ended(13, "system_vitals:13,session_status:600", 0, 680)).expect("decided");
    assert_eq!(poller.verdict, Disposition::Fail, "{poller}");
    assert!(
        poller.why.contains("event_wakes=680, past the 180"),
        "{poller}"
    );
    assert_eq!(
        verdict(&ended(13, "system_vitals:13", 0, 180)),
        Disposition::Pass
    );
    assert_eq!(
        verdict(&ended(13, "system_vitals:13", 0, 181)),
        Disposition::Fail
    );
    assert_eq!(
        verdict(&ended(128, "blink:113,system_vitals:13", 0, 407)),
        Disposition::Pass,
        "180, two a flip and one the carried flip"
    );
    assert_eq!(
        verdict(&ended(128, "blink:113,system_vitals:13", 0, 408)),
        Disposition::Fail
    );
}

/// A window event in the window refuses the take, as does an edge whose window is not focused,
/// unattended, on glass and with its pet asleep or not drawn; a pet the window does not draw is
/// still; a build whose replies lack the fields leaves the take unanswered.
#[test]
fn a_window_event_refuses_the_take() {
    match take(&ended(13, "system_vitals:13", 1, 40)) {
        Err(CouldNotRun::Refused(why)) => {
            assert!(
                why.contains("took 1 event other than a redraw (focus:1)"),
                "{why}"
            );
        }
        other => panic!("one window event: {other:?}"),
    }
    for (from, to, off) in [
        ("attended=false", "attended=true", "attended=true"),
        ("focused=true", "focused=false", "focused=false"),
        ("sight=visible", "sight=covered", "sight=covered"),
        (
            "pet_deep=true",
            "pet_deep=false",
            "pet_active=true pet_deep=false",
        ),
    ] {
        let lost = TRAIL.replacen(from, to, 1);
        for (edge, result) in [
            ("opened", take_with(&quiet(), &lost, TRAIL)),
            ("ended", take_with(&quiet(), TRAIL, &lost)),
        ] {
            match result {
                Err(CouldNotRun::Refused(why)) => assert!(
                    why.contains(&format!("read {off} as its window {edge}")),
                    "{from} -> {to} at {edge}: {why}"
                ),
                other => panic!("{from} -> {to} at {edge}: {other:?}"),
            }
        }
    }
    let undrawn = TRAIL
        .replacen("pet_active=true", "pet_active=false", 1)
        .replacen("pet_deep=true", "pet_deep=false", 1);
    assert_eq!(
        take_with(&quiet(), &undrawn, &undrawn).map(|d| d.verdict),
        Ok(Disposition::Pass)
    );
    let older = TRAIL.replacen(" attended=false", "", 1);
    assert_eq!(
        take_with(&quiet(), &older, TRAIL),
        Err(CouldNotRun::Unanswered(
            "ctl trail status does not report attended".into()
        ))
    );
    assert_eq!(
        take("OK frames=13 past_deadline_arms=1 event_wakes=40 deadline_fires_by_owner=none"),
        Err(CouldNotRun::Unanswered(
            "ctl metrics does not report os_window_events".into()
        ))
    );
}

/// The unattended scene runs at the shipped motion, rest and effect defaults: its head writes
/// its recording and no other setting, so it measures those defaults if one moves. The scratch
/// world the take runs in adds its own lines (`World::prepare`: a silent bell, the lanes off).
#[test]
fn an_unattended_head_sets_nothing_but_its_recording() {
    let head = head(&ROW);
    let settings: Vec<&str> = head
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .collect();
    assert_eq!(settings, ["temporal_recording = true"], "{head}");
    assert!(SpinScene::Unattended.windowed(), "it opens a window");
}
