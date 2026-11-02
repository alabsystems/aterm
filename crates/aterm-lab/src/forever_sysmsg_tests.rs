// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `--mode sysmsg` (v6 §14.17.5's 12a): the forms and their chains, and the judge, each
//! want with the run that breaks it.

use aterm_agent::supervise::Outage;
use aterm_agent::supervise::policy::turn_end::{
    self, Cleared, Clearing, RULE_OUTAGE_RESUME, cleared_text,
};

use super::*;
use crate::forever::sysmsg::{self, Form, STANDS_MS};

const MET: i64 = 125_000;
/// The wall's first reading: the point the loop read past the act that met it.
const WALL: i64 = MET + 2_000;
/// The outage's first try, on its rung (2 min) from the act, into the standing wall.
const TRY: i64 = MET + 121_000;
/// The clearing: [`STANDS_MS`] past the wall's first reading.
const CLEAR: i64 = WALL + STANDS_MS;
/// The resume: the second rung (4 min) from the try's own wall, past the clearing.
const RESUMED: i64 = TRY + 2_000 + 240_000;

fn status(t: i64, waiting: &str) -> (i64, String) {
    j(
        t,
        &format!(
            "task=open needs=- autopilot=cruise supervisor=aterm-harness@1 agent=- \
             waiting={waiting}"
        ),
    )
}

/// The blind resume of `o`, as the policy types it.
fn blind(o: Outage) -> String {
    cleared_text(Cleared::Outage(o), Clearing::Blind).to_string()
}

/// The run on Claude Code's 529: Cruise's done check meets the wall, the harness waits on the
/// outage's first rung and tries its line once into the standing wall, waits the second rung past
/// the clearing, types its line once more, and Cruise goes on.
fn overloaded() -> Seen {
    let line = blind(Outage::Overload);
    Seen {
        journal: vec![
            j(5_000, &format!("EVENT idle seq=1 {DONE_CLAIM}")),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                MET,
                "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] done is never the end",
            ),
            j(
                WALL,
                "EVENT idle seq=2 API Error: Repeated 529 Overloaded errors",
            ),
            j(
                WALL + 100,
                &format!("WAITING seq=2 until={} waiting: API overload", stamp(TRY)),
            ),
            j(
                TRY,
                &format!("CONTINUED seq=2 rule={RULE_OUTAGE_RESUME} {line}"),
            ),
            j(
                TRY + 2_000,
                "EVENT idle seq=3 API Error: Repeated 529 Overloaded errors",
            ),
            j(
                TRY + 2_100,
                &format!(
                    "WAITING seq=3 until={} waiting: API overload",
                    stamp(RESUMED)
                ),
            ),
            j(
                RESUMED,
                &format!("CONTINUED seq=3 rule={RULE_OUTAGE_RESUME} {line}"),
            ),
            j(RESUMED + 5_000, &format!("EVENT idle seq=4 {DONE_CLAIM}")),
            j(
                RESUMED + 5_100,
                "WAITING seq=4 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                RESUMED + 125_000,
                "CONTINUED seq=4 rule=continue-done-check@v1 [aterm harness] done is never the end",
            ),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(MET, "continue-done-check@v1", DONE_CHECK),
            typed(TRY, RULE_OUTAGE_RESUME, &line),
            typed(RESUMED, RULE_OUTAGE_RESUME, &line),
            point(RESUMED + 5_000, 1),
            typed(RESUMED + 125_000, "continue-done-check@v1", DONE_CHECK),
        ],
        status: vec![status(1_000, "-"), status(MET + 3_000, "overload")],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + RESUMED + 150_000,
        cover: Cover {
            outage_ms: Some(T0 + 6_000),
            outage_lifted_ms: Some(T0 + CLEAR),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

fn verdict(form: Form, seen: &Seen, program: &str) -> (&'static str, String) {
    one(form.chain(), seen, program, Scenario::Sys(form))
}

#[test]
fn a_wait_waited_out_and_resumed_once_in_its_line_passes() {
    let (w, why) = verdict(Form::Overload, &overloaded(), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("Cruise's continue-done-check@v1 typed at +125.0 s met it")
            && why.contains("waited from +127.1 s")
            && why.contains("status waiting=overload at +128.0 s")
            && why.contains("1 try on the ladder's rungs while it stood")
            && why.contains(&format!("one resume, {RULE_OUTAGE_RESUME} at +488.0 s"))
            && why.contains("Cruise went on with continue-done-check@v1 at +613.0 s"),
        "{why}"
    );
}

#[test]
fn a_prompt_typed_into_the_standing_wall_fails() {
    // Main's run on Muse Code's dropped host (no reader for its spent tries): the wall a plain
    // point, Cruise's own act typed into it on its short-turn back-off.
    let mut s = overloaded();
    let into = WALL + 241_000;
    s.journal.push(j(
        into,
        "CONTINUED seq=3 rule=continue@v1 [aterm harness] keep going",
    ));
    s.journal.sort_by_key(|(t, _)| *t);
    s.ledger.push(typed(into, "continue@v1", "keep going"));
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("TYPED INTO THE STANDING WALL: continue@v1 at +368.0 s"),
        "{why}"
    );
    // A try off its rung (a minute from the act, a poll's resume) is typed into the wall too.
    let mut s = overloaded();
    for (t, _) in &mut s.journal {
        if *t == T0 + TRY {
            *t = T0 + MET + 60_000;
        }
    }
    for r in &mut s.ledger {
        if r.ts == T0 + TRY {
            r.ts = T0 + MET + 60_000;
        }
    }
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains(&format!(
            "TYPED INTO THE STANDING WALL: {RULE_OUTAGE_RESUME} at +185.0 s, off its rung"
        )),
        "{why}"
    );
}

