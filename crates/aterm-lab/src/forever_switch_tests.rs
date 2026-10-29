// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the switch's judges (v6 §14.10's 10a) over records shaped as the loop writes them.

use super::*;
use crate::forever::{Cover, Row};

/// The first prompt (unix ms): 2026-10-24T18:40:20Z.
const T0: i64 = 1_792_867_220_000;

/// The first short turn's back-off, journaled at +3.7 s and due at +125 s (`18:42:25Z`).
const WAITING: &str =
    "WAITING seq=882 until=2026-10-24T18:42:25Z 1 short turn(s) in a row: 2 min back-off";

/// When [`WAITING`] was journaled, after [`T0`].
const REST_AT: i64 = 3_700;

/// The back-off's due, after [`T0`].
const DUE_AT: i64 = 125_000;

/// The loop's wake at a switch, as `Session::word_moved` journals it.
const WAKE: &str = "NOTE seq=882 autopilot=land the session's Autopilot word moved: decided after a \
                    1000 ms settle";

/// Land's instruction as the loop journals it.
const LAND: &str = "CONTINUED seq=882 rule=land@v1 [aterm harness] Autopilot is Land: land ALL of \
                    this work";

/// The same-reply hold's journal line (`Exception::repeats`).
const STALL: &str = "EXCEPTION seq=2681 kind=stalled when=later Its program answers aterm's acts \
                     with the same reply and no work, so aterm types nothing more. => Open the \
                     tab and do what its last reply asks.";

/// A journal line at `t` ms after [`T0`].
fn j(t: i64, line: &str) -> (i64, String) {
    (T0 + t, line.to_string())
}

/// A ledger row of `rule` decided `decision` at `t` after [`T0`].
fn row(t: i64, rule: &str, decision: &str) -> Row {
    Row {
        ts: T0 + t,
        rule: rule.into(),
        decision: decision.into(),
        command: String::new(),
        reason: "the turn-end policy (short turns in a row: 1)".into(),
    }
}

/// The lab's write of `word` at `t` after [`T0`], taken.
fn switched(t: i64, word: &'static str) -> Switched {
    Switched {
        at: T0 + t,
        word,
        reply: "OK".into(),
        took: true,
        phase: "idle".into(),
    }
}

/// A Cruise tab at rest in its first back-off, switched to Land at `+13.7 s`, woken a second
/// after, land@v1 typed at `land_at` (its ledger row with it) and landed 4 s later; watched to
/// `+190 s`.
fn at_rest(land_at: i64) -> Seen {
    Seen {
        journal: vec![
            j(
                REST_AT,
                "EVENT idle seq=882 ⏺ Implemented the parser module.",
            ),
            j(REST_AT, WAITING),
            j(14_800, WAKE),
            j(land_at, LAND),
            j(land_at + 4_000, "DONE seq=1791 landed"),
        ],
        ledger: vec![
            row(REST_AT, RULE_POINT, "waited"),
            row(land_at, turn_end::RULE_LAND, "typed"),
        ],
        end_ms: T0 + 190_000,
        cover: Cover {
            words: vec![switched(13_700, "land")],
            ..Cover::default()
        },
        ..Seen::default()
    }
}

#[test]
fn only_the_switch_chains_switch_and_only_the_measured_land_chains_land_at_once() {
    for c in Chain::SWITCH {
        assert_eq!(switches(c), c != Chain::Off, "{c:?}");
        assert!(!at_once(c), "{c:?} is no measured Land chain");
    }
    let land: Vec<Chain> = Chain::ALL
        .into_iter()
        .filter(|c| c.autopilot() == "land")
        .collect();
    assert!(!land.is_empty());
    assert!(land.iter().all(|c| at_once(*c)));
    assert!(
        Chain::ALL
            .into_iter()
            .filter(|c| c.autopilot() != "land")
            .all(|c| !at_once(c))
    );
}

#[test]
fn the_switch_mode_runs_its_five_cruise_chains_and_the_off_control() {
    use Chain::{Off, SwitchAgent, SwitchBusy, SwitchFlip, SwitchHeld, SwitchSeq};
    assert_eq!(
        Chain::of_mode(crate::forever::Mode::Switch),
        [
            SwitchHeld,
            SwitchAgent,
            SwitchSeq,
            SwitchFlip,
            SwitchBusy,
            Off
        ],
        "the held chain first: its row on the Harness page's first screen"
    );
    for c in &Chain::SWITCH[..5] {
        assert_eq!(c.autopilot(), "cruise", "{c:?}");
        assert!(
            Chain::every().contains(c) && !Chain::ALL.contains(c),
            "{c:?}"
        );
    }
    assert!(SwitchHeld.same_reply() && !SwitchAgent.same_reply());
    assert_eq!(
        Chain::SWITCH[..5]
            .iter()
            .map(|c| c.scenarios()[0].id())
            .collect::<Vec<_>>(),
        ["SWH", "SWA", "SWS", "SWF", "SWB"]
    );
}

