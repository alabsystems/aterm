// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One test per rule of [`decide_turn_end`], each with its negative control.

use super::*;
use aterm_phase::prompt::fixtures::{
    END_529, END_OFFER, END_SESSION_LIMIT, GOAL_ACTIVE_SUGGESTION, screen,
};

const MIN: Duration = Duration::from_secs(60);

fn cfg() -> SupervisorConfig {
    SupervisorConfig::default()
}

/// The owner's `answer_questions = false`: a question is escalated.
fn no_answers() -> SupervisorConfig {
    SupervisorConfig {
        answer_questions: false,
        ..cfg()
    }
}

/// `observe`, then `decide` under `cfg`.
fn at_under(
    st: &mut TurnEndState,
    r: &TurnEndReading,
    now: Instant,
    cfg: &SupervisorConfig,
) -> TurnEndAction {
    st.observe(r, now);
    decide_turn_end(st, r, cfg, now)
}

fn answered() -> TurnEndAction {
    TurnEndAction::Type {
        text: cfg().answer_text,
        rule_id: RULE_ANSWER,
    }
}

/// The reversible-only answer a decision naming an irreversible act gets
/// (D1).
fn answered_reversibly() -> TurnEndAction {
    TurnEndAction::Type {
        text: REVERSIBLE_ANSWER.to_string(),
        rule_id: RULE_ANSWER,
    }
}

/// A wait until `until`.
fn waits_until(a: &TurnEndAction, until: Instant) -> bool {
    matches!(a, TurnEndAction::WaitUntil { until: u, .. } if *u == until)
}

/// A base clock far enough from the process start that `now - d` never
/// underflows.
fn t0() -> Instant {
    Instant::now() + Duration::from_secs(10 * 3600)
}

/// An idle point that ended a turn of `worked` busy work, the worker's last
/// words `said`.
fn idle(said: &str, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        phase: if said.trim_end().ends_with('?') {
            Phase::Question
        } else {
            Phase::Idle
        },
        authoritative: true,
        survey: false,
        wall: None,
        wall_message: String::new(),
        reset_at: None,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some(said.to_string()),
        worked,
        rules: None,
        pending_input: false,
        interrupted: false,
        // The window's host: a restart the policy asks for can be made.
        restartable: true,
        upgrading: false,
        taskless: false,
        person: None,
        login_back: false,
    }
}

fn walled(kind: WallKind, message: &str, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        wall: Some(kind),
        wall_message: message.to_string(),
        ..idle("Suites are running.", worked)
    }
}

/// `observe`, then `decide`.
fn at(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> TurnEndAction {
    st.observe(r, now);
    decide_turn_end(st, r, &cfg(), now)
}

/// `at`, and the act recorded as typed — a restart as made.
fn act(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> TurnEndAction {
    let a = at(st, r, now);
    if let TurnEndAction::Restart { why, .. } = &a {
        st.restarted(why, true, r, now);
    }
    st.acted(&a, r, now);
    a
}

/// A restart for `why` under `rule`, whatever it falls back to.
fn restarts(a: &TurnEndAction, want: &Restart, rule: &str) -> bool {
    matches!(a, TurnEndAction::Restart { why, rule_id, .. } if why == want && *rule_id == rule)
}

fn typed(rule: &'static str) -> TurnEndAction {
    TurnEndAction::Type {
        text: "keep going".to_string(),
        rule_id: rule,
    }
}

/// Nothing typed: the act's point waited for, to its deadline
/// ([`TurnEndTiming::take_within`] from `act_or_first_unseen`).
fn awaits(a: &TurnEndAction, act_or_first_unseen: Instant) -> bool {
    matches!(a, TurnEndAction::WaitUntil { until, why }
        if *until == act_or_first_unseen + TurnEndTiming::default().take_within
            && why.starts_with("the worker to take"))
}

fn is_escalate(a: &TurnEndAction, needle: &str) -> bool {
    matches!(a, TurnEndAction::Escalate { reason } if reason.contains(needle))
}

/// A turn of real work is continued at once; a SHORT one (under
/// `min_work`, or the first point seen, its work unknown) after the first
/// back-off — never left waiting for a person, as it was until 2026-09-24.
/// Negative control: the short point before its back-off types nothing.
#[test]
fn a_turn_end_after_real_work_is_continued_at_once_and_a_short_one_after_its_back_off() {
    let now = t0();
    let mut st = TurnEndState::default();
    assert_eq!(
        at(&mut st, &idle("Fixed the parser.", Some(3 * MIN)), now),
        typed(RULE_CONTINUE)
    );
    for worked in [Some(30 * Duration::from_secs(1)), None] {
        let mut st = TurnEndState::default();
        let r = idle("Fixed the parser.", worked);
        assert!(
            waits_until(&at(&mut st, &r, now), now + 2 * MIN),
            "{worked:?}"
        );
        let same = TurnEndReading {
            worked: None,
            ..r.clone()
        };
        assert!(
            waits_until(&at(&mut st, &same, now + MIN), now + 2 * MIN),
            "{worked:?}: the back-off counts from the point, not from each read"
        );
        assert_eq!(at(&mut st, &same, now + 2 * MIN), typed(RULE_CONTINUE));
    }
}

#[test]
fn the_workers_suggestion_is_accepted_once() {
    let now = t0();
    let mut st = TurnEndState::default();
    let mut r = idle("Done with stage 1.", Some(3 * MIN));
    r.composer = Composer::Placeholder("keep going".to_string());
    let a = act(&mut st, &r, now);
    assert_eq!(
        a,
        TurnEndAction::Accept {
            text: "keep going".to_string(),
            rule_id: RULE_SUGGESTION
        }
    );
    // The same point decided again before the next point is seen (the loop
    // observes a point once, when it is new): nothing more is typed — the
    // act's point is waited for, to its deadline.
    let again = TurnEndReading {
        worked: None,
        ..r.clone()
    };
    assert!(awaits(&decide_turn_end(&st, &again, &cfg(), now), now));
    // A suggestion that is not a continuation is not accepted: the
    // continuation text is typed over it.
    let mut st = TurnEndState::default();
    r.composer = Composer::Placeholder("delete the branch".to_string());
    assert_eq!(at(&mut st, &r, now), typed(RULE_CONTINUE));
}

#[test]
fn the_allow_list_takes_close_variants_and_nothing_else() {
    for ok in [
        "keep going",
        "Keep going.",
        "continue",
        "keep going, push it when green",
        "keep going and push it when green",
        "Keep fixing forward!",
    ] {
        assert!(suggestion_allowed(ok), "{ok}");
    }
    for no in [
        "",
        "keep going and delete the branch",
        "yes, drop the table",
        "push it",
        "continue with option B",
    ] {
        assert!(!suggestion_allowed(no), "{no}");
    }
}

/// A worker that asks a person — a stop phrase, a choice, a question — is
/// ANSWERED with `answer_text` (decide yourself, keep going); an offer is
/// continued. NEGATIVE CONTROL: under `answer_questions = false` each ask is
/// escalated, and the offers are continued all the same.
#[test]
fn a_question_is_answered_an_offer_continues_and_no_answers_escalates() {
    let now = t0();
    for said in [
        "I need your decision on the schema before going on.",
        "Blocked on the signing key; waiting on you.",
        "Should I rewrite the parser or patch the lexer?",
        "Two ways forward:\n1. rewrite the parser\n2. patch the lexer\nWhich one?",
        "Did the suite pass on your machine?",
    ] {
        let mut st = TurnEndState::default();
        assert_eq!(
            at(&mut st, &idle(said, Some(3 * MIN)), now),
            answered(),
            "{said}"
        );
        let mut st = TurnEndState::default();
        let a = at_under(&mut st, &idle(said, Some(3 * MIN)), now, &no_answers());
        assert!(is_escalate(&a, "answer_questions is off"), "{said}: {a:?}");
    }
    for said in [
        "Done: 12 of 67 solve. Next: the binder path; want me to take that on?",
        "Shall I carry on with stage 2?",
        "Two ways forward:\n1. rewrite the parser\n2. patch the lexer\nI recommend 1; shall I start?",
        "Next steps: wire the lane into the host.",
        "Created a.txt with one line. Should I also create b.txt?",
    ] {
        for c in [cfg(), no_answers()] {
            let mut st = TurnEndState::default();
            assert_eq!(
                at_under(&mut st, &idle(said, Some(3 * MIN)), now, &c),
                typed(RULE_CONTINUE),
                "{said}"
            );
        }
    }
}

/// `continue_per_hour = 6` (a cap the owner wrote) stops the seventh
/// continuation within the hour. Negative control: the default, `0`, is no
/// cap — the seventh goes.
#[test]
fn a_written_budget_stops_at_six_an_hour_and_the_default_has_none() {
    let capped = SupervisorConfig {
        continue_per_hour: 6,
        ..cfg()
    };
    let point = idle("Stage done.", Some(3 * MIN));
    let mut now = t0();
    let mut st = TurnEndState::default();
    for i in 0..6 {
        st.observe(&point, now);
        let a = decide_turn_end(&st, &point, &capped, now);
        assert_eq!(a, typed(RULE_CONTINUE), "continuation {i}");
        st.acted(&a, &point, now);
        now += 5 * MIN;
    }
    st.observe(&point, now);
    let a = decide_turn_end(&st, &point, &capped, now);
    assert!(is_escalate(&a, "budget spent: 6"), "{a:?}");
    assert_eq!(
        decide_turn_end(&st, &point, &cfg(), now),
        typed(RULE_CONTINUE),
        "no cap by default"
    );
    // The window slides: an hour after the first, one more may go.
    now = t0() + 61 * MIN;
    assert_eq!(
        decide_turn_end(&st, &point, &capped, now),
        typed(RULE_CONTINUE)
    );
}

/// N1 OF THE LIVE E2E OF 2026-09-26: the harness's OWN turns are no work of
/// the worker's. After a stage of real work (2m34s), the upgrade's notice
/// was answered READY in 2.8 s and, once relaunched, its carry-on in 2.7 s:
/// read as someone else's turns, the two were a short streak of two, and the
/// stage's continuation waited a 4-minute back-off. Each is a turn the host
/// typed ([`TurnEndState::host_typed`]): the streak stands as the stage left
/// it, and the point after the carry-on's reply is continued at once — the
/// act waited for until then (a point with no busy read after the typed
/// turn is its answer's, not the worker's). NEGATIVE CONTROL: two genuinely
/// short turns of the worker's still back off, 4 minutes after the second.
#[test]
fn the_harness_own_turns_are_no_short_turns_of_the_workers() {
    let mut now = t0();
    let mut st = TurnEndState::default();
    let stage = idle("Stage 1 complete.", Some(154 * Duration::from_secs(1)));
    // The stage's end: the upgrade's (no act here), its streak none.
    st.observe(&stage, now);
    assert_eq!(st.short_streak(), 0);
    // The notice, typed by the host, and its READY answer.
    now += Duration::from_secs(21);
    st.host_typed(now);
    assert!(
        !st.act_in_flight(),
        "the host's own next step is never held on it"
    );
    // Its echo before the spinner: not its answer — the policy's act waits.
    assert!(awaits(
        &at(&mut st, &idle("Stage 1 complete.", None), now),
        now
    ));
    now += Duration::from_secs(3);
    let ready = idle(
        "ATERM-UPGRADE-READY-c3428058",
        Some(Duration::from_millis(2_800)),
    );
    st.observe(&ready, now);
    assert_eq!(st.short_streak(), 0, "READY is no short turn");
    // Relaunched, the carry-on typed, and its short reply: continued at once.
    now += Duration::from_secs(84);
    st.host_typed(now);
    now += Duration::from_secs(3);
    let reply = idle(
        "Waiting for Stage 2 instructions.",
        Some(Duration::from_millis(2_700)),
    );
    assert_eq!(at(&mut st, &reply, now), typed(RULE_CONTINUE));
    assert_eq!(st.short_streak(), 0);
    assert!(!st.act_in_flight());

    // NEGATIVE CONTROL: the same two short turns, the worker's own.
    let mut now = t0();
    let mut st = TurnEndState::default();
    st.observe(&stage, now);
    now += Duration::from_secs(24);
    st.observe(&ready, now);
    now += Duration::from_secs(87);
    assert!(waits_until(&at(&mut st, &reply, now), now + 4 * MIN));
    assert_eq!(st.short_streak(), 2);
}

/// THE ANSWER TO A HARNESS TURN THAT IS REAL WORK ENDS THE STREAK (review of
/// the N1 fix, 2026-09-26): a supervisor that attaches to an idle session
/// starts a streak of one (the first point, its work unknown); the upgrade
/// is due at once, and its carry-on — "continue where you left off" — starts
/// twenty minutes of the worker's own work. That answer is real work: the
/// next point is continued at once, never backed off 2 minutes from the
/// answer's end as the first point was. Answered SHORT, the harness's turn
/// leaves the worker's back-off exactly as it stood — counted from the
/// worker's own point, never pushed later by the harness's answer.
/// NEGATIVE CONTROL: the same short answer read as the worker's own turn
/// lengthens the streak.
#[test]
fn a_harness_turn_answered_with_real_work_ends_the_streak() {
    let t = t0();
    let mut st = TurnEndState::default();
    // The first point, its work unknown: a streak of one, 2 min back-off.
    assert!(waits_until(
        &at(&mut st, &idle("Stage 1 complete.", None), t),
        t + 2 * MIN
    ));
    assert_eq!(st.short_streak(), 1);
    // The notice, answered READY (short): the back-off stands, from the
    // worker's point.
    let now = t + Duration::from_secs(20);
    st.host_typed(now);
    let ready = idle(
        "ATERM-UPGRADE-READY-c3428058",
        Some(Duration::from_millis(2_800)),
    );
    let now = now + Duration::from_secs(3);
    assert!(waits_until(&at(&mut st, &ready, now), t + 2 * MIN));
    assert_eq!(st.short_streak(), 1, "READY lengthens nothing");
    // The carry-on, answered with twenty minutes of work: continued at once.
    let now = now + Duration::from_secs(60);
    st.host_typed(now);
    let now = now + 20 * MIN;
    let worked = idle("Stage 2 complete.", Some(20 * MIN));
    assert_eq!(at(&mut st, &worked, now), typed(RULE_CONTINUE));
    assert_eq!(st.short_streak(), 0, "the carry-on's work ends the streak");

    // NEGATIVE CONTROL: the READY answer as the worker's own short turn.
    let mut st = TurnEndState::default();
    st.observe(&idle("Stage 1 complete.", None), t);
    let now = t + Duration::from_secs(23);
    assert!(waits_until(&at(&mut st, &ready, now), now + 4 * MIN));
    assert_eq!(st.short_streak(), 2);
}

/// "Worker reports done" is no escalation any more: each short turn in a
/// row DOUBLES the wait before the next continuation — 2, 4, 8 … minutes,
/// never past an hour — and a turn of real work ends the streak (the next
/// point is continued at once). NEGATIVE CONTROL: the same point before its
/// back-off types nothing, and nothing is ever escalated.
#[test]
fn short_turns_back_off_doubling_to_an_hour_and_real_work_resets_it() {
    let short = Some(Duration::from_secs(20));
    let mut now = t0();
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &idle("Done.", Some(3 * MIN)), now),
        typed(RULE_CONTINUE)
    );
    let mut waits = Vec::new();
    for _ in 0..8 {
        now += Duration::from_secs(30);
        let point = idle("Done.", short);
        let a = at(&mut st, &point, now);
        let TurnEndAction::WaitUntil { until, .. } = a else {
            panic!("a short turn is backed off, never escalated: {a:?}");
        };
        waits.push((until - now).as_secs() / 60);
        let same = TurnEndReading {
            worked: None,
            ..point
        };
        assert!(matches!(
            at(&mut st, &same, until - Duration::from_secs(1)),
            TurnEndAction::WaitUntil { .. }
        ));
        now = until;
        assert_eq!(act(&mut st, &same, now), typed(RULE_CONTINUE));
    }
    assert_eq!(waits, [2, 4, 8, 16, 32, 60, 60, 60]);
    assert_eq!(st.short_streak(), 8);
    // Real work ends the streak: continued at once.
    now += 10 * MIN;
    assert_eq!(
        act(&mut st, &idle("Stage 2 done.", Some(5 * MIN)), now),
        typed(RULE_CONTINUE)
    );
    assert_eq!(st.short_streak(), 0);
}