#[test]
fn a_wait_never_journaled_or_never_in_status_fails() {
    let mut s = overloaded();
    s.status = vec![j(1_000, "task=open needs=- autopilot=cruise agent=-")];
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("NOT WAITING IN STATUS: `aterm ctl status` never said waiting=overload"),
        "{why}"
    );
    // Muse Code reads the Responses API's 503 as a server error: its wait is an API error's.
    let (w, why) = verdict(Form::Overload, &overloaded(), "muse");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("never said waiting=api-error"), "{why}");
    let mut s = overloaded();
    s.status = vec![status(1_000, "-"), status(MET + 3_000, "api-error")];
    for (_, l) in &mut s.journal {
        *l = l
            .replace("waiting: API overload", "waiting: API error")
            .replace(&blind(Outage::Overload), &blind(Outage::ApiError));
    }
    for r in &mut s.ledger {
        r.command = r
            .command
            .replace(&blind(Outage::Overload), &blind(Outage::ApiError));
    }
    let (w, why) = verdict(Form::Overload, &s, "muse");
    assert_eq!(w, "PASS", "{why}");
    let mut s = overloaded();
    s.journal
        .retain(|(_, l)| !l.contains("waiting: API overload"));
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("NOT WAITED"), "{why}");
}

#[test]
fn a_second_resume_a_resume_in_other_words_or_none_fails() {
    let line = blind(Outage::Overload);
    let mut twice = overloaded();
    let again = RESUMED + 60_000;
    twice.journal.push(j(
        again,
        &format!("CONTINUED seq=3 rule={RULE_OUTAGE_RESUME} {line}"),
    ));
    twice.journal.sort_by_key(|(t, _)| *t);
    twice.ledger.push(typed(again, RULE_OUTAGE_RESUME, &line));
    let (w, why) = verdict(Form::Overload, &twice, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("2 resumes past the clearing"), "{why}");

    let mut worded = overloaded();
    for r in &mut worded.ledger {
        if r.rule == RULE_OUTAGE_RESUME {
            r.command = "The API was overloaded.".to_string();
        }
    }
    let (w, why) = verdict(Form::Overload, &worded, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("is not the cleared line"), "{why}");

    let mut none = overloaded();
    none.journal
        .retain(|(_, l)| !l.contains(&format!("rule={RULE_OUTAGE_RESUME}")));
    none.ledger.retain(|r| r.rule != RULE_OUTAGE_RESUME);
    let (w, why) = verdict(Form::Overload, &none, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("NO RESUME: Cruise's continue-done-check@v1 typed at +613.0 s first"),
        "{why}"
    );
}

#[test]
fn a_need_raised_on_an_outage_fails() {
    let mut s = overloaded();
    s.journal.push(j(
        MET + 70_000,
        "NEEDS seq=2 the API overloaded: retry when it is back",
    ));
    s.journal.sort_by_key(|(t, _)| *t);
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("a need was raised at +195.0 s"), "{why}");
}

#[test]
fn a_stand_in_never_met_or_never_cleared_is_unobserved() {
    let mut s = overloaded();
    s.cover.outage_lifted_ms = None;
    assert_eq!(verdict(Form::Overload, &s, "claude").0, "UNOBSERVED");
    let mut s = overloaded();
    s.cover.outage_ms = None;
    assert_eq!(verdict(Form::Overload, &s, "claude").0, "UNOBSERVED");
}

#[test]
fn the_stand_in_stands_past_the_first_rung_and_cruises_back_off_from_the_walls_reading() {
    let t = turn_end::TurnEndTiming::default();
    let ms = |d: std::time::Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    let first = ms(t.outage_first);
    // Past the first rung and Cruise's short-turn back-off (4 min), so a plain point's act lands
    // inside it; short of the second rung from the first try's own wall.
    assert!(first + 20_000 < STANDS_MS, "{STANDS_MS} {first}");
    const { assert!(4 * 60_000 + 20_000 < STANDS_MS) };
    assert!(
        STANDS_MS + 20_000 < first + 2 * first,
        "{STANDS_MS} {first}"
    );
    let s = overloaded();
    assert_eq!(sysmsg::wall_read(&s), Some(T0 + WALL));
    assert_eq!(sysmsg::lift_due(&s), Some(T0 + CLEAR));
    assert_eq!(sysmsg::lift_due(&Seen::default()), None);
    // THE CONTROL: no point read past the act (the program's own tries still running), no
    // clearing due, however long since the act.
    let mut trying = overloaded();
    trying
        .journal
        .retain(|(t, l)| *t <= T0 + MET || !l.starts_with("EVENT "));
    assert_eq!(sysmsg::lift_due(&trying), None);
}

#[test]
fn the_watch_ends_once_cruise_went_on_past_the_resume() {
    let s = overloaded();
    let went_on = T0 + RESUMED + 125_000;
    assert!(!sysmsg::watched(&s, false, went_on + 1_000));
    assert!(sysmsg::watched(&s, false, went_on + 20_000));
    let mut quiet = overloaded();
    quiet.journal.retain(|(t, _)| *t <= T0 + RESUMED);
    assert!(!sysmsg::watched(&quiet, false, T0 + RESUMED + 60_000));
    assert!(sysmsg::watched(&quiet, false, T0 + RESUMED + 120_000));
    let mut unlifted = overloaded();
    unlifted.cover.outage_lifted_ms = None;
    assert!(!sysmsg::watched(&unlifted, false, went_on + 60_000));
}

