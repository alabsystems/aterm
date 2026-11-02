// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `--mode wall-tap` (W10, [`crate::forever::tap`]) over records shaped as the loop
//! writes them: the notice's reset at +300 s, the window's 120 s later.

use aterm_agent::supervise::approvals::RULE_WALL_WAIT;

use super::*;
use crate::forever::tap::{self, Driven, TAP_LATER_S};

/// The notice's reset, ms after [`T0`].
const RESET: i64 = 300_000;

/// The session's share of the jitter the fixture's waits carry, ms.
const SHARE: i64 = 17_000;

fn grace_ms() -> i64 {
    i64::try_from(turn_end::TurnEndTiming::default().reset_grace.as_millis()).unwrap()
}

/// The wait's due timed by the tap: its reset, the grace, the share; ms after [`T0`].
fn tap_due() -> i64 {
    RESET + TAP_LATER_S * 1000 + grace_ms() + SHARE
}

fn wall_wait(tap_unix: Option<i64>) -> String {
    wall_wait_of("1:06 PM", tap_unix)
}

/// The `wall-wait@v1` reason of the turn's notice, its reset's zone named, as the new loop reads
/// it once Esc took the footer away.
fn zoned_wait(tap_unix: Option<i64>) -> String {
    wall_wait_of("1:06 PM (America/Los_Angeles)", tap_unix)
}

fn wall_wait_of(reset: &str, tap_unix: Option<i64>) -> String {
    format!(
        "the turn-end policy (wall: reset_unix={}; reset={reset}; kind=usage-session{}; message=You've hit your session limit · resets {reset})",
        (T0 + RESET) / 1000,
        tap_unix.map_or_else(String::new, |t| format!("; tap_unix={t}"))
    )
}

/// The window's reset, as `tap_unix=` names it.
fn tap_unix() -> i64 {
    (T0 + RESET) / 1000 + TAP_LATER_S
}

fn resets(t: i64, seq: u32, due: i64) -> (i64, String) {
    j(
        t,
        &format!(
            "WAITING seq={seq} until={} usage-session resets",
            stamp(due)
        ),
    )
}