#[test]
fn nothing_is_typed_under_a_box_a_survey_a_draft_or_a_non_agent_screen() {
    let now = t0();
    let base = idle("Fixed it.", Some(3 * MIN));
    let cases = [
        TurnEndReading {
            phase: Phase::Prompt,
            ..base.clone()
        },
        TurnEndReading {
            phase: Phase::Busy,
            ..base.clone()
        },
        TurnEndReading {
            survey: true,
            ..base.clone()
        },
        TurnEndReading {
            composer: Composer::Absent,
            ..base.clone()
        },
        TurnEndReading {
            authoritative: false,
            ..base.clone()
        },
        // A continuation just submitted, its spinner not drawn yet.
        TurnEndReading {
            pending_input: true,
            ..base.clone()
        },
    ];
    for r in cases {
        let mut st = TurnEndState::default();
        assert_eq!(at(&mut st, &r, now), TurnEndAction::Nothing, "{r:?}");
    }
    // The control: the same point with none of them.
    let mut st = TurnEndState::default();
    assert_eq!(at(&mut st, &base, now), typed(RULE_CONTINUE));
    // Off: nothing.
    let mut off = cfg();
    off.continue_policy = false;
    let mut st = TurnEndState::default();
    st.observe(&base, now);
    assert_eq!(
        decide_turn_end(&st, &base, &off, now),
        TurnEndAction::Nothing
    );
}

#[test]
fn a_529_backs_off_then_continues_exactly_once_per_step_for_ever() {
    let t = t0();
    let mut st = TurnEndState::default();
    let r = walled(
        WallKind::Overloaded,
        "API Error: 529 Overloaded.",
        Some(3 * MIN),
    );
    let a = at(&mut st, &r, t);
    assert_eq!(
        a,
        TurnEndAction::WaitUntil {
            until: t + MIN,
            why: "overloaded retry 1".to_string()
        }
    );
    // Still waiting a second before the step; then exactly one continue.
    let same = TurnEndReading {
        worked: None,
        ..r.clone()
    };
    assert!(matches!(
        at(&mut st, &same, t + MIN - Duration::from_secs(1)),
        TurnEndAction::WaitUntil { .. }
    ));
    assert_eq!(act(&mut st, &same, t + MIN), typed(RULE_API_RETRY));
    assert!(
        awaits(&decide_turn_end(&st, &same, &cfg(), t + MIN), t + MIN),
        "the retry's point not seen yet"
    );
    // The wall again: 5 min from its new appearance, then 15, 30, 60, and
    // 60 for ever after — never escalated.
    let again = t + 2 * MIN;
    let back = TurnEndReading {
        worked: Some(Duration::from_secs(5)),
        ..r.clone()
    };
    assert!(matches!(
        at(&mut st, &back, again),
        TurnEndAction::WaitUntil { until, .. } if until == again + 5 * MIN
    ));
    assert_eq!(act(&mut st, &same, again + 5 * MIN), typed(RULE_API_RETRY));
    let third = again + 6 * MIN;
    assert!(matches!(
        at(&mut st, &back, third),
        TurnEndAction::WaitUntil { until, .. } if until == third + 15 * MIN
    ));
    assert_eq!(act(&mut st, &same, third + 15 * MIN), typed(RULE_API_RETRY));
    let mut next = third + 16 * MIN;
    for wait in [30, 60, 60, 60] {
        assert!(
            waits_until(&at(&mut st, &back, next), next + wait * MIN),
            "{wait} min"
        );
        assert_eq!(
            act(&mut st, &same, next + wait * MIN),
            typed(RULE_API_RETRY)
        );
        next += (wait + 1) * MIN;
    }
    // Negative control: a retry that got the worker going clears the track,
    // and a 529 hours later starts from the first step.
    let mut st = TurnEndState::default();
    at(&mut st, &r, t);
    act(&mut st, &same, t + MIN);
    at(
        &mut st,
        &idle("Suites green.", Some(10 * MIN)),
        t + 12 * MIN,
    );
    assert!(matches!(
        at(&mut st, &r, t + 3 * 60 * MIN),
        TurnEndAction::WaitUntil { until, .. } if until == t + 3 * 60 * MIN + MIN
    ));
}

/// An API error the vendor does not retry is retried on the same ladder:
/// nobody is there to do anything else with it. NEGATIVE CONTROL: with
/// `retry_api_errors = false` it is escalated.
#[test]
fn an_api_error_is_retried_on_the_ladder_and_retries_off_escalate() {
    let t = t0();
    let mut st = TurnEndState::default();
    let r = walled(
        WallKind::ApiError {
            code: Some(400),
            retryable: false,
        },
        "API Error: 400 bad request",
        Some(3 * MIN),
    );
    assert!(waits_until(&at(&mut st, &r, t), t + MIN));
    let same = TurnEndReading {
        worked: None,
        ..r.clone()
    };
    assert_eq!(at(&mut st, &same, t + MIN), typed(RULE_API_RETRY));
    let mut off = cfg();
    off.retry_api_errors = false;
    let mut st = TurnEndState::default();
    let r = walled(WallKind::Overloaded, "529", Some(3 * MIN));
    st.observe(&r, t);
    assert!(is_escalate(
        &decide_turn_end(&st, &r, &off, t),
        "retry_api_errors is off"
    ));
}

#[test]
fn a_usage_reset_gets_the_continuation_not_a_probe() {
    let t = t0();
    let mut st = TurnEndState::default();
    let mut r = walled(
        WallKind::UsageSession,
        "You've hit your session limit · resets 3pm",
        Some(3 * MIN),
    );
    r.phase = Phase::Limited {
        message: r.wall_message.clone(),
        reset: Some("3pm".to_string()),
    };
    r.reset_at = Some(t + 60 * MIN);
    assert!(matches!(
        at(&mut st, &r, t),
        TurnEndAction::WaitUntil { until, .. } if until == t + 61 * MIN
    ));
    let a = act(&mut st, &r, t + 61 * MIN);
    assert_eq!(a, typed(RULE_LIMIT_RESUME));
    let TurnEndAction::Type { text, .. } = &a else {
        unreachable!()
    };
    assert!(!text.contains("watcher") && !text.contains('?'), "{text}");
    // The wall again after it: 10 min from then, and 30 after that.
    let back = t + 62 * MIN;
    assert!(matches!(
        at(&mut st, &TurnEndReading { worked: Some(Duration::from_secs(3)), ..r.clone() }, back),
        TurnEndAction::WaitUntil { until, .. } if until == back + 10 * MIN
    ));
    // A notice that says the vendor goes on by itself: waited out — a wait,
    // so the point is HANDLED and nobody is told (the philosophy review of
    // 2026-09-25: `Nothing` opened an escalated episode) — nothing typed
    // within `auto_resume_grace` of its time, and continued past it, the
    // net under a vendor that did not go on.
    let mut st = TurnEndState::default();
    r.resumes_by_itself = true;
    assert!(matches!(
        at(&mut st, &r, t + 61 * MIN),
        TurnEndAction::WaitUntil { until, .. } if until == t + 70 * MIN
    ));
    assert_eq!(at(&mut st, &r, t + 90 * MIN), typed(RULE_LIMIT_RESUME));
    // `resume_limits` off: nothing.
    r.resumes_by_itself = false;
    let mut off = cfg();
    off.resume_limits = false;
    let mut st = TurnEndState::default();
    st.observe(&r, t);
    assert_eq!(
        decide_turn_end(&st, &r, &off, t + 90 * MIN),
        TurnEndAction::Nothing
    );
}

