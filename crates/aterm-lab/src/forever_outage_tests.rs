// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the outage modes (`--mode outage-land`, `--mode outage-cruise`, v6 §14.17.5.2's 12b)
//! over records shaped as the loop writes them, the rungs at the outage modes' three eighths.

use aterm_agent::harness::acts::FACT_SEPARATOR;
use aterm_agent::supervise::policy::turn_end::RULE_OUTAGE_RESUME;
use aterm_phase::Program;
use aterm_phase::wall::WallKind;

use super::*;
use crate::forever::outage::{self, OutageMove};

/// Claude Code's final answer to a request the stub's outage met, as its screen reads at the point.
const ERRORED: &str =
    r#"API Error: 529 {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;

/// The outage's try as the policy types it, blind.
fn resume_text() -> String {
    outage::line(outage::WALL)
}

/// The loop's wait on the outage at `t`, its next try due at `due` (both after [`T0`]).
fn waits(t: i64, seq: u32, due: i64) -> (i64, String) {
    j(
        t,
        &format!(
            "WAITING seq={seq} until={} waiting: {}",
            stamp(due),
            outage::WALL.words()
        ),
    )
}

/// A short turn's back-off at `t`, due at `due`.
fn backs_off(t: i64, seq: u32, streak: u32, due: i64) -> (i64, String) {
    j(
        t,
        &format!(
            "WAITING seq={seq} until={} {streak} short turn(s) in a row: {} s back-off",
            stamp(due),
            (due - t) / 1000
        ),
    )
}

/// The outage's try typed at `t` (its journal line and ledger row): the line, or `text`.
fn tried(t: i64, seq: u32, text: &str) -> ((i64, String), Row) {
    (
        j(
            t,
            &format!("CONTINUED seq={seq} rule={RULE_OUTAGE_RESUME} {text}"),
        ),
        typed(t, RULE_OUTAGE_RESUME, text),
    )
}

/// The status words with `waiting` and `task` as given.
fn waiting(t: i64, task: &str, autopilot: &str, waiting: &str) -> (i64, String) {
    j(
        t,
        &format!(
            "task={task} needs=- autopilot={autopilot} supervisor=aterm-harness@1 agent=- \
             waiting={waiting}"
        ),
    )
}

/// How a fixture's loop times try `n` of the outage, met again at its wall `wall` (after [`T0`]),
/// the try before it (or the outage's meeting, for the first) at `prev`: the due its wait names,
/// and when it types.
type Plan = fn(u32, i64, i64) -> (i64, i64);

/// The loop as 12b times it: each wait names its rung from its wall, the try typed 100 ms past.
fn on_rungs(n: u32, wall: i64, _: i64) -> (i64, i64) {
    let due = wall + outage::rung_ms(n, outage::SCALE);
    (due, due + 100)
}

/// A fixture's tries on the outage ([`ladder`]).
struct Tries {
    journal: Vec<(i64, String)>,
    ledger: Vec<Row>,
    /// The fourth try's time after [`T0`]: the resume.
    resumed: i64,
    /// When the lab lifts the outage, after [`T0`]: once the third try met it and was waited on.
    lifted: i64,
}

/// The three tries on the outage's rungs after its meeting at `met` (after [`T0`]), each met by the
/// outage again 1.9 s on (the meeting 2.9 s on) and waited on, then the resume on the fourth rung,
/// each timed by `plan`; the points numbered from `from_seq`, the first try's words `first`.
fn ladder(met: i64, first: &str, from_seq: u32, plan: Plan) -> Tries {
    let mut journal = Vec::new();
    let mut ledger = Vec::new();
    let mut wall = met + 2_900;
    let mut prev = met;
    let mut lifted = 0;
    for (seq, n) in (from_seq..).zip(1..=4u32) {
        let (due, at) = plan(n, wall, prev);
        journal.push(j(wall - 100, &format!("EVENT idle seq={seq} {ERRORED}")));
        ledger.push(point(wall - 100, 1));
        journal.push(waits(wall, seq, due));
        if n == 4 {
            lifted = wall + 1_100;
        }
        let text = if n == 1 {
            first.to_string()
        } else {
            resume_text()
        };
        let (line, row) = tried(at, seq, &text);
        journal.push(line);
        ledger.push(row);
        prev = at;
        wall = at + 1_900;
    }
    Tries {
        journal,
        ledger,
        resumed: prev,
        lifted,
    }
}

/// `cruise-outage` passed: the first point's back-off, Cruise's done check meeting the outage
/// raised at that point, three tries on 45 s, 90 s and 180 s, the lift, the resume on 360 s, and
/// Cruise's own done check at its back-off after the resume's point.
fn cruise_outage() -> Seen {
    cruise_outage_by(on_rungs)
}