/// W10 as it should go: the wall met at +140 s, Esc at +150 s, the wait to the tap's reset at
/// +152 s, the key changed at +160 s, the loop before gone at +161 s, the new loop reading the
/// zone-named notice and waiting to the same point at +162 s, the resume at its due, landed.
fn tapped() -> Seen {
    let due = tap_due();
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(
                140_000,
                "EVENT limited seq=2 message=You've hit your session limit · resets 1:06 PM reset=1:06 PM",
            ),
            j(
                140_100,
                &format!(
                    "WAITING seq=2 until={} usage-session goes on by itself",
                    stamp(RESET + 600_000)
                ),
            ),
            resets(152_000, 3, due),
            j(161_000, "EXIT stopped"),
            resets(162_000, 3, due),
            j(
                due + 200,
                "CONTINUED seq=3 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(due + 5_000, "EVENT idle seq=4 Nothing left: the parser"),
            j(due + 5_000, "DONE seq=4 rule=land@v1 x"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            row(
                140_000,
                RULE_WALL_WAIT,
                "waiting",
                "",
                &wall_wait(Some(tap_unix())),
            ),
            point(140_000, 2),
            row(
                162_000,
                RULE_WALL_WAIT,
                "waiting",
                "",
                &zoned_wait(Some(tap_unix())),
            ),
            typed(due + 200, "usage-resume@v1", LAND_NUDGE),
            point(due + 5_000, 3),
            task_done(due + 5_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(due + 6_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + due + 5_000 + 545_000,
        stub: Some(StubSeen {
            baseline: 2,
            read: 0,
            new_turns: 0,
        }),
        cover: Cover {
            wall: Some((T0 + 6_000, (T0 + RESET) / 1000)),
            lifted_ms: Some(T0 + RESET - 3_000),
            window_stub: Some(StubSeen {
                baseline: 1,
                read: 3,
                new_turns: 0,
            }),
            tap: Some(Driven {
                cancelled_ms: Some(T0 + 150_000),
                restarted_ms: Some(T0 + 160_000),
                ..Driven::default()
            }),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// [`tapped`] with every act and line from `from` on moved by `by` ms.
fn moved(mut s: Seen, from: i64, by: i64) -> Seen {
    for (t, _) in s.journal.iter_mut().filter(|(t, _)| *t >= T0 + from) {
        *t += by;
    }
    for r in s.ledger.iter_mut().filter(|r| r.ts >= T0 + from) {
        r.ts += by;
    }
    for (t, _) in s.status.iter_mut().filter(|(t, _)| *t >= T0 + from) {
        *t += by;
    }
    s
}

fn w10(s: &Seen) -> (&'static str, String) {
    one(Chain::TapWall, s, "claude", Scenario::W10)
}

#[test]
fn the_tap_times_the_wall_through_a_new_loop_to_the_resume() {
    let (w, why) = w10(&tapped());
    assert_eq!(w, "PASS", "{why}");
    let named = format!(
        "wall-wait@v1 names tap_unix={}",
        (T0 + RESET) / 1000 + TAP_LATER_S
    );
    for said in [
        named.as_str(),
        "the new loop's wait",
        "usage-resume@v1 typed at",
    ] {
        assert!(why.contains(said), "{said}: {why}");
    }
}

/// The negative control: the run `--untapped` makes, no tap on the line, so the row names no
/// tap's reset and every wait goes to the notice's reset with the grace and a share of it.
#[test]
fn a_wait_to_the_notices_reset_fails_w10() {
    let early = RESET + grace_ms() + 23_000;
    let mut s = tapped();
    s.ledger[2].reason = wall_wait(None);
    s.ledger[4].reason = zoned_wait(None);
    s.journal[5] = resets(152_000, 3, early);
    s.journal[7] = resets(162_000, 3, early);
    let s = moved(s, tap_due(), early - tap_due());
    let (w, why) = w10(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("NO wall-wait@v1 row names the tap's reset"),
        "{why}"
    );
    assert!(why.contains("not the tap's reset"), "{why}");
    assert!(why.contains("TYPED at"), "{why}");

    let mut s = tapped();
    s.ledger[2].reason = wall_wait(Some((T0 + RESET) / 1000));
    let (w, why) = w10(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("not the window's"), "{why}");
}

#[test]
fn a_new_loop_that_drops_the_taps_reset_fails_w10() {
    let mut s = tapped();
    s.journal[7] = resets(162_000, 3, RESET + grace_ms() + 23_000);
    let (w, why) = w10(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("the new loop's wait"), "{why}");
    let mut s = tapped();
    s.journal.remove(7);
    let (w, why) = w10(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("journaled no wait"), "{why}");
}

/// The new loop's own `wall-wait@v1` row is judged, not the first loop's: one written from the
/// zone-named notice with no `tap_unix=` FAILs, though the first loop's row names it.
#[test]
fn a_new_loops_row_naming_no_tap_fails_w10() {
    let mut s = tapped();
    s.ledger[4].reason = zoned_wait(None);
    let (w, why) = w10(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("of the new loop's wall-wait@v1 row(s) name NO tap's reset"),
        "{why}"
    );
    let mut s = tapped();
    s.ledger[4].reason = zoned_wait(Some((T0 + RESET) / 1000));
    let (w, why) = w10(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("not the window's"), "{why}");
}

/// W10 judges the new loop across the reset's key change: a new loop that never read the turn's
/// zone-named notice (its rows the footer's words, or none), or one never seen starting (no
/// `EXIT` of the loop before it past the key's change), is UNOBSERVED, not PASS.
#[test]
fn a_new_loop_that_never_read_the_zone_named_notice_is_unobserved() {
    let mut s = tapped();
    s.ledger[4].reason = wall_wait(Some(tap_unix()));
    let (w, why) = w10(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("never read the turn's zone-named notice"),
        "{why}"
    );
    let mut s = tapped();
    s.ledger.remove(4);
    assert_eq!(w10(&s).0, "UNOBSERVED", "no row of its own");
    let mut s = tapped();
    s.journal.remove(6);
    let (w, why) = w10(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("no loop's EXIT"), "{why}");
}

/// Every wait to the reset past the cancel is judged, up to the resume: a wait to the notice's
/// reset that the tap's next reading moved back still FAILs, the new loop's or the loop before
/// its.
#[test]
fn a_wait_to_the_notices_reset_moved_back_before_the_resume_fails_w10() {
    let wrong = RESET + grace_ms() + 23_000;
    let mut s = tapped();
    s.journal.insert(8, resets(170_000, 4, wrong));
    s.journal.insert(9, resets(190_000, 5, tap_due()));
    let (w, why) = w10(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("1 other wait(s)") && why.contains("the new loop's at"),
        "{why}"
    );
    let mut s = tapped();
    s.journal.insert(5, resets(151_000, 3, wrong));
    let (w, why) = w10(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("the loop before its at"), "{why}");
}

/// A run cut short before the tap's due keeps the faults it showed: FAIL, not UNOBSERVED.
/// CONTROL: the same run with no fault is UNOBSERVED.
#[test]
fn a_w10_run_cut_before_the_taps_due_keeps_its_faults() {
    let cut = |s: &mut Seen| {
        s.journal.truncate(8);
        s.ledger.truncate(5);
        s.status.truncate(1);
        s.end_ms = T0 + tap_due() - 30_000;
    };
    let mut s = tapped();
    cut(&mut s);
    let (w, why) = w10(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("before the tap's due"), "{why}");
    let mut s = tapped();
    s.ledger[4].reason = zoned_wait(None);
    cut(&mut s);
    let (w, why) = w10(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("name NO tap's reset"), "{why}");
    assert!(why.contains("before the tap's due"), "{why}");
}

#[test]
fn w10_undriven_is_unobserved() {
    for (gave_up, cancelled, restarted) in [
        (Some("no countdown armed".to_string()), None, None),
        (None, None, None),
        (None, Some(T0 + 150_000), None),
    ] {
        let mut s = tapped();
        s.cover.tap = Some(Driven {
            cancelled_ms: cancelled,
            restarted_ms: restarted,
            gave_up,
            ..Driven::default()
        });
        assert_eq!(w10(&s).0, "UNOBSERVED", "{:?}", s.cover.tap);
    }
    let mut s = tapped();
    s.cover.tap = None;
    assert_eq!(w10(&s).0, "UNOBSERVED");
}

#[test]
fn wall_tap_is_claude_codes_alone_and_its_429_names_the_windows_later_reset() {
    assert_eq!(Mode::of("wall-tap"), Some(Mode::WallTap));
    assert_eq!(Mode::WallTap.walled(), Some(Scenario::W10));
    assert_eq!(Chain::of_mode(Mode::WallTap), [Chain::TapWall]);
    assert!(Chain::TapWall.walled() && Chain::TapWall.lands());
    assert_eq!(Chain::TapWall.scenarios(), [Scenario::W10]);
    assert_eq!(Scenario::W10.id(), "W10");
    assert!(wall_out_of_reach(Harness::Claude, Mode::WallTap).is_none());
    for h in [Harness::Agy, Harness::Muse] {
        assert!(wall_out_of_reach(h, Mode::WallTap).is_some());
        assert!(Chain::of_mode_on(Mode::WallTap, h).is_empty());
    }
    assert!(Chain::every().contains(&Chain::TapWall));
    let blocks = crate::verbs::rate_blocks(Some(&tap::on_429())).unwrap();
    let rate = crate::stub::rate_headers(&blocks, 0);
    let reset = rate
        .on429
        .iter()
        .find(|(h, _)| h == "anthropic-ratelimit-unified-5h-reset")
        .map(|(_, v)| crate::stub::reset_value(v, 1_800_000_000));
    assert_eq!(reset, Some((1_800_000_000 + TAP_LATER_S).to_string()));
    assert!(rate.on200.is_empty());
    let a = |w: &[&str]| parse_args(&w.iter().map(|s| (*s).to_string()).collect::<Vec<_>>());
    assert!(
        a(&["--harness", "claude", "--mode", "wall-tap", "--untapped"])
            .is_ok_and(|a| a.is_some_and(|a| a.untapped))
    );
    assert_eq!(
        a(&["--harness", "claude", "--mode", "wall", "--untapped"])
            .err()
            .map(|f| f.code),
        Some(2)
    );
}

/// W11 as it should go: the wall met at +140 s with the countdown armed, the pane narrowed at
/// +150 s, the wait decided there to the tap's reset with the grace and the share, a
/// `wall-wait@v1` row naming it, the pane widened at +175 s, Claude Code going on by itself past
/// the reset, landed.
fn narrowed() -> Seen {
    let mut s = tapped();
    let due = tap_due();
    s.journal = vec![
        s.journal[0].clone(),
        s.journal[1].clone(),
        s.journal[2].clone(),
        s.journal[3].clone(),
        s.journal[4].clone(),
        j(
            170_000,
            "EVENT limited seq=3 message=⚠ Usage limit reached · limit resets 1:06 PM reset=1:06 PM",
        ),
        resets(170_001, 3, due),
        j(RESET + 80_000, "EVENT idle seq=4 Nothing left: the parser"),
        j(RESET + 80_000, "DONE seq=4 rule=land@v1 x"),
    ];
    s.ledger = vec![
        point(5_000, 1),
        typed(125_000, "land@v1", LAND),
        row(
            140_000,
            RULE_WALL_WAIT,
            "waiting",
            "",
            &wall_wait(Some((T0 + RESET) / 1000 + TAP_LATER_S)),
        ),
        point(140_000, 2),
        row(
            170_001,
            RULE_WALL_WAIT,
            "waiting",
            "",
            &wall_wait(Some((T0 + RESET) / 1000 + TAP_LATER_S)),
        ),
        point(RESET + 80_000, 3),
        task_done(RESET + 80_000),
    ];
    s.status = vec![
        status(1_000, "open", "land"),
        status(RESET + 81_000, "landed", "land"),
    ];
    s.end_ms = T0 + RESET + 80_000 + 545_000;
    s.cover.went_on_ms = Some(T0 + RESET + 75_000);
    s.cover.tap = Some(Driven {
        narrowed_ms: Some(T0 + 150_000),
        widened_ms: Some(T0 + 175_000),
        ..Driven::default()
    });
    s
}

fn w11(s: &Seen) -> (&'static str, String) {
    one(Chain::NarrowTapWall, s, "claude", Scenario::W11)
}

#[test]
fn the_tap_times_the_wall_in_a_narrow_pane() {
    let (w, why) = w11(&narrowed());
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("the tap's"), "{why}");
    let armed = i64::try_from(
        turn_end::TurnEndTiming::default()
            .auto_resume_grace
            .as_millis(),
    )
    .unwrap();
    let mut s = narrowed();
    s.journal[6] = j(
        170_001,
        &format!(
            "WAITING seq=3 until={} usage-session goes on by itself",
            stamp(RESET + TAP_LATER_S * 1000 + armed)
        ),
    );
    assert_eq!(w11(&s).0, "PASS", "the countdown read whole: its grace");
    for at in [150_000, 175_000] {
        let mut s = narrowed();
        s.ledger[4].ts = T0 + at;
        assert_eq!(w11(&s).0, "PASS", "both narrow-interval endpoints count");
    }
}

#[test]
fn a_refused_restore_completes_no_widening() {
    let pending = Driven {
        narrowed_ms: Some(T0 + 150_000),
        ..Driven::default()
    };
    let denied = pending.clone().after_widening(
        &crate::door::Reply {
            code: 1,
            stderr: "resize denied".to_string(),
            ..crate::door::Reply::default()
        },
        T0 + 175_000,
    );
    assert_eq!(denied.widened_ms, None);
    assert_eq!(denied.narrowed_ms, pending.narrowed_ms);
    assert!(
        denied
            .gave_up
            .as_deref()
            .is_some_and(|why| why.contains("resize denied"))
    );
    let mut s = narrowed();
    s.cover.tap = Some(denied);
    assert_eq!(w11(&s).0, "UNOBSERVED");

    let restored = pending.after_widening(&crate::door::Reply::default(), T0 + 175_000);
    assert_eq!(restored.widened_ms, Some(T0 + 175_000));
    assert_eq!(restored.gave_up, None);
    s.cover.tap = Some(restored);
    assert_eq!(w11(&s).0, "PASS");
}

/// The negative control: `--untapped`, the wait decided in the narrow pane to the notice's
/// reset, its row naming no tap's reset.
#[test]
fn a_narrow_wait_to_the_notices_reset_fails_w11() {
    let mut s = narrowed();
    s.journal[6] = resets(170_001, 3, RESET + grace_ms() + 23_000);
    s.ledger[4].reason = wall_wait(None);
    let (w, why) = w11(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("not timed by the tap's reset"), "{why}");
    assert!(
        why.contains("of the narrow pane's wall-wait@v1 row(s) name NO tap's reset"),
        "{why}"
    );
    let mut s = narrowed();
    s.journal.remove(6);
    assert_eq!(w11(&s).0, "UNOBSERVED");
    let mut s = narrowed();
    s.cover.tap = Some(Driven {
        narrowed_ms: Some(T0 + 150_000),
        ..Driven::default()
    });
    assert_eq!(w11(&s).0, "UNOBSERVED");
    let mut s = narrowed();
    s.ledger[4].ts = T0 + 175_001;
    let (w, why) = w11(&s);
    assert_eq!(
        w, "FAIL",
        "a row written after widening is not narrow evidence: {why}"
    );
}

/// Every `wall-wait@v1` row written in the narrow pane is judged: one naming no tap's reset FAILs,
/// whatever a later row in the pane names.
#[test]
fn a_narrow_row_naming_no_tap_is_not_excused_by_a_later_one() {
    let mut s = narrowed();
    s.ledger.insert(
        4,
        row(160_000, RULE_WALL_WAIT, "waiting", "", &wall_wait(None)),
    );
    let (w, why) = w11(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("1 of the narrow pane's"), "{why}");
}

/// The tapped walls are driven once the loop waits on the wall, whatever re-read decided the
/// wait: past the land act's take window the decision comes at a `seq=` no point names
/// (live, 2026-10-30: `EVENT limited seq=1720`, then `WAITING seq=1722 … goes on by itself`).
#[test]
fn the_tapped_walls_drive_at_the_walls_wait_past_the_take_window() {
    let s = tapped();
    let at_act = |n: usize| Seen {
        journal: s.journal[..n].to_vec(),
        ledger: s.ledger.clone(),
        ..Seen::default()
    };
    assert!(
        !tap::waits_on_the_wall(&at_act(4)),
        "the wall's point alone"
    );
    let mut taking = at_act(4);
    taking.journal.push(j(
        140_050,
        &format!(
            "WAITING seq=2 until={} the worker to take the land@v1 act",
            stamp(170_000)
        ),
    ));
    assert!(!tap::waits_on_the_wall(&taking), "the take window");
    taking.journal.push(j(
        170_100,
        &format!(
            "WAITING seq=9 until={} usage-session goes on by itself",
            stamp(RESET + 600_000)
        ),
    ));
    assert!(
        tap::waits_on_the_wall(&taking),
        "the wall's wait at its own seq"
    );
    assert!(
        !super::waits_on_the_wall(&taking),
        "the hide's rule wants the point's seq"
    );
}
