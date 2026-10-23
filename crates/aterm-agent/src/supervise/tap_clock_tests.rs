// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The usage tap's clock at a wall (`tap_clock.rs`): which window times which
// wall, which reading counts, what times and what rolls over, and the notice
// and the tap together. Each rule has its negative control.

use super::*;
use crate::harness::tap::{Fresh, Reading};

/// The wall was first read here (unix seconds).
const FIRST: i64 = 1_790_000_000;
/// The clock of every step below: a minute after the first read.
const NOW: i64 = FIRST + 60;

fn full(pct: u32, resets_at: i64, read_at: i64) -> Option<TapWindow> {
    Some(TapWindow {
        figure: Some(Window {
            pct,
            resets_at,
            pct_x10: Some(pct * 10),
        }),
        read_at,
    })
}

fn dropped(read_at: i64) -> Option<TapWindow> {
    Some(TapWindow {
        figure: None,
        read_at,
    })
}

/// A clock of a wall timed by `windows`, the readings `steps` folded in at
/// [`NOW`], and what it says.
fn says(windows: &[UsageWindow], steps: &[&[(UsageWindow, Option<TapWindow>)]]) -> TapSays {
    let mut c = TapClock::new(FIRST);
    for step in steps {
        c.step(step, NOW);
    }
    c.says(windows)
}

const FIVE: UsageWindow = UsageWindow::FiveHour;
const WEEK: UsageWindow = UsageWindow::SevenDay;

/// WHICH WINDOW TIMES WHICH WALL: a session window named, the five-hour one;
/// a weekly wall, the seven-day one; a usage limit that names no window,
/// both; nothing else — a spend limit neither (R-9: credits lift it, and its
/// act hangs on whether a reset is named). A wall's kind is matched whole.
#[test]
fn each_wall_is_timed_by_its_own_window() {
    use aterm_phase::{ApiCause, WallKind as K};
    assert_eq!(
        UsageWindow::of_wall(
            K::UsageSession,
            "You've hit your session limit · resets 3pm"
        ),
        [FIVE]
    );
    assert_eq!(
        UsageWindow::of_wall(K::UsageSession, "5-hour limit reached"),
        [FIVE]
    );
    assert_eq!(
        UsageWindow::of_wall(K::UsageWeekly, "You've hit your weekly limit"),
        [WEEK]
    );
    assert_eq!(
        UsageWindow::of_wall(K::UsageSession, "Usage limit reached · limit resets 5am"),
        [FIVE, WEEK]
    );
    for kind in [
        K::Spend,
        K::ModelBucket { consent: false },
        K::Context,
        K::Auth,
        K::Overloaded,
        K::Memory,
        K::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        },
    ] {
        assert!(
            UsageWindow::of_wall(kind, "limit").is_empty(),
            "{kind:?} is no usage window's"
        );
    }
    assert_eq!(FIVE.len_s(), 18_000);
    assert_eq!(WEEK.len_s(), 604_800);
    assert_eq!((FIVE.word(), WEEK.word()), ("five_hour", "seven_day"));
}

/// A FULL WINDOW TIMES THE WALL TO ITS RESET, to the second, from a reading
/// written at or after the wall's first read. NEGATIVE CONTROLS, each
/// timing nothing: a window under 100 %, a reset already behind, a reset
/// further off than the window's length, and a reading the tap wrote before
/// the wall was first read.
#[test]
fn a_full_window_read_since_the_wall_times_it_to_its_reset() {
    let reset = NOW + 3 * 3600;
    assert_eq!(
        says(&[FIVE], &[&[(FIVE, full(100, reset, FIRST))]]),
        TapSays::ResetsAt(reset)
    );
    assert_eq!(
        says(&[FIVE], &[&[(FIVE, full(100, NOW + FIVE.len_s(), NOW))]]),
        TapSays::ResetsAt(NOW + FIVE.len_s()),
        "at the window's length"
    );
    for (why, reading) in [
        ("under 100", full(99, reset, NOW)),
        ("a reset behind", full(100, NOW, NOW)),
        ("past the window", full(100, NOW + FIVE.len_s() + 1, NOW)),
        ("written before the wall", full(100, reset, FIRST - 1)),
        ("no reading", None),
        ("the window dropped", dropped(NOW)),
    ] {
        assert_eq!(
            says(&[FIVE], &[&[(FIVE, reading)]]),
            TapSays::Nothing,
            "{why}"
        );
    }
    // The weekly window's length is a week.
    let week = NOW + 3 * 86_400;
    assert_eq!(
        says(&[WEEK], &[&[(WEEK, full(100, week, NOW))]]),
        TapSays::ResetsAt(week)
    );
}

