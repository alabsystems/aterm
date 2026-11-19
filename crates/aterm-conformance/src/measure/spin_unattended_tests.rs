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

/// The `metrics` line read just after the readiness wait's last read, since the `metrics reset`
/// just before it: that read's own wake, a desk mark, and no fire yet.
const READY: &str = "OK frames=0 past_deadline_arms=0 event_wakes=1 timer_wakes=0 \
                     os_window_events=0 os_window_events_by_kind=none desk_activity=1 \
                     deadline_fires_by_owner=none max_wake_late_ms=0.40 max_turn_ms=2.10 \
                     wakes_by_variant=TrailStatus:1";

/// The `metrics` line as the window opened, the settle after [`READY`]: a reading and its frame,
/// and the journal's settle capture and the facts' read the ready read owed, with no mark more.
const OPENED: &str = "OK frames=1 past_deadline_arms=0 event_wakes=4 timer_wakes=3 \
                      os_window_events=0 os_window_events_by_kind=none desk_activity=1 \
                      deadline_fires_by_owner=crash_journal:1,disk_facts:1,system_vitals:1 \
                      max_wake_late_ms=0.40 max_turn_ms=2.10 \
                      wakes_by_variant=TrailStatus:1,Vitals:1";

/// What a window did from [`OPENED`] to its close, which [`Window::line`] adds to that line's
/// counts as the closing `metrics` line holds them, since the reset.
#[derive(Clone, Copy)]
struct Window<'a> {
    /// Frames presented.
    frames: u64,
    /// `deadline_fires_by_owner` pairs; their sum is the timer wakes.
    fires: &'a str,
    /// Window events since the reset (focus changes).
    events: u64,
    /// Loop wakes.
    wakes: u64,
    /// Desk marks.
    marks: u64,
    /// The latest `max_wake_late_ms` and `max_turn_ms` since the reset.
    late_ms: (&'a str, &'a str),
    /// The census since the reset.
    census: &'a str,
}

impl Window<'_> {
    /// The closing line, from [`OPENED`]'s counts and this window's.
    fn line(self) -> String {
        let counted = |key: &str| uint_field(OPENED, key).expect("an OPENED count");
        let mut fires: Vec<(String, u64)> = field(OPENED, "deadline_fires_by_owner")
            .expect("OPENED's fires")
            .split(',')
            .filter_map(|pair| {
                let (owner, n) = pair.split_once(':')?;
                Some((owner.to_string(), n.parse().ok()?))
            })
            .collect();
        let mut timer_wakes = 0;
        for pair in self.fires.split(',').filter(|pair| !pair.is_empty()) {
            let (owner, n) = pair.split_once(':').expect("owner:count");
            let n: u64 = n.parse().expect("a count");
            timer_wakes += n;
            match fires.iter_mut().find(|(o, _)| o == owner) {
                Some((_, total)) => *total += n,
                None => fires.push((owner.to_string(), n)),
            }
        }
        let fires = fires
            .iter()
            .map(|(owner, n)| format!("{owner}:{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let kinds = if self.events == 0 {
            "none".to_string()
        } else {
            format!("focus:{}", self.events)
        };
        format!(
            "OK frames={} past_deadline_arms=1 event_wakes={} timer_wakes={} \
             os_window_events={} os_window_events_by_kind={kinds} desk_activity={} \
             deadline_fires_by_owner={fires} max_wake_late_ms={} max_turn_ms={} \
             wakes_by_variant={}",
            counted("frames") + self.frames,
            counted("event_wakes") + self.wakes,
            counted("timer_wakes") + timer_wakes,
            self.events,
            counted("desk_activity") + self.marks,
            self.late_ms.0,
            self.late_ms.1,
            self.census
        )
    }
}

/// A window of `frames` frames, `fires`, `events` window events and `wakes` loop wakes, with one
/// desk mark (a host note's) and a loop on its timers.
fn window(frames: u64, fires: &str, events: u64, wakes: u64) -> Window<'_> {
    Window {
        frames,
        fires,
        events,
        wakes,
        marks: 1,
        late_ms: ("0.40", "2.10"),
        census: "Vitals:14,HostNote.ProgramCheckUnstuck:1,TrailStatus:1",
    }
}

/// [`window`]'s closing line.
fn ended(frames: u64, fires: &str, events: u64, wakes: u64) -> String {
    window(frames, fires, events, wakes).line()
}

/// A quiet window's close: 12 readings and the band's poll once, a frame each, and the journal's
/// settle and the facts' read its host note owes.
fn quiet() -> String {
    ended(13, "system_vitals:13,crash_journal:1,disk_facts:1", 0, 40)
}