#[test]
fn each_form_is_one_chain_of_its_own_under_cruise_and_land_each_on_a_lab_of_its_own() {
    let land: Vec<Chain> = Form::LAND.iter().filter_map(|f| f.land_chain()).collect();
    assert_eq!(
        land.len(),
        Form::LAND.len(),
        "every Land form has its chain"
    );
    let both: Vec<Chain> = Form::ALL.map(Form::chain).into_iter().chain(land).collect();
    assert_eq!(both, Chain::SYSMSG.to_vec());
    assert_eq!(Chain::of_mode(Mode::Sysmsg), Chain::SYSMSG.to_vec());
    assert_eq!(Mode::of("sysmsg"), Some(Mode::Sysmsg));
    assert_eq!(Mode::Sysmsg.walled(), None);
    for f in Form::ALL {
        let c = f.chain();
        assert_eq!(Form::of(c), Some(f));
        assert_eq!(c.scenarios().first(), Some(&Scenario::Sys(f)));
        assert_eq!(c.autopilot(), "cruise");
        assert_eq!(
            c.raise_gate(),
            None,
            "Cruise's own act meets it at its point"
        );
        assert!(c.sysmsg() && !c.walled() && !c.outage(), "{c:?}");
        assert!(Chain::every().contains(&c), "{c:?}");
        assert!(crate::stub::not_custom(f.stub_mode()).is_some(), "{f:?}");
        assert_eq!(c.login(), f == Form::Login, "{c:?}");
    }
    for f in Form::LAND {
        let c = f.land_chain().expect("a Land chain");
        assert_eq!(Form::of(c), Some(f));
        assert_eq!(c.scenarios().first(), Some(&Scenario::SysLand(f)));
        assert_eq!(c.autopilot(), "land");
        assert!(c.sysmsg() && !c.walled() && !c.outage(), "{c:?}");
        assert!(Chain::every().contains(&c), "{c:?}");
        // Land acts on the first prompt's answer at once: the stub holds that answer on the
        // chain's gate, which the lab opens once the stand-in stands.
        let gate = c.raise_gate().expect("a raise gate");
        let gated: Vec<J> = c
            .rules()
            .into_iter()
            .filter(|r| r.get("gate").and_then(J::as_str) == Some(gate.as_str()))
            .collect();
        assert_eq!(gated.len(), 1, "{c:?}: one gated rule");
        assert_eq!(gated[0].get("last").and_then(J::as_str), Some(""), "{c:?}");
        assert_eq!(c.login(), f == Form::Login, "{c:?}");
    }
    assert_eq!(
        Form::Witness.land_chain(),
        None,
        "the witness's second tab is Cruise's"
    );
    assert_eq!(
        Chain::SysLogin.scenarios(),
        &[Scenario::Sys(Form::Login), Scenario::SignedIn(false)]
    );
    assert_eq!(
        Chain::SysLandLogin.scenarios(),
        &[Scenario::SysLand(Form::Login), Scenario::SignedIn(true)]
    );
    assert_eq!(Form::of(Chain::CruiseOutage), None, "the control");
    assert!(!Chain::CruiseOutage.sysmsg());
    let ids: Vec<&str> = Form::ALL.iter().map(|f| f.id()).collect();
    assert_eq!(ids, ["OVL", "5XX", "429", "NET", "WIT", "LOG"]);
    let land_ids: Vec<&str> = Form::LAND.iter().map(|f| f.land_id()).collect();
    assert_eq!(land_ids, ["LOVL", "L5XX", "L429", "LNET", "LLOG"]);
    assert_eq!(
        [Scenario::SignedIn(false), Scenario::SignedIn(true)].map(Scenario::id),
        ["SGN", "LSGN"]
    );
    // Each scenario id names one scenario across every chain, so a report row is never two.
    let mut every: Vec<Scenario> = Chain::every()
        .iter()
        .flat_map(|c| c.scenarios().iter().copied())
        .collect();
    every.dedup();
    for s in &every {
        assert!(
            every.iter().all(|o| o == s || o.id() != s.id()),
            "{s:?}'s id {} is another's",
            s.id()
        );
    }
    // The forms the stub cannot play are notes with ids of their own, on no chain and in no
    // verdict, so no run's exit counts them.
    let unplayed = sysmsg::Unplayed::ALL.map(sysmsg::Unplayed::id);
    assert_eq!(unplayed, ["REF", "DNS"]);
    assert!(
        Chain::every()
            .iter()
            .flat_map(|c| c.scenarios())
            .all(|s| !unplayed.contains(&s.id())),
        "{unplayed:?}"
    );
    for h in Harness::ALL {
        assert_eq!(Chain::of_mode_on(Mode::Sysmsg, h), Chain::SYSMSG, "{h:?}");
    }
    // The witness lab's second tab rides with its first, never on a lab of its own.
    let mate = Chain::SysWitnessMate;
    assert_eq!(Form::of(mate), Some(Form::Witness));
    assert!(!Chain::SYSMSG.contains(&mate) && Chain::every().contains(&mate));
    assert_eq!(mate.scenarios(), &[Scenario::Witnessed]);
    assert_eq!(mate.opening(), Opening::Idle);
    assert_eq!(mate.autopilot(), "cruise");
}

#[test]
fn every_form_is_in_reach_and_each_agent_retries_little() {
    for h in Harness::ALL {
        for f in Form::ALL {
            assert_eq!(f.out_of_reach(h), None, "{f:?} {h:?}");
        }
        let (envs, args) = sysmsg::vendor_retries(h);
        assert_eq!(envs.len() + args.len().min(1), 1, "{h:?}: one setting");
    }
    assert_eq!(
        Form::Overload.outage(Harness::Claude),
        Some(Outage::Overload)
    );
    assert_eq!(Form::Overload.outage(Harness::Agy), Some(Outage::Overload));
    assert_eq!(Form::Overload.outage(Harness::Muse), Some(Outage::ApiError));
    assert_eq!(Form::Drop.outage(Harness::Claude), Some(Outage::Connection));
    assert_eq!(Form::Rate.outage(Harness::Muse), Some(Outage::ApiError));
    for h in Harness::ALL {
        assert_eq!(
            Form::Login.outage(h),
            None,
            "a login is a person's, never waited out"
        );
    }
}