/// A WINDOW IS FULL WHERE THE VENDOR'S OWN FIGURE IS 100.0: the figure is
/// read in tenths (`Window::x10`), so a 99.6, which the tap's row rounds to
/// `100%`, times nothing — never the wait to that window's later reset an
/// unnamed wall would take. NEGATIVE CONTROLS beside it: a 100.0, a whole
/// 100, and a 100.4 the tap reads as no figure.
#[test]
fn a_window_is_full_only_where_the_vendors_figure_is_100() {
    let reset = NOW + 3600;
    let vars = crate::harness::tap::TapVars {
        sid: Some("s-1a".to_string()),
        pid: Some("77".to_string()),
        login: None,
        home: None,
    };
    let five = |pct: &str| {
        let payload = format!(
            r#"{{"rate_limits":{{"five_hour":{{"used_percentage":{pct},"resets_at":{reset}}}}}}}"#
        );
        let r = Reading::of_stdin(&payload, &vars, NOW).expect("a reading");
        Some(TapWindow {
            figure: r.five_hour,
            read_at: NOW,
        })
    };
    let near = five("99.6");
    assert_eq!(near.and_then(|t| t.figure).map(|f| f.pct), Some(100));
    assert_eq!(says(&[FIVE], &[&[(FIVE, near)]]), TapSays::Nothing, "99.6");
    for pct in ["100.0", "100"] {
        assert_eq!(
            says(&[FIVE], &[&[(FIVE, five(pct))]]),
            TapSays::ResetsAt(reset),
            "{pct}"
        );
    }
    assert_eq!(five("100.4").and_then(|t| t.figure), None);
}

/// A REPEAT WRITTEN AFTER THE WALL WAS READ IS THE WALL'S OWN FIGURE: the tap
/// ran 0.4 s after the 429, before the loop's first read, and its next run
/// re-reports what the process holds since that 429. It times the wall,
/// though the recorder's freshness rule (`tap::Fresh`, news for the
/// account's figure) refuses it as a repeat — which is why that rule is not
/// this one's — and though the reading keeps the 429's run as its own `at`
/// (`tap::run`): a reading's time here is when the tap wrote it, its file's
/// write. NEGATIVE CONTROL: the 429's own run, written before the first read,
/// times nothing alone.
#[test]
fn a_repeat_written_after_the_walls_first_read_times_it() {
    let reset = NOW + 3600;
    let payload = format!(
        r#"{{"cost":{{"total_api_duration_ms":900}},"rate_limits":{{"five_hour":{{"used_percentage":100,"resets_at":{reset}}}}}}}"#
    );
    let vars = crate::harness::tap::TapVars {
        sid: Some("s-1a".to_string()),
        pid: Some("77".to_string()),
        login: None,
        home: None,
    };
    let at_429 = Reading::of_stdin(&payload, &vars, FIRST - 1).expect("a reading");
    let repeat = Reading::of_stdin(&payload, &vars, FIRST + 9).expect("a reading");
    let mut fresh = Fresh::default();
    assert!(fresh.admit(&at_429));
    assert!(!fresh.admit(&repeat), "the recorder's rule: no news");
    let window = |r: &Reading, written: i64| {
        Some(TapWindow {
            figure: r.five_hour,
            read_at: written,
        })
    };
    assert_eq!(
        says(&[FIVE], &[&[(FIVE, window(&at_429, FIRST - 1))]]),
        TapSays::Nothing,
        "written before the wall was read"
    );
    assert_eq!(
        says(
            &[FIVE],
            &[
                &[(FIVE, window(&at_429, FIRST - 1))],
                &[(FIVE, window(&repeat, FIRST + 9))]
            ]
        ),
        TapSays::ResetsAt(reset)
    );
}

/// THE WALL'S OWN WINDOW, AND THE LATER OF TWO: a five-hour wall is not timed
/// by a full seven-day window (the loop asks only for the windows its wall
/// is timed by), a weekly one not by a full five-hour one; a wall timed by
/// both takes the later reset of those that read full, and a window under
/// 100 % times nothing there. The newest reading that times wins.
#[test]
fn a_wall_timed_by_two_windows_takes_the_later_full_reset() {
    let (five, week) = (NOW + 3600, NOW + 2 * 86_400);
    let both = [(FIVE, full(100, five, NOW)), (WEEK, full(100, week, NOW))];
    assert_eq!(says(&[FIVE, WEEK], &[&both]), TapSays::ResetsAt(week));
    let five_only = [(FIVE, full(100, five, NOW)), (WEEK, full(60, week, NOW))];
    assert_eq!(says(&[FIVE, WEEK], &[&five_only]), TapSays::ResetsAt(five));
    // The loop folds in only its wall's windows.
    assert_eq!(
        says(&[FIVE], &[&[(FIVE, full(40, five, NOW))]]),
        TapSays::Nothing
    );
    assert_eq!(
        says(&[WEEK], &[&[(WEEK, full(100, week, NOW))]]),
        TapSays::ResetsAt(week)
    );
    // A newer full reading names a later reset: it is the one.
    let later = five + 600;
    assert_eq!(
        says(
            &[FIVE],
            &[
                &[(FIVE, full(100, five, NOW))],
                &[(FIVE, full(100, later, NOW + 10))]
            ]
        ),
        TapSays::ResetsAt(later)
    );
    // A reading that times nothing leaves the timed reset standing: the
    // window dropped at its reset is still that reset.
    assert_eq!(
        says(
            &[FIVE],
            &[
                &[(FIVE, full(100, five, NOW))],
                &[(FIVE, dropped(five + 1))]
            ]
        ),
        TapSays::ResetsAt(five)
    );
}