#[test]
fn the_screen_leaving_a_usage_wall_with_no_work_is_continued_at_once() {
    let t = t0();
    let mut st = TurnEndState::default();
    let mut r = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit",
        Some(3 * MIN),
    );
    r.reset_at = Some(t + 600 * MIN);
    assert!(matches!(
        at(&mut st, &r, t),
        TurnEndAction::WaitUntil { .. }
    ));
    // A human's `/login` output over it: no wall, no work.
    let left = idle("", None);
    assert_eq!(at(&mut st, &left, t + MIN), typed(RULE_LIMIT_RESUME));
    // Negative control: the worker worked since — the wall is over, and the
    // point is the continue policy's (a short turn: its back-off).
    let mut st = TurnEndState::default();
    at(&mut st, &r, t);
    assert!(waits_until(
        &at(
            &mut st,
            &idle("Back.", Some(Duration::from_secs(20))),
            t + MIN
        ),
        t + 3 * MIN
    ));
}

/// A model bucket (owner decision 3) under D7: the agent RELAUNCHED on the
/// fallback — `Restart::Model`, the host's `--model opus` on the relaunch
/// line, session-only — and relaunched on the bucket's model again at its
/// reset, the point's continuation the fallback should that not be made.
/// Never Claude's own `/model`, which also saves the person's default for
/// every new session. NEGATIVE CONTROLS: where no host relaunches it
/// (`drive watch`) or its relaunch could not be made, the reset is waited
/// out — nothing typed, nothing escalated.
#[test]
fn a_fable_limit_relaunches_on_opus_and_back_at_its_reset_never_by_model() {
    let t = t0();
    let mut st = TurnEndState::default();
    let mut r = walled(
        WallKind::ModelBucket { consent: false },
        "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
        Some(3 * MIN),
    );
    r.reset_at = Some(t + 120 * MIN);
    let a = act(&mut st, &r, t);
    assert!(
        restarts(
            &a,
            &Restart::Model {
                to: "opus".to_string()
            },
            RULE_MODEL_FALLBACK
        ),
        "{a:?}"
    );
    let TurnEndAction::Restart { otherwise, .. } = &a else {
        unreachable!()
    };
    assert!(
        waits_until(otherwise, t + 121 * MIN),
        "unmade, the reset is waited out: {otherwise:?}"
    );
    assert_eq!(
        st.model_switch(),
        Some(&ModelSwitch {
            from: Some("fable".to_string()),
            to: "opus".to_string(),
            back_at: Some(t + 120 * MIN)
        })
    );
    // Hours of work on opus; the turn ends after the bucket's reset: back to
    // fable by a relaunch, which carries it on — else the point's own act.
    let later = idle("Stage 3 done.", Some(30 * MIN));
    let a = act(&mut st, &later, t + 130 * MIN);
    assert!(
        restarts(
            &a,
            &Restart::ModelBack {
                to: Some("fable".to_string())
            },
            RULE_MODEL_RESTORE
        ),
        "{a:?}"
    );
    let TurnEndAction::Restart { otherwise, .. } = &a else {
        unreachable!()
    };
    assert_eq!(**otherwise, typed(RULE_CONTINUE));
    assert_eq!(st.model_switch(), None);
    // NEGATIVE CONTROLS: no host relaunches it — the reset is waited out;
    // a relaunch that could not be made — the same.
    let bare = TurnEndReading {
        restartable: false,
        ..r.clone()
    };
    let mut st = TurnEndState::default();
    assert!(waits_until(&at(&mut st, &bare, t), t + 121 * MIN));
    let mut st = TurnEndState::default();
    let a = at(&mut st, &r, t);
    let TurnEndAction::Restart { why, .. } = &a else {
        panic!("{a:?}")
    };
    st.restarted(why, false, &r, t);
    assert!(waits_until(&at(&mut st, &r, t + MIN), t + 121 * MIN));
    assert_eq!(st.model_switch(), None, "nothing switched");
}

/// A bucket that asks CONSENT to go on on usage credits is accepted (owner,
/// 2026-09-24): continued under the consent rule — the vendor's confirm is
/// then a box the approval policy answers — and, only when the bucket comes
/// back after that, switched off like any bucket. Negative control: under
/// `continue = false` it is switched at once. Under a box nothing is typed;
/// switched once, the bucket again is waited out to its reset, never
/// escalated; with no fallback the reset is waited out, and escalated only
/// where the owner switched resuming off.
#[test]
fn a_model_bucket_is_accepted_under_consent_never_under_a_box_and_waits_out_a_second() {
    let t = t0();
    let consent = walled(
        WallKind::ModelBucket { consent: true },
        "Fable limit reached · continuing on Sonnet uses usage credits, and the prompt to confirm",
        Some(3 * MIN),
    );
    let mut st = TurnEndState::default();
    assert_eq!(act(&mut st, &consent, t), typed(RULE_CONSENT));
    // The consent's continuation met the bucket again: switched off.
    let again = walled(
        WallKind::ModelBucket { consent: true },
        "Fable limit reached · continuing on Sonnet uses usage credits, and the prompt to confirm",
        Some(Duration::from_secs(5)),
    );
    assert!(matches!(
        at(&mut st, &again, t + MIN),
        TurnEndAction::Restart {
            rule_id: RULE_MODEL_FALLBACK,
            ..
        }
    ));
    let no_continue = SupervisorConfig {
        continue_policy: false,
        ..cfg()
    };
    let mut st = TurnEndState::default();
    assert!(matches!(
        at_under(&mut st, &consent, t, &no_continue),
        TurnEndAction::Restart {
            rule_id: RULE_MODEL_FALLBACK,
            ..
        }
    ));
    // The consent dialog is a box: nothing is typed.
    let boxed = TurnEndReading {
        phase: Phase::Prompt,
        ..consent.clone()
    };
    let mut st = TurnEndState::default();
    assert_eq!(at(&mut st, &boxed, t), TurnEndAction::Nothing);
    // The bucket again on the fallback: its reset waited out, no second
    // switch, nothing escalated.
    let r = walled(
        WallKind::ModelBucket { consent: false },
        "You've reached your Fable limit.",
        Some(3 * MIN),
    );
    let mut st = TurnEndState::default();
    act(&mut st, &r, t);
    act(&mut st, &idle("", None), t + MIN);
    let opus = TurnEndReading {
        reset_at: Some(t + 90 * MIN),
        ..walled(
            WallKind::ModelBucket { consent: false },
            "You've reached your Opus limit.",
            Some(3 * MIN),
        )
    };
    assert!(waits_until(&at(&mut st, &opus, t + 5 * MIN), t + 91 * MIN));
    // No fallback configured: wait out the reset — escalated only where the
    // owner switched resuming off.
    let mut none = cfg();
    none.model_fallback = None;
    let reset = TurnEndReading {
        reset_at: Some(t + 90 * MIN),
        ..r.clone()
    };
    let mut st = TurnEndState::default();
    st.observe(&reset, t);
    assert!(waits_until(
        &decide_turn_end(&st, &reset, &none, t),
        t + 91 * MIN
    ));
    none.resume_limits = false;
    assert!(is_escalate(
        &decide_turn_end(&st, &reset, &none, t),
        "resume_limits is off"
    ));
    // An unknown bucket model is switched, never switched back.
    let mut st = TurnEndState::default();
    let odd = walled(
        WallKind::ModelBucket { consent: false },
        "Nimbus requires usage credits.",
        Some(3 * MIN),
    );
    act(&mut st, &odd, t);
    assert_eq!(st.model_switch().and_then(|m| m.from.clone()), None);
}

#[test]
fn a_full_context_is_compacted_then_continued_and_compacted_again_on_the_ladder() {
    let t = t0();
    let mut st = TurnEndState::default();
    let r = walled(
        WallKind::Context,
        "Context limit reached · /compact or /clear to continue",
        Some(3 * MIN),
    );
    assert_eq!(
        act(&mut st, &r, t),
        TurnEndAction::TypeCommand {
            command: "/compact".to_string(),
            rule_id: RULE_COMPACT,
            then: Then::Continue
        }
    );
    // The same wall read again before `/compact` ran: nothing typed.
    assert!(awaits(
        &decide_turn_end(
            &st,
            &TurnEndReading {
                worked: None,
                ..r.clone()
            },
            &cfg(),
            t
        ),
        t
    ));
    // The compaction ran (busy), the wall is gone: the continuation.
    assert_eq!(
        act(
            &mut st,
            &idle("Compacted.", Some(40 * Duration::from_secs(1))),
            t + MIN
        ),
        typed(RULE_COMPACT)
    );
    // Off: escalate.
    let mut off = cfg();
    off.compact_on_context_wall = false;
    let mut st = TurnEndState::default();
    st.observe(&r, t);
    assert!(is_escalate(
        &decide_turn_end(&st, &r, &off, t),
        "context full"
    ));
    // `/compact` did not free it (no work in between): `/compact` again,
    // after the retry ladder's first wait — never escalated.
    let mut st = TurnEndState::default();
    act(&mut st, &r, t);
    let still = TurnEndReading {
        worked: None,
        ..r.clone()
    };
    let half = Duration::from_secs(30);
    assert!(waits_until(&at(&mut st, &still, t + half), t + MIN));
    assert!(matches!(
        at(&mut st, &still, t + MIN),
        TurnEndAction::TypeCommand {
            rule_id: RULE_COMPACT,
            ..
        }
    ));
}

/// The reading of a real Claude Code screen, `worked` the busy work since
/// the last point.
fn read_screen(rows: &[String], worked: Option<Duration>) -> TurnEndReading {
    let reading = aterm_phase::read(Some("claude"), rows, Some(2));
    TurnEndReading {
        restartable: true,
        ..TurnEndReading::of(&reading, rows, false, worked, None, None)
    }
}

/// The incident's screen (`⏺ Login expired · Please run /login`, the
/// supervisor's `continue` answered by it) with its last rows replaced by
/// `tail`.
fn login_screen(tail: &[&str]) -> Vec<String> {
    let mut r = screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED);
    let at = r
        .iter()
        .position(|row| row.starts_with("⏺ Login expired"))
        .expect("the wall row");
    r.splice(at..=at, tail.iter().map(|s| (*s).to_string()));
    r
}

/// THE LOGIN WALL OF 2026-09-27, over the real screens: the incident's
/// point — the supervisor's own `continue` answered by `⏺ Login expired ·
/// Please run /login` — types `/login` ONCE, escalated as it is typed (the
/// owner told at once), and main's `keep going` there is gone (it read the
/// screen idle with no wall, and continued it nine hours). The wall again —
/// the `/login` dialog dismissed and the wall row back, or the dialog's
/// `Login interrupted` under it — types nothing more: no second `/login`, no
/// continuation into a login the policy saw gone. The person's `/login`
/// done (`⎿  Login successful`) is continued at once; the worker working
/// ends the track, and a later lost login gets its `/login` again. A draft
/// standing at the wall is never sent into it: the point is escalated.
/// NEGATIVE CONTROLS: the dismissed dialog's screen with no lost login seen
/// is an ordinary point and continued; and the text under the gutter reads
/// the same wall.
#[test]
fn the_login_expired_row_types_login_once_and_holds_until_the_login_is_back() {
    let t = t0();
    let walled = login_screen(&["⏺ Login expired · Please run /login"]);
    let r = read_screen(&walled, Some(3 * MIN));
    assert_eq!(r.wall, Some(WallKind::Auth), "read as the auth wall");
    assert_eq!(r.wall_message, "Login expired · Please run /login");
    assert!(!r.login_back);
    let mut st = TurnEndState::default();
    let login = TurnEndAction::TypeCommand {
        command: "/login".to_string(),
        rule_id: RULE_LOGIN,
        then: Then::Escalate("finish sign-in in the browser".to_string()),
    };
    assert_eq!(act(&mut st, &r, t), login);
    assert_eq!(st.login_track(), Some(1));
    // The wall again (a person's Esc on the dialog, a turn someone else
    // began): nothing — the same wall's `/login` is never typed twice.
    let again = read_screen(&walled, None);
    for k in 1..=3 {
        assert_eq!(act(&mut st, &again, t + k * MIN), TurnEndAction::Nothing);
    }
    // The dialog gone with no login: the wall's row is history, and still
    // nothing is typed.
    let interrupted = login_screen(&[
        "⏺ Login expired · Please run /login",
        "",
        "❯ /login",
        "  ⎿  Login interrupted",
    ]);
    let off = read_screen(&interrupted, None);
    assert_eq!(off.wall, None);
    assert!(!off.login_back);
    assert_eq!(act(&mut st, &off, t + 5 * MIN), TurnEndAction::Nothing);
    assert_eq!(st.login_track(), Some(1), "the track stands with no work");
    // The login back: continued at once.
    let back = read_screen(
        &login_screen(&[
            "⏺ Login expired · Please run /login",
            "",
            "❯ /login",
            "  ⎿  Login successful",
        ]),
        None,
    );
    assert!(back.login_back);
    assert_eq!(act(&mut st, &back, t + 6 * MIN), typed(RULE_CONTINUE));
    // The worker works: the track ends, and a later lost login is told of
    // and typed `/login` again.
    let worked = idle("Back at it: the suites are running.", Some(3 * MIN));
    st.observe(&worked, t + 10 * MIN);
    assert_eq!(st.login_track(), None);
    assert_eq!(act(&mut st, &r, t + 60 * MIN), login);

    // A draft at the wall: escalated, never submitted into it.
    let mut st = TurnEndState::default();
    let drafted = TurnEndReading {
        composer: Composer::Typed,
        ..r.clone()
    };
    st.observe(&drafted, t);
    let a = decide_turn_end(&st, &drafted, &cfg(), t);
    assert!(is_escalate(&a, "a draft stands in the composer"), "{a:?}");

    // NEGATIVE CONTROLS: the same dismissed-dialog screen where no lost
    // login was seen is an ordinary point — continued — so it is the track
    // that holds it above; and the wall's words under the gutter are the
    // same wall.
    let mut fresh = TurnEndState::default();
    assert_eq!(
        act(
            &mut fresh,
            &read_screen(&interrupted, Some(3 * MIN)),
            t + 5 * MIN
        ),
        typed(RULE_CONTINUE)
    );
    let gutter = read_screen(
        &login_screen(&["⏺ Pushing.", "  ⎿  Login expired · Please run /login"]),
        Some(3 * MIN),
    );
    assert_eq!(gutter.wall, Some(WallKind::Auth));
    assert_eq!(act(&mut TurnEndState::default(), &gutter, t), login);
}