#[test]
fn the_wakes_while_the_wall_stood_are_the_metrics_fields_and_said() {
    let metrics = "frames=3 timer_wakes=2 event_wakes=9 deadline_arms_by_owner=presence:1/0 \
                   deadline_fires_by_owner=none wakes_by_variant=Vitals:1";
    let wakes = sysmsg::wakes(metrics);
    assert_eq!(
        wakes,
        "timer_wakes=2 deadline_arms_by_owner=presence:1/0 deadline_fires_by_owner=none \
         wakes_by_variant=Vitals:1"
    );
    assert!(sysmsg::wakes("").starts_with("timer_wakes=? "));
    let mut s = overloaded();
    s.cover.wall_wakes = Some(wakes);
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("; while it stood (421 s): timer_wakes=2 "),
        "{why}"
    );
    // The control: no reading, nothing said of it.
    let (_, why) = verdict(Form::Overload, &overloaded(), "claude");
    assert!(!why.contains("while it stood ("), "{why}");
}

/// The wait's wake budget (v6 §14.17.5): the harness's glass fires once at the meeting, once a
/// try and once at the clearing; more fails, and a figure the failed reset spoilt is unobserved.
#[test]
fn the_glass_waking_past_the_tries_fails_and_a_spoilt_figure_is_unjudged() {
    let fired = |n: u32| {
        format!(
            "timer_wakes=9 deadline_arms_by_owner=harness_acts:4/0 \
             deadline_fires_by_owner=title_drift:1,harness_acts:{n},system_vitals:1"
        )
    };
    assert_eq!(sysmsg::harness_acts_fires(&fired(3)), Some(3));
    assert_eq!(
        sysmsg::harness_acts_fires("deadline_fires_by_owner=none"),
        Some(0)
    );
    assert_eq!(sysmsg::harness_acts_fires("timer_wakes=1"), None);
    let mut s = overloaded();
    s.cover.wall_wakes = Some(fired(3));
    assert_eq!(verdict(Form::Overload, &s, "claude").0, "PASS");
    s.cover.wall_wakes = Some(fired(4));
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("WAKES PAST THE BACK-OFF: the harness's glass fired 4 times"),
        "{why}"
    );
    s.cover.wall_wakes_unreset = true;
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("UNOBSERVED (the metrics reset at the raise failed)"),
        "{why}"
    );
}

/// The page's want (v6 §14.17.5: an outage shows only as `waiting: <condition>` on the tab's
/// state line): a row that says the wait's words while the wall stands passes; one that says its
/// pause, or a need, fails.
#[test]
fn the_pages_state_line_says_the_wait_while_the_wall_stands() {
    let words = format!("waiting: {}", Outage::Overload.words());
    let page =
        |state: &str| format!("head=sys-overload: Window 2, tab 1 · Claude Code\nstate={state}");
    let with_page = |states: &[(i64, String)]| {
        let mut s = overloaded();
        s.window.read = true;
        s.window.label = "sys-overload".into();
        s.window.page = states.iter().map(|(t, p)| (T0 + t, p.clone())).collect();
        s.window.page_reads = states.iter().map(|(t, _)| T0 + t).collect();
        s
    };
    let pausing = page("pausing until 18:41 (1 short turn in a row)");
    let waits = with_page(&[
        (1_000, pausing.clone()),
        (MET + 2_500, page(&format!("{words} · next try 18:45"))),
        (RESUMED + 1_000, page("cruising")),
    ]);
    let (w, why) = verdict(Form::Overload, &waits, "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains(&format!("+127.5 s \"{words}")), "{why}");
    // The control: its pause stands in the wait's place.
    let paused = with_page(&[(1_000, pausing.clone()), (MET + 2_500, pausing)]);
    let (w, why) = verdict(Form::Overload, &paused, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("NEVER SAID WAITING"), "{why}");
    let needy = with_page(&[(MET + 2_500, page(&format!("{words} · needs you: the API")))]);
    let (w, why) = verdict(Form::Overload, &needy, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("a need on the state line"), "{why}");
    // A row never on the page is unread, and a run that read no window names the want a skip.
    let off = with_page(&[(1_000, "-".into()), (MET + 2_500, "-".into())]);
    assert_eq!(verdict(Form::Overload, &off, "claude").0, "UNOBSERVED");
    let (w, why) = verdict(Form::Overload, &overloaded(), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(why.contains("window wants SKIPPED"), "{why}");
}