/// The decision of a 60 s window the bench's clock read as at most 60.15 s, opened on `opened`.
fn decide_all(
    ended: &str,
    ready_trail: &str,
    ended_trail: &str,
    opened: &str,
) -> Result<UnattendedDecision, CouldNotRun> {
    decide(
        &ROW,
        &UnattendedReadings {
            at_least: Duration::from_secs(60),
            at_most: Duration::from_millis(60_150),
            ready_trail,
            ready: READY,
            opened,
            ended,
            ended_trail,
        },
    )
}

fn take_with(
    ended: &str,
    ready_trail: &str,
    ended_trail: &str,
) -> Result<UnattendedDecision, CouldNotRun> {
    decide_all(ended, ready_trail, ended_trail, OPENED)
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
        journal: 0,
        facts: 0,
        marks: 1,
        late: Duration::ZERO,
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
            journal_max: 2,
            facts_max: 1,
            journal_spilled: 0,
            facts_spilled: 0,
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
            journal_max: 2,
            facts_max: 1,
            journal_spilled: 0,
            facts_spilled: 0,
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
            journal_max: 2,
            facts_max: 1,
            journal_spilled: 0,
            facts_spilled: 0,
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
        (13, 13, 0, 15)
    );
    let shown = decision.to_string();
    assert!(
        shown.contains("readings=13 readings_cadence=12..=15"),
        "{shown}"
    );
    assert!(
        shown.contains(
            "marks=1 late=2.5ms journal=1 journal_max=2 journal_spilled=0 facts=1 facts_max=1 \
             facts_spilled=0"
        ),
        "the settle's reading and the ready read's own work owe the window nothing: {shown}"
    );
    assert!(
        shown.contains("ready=[focused=true attended=false sight=visible"),
        "{shown}"
    );
    assert!(!shown.contains("why="), "{shown}");
    assert_eq!(
        verdict(&ended(
            126,
            "blink:113,system_vitals:13,crash_journal:1",
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
        ("no reading", "crash_journal:1"),
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
/// unattended, on glass and with its pet asleep or not drawn: the readiness wait's last read,
/// which stands for the opening edge, or the closing read. A pet the window does not draw is
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
            (
                "as the readiness wait read it, before its settle",
                take_with(&quiet(), &lost, TRAIL),
            ),
            ("as its window ended", take_with(&quiet(), TRAIL, &lost)),
        ] {
            match result {
                Err(CouldNotRun::Refused(why)) => assert!(
                    why.contains(&format!("read {off} {edge}")),
                    "{from} -> {to} {edge}: {why}"
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

/// The crash journal and the disk facts fire for the desk marks, never on their own: two journal
/// fires a mark at most (a capture deferred to its write interval, and its settle) and one read
/// of the panes' facts. One host note's are a pass, and past them each fails by its owner's name;
/// a window with no mark owes none, whatever its census names, a wake that only observes owing
/// nothing (a surface ready to draw, a reading). Lines that lack the marks or the lateness leave
/// the take unanswered.
#[test]
fn the_journal_and_the_facts_fire_only_for_the_marks_that_owe_them() {
    let with = |fires: &str, marks: u64, census: &str| {
        let fires = if fires.is_empty() {
            "system_vitals:13".to_string()
        } else {
            format!("system_vitals:13,{fires}")
        };
        take(
            &Window {
                marks,
                census,
                ..window(13, &fires, 0, 40)
            }
            .line(),
        )
    };
    let note = "Vitals:14,HostNote.ProgramCheckUnstuck:1,TrailStatus:1";
    let owed = with("crash_journal:1,disk_facts:1", 1, note).expect("decided");
    assert_eq!(owed.verdict, Disposition::Pass, "{owed}");
    assert_eq!(
        (owed.counts.journal, owed.counts.facts, owed.counts.marks),
        (1, 1, 1),
        "the settle's fires are the ready read's own, outside the window"
    );
    assert_eq!((owed.bounds.journal_max, owed.bounds.facts_max), (2, 1));
    assert_eq!(
        with("crash_journal:2,disk_facts:1", 1, note).map(|d| d.verdict),
        Ok(Disposition::Pass),
        "a capture deferred to its write interval, and its settle"
    );
    for (fires, lane, n, max) in [
        ("crash_journal:4,disk_facts:1", "crash_journal", 4, 2),
        ("crash_journal:1,disk_facts:4", "disk_facts", 4, 1),
        ("crash_journal:3", "crash_journal", 3, 2),
        ("disk_facts:2", "disk_facts", 2, 1),
    ] {
        let decision = with(fires, 1, note).expect("decided");
        assert_eq!(decision.verdict, Disposition::Fail, "{fires}: {decision}");
        assert!(
            decision.why.contains(&format!(
                "{lane} fired {n} times, past the {max} its 1 desk marks in the settle and the \
                 window owe"
            )) && decision.why.ends_with("work no mark asked"),
            "{fires}: {decision}"
        );
    }
    let observed = "Vitals:14,GpuSurfaceReady:13,HostNote.DiskLooked:1,TrailStatus:1";
    for (what, census) in [
        ("readings alone", "Vitals:14,TrailStatus:1"),
        ("observations", observed),
    ] {
        let alone = with("crash_journal:1", 0, census).expect("decided");
        assert_eq!(alone.verdict, Disposition::Fail, "{what}: {alone}");
        assert!(
            alone
                .why
                .contains("crash_journal fired 1 times, past the 0"),
            "{what}: {alone}"
        );
        assert_eq!(
            with("", 0, census).map(|d| d.verdict),
            Ok(Disposition::Pass),
            "{what}, no fire"
        );
    }
    assert_eq!(
        with("crash_journal:4,disk_facts:2", 2, note).map(|d| d.verdict),
        Ok(Disposition::Pass),
        "two marks"
    );
    let unmarked = quiet().replace(" desk_activity=2", "");
    assert!(!unmarked.contains("desk_activity"), "{unmarked}");
    assert_eq!(
        take(&unmarked),
        Err(CouldNotRun::Unanswered(
            "ctl metrics does not report desk_activity".into()
        ))
    );
    let unread = READY.replace(" desk_activity=1", "");
    assert_eq!(
        decide(
            &ROW,
            &UnattendedReadings {
                at_least: Duration::from_secs(60),
                at_most: Duration::from_millis(60_150),
                ready_trail: TRAIL,
                ready: &unread,
                opened: OPENED,
                ended: &quiet(),
                ended_trail: TRAIL,
            },
        ),
        Err(CouldNotRun::Unanswered(
            "ctl metrics does not report desk_activity".into()
        )),
        "the line the marks count from"
    );
    let unturned = quiet().replace(" max_turn_ms=2.10", "");
    assert_eq!(
        take(&unturned),
        Err(CouldNotRun::Unanswered(
            "ctl metrics does not report max_wake_late_ms and max_turn_ms".into()
        ))
    );
}

/// A mark in the settle may book its work in the window, so it owes there: a host note in the
/// settle beside the window's admits four journal fires and two reads, and no more.
#[test]
fn a_mark_in_the_settle_owes_its_work_in_the_window() {
    let noted = OPENED.replacen("desk_activity=1", "desk_activity=2", 1);
    let close = |fires: &str| {
        Window {
            marks: 2,
            ..window(13, fires, 0, 40)
        }
        .line()
    };
    let owed = decide_all(
        &close("system_vitals:13,crash_journal:4,disk_facts:2"),
        TRAIL,
        TRAIL,
        &noted,
    )
    .expect("decided");
    assert_eq!(owed.verdict, Disposition::Pass, "{owed}");
    assert_eq!(
        (
            owed.counts.marks,
            owed.bounds.journal_max,
            owed.bounds.facts_max
        ),
        (2, 4, 2)
    );
    for (fires, lane, n, max) in [
        ("crash_journal:5,disk_facts:2", "crash_journal", 5, 4),
        ("crash_journal:4,disk_facts:3", "disk_facts", 3, 2),
    ] {
        let decision = decide_all(
            &close(&format!("system_vitals:13,{fires}")),
            TRAIL,
            TRAIL,
            &noted,
        )
        .expect("decided");
        assert_eq!(decision.verdict, Disposition::Fail, "{fires}: {decision}");
        assert!(
            decision.why.contains(&format!(
                "{lane} fired {n} times, past the {max} its 2 desk marks in the settle and the \
                 window owe"
            )),
            "{fires}: {decision}"
        );
    }
}

/// The work the marks before the settle owe ends inside it on a loop that ran less late than its
/// steps leave room for: the journal's `k`th step of 2 s and the lateness lands in the window
/// from `k·(2 s + late) ≥ 5 s`, its second from 0.5 s late and both from 3 s, and the facts' one
/// of 1 s from 4 s. Up to the first, journal or facts fires past the window's marks fail; past
/// it, fires by no more than the steps that may land there leave the take undecided, the late
/// loop named, and more still fail it, as does the other lane's breach. The lateness is the
/// closing line's slowest timer wake and longest turn together.
#[test]
fn work_owed_before_the_settle_is_no_evidence_on_a_late_loop() {
    for (steps, step, late_us, n) in [
        (2, MIN_WRITE_INTERVAL, 499_999, 0),
        (2, MIN_WRITE_INTERVAL, 500_000, 1),
        (2, MIN_WRITE_INTERVAL, 2_999_999, 1),
        (2, MIN_WRITE_INTERVAL, 3_000_000, 2),
        (2, MIN_WRITE_INTERVAL, u64::MAX, 2),
        (1, FACTS_EVERY, 3_999_999, 0),
        (1, FACTS_EVERY, 4_000_000, 1),
        (0, FACTS_EVERY, 4_000_000, 0),
        (2, Duration::ZERO, 0, 0),
    ] {
        assert_eq!(
            spilled(steps, step, Duration::from_micros(late_us)),
            n,
            "{steps} steps of {step:?} on a loop {late_us} µs late"
        );
    }
    assert_eq!(
        late("OK max_wake_late_ms=249.99 max_turn_ms=250.00"),
        Some(Duration::from_micros(499_990))
    );
    let on = |late_ms: (&'static str, &'static str), fires: &str| {
        take(
            &Window {
                marks: 0,
                late_ms,
                ..window(13, &format!("system_vitals:13,{fires}"), 0, 40)
            }
            .line(),
        )
        .expect("decided")
    };
    let on_time = on(("249.99", "250.00"), "crash_journal:1");
    assert_eq!(on_time.verdict, Disposition::Fail, "{on_time}");
    assert!(
        on_time
            .why
            .contains("crash_journal fired 1 times, past the 0")
            && on_time.why.ends_with("work no mark asked"),
        "{on_time}"
    );
    let late_loop = on(("250.00", "250.00"), "crash_journal:1");
    assert_eq!(late_loop.verdict, Disposition::Unproved, "{late_loop}");
    assert_eq!(late_loop.bounds.journal_spilled, 1, "{late_loop}");
    assert!(
        late_loop.why.contains(
            "crash_journal fired 1 times, past the 0 its 0 desk marks in the settle and the \
             window owe (2 a mark: a capture and its settle), by no more than the 1 the marks \
             before the settle may book there on a loop that ran up to 500ms late"
        ) && late_loop.why.ends_with("no evidence"),
        "{late_loop}"
    );
    let past = on(("250.00", "250.00"), "crash_journal:2");
    assert_eq!(past.verdict, Disposition::Fail, "{past}");
    assert!(
        past.why.contains(
            "crash_journal fired 2 times, past the 0 its 0 desk marks in the settle and the \
             window owe (2 a mark: a capture and its settle) and the 1 the marks before the \
             settle may book there on a loop that ran up to 500ms late: work no mark asked"
        ),
        "{past}"
    );
    assert_eq!(
        on(("3000.00", "0.00"), "crash_journal:2").verdict,
        Disposition::Unproved,
        "both of the journal's steps"
    );
    let rearmed = on(("3000.00", "0.00"), "crash_journal:24");
    assert_eq!(
        rearmed.verdict,
        Disposition::Fail,
        "a journal re-armed every 2.5 s fails however late its loop ran: {rearmed}"
    );
    assert!(
        rearmed.why.contains("crash_journal fired 24 times"),
        "{rearmed}"
    );
    for (late_ms, fires, verdict) in [
        (("250.00", "250.00"), "disk_facts:1", Disposition::Fail),
        (("3999.99", "0.00"), "disk_facts:1", Disposition::Fail),
        (
            ("3000.00", "1000.00"),
            "disk_facts:1",
            Disposition::Unproved,
        ),
        (("3000.00", "1000.00"), "disk_facts:2", Disposition::Fail),
        (("3000.00", "1000.00"), "disk_facts:12", Disposition::Fail),
    ] {
        assert_eq!(
            on(late_ms, fires).verdict,
            verdict,
            "{fires} at {late_ms:?}"
        );
    }
    let both = on(("250.00", "250.00"), "crash_journal:1,disk_facts:1");
    assert_eq!(both.verdict, Disposition::Fail, "{both}");
    assert!(
        both.why.contains("disk_facts fired 1 times, past the 0")
            && !both.why.contains("crash_journal"),
        "the journal's excess is no evidence, the facts' still a breach: {both}"
    );
}

/// A window event from the reset before the readiness wait's last read to the window's end
/// refuses the take, wherever it came: the closing line's count holds the settle's, so one there
/// refuses as one in the window does.
#[test]
fn a_window_event_in_the_settle_refuses_the_take() {
    let touched = OPENED.replacen(
        "os_window_events=0 os_window_events_by_kind=none",
        "os_window_events=1 os_window_events_by_kind=focus:1",
        1,
    );
    match decide_all(
        &ended(13, "system_vitals:13", 1, 40),
        TRAIL,
        TRAIL,
        &touched,
    ) {
        Err(CouldNotRun::Refused(why)) => assert!(
            why.contains(
                "took 1 event other than a redraw (focus:1) from just before the \
                          readiness wait's last read"
            ),
            "{why}"
        ),
        other => panic!("one event in the settle: {other:?}"),
    }
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