#[test]
fn a_lost_login_types_login_then_escalates_and_a_spend_wall_waits_its_reset() {
    let t = t0();
    let mut st = TurnEndState::default();
    let r = walled(
        WallKind::Auth,
        "Not logged in · Please run /login",
        Some(3 * MIN),
    );
    let a = act(&mut st, &r, t);
    assert_eq!(
        a,
        TurnEndAction::TypeCommand {
            command: "/login".to_string(),
            rule_id: RULE_LOGIN,
            then: Then::Escalate("finish sign-in in the browser".to_string())
        }
    );
    // Back at the notice after the dialog: not typed again.
    assert_eq!(
        at(
            &mut st,
            &TurnEndReading {
                worked: None,
                ..r.clone()
            },
            t + MIN
        ),
        TurnEndAction::Nothing
    );
    // A policy that retries no API error types no `/login` either.
    let mut off = cfg();
    off.retry_api_errors = false;
    let mut st = TurnEndState::default();
    st.observe(&r, t);
    assert!(is_escalate(
        &decide_turn_end(&st, &r, &off, t),
        "the login is gone"
    ));
    let mut st = TurnEndState::default();
    // Money: never bought, its reset waited out (the longest limit back-off
    // when it names none), then continued.
    let s = walled(
        WallKind::Spend,
        "You've hit your monthly spend limit.",
        Some(3 * MIN),
    );
    let back = *st.timing.limit_backoff.last().expect("a ladder");
    assert!(waits_until(&at(&mut st, &s, t), t + back));
    let mut limited = cfg();
    limited.resume_limits = false;
    let mut st = TurnEndState::default();
    st.observe(&s, t);
    assert!(is_escalate(
        &decide_turn_end(&st, &s, &limited, t),
        "resume_limits is off"
    ));
}

#[test]
fn the_rules_ride_in_the_continuation() {
    let now = t0();
    let mut st = TurnEndState::default();
    let mut r = idle("Done.", Some(3 * MIN));
    r.rules = Some("commit after each stage".to_string());
    assert_eq!(
        at(&mut st, &r, now),
        TurnEndAction::Type {
            text: "keep going (standing rules: commit after each stage)".to_string(),
            rule_id: RULE_CONTINUE
        }
    );
}

#[test]
fn the_done_row_measures_the_turn_and_the_bucket_names_its_model() {
    assert_eq!(
        done_row_work(&screen(END_529)),
        Some(Duration::from_secs(182))
    );
    assert_eq!(done_row_work(&screen(GOAL_ACTIVE_SUGGESTION)), None, "busy");
    assert_eq!(
        bucket_model("You've reached your Fable 5 limit."),
        Some("fable".into())
    );
    assert_eq!(bucket_model("Opus 4.1 limit reached"), Some("opus".into()));
    assert_eq!(bucket_model("Usage limit reached"), None);
}

/// The measured and hand-built screens through `aterm-phase`'s reader: the
/// 529 end waits, the offer end continues, the session limit waits for its
/// reset, the `/goal` screen (a workflow running) is busy — nothing.
/// A continuation just submitted is not judged by a point the worker has
/// not taken it at: a user's `❯` row last on the screen (no turn ended
/// there), or — as Claude Code draws it, under the last done row, where it
/// reads as queued — a point with no busy read since the act. Its point is
/// the one after the worker was seen working. Negative control: that point
/// is judged, and a short yield counts.
#[test]
fn a_continuation_is_judged_only_once_the_worker_took_it() {
    let now = t0();
    let mut rows = rows_of(&["⏺ Done.", "", "❯ keep going", ""]);
    rows.extend(aterm_phase::prompt::fixtures::composer("  ? for shortcuts"));
    let reading = aterm_phase::read(Some("claude"), &rows, Some(2));
    let r = TurnEndReading::of(&reading, &rows, false, None, None, None);
    assert!(r.pending_input, "{rows:#?}");
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &idle("Done.", Some(3 * MIN)), now),
        typed(RULE_CONTINUE)
    );
    assert!(awaits(&at(&mut st, &r, now), now));
    // Queued under the done row: an idle read with no busy since.
    assert!(awaits(&at(&mut st, &idle("Done.", None), now), now));
    assert_eq!(st.short_streak(), 0, "not judged yet");
    // The worker took it, briefly: judged now, a short turn.
    let short = Some(Duration::from_secs(20));
    assert!(waits_until(
        &at(&mut st, &idle("Done.", short), now),
        now + 2 * MIN
    ));
    assert_eq!(st.short_streak(), 1);
}

fn rows_of(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_fixture_screens_read_and_decide() {
    let now = t0();
    let read = |text: &str, col: usize, worked: Option<Duration>, reset: Option<Instant>| {
        let rows = screen(text);
        let reading = aterm_phase::read(Some("claude"), &rows, Some(col));
        TurnEndReading::of(&reading, &rows, false, worked, reset, None)
    };
    let mut st = TurnEndState::default();
    let r = read(END_529, 2, Some(Duration::from_secs(5)), None);
    assert_eq!(r.wall, Some(WallKind::Overloaded));
    assert_eq!(
        r.worked,
        Some(Duration::from_secs(182)),
        "the done row's measure"
    );
    assert!(matches!(
        at(&mut st, &r, now),
        TurnEndAction::WaitUntil { .. }
    ));

    let mut st = TurnEndState::default();
    let r = read(END_OFFER, 2, Some(Duration::from_secs(5)), None);
    assert_eq!(r.phase, Phase::Question);
    assert_eq!(at(&mut st, &r, now), typed(RULE_CONTINUE));

    let mut st = TurnEndState::default();
    let r = read(
        END_SESSION_LIMIT,
        2,
        Some(Duration::from_secs(5)),
        Some(now + 30 * MIN),
    );
    assert_eq!(r.wall, Some(WallKind::UsageSession));
    assert!(matches!(
        at(&mut st, &r, now),
        TurnEndAction::WaitUntil { until, .. } if until == now + 31 * MIN
    ));

    let mut st = TurnEndState::default();
    let r = read(GOAL_ACTIVE_SUGGESTION, 2, Some(10 * MIN), None);
    assert_eq!(r.phase, Phase::Busy);
    assert_eq!(r.composer, Composer::Placeholder("keep going".into()));
    assert_eq!(at(&mut st, &r, now), TurnEndAction::Nothing);
}

/// Lane B2's review (major 1): a continuation whose point never shows the
/// worker busy — a reply that finished inside the `turn` verb's settle
/// (`All finished, nothing left.`) — was awaited forever. It is waited for
/// [`TurnEndTiming::take_within`] from the first such point, then judged as
/// the short yield it is: backed off and continued, on a longer back-off
/// each time — hours on, still never escalated. Negative control: a busy
/// read before the deadline is the act's point as before, and a busy screen
/// past the deadline is never judged idle.
#[test]
fn a_continuation_never_seen_busy_is_judged_at_its_deadline_not_latched() {
    let t = t0();
    let take = TurnEndTiming::default().take_within;
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t),
        typed(RULE_CONTINUE)
    );
    let quiet = idle("All finished, nothing left.", None);
    let first = t + Duration::from_secs(3);
    assert!(awaits(&at(&mut st, &quiet, first), first));
    // The deadline counts from the first unseen point, not from each read.
    assert!(awaits(&at(&mut st, &quiet, first + take / 2), first));
    // Busy past the deadline: never judged idle.
    let busy = TurnEndReading {
        phase: Phase::Busy,
        ..quiet.clone()
    };
    assert_eq!(at(&mut st, &busy, first + take), TurnEndAction::Nothing);
    // At the deadline: the short yield, backed off 2 min, then continued.
    let due = first + take;
    assert!(waits_until(&at(&mut st, &quiet, due), due + 2 * MIN));
    assert_eq!(act(&mut st, &quiet, due + 2 * MIN), typed(RULE_CONTINUE));
    assert_eq!(st.short_streak(), 1);
    // The second unseen yield: 4 min this time — and hours on, never an
    // escalation.
    let later = due + 2 * MIN + Duration::from_secs(5);
    assert!(awaits(&at(&mut st, &quiet, later), later));
    let judged = later + take;
    assert!(waits_until(&at(&mut st, &quiet, judged), judged + 4 * MIN));
    for h in 1..=5 {
        let a = at(&mut st, &quiet, judged + h * 60 * MIN);
        assert_eq!(a, typed(RULE_CONTINUE), "{a:?}");
    }
    // Negative control: a busy read before the deadline is the act's point.
    let mut st = TurnEndState::default();
    act(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t);
    assert!(awaits(&at(&mut st, &quiet, first), first));
    let worked = idle("Stage 2 done.", Some(3 * MIN));
    assert_eq!(at(&mut st, &worked, first + take / 2), typed(RULE_CONTINUE));
    assert_eq!(st.short_streak(), 0);
}

/// A continuation submitted and never answered (the user's `❯` row last,
/// no spinner, no reply) is, at its deadline, the short yield it is: backed
/// off, then continued again with the row still there — on a longer
/// back-off each time, never escalated (it was, as "not taken", until
/// 2026-09-24). Controls: before the deadline it is waited for; a row that
/// is no act of this policy's (a person's message, before its spinner) is
/// never a point.
#[test]
fn a_continuation_left_unanswered_is_acted_again_on_the_back_off() {
    let t = t0();
    let take = TurnEndTiming::default().take_within;
    let mut st = TurnEndState::default();
    act(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t);
    let pending = TurnEndReading {
        pending_input: true,
        ..idle("Stage 1 done.", None)
    };
    assert!(awaits(&at(&mut st, &pending, t), t));
    let due = t + take;
    assert!(waits_until(&at(&mut st, &pending, due), due + 2 * MIN));
    assert_eq!(st.short_streak(), 1);
    assert_eq!(act(&mut st, &pending, due + 2 * MIN), typed(RULE_CONTINUE));
    // Not taken again: judged at its own deadline, 4 min this time.
    let seen = due + 2 * MIN + Duration::from_secs(5);
    assert!(awaits(&at(&mut st, &pending, seen), seen));
    let again = seen + take;
    assert!(waits_until(&at(&mut st, &pending, again), again + 4 * MIN));
    assert_eq!(st.short_streak(), 2);
    // Anyone else's unanswered row: nothing, however long it stands.
    let mut st = TurnEndState::default();
    at(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t);
    for later in [t, t + take, t + 60 * MIN] {
        assert_eq!(at(&mut st, &pending, later), TurnEndAction::Nothing);
    }
}