#[test]
fn the_at_once_want_folds_into_a_measured_land_chains_first_scenario_alone() {
    let cx = |chain| Ctx {
        chain,
        program: "agy",
        t0: T0,
    };
    let own = || Verdict::Pass("its own judge".into());
    let first = Chain::LandA.scenarios()[0];
    let ok = with_at_once(first, &at_rest(5_000), &cx(Chain::LandA), own());
    assert!(matches!(ok, Verdict::Pass(_)), "{ok:?}");
    // Control: main's land@v1, typed at the back-off's due.
    let late = at_rest(124_300);
    let Verdict::Fail(why) = with_at_once(first, &late, &cx(Chain::LandA), own()) else {
        panic!("main's late land@v1 passed the fold");
    };
    assert!(why.starts_with("its own judge; land@v1 at once"), "{why}");
    // Every other scenario and chain keeps its verdict as it was.
    let later = Chain::LandA.scenarios()[1];
    assert_eq!(with_at_once(later, &late, &cx(Chain::LandA), own()), own());
    let s = Chain::SwitchAgent.scenarios()[0];
    assert_eq!(
        with_at_once(s, &late, &cx(Chain::SwitchAgent), own()),
        own()
    );
}

#[test]
fn land_at_once_wants_the_instruction_within_ten_seconds_of_the_first_point() {
    let seen = at_rest(5_000);
    assert!(
        matches!(land_at_once(&seen, T0), Verdict::Pass(_)),
        "{:?}",
        land_at_once(&seen, T0)
    );
    // Control: main typed it at +124.3 s, the back-off waited out first.
    let seen = at_rest(124_300);
    let Verdict::Fail(why) = land_at_once(&seen, T0) else {
        panic!("{:?}", land_at_once(&seen, T0));
    };
    assert!(
        why.contains("waiting out") && why.contains("2 min back-off"),
        "{why}"
    );
    let unpointed = Seen {
        ledger: Vec::new(),
        ..at_rest(5_000)
    };
    assert!(matches!(
        land_at_once(&unpointed, T0),
        Verdict::Unobserved(_)
    ));
    let unlanded = Seen {
        journal: vec![j(REST_AT, WAITING)],
        ..at_rest(5_000)
    };
    assert!(matches!(land_at_once(&unlanded, T0), Verdict::Fail(_)));
}