/// The rate limit's stand-in names its `retry-after` (`rate-limit:150`,
/// [`sysmsg::RATE_AFTER_MS`]), past the ladder's first rung and short of the stand: in agy's and
/// Muse Code's words, whose tries' rungs are that wait first, then no shorter; in Claude Code's
/// header alone, whose tries are the ladder's.
#[test]
fn the_rate_limits_stand_in_names_its_retry_after_past_the_first_rung() {
    assert_eq!(Form::Rate.stub_mode(), "rate-limit:150");
    assert_eq!(
        crate::stub::rate_limit_after(Form::Rate.stub_mode()),
        Some("150")
    );
    assert_eq!(sysmsg::RATE_AFTER_MS, 150_000);
    let first = outage::rung_ms(1, 1.0);
    assert!(first < sysmsg::RATE_AFTER_MS && sysmsg::RATE_AFTER_MS < STANDS_MS);
    for h in [Harness::Agy, Harness::Muse] {
        assert!(Form::Rate.waits_named(h), "{h:?}");
        assert_eq!(Form::Rate.rung_ms(h, 1), sysmsg::RATE_AFTER_MS, "{h:?}");
        assert_eq!(Form::Rate.rung_ms(h, 2), outage::rung_ms(2, 1.0), "{h:?}");
        assert_eq!(
            Form::Overload.rung_ms(h, 1),
            first,
            "the control: the ladder's own"
        );
    }
    assert!(!Form::Rate.waits_named(Harness::Claude));
    assert_eq!(Form::Rate.rung_ms(Harness::Claude, 1), first);
}

/// [`overloaded`] as a rate limit naming its wait ([`sysmsg::RATE_AFTER_MS`]), its first try
/// `after` ms from the act that met it.
fn rate_limited(after: i64) -> Seen {
    let line = blind(Outage::ApiError);
    let tried = MET + after;
    let resumed = tried + 2_000 + 240_000;
    let mut s = overloaded();
    let moved = |t: i64| match t - T0 {
        TRY => T0 + tried,
        x if x == TRY + 2_000 => T0 + tried + 2_000,
        x if x == TRY + 2_100 => T0 + tried + 2_100,
        x if x >= RESUMED => T0 + x - RESUMED + resumed,
        _ => t,
    };
    for (t, l) in &mut s.journal {
        *t = moved(*t);
        *l = l
            .replace("Repeated 529 Overloaded errors", "Request rejected (429)")
            .replace("waiting: API overload", "waiting: API error")
            .replace(&blind(Outage::Overload), &line);
        if l.contains(&format!("until={}", stamp(TRY))) {
            *l = l.replace(&stamp(TRY), &stamp(tried));
        }
        if l.contains(&format!("until={}", stamp(RESUMED))) {
            *l = l.replace(&stamp(RESUMED), &stamp(resumed));
        }
    }
    for r in &mut s.ledger {
        r.ts = moved(r.ts);
        r.command = r.command.replace(&blind(Outage::Overload), &line);
    }
    s.status = vec![status(1_000, "-"), status(MET + 3_000, "api-error")];
    s
}

/// The rate limit's first try comes no sooner than the retry-after agy's and Muse Code's words
/// name, nor more than a late allowance past it; main's ladder, which tries on its first rung
/// (2 min), types into the standing wall before it. Claude Code's words name none: its first try
/// is the ladder's.
#[test]
fn a_rate_limit_tried_before_its_retry_after_or_late_fails() {
    for program in ["agy", "muse"] {
        let (w, why) = verdict(Form::Rate, &rate_limited(151_000), program);
        assert_eq!(w, "PASS", "{program}: {why}");
        assert!(
            why.contains("its first try 151.0 s after the act that met it (the retry-after its words name: 150.0 s)"),
            "{program}: {why}"
        );
        let (w, why) = verdict(Form::Rate, &rate_limited(121_000), program);
        assert_eq!(w, "FAIL", "{program}: {why}");
        assert!(
            why.contains(&format!(
                "TYPED INTO THE STANDING WALL: {RULE_OUTAGE_RESUME} at +246.0 s, off its rung (150.0 s"
            )),
            "{program}: {why}"
        );
        let (w, why) = verdict(Form::Rate, &rate_limited(240_000), program);
        assert_eq!(w, "FAIL", "{program}: {why}");
        assert!(
            why.contains("LATE: try 1 at +365.0 s, 238.0 s past its wall's reading at +127.0 s, its rung 150.0 s"),
            "{program}: {why}"
        );
    }
    let (w, why) = verdict(Form::Rate, &rate_limited(121_000), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("1 try on the ladder's rungs") && !why.contains("its words name"),
        "{why}"
    );
}

/// Each try inside the standing wall is the outage's blind line (nothing measured its end), and a
/// wall that stood past its first rung and the late allowance saw a try.
#[test]
fn a_measured_try_inside_the_wall_or_no_try_on_its_rung_fails() {
    let measured = cleared_text(Cleared::Outage(Outage::Overload), Clearing::Measured);
    let mut s = overloaded();
    for (t, l) in &mut s.journal {
        if *t == T0 + TRY {
            *l = l.replace(&blind(Outage::Overload), measured);
        }
    }
    for r in &mut s.ledger {
        if r.ts == T0 + TRY {
            r.command = measured.to_string();
        }
    }
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("NOT BLIND INSIDE THE STANDING WALL: try 1 at +246.0 s"),
        "{why}"
    );
    let mut s = overloaded();
    s.journal.retain(|(t, _)| *t != T0 + TRY);
    s.ledger.retain(|r| r.ts != T0 + TRY);
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("NO TRY ON ITS RUNG: the wall read at +127.0 s stood 300.0 s more, and try 1 on its rung 120.0 s never came"),
        "{why}"
    );
}