/// A DRAFT in the composer is never typed over. Within a person's grace
/// (their keystroke, or the draft still changing — the loop folds both
/// into `person`) it waits; past it, the draft is SUBMITTED in the place
/// of whatever act was due, under that act's rule: a continuation's, a
/// retry's — and a wall's wait or a back-off still holds it. Before
/// 2026-09-24 a draft stopped the session for ever with nobody told.
/// NEGATIVE CONTROL: the same point with an empty composer types the act.
#[test]
fn a_draft_left_standing_is_submitted_where_the_policy_would_act() {
    let t = t0();
    let drafted = |person: Option<Duration>| TurnEndReading {
        composer: Composer::Typed,
        person,
        ..idle("Fixed the parser.", Some(3 * MIN))
    };
    let grace = Duration::from_secs(u64::from(cfg().human_grace_s));
    let mut st = TurnEndState::default();
    let typing = drafted(Some(Duration::from_secs(10)));
    assert!(waits_until(
        &at(&mut st, &typing, t),
        t + grace - Duration::from_secs(10)
    ));
    let mut st = TurnEndState::default();
    assert_eq!(
        at(&mut st, &drafted(Some(grace)), t),
        TurnEndAction::Submit {
            rule_id: RULE_CONTINUE
        }
    );
    let mut st = TurnEndState::default();
    assert_eq!(
        at(&mut st, &drafted(None), t),
        TurnEndAction::Submit {
            rule_id: RULE_CONTINUE
        }
    );
    // Negative control: no draft, the continuation typed.
    let mut st = TurnEndState::default();
    assert_eq!(
        at(&mut st, &idle("Fixed the parser.", Some(3 * MIN)), t),
        typed(RULE_CONTINUE)
    );
    // A wall's wait holds a draft; once it is over, the draft is the retry.
    let mut st = TurnEndState::default();
    let walled_draft = TurnEndReading {
        composer: Composer::Typed,
        ..walled(
            WallKind::Overloaded,
            "API Error: 529 Overloaded.",
            Some(3 * MIN),
        )
    };
    assert!(waits_until(&at(&mut st, &walled_draft, t), t + MIN));
    assert_eq!(
        at(&mut st, &walled_draft, t + MIN),
        TurnEndAction::Submit {
            rule_id: RULE_API_RETRY
        }
    );
    // A back-off holds it too: a short turn's draft goes after 2 min.
    let mut st = TurnEndState::default();
    let short = TurnEndReading {
        worked: Some(Duration::from_secs(10)),
        ..drafted(None)
    };
    assert!(waits_until(&at(&mut st, &short, t), t + 2 * MIN));
}

/// A usage-resume continuation answered at once by the same notice (no busy
/// read) is that wall's next appearance at the deadline: the next backoff
/// counts from there, and the loop does not latch.
#[test]
fn a_resume_answered_at_once_by_the_wall_backs_off_instead_of_latching() {
    let t = t0();
    let take = TurnEndTiming::default().take_within;
    let cfg = SupervisorConfig {
        resume_limits: true,
        ..cfg()
    };
    let mut st = TurnEndState::default();
    let reset = t + 10 * MIN;
    let wall = TurnEndReading {
        reset_at: Some(reset),
        ..walled(
            WallKind::UsageSession,
            "Session limit reached",
            Some(3 * MIN),
        )
    };
    st.observe(&wall, t);
    let due = reset + st.timing.reset_grace;
    let a = decide_turn_end(&st, &wall, &cfg, due);
    assert_eq!(a, typed(RULE_LIMIT_RESUME));
    st.acted(&a, &wall, due);
    let again = TurnEndReading {
        worked: None,
        ..wall.clone()
    };
    let first = due + Duration::from_secs(2);
    st.observe(&again, first);
    assert!(awaits(&decide_turn_end(&st, &again, &cfg, first), first));
    st.observe(&again, first + take);
    let a = decide_turn_end(&st, &again, &cfg, first + take);
    assert!(
        matches!(a, TurnEndAction::WaitUntil { until, .. }
            if until == first + take + st.timing.limit_backoff[0]),
        "{a:?}"
    );
}

/// Lane B2's review (major 2): the worker's last words come one line per
/// SCREEN ROW, so a stop phrase or an either/or split by a soft wrap was
/// missed and continued; several ways of asking for a sign-off were not
/// stop phrases; and an offer to do something destructive was answered
/// `keep going`, which the worker reads as consent. Each wrapped case has
/// its one-line control, and the controls that must still continue do.
#[test]
fn wrapped_stops_sign_off_asks_and_destructive_offers_are_not_continued() {
    let stop = |said: &str| {
        matches!(
            classify_said(Some(said)),
            Said::Stop(_) | Said::Irreversible(_)
        )
    };
    // The wrapped stop phrase and its one-line control.
    assert!(stop(
        "Migrated the tables; before touching the shared database I need your\ndecision on the backup."
    ));
    assert!(stop(
        "Migrated the tables; before touching the shared database I need your decision on the backup."
    ));
    // The wrapped choice and its one-line control.
    assert!(stop("Want me to keep the old API\nor rename it?"));
    assert!(stop("Want me to keep the old API or rename it?"));
    assert!(stop("Should I keep the\nold API or drop it?"));
    // The sign-off asks the review listed.
    for said in [
        "The migration is written. Please sign off before I run it.",
        "I'll wait for your go-ahead before force-pushing.",
        "Let me know how you'd like to proceed.",
        "The branch is ready for your review.",
        "Ready for review.",
    ] {
        assert!(stop(said), "{said}");
    }
    // Destructive offers.
    for said in [
        "Want me to force-push this to main?",
        "Shall I delete the old release branches?",
        "Next steps:\n- drop the legacy table\n- ship",
        "Would you like me to\nrm the stale build dirs?",
    ] {
        assert!(stop(said), "{said}");
    }
    // Controls: offers and reports that still continue.
    let now = t0();
    for said in [
        "Done: 12 of 67 solve. Next: the binder path; want me to take that on?",
        "Fixed the lock by removing the stale file. Want me to keep\ngoing with stage 2?",
        "Suite green on v1.2. Want me to push it when green?",
        "Two ways forward:\n1. rewrite the parser\n2. patch the lexer\nI recommend 1; shall I start?",
        "Next steps: wire the lane into the host.",
        "Ran the suite before I committed; all 40 pass.",
    ] {
        let mut st = TurnEndState::default();
        assert_eq!(
            at(&mut st, &idle(said, Some(3 * MIN)), now),
            typed(RULE_CONTINUE),
            "{said}"
        );
    }
    // And through the decider: the wrapped choice is answered, never
    // continued — and escalated where the owner switched the answers off.
    let choice = idle("Want me to keep the old API\nor rename it?", Some(3 * MIN));
    let mut st = TurnEndState::default();
    assert_eq!(at(&mut st, &choice, now), answered());
    let mut st = TurnEndState::default();
    let a = at_under(&mut st, &choice, now, &no_answers());
    assert!(is_escalate(&a, "choose between options"), "{a:?}");
}

/// The safety review of 2026-09-24 (blocker): Esc on a turn that ran for
/// minutes drew `⎿  Interrupted · What should Claude do instead?`, read as a
/// question, and the policy typed `keep going` at once — restarting what the
/// person had just stopped. The Esc is a person at the keyboard: nothing is
/// typed (or escalated) for `human_grace_s` from the point; after that
/// nobody is there, and the turn is continued. Negative control: the same
/// turn without the interrupt row is continued at once.
#[test]
fn a_persons_interrupt_holds_the_turn_for_the_grace() {
    let rule = "─".repeat(100);
    let grace = Duration::from_secs(u64::from(cfg().human_grace_s));
    let now = t0();
    let decide = |body: &[&str], after: Duration| {
        let mut rows = rows_of(body);
        rows.extend(rows_of(&[
            "",
            &rule,
            "❯",
            &rule,
            "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
        ]));
        let reading = aterm_phase::read(Some("claude"), &rows, Some(2));
        let r = TurnEndReading::of(&reading, &rows, false, Some(5 * MIN), None, None);
        let mut st = TurnEndState::default();
        let first = at(&mut st, &r, now);
        let again = TurnEndReading {
            worked: None,
            ..r.clone()
        };
        (r.interrupted, first, at(&mut st, &again, now + after))
    };
    for body in [
        &[
            "⏺ Running the schema migration against the staging database now.",
            "",
            "⏺ Bash(./migrate.sh --env staging)",
            "  ⎿  Interrupted · What should Claude do instead?",
        ][..],
        &[
            "⏺ Running the schema migration against the staging database now.",
            "  ⎿  Interrupted · What should Claude do instead?",
        ][..],
    ] {
        let (read, first, after) = decide(body, grace);
        assert!(read);
        assert!(waits_until(&first, now + grace), "{first:?}");
        assert_eq!(after, typed(RULE_CONTINUE), "the grace over: continued");
    }
    // Negative control: no interrupt, the same work — continued at once.
    let (read, first, _) = decide(
        &["⏺ Ran the schema migration against the staging database."],
        Duration::ZERO,
    );
    assert!(!read);
    assert_eq!(first, typed(RULE_CONTINUE));
}

/// The safety review of 2026-09-24 (blocker): three shapes of a destructive
/// question were continued — a benign offer AFTER the destructive one, a
/// trailing `(y/n)` / `[y/N]`, and a question followed by an aside — and
/// `keep going` reads as yes. Each is answered now, through the decider —
/// and one that names the act only with the reversible-only answer (D1):
/// never `keep going`, never "take the option you would recommend", both of
/// which read as yes. Negative controls: the offers and reports the audit's
/// census continues still continue (a destructive word BEFORE the offer is
/// what was done); a choice that names no act gets `answer_text`.
#[test]
fn a_destructive_question_in_any_shape_is_answered_never_continued() {
    let now = t0();
    for (said, irreversible) in [
        (
            "Want me to delete the 3 stale worktrees under ~/aterm-*? Happy to also update the \
             docs.",
            true,
        ),
        (
            "Want me to delete the 3 stale worktrees under ~/aterm-*?\nOr want me to update the \
             changelog first?",
            true,
        ),
        (
            "Cleanup is staged. Delete the stale release branches now? (y/n)",
            true,
        ),
        (
            "All green locally. OK to force-push the rebased branch to main? [y/N]",
            true,
        ),
        (
            "Should I drop the legacy table now?\n(I have not touched it yet.)",
            true,
        ),
        ("Created a.txt. Should I also delete the old b.txt?", true),
        (
            "Created a.txt. Should I also create b.txt, or stop here?",
            false,
        ),
    ] {
        let got = classify_said(Some(said));
        let (want, answer) = if irreversible {
            (matches!(got, Said::Irreversible(_)), answered_reversibly())
        } else {
            (matches!(got, Said::Stop(_)), answered())
        };
        assert!(want, "{said}: {got:?}");
        let mut st = TurnEndState::default();
        assert_eq!(
            at(&mut st, &idle(said, Some(5 * MIN)), now),
            answer,
            "{said}"
        );
        let mut st = TurnEndState::default();
        let a = at_under(&mut st, &idle(said, Some(5 * MIN)), now, &no_answers());
        assert!(matches!(a, TurnEndAction::Escalate { .. }), "{said}: {a:?}");
    }
    // A plain yes/no question with an aside is still a question.
    assert_eq!(
        classify_said(Some(
            "Should I rename the module?\n(I have not touched it yet.)"
        )),
        Said::Question
    );
    assert_eq!(
        classify_said(Some("Proceed with stage 2 (y/n)")),
        Said::Question
    );
    // Negative controls.
    for said in [
        "Fixed the lock by removing the stale file. Want me to keep going with stage 2?",
        "Removed the dead code and the suite is green.",
        "Done: 12 of 67 solve. Next: the binder path; want me to take that on?",
        "Fixed it (the parser, not the lexer).",
    ] {
        let mut st = TurnEndState::default();
        assert_eq!(
            at(&mut st, &idle(said, Some(3 * MIN)), now),
            typed(RULE_CONTINUE),
            "{said}"
        );
    }
}

