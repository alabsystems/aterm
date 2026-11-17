// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The glance word `raw` names.
fn glance(raw: &str) -> State {
    State::ALL
        .into_iter()
        .find(|state| state.word() == raw)
        .unwrap_or_else(|| panic!("{raw:?} is no glance word"))
}

/// The task word `raw` names, `-` for none.
fn task(raw: &str) -> Option<Task> {
    (raw != "-").then(|| {
        Task::ALL
            .into_iter()
            .find(|task| task.word() == raw)
            .unwrap_or_else(|| panic!("{raw:?} is no task word"))
    })
}

/// The verdict `raw` names, `unread` for none.
fn verdict(raw: &str) -> Option<TrackWhy> {
    (raw != "unread")
        .then(|| TrackWhy::of(raw).unwrap_or_else(|| panic!("{raw:?} is no reason word")))
}

/// The activity `raw` names, `?` for the unread cell.
fn expected(raw: &str) -> Option<Activity> {
    (raw != "?").then(|| Activity::of(raw).unwrap_or_else(|| panic!("{raw:?} is no activity")))
}

/// Whether `seen` under `verdict` is one of the rows that read `stuck`: a failed or stalled
/// task, the glance's own `stuck`, or a blocked or stalled verdict, whatever the glance.
fn stuck_holds(seen: &Seen, verdict: Option<TrackWhy>) -> bool {
    matches!(seen.task, Some(Task::Failed | Task::Stalled))
        || seen.glance == State::Stuck
        || matches!(verdict, Some(TrackWhy::Blocked | TrackWhy::Stalled))
}

/// TODAY'S WIRE WORDS MAP TO THE FIVE ACTIVITY WORDS (§5.6's activity table, its third column):
/// one row per entry of that column, each the glance word, the word with the need set aside,
/// the task word, whether it ended and the verdict, then over every combination of them,
/// `ended` exactly where the row ended and `stuck` exactly where a failed or stalled task, the
/// glance's `stuck`, or a blocked or stalled verdict holds. CONTROL: a waiting row at work reads
/// `working` under every verdict that is neither `blocked` nor `stalled` (`failed`, `check-red`
/// and `repeat` included) and `quiet` with the verdict unread, so the verdict and never the need
/// gives `stuck`; the same row read by its glance word alone, its need never set aside, reads
/// `quiet` under each of those verdicts and never `stuck`.
#[test]
fn todays_wire_words_map_to_the_five_activity_words() {
    let table = [
        ("busy", "busy", "-", false, "unread", "working"),
        ("waiting", "busy", "working", false, "progress", "working"),
        ("walled", "walled", "-", false, "unread", "quiet"),
        ("waiting", "walled", "-", false, "walled", "quiet"),
        ("waiting", "busy", "-", false, "unread", "quiet"),
        ("waiting", "idle", "-", false, "blocked", "stuck"),
        ("waiting", "busy", "working", false, "stalled", "stuck"),
        ("stuck", "busy", "stalled", false, "unread", "stuck"),
        ("busy", "busy", "failed", false, "unread", "stuck"),
        ("walled", "walled", "stalled", false, "unread", "stuck"),
        ("busy", "busy", "-", false, "stalled", "stuck"),
        ("walled", "walled", "-", false, "blocked", "stuck"),
        ("idle", "idle", "-", false, "blocked", "stuck"),
        ("walled", "walled", "-", false, "walled", "quiet"),
        ("idle", "idle", "-", false, "unread", "idle"),
        ("waiting", "idle", "-", false, "progress", "idle"),
        ("busy", "busy", "working", true, "unread", "ended"),
        ("idle", "idle", "done", true, "landed", "ended"),
        ("unknown", "unknown", "-", false, "unread", "?"),
        ("waiting", "unknown", "-", false, "progress", "?"),
    ];
    let mut read = Vec::new();
    for (g, aside, t, ended, v, want) in table {
        let seen = Seen {
            glance: glance(g),
            aside: glance(aside),
            task: task(t),
            ended,
        };
        let got = activity(&seen, verdict(v));
        assert_eq!(
            got,
            expected(want),
            "{g} (aside {aside}), task {t}, ended {ended}, verdict {v}"
        );
        read.push(got);
    }
    for word in Activity::ALL.map(Some).into_iter().chain([None]) {
        assert!(read.contains(&word), "no row of the table reads {word:?}");
    }

    let verdicts: Vec<Option<TrackWhy>> = std::iter::once(None)
        .chain(TrackWhy::ALL.map(Some))
        .collect();
    let tasks: Vec<Option<Task>> = std::iter::once(None).chain(Task::ALL.map(Some)).collect();
    let asides = [State::Busy, State::Idle, State::Walled, State::Unknown];
    let mut combinations = 0;
    for glance in State::ALL {
        for aside in asides {
            for &task in &tasks {
                for ended in [false, true] {
                    for &verdict in &verdicts {
                        let seen = Seen {
                            glance,
                            aside,
                            task,
                            ended,
                        };
                        let got = activity(&seen, verdict);
                        assert_eq!(
                            got == Some(Activity::Ended),
                            ended,
                            "{seen:?} under {verdict:?} reads {got:?}"
                        );
                        assert_eq!(
                            got == Some(Activity::Stuck),
                            !ended && stuck_holds(&seen, verdict),
                            "{seen:?} under {verdict:?} reads {got:?}"
                        );
                        combinations += 1;
                    }
                }
            }
        }
    }
    assert_eq!(
        combinations,
        6 * 4 * 6 * 2 * 12,
        "the whole product was read"
    );

    let at_work = Seen {
        glance: State::Waiting,
        aside: State::Busy,
        task: Some(Task::Working),
        ended: false,
    };
    let by_its_glance_alone = Seen {
        aside: State::Waiting,
        ..at_work
    };
    assert_eq!(activity(&at_work, None), Some(Activity::Quiet));
    assert_eq!(activity(&by_its_glance_alone, None), Some(Activity::Quiet));
    let mut blocking = 0;
    for why in TrackWhy::ALL {
        if matches!(why, TrackWhy::Blocked | TrackWhy::Stalled) {
            assert_eq!(activity(&at_work, Some(why)), Some(Activity::Stuck));
            blocking += 1;
            continue;
        }
        assert_eq!(
            activity(&at_work, Some(why)),
            Some(Activity::Working),
            "a need that blocks nothing under {why:?} leaves the agent at work"
        );
        assert_eq!(
            activity(&by_its_glance_alone, Some(why)),
            Some(Activity::Quiet),
            "a wait read by its glance alone under {why:?}"
        );
    }
    assert_eq!(
        blocking, 2,
        "only `blocked` and `stalled` make a wait stuck"
    );
    for why in [TrackWhy::Failed, TrackWhy::CheckRed, TrackWhy::Repeat] {
        assert_eq!(why.track(), Track::OffTrack, "{why:?} is off track");
        assert_ne!(activity(&at_work, Some(why)), Some(Activity::Stuck));
    }
}