#[test]
fn a_switch_at_rest_lands_within_five_seconds_and_before_the_back_offs_due() {
    let seen = at_rest(15_200);
    assert_eq!(
        rest_from(&seen, T0).map(|(t, _)| *t - T0),
        Some(REST_AT),
        "the back-off at rest"
    );
    assert_eq!(
        wake_after(&seen, T0 + 13_700).map(|(t, _)| *t - T0),
        Some(14_800)
    );
    let v = judge_at_rest(&seen, T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    // Controls: the back-off waited out; no wake; land@v1 twice; Cruise's nudge before the switch.
    let Verdict::Fail(why) = judge_at_rest(&at_rest(DUE_AT + 500), T0) else {
        panic!("a land@v1 past the due passed");
    };
    assert!(why.contains("waited out the back-off's due"), "{why}");
    let mut deaf = at_rest(15_200);
    deaf.journal.retain(|(_, l)| !is_wake(l));
    let Verdict::Fail(why) = judge_at_rest(&deaf, T0) else {
        panic!("a switch nothing woke passed");
    };
    assert!(why.contains("no wake journaled"), "{why}");
    let mut twice = at_rest(15_200);
    twice.journal.insert(4, j(16_000, LAND));
    let Verdict::Fail(why) = judge_at_rest(&twice, T0) else {
        panic!("land@v1 typed twice passed");
    };
    assert!(why.contains("land@v1 typed again"), "{why}");
    let mut early = at_rest(15_200);
    early.journal.insert(
        2,
        j(
            9_000,
            "CONTINUED seq=882 rule=nudge@v1 [aterm harness] Keep going",
        ),
    );
    let Verdict::Fail(why) = judge_at_rest(&early, T0) else {
        panic!("a nudge before the switch passed");
    };
    assert!(why.contains("typed before the switch"), "{why}");
    let unswitched = Seen {
        cover: Cover::default(),
        ..at_rest(15_200)
    };
    assert!(matches!(
        judge_at_rest(&unswitched, T0),
        Verdict::Unobserved(_)
    ));
}

/// [`at_rest`] under `switch-seq`'s draft, typed at +8.7 s and cleared at +23.7 s.
fn drafted(land_at: i64) -> Seen {
    let mut seen = at_rest(land_at);
    seen.cover.draft = Some(Draft {
        typed_ms: T0 + 8_700,
        stood: true,
        cleared_ms: Some(T0 + 23_700),
        emptied: true,
    });
    seen
}

#[test]
fn a_switch_under_a_draft_types_nothing_until_the_draft_clears() {
    let v = judge_draft(&drafted(25_000), T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    // Control: land@v1 typed over the standing draft.
    let Verdict::Fail(why) = judge_draft(&drafted(15_200), T0) else {
        panic!("land@v1 over the draft passed");
    };
    assert!(why.contains("typed while the draft stood"), "{why}");
    let mut unstood = drafted(25_000);
    if let Some(d) = unstood.cover.draft.as_mut() {
        d.stood = false;
    }
    assert!(matches!(judge_draft(&unstood, T0), Verdict::Unobserved(_)));
    let mut uncleared = drafted(25_000);
    if let Some(d) = uncleared.cover.draft.as_mut() {
        d.emptied = false;
    }
    assert!(matches!(
        judge_draft(&uncleared, T0),
        Verdict::Unobserved(_)
    ));
}

/// [`drafted`] with the hands-off hold the loop journals at its wake for the person who typed the
/// draft, `until` its grace from the draft's sighting runs out (+134 s, past the back-off's due,
/// as the real lab's run held; +100 s, before it).
fn person_held(until: &str, land_at: i64) -> Seen {
    let mut seen = drafted(land_at);
    let hold = format!("WAITING seq=912 until={until} {PERSON_HOLD}");
    let at = seen
        .journal
        .iter()
        .position(|(_, l)| l == WAKE)
        .expect("the wake")
        + 1;
    seen.journal.insert(at, j(15_800, &hold));
    seen
}

/// The real lab's SWS (2026-10-26): the loop held its hands off for the person who typed the draft
/// until their grace ran out, past the back-off's due, and typed land@v1 0.7 s after; the judge
/// read that as the back-off waited out.
#[test]
fn a_switch_under_a_draft_lands_at_the_end_of_the_hold_for_the_person_who_typed_it() {
    const PAST_DUE: &str = "2026-10-24T18:42:34Z";
    const BEFORE_DUE: &str = "2026-10-24T18:42:00Z";
    let v = judge_draft(&person_held(PAST_DUE, 134_700), T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let Verdict::Pass(said) = v else {
        unreachable!()
    };
    assert!(said.contains("the hold for the person to +134"), "{said}");
    let v = judge_draft(&person_held(BEFORE_DUE, 101_000), T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    // Controls: typed inside the hold; too long after it; past the due where the hold ended first.
    let Verdict::Fail(why) = judge_draft(&person_held(PAST_DUE, 40_000), T0) else {
        panic!("land@v1 inside the hold passed");
    };
    assert!(
        why.contains("typed inside the hold for the person"),
        "{why}"
    );
    let Verdict::Fail(why) = judge_draft(&person_held(PAST_DUE, 145_000), T0) else {
        panic!("land@v1 11 s past the hold passed");
    };
    assert!(why.contains("past 5 s"), "{why}");
    let Verdict::Fail(why) = judge_draft(&person_held(BEFORE_DUE, DUE_AT + 500), T0) else {
        panic!("land@v1 past the due, the hold ended before it, passed");
    };
    assert!(why.contains("waited out the back-off's due"), "{why}");
    // Without the hold journaled the clearing is what land@v1 is timed from, as before.
    let Verdict::Fail(why) = judge_draft(&drafted(134_700), T0) else {
        panic!("land@v1 110 s past the clearing, no hold journaled, passed");
    };
    assert!(why.contains("past 5 s"), "{why}");
}

/// A Cruise tab at rest flipped to Land then Off 100 ms apart at +13.7 s, woken, nothing typed,
/// its last `status` read `autopilot=off`, watched to `end`.
fn flipped(end: i64) -> Seen {
    Seen {
        journal: vec![
            j(
                REST_AT,
                "EVENT idle seq=882 ⏺ Implemented the parser module.",
            ),
            j(REST_AT, WAITING),
            j(14_800, WAKE),
        ],
        status: vec![(
            T0 + 20_000,
            "task=- needs=- autopilot=off supervisor=attached".into(),
        )],
        end_ms: T0 + end,
        cover: Cover {
            words: vec![switched(13_700, "land"), switched(13_800, "off")],
            ..Cover::default()
        },
        ..Seen::default()
    }
}

#[test]
fn a_flip_to_land_and_back_to_off_types_nothing_past_the_back_offs_due() {
    let v = judge_flip(&flipped(DUE_AT + 40_000), T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    // Not watched past the due: no verdict either way.
    assert!(matches!(
        judge_flip(&flipped(DUE_AT), T0),
        Verdict::Unobserved(_)
    ));
    // Controls: land@v1 typed between the writes; the word left on Land.
    let mut typed = flipped(DUE_AT + 40_000);
    typed.journal.push(j(14_900, LAND));
    let Verdict::Fail(why) = judge_flip(&typed, T0) else {
        panic!("land@v1 under a flip passed");
    };
    assert!(why.contains("typed: +"), "{why}");
    let mut stuck = flipped(DUE_AT + 40_000);
    stuck.status[0].1 = "task=- needs=- autopilot=land supervisor=attached".into();
    let Verdict::Fail(why) = judge_flip(&stuck, T0) else {
        panic!("a flip left on Land passed");
    };
    assert!(why.contains("autopilot=land, not off"), "{why}");
    let mut slow = flipped(DUE_AT + 40_000);
    slow.cover.words[1].at = T0 + 13_700 + FLIP_WITHIN_MS + 1;
    assert!(matches!(judge_flip(&slow, T0), Verdict::Unobserved(_)));
    // A Land write that did not take: Off's wake, word and quiet are all there, but no flip raced.
    let mut refused = flipped(DUE_AT + 40_000);
    refused.cover.words[0].took = false;
    refused.cover.words[0].reply = "exit 1: ERR the control server is busy".into();
    let Verdict::Fail(why) = judge_flip(&refused, T0) else {
        panic!("a flip whose Land write did not take passed");
    };
    assert!(why.contains("the write of land at +13.7"), "{why}");
    assert!(why.contains("did not take: exit 1"), "{why}");
}

#[test]
fn a_write_meta_set_refused_or_failed_is_not_taken() {
    let reply = |code: i32, stdout: &str, stderr: &str| Reply {
        code,
        stdout: stdout.into(),
        stderr: stderr.into(),
    };
    assert!(taken(&reply(0, "OK autopilot land\n", "")));
    // Controls: an `ERR` on stderr (exit 1), an `ERR` exiting 0, no answer at all.
    assert!(!taken(&reply(1, "", "ERR the control server is busy\n")));
    assert!(!taken(&reply(0, "ERR usage: meta set\n", "")));
    assert!(!taken(&reply(0, "", "")));
}

/// When `switch-held`'s hold was journaled, after [`T0`].
const HELD_AT: i64 = 372_000;

/// A tab held at the same reply under Cruise, switched to Land [`HELD_SWITCH_MS`] past its hold,
/// woken, every `status` read from then on `autopilot=land held=<arm>`, watched [`WATCH_MS`] past.
fn held() -> Seen {
    let sw = HELD_AT + HELD_SWITCH_MS;
    let arm = arm_of(STALL).word();
    Seen {
        journal: vec![
            j(
                HELD_AT,
                "EVENT idle seq=2681 ⏺ Nothing left: the parser is committed and pushed.",
            ),
            j(HELD_AT, STALL),
            j(sw + 1_100, WAKE),
        ],
        status: vec![
            (
                T0 + HELD_AT + 1_000,
                format!("task=- autopilot=cruise held={arm}"),
            ),
            (T0 + sw + 1_200, format!("task=- autopilot=land held={arm}")),
        ],
        status_reads: (0..12).map(|i| T0 + sw + 5_000 * (i + 1)).collect(),
        end_ms: T0 + sw + WATCH_MS,
        cover: Cover {
            words: vec![switched(sw, "land")],
            ..Cover::default()
        },
        ..Seen::default()
    }
}

#[test]
fn a_held_tab_switched_to_land_stays_held_and_types_nothing() {
    let v = judge_held(&held(), T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    // Controls: the hold lifted at the switch; Land's instruction typed; the arm dropped.
    let mut lifted = held();
    lifted.journal.push(j(
        HELD_AT + HELD_SWITCH_MS + 1_500,
        "CLEARED seq=2681 exception=stalled the Autopilot word moved",
    ));
    let Verdict::Fail(why) = judge_held(&lifted, T0) else {
        panic!("a lifted hold passed");
    };
    assert!(why.contains("the hold lifted"), "{why}");
    let mut typed = held();
    typed
        .journal
        .push(j(HELD_AT + HELD_SWITCH_MS + 1_500, LAND));
    let Verdict::Fail(why) = judge_held(&typed, T0) else {
        panic!("land@v1 over a hold passed");
    };
    assert!(why.contains("typed after the hold"), "{why}");
    let mut unheld = held();
    unheld.status[1].1 = "task=- autopilot=land held=-".into();
    let Verdict::Fail(why) = judge_held(&unheld, T0) else {
        panic!("a status with no hold passed");
    };
    assert!(why.contains("held=-"), "{why}");
    let short = Seen {
        end_ms: T0 + HELD_AT + HELD_SWITCH_MS + WATCH_MS - 1,
        ..held()
    };
    assert!(matches!(judge_held(&short, T0), Verdict::Unobserved(_)));
}

#[test]
fn a_held_tabs_state_line_still_leads_with_its_stop_after_the_switch() {
    let lead = stop_words(arm_of(STALL), StopForm::Line);
    let sw = T0 + HELD_AT + HELD_SWITCH_MS;
    let cx = Ctx {
        chain: Chain::SwitchHeld,
        program: "claude",
        t0: T0,
    };
    let mut seen = held();
    seen.window.page = vec![(sw + 4_000, format!("row=1\nstate={lead} · the same reply"))];
    seen.window.page_reads = vec![sw + 4_000, sw + 9_000];
    let v = held_line(&seen, &cx);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    // Control: the line the Land word would draw over the hold, no stop leading it.
    seen.window
        .page
        .push((sw + 9_000, "row=1\nstate=landing".into()));
    let Verdict::Fail(why) = held_line(&seen, &cx) else {
        panic!("a state line with no stop passed");
    };
    assert!(why.contains("leads with no stop"), "{why}");
    seen.window.page.clear();
    seen.window.page_reads.clear();
    assert!(matches!(held_line(&seen, &cx), Verdict::Unobserved(_)));
}

/// A long busy turn switched to Land at +5 s, its point at +40 s, land@v1 typed at `land_at`, the
/// wake journaled at `wake_at`.
fn busy(land_at: i64, wake_at: i64) -> Seen {
    Seen {
        journal: vec![
            j(wake_at, WAKE),
            j(
                40_000,
                "EVENT idle seq=1204 ⏺ Wrote the long module in this turn.",
            ),
            j(land_at, LAND),
            j(land_at + 4_000, "DONE seq=1791 landed"),
        ],
        ledger: vec![row(land_at, turn_end::RULE_LAND, "typed")],
        end_ms: T0 + 120_000,
        cover: Cover {
            words: vec![Switched {
                phase: "busy".into(),
                ..switched(BUSY_SWITCH_MS, "land")
            }],
            ..Cover::default()
        },
        ..Seen::default()
    }
}

#[test]
fn a_busy_turn_switched_to_land_types_nothing_until_its_point() {
    let v = judge_busy(&busy(41_000, 40_100), T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    // Controls: the instruction typed into the busy turn; one that waited past its point.
    let mut mid = busy(41_000, 40_100);
    mid.journal.insert(0, j(6_000, LAND));
    let Verdict::Fail(why) = judge_busy(&mid, T0) else {
        panic!("land@v1 typed mid-turn passed");
    };
    assert!(why.contains("typed mid-turn"), "{why}");
    let Verdict::Fail(why) = judge_busy(&busy(40_000 + BUSY_LAND_MS + 1, 40_100), T0) else {
        panic!("a late land@v1 passed");
    };
    assert!(why.contains("past 3 s"), "{why}");
    let Verdict::Fail(why) = judge_busy(&busy(41_000, 42_000), T0) else {
        panic!("a wake after the landing passed");
    };
    assert!(why.contains("came after land@v1"), "{why}");
    let mut pointed = busy(41_000, 40_100);
    pointed.journal[1].0 = T0 + BUSY_SWITCH_MS - 1;
    assert!(matches!(judge_busy(&pointed, T0), Verdict::Unobserved(_)));
}

#[test]
fn a_land_typed_after_a_switch_in_its_back_off_is_not_early() {
    let landed = at_rest(15_200);
    assert_eq!(super::super::early_acts(&landed, T0), Vec::<String>::new());
    // Control: the same act with no switch before it is early by its back-off.
    let unswitched = Seen {
        cover: Cover::default(),
        ..at_rest(15_200)
    };
    let early = super::super::early_acts(&unswitched, T0);
    assert_eq!(early.len(), 1, "{early:?}");
    assert!(early[0].contains("BEFORE the due"), "{early:?}");
}

#[test]
fn each_switch_chains_watch_ends_where_its_verdict_can_be_read() {
    let sw = T0 + 13_700;
    let mut seen = at_rest(15_200);
    let done = T0 + 19_200;
    assert!(!watched(Chain::SwitchAgent, &seen, done + WATCH_MS / 3 - 1));
    assert!(watched(Chain::SwitchAgent, &seen, done + WATCH_MS / 3));
    seen.journal.retain(|(_, l)| !l.starts_with("DONE "));
    assert!(!watched(Chain::SwitchAgent, &seen, sw + LAND_WATCH_MS - 1));
    assert!(watched(Chain::SwitchAgent, &seen, sw + LAND_WATCH_MS));
    let flip = flipped(0);
    let off = T0 + 13_800;
    let due = T0 + DUE_AT;
    assert!(!watched(Chain::SwitchFlip, &flip, off + WATCH_MS));
    assert!(watched(Chain::SwitchFlip, &flip, due + PAST_DUE_MS));
    let hold = held();
    let hsw = T0 + HELD_AT + HELD_SWITCH_MS;
    assert!(!watched(Chain::SwitchHeld, &hold, hsw + WATCH_MS - 1));
    assert!(watched(Chain::SwitchHeld, &hold, hsw + WATCH_MS));
    let mut draft = drafted(25_000);
    draft.journal.retain(|(_, l)| !l.starts_with("DONE "));
    assert!(!watched(
        Chain::SwitchSeq,
        &draft,
        T0 + 23_700 + LAND_WATCH_MS - 1
    ));
    assert!(watched(
        Chain::SwitchSeq,
        &draft,
        T0 + 23_700 + LAND_WATCH_MS
    ));
    assert!(!watched(Chain::Off, &seen, i64::MAX));
}

/// A tape's text as [`text::render`] writes a wait at rest that carried `autopilot=cruise` and
/// woke on Land, a bare wait before it, and a wait that carried the word and timed out.
fn tape_text() -> String {
    [
        "atape-text 1",
        "exchange ctl stamp=10,1792867233000",
        "> @s7",
        "> await",
        "> agent",
        "> timeout=120",
        "< ok 0",
        "!",
        "| OK idle",
        "|",
        "exchange ctl stamp=20,1792867234800",
        "> @s7",
        "> await",
        "> seq",
        "> 882",
        "> autopilot=cruise",
        "< ok 0",
        "!",
        "| OK autopilot land",
        "|",
        "exchange ctl stamp=30,1792867300000",
        "> @s7",
        "> await",
        "> gone",
        "> autopilot=land",
        "< ok 124",
        "! timeout",
        "|",
        "",
    ]
    .join("\n")
}

#[test]
fn the_tapes_witness_is_each_wait_that_carried_the_word_and_woke_on_another() {
    let text = tape_text();
    let (header, items) = text::parse(&text).expect("the renderer's text");
    assert_eq!(
        text::render(header.as_deref(), &items),
        text,
        "a text render writes"
    );
    let woke = woke_of(&text);
    assert_eq!(
        woke,
        vec![Woke {
            at: 1_792_867_234_800,
            form: "seq".into(),
            carried: "autopilot=cruise".into(),
            answered: "OK autopilot land".into(),
        }]
    );
    let line = woke_line(Some(&woke), T0);
    assert!(
        line.contains("await seq … autopilot=cruise answered OK autopilot land"),
        "{line}"
    );
    // Controls: no tape, and tapes whose waits never woke on a word.
    assert_eq!(woke_line(None, T0), "no tape of the session read");
    assert!(woke_line(Some(&[]), T0).starts_with("no wait at rest"));
    assert!(woke_of(&text.replace("OK autopilot land", "OK idle")).is_empty());
    assert!(woke_in(Path::new("/nonexistent/aterm-lab-switch-tapes")).is_none());
}