/// The safety review of 2026-09-24 (major): a final message taller than the
/// screen had no visible `⏺` row, `said_tail` was `None`, and `None` read as
/// Plain — so the question and the destructive offer it ended on were
/// continued. Through the real reader both escalate now; a Question with no
/// readable words escalates whatever. Negative control: the same long
/// report ending on a statement is continued.
#[test]
fn a_long_message_whose_head_scrolled_off_is_still_judged() {
    let rule = "─".repeat(100);
    let decide = |last: &str| {
        let mut rows: Vec<String> = (0..30)
            .map(|i| format!("  paragraph {i} of the summary continues here with more words"))
            .collect();
        rows.extend(rows_of(&[
            "",
            last,
            "",
            "✻ Cooked for 3m 2s · done 4:24 PM",
            "",
            &rule,
            "❯",
            &rule,
            "  ? for shortcuts",
        ]));
        let reading = aterm_phase::read(Some("claude"), &rows, Some(2));
        let r = TurnEndReading::of(&reading, &rows, false, Some(5 * MIN), None, None);
        let mut st = TurnEndState::default();
        at_under(&mut st, &r, t0(), &no_answers())
    };
    for last in [
        "  Should I drop the prod table or keep it?",
        "  Want me to force-push the rewritten history to origin/main?",
    ] {
        let a = decide(last);
        assert!(matches!(a, TurnEndAction::Escalate { .. }), "{last}: {a:?}");
    }
    assert_eq!(decide("  The suite is green."), typed(RULE_CONTINUE));
    // A question the reader has no words for: never "nothing asked" —
    // answered, or escalated without answers.
    let mut r = idle("x?", Some(3 * MIN));
    r.said_tail = None;
    let mut st = TurnEndState::default();
    assert_eq!(at(&mut st, &r, t0()), answered());
    let mut st = TurnEndState::default();
    assert!(is_escalate(
        &at_under(&mut st, &r, t0(), &no_answers()),
        "do not show"
    ));
}

/// THE UPGRADE'S OWNERSHIP IS ITS HOST'S WORD, never a screen pattern (the
/// philosophy review of 2026-09-25, blocking, and the hazards review of the
/// same day). The screen said "the last `❯` row is the announcement", so a
/// worker that answered it without the READY marker, an upgrade that gave
/// up, and an upgrade switched off left the worker idle for ever — and a
/// 33-row answer pushed the announcement off the loop's 40-row read, and
/// `keep going` went into the wind-down. Now: a point the host owns
/// (`upgrading`) types nothing; the same screens the host does not own —
/// the announcement answered without READY — are ordinary turn ends and are
/// continued. The reading itself never claims ownership from the screen.
#[test]
fn the_upgrade_owns_a_point_only_by_its_hosts_word() {
    use crate::harness::upgrade::{Source, Version, prepare_prompt, ready_marker};
    let from = Version::parse("2.1.280").expect("version");
    let to = Version::parse("2.1.281").expect("version");
    let marker = ready_marker("sess", &to, 7);
    let announce = prepare_prompt(&from, &to, Source::Native, &marker);
    let rule = "─".repeat(100);
    let mut rows = rows_of(&["⏺ Earlier work.", "", &format!("❯ {announce}"), ""]);
    rows.extend(rows_of(&[
        "⏺ Committed the parser work; nothing is running.",
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        "",
        &rule,
        "❯",
        &rule,
        "  ? for shortcuts",
    ]));
    let reading = aterm_phase::read(Some("claude"), &rows, Some(2));
    let r = TurnEndReading::of(&reading, &rows, false, Some(5 * MIN), None, None);
    assert!(!r.upgrading, "no screen says it");
    let mut st = TurnEndState::default();
    assert_eq!(at(&mut st, &r, t0()), typed(RULE_CONTINUE));
    let owned = TurnEndReading {
        upgrading: true,
        ..r
    };
    let mut st = TurnEndState::default();
    assert_eq!(at(&mut st, &owned, t0()), TurnEndAction::Nothing);
}

/// A WALL IS ITS OWN RULE'S EVEN WHERE THE UPGRADE OWNS THE TURN ENDS (the
/// reviews of 2026-09-26): the upgrade types nothing at a wall and its host
/// takes no step at one, so a wind-down turn that hit `You've hit your
/// session limit · resets 3pm` was held by `upgrading` for good — and, once
/// limits were let through, so was one that ended on `529 Overloaded`, a
/// retryable API error, a full context or a lost login. Now every wall is
/// decided as where nothing owns the point: a limit waited out and continued
/// past its reset, the overloaded vendor retried, the context compacted, the
/// login typed, the memory banner's restart asked for; a model bucket is
/// waited out, never relaunched on its fallback over the upgrade's own
/// restart. NEGATIVE CONTROLS: an owned point with no wall still types
/// nothing, and the bucket unowned is relaunched on its fallback.
#[test]
fn a_wall_is_its_own_rules_even_where_the_upgrade_owns_the_turn_ends() {
    let t = t0();
    let mut r = walled(
        WallKind::UsageSession,
        "You've hit your session limit · resets 3pm",
        Some(3 * MIN),
    );
    r.reset_at = Some(t + 60 * MIN);
    r.upgrading = true;
    let mut st = TurnEndState::default();
    assert!(matches!(
        at(&mut st, &r, t),
        TurnEndAction::WaitUntil { until, .. } if until == t + 61 * MIN
    ));
    assert_eq!(at(&mut st, &r, t + 61 * MIN), typed(RULE_LIMIT_RESUME));

    let mut bucket = walled(
        WallKind::ModelBucket { consent: false },
        "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
        Some(3 * MIN),
    );
    bucket.reset_at = Some(t + 120 * MIN);
    let owned = TurnEndReading {
        upgrading: true,
        ..bucket.clone()
    };
    let mut st = TurnEndState::default();
    let a = at(&mut st, &owned, t);
    assert!(
        waits_until(&a, t + 121 * MIN),
        "waited out, no relaunch: {a:?}"
    );
    // The control: the same wall, not owned, is relaunched on its fallback.
    let mut st = TurnEndState::default();
    assert!(matches!(
        at(&mut st, &bucket, t),
        TurnEndAction::Restart { .. }
    ));

    let mut st = TurnEndState::default();
    let idle_owned = TurnEndReading {
        upgrading: true,
        ..idle("Committed; nothing is running.", Some(5 * MIN))
    };
    assert_eq!(at(&mut st, &idle_owned, t), TurnEndAction::Nothing);
    // Every other wall: what the same wall gets where nothing owns the
    // point, step for step — its first never `Nothing`.
    for (kind, message) in [
        (WallKind::Overloaded, "529 Overloaded"),
        (
            WallKind::ApiError {
                code: Some(500),
                retryable: true,
            },
            "API Error: 500 Internal server error",
        ),
        (
            WallKind::Context,
            "Context limit reached · /compact or /clear to continue",
        ),
        (WallKind::Auth, "Not logged in · Please run /login"),
        (
            WallKind::Memory,
            "Claude Code is using 140.4GB of memory · restart to continue",
        ),
    ] {
        let free = walled(kind, message, Some(3 * MIN));
        let owned = TurnEndReading {
            upgrading: true,
            ..free.clone()
        };
        let (mut st_free, mut st_owned) = (TurnEndState::default(), TurnEndState::default());
        for step in [0, 1, 2] {
            let when = t + step * MIN;
            let a = act(&mut st_owned, &owned, when);
            assert!(step > 0 || a != TurnEndAction::Nothing, "{kind:?}");
            assert_eq!(a, act(&mut st_free, &free, when), "{kind:?} at +{step}m");
        }
    }
    // The overloaded vendor: waited its first back-off, then retried.
    let overloaded = TurnEndReading {
        upgrading: true,
        ..walled(WallKind::Overloaded, "529", Some(3 * MIN))
    };
    let mut st = TurnEndState::default();
    assert!(waits_until(&at(&mut st, &overloaded, t), t + MIN));
    let same = TurnEndReading {
        worked: None,
        ..overloaded
    };
    assert_eq!(act(&mut st, &same, t + MIN), typed(RULE_API_RETRY));
}

/// THE CONTINUATION A WALL'S ACT OWES IS PAID AT A POINT THE UPGRADE OWNS.
/// `/compact` at a full context is typed `then: Continue`: it leaves
/// `after` owed, and an owed act is an act in flight, which keeps the
/// upgrade's host out (`run.rs`'s `host_steps_here` asks
/// `!act_in_flight()`). Muted at the owned idle point after it — the wall gone,
/// so the point read as the upgrade's — the continuation was never typed and
/// the host never stepped: the agent sat compacted and idle for good, the
/// upgrade's notice unanswered (found by an adversarial review of the owned-wall
/// rule, 2026-09-27). NEGATIVE CONTROL: once it is paid, an owned idle point
/// types nothing again.
#[test]
fn the_continuation_a_walls_act_owes_is_paid_where_the_upgrade_owns_the_point() {
    let t = t0();
    let owned = |r: TurnEndReading| TurnEndReading {
        upgrading: true,
        ..r
    };
    let full = owned(walled(
        WallKind::Context,
        "Context limit reached · /compact or /clear to continue",
        Some(3 * MIN),
    ));
    let mut st = TurnEndState::default();
    assert!(matches!(
        act(&mut st, &full, t),
        TurnEndAction::TypeCommand {
            rule_id: RULE_COMPACT,
            then: Then::Continue,
            ..
        }
    ));
    // The compaction ran, the wall is gone: the owed continuation, owned
    // point or not — the same act as with no upgrade pending.
    let compacted = idle("Compacted.", Some(40 * Duration::from_secs(1)));
    let mut free = TurnEndState::default();
    act(
        &mut free,
        &TurnEndReading {
            upgrading: false,
            ..full.clone()
        },
        t,
    );
    assert_eq!(act(&mut free, &compacted, t + MIN), typed(RULE_COMPACT));
    assert_eq!(
        act(&mut st, &owned(compacted), t + MIN),
        typed(RULE_COMPACT)
    );
    // Paid, and judged at the next point (the worker took it): nothing is
    // owed or awaited, so the host may step, and the owned idle point is the
    // upgrade's again.
    assert_eq!(
        at(
            &mut st,
            &owned(idle("Stage done.", Some(3 * MIN))),
            t + 3 * MIN
        ),
        TurnEndAction::Nothing
    );
    assert!(!st.act_in_flight(), "nothing left to keep the host out");
}

/// A PERSON AT THE KEYBOARD wins: within `human_grace_s` of their last
/// keystroke (the server's `human_ms=`) nothing is typed — neither a
/// continuation, nor an answer, nor a wall's retry — and the point is
/// decided again when the grace is over. NEGATIVE CONTROLS: a keystroke
/// older than the grace, and no person at all, act at once.
#[test]
fn a_persons_keystroke_holds_every_act_for_the_grace() {
    let now = t0();
    let grace = Duration::from_secs(u64::from(cfg().human_grace_s));
    let ago = Duration::from_secs(30);
    for r in [
        idle("Stage done.", Some(3 * MIN)),
        idle("Should I rename the module?", Some(3 * MIN)),
        walled(WallKind::Overloaded, "529", Some(3 * MIN)),
    ] {
        let held = TurnEndReading {
            person: Some(ago),
            ..r.clone()
        };
        let mut st = TurnEndState::default();
        let a = at(&mut st, &held, now);
        assert!(waits_until(&a, now + grace - ago), "{r:?}: {a:?}");
        let old = TurnEndReading {
            person: Some(grace),
            ..r.clone()
        };
        let mut st = TurnEndState::default();
        let a = at(&mut st, &old, now);
        let free = at(&mut TurnEndState::default(), &r, now);
        assert_eq!(a, free, "{r:?}: a keystroke past the grace holds nothing");
        assert!(!waits_until(&free, now + grace - ago), "{r:?}");
    }
}