/// The witness lab's second tab: prompted into the standing wall [`sysmsg::MATE_AFTER_MS`] past
/// the first tab's wall, tried once on its rung, and resumed past the clearing at `resumed` in
/// `line`, a witness journaled first where `noted`.
fn mate(resumed: i64, line: &str, noted: bool) -> Seen {
    let prompted = WALL + sysmsg::MATE_AFTER_MS;
    let met = prompted + 1_000;
    let tried = met + 120_000;
    let blind_line = blind(Outage::Overload);
    let mut journal = vec![
        j(
            met,
            "EVENT idle seq=1 API Error: Repeated 529 Overloaded errors",
        ),
        j(
            met + 100,
            &format!("WAITING seq=1 until={} waiting: API overload", stamp(tried)),
        ),
        j(
            tried,
            &format!("CONTINUED seq=1 rule={RULE_OUTAGE_RESUME} {blind_line}"),
        ),
        j(
            tried + 2_000,
            "EVENT idle seq=2 API Error: Repeated 529 Overloaded errors",
        ),
        j(
            tried + 2_100,
            &format!(
                "WAITING seq=2 until={} waiting: API overload",
                stamp(tried + 242_000)
            ),
        ),
        j(
            resumed,
            &format!("CONTINUED seq=2 rule={RULE_OUTAGE_RESUME} {line}"),
        ),
        j(resumed + 5_000, &format!("EVENT idle seq=3 {DONE_CLAIM}")),
    ];
    if noted {
        journal.push(j(
            resumed,
            &aterm_agent::harness::witness::witness_note(2, RULE_OUTAGE_RESUME, "04:31:07.000"),
        ));
    }
    journal.sort_by_key(|(t, _)| *t);
    Seen {
        journal,
        ledger: vec![
            typed(tried, RULE_OUTAGE_RESUME, &blind_line),
            typed(resumed, RULE_OUTAGE_RESUME, line),
            point(resumed + 5_000, 1),
        ],
        status: vec![status(1_000, "-"), status(met + 3_000, "overload")],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + resumed + 150_000,
        cover: Cover {
            outage_ms: Some(T0 + 6_000),
            outage_lifted_ms: Some(T0 + CLEAR),
            mate_ms: Some(T0 + prompted),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// THE SECOND TAB RESUMES ON THE WITNESS (v6 §14.17.5.4): its first act past the clearing is the
/// measured line, a witness journaled, well before its own next rung. Main's run (no witness: the
/// blind line on that rung), a resume with no witness journaled, and a blind try after it fail.
#[test]
fn the_second_tab_resumes_on_the_witness_and_tries_nothing_blind_past_the_clearing() {
    let measured = cleared_text(Cleared::Outage(Outage::Overload), Clearing::Measured);
    let witnessed = |s: &Seen| one(Chain::SysWitnessMate, s, "claude", Scenario::Witnessed);
    let (w, why) = witnessed(&mate(CLEAR + 70_000, measured, true));
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("1 try on the ladder's rungs while it stood")
            && why.contains("resumed at +497.0 s, 70.0 s past the clearing")
            && why.contains("witness: another session's turn on this route"),
        "{why}"
    );
    // Main: no witness, the blind line on the second rung.
    let rung = WALL + sysmsg::MATE_AFTER_MS + 1_000 + 120_000 + 242_000;
    let (w, why) = witnessed(&mate(rung, &blind(Outage::Overload), false));
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("NOT ON A WITNESS") && why.contains("BLIND PAST THE CLEARING"),
        "{why}"
    );
    let (w, why) = witnessed(&mate(CLEAR + 70_000, measured, false));
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("NO WITNESS JOURNALED"), "{why}");
    let mut after = mate(CLEAR + 70_000, measured, true);
    let late = CLEAR + 200_000;
    after.journal.push(j(
        late,
        &format!(
            "CONTINUED seq=3 rule={RULE_OUTAGE_RESUME} {}",
            blind(Outage::Overload)
        ),
    ));
    after.journal.sort_by_key(|(t, _)| *t);
    after
        .ledger
        .push(typed(late, RULE_OUTAGE_RESUME, &blind(Outage::Overload)));
    let (w, why) = witnessed(&after);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("BLIND PAST THE CLEARING") && why.contains("2 resumes past the clearing"),
        "{why}"
    );
    let mut unprompted = mate(CLEAR + 70_000, measured, true);
    unprompted.cover.mate_ms = None;
    assert_eq!(witnessed(&unprompted).0, "UNOBSERVED");
}

/// THE LAND RUN (v6 §14.17.5.6) on Claude Code's 529: Land's `land@v1` on the first prompt's
/// answer meets the wall; inside it only the outage's try on its rung; past the clearing one
/// resume, whose answer is a clean defense; Land's check, `LANDED`, landed.
fn overloaded_land() -> Seen {
    let line = blind(Outage::Overload);
    let mut s = overloaded();
    s.journal = vec![
        j(5_000, &format!("EVENT idle seq=1 {WORK}")),
        j(
            MET,
            "TYPED seq=1 rule=land@v1 [aterm harness] Autopilot is Land: land",
        ),
        j(
            WALL,
            "EVENT idle seq=2 API Error: Repeated 529 Overloaded errors",
        ),
        j(
            WALL + 100,
            &format!("WAITING seq=2 until={} waiting: API overload", stamp(TRY)),
        ),
        j(
            TRY,
            &format!("CONTINUED seq=2 rule={RULE_OUTAGE_RESUME} {line}"),
        ),
        j(
            TRY + 2_000,
            "EVENT idle seq=3 API Error: Repeated 529 Overloaded errors",
        ),
        j(
            TRY + 2_100,
            &format!(
                "WAITING seq=3 until={} waiting: API overload",
                stamp(RESUMED)
            ),
        ),
        j(
            RESUMED,
            &format!("CONTINUED seq=3 rule={RULE_OUTAGE_RESUME} {line}"),
        ),
        j(RESUMED + 5_000, &format!("EVENT idle seq=4 {CLEAN}")),
        j(
            RESUMED + 5_200,
            "CONTINUED seq=4 rule=land-check@v1 [aterm harness] Land check: Reread",
        ),
        j(RESUMED + 9_000, "EVENT idle seq=5 LANDED"),
        j(
            RESUMED + 9_000,
            "DONE seq=5 rule=land@v1 the worker answered the land check: LANDED",
        ),
    ];
    s.ledger = vec![
        point(5_000, 1),
        typed(MET, "land@v1", LAND),
        typed(TRY, RULE_OUTAGE_RESUME, &line),
        typed(RESUMED, RULE_OUTAGE_RESUME, &line),
        point(RESUMED + 5_000, 1),
        typed(RESUMED + 5_200, RULE_LAND_CHECK, &acked_check()),
        point(RESUMED + 9_000, 2),
        task_done(RESUMED + 9_000),
    ];
    s.status = vec![
        status(1_000, "-"),
        status(MET + 3_000, "overload"),
        j(
            RESUMED + 10_000,
            "task=landed needs=- autopilot=land supervisor=aterm-harness@1 agent=- waiting=-",
        ),
    ];
    s.end_ms = T0 + RESUMED + 40_000;
    s
}