/// [`cruise_outage`] with its tries timed by `plan`.
fn cruise_outage_by(plan: Plan) -> Seen {
    let mut journal = vec![
        j(5_000, &format!("EVENT idle seq=1 {DONE_CLAIM}")),
        backs_off(5_100, 1, 1, 20_100),
        j(
            20_100,
            "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] done is never the end",
        ),
    ];
    let mut ledger = vec![
        point(5_000, 1),
        typed(20_100, "continue-done-check@v1", DONE_CHECK),
    ];
    let tries = ladder(20_100, &resume_text(), 2, plan);
    let resumed = tries.resumed;
    journal.extend(tries.journal);
    ledger.extend(tries.ledger);
    let point_at = resumed + 3_800;
    journal.push(j(point_at, &format!("EVENT idle seq=6 {DONE_CLAIM}")));
    journal.push(backs_off(point_at + 100, 6, 2, point_at + 30_100));
    journal.push(j(
        point_at + 30_200,
        "CONTINUED seq=6 rule=continue-done-check@v1 [aterm harness] done is never the end",
    ));
    ledger.push(point(point_at, 2));
    ledger.push(typed(
        point_at + 30_200,
        "continue-done-check@v1",
        DONE_CHECK,
    ));
    Seen {
        journal,
        ledger,
        status: vec![
            waiting(1_000, "open", "cruise", "-"),
            waiting(24_000, "open", "cruise", "overload"),
            waiting(resumed + 800, "open", "cruise", "-"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + point_at + 60_000,
        cover: Cover {
            outage_ms: Some(T0 + 5_500),
            outage_lifted_ms: Some(T0 + tries.lifted),
            outage_resumed: Some(1),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// `outage-land` passed: the outage raised before the first prompt, its first point a wait on it,
/// Land's instruction as the first try, two more tries, the lift, the resume, and the landing on
/// the resume's answer. Its first point counts no short turn: it met the outage.
fn land_outage() -> Seen {
    let tries = ladder(0, LAND, 1, on_rungs);
    let (mut journal, mut ledger, resumed) = (tries.journal, tries.ledger, tries.resumed);
    ledger[0] = point(2_800, 0);
    let done = resumed + 3_800;
    journal.push(j(done - 100, &format!("EVENT idle seq=5 {CLEAN}")));
    journal.push(j(done, &format!("DONE seq=5 rule=land@v1 {CLEAN}")));
    ledger.push(point(done, 1));
    ledger.push(task_done(done));
    Seen {
        journal,
        ledger,
        status: vec![
            waiting(1_000, "open", "land", "-"),
            waiting(3_500, "open", "land", "overload"),
            waiting(resumed + 800, "open", "land", "-"),
            waiting(done + 500, "landed", "land", "-"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + done + 200_000,
        stub: Some(StubSeen {
            baseline: 5,
            read: 2,
            new_turns: 0,
        }),
        cover: Cover {
            outage_ms: Some(T0),
            outage_lifted_ms: Some(T0 + tries.lifted),
            outage_resumed: Some(1),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

fn o1(s: &Seen) -> (&'static str, String) {
    one(Chain::LandOutage, s, "claude", Scenario::O1)
}

fn o2(s: &Seen) -> (&'static str, String) {
    one(Chain::CruiseOutage, s, "claude", Scenario::O2)
}

fn fails(v: (&'static str, String), needle: &str) {
    let (w, why) = v;
    assert_eq!(w, "FAIL", "{needle}: {why}");
    assert!(why.contains(needle), "{needle}: {why}");
}

/// The index of the journal line at `t` after [`T0`] that starts with `head`.
fn line_at(s: &Seen, t: i64, head: &str) -> usize {
    s.journal
        .iter()
        .position(|(at, l)| *at == T0 + t && l.starts_with(head))
        .expect("the line")
}

/// The times after [`T0`] of the outage's tries, in order.
fn tries_of(s: &Seen) -> Vec<i64> {
    s.acts(RULE_OUTAGE_RESUME)
        .iter()
        .map(|(t, _)| t - T0)
        .collect()
}

/// AN OUTAGE MET BY CRUISE IS A SILENT WAIT: tries on 45 s, 90 s and 180 s after the meeting, the
/// state line saying `waiting=overload`, one resume on 360 s after the lift, then Cruise's own act.
/// NEGATIVE CONTROLS: main's typed retry into it, a second resume, a try before its rung, a try
/// with no wait before it, Cruise's prompt typed into it, the state line never saying it, Cruise's
/// own act where the resume was due, a landing, the stub never reading the resume, and a hold
/// while it stood or on the resume's answer (a hold on Cruise's own act's answer passes).
#[test]
fn an_outage_met_by_cruise_is_tried_on_its_rungs_and_resumed_once() {
    let (w, why) = o2(&cruise_outage());
    assert_eq!(w, "PASS", "{why}");
    assert_eq!(
        tries_of(&cruise_outage()),
        [68_100, 160_100, 342_100, 704_100]
    );
    assert!(
        why.contains("Cruise's continue-done-check@v1 typed at +20.1 s met it")
            && why.contains("waited from +23.0 s")
            && why.contains(
                "3 outage-resume@v1: [1 at +68.1 s (45.1 s past its wall), \
                 2 at +160.1 s (90.1 s past its wall), 3 at +342.1 s (180.1 s past its wall)]"
            )
            && why.contains("status waiting=overload at +24.0 s")
            && why.contains("lifted at +345.1 s")
            && why.contains("the resume at +704.1 s (360.1 s past its wall)")
            && why.contains("Cruise went on: continue-done-check@v1 at +738.1 s"),
        "{why}"
    );
    assert_eq!(outage::went_on(&cruise_outage()), Some(T0 + 738_100));
    assert_eq!(
        outage::met(Chain::CruiseOutage, &cruise_outage()),
        Some(T0 + 20_100)
    );
    let mut s = cruise_outage();
    let i = line_at(&s, 160_100, "CONTINUED");
    s.journal.insert(
        i,
        j(
            111_000,
            "CONTINUED seq=3 rule=api-retry@v1 Claude Code reported the API overloaded.",
        ),
    );
    s.journal
        .insert(i, j(91_000, "WAITING seq=3 until=x overloaded retry 1"));
    s.ledger.push(typed(111_000, "api-retry@v1", "x"));
    fails(
        o2(&s),
        "TYPED INTO THE OUTAGE: api-retry@v1 at +111.0 s (main's typed retry)",
    );
    let mut s = cruise_outage();
    let resumed = 704_100;
    s.journal.push(j(
        resumed + 70_000,
        &format!("EVENT idle seq=7 {DONE_CLAIM}"),
    ));
    s.journal
        .push(backs_off(resumed + 70_100, 7, 3, resumed + 130_100));
    let (line, row) = tried(resumed + 130_200, 7, &resume_text());
    s.journal.push(line);
    s.ledger.push(row);
    s.end_ms = T0 + resumed + 200_000;
    fails(o2(&s), "A SECOND RESUME: outage-resume@v1 at +834.3 s");
    let mut s = cruise_outage();
    let i = line_at(&s, 160_100, "CONTINUED");
    s.journal[i].0 = T0 + 115_000;
    let r = s.ledger.iter().position(|r| r.ts == T0 + 160_100).unwrap();
    s.ledger[r].ts = T0 + 115_000;
    fails(
        o2(&s),
        "try 2 at +115.0 s BEFORE ITS RUNG: 45.0 s after its wall's wait at +70.0 s, its rung 90.0 s",
    );
    let mut s = cruise_outage();
    s.journal.remove(line_at(&s, 70_000, "WAITING "));
    fails(
        o2(&s),
        "try 2 at +160.1 s came with no wait on the outage journaled before it",
    );
    let mut s = cruise_outage();
    let i = line_at(&s, 160_100, "CONTINUED");
    s.journal[i].1 =
        "CONTINUED seq=3 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going".into();
    let r = s.ledger.iter().position(|r| r.ts == T0 + 160_100).unwrap();
    s.ledger[r] = typed(160_100, "continue@v1", CONTINUE_TEXT);
    fails(o2(&s), "TYPED INTO THE OUTAGE: continue@v1 at +160.1 s");
    let mut s = cruise_outage();
    s.status[1] = waiting(24_000, "open", "cruise", "-");
    fails(
        o2(&s),
        "NOT WAITING IN STATUS: `aterm ctl status` never said waiting=overload while the outage stood",
    );
    let mut s = cruise_outage();
    s.status
        .push(waiting(750_000, "open", "cruise", "overload"));
    fails(o2(&s), "STILL WAITING IN STATUS");
    let mut s = cruise_outage();
    let i = line_at(&s, resumed, "CONTINUED");
    s.journal[i].1 =
        "CONTINUED seq=5 rule=continue-done-check@v1 [aterm harness] done is never the end".into();
    let r = s.ledger.iter().position(|r| r.ts == T0 + resumed).unwrap();
    s.ledger[r] = typed(resumed, "continue-done-check@v1", DONE_CHECK);
    fails(
        o2(&s),
        "NO RESUME: continue-done-check@v1 typed at +704.1 s",
    );
    let mut s = cruise_outage();
    s.journal.push(j(300_000, "DONE seq=7 rule=land@v1 x"));
    fails(o2(&s), "DONE under Cruise at +300.0 s");
    let mut s = cruise_outage();
    s.cover.outage_resumed = Some(0);
    fails(o2(&s), "the stub read NO request carrying the resume");
    let stall = |t: i64| {
        j(
            t,
            "EXCEPTION seq=8 kind=stalled when=later held=same-reply x",
        )
    };
    let mut s = cruise_outage();
    s.journal.push(stall(741_600));
    let (w, why) = o2(&s);
    assert_eq!(
        w, "PASS",
        "a hold on Cruise's own act's answer is no outage's: {why}"
    );
    let mut s = cruise_outage();
    let i = line_at(&s, 707_900, "EVENT idle");
    s.journal.insert(i + 1, stall(707_950));
    fails(o2(&s), "on the resume's answer: the outage's tries counted");
    let mut s = cruise_outage();
    let i = line_at(&s, 160_100, "CONTINUED");
    s.journal.insert(i, stall(100_000));
    fails(o2(&s), "HELD at +100.0 s: aterm stopped typing into it");
    let mut s = cruise_outage();
    s.cover.outage_resumed = None;
    let (w, why) = o2(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("dumps after the lift were not read"), "{why}");
}

/// MAIN'S RULE FAILS THE OUTAGE: its typed retry a minute after the overload, with its own wait's
/// words ("overloaded retry 1"), and nothing on the outage's rungs, so the lab's hang detector
/// lifts it and no sooner.
#[test]
fn mains_typed_retry_into_an_overload_fails_it() {
    let retry = turn_end::wall_retry_text(
        turn_end::RULE_API_RETRY,
        WallKind::Overloaded,
        Program::Claude,
    );
    let met = 20_100;
    let lifted = met + outage::hold_at_most_ms(outage::SCALE);
    let mut s = cruise_outage();
    s.journal.retain(|(t, _)| *t <= T0 + met);
    s.ledger.retain(|r| r.ts <= T0 + met);
    s.journal.extend([
        j(23_000, &format!("EVENT idle seq=2 {ERRORED}")),
        j(
            23_100,
            &format!("WAITING seq=2 until={} overloaded retry 1", stamp(83_100)),
        ),
        j(
            83_200,
            &format!("CONTINUED seq=2 rule={} {retry}", turn_end::RULE_API_RETRY),
        ),
        j(86_000, &format!("EVENT idle seq=3 {ERRORED}")),
        j(
            86_100,
            &format!("WAITING seq=3 until={} overloaded retry 2", stamp(386_100)),
        ),
    ]);
    s.ledger.push(point(23_000, 1));
    s.ledger
        .push(typed(83_200, turn_end::RULE_API_RETRY, &retry));
    s.ledger.push(point(86_000, 1));
    s.cover.outage_lifted_ms = Some(T0 + lifted);
    s.end_ms = T0 + lifted + 30_000;
    s.status = vec![waiting(1_000, "open", "cruise", "-")];
    let (w, why) = o2(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("TYPED INTO THE OUTAGE: api-retry@v1 at +83.2 s (main's typed retry)")
            && why.contains("NOT WAITED: no `waiting: API overload` journaled"),
        "{why}"
    );
    s.cover.outage_lifted_ms = None;
    let mv = |now: i64| outage::outage_move(Chain::CruiseOutage, &s, T0, outage::SCALE, T0 + now);
    assert_eq!(mv(lifted - 1), OutageMove::Hold);
    assert_eq!(mv(lifted), OutageMove::Lift);
}

/// AN OUTAGE FROM LAND'S FIRST PROMPT GIVES LAND'S INSTRUCTION AS ITS FIRST TRY (12a's rule), the
/// line after, and lands on the resume's answer. Its first point, a wait on the outage, counts no
/// short turn (P3 starts nowhere there). NEGATIVE CONTROLS: the line where the instruction was
/// owed, a landing while it stood, a turn typed between the resume and the landing, no landing,
/// and a first point that waited on no outage closing no short turn.
#[test]
fn an_outage_from_lands_first_prompt_gives_the_instruction_first_and_lands_on_the_resume() {
    let (w, why) = o1(&land_outage());
    assert_eq!(w, "PASS", "{why}");
    assert_eq!(
        tries_of(&land_outage()),
        [48_000, 140_000, 322_000, 684_000]
    );
    assert!(
        why.contains("the overload raised at +0.0 s, before the first prompt")
            && why.contains("waited from +2.9 s")
            && why.contains("status waiting=overload at +3.5 s")
            && why.contains("lifted at +325.0 s")
            && why.contains("the resume at +684.0 s (360.1 s past its wall)")
            && why.contains("landed on its answer: DONE at +687.8 s"),
        "{why}"
    );
    assert_eq!(outage::met(Chain::LandOutage, &land_outage()), Some(T0));
    let mut s = land_outage();
    let i = line_at(&s, 48_000, "CONTINUED");
    (s.journal[i], s.ledger[1]) = tried(48_000, 1, &resume_text());
    fails(
        o1(&s),
        "try 1 at +48.0 s: not Land's instruction alone, which was never given before it",
    );
    let mut s = land_outage();
    let i = line_at(&s, 140_000, "CONTINUED");
    (s.journal[i], s.ledger[3]) = tried(140_000, 2, LAND);
    fails(o1(&s), "try 2 at +140.0 s: not the outage's line alone");
    let mut s = land_outage();
    let i = line_at(&s, 140_000, "CONTINUED");
    s.journal.insert(i, j(100_000, "DONE seq=2 rule=land@v1 x"));
    fails(o1(&s), "LANDED at +100.0 s while the outage stood");
    let mut s = land_outage();
    let done = line_at(&s, 687_800, "DONE ");
    s.journal.insert(
        done,
        j(
            686_000,
            "CONTINUED seq=5 rule=land-nudge@v1 [aterm harness] Still landing: x",
        ),
    );
    s.journal.insert(
        done,
        j(
            685_900,
            "WAITING seq=5 until=x 1 short turn(s) in a row: 2 min back-off",
        ),
    );
    s.ledger.push(typed(686_000, "land-nudge@v1", LAND_NUDGE));
    fails(
        o1(&s),
        "NOT LANDED ON THE RESUME'S ANSWER: land-nudge@v1 typed at +686.0 s",
    );
    let mut s = land_outage();
    s.journal.retain(|(_, l)| !l.starts_with("DONE "));
    s.end_ms = T0 + 684_000 + outage_lands_within();
    fails(o1(&s), "NOT LANDED: no DONE");
    let mut s = land_outage();
    s.journal.truncate(s.journal.len() - 2);
    s.end_ms = T0 + 685_000;
    let (w, why) = o1(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("not landed yet"), "{why}");
    let mut s = land_outage();
    s.cover.outage_ms = None;
    s.cover.outage_lifted_ms = None;
    let (w, why) = o1(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never raised"), "{why}");
    let p3 = |s: &Seen| chain_faults(s, T0, Chain::LandOutage.first_reply_works());
    assert_eq!(p3(&land_outage()), Vec::<String>::new());
    let mut s = land_outage();
    s.journal[1].1 = "NOTE seq=1 the point waited on no outage".into();
    assert!(
        p3(&s)
            .iter()
            .any(|f| f.contains("the first point at +2.8 s closed 0 short turn(s), not 1")),
        "{:?}",
        p3(&s)
    );
}

/// How long past the resume Land must have landed: the take window and a grace.
fn outage_lands_within() -> i64 {
    take_window_ms() + PERSISTENCE_GRACE_MS
}

/// THE LAB STANDS THE OUTAGE WHERE THE CHAIN'S FIRST ACT MEETS IT AND LIFTS IT ON ITS TRIES: Land's
/// before the first prompt, Cruise's at its first point while nothing was typed; lifted once three
/// acts met it and a wait followed the last, or past the hang detector's hold. NEGATIVE CONTROLS:
/// another chain, a Cruise chain before its point or past its first act, a lift before the third
/// try's wait, and anything once lifted.
#[test]
fn the_lab_stands_the_outage_for_the_first_act_and_lifts_it_on_the_third_try() {
    let mv = |c: Chain, s: &Seen, now: i64| outage::outage_move(c, s, T0, outage::SCALE, T0 + now);
    let none = Seen::default();
    assert_eq!(mv(Chain::LandOutage, &none, 0), OutageMove::Raise);
    assert_eq!(mv(Chain::CruiseOutage, &none, 0), OutageMove::Hold);
    assert_eq!(mv(Chain::CruiseWall, &none, 0), OutageMove::Hold);
    let pointed = Seen {
        journal: vec![j(5_000, &format!("EVENT idle seq=1 {DONE_CLAIM}"))],
        ..Seen::default()
    };
    assert_eq!(mv(Chain::CruiseOutage, &pointed, 5_100), OutageMove::Raise);
    assert_eq!(mv(Chain::LandOutage, &pointed, 5_100), OutageMove::Hold);
    assert_eq!(mv(Chain::Wall, &pointed, 5_100), OutageMove::Hold);
    let mut s = cruise_outage();
    s.cover.outage_lifted_ms = None;
    s.cover.outage_ms = None;
    assert_eq!(mv(Chain::CruiseOutage, &s, 400_000), OutageMove::Hold);
    let upto = |t: i64| {
        let mut s = cruise_outage();
        s.journal.retain(|(at, _)| *at <= T0 + t);
        s.cover.outage_lifted_ms = None;
        s
    };
    assert_eq!(
        mv(Chain::CruiseOutage, &upto(20_000), 20_000),
        OutageMove::Hold
    );
    assert_eq!(
        mv(Chain::CruiseOutage, &upto(342_100), 342_200),
        OutageMove::Hold
    );
    assert_eq!(
        mv(Chain::CruiseOutage, &upto(344_000), 344_100),
        OutageMove::Lift
    );
    let hold = outage::hold_at_most_ms(outage::SCALE);
    assert_eq!(hold, 45_000 + 90_000 + 180_000 + 3 * PERSISTENCE_GRACE_MS);
    assert_eq!(
        mv(Chain::CruiseOutage, &upto(160_100), 20_100 + hold - 1),
        OutageMove::Hold
    );
    assert_eq!(
        mv(Chain::CruiseOutage, &upto(160_100), 20_100 + hold),
        OutageMove::Lift
    );
    let mut s = land_outage();
    s.cover.outage_lifted_ms = None;
    s.journal.retain(|(at, _)| *at <= T0 + 323_900);
    assert_eq!(mv(Chain::LandOutage, &s, 323_900), OutageMove::Lift);
    s.cover.outage_lifted_ms = Some(T0 + 323_900);
    assert_eq!(mv(Chain::LandOutage, &s, 324_000), OutageMove::Hold);
}

/// THE OUTAGE MODES RUN CLAUDE CODE ALONE, EACH ONE CHAIN AND ITS SCENARIO, ON RUNGS OF 45 S
/// DOUBLING (THE FIRST PAST THE TAKE WINDOW), AND READ THE WINDOW: the stub's answer is Claude Code's overload with no retry and no
/// end, the line is 12a's blind resume line naming the outage, and a resume's answer is the
/// chain's script (Land's clean defense, Cruise's done claim). NEGATIVE CONTROLS: agy and Muse
/// Code out of reach, and an outage's wait is no other `WAITING`.
#[test]
fn the_outage_modes_run_claude_code_on_rungs_of_forty_five_seconds_doubling() {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    for (w, m, c, sc) in [
        (
            "outage-land",
            Mode::OutageLand,
            Chain::LandOutage,
            Scenario::O1,
        ),
        (
            "outage-cruise",
            Mode::OutageCruise,
            Chain::CruiseOutage,
            Scenario::O2,
        ),
    ] {
        let a = parse_args(&v(&["--harness", "all", "--mode", w]))
            .unwrap()
            .unwrap();
        assert_eq!(a.mode, m);
        assert_eq!(a.chains(Harness::Claude), [c]);
        assert!(a.chains(Harness::Agy).is_empty() && a.chains(Harness::Muse).is_empty());
        assert_eq!(m.walled(), Some(sc));
        assert_eq!(Chain::of_mode(m), [c]);
        assert_eq!(c.scenarios(), [sc]);
        assert!(c.outage() && !c.walled() && !c.managed(), "{c:?}");
        assert_eq!(wall_out_of_reach(Harness::Claude, m), None);
        for h in [Harness::Agy, Harness::Muse] {
            assert!(wall_out_of_reach(h, m).is_some(), "{h:?} {m:?}");
        }
        assert!(Chain::every().contains(&c));
        assert_eq!(surfaces::unread_why(true, &[c]), None, "{c:?}");
    }
    assert!(Chain::LandOutage.outage_at_prompt() && Chain::LandOutage.lands());
    assert!(!Chain::CruiseOutage.outage_at_prompt() && !Chain::CruiseOutage.lands());
    assert_eq!(
        [Chain::LandOutage, Chain::CruiseOutage].map(Chain::autopilot),
        ["land", "cruise"]
    );
    assert!(!Chain::Wall.outage() && !Chain::CruiseWall.outage());
    assert_eq!([Scenario::O1, Scenario::O2].map(Scenario::id), ["O1", "O2"]);
    assert_eq!(
        (1..=6)
            .map(|n| outage::rung_ms(n, outage::SCALE))
            .collect::<Vec<_>>(),
        [45_000, 90_000, 180_000, 360_000, 720_000, 1_350_000]
    );
    assert_eq!(outage::rung_ms(9, outage::SCALE), 1_350_000);
    assert!(outage::rung_ms(1, outage::SCALE) > take_window_ms());
    assert_eq!(outage::rung_ms(1, 1.0), 120_000);
    let spec = outage::spec();
    assert_eq!(spec.get("status").and_then(J::as_i64), Some(529));
    assert_eq!(
        spec.get("headers")
            .and_then(|h| h.get("x-should-retry"))
            .and_then(J::as_str),
        Some("false")
    );
    assert_eq!(
        spec.get("json")
            .and_then(|b| b.get("error"))
            .and_then(|e| e.get("type"))
            .and_then(J::as_str),
        Some("overloaded_error")
    );
    assert!(spec.get("times").is_none());
    assert_ne!(outage::MODE, "overloaded");
    assert!(outage::is_wait(&waits(1_000, 1, 16_000).1));
    for other in [
        "WAITING seq=1 until=2026-10-27T10:00:00Z overloaded retry 1".to_string(),
        "WAITING seq=1 until=2026-10-27T10:00:00Z 1 short turn(s) in a row: 2 min back-off".into(),
        format!("EVENT idle seq=1 waiting: {}", outage::WALL.words()),
    ] {
        assert!(!outage::is_wait(&other), "{other}");
    }
    let line = resume_text();
    assert_eq!(
        reply(Chain::LandOutage, &[LAND, &line, &line]).as_deref(),
        Some(CLEAN)
    );
    assert_eq!(
        reply(Chain::CruiseOutage, &[&line]).as_deref(),
        Some(DONE_CLAIM)
    );
    assert_eq!(reply(Chain::LandOutage, &[]).as_deref(), Some(WORK));
}

/// THE RESUME REACHES THE STUB: the requests of the chain's conversation read after the lift whose
/// last user text is the outage's line. NEGATIVE CONTROLS: one read before the lift, another
/// conversation's, and one whose last user text is another.
#[test]
fn the_resume_reaching_the_stub_is_counted_from_the_lift() {
    let token = Chain::CruiseOutage.token();
    let body = |users: &[&str]| -> Vec<u8> {
        let messages: Vec<String> = users
            .iter()
            .map(|u| format!(r#"{{"role": "user", "content": "{u}"}}"#))
            .collect();
        format!(r#"{{"messages": [{}]}}"#, messages.join(", ")).into_bytes()
    };
    let prompt = Chain::CruiseOutage.prompt();
    let other = Chain::LandOutage.prompt();
    let line = resume_text();
    let dumps = vec![
        (T0 + 100, body(&[&prompt, &line])),
        (T0 + 300, body(&[&prompt, &line])),
        (T0 + 400, body(&[&other, &line])),
        (T0 + 500, body(&[&prompt, &line, "carry on"])),
        (T0 + 600, body(&[&prompt, DONE_CHECK, &line])),
    ];
    assert_eq!(outage::resumed_requests(&dumps, &token, T0 + 200), 2);
    assert_eq!(outage::resumed_requests(&dumps, &token, T0 + 600), 0);
}

/// A TRY IS TIMED FROM ITS OWN WALL, ON ITS RUNG (v6 §14.17.5.2: the doubling is counted from each
/// try's own wall): each wait names its rung from the point the outage met the agent again, and
/// the try comes at it. NEGATIVE CONTROLS: a first try a rung late, a ladder a quarter long, and
/// a first rung counted from the act that met the outage rather than from its wall.
#[test]
fn a_try_is_timed_from_its_own_wall_on_its_rung() {
    let late = |n: u32, wall: i64, _: i64| {
        let rung = outage::rung_ms(n, outage::SCALE);
        let due = wall + if n == 1 { 2 * rung } else { rung };
        (due, due + 100)
    };
    fails(o2(&cruise_outage_by(late)), "try 1 at +113.1 s LATE");
    let long = |n: u32, wall: i64, _: i64| {
        let due = wall + outage::rung_ms(n, outage::SCALE) * 5 / 4;
        (due, due + 100)
    };
    fails(
        o2(&cruise_outage_by(long)),
        "try 1: its wall's wait at +23.0 s OFF ITS RUNG",
    );
    let from_the_act = |n: u32, _: i64, prev: i64| {
        let due = prev + outage::rung_ms(n, outage::SCALE);
        (due, due + 100)
    };
    fails(
        o2(&cruise_outage_by(from_the_act)),
        "try 1 at +65.2 s BEFORE ITS RUNG: 42.2 s after its wall's wait at +23.0 s, its rung 45.0 s",
    );
}

/// A TRY IS COUNTED FROM THE OUTAGE'S FIRST READING WHERE THE MODE'S OWN ACT MET IT (the
/// 2026-10-27 O2 run): the loop read the outage half a second after Cruise's act, waited out the
/// act's take window, then waited on the outage until its rung from that reading. NEGATIVE
/// CONTROL: the same wait a rung on from neither the reading nor the wait is off its rung.
#[test]
fn a_try_after_the_modes_own_act_is_counted_from_its_first_reading() {
    let moved = |due: i64| {
        let mut s = cruise_outage();
        let read = line_at(&s, 22_900, "EVENT idle");
        s.journal[read].0 = T0 + 20_600;
        let waited = line_at(&s, 23_000, "WAITING ");
        s.journal[waited] = waits(50_600, 2, due);
        s.journal.insert(
            waited,
            j(
                20_700,
                "WAITING seq=2 until=x the worker to take the continue-done-check@v1 act",
            ),
        );
        let tried = line_at(&s, 68_100, "CONTINUED");
        s.journal[tried].0 = T0 + due + 100;
        for r in &mut s.ledger {
            if r.ts == T0 + 22_900 {
                r.ts = T0 + 20_600;
            } else if r.ts == T0 + 68_100 {
                r.ts = T0 + due + 100;
            }
        }
        s
    };
    let (w, why) = o2(&moved(20_600 + 45_000));
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("1 at +65.7 s (45.1 s past its wall)"), "{why}");
    fails(
        o2(&moved(20_600 + 30_000)),
        "try 1: its wall's wait at +50.6 s OFF ITS RUNG",
    );
}

/// NOTHING MEASURES AN OVERLOAD'S END (v6 §14.17.5.2: what the host cannot measure is tried
/// blind), so the resume after the lift is the blind line on its rung. NEGATIVE CONTROL: a resume
/// typed as a measured end just past the lift.
#[test]
fn an_overloads_resume_is_never_a_measured_end() {
    let mut s = cruise_outage();
    let i = line_at(&s, 704_100, "CONTINUED");
    let r = s.ledger.iter().position(|r| r.ts == T0 + 704_100).unwrap();
    let measured = format!(
        "{MARK} The {} has cleared; continue where you stopped.",
        outage::WALL.words()
    );
    (s.journal[i], s.ledger[r]) = tried(350_000, 5, &measured);
    let (w, why) = o2(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("try 4 at +350.0 s: not the outage's line alone")
            && why.contains("try 4 at +350.0 s BEFORE ITS RUNG"),
        "{why}"
    );
}

/// A ONE-OFF CUT AFTER THE RESUME KEEPS ITS ACT (v6 §14.17.5.2: `api-cutoff@v1` stays for a
/// one-off cut): it is no second resume, and Cruise's own act after it is Cruise going on.
/// NEGATIVE CONTROL: main's retired typed retry in its place is one resume too many.
#[test]
fn a_one_off_cut_after_the_resume_keeps_its_act() {
    let cut = |rule: &str| {
        let mut s = cruise_outage();
        let k = line_at(&s, 707_900, "EVENT idle");
        s.journal.splice(
            k..k,
            [
                j(705_000, "EVENT idle seq=5 API Error: Connection error."),
                j(
                    705_100,
                    &format!("WAITING seq=5 until={} cut-off retry 1", stamp(706_000)),
                ),
                j(
                    706_000,
                    &format!("CONTINUED seq=5 rule={rule} Claude Code's reply was cut off."),
                ),
            ],
        );
        s.ledger.push(point(705_000, 1));
        s.ledger.push(typed(706_000, rule, "x"));
        s
    };
    let s = cut(turn_end::RULE_API_CUTOFF);
    let (w, why) = o2(&s);
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("Cruise went on: continue-done-check@v1 at +738.1 s"),
        "{why}"
    );
    assert_eq!(outage::went_on(&s), Some(T0 + 738_100));
    fails(
        o2(&cut(turn_end::RULE_API_RETRY)),
        "A SECOND RESUME: api-retry@v1 at +706.0 s (main's typed retry)",
    );
}

/// The local `HH:MM` of `t` after [`T0`], as the window's next try reads.
fn clock(t: i64) -> String {
    let (.., h, m, _) = crate::sys::local_time((T0 + t).div_euclid(1000));
    format!("{h:02}:{m:02}")
}

/// `cruise-outage` passed, its window read: the Harness row's state line saying the wait, its next
/// try the first wait's due, while the outage stood, and the last act after the resume.
fn cruise_outage_read() -> Seen {
    let row = |state: &str| format!("head=cruise-outage: Window 1, tab 1\nstate={state}");
    let mut s = cruise_outage();
    s.window = surfaces::Window {
        read: true,
        label: Chain::CruiseOutage.name().into(),
        page: vec![
            (
                T0 + 24_500,
                row(&format!(
                    "waiting: API overload{FACT_SEPARATOR}next try {}",
                    clock(68_000)
                )),
            ),
            (T0 + 706_000, row("last: \u{2713} continued 10:00")),
        ],
        page_reads: vec![T0 + 24_500, T0 + 60_000, T0 + 706_000],
        ..surfaces::Window::default()
    };
    s
}

/// THE STATE LINE SAYS THE OUTAGE'S WAIT (v6 §14.17.5.2): while it stood, the tab's Settings ▸
/// Harness row led with `waiting: API overload · next try HH:MM`, the clock a due its waits named,
/// with no need and no chip title raised for it; the run reads the window for it. NEGATIVE
/// CONTROLS: a state line that never says the wait, a next try no wait named, a need on the line,
/// a chip title naming the outage, and a page not read while the outage stood.
#[test]
fn the_state_line_says_the_outages_wait_and_raises_nothing() {
    let cx = surfaces::Ctx {
        chain: Chain::CruiseOutage,
        program: "claude",
        t0: T0,
    };
    let v = outage::state_line(&cruise_outage_read(), &cx);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    assert!(
        v.why().contains(&format!(
            "+24.5 s \"waiting: API overload{FACT_SEPARATOR}next try "
        )),
        "{v:?}"
    );
    let (w, why) = o2(&cruise_outage_read());
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains(outage::STATE_LINE_WANT), "{why}");
    let restate = |state: &str| {
        let mut s = cruise_outage_read();
        s.window.page[0].1 = format!("head=cruise-outage: Window 1, tab 1\nstate={state}");
        s
    };
    let failed = |s: &Seen, needle: &str| {
        let v = outage::state_line(s, &cx);
        assert!(
            matches!(&v, Verdict::Fail(why) if why.contains(needle)),
            "{needle}: {v:?}"
        );
    };
    let tomorrow = restate(&format!(
        "waiting: API overload{FACT_SEPARATOR}next try Wed {}",
        clock(68_000)
    ));
    let v = outage::state_line(&tomorrow, &cx);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let quiet = restate("last: \u{2713} continued 10:00");
    failed(&quiet, "NEVER SAID WAITING");
    fails(o2(&quiet), "NEVER SAID WAITING");
    failed(
        &restate(&format!(
            "waiting: API overload{FACT_SEPARATOR}next try 99:99"
        )),
        "its next try \"99:99\" is no due its waits named",
    );
    failed(
        &restate(&format!(
            "needs you: retry the API; carrying on{FACT_SEPARATOR}waiting: API overload"
        )),
        "a need on the state line",
    );
    let mut titled = cruise_outage_read();
    titled.window.title = vec![(T0 + 30_000, "waiting: API overload".into())];
    failed(&titled, "the chip's title names the outage");
    let mut unread = cruise_outage_read();
    unread.window.page.remove(0);
    unread.window.page_reads = vec![T0 + 706_000];
    let v = outage::state_line(&unread, &cx);
    assert!(
        matches!(&v, Verdict::Unobserved(why) if why.contains("not read while the outage stood")),
        "{v:?}"
    );
}