/// THE HAZARDS REVIEW OF 2026-09-25 (major): a worker that answers every
/// continuation by re-running its checks for a few minutes and saying it is
/// done was continued AT ONCE, for ever — real work reset the streak — and
/// spent the owner's usage on busywork. A continuation's yield that ends
/// DONE ([`says_done`]) is now short whatever it worked: the back-off
/// doubles, 2, 4, 8 … minutes. NEGATIVE CONTROL: the same work reported as
/// progress (not done) ends the streak and is continued at once.
#[test]
fn a_continuation_that_ends_done_backs_off_whatever_it_worked() {
    let done = "Re-ran the whole suite: everything is already done; nothing left to do.";
    let mut now = t0();
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &idle("Stage 1 landed.", Some(3 * MIN)), now),
        typed(RULE_CONTINUE)
    );
    let mut waits = Vec::new();
    for _ in 0..4 {
        now += 3 * MIN + Duration::from_secs(5);
        let point = idle(done, Some(3 * MIN + Duration::from_secs(5)));
        let a = at(&mut st, &point, now);
        let TurnEndAction::WaitUntil { until, .. } = a else {
            panic!("a done yield is backed off: {a:?}");
        };
        waits.push((until - now).as_secs() / 60);
        let same = TurnEndReading {
            worked: None,
            ..point
        };
        now = until;
        assert_eq!(act(&mut st, &same, now), typed(RULE_CONTINUE));
    }
    assert_eq!(waits, [2, 4, 8, 16]);
    // NEGATIVE CONTROL: the same three minutes reported as progress.
    now += 3 * MIN;
    assert_eq!(
        act(
            &mut st,
            &idle("Stage 2 landed; stage 3 next.", Some(3 * MIN)),
            now
        ),
        typed(RULE_CONTINUE)
    );
    assert_eq!(st.short_streak(), 0);
    assert!(says_done(Some("What would you like me to work on?")));
    assert!(!says_done(Some("Stage 3 is next.")));
}

/// THE LIVE E2E OF 2026-09-25 (defect 6): a brand-new session's first idle
/// point — the launch card, nobody has asked it anything — was read as a
/// worker that stopped short: `keep going` two minutes later, and the first
/// real question then waited four minutes, not two. A FRESH point is no
/// turn end: nothing is typed and no streak begins. NEGATIVE CONTROL: the
/// first point that is not fresh (a turn nobody here saw) still backs off
/// before it types.
#[test]
fn a_fresh_session_is_no_turn_end_and_starts_no_streak() {
    let now = t0();
    let mut st = TurnEndState::default();
    let fresh = TurnEndReading {
        said_tail: None,
        taskless: true,
        ..idle("", None)
    };
    assert_eq!(at(&mut st, &fresh, now), TurnEndAction::Nothing);
    assert_eq!(
        at(&mut st, &fresh, now + 10 * MIN),
        TurnEndAction::Nothing,
        "never continued, however long it sits"
    );
    assert_eq!(st.short_streak(), 0);
    // Its first question, after a short first turn: a 2-minute back-off.
    let q = idle(
        "Should I use approach A or approach B?",
        Some(Duration::from_secs(20)),
    );
    let a = at(&mut st, &q, now + 11 * MIN);
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { until, .. } if *until == now + 13 * MIN),
        "{a:?}"
    );
    // NEGATIVE CONTROL: a first point that is not fresh backs off (short 1).
    let mut st = TurnEndState::default();
    let a = at(&mut st, &idle("Stage 1 landed.", None), now);
    assert!(matches!(a, TurnEndAction::WaitUntil { .. }), "{a:?}");
    assert_eq!(st.short_streak(), 1);
}

/// D1 OF THE LIVE E2E OF 2026-09-26: a session whose only turns are the
/// HARNESS'S OWN — its upgrade notice, its carry-on, each answered by the
/// agent, so the screen shows messages and work — has no task, and gets NO
/// ACT of any kind at its turn ends: not `keep going` after a report (the
/// E2E's B 1a5299ab got one after the notice, READY, restart and carry-on,
/// and its agent asked what it should help with), not `answer_text` after a
/// question or a stop, not a wall's retry, not a draft's submit — however
/// long it sits. NEGATIVE CONTROL: the same points
/// once a person or an orchestrator has asked it something (`taskless`
/// false) get the ordinary policy — continued, answered, retried, submitted.
#[test]
fn a_session_only_the_harness_has_typed_into_gets_no_act() {
    let now = t0();
    let worked = Some(Duration::from_secs(4 * 60));
    let points = [
        (
            "a report",
            idle("Upgrade complete, ready to continue.", worked),
        ),
        (
            "a question",
            idle("What would you like me to help you with?", worked),
        ),
        (
            "a choice",
            idle("Should I rewrite the parser or patch the lexer?", worked),
        ),
        (
            "a wall",
            walled(WallKind::Overloaded, "API Error: 529 Overloaded.", worked),
        ),
        (
            "a draft",
            TurnEndReading {
                composer: Composer::Typed,
                ..idle("Done.", worked)
            },
        ),
    ];
    for (what, point) in &points {
        let taskless = TurnEndReading {
            taskless: true,
            ..point.clone()
        };
        let mut st = TurnEndState::default();
        for later in [0, 10, 120] {
            let a = at(&mut st, &taskless, now + later * MIN);
            assert_eq!(a, TurnEndAction::Nothing, "{what} at +{later} min");
        }
        // NEGATIVE CONTROL: asked something, the same point is acted on
        // (at once, or once its wait is over).
        let mut st = TurnEndState::default();
        let acted = [0, 2, 10, 120]
            .iter()
            .any(|later| at(&mut st, point, now + *later * MIN).rule_id().is_some());
        assert!(acted, "{what}: the ordinary policy acts on it");
    }
}

/// A CHOICE ASKED IN PROSE is ANSWERED (owner directive of 2026-09-25: "the
/// harness must choose the recommended option(s) and continue
/// automatically"): a worker that asks to choose — `… A or B?` the worker
/// would act on, two or more listed options under an ask that chooses, a
/// [`CHOICE_PHRASES`] ask with or without a `?` — is a stop, and every stop
/// gets `answer_text` ([`RULE_ANSWER`]); a recommendation with an offer to
/// start is an offer (`keep going`); `your call` stays a stop, answered too.
/// Under `answer_questions = false` each is escalated with its reason.
#[test]
fn a_choice_asked_in_prose_is_answered() {
    let now = t0();
    for (said, why) in [
        (
            "Should I rewrite the parser or patch the lexer?",
            "the worker asks to choose between options",
        ),
        (
            "Two ways forward:\n1. rewrite the parser\n2. patch the lexer\nWhich one?",
            "the worker listed options with no recommendation",
        ),
        (
            "Created a.txt. Should I also create b.txt, or stop here?",
            "the worker asks to choose between options",
        ),
        (
            "Both builds are green. Which would you prefer: the arena or the slab allocator?",
            "the worker said \"which would you prefer\"",
        ),
        // A choice phrase asks without a `?`.
        (
            "Let me know which layout you want for the sidebar.",
            "the worker said \"let me know which\"",
        ),
        (
            "It's your call: rewrite the parser or patch the lexer?",
            "the worker said \"your call\"",
        ),
    ] {
        assert_eq!(
            classify_said(Some(said)),
            Said::Stop(why.to_string()),
            "{said}"
        );
        let mut st = TurnEndState::default();
        assert_eq!(
            at(&mut st, &idle(said, Some(3 * MIN)), now),
            answered(),
            "{said}"
        );
        let mut st = TurnEndState::default();
        assert_eq!(
            at_under(&mut st, &idle(said, Some(3 * MIN)), now, &no_answers()),
            TurnEndAction::Escalate {
                reason: format!("{why}; answer_questions is off")
            },
            "{said}"
        );
    }
    // A recommendation is the worker's to take: a question, answered; with
    // an offer to start, an offer, continued.
    let said = "Two ways forward:\n1. rewrite the parser\n2. patch the lexer\nI recommend 1. \
                Which one do you want?";
    assert_eq!(classify_said(Some(said)), Said::Question);
    assert_eq!(
        at(
            &mut TurnEndState::default(),
            &idle(said, Some(3 * MIN)),
            now
        ),
        answered()
    );
    let said = "Two ways forward:\n1. rewrite the parser\n2. patch the lexer\nI recommend 1; \
                shall I start?";
    assert_eq!(classify_said(Some(said)), Said::Offer);
    assert_eq!(
        at(
            &mut TurnEndState::default(),
            &idle(said, Some(3 * MIN)),
            now
        ),
        typed(RULE_CONTINUE)
    );
}

/// A choice any of whose options — on the option row, on its DESCRIPTION
/// row, in the ask, or in the report before it — names a destructive act,
/// in any inflection, is a stop that NAMES the act (the adversarial review
/// of 2026-09-25: `1. Clean slate⏎   Delete build/ and target/⏎2.
/// Incremental⏎Which do you prefer?` was answered as a plain choice). That
/// holds for a recommendation with an offer to start too. NEGATIVE
/// CONTROLS: the same shapes with harmless words name no act.
#[test]
fn a_destructive_act_anywhere_in_a_choice_is_named() {
    let now = t0();
    for (said, w) in [
        (
            "Two options:\n1. Clean slate\n   Delete build/ and target/, then rebuild.\n2. \
             Incremental\n   Keep the caches.\nWhich do you prefer?",
            "delete",
        ),
        (
            "1. Fresh start (removes node_modules)\n2. Keep it\nWhich one?",
            "remove",
        ),
        (
            "1. Clean slate — wipes build/\n2. Keep\nWhich do you prefer?",
            "wipe",
        ),
        (
            "Two ways forward:\n1. Rebuild\n   The old table is dropped first.\n2. Patch in \
             place\nI recommend 1; shall I start?",
            "drop",
        ),
        (
            "Two ways forward:\n1. drop the legacy table\n2. keep it read-only\nI recommend 2; \
             shall I start?",
            "drop",
        ),
        // No option row: the act in the sentence before the ask.
        (
            "I can wipe the cache and rebuild, or patch in place. Which do you prefer?",
            "wipe",
        ),
        (
            "Let me know which you prefer:\n- reset the branch to main\n- open a new PR",
            "reset",
        ),
        (
            "Let me know which you prefer:\n- force-push the rebased branch\n- open a new PR",
            "force-push",
        ),
    ] {
        assert_eq!(
            classify_said(Some(said)),
            Said::Irreversible(format!("the worker offers a choice that would {w}")),
            "{said}"
        );
        let mut st = TurnEndState::default();
        assert_eq!(
            at(&mut st, &idle(said, Some(3 * MIN)), now),
            answered_reversibly(),
            "{said}"
        );
    }
    // A phrase that does not ask, followed by an act: still the act's.
    assert!(matches!(
        classify_said(Some(
            "Documented which option removes the cache. All tests pass."
        )),
        Said::Irreversible(why) if why.contains("whether to remove")
    ));
    // Negative controls.
    for said in [
        "Two options:\n1. Clean slate\n   Rebuild from scratch.\n2. Incremental\n   Keep the \
         caches.\nWhich do you prefer?",
        "1. Fresh start (a new lockfile)\n2. Keep it\nWhich one?",
        "Let me know which you prefer:\n- rebase the branch\n- open a new PR",
    ] {
        let got = classify_said(Some(said));
        assert!(
            matches!(&got, Said::Stop(why) if !why.contains("would")),
            "{said}: {got:?}"
        );
    }
}

/// D1: the reversible-only answer is not the owner's `answer_text` — which
/// may say "take the option you would recommend", and the recommendation may
/// be the act — but it carries the standing rules as every answer does.
/// NEGATIVE CONTROL: a choice that names no act gets the owner's text.
#[test]
fn an_irreversible_decision_never_gets_the_owners_answer_text() {
    let now = t0();
    let owners = SupervisorConfig {
        answer_text: "Go with option 1.".to_string(),
        ..cfg()
    };
    let mut reading = idle(
        "1. Clean slate — wipes build/\n2. Keep\nWhich do you prefer?",
        Some(3 * MIN),
    );
    reading.rules = Some("commit after each stage".to_string());
    assert_eq!(
        at_under(&mut TurnEndState::default(), &reading, now, &owners),
        TurnEndAction::Type {
            text: format!("{REVERSIBLE_ANSWER} (standing rules: commit after each stage)"),
            rule_id: RULE_ANSWER,
        }
    );
    let harmless = idle(
        "1. the arena\n2. the slab\nWhich do you prefer?",
        Some(3 * MIN),
    );
    assert_eq!(
        at_under(&mut TurnEndState::default(), &harmless, now, &owners),
        TurnEndAction::Type {
            text: "Go with option 1.".to_string(),
            rule_id: RULE_ANSWER,
        }
    );
}

