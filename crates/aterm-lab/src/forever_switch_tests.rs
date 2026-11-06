// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the switch's judges (v6 §14.10's 10a) over records shaped as the loop writes them.

use super::*;
use crate::forever::{Cover, Row};
use aterm_agent::supervise::tape::show;

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
        box_seq: None,
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
        person: false,
    }
}

/// The person's press of `word` at `t` after [`T0`], taken.
fn pressed(t: i64, word: &'static str) -> Switched {
    Switched {
        person: true,
        ..switched(t, word)
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
fn the_switch_mode_runs_its_six_cruise_chains_and_the_off_control() {
    use Chain::{
        CommandDraft, Off, SwitchAgent, SwitchBusy, SwitchFlip, SwitchOverHeld, SwitchSeq,
    };
    assert_eq!(
        Chain::of_mode(crate::forever::Mode::Switch),
        [
            SwitchOverHeld,
            SwitchAgent,
            SwitchSeq,
            SwitchFlip,
            SwitchBusy,
            CommandDraft,
            Off
        ],
        "the held chain first: its row on the Harness page's first screen"
    );
    for c in &Chain::SWITCH[..6] {
        assert_eq!(c.autopilot(), "cruise", "{c:?}");
        assert!(
            Chain::every().contains(c) && !Chain::ALL.contains(c),
            "{c:?}"
        );
    }
    assert!(!SwitchOverHeld.same_reply() && !SwitchAgent.same_reply());
    assert_eq!(
        Chain::SWITCH[..6]
            .iter()
            .map(|c| c.scenarios()[0].id())
            .collect::<Vec<_>>(),
        ["SWO", "SWA", "SWS", "SWF", "SWB", "SWC"]
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
    // No tape read: the note's want is named skipped and leaves the word as it was.
    assert!(
        ok.why().contains(&format!("{HELD_NOTED}: SKIPPED")),
        "{ok:?}"
    );
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

/// rv1-l2's land-n (2026-11-01), as its tape and records say it: the point at +24.0 s, aterm's
/// decision there and its write 70 ms on, Claude Code drawing nothing of the text until the read
/// at +48.9 s (a `status` read at +34.1 s finding its 565 bytes unread; 24.8 s of hold aterm
/// watched), the Enter at +49.5 s, and the journal's `CONTINUED` with it. `tape` false: no tape
/// read.
fn held_seen(tape: bool) -> Seen {
    let typed = Typed {
        rule: Some(turn_end::RULE_LAND.to_string()),
        seq: 868,
        decided_ms: Some(T0 + 24_000),
        sent_ms: T0 + 24_070,
        drawn_ms: Some(T0 + 48_910),
        held_ms: Some(24_780),
        unshown_ms: None,
        unread: Some(show::Unread {
            at_ms: T0 + 34_080,
            word: "pending".to_string(),
            bytes: Some(565),
            wait_ms: Some(10_004),
        }),
        reaction: Some(false),
        enter_ms: Some(T0 + 49_460),
    };
    Seen {
        journal: vec![
            j(
                24_000,
                "EVENT idle seq=868 ⏺ Implemented the parser module.",
            ),
            j(49_470, LAND),
        ],
        ledger: vec![
            row(24_000, RULE_POINT, "waiting"),
            row(49_470, turn_end::RULE_LAND, "typed"),
        ],
        end_ms: T0 + 960_000,
        cover: Cover {
            typed: tape.then(|| vec![typed]),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// [`held_seen`] with its write shaped by `shape`.
fn held_shaped(shape: impl FnOnce(&mut Typed)) -> Seen {
    let mut seen = held_seen(true);
    if let Some(a) = seen.cover.typed.as_mut().and_then(|t| t.first_mut()) {
        shape(a);
    }
    seen
}

/// THE WANT TIMES ATERM'S SHARE, AND THE PROGRAM'S HOLD IS ITS OWN NUMBER: land-n's write 70 ms
/// after its point and its Enter 0.6 s after the read that showed the text pass, aterm's share
/// 0.7 s, the 24.8 s Claude Code held the text said beside it with the unread bytes. Control: the
/// same records with no tape read fall back to the journal's line, 25.5 s past the point, and
/// fail: the old clock's verdict on the program's stall.
#[test]
fn land_at_once_times_aterms_share_and_says_the_programs_hold_apart() {
    let v = land_at_once(&held_seen(true), T0);
    let Verdict::Pass(said) = &v else {
        panic!("{v:?}");
    };
    for number in ["+24.1 s", "0.7 s", "24.8 s", "input_bytes=565", "+49.5 s"] {
        assert!(said.contains(number), "{number}: {said}");
    }
    let v = land_at_once(&held_seen(false), T0);
    let Verdict::Fail(why) = &v else {
        panic!("the journal's clock passed: {v:?}");
    };
    assert!(why.contains("+49.5 s"), "{why}");
    let line = typed_line(held_seen(true).cover.typed.as_deref(), T0);
    for number in ["+24.1 s land@v1", "+24.0 s", "+48.9 s", "24.8 s", "+49.5 s"] {
        assert!(line.contains(number), "{number}: {line}");
    }
}

/// ATERM'S OWN TIME AFTER THE DRAW IS ATERM'S (the at1 review, 2026-11-01): a text the program drew
/// at once and aterm sent 30 s later fails, and so does one it drew and aterm never sent; the
/// write alone would have passed both (negative control: a judge of the write alone). A text the
/// program held unshown 75 s and aterm sent 0.7 s after it showed passes.
#[test]
fn aterms_own_wait_after_the_draw_fails_land_at_once() {
    let late_enter = held_shaped(|a| {
        a.drawn_ms = Some(a.sent_ms + 200);
        a.held_ms = Some(5);
        a.unread = None;
        a.enter_ms = Some(a.sent_ms + 30_000);
    });
    let v = land_at_once(&late_enter, T0);
    assert!(
        matches!(&v, Verdict::Fail(why) if why.contains("30.1 s")),
        "{v:?}"
    );
    let never_sent = held_shaped(|a| {
        a.drawn_ms = Some(a.sent_ms + 200);
        a.held_ms = Some(5);
        a.enter_ms = None;
    });
    let v = land_at_once(&never_sent, T0);
    assert!(
        matches!(&v, Verdict::Fail(why) if why.contains("+24.3 s")),
        "{v:?}"
    );
    let late_draw = held_shaped(|a| {
        a.drawn_ms = Some(a.sent_ms + 75_000);
        a.held_ms = Some(74_900);
        a.enter_ms = Some(a.sent_ms + 75_700);
    });
    let v = land_at_once(&late_draw, T0);
    assert!(
        matches!(&v, Verdict::Pass(said) if said.contains("74.9 s")),
        "{v:?}"
    );
}

/// A PROGRAM THAT NEVER DRAWS THE TEXT BY THE WATCH'S END HAS ATERM'S WRITE JUDGED (rv1-ab3's
/// land-n: 565 bytes unread 60.5 s, never submitted on that build): the write at the point passes,
/// the program said to have drawn none of it by the end.
#[test]
fn a_text_never_drawn_still_judges_aterms_write() {
    let mut seen = held_shaped(|a| {
        a.drawn_ms = None;
        a.held_ms = None;
        a.unshown_ms = Some(T0 + 900_000);
        a.enter_ms = None;
        a.unread = Some(show::Unread {
            at_ms: T0 + 84_580,
            word: "pending".to_string(),
            bytes: Some(565),
            wait_ms: Some(60_510),
        });
    });
    seen.journal.retain(|(_, l)| !l.starts_with("CONTINUED "));
    seen.ledger.retain(|r| r.rule == RULE_POINT);
    let v = land_at_once(&seen, T0);
    let Verdict::Pass(said) = &v else {
        panic!("{v:?}");
    };
    for number in ["+24.1 s", "+900.0 s", "input_wait_ms=60510"] {
        assert!(said.contains(number), "{number}: {said}");
    }
}

/// THE OLD CLOCK'S FAILING CASE STILL FAILS ON THE NEW ONE: a doctored record whose decision itself
/// came late (main's +124.3 s, the first short turn's back-off waited out, then written at once
/// and drawn at once) fails, the back-off named; so does a decision at the point whose write came
/// 30 s on. A tape that shows other writes but no land@v1 falls back to the journal's line.
#[test]
fn a_late_decision_fails_land_at_once_by_aterms_own_act() {
    let late = |decided: i64, sent: i64| {
        let mut seen = at_rest(sent + 400);
        seen.cover.typed = Some(vec![Typed {
            rule: Some(turn_end::RULE_LAND.to_string()),
            seq: 1791,
            decided_ms: Some(T0 + decided),
            sent_ms: T0 + sent,
            drawn_ms: Some(T0 + sent + 5),
            held_ms: Some(5),
            unshown_ms: None,
            unread: None,
            reaction: Some(true),
            enter_ms: Some(T0 + sent + 400),
        }]);
        seen
    };
    let v = land_at_once(&late(124_300, 124_400), T0);
    let Verdict::Fail(why) = &v else {
        panic!("a late decision passed: {v:?}");
    };
    assert!(
        why.contains("+124.4 s") && why.contains("+124.3 s") && why.contains("2 min back-off"),
        "{why}"
    );
    assert!(matches!(
        land_at_once(&late(REST_AT, 33_700), T0),
        Verdict::Fail(_)
    ));
    assert!(matches!(
        land_at_once(&late(REST_AT, REST_AT + 100), T0),
        Verdict::Pass(_)
    ));
    let mut other = late(REST_AT, REST_AT + 100);
    if let Some(a) = other.cover.typed.as_mut().and_then(|t| t.first_mut()) {
        a.rule = Some(turn_end::RULE_CONTINUE.to_string());
    }
    let v = land_at_once(&other, T0);
    assert!(
        matches!(&v, Verdict::Pass(s) if s.contains("no write of it on the tapes")),
        "{v:?}"
    );
}

/// LAND'S INSTRUCTION IS NEVER TYPED TWICE IN A ROW (the at1 review's real run, 2026-11-01): a
/// land@v1 sent late and then typed again at the agent's defense fails; the land check between
/// them passes, and so does one land@v1. Negative control: the same records with the check taken
/// out fail.
#[test]
fn land_once_fails_the_instruction_typed_again_with_no_act_between() {
    const CHECK: &str = "CONTINUED seq=1781 rule=land-check@v1 [aterm harness] Land check";
    let twice = Seen {
        journal: vec![j(212_300, LAND), j(216_400, LAND)],
        ..Seen::default()
    };
    let v = land_once(&twice, T0);
    assert!(
        matches!(&v, Verdict::Fail(why) if why.contains("+212.3 s") && why.contains("+216.4 s")),
        "{v:?}"
    );
    let checked = Seen {
        journal: vec![j(212_300, LAND), j(216_400, CHECK), j(300_000, LAND)],
        ..Seen::default()
    };
    assert!(matches!(land_once(&checked, T0), Verdict::Pass(_)));
    let once = Seen {
        journal: vec![j(212_300, LAND), j(216_400, CHECK)],
        ..Seen::default()
    };
    assert!(matches!(land_once(&once, T0), Verdict::Pass(_)));
}

/// THE HELD TEXT IS NOTED ONCE PER WRITE (slice at1's item 3): where the first look past land@v1's
/// write ran out, one `NOTE … rule=land@v1` naming the program passes, and none or two fail; where
/// it saw the screen move, a note fails and none passes; with no tape, it is unobserved.
#[test]
fn held_noted_wants_one_note_where_the_first_look_ran_out() {
    let note = |t: i64| {
        j(
            t,
            &format!(
                "NOTE seq=868 rule=land@v1 {} 10 s after the write: input=pending \
                 input_bytes=565 input_wait_ms=10004",
                aterm_agent::supervise::journal::TEXT_HELD
            ),
        )
    };
    let with = |reaction: bool, notes: &[i64]| {
        let mut seen = held_shaped(|a| a.reaction = Some(reaction));
        seen.journal.extend(notes.iter().map(|&t| note(t)));
        seen.journal.sort_by_key(|(t, _)| *t);
        seen
    };
    assert!(matches!(
        held_noted(&with(false, &[34_080]), T0),
        Verdict::Pass(_)
    ));
    assert!(matches!(
        held_noted(&with(false, &[]), T0),
        Verdict::Fail(_)
    ));
    assert!(matches!(
        held_noted(&with(false, &[34_080, 44_080]), T0),
        Verdict::Fail(_)
    ));
    assert!(matches!(
        held_noted(&with(true, &[34_080]), T0),
        Verdict::Fail(_)
    ));
    assert!(matches!(held_noted(&with(true, &[]), T0), Verdict::Pass(_)));
    assert!(matches!(
        held_noted(&held_seen(false), T0),
        Verdict::Unobserved(_)
    ));
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
        text: String::new(),
        typed_ms: T0 + 8_700,
        stood: true,
        cleared_ms: Some(T0 + 23_700),
        emptied: true,
    });
    seen
}

/// [`drafted`] with the hands-off holds the loop journals for the person at `holds` (when, and
/// `until` as the journal stamps it).
fn person_held(holds: &[(i64, &str)], land_at: i64) -> Seen {
    let mut seen = drafted(land_at);
    for &(t, until) in holds {
        let at = seen
            .journal
            .iter()
            .position(|(at, _)| *at > T0 + t)
            .unwrap_or(seen.journal.len());
        let hold = format!("WAITING seq=912 until={until} {PERSON_HOLD}");
        seen.journal.insert(at, j(t, &hold));
    }
    seen
}

/// The hold the loop journals at the switch's wake for the draft standing (+135 s).
const AT_THE_SWITCH: (i64, &str) = (15_800, "2026-10-24T18:42:35Z");
/// The hold it journals once it read the person's last key, their clearing, at +24.5 s: their
/// grace from it (+144.5 s).
const AT_THE_CLEARING: (i64, &str) = (24_500, "2026-10-24T18:42:44Z");

/// SWS (ATERM-KI-1031-1): nothing typed while the draft stands, nor inside the person's grace from
/// its clearing, timed by the lab (+23.7 s to +143.7 s); the hold the loop journals for them ends
/// no sooner than that and within [`CLEAR_READ_MS`] past it, and land@v1 comes within
/// [`LAND_WITHIN_MS`] of its end.
#[test]
fn a_switch_under_a_draft_waits_out_the_persons_grace_from_its_clearing() {
    assert_eq!(person_grace_ms(), 120_000, "the policy's default grace");
    let v = judge_draft(&person_held(&[AT_THE_SWITCH, AT_THE_CLEARING], 144_900), T0);
    let Verdict::Pass(said) = v else {
        panic!("{v:?}")
    };
    assert!(said.contains("the hold for the person to +144"), "{said}");
    // Controls. Main's agy run (2026-10-31): no hold after the clearing, land@v1 at once.
    let Verdict::Fail(why) = judge_draft(&person_held(&[AT_THE_SWITCH], 25_000), T0) else {
        panic!("land@v1 1.3 s after the clearing passed");
    };
    assert!(
        why.contains("typed inside the person's grace from the clearing"),
        "{why}"
    );
    // Nothing journaled for the person at all, whatever was typed when.
    let Verdict::Fail(why) = judge_draft(&drafted(144_000), T0) else {
        panic!("a run that journaled no hold for the person passed");
    };
    assert!(why.contains("no hold for the person journaled"), "{why}");
    // Main's Claude Code run: the hold from the draft's sighting, waited out, land@v1 after it.
    let Verdict::Fail(why) = judge_draft(&person_held(&[AT_THE_SWITCH], 135_500), T0) else {
        panic!("land@v1 at the end of the hold from the draft's sighting passed");
    };
    assert!(
        why.contains("inside their grace from the clearing"),
        "{why}"
    );
    // A hold past the grace by more than the loop's late read of their last key.
    let late = (24_500, "2026-10-24T18:43:05Z");
    let Verdict::Fail(why) = judge_draft(&person_held(&[AT_THE_SWITCH, late], 165_400), T0) else {
        panic!("a hold 21 s past the person's grace passed");
    };
    assert!(why.contains("past 15 s"), "{why}");
    // land@v1 too long after the hold.
    let Verdict::Fail(why) =
        judge_draft(&person_held(&[AT_THE_SWITCH, AT_THE_CLEARING], 151_000), T0)
    else {
        panic!("land@v1 7 s past the hold passed");
    };
    assert!(why.contains("past 5 s"), "{why}");
    // land@v1 typed over the standing draft.
    let Verdict::Fail(why) = judge_draft(&drafted(15_200), T0) else {
        panic!("land@v1 over the draft passed");
    };
    assert!(why.contains("typed while the draft stood"), "{why}");
    let mut unstood = drafted(144_900);
    if let Some(d) = unstood.cover.draft.as_mut() {
        d.stood = false;
    }
    assert!(matches!(judge_draft(&unstood, T0), Verdict::Unobserved(_)));
    let mut uncleared = drafted(144_900);
    if let Some(d) = uncleared.cover.draft.as_mut() {
        d.emptied = false;
    }
    assert!(matches!(
        judge_draft(&uncleared, T0),
        Verdict::Unobserved(_)
    ));
}

/// A DRAFT TYPED AND CLEARED IS NO NEW POINT (ATERM-KI-1101-2): the passing run journals no
/// `EVENT` line from the draft's typing to land@v1. NEGATIVE CONTROL: main's agy run (base-sws,
/// 2026-11-01) read the draft and its clearing as two points, `EVENT idle seq=924` and `seq=938`,
/// with no turn between.
#[test]
fn a_draft_typed_and_cleared_is_no_new_point() {
    let v = judge_draft(&person_held(&[AT_THE_SWITCH, AT_THE_CLEARING], 144_900), T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let mut repointed = person_held(&[AT_THE_SWITCH, AT_THE_CLEARING], 144_900);
    for (t, line) in [
        (9_900, "EVENT idle seq=924 ⏺ Implemented the parser module."),
        (
            24_600,
            "EVENT idle seq=938 ⏺ Implemented the parser module.",
        ),
    ] {
        let at = repointed
            .journal
            .iter()
            .position(|(at, _)| *at > T0 + t)
            .unwrap_or(repointed.journal.len());
        repointed.journal.insert(at, j(t, line));
    }
    let Verdict::Fail(why) = judge_draft(&repointed, T0) else {
        panic!("a draft read as two points passed");
    };
    assert!(
        why.contains(DRAFT_REPOINTED) && why.contains("+9.9") && why.contains("+24.6"),
        "{why}"
    );
}

/// A person's clearing keys buy their grace again, so the hold a later read journals is the one
/// land@v1 waits out, however late the loop reads it: agy decides again once its footer is back,
/// where Claude Code decides at the first hold's end.
#[test]
fn a_switch_under_a_draft_lands_at_the_end_of_the_last_hold_for_the_person() {
    const LAST: &str = "2026-10-24T18:42:50Z";
    let last_held =
        |land_at: i64, read_at: i64| person_held(&[AT_THE_SWITCH, (read_at, LAST)], land_at);
    for read_at in [29_000, 134_700] {
        let v = judge_draft(&last_held(150_700, read_at), T0);
        assert!(matches!(v, Verdict::Pass(_)), "{read_at}: {v:?}");
    }
    // Controls: typed between the clearing and the first hold's end, and between the two ends.
    for land_at in [35_000, 140_000] {
        let Verdict::Fail(why) = judge_draft(&last_held(land_at, 29_000), T0) else {
            panic!("land@v1 at +{land_at} ms, inside the last hold, passed");
        };
        assert!(
            why.contains("typed inside the person's grace from the clearing"),
            "{why}"
        );
    }
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

/// When `switch-over-held`'s hold was journaled, after [`T0`].
const HELD_AT: i64 = 372_000;

/// The stand-in's one reply, which held the tab.
const HELD_REPLY: &str = "Merged the parser and pushed it to origin/main. All done.";

/// When the person pressed Land, after [`T0`]: past `meta set`'s Land [`HELD_SWITCH_MS`] past the
/// hold and its Cruise [`CTL_WATCH_MS`] later, as the lab's word step makes them.
const PRESS_AT: i64 = HELD_AT + HELD_SWITCH_MS + CTL_WATCH_MS + SWITCH_SETTLE_MS;

/// How long after the switch Land's one ask was typed.
const ASK_AFTER: i64 = 1_300;

/// How long after the ask its answer held the tab again.
const AGAIN_AFTER: i64 = 16_000;

/// Land's one ask as the loop types it at the switch (`turn_end::switch_why` before [`LAND`]'s own
/// words).
fn asked_text() -> String {
    let mark = "[aterm harness]";
    format!(
        "{mark} {}{}",
        turn_end::switch_why(&turn_end::held_quote(HELD_REPLY)),
        turn_end::LAND.strip_prefix(mark).unwrap_or(turn_end::LAND)
    )
}

/// A tab held at the same reply under Cruise, switched to Land by `meta set` [`HELD_SWITCH_MS`]
/// past its hold with nothing typed, back to Cruise, then by the person's press at [`PRESS_AT`] and
/// woken; Land's one ask typed [`ASK_AFTER`] past the press, `status land_asked=switch` read while
/// it stood, held again [`AGAIN_AFTER`] past it, every `status` read from then on `held=<arm>
/// land_asked=-`, watched [`WATCH_MS`] past the second hold.
fn over_held() -> Seen {
    let sw = PRESS_AT;
    let ask = sw + ASK_AFTER;
    let again = ask + AGAIN_AFTER;
    let arm = arm_of(STALL).word();
    let idle = format!("EVENT idle seq=2681 ⏺ {HELD_REPLY}");
    Seen {
        journal: vec![
            j(HELD_AT, &idle),
            j(HELD_AT, STALL),
            j(sw + 1_100, WAKE),
            j(
                sw + 1_250,
                "CLEARED seq=2681 exception=stalled the agent was asked again",
            ),
            j(
                ask,
                &format!("CONTINUED seq=2681 rule=land@v1 {}", asked_text()),
            ),
            j(again, &idle.replace("2681", "2702")),
            j(again, &STALL.replace("2681", "2702")),
        ],
        ledger: vec![Row {
            command: asked_text(),
            ..row(ask, turn_end::RULE_LAND, "typed")
        }],
        status: vec![
            (
                T0 + HELD_AT + 1_000,
                format!("task=- autopilot=cruise held={arm} land_asked=-"),
            ),
            (
                T0 + ask + 2_000,
                "task=- autopilot=land held=- land_asked=switch".into(),
            ),
            (
                T0 + again + 1_000,
                format!("task=- autopilot=land held={arm} land_asked=-"),
            ),
        ],
        status_reads: (0..30).map(|i| T0 + sw + 3_000 * (i + 1)).collect(),
        end_ms: T0 + again + WATCH_MS,
        cover: Cover {
            words: vec![
                switched(HELD_AT + HELD_SWITCH_MS, "land"),
                switched(HELD_AT + HELD_SWITCH_MS + CTL_WATCH_MS, "cruise"),
                pressed(sw, "land"),
            ],
            ..Cover::default()
        },
        ..Seen::default()
    }
}

#[test]
fn a_held_tab_switched_to_land_asks_once_and_is_held_again_at_the_same_answer() {
    let v = judge_over_held(&over_held(), T0);
    let Verdict::Pass(said) = &v else {
        panic!("{v:?}");
    };
    assert!(
        said.contains("opened by the why") && said.contains("land_asked=switch"),
        "{said}"
    );
    // Controls, each a fail naming its fault: nothing typed at the switch (a build before 10b);
    // Land's plain instruction; the ask typed twice; no hold again; the ask unsaid by `status`.
    let mut silent = over_held();
    silent
        .journal
        .retain(|(_, l)| !l.starts_with("CONTINUED ") && !l.starts_with("CLEARED "));
    let Verdict::Fail(why) = judge_over_held(&silent, T0) else {
        panic!("no ask passed");
    };
    assert!(why.contains("NO ASK"), "{why}");
    let mut plain = over_held();
    plain.ledger[0].command = turn_end::LAND.into();
    let Verdict::Fail(why) = judge_over_held(&plain, T0) else {
        panic!("Land's plain instruction passed");
    };
    assert!(why.contains("opens with no why"), "{why}");
    let mut twice = over_held();
    let ask = PRESS_AT + ASK_AFTER;
    twice.journal.push(j(
        ask + AGAIN_AFTER + 30_000,
        &format!("CONTINUED seq=2702 rule=land@v1 {}", asked_text()),
    ));
    let Verdict::Fail(why) = judge_over_held(&twice, T0) else {
        panic!("a second prompt passed");
    };
    assert!(why.contains("typed again after the ask"), "{why}");
    let mut unheld = over_held();
    unheld.journal.truncate(5);
    let Verdict::Fail(why) = judge_over_held(&unheld, T0) else {
        panic!("no second hold passed");
    };
    assert!(why.contains("not held again"), "{why}");
    let mut misquoted = over_held();
    misquoted.ledger[0].command = asked_text().replace("Merged the parser", "Wrote the lexer");
    let Verdict::Fail(why) = judge_over_held(&misquoted, T0) else {
        panic!("a why quoting another reply passed");
    };
    assert!(why.contains("quotes no \"Merged the parser"), "{why}");
    let mut unsaid = over_held();
    unsaid.status[1].1 = "task=- autopilot=land held=- land_asked=-".into();
    let Verdict::Fail(why) = judge_over_held(&unsaid, T0) else {
        panic!("an ask status never said passed");
    };
    assert!(why.contains("never said land_asked=switch"), "{why}");
    let short = Seen {
        end_ms: T0 + ask + AGAIN_AFTER + WATCH_MS - 1,
        ..over_held()
    };
    assert!(matches!(
        judge_over_held(&short, T0),
        Verdict::Unobserved(_)
    ));
    // LIMIT 1 AT A SWITCH NO PERSON MADE: the ask typed at `meta set`'s Land (the build before
    // the review's fix) fails, named; with no `meta set` Land, or no press, nothing is judged.
    let mut ctl_asked = over_held();
    ctl_asked.journal.insert(
        2,
        j(
            HELD_AT + HELD_SWITCH_MS + ASK_AFTER,
            &format!("CONTINUED seq=2681 rule=land@v1 {}", asked_text()),
        ),
    );
    let Verdict::Fail(why) = judge_over_held(&ctl_asked, T0) else {
        panic!("an ask at the switch no person made passed");
    };
    assert!(why.contains("typed at the switch no person made"), "{why}");
    let mut unpressed = over_held();
    unpressed.cover.words.pop();
    assert!(matches!(
        judge_over_held(&unpressed, T0),
        Verdict::Unobserved(_)
    ));
    let mut no_ctl = over_held();
    no_ctl.cover.words.drain(..2);
    assert!(matches!(
        judge_over_held(&no_ctl, T0),
        Verdict::Unobserved(_)
    ));
}

/// `switch-over-held` as main records it since the continuous-forward design of 2026-11-01:
/// Cruise holds nothing at the repeated reply but waits the ladder's step for two short turns
/// (4 min) before its request ([`recovery_wait`]); `meta set`'s Land, no person's, holds there
/// (limit 1), and the person's press, inside Cruise's wait, asks once. Every other record as
/// [`over_held`]'s.
fn over_repeated() -> Seen {
    let until = |t: i64| crate::util::utc_iso((T0 + t).div_euclid(1000)).replace(".000Z", "Z");
    let wait = |t: i64| {
        j(
            t,
            &format!(
                "WAITING seq=2681 until={} {}",
                until(HELD_AT + 240_000),
                turn_end::REPEAT_RECOVERY_WAIT
            ),
        )
    };
    let mut s = over_held();
    let k = s.journal.iter().position(|(_, l)| l == STALL).unwrap();
    s.journal[k] = wait(HELD_AT);
    let ctl = HELD_AT + HELD_SWITCH_MS;
    s.journal.insert(k + 1, j(ctl + 1_100, STALL));
    s.journal.insert(
        k + 2,
        j(
            ctl + CTL_WATCH_MS + 1_100,
            "CLEARED seq=2681 exception=stalled someone's turn",
        ),
    );
    s.journal.insert(k + 3, wait(ctl + CTL_WATCH_MS + 1_200));
    s.status[0].1 = "task=- autopilot=cruise held=- land_asked=-".into();
    s
}

/// SWO on main's records passes: the repeat is Cruise's wait, not a hold. NEGATIVE CONTROLS:
/// Cruise's request typed before the person's press fails, named; records with neither a wait
/// nor a hold are unobserved.
#[test]
fn a_repeated_cruise_tab_switched_to_land_asks_once_and_land_holds() {
    let seen = over_repeated();
    let v = judge_over_held(&seen, T0);
    let Verdict::Pass(said) = &v else {
        panic!("{v:?}");
    };
    assert!(said.starts_with("repeated at +372.0 s (WAITING"), "{said}");
    assert!(said.contains("opened by the why"), "{said}");
    assert_eq!(repeat_of(&seen, T0).map(|(t, _)| *t), Some(T0 + HELD_AT));
    let mut requested = over_repeated();
    let at = HELD_AT + HELD_SWITCH_MS - 5_000;
    let k = requested
        .journal
        .iter()
        .position(|(t, _)| *t > T0 + at)
        .unwrap();
    let text: String = turn_end::FORWARD_RECOVERY.chars().take(120).collect();
    requested.journal.insert(
        k,
        j(at, &format!("CONTINUED seq=2681 rule=continue@v1 {text}")),
    );
    let Verdict::Fail(why) = judge_over_held(&requested, T0) else {
        panic!("a request before the press passed");
    };
    assert!(why.contains("typed after the repeat"), "{why}");
    let mut unrepeated = over_repeated();
    unrepeated
        .journal
        .retain(|(_, l)| !recovery_wait(l) && !is_stall(l));
    assert!(matches!(
        judge_over_held(&unrepeated, T0),
        Verdict::Unobserved(_)
    ));
}

/// The hold is read at a new point (the agent's same answer), so the switch's one ask after it is
/// no act typed twice for one point. NEGATIVE CONTROL: the same two acts with no point read
/// between are.
#[test]
fn the_switchs_ask_after_a_hold_is_no_doubled_act() {
    let doubled = |seen: &Seen| {
        crate::forever::chain_faults(seen, T0, false)
            .into_iter()
            .filter(|f| f.contains("no point judged between"))
            .collect::<Vec<_>>()
    };
    let mut seen = over_held();
    seen.journal.insert(
        0,
        j(
            HELD_AT - 3_500,
            "CONTINUED seq=2660 rule=continue-done-check@v1 [aterm harness] x",
        ),
    );
    assert_eq!(doubled(&seen), Vec::<String>::new());
    seen.journal
        .retain(|(_, l)| !l.starts_with("EVENT idle seq=2681 "));
    assert_eq!(doubled(&seen).len(), 1, "CONTROL: no point read between");
}

#[test]
fn a_held_tabs_state_line_says_lands_ask_then_its_stop_again() {
    let lead = stop_words(arm_of(STALL), StopForm::Line, false);
    let ask = T0 + PRESS_AT + ASK_AFTER;
    let again = ask + AGAIN_AFTER;
    let cx = Ctx {
        chain: Chain::SwitchOverHeld,
        program: "claude",
        t0: T0,
    };
    let mut seen = over_held();
    seen.window.page = vec![
        (ask + 3_000, format!("row=1\nstate={}", acts::SWITCH_ASKED)),
        (
            again + 5_000,
            format!("row=1\nstate={lead} · the same reply"),
        ),
    ];
    seen.window.page_reads = vec![ask + 3_000, ask + 9_000, again + 5_000, again + 9_000];
    let v = asked_line(&seen, &cx);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    // Controls: the ask never said while it stood; the stop not said again after the second hold.
    let mut unasked = seen.clone();
    unasked.window.page[0].1 = "row=1\nstate=landing".into();
    let Verdict::Fail(why) = asked_line(&unasked, &cx) else {
        panic!("an ask unsaid passed");
    };
    assert!(why.contains("leads with \""), "{why}");
    seen.window
        .page
        .push((again + 9_000, "row=1\nstate=landing".into()));
    let Verdict::Fail(why) = asked_line(&seen, &cx) else {
        panic!("a state line with no stop after the second hold passed");
    };
    assert!(why.contains("leads with no stop"), "{why}");
    seen.window.page.clear();
    seen.window.page_reads.clear();
    assert!(matches!(asked_line(&seen, &cx), Verdict::Unobserved(_)));
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
    let hold = over_held();
    let hsw = T0 + PRESS_AT;
    let again = hsw + ASK_AFTER + AGAIN_AFTER;
    assert!(!watched(Chain::SwitchOverHeld, &hold, again + WATCH_MS - 1));
    assert!(watched(Chain::SwitchOverHeld, &hold, again + WATCH_MS));
    let mut silent = over_held();
    silent.journal.truncate(3);
    assert!(!watched(Chain::SwitchOverHeld, &silent, again + WATCH_MS));
    assert!(watched(Chain::SwitchOverHeld, &silent, hsw + LAND_WATCH_MS));
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

/// `command-draft`'s run: [`COMMAND_DRAFT`] typed at +8.7 s into the back-off at rest, its hold
/// journalled at the person's grace's end (+128.7 s) where `held`, `sent_at` an act the draft
/// step's Enter sent it at (main's), cleared at +158.7 s, and the loop's next act at `next_at`.
fn commanded(held: bool, sent_at: Option<i64>, next_at: Option<i64>) -> Seen {
    let mut journal = vec![
        j(
            REST_AT,
            "EVENT idle seq=882 ⏺ Implemented the parser module.",
        ),
        j(REST_AT, WAITING),
    ];
    if held {
        journal.push(j(
            128_700,
            &format!(
                "HOLD seq=882 point nothing why=command a person's slash command {}",
                turn_end::COMMAND_STANDS
            ),
        ));
    }
    if let Some(t) = sent_at {
        journal.push(j(
            t,
            &format!("CONTINUED seq=882 rule=continue@v1 {COMMAND_DRAFT}"),
        ));
    }
    if let Some(t) = next_at {
        journal.push(j(t, "CONTINUED seq=882 rule=continue@v1 keep going"));
    }
    Seen {
        journal,
        end_ms: T0 + 320_000,
        cover: Cover {
            draft: Some(Draft {
                text: COMMAND_DRAFT.to_string(),
                typed_ms: T0 + 8_700,
                stood: true,
                cleared_ms: Some(T0 + 158_700),
                emptied: true,
            }),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// SWC's commands (v6 §14.17.5.8): each harness's is one its reader holds as a command, harmless
/// where a build sends it, and agy's goes in as a paste, the one `!` form agy's composer keeps as
/// text. Negative control: agy given the typed `/help` the others take: the paste check fails.
#[test]
fn each_harness_command_draft_is_a_command_its_reader_holds() {
    use crate::forever::Harness;
    for h in Harness::ALL {
        let (text, pasted) = command_draft(h);
        assert!(
            aterm_phase::draft_command(h.program(), &[text.to_string()]).is_some(),
            "{}: {text:?} reads as a command",
            h.name()
        );
        assert_eq!(pasted, h == Harness::Agy, "{}", h.name());
    }
    assert_eq!(
        aterm_phase::draft_command(aterm_phase::Program::Agy, &[AGY_COMMAND_DRAFT.to_string()]),
        Some(aterm_phase::DraftCommand::Shell)
    );
}

/// SWC (v6 §14.17.5.8): a person's command standing past their grace is never sent, its hold is
/// journalled once, and once they clear it the loop's next act comes past their grace from the
/// clearing (+278.7 s), within [`CLEAR_READ_MS`]. Controls: main's run, whose draft step sent it
/// at the grace's end and journalled no hold; an act inside the grace from the clearing; no act
/// past it (a loop parked for good); the hold journalled twice.
#[test]
fn a_persons_command_is_held_never_sent_and_the_loop_goes_on_once_it_clears() {
    let v = judge_command(&commanded(true, None, Some(280_000)), T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    assert!(v.why().contains("held +128.7"), "{v:?}");
    let Verdict::Fail(why) = judge_command(&commanded(false, Some(128_700), Some(280_000)), T0)
    else {
        panic!("main's run, its command sent, passed");
    };
    assert!(why.contains("typed while the command stood"), "{why}");
    assert!(why.contains("no hold journaled"), "{why}");
    let Verdict::Fail(why) = judge_command(&commanded(true, None, Some(200_000)), T0) else {
        panic!("an act inside the person's grace passed");
    };
    assert!(why.contains("inside the person's grace"), "{why}");
    let Verdict::Fail(why) = judge_command(&commanded(true, None, None), T0) else {
        panic!("a loop that never went on passed");
    };
    assert!(why.contains("did not go on"), "{why}");
    let mut twice = commanded(true, None, Some(280_000));
    let hold = twice.journal[2].clone();
    twice.journal.insert(3, (hold.0 + 1_000, hold.1));
    let Verdict::Fail(why) = judge_command(&twice, T0) else {
        panic!("a hold journalled twice passed");
    };
    assert!(why.contains("journaled 2 times"), "{why}");
}

/// The captures' switch-asked scene reads the repeat off the session's journal file: Cruise's
/// recovery wait, or an older build's hold; a plain back-off, or no journal, is none (THE NEGATIVE
/// CONTROL: the ladder's own step is no repeat).
#[test]
fn the_repeat_is_read_off_the_journal_file() {
    let line = |t: i64, l: &str| format!("{{\"t\":{t},\"line\":{l:?}}}");
    let backoff = line(
        T0,
        "WAITING seq=2 until=2026-10-25T10:07:44Z 2 short turn(s) in a row: 4 min back-off",
    );
    assert!(!repeat_journaled(&backoff));
    assert!(!repeat_journaled(""));
    let recovery = format!(
        "{backoff}\n{}",
        line(
            T0 + 1,
            &format!(
                "WAITING seq=3 until=2026-10-25T10:15:44Z {}",
                turn_end::REPEAT_RECOVERY_WAIT
            )
        )
    );
    assert!(repeat_journaled(&recovery));
    let held = line(
        T0,
        "EXCEPTION seq=3 kind=stalled when=later held=same-reply Its program answers aterm's acts \
         with the same reply and no work, so aterm types nothing more.",
    );
    assert!(repeat_journaled(&held));
}