/// NO RESET AT ALL, AND A ROLLOVER: a window the tap showed full at this wall
/// (its reset behind by the time it was read, so it timed nothing), then
/// dropped or under 100 % in a later reading, has rolled over at that
/// reading. NEGATIVE CONTROLS: a window never read full — the 429 carried no
/// figure for it, or none at all — rolls nothing over when it is dropped or
/// read under 100 %; a full window read again full after a drop is full; and
/// with two windows, one still full holds the rollover.
#[test]
fn a_full_window_shown_rolled_over_ends_a_wait_with_no_reset() {
    let behind = NOW - 5;
    let full_behind = [(FIVE, full(100, behind, NOW))];
    assert_eq!(says(&[FIVE], &[&full_behind]), TapSays::Nothing);
    assert_eq!(
        says(&[FIVE], &[&full_behind, &[(FIVE, dropped(NOW + 2))]]),
        TapSays::RolledAt(NOW + 2)
    );
    assert_eq!(
        says(
            &[FIVE],
            &[&full_behind, &[(FIVE, full(3, NOW + 18_000, NOW + 4))]]
        ),
        TapSays::RolledAt(NOW + 4),
        "under 100 % again"
    );
    // The first reading that showed it rolled is the instant.
    assert_eq!(
        says(
            &[FIVE],
            &[
                &full_behind,
                &[(FIVE, dropped(NOW + 2))],
                &[(FIVE, dropped(NOW + 12))]
            ]
        ),
        TapSays::RolledAt(NOW + 2)
    );
    // NEGATIVE CONTROLS.
    for (why, before) in [
        ("never read full", full(97, NOW + 600, NOW)),
        ("no figure at all", dropped(NOW)),
        ("written before the wall", full(100, behind, FIRST - 1)),
    ] {
        assert_eq!(
            says(&[FIVE], &[&[(FIVE, before)], &[(FIVE, dropped(NOW + 2))]]),
            TapSays::Nothing,
            "{why}"
        );
    }
    assert_eq!(
        says(
            &[FIVE],
            &[
                &full_behind,
                &[(FIVE, dropped(NOW + 2))],
                &[(FIVE, full(100, behind, NOW + 3))]
            ]
        ),
        TapSays::Nothing,
        "full again"
    );
    let week_behind = NOW - 9;
    let both_full = [
        (FIVE, full(100, behind, NOW)),
        (WEEK, full(100, week_behind, NOW)),
    ];
    let five_rolled = [
        (FIVE, dropped(NOW + 2)),
        (WEEK, full(100, week_behind, NOW + 2)),
    ];
    assert_eq!(
        says(&[FIVE, WEEK], &[&both_full, &five_rolled]),
        TapSays::Nothing,
        "the week still full"
    );
    let both_rolled = [(FIVE, dropped(NOW + 2)), (WEEK, dropped(NOW + 8))];
    assert_eq!(
        says(&[FIVE, WEEK], &[&both_full, &five_rolled, &both_rolled]),
        TapSays::RolledAt(NOW + 8)
    );
}

/// THE NOTICE AND THE TAP: the tap's reset within the clock slack of the
/// notice's (inclusive) — the tap's, exact; further apart, the later, and the
/// disagreement named for the ledger; a tap and no notice, the tap's; a
/// rollover only where nothing names a reset; nothing from the tap, the
/// notice's.
#[test]
fn the_tap_and_the_notice_give_the_tap_within_the_slack_and_else_the_later() {
    let n = NOW + 3600;
    let at = |t: Timed| t.at;
    assert_eq!(at(timed(Some(n), TapSays::ResetsAt(n + 45))), Some(n + 45));
    assert_eq!(
        timed(Some(n), TapSays::ResetsAt(n - CLOCK_SLACK_S)).disagree,
        None
    );
    assert_eq!(
        timed(Some(n), TapSays::ResetsAt(n - CLOCK_SLACK_S)).at,
        Some(n - CLOCK_SLACK_S),
        "at the slack: the tap's"
    );
    assert_eq!(
        timed(Some(n), TapSays::ResetsAt(n + CLOCK_SLACK_S + 1)),
        Timed {
            at: Some(n + CLOCK_SLACK_S + 1),
            disagree: Some((n, n + CLOCK_SLACK_S + 1)),
        },
        "the tap later"
    );
    assert_eq!(
        timed(Some(n), TapSays::ResetsAt(n - CLOCK_SLACK_S - 1)),
        Timed {
            at: Some(n),
            disagree: Some((n, n - CLOCK_SLACK_S - 1)),
        },
        "the notice later"
    );
    assert_eq!(at(timed(None, TapSays::ResetsAt(n))), Some(n));
    assert_eq!(at(timed(None, TapSays::RolledAt(NOW))), Some(NOW));
    assert_eq!(
        at(timed(Some(n), TapSays::RolledAt(NOW))),
        Some(n),
        "a notice's reset stands over a rollover"
    );
    assert_eq!(at(timed(Some(n), TapSays::Nothing)), Some(n));
    assert_eq!(at(timed(None, TapSays::Nothing)), None);
}