fn land_verdict(form: Form, seen: &Seen, program: &str) -> (&'static str, String) {
    one(
        form.land_chain().expect("a Land chain"),
        seen,
        program,
        Scenario::SysLand(form),
    )
}

#[test]
fn under_land_a_wait_waited_out_resumed_once_and_landed_passes() {
    let (w, why) = land_verdict(Form::Overload, &overloaded_land(), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("Land's land@v1 typed at +125.0 s met it")
            && why.contains("1 try on the ladder's rungs while it stood")
            && why.contains(&format!("one resume, {RULE_OUTAGE_RESUME} at +488.0 s"))
            && why.contains("Land carried on (land-check@v1 at +493.2 s) and landed at +497.0 s"),
        "{why}"
    );
}

/// Main's rule under Land (bin-w4's): the wall a plain point, Land's nudge typed into it.
#[test]
fn under_land_an_act_of_lands_inside_the_wall_fails() {
    for (rule, text) in [
        ("land-nudge@v1", "[aterm harness] Still landing: x"),
        ("land@v1", "[aterm harness] Autopilot is Land: land"),
        ("land-check@v1", "[aterm harness] Land check: Reread"),
    ] {
        let mut s = overloaded_land();
        let into = WALL + 30_000;
        s.journal
            .push(j(into, &format!("CONTINUED seq=2 rule={rule} {text}")));
        s.journal.sort_by_key(|(t, _)| *t);
        s.ledger.push(typed(into, rule, text));
        let (w, why) = land_verdict(Form::Overload, &s, "claude");
        assert_eq!(w, "FAIL", "{why}");
        assert!(
            why.contains(&format!("TYPED INTO THE STANDING WALL: {rule} at +157.0 s")),
            "{why}"
        );
    }
}

#[test]
fn under_land_no_landing_past_the_resume_or_another_act_on_the_way_fails() {
    // Land never carried on: nothing past the resume's answer, watched past the bound.
    let mut s = overloaded_land();
    s.journal.retain(|(t, _)| *t <= T0 + RESUMED + 5_000);
    s.ledger.retain(|r| r.ts <= T0 + RESUMED + 5_000);
    s.end_ms = T0 + RESUMED + sysmsg::LAND_ON_MS + 1_000;
    let (w, why) = land_verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("LAND DID NOT CARRY ON"), "{why}");
    // The same records watched short of the bound: not yet known.
    s.end_ms = T0 + RESUMED + 60_000;
    assert_eq!(land_verdict(Form::Overload, &s, "claude").0, "UNOBSERVED");
    // An act that is not Land's between the resume and the landing.
    let mut s = overloaded_land();
    s.journal.push(j(
        RESUMED + 7_000,
        "CONTINUED seq=4 rule=continue@v1 [aterm harness] keep going",
    ));
    s.journal.sort_by_key(|(t, _)| *t);
    let (w, why) = land_verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("NOT LAND'S: continue@v1 typed at +495.0 s"),
        "{why}"
    );
    // A hold parks the session where the stub answered every act of Land's clean: no carrying
    // on, however soon it came.
    let mut s = overloaded_land();
    s.journal.retain(|(t, _)| *t <= T0 + RESUMED + 5_200);
    s.journal.push(j(
        RESUMED + 9_000,
        "EXCEPTION seq=5 kind=stalled held=same-reply the same answer twice",
    ));
    let (w, why) = land_verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("HELD, NOT LANDED"), "{why}");
}

#[test]
fn under_land_the_watch_ends_past_the_landing_or_the_bound() {
    let s = overloaded_land();
    let done = T0 + RESUMED + 9_000;
    assert!(!sysmsg::watched(&s, true, done + 1_000));
    assert!(sysmsg::watched(&s, true, done + 20_000));
    let mut quiet = overloaded_land();
    quiet.journal.retain(|(t, _)| *t <= T0 + RESUMED + 5_000);
    assert!(!sysmsg::watched(&quiet, true, T0 + RESUMED + 120_000));
    assert!(sysmsg::watched(
        &quiet,
        true,
        T0 + RESUMED + sysmsg::LAND_ON_MS
    ));
    // Cruise's watch over the same records ends at its own bound, on no landing.
    assert!(sysmsg::watched(&quiet, false, T0 + RESUMED + 120_000));
}