/// D1 for a STOP PHRASE: a request for a go-ahead or a sign-off that names a
/// destructive act asks consent to that act, and the owner's `answer_text`
/// ("take the option you would recommend … keep going") would give it — the
/// stop phrase used to win before any act was looked for. Each is answered
/// with [`REVERSIBLE_ANSWER`], and escalated under `answer_questions =
/// false` as every stop is. NEGATIVE CONTROL: a stop that names no act still
/// gets the owner's text.
#[test]
fn a_stop_that_names_a_destructive_act_is_answered_reversibly() {
    let now = t0();
    for said in [
        "I'll wait for your go-ahead before force-pushing.",
        "The rebase is clean. Blocked on your approval to drop the legacy table.",
        "Everything is staged; before I delete the old release branches, please confirm.",
        "Waiting on you: shall I wipe build/ and start over?",
    ] {
        let got = classify_said(Some(said));
        assert!(
            matches!(&got, Said::Irreversible(why) if why.starts_with("the worker said")),
            "{said}: {got:?}"
        );
        assert_eq!(
            at(
                &mut TurnEndState::default(),
                &idle(said, Some(3 * MIN)),
                now
            ),
            answered_reversibly(),
            "{said}"
        );
        assert!(
            matches!(
                at_under(
                    &mut TurnEndState::default(),
                    &idle(said, Some(3 * MIN)),
                    now,
                    &no_answers()
                ),
                TurnEndAction::Escalate { .. }
            ),
            "{said}"
        );
    }
    for said in [
        "Tests pass; before touching the shared database I need your decision on the backup.",
        "The migration is written. Please sign off before I run it.",
    ] {
        assert!(matches!(classify_said(Some(said)), Said::Stop(_)), "{said}");
        assert_eq!(
            at(
                &mut TurnEndState::default(),
                &idle(said, Some(3 * MIN)),
                now
            ),
            answered(),
            "{said}"
        );
    }
}

/// [`destructive_word`] matches a destructive verb in any regular
/// inflection — and names its base form — and nothing that merely starts
/// with one. NEGATIVE CONTROLS: `dropdown`, `deployment`, `reformat`.
#[test]
fn destructive_words_match_their_inflections() {
    for (text, w) in [
        ("it deletes the rows", "delete"),
        ("the rows are deleted", "delete"),
        ("the table is dropped", "drop"),
        ("dropping it", "drop"),
        ("drops the legacy table", "drop"),
        ("removes node_modules", "remove"),
        ("wiping build/", "wipe"),
        ("wiped", "wipe"),
        ("resetting the branch", "reset"),
        ("the branch is force-pushed", "force-push"),
        ("the file is overwritten", "overwrite"),
        ("it overwrote the file", "overwrite"),
        ("this rewrote history", "rewrite history"),
        ("purges the cache", "purge"),
        ("destroys the volume", "destroy"),
        ("truncated the log", "truncate"),
        ("migrates the schema", "migrate"),
        ("deploys to prod", "deploy"),
        ("publishes the crate", "publish"),
        ("uninstalls it", "uninstall"),
        ("reverted the merge", "revert"),
        ("a full removal of the cache", "removal"),
        ("the deletion of stale branches", "deletion"),
    ] {
        assert_eq!(destructive_word(text), Some(w), "{text}");
    }
    for text in [
        "a dropdown menu",
        "the deployment guide",
        "reformat the file",
        "rename the table",
        "the migration tests pass",
    ] {
        assert_eq!(destructive_word(text), None, "{text}");
    }
}

/// A choice phrase counts where it ASKS, an either/or where the worker would
/// act on it, listed options only under an ask to choose (the critique of
/// 2026-09-25): a report that says `which option` is plain, continued; a
/// question only the person can answer is a question — answered, as every
/// question is, with its own reason. NEGATIVE CONTROLS: the choice shapes
/// are stops.
#[test]
fn a_report_or_a_question_only_the_person_can_answer_is_no_choice() {
    let now = t0();
    let report = "Added a table documenting which option each flag maps to. All tests pass.";
    assert_eq!(classify_said(Some(report)), Said::Plain);
    assert_eq!(
        at(
            &mut TurnEndState::default(),
            &idle(report, Some(3 * MIN)),
            now
        ),
        typed(RULE_CONTINUE)
    );
    for said in [
        "Did the suite pass on your machine, or should I rerun it?",
        "Rebuilt the index:\n- 12 files\n- 3 crates\nDid the suite pass on your machine?",
        "Is the flake on CI or only local?",
    ] {
        assert_eq!(classify_said(Some(said)), Said::Question, "{said}");
        assert_eq!(
            at_under(
                &mut TurnEndState::default(),
                &idle(said, Some(3 * MIN)),
                now,
                &no_answers()
            ),
            TurnEndAction::Escalate {
                reason: "the worker asked a question; answer_questions is off".to_string()
            },
            "{said}"
        );
    }
    for said in [
        "Which do you prefer?\n1. the arena\n2. the slab",
        "Let me know which you prefer:\n1. the arena\n2. the slab",
        "Both work, so should I rewrite the parser or patch the lexer?",
        "1. rewrite the parser\n2. patch the lexer\n1 or 2?",
    ] {
        assert!(
            matches!(classify_said(Some(said)), Said::Stop(_)),
            "{said}: {:?}",
            classify_said(Some(said))
        );
    }
}

/// A report of finished work that asks only what comes next (`Done. I've
/// created hello.txt … What's next?`, the live session of 2026-09-25) says
/// the worker is DONE ([`says_done`]): the point is answered, and its yield
/// is short, so the next act waits on the back-off. NEGATIVE CONTROL: a
/// question about the work is no done report.
#[test]
fn whats_next_after_finished_work_is_a_done_report() {
    for said in [
        "Done. I've created hello.txt with the greeting. What's next?",
        "The migration is in and the suite is green. Anything else?",
    ] {
        assert!(says_done(Some(said)), "{said}");
    }
    assert!(!says_done(Some("Should I also update the docs?")));
    assert!(!says_done(Some(
        "Next I will check what's next in the plan."
    )));
}

/// Claude Code's critical-memory banner at a point (2026-09-24) is ANSWERED
/// by its remedy (D3): the host restarts the agent — `Restart::Memory`, rule
/// `memory-restart@v1` — every time it shows, the escalation with its remedy
/// kept as the reason should the restart not be made; and it is never typed
/// at: no `/compact`, no retry, no continuation; and nothing that holds a
/// TYPED act back holds it back — a draft (the incident's person had one,
/// quoting the banner), a message still unanswered, the survey, a spinner
/// still running (the incident's was 36 minutes into its turn), an act of
/// ours still awaited. NEGATIVE CONTROLS: `[harness] relaunch = false` limits
/// it to the escalation; a turn a person stopped is theirs for the grace;
/// the same point with no wall is continued.
#[test]
fn a_memory_wall_restarts_the_agent_and_never_types() {
    let t = t0();
    let banner = format!(
        "{} (140.4GB) \u{2014} restart and resume with claude --continue",
        aterm_phase::anchor_text("wall.memory")
    );
    let restart = "memory critical: restart it, then resume with claude --continue";
    let is_restart = |a: &TurnEndAction| {
        matches!(
            a,
            TurnEndAction::Restart {
                why: Restart::Memory,
                rule_id: RULE_MEMORY_RESTART,
                otherwise,
            } if is_escalate(otherwise, restart) && is_escalate(otherwise, "140.4GB")
        )
    };
    let base = walled(WallKind::Memory, &banner, Some(3 * MIN));
    let mut st = TurnEndState::default();
    for step in 0..3 {
        let a = at(&mut st, &base, t + step * MIN);
        assert!(is_restart(&a), "{a:?}");
    }
    let no_relaunch = SupervisorConfig {
        relaunch: false,
        ..cfg()
    };
    let a = at_under(&mut TurnEndState::default(), &base, t, &no_relaunch);
    assert!(
        is_escalate(&a, restart) && is_escalate(&a, "relaunch is off"),
        "{a:?}"
    );
    // No host restarts it here (`drive watch`): its escalation, at once.
    let bare = TurnEndReading {
        restartable: false,
        ..base.clone()
    };
    let a = at(&mut TurnEndState::default(), &bare, t);
    assert!(is_escalate(&a, restart), "{a:?}");
    for (name, r) in [
        (
            "a draft",
            TurnEndReading {
                composer: Composer::Typed,
                ..base.clone()
            },
        ),
        (
            "an unanswered message",
            TurnEndReading {
                pending_input: true,
                ..base.clone()
            },
        ),
        (
            "the survey",
            TurnEndReading {
                survey: true,
                ..base.clone()
            },
        ),
        (
            "a running spinner",
            TurnEndReading {
                phase: Phase::Busy,
                ..base.clone()
            },
        ),
    ] {
        let a = at(&mut TurnEndState::default(), &r, t);
        assert!(is_restart(&a), "{name}: {a:?}");
    }
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &idle("Fixed the parser.", Some(3 * MIN)), t),
        typed(RULE_CONTINUE)
    );
    let next = TurnEndReading {
        worked: None,
        ..base.clone()
    };
    let a = at(&mut st, &next, t + Duration::from_secs(5));
    assert!(is_restart(&a), "an act awaited: {a:?}");
    // Negative controls.
    let stopped = TurnEndReading {
        interrupted: true,
        ..base.clone()
    };
    assert!(matches!(
        at(&mut TurnEndState::default(), &stopped, t),
        TurnEndAction::WaitUntil { .. }
    ));
    assert_eq!(
        at(
            &mut TurnEndState::default(),
            &idle("Suites are running.", Some(3 * MIN)),
            t
        ),
        typed(RULE_CONTINUE),
        "the control"
    );
}

/// R12 of the question-answer critique (2026-09-25): a person's Esc on the
/// question dialog, or its review's `2. Cancel`, leaves `⏺ User declined to
/// answer questions` and no `Interrupted ·` row (S8-02, S7-03). It reads as a
/// person's stop, joining Esc's family: held for `human_grace_s` from the
/// point, then continued — nobody is left at the keyboard. Negative control:
/// the same screen with that row as the worker's own words is continued at
/// once.
#[test]
fn a_persons_declined_question_is_held_for_the_grace() {
    use aterm_phase::prompt::fixtures::{QUESTION_DECLINED_CANCEL, QUESTION_DECLINED_ESC};
    let grace = Duration::from_secs(u64::from(cfg().human_grace_s));
    let now = t0();
    let decide = |rows: &[String]| {
        let reading = aterm_phase::read(Some("claude"), rows, Some(2));
        let r = TurnEndReading::of(&reading, rows, false, Some(5 * MIN), None, None);
        let mut st = TurnEndState::default();
        let first = at(&mut st, &r, now);
        let again = TurnEndReading {
            worked: None,
            ..r.clone()
        };
        (r.interrupted, first, at(&mut st, &again, now + grace))
    };
    for text in [QUESTION_DECLINED_ESC, QUESTION_DECLINED_CANCEL] {
        let rows = screen(text);
        assert!(
            rows.iter()
                .any(|r| r.starts_with('⏺') && r.ends_with("User declined to answer questions")),
            "PRECONDITION: the declined row"
        );
        let (read, first, after) = decide(&rows);
        assert!(read, "a person's decline is a stop");
        assert!(waits_until(&first, now + grace), "{first:?}");
        assert!(
            after.rule_id().is_some(),
            "the grace over: acted: {after:?}"
        );
    }
    let said: Vec<String> = screen(QUESTION_DECLINED_ESC)
        .iter()
        .map(|r| {
            if r.starts_with('⏺') && r.ends_with("User declined to answer questions") {
                "⏺ Updated the parser and its tests.".to_string()
            } else {
                r.clone()
            }
        })
        .collect();
    let (read, first, _) = decide(&said);
    assert!(!read);
    assert!(
        first.rule_id().is_some(),
        "the control acts at once: {first:?}"
    );
}