/// THE WORDS ARE §5.5'S SCHEMA LINES, IN THEIR ORDER: each word reads back as its value, each
/// `ALL` is its `WORDS` in order, and each reason names its track word by §5.6's track table.
/// CONTROL: a word no list holds, `-` and the empty word read as none.
#[test]
fn the_track_and_activity_words_round_trip_in_the_schemas_order() {
    use Track::{AtRisk, Done, OffTrack, OnTrack, Untracked};
    assert_eq!(
        Track::WORDS,
        ["on-track", "at-risk", "off-track", "done", "none"]
    );
    assert_eq!(
        TrackWhy::WORDS,
        [
            "failed",
            "check-red",
            "stalled",
            "blocked",
            "repeat",
            "no-progress",
            "walled",
            "flagged",
            "progress",
            "landed",
            "none"
        ]
    );
    assert_eq!(
        Activity::WORDS,
        ["working", "quiet", "idle", "ended", "stuck"]
    );
    assert_eq!(Track::ALL.map(Track::word), Track::WORDS);
    assert_eq!(TrackWhy::ALL.map(TrackWhy::word), TrackWhy::WORDS);
    assert_eq!(Activity::ALL.map(Activity::word), Activity::WORDS);
    assert_eq!(State::ALL.map(State::word), State::WORDS);
    assert_eq!(Task::ALL.map(Task::word), Task::WORDS);
    for track in Track::ALL {
        assert_eq!(Track::of(track.word()), Some(track));
    }
    for why in TrackWhy::ALL {
        assert_eq!(TrackWhy::of(why.word()), Some(why));
    }
    for activity in Activity::ALL {
        assert_eq!(Activity::of(activity.word()), Some(activity));
    }

    let tracks = TrackWhy::ALL.map(TrackWhy::track);
    assert_eq!(
        tracks,
        [
            OffTrack, OffTrack, OffTrack, OffTrack, OffTrack, AtRisk, AtRisk, AtRisk, OnTrack,
            Done, Untracked
        ]
    );

    for raw in ["maybe", "-", "", "Off-Track", " none"] {
        assert_eq!(Track::of(raw), None, "{raw:?}");
        assert_eq!(TrackWhy::of(raw), None, "{raw:?}");
        assert_eq!(Activity::of(raw), None, "{raw:?}");
    }
}