/// THE STREAK STANDS AT THE AGENT'S OWN SHORT ANSWERS past an outage (v6 §14.17.5.7): the act the
/// wall cut and the wall's tries and resume add nothing, so no point and no back-off past the
/// resume names more short turns in a row than the point the act that met the wall was typed
/// on. A build that counts the cut act (main's before the slice: two past the resume of the one
/// before the wall, a 4 min back-off) fails under Cruise and Land; the control, two short turns
/// the agent's own answers earned before the wall, passes with the same back-off; with no point
/// before the act or none past the resume the streak is unread.
#[test]
fn past_an_outages_resume_a_streak_the_cut_act_lengthened_fails() {
    let (w, why) = verdict(Form::Overload, &overloaded(), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains(
            "the streak kept: 1 short turn(s) in a row past the resume, of the 1 before the act \
             the wall cut"
        ),
        "{why}"
    );
    let step = |n: u32| {
        let d = turn_end::TurnEndTiming::default()
            .short_wait_at(n)
            .expect("a back-off");
        i64::try_from(d.as_millis()).expect("ms")
    };
    // `before` short turns by the point the act that met the wall was typed on, `past` at the
    // resume's answer, its back-off the ladder's, and Cruise's act 2 s past its end.
    let counted = |before: u32, past: u32| {
        let mut s = overloaded();
        let went = RESUMED + 5_100 + step(past) + 2_000;
        for (t, l) in &mut s.journal {
            if *t == T0 + RESUMED + 5_100 {
                (*t, *l) = backoff(RESUMED + 5_100, 4, past, step(past));
            } else if *t == T0 + RESUMED + 125_000 {
                *t = T0 + went;
            }
        }
        for r in &mut s.ledger {
            if r.ts == T0 + RESUMED + 5_000 {
                *r = point(RESUMED + 5_000, past);
            } else if r.ts == T0 + RESUMED + 125_000 {
                r.ts = T0 + went;
            }
        }
        if before > 1 {
            s.ledger.push(point(MET - 1_000, before));
            s.ledger.sort_by_key(|r| r.ts);
        }
        s.end_ms = T0 + went + 25_000;
        s
    };
    let (w, why) = verdict(Form::Overload, &counted(1, 2), "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains(
            "COUNTED A TURN THAT WAS NOT THE AGENT'S ANSWER: at +493.0 s a point names 2 short \
             turns in a row past the resume (+488.0 s), of the 1 the point at +5.0 s named before \
             the act the wall cut (+125.0 s)"
        ),
        "{why}"
    );
    // The back-off's line alone names it, the point record unread.
    let mut s = counted(1, 2);
    s.ledger.retain(|r| r.ts != T0 + RESUMED + 5_000);
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("at +493.1 s a back-off names 2 short turns in a row"),
        "{why}"
    );
    // The control: the agent's own two by the act that met the wall stand past it.
    let (w, why) = verdict(Form::Overload, &counted(2, 2), "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("the streak kept: 2 short turn(s) in a row past the resume, of the 2"),
        "{why}"
    );
    // Under Land, the cut land act counted fails as well.
    let mut s = overloaded_land();
    for r in &mut s.ledger {
        if r.ts == T0 + RESUMED + 5_000 {
            *r = point(RESUMED + 5_000, 2);
        }
    }
    let (w, why) = land_verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("COUNTED A TURN THAT WAS NOT THE AGENT'S ANSWER"),
        "{why}"
    );
    // No point before the act that met the wall, or none past the resume: unread.
    let mut s = overloaded();
    s.ledger.retain(|r| r.ts != T0 + 5_000);
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("the streak it was typed on is unread"),
        "{why}"
    );
    let mut s = overloaded();
    s.ledger.retain(|r| r.ts != T0 + RESUMED + 5_000);
    s.journal.retain(|(t, _)| *t != T0 + RESUMED + 5_100);
    let (w, why) = verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("no point judged past the resume"), "{why}");
}

/// A STREAK A WALL RESET FAILS, and A WANT LEFT UNREAD HIDES NO OTHER'S FAULT (v6 §14.17.5.7). Past
/// the resume a point naming none of the one short turn the agent's own answer earned before the
/// wall fails, under Cruise and Land: a build that resets the streak at a wall leaves a wall that
/// cuts every act prompting at one pace. And Land's run that stopped at the resume (no point
/// journaled past it, so the streak there is unread, and no landing within the bound) fails on
/// Land not carrying on, rather than going unobserved on the unread streak. NEGATIVE CONTROLS: the
/// one-sided want passes the reset streak; deciding the unread streak at once, before the other
/// wants, makes the stopped run UNOBSERVED.
#[test]
fn past_an_outages_resume_a_reset_streak_fails_and_an_unread_one_hides_no_fault() {
    for land in [false, true] {
        let mut s = if land {
            overloaded_land()
        } else {
            overloaded()
        };
        for r in &mut s.ledger {
            if r.ts == T0 + RESUMED + 5_000 {
                *r = point(RESUMED + 5_000, 0);
            }
        }
        let (w, why) = if land {
            land_verdict(Form::Overload, &s, "claude")
        } else {
            verdict(Form::Overload, &s, "claude")
        };
        assert_eq!(w, "FAIL", "land={land}: {why}");
        assert!(
            why.contains(
                "DROPPED THE AGENT'S OWN SHORT ANSWERS: at +493.0 s a point names 0 short turns \
                 in a row past the resume (+488.0 s), of the 1"
            ),
            "land={land}: {why}"
        );
    }
    let mut s = overloaded_land();
    s.journal.retain(|(t, _)| *t <= T0 + RESUMED);
    s.ledger.retain(|r| r.ts <= T0 + RESUMED);
    s.end_ms = T0 + RESUMED + sysmsg::LAND_ON_MS + 1_000;
    let (w, why) = land_verdict(Form::Overload, &s, "claude");
    assert_eq!(w, "FAIL", "{why}");
    assert!(why.contains("LAND DID NOT CARRY ON"), "{why}");
    assert!(why.contains("no point judged past the resume"), "{why}");
}
