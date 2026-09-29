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
        resume: None,
        upgrading: false,
        taskless: false,
        person: None,
        reach: Default::default(),
        login_back: false,
        program: aterm_phase::Program::Claude,
        goal: None,
        model_field: None,
        limits: Default::default(),
        sandbox_fell: None,
        thread_model: None,
        upgrade_goal: false,
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

/// A wall's act as typed: the vendor's line quoted ([`wall_retry_text`]).
fn retried(rule: &'static str, cause: ApiCause, r: &TurnEndReading) -> TurnEndAction {
    TurnEndAction::Type {
        text: wall_retry_text(rule, cause, &r.wall_message),
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
    assert_eq!(
        act(&mut st, &same, t + MIN),
        retried(RULE_API_RETRY, ApiCause::Server, &same)
    );
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
    assert_eq!(
        act(&mut st, &same, again + 5 * MIN),
        retried(RULE_API_RETRY, ApiCause::Server, &same)
    );
    let third = again + 6 * MIN;
    assert!(matches!(
        at(&mut st, &back, third),
        TurnEndAction::WaitUntil { until, .. } if until == third + 15 * MIN
    ));
    assert_eq!(
        act(&mut st, &same, third + 15 * MIN),
        retried(RULE_API_RETRY, ApiCause::Server, &same)
    );
    let mut next = third + 16 * MIN;
    for wait in [30, 60, 60, 60] {
        assert!(
            waits_until(&at(&mut st, &back, next), next + wait * MIN),
            "{wait} min"
        );
        assert_eq!(
            act(&mut st, &same, next + wait * MIN),
            retried(RULE_API_RETRY, ApiCause::Server, &same)
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
            cause: aterm_phase::ApiCause::Server,
        },
        "API Error: 400 bad request",
        Some(3 * MIN),
    );
    assert!(waits_until(&at(&mut st, &r, t), t + MIN));
    let same = TurnEndReading {
        worked: None,
        ..r.clone()
    };
    assert_eq!(
        at(&mut st, &same, t + MIN),
        retried(RULE_API_RETRY, ApiCause::Server, &same)
    );
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

/// A POINT THE UPGRADE OWNS IS NEVER CONTINUED AT THE GRACE'S LAPSE (the
/// 2026-09-27 20:28:42 continuation over s-d3346's READY: the person's grace
/// ran out while the upgrade's attended wait owned nothing, and this policy
/// typed `keep going`). **TIER-1 FOR `UpgradeAttendedTurnEnd`'s
/// `LoopContinues`**: at a point the host has looked at, the real policy types
/// its continuation exactly where the model's `LoopContinues` is enabled — a
/// person's grace running holds it (`WaitUntil`, nothing typed), and so does a
/// host that owns the point; only a lapsed grace the host does not own is
/// continued. NEGATIVE CONTROL: the lapse the host does not own IS continued —
/// the fix is the host's ownership, never this policy going quiet.
#[test]
fn a_point_the_upgrade_owns_is_never_continued_at_the_grace_lapse() {
    let model = aterm_spec::derive::upgrade_attended_turn_end_model();
    let grace = Duration::from_secs(u64::from(cfg().human_grace_s));
    for (owns, ago, why) in [
        (
            true,
            grace + Duration::from_secs(1),
            "lapsed, the upgrade's",
        ),
        (
            false,
            grace + Duration::from_secs(1),
            "lapsed, nobody's: continued",
        ),
        (
            true,
            Duration::from_secs(45),
            "the person's, and the upgrade's",
        ),
        (false, Duration::from_secs(45), "the person's"),
    ] {
        let r = TurnEndReading {
            upgrading: owns,
            person: Some(ago),
            ..idle("Nothing of mine is running.", Some(10 * MIN))
        };
        let mut st = TurnEndState::default();
        let action = at(&mut st, &r, t0());
        let mut m = model.init_state();
        m.insert("point", 1);
        m.insert("looked", 1);
        m.insert("owns", i64::from(owns));
        m.insert("person", i64::from(ago < grace));
        assert_eq!(
            action.rule_id().is_some(),
            model.action_enabled("LoopContinues", &m),
            "{why}: the policy's {action:?} against the model at {m:?}"
        );
        if !owns && ago >= grace {
            assert_eq!(action, typed(RULE_CONTINUE), "{why}");
        }
    }
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
                cause: aterm_phase::ApiCause::Server,
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
    assert_eq!(
        act(&mut st, &same, t + MIN),
        retried(RULE_API_RETRY, ApiCause::Server, &same)
    );
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

/// The done check, as typed.
fn checked() -> TurnEndAction {
    TurnEndAction::Type {
        text: DONE_CHECK.to_string(),
        rule_id: RULE_DONE_CHECK,
    }
}

/// THE HAZARDS REVIEW OF 2026-09-25 (major): a worker that answers every
/// continuation by re-running its checks for a few minutes and saying it is
/// done was continued AT ONCE, for ever — real work reset the streak — and
/// spent the owner's usage on busywork. A yield that ends DONE ([`says_done`])
/// is short whatever it worked: the next act waits the back-off. And that
/// act is no `keep going` (decided 2026-09-27 under the owner's standing
/// direction): it is the DONE CHECK, and the check's own yield saying done
/// ENDS THE TASK — nothing is typed after it, a day later either. NEGATIVE
/// CONTROL: the same work reported as progress ends the streak and is
/// continued at once, and a check answered with progress is continued.
#[test]
fn a_done_report_gets_the_done_check_and_a_second_ends_the_task() {
    let done = "Re-ran the whole suite: everything is already done; nothing left to do.";
    let mut now = t0();
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &idle("Stage 1 landed.", Some(3 * MIN)), now),
        typed(RULE_CONTINUE)
    );
    // The continuation's yield says done: backed off, then checked.
    now += 3 * MIN + Duration::from_secs(5);
    let point = idle(done, Some(3 * MIN + Duration::from_secs(5)));
    let a = at(&mut st, &point, now);
    let TurnEndAction::WaitUntil { until, .. } = a else {
        panic!("a done yield is backed off: {a:?}");
    };
    assert_eq!((until - now).as_secs() / 60, 2);
    assert_eq!(st.done_reports(), 1);
    let same = TurnEndReading {
        worked: None,
        ..point
    };
    now = until;
    assert_eq!(act(&mut st, &same, now), checked());
    // The check's yield: `DONE`. The task is done — nothing, now or a day on.
    now += MIN;
    let a = at(&mut st, &idle("DONE", Some(Duration::from_secs(4))), now);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.task_done());
    for later in [2 * MIN, 60 * MIN, 24 * 60 * MIN] {
        let a = at(&mut st, &idle("DONE", None), now + later);
        assert_eq!(a, TurnEndAction::Nothing, "{later:?}");
    }
    // NEGATIVE CONTROL: the same three minutes reported as progress.
    let mut st = TurnEndState::default();
    let mut now = t0();
    act(&mut st, &idle("Stage 1 landed.", Some(3 * MIN)), now);
    now += 3 * MIN;
    assert_eq!(
        act(
            &mut st,
            &idle("Stage 2 landed; stage 3 next.", Some(3 * MIN)),
            now
        ),
        typed(RULE_CONTINUE)
    );
    assert_eq!((st.short_streak(), st.done_reports()), (0, 0));
    assert!(says_done(Some("What would you like me to work on?")));
    assert!(!says_done(Some("Stage 3 is next.")));
}

/// The check answered with progress: the worker was not done, and is
/// continued as any worker is — the check's next done report is a first one
/// again, never the end. NEGATIVE CONTROL: answered `DONE.`, the task ends.
#[test]
fn a_done_check_answered_with_progress_is_continued() {
    let mut now = t0();
    for (reply, ends) in [
        (
            "Not yet: the lexer tests still fail. Fixing them now.",
            false,
        ),
        ("DONE.", true),
        ("Done.", false),
    ] {
        let mut st = TurnEndState::default();
        st.observe(&idle("All done.", Some(3 * MIN)), now);
        let wait = st.short_wait().unwrap_or_default();
        now += wait;
        assert_eq!(act(&mut st, &idle("All done.", None), now), checked());
        now += 4 * MIN;
        let a = at(&mut st, &idle(reply, Some(4 * MIN)), now);
        assert_eq!(st.task_done(), ends, "{reply}");
        if ends {
            assert_eq!(a, TurnEndAction::Nothing);
        } else {
            assert_eq!(a, typed(RULE_CONTINUE), "{reply}");
        }
    }
}

/// A done task is opened again by SOMEONE ELSE'S turn — a person's or an
/// orchestrator's prompt, answered — and its done report is a first one:
/// checked, never ended at once. The harness's own turn (a carry-on the
/// agent had nothing to add to) opens nothing.
#[test]
fn someone_elses_turn_opens_a_done_task_again() {
    let mut now = t0();
    let mut st = TurnEndState::default();
    st.observe(&idle("All done.", Some(3 * MIN)), now);
    now += st.short_wait().unwrap_or_default();
    act(&mut st, &idle("All done.", None), now);
    now += MIN;
    at(&mut st, &idle("DONE", Some(Duration::from_secs(3))), now);
    assert!(st.task_done());
    // The harness's carry-on, answered short and done: still done.
    now += MIN;
    st.host_typed(now);
    let a = at(
        &mut st,
        &idle("Nothing left to do.", Some(Duration::from_secs(3))),
        now,
    );
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.task_done());
    // A person's prompt, answered with work and a done report: checked.
    now += 10 * MIN;
    let a = at(
        &mut st,
        &idle("Added the flag. All done.", Some(5 * MIN)),
        now,
    );
    assert!(!st.task_done());
    assert_eq!(st.done_reports(), 1);
    assert_eq!(a, checked(), "their turn was real work: no back-off");
}

/// THE REVIEW OF 2026-09-27 (major): someone else's turn that ends on a
/// WALL opens a done task again too — the wall's own ladder runs (the 529's
/// retry, the full context's `/compact`), never nothing. The first cut of
/// the check reset its done reports only on a turn that ended clean, so a
/// person's turn that hit a 529 after the task was done was stranded at the
/// wall until they typed again. NEGATIVE CONTROL: the same wall on a fresh
/// state acts the same way.
#[test]
fn someone_elses_turn_that_ends_on_a_wall_is_walled_not_done() {
    for (wall, message) in [
        (WallKind::Overloaded, "API Error: 529 Overloaded."),
        (
            WallKind::Context,
            "Context limit reached · /compact or /clear to continue",
        ),
    ] {
        let mut now = t0();
        let mut st = TurnEndState::default();
        st.observe(&idle("All done.", Some(3 * MIN)), now);
        now += st.short_wait().unwrap_or_default();
        act(&mut st, &idle("All done.", None), now);
        now += MIN;
        at(&mut st, &idle("DONE", Some(Duration::from_secs(3))), now);
        assert!(st.task_done());
        // A person's five-minute turn ends on the wall.
        now += 10 * MIN;
        let r = walled(wall, message, Some(5 * MIN));
        let a = at(&mut st, &r, now);
        assert!(!st.task_done(), "{wall:?}: their turn opened the task");
        let fresh = at(&mut TurnEndState::default(), &r, now);
        assert_eq!(a, fresh, "{wall:?}: the wall acts as on a fresh state");
        if wall == WallKind::Context {
            assert_eq!(
                a,
                TurnEndAction::TypeCommand {
                    command: "/compact".to_string(),
                    rule_id: RULE_COMPACT,
                    then: Then::Continue,
                }
            );
            continue;
        }
        assert_eq!(
            a,
            TurnEndAction::WaitUntil {
                until: now + MIN,
                why: "overloaded retry 1".to_string()
            }
        );
        let same = TurnEndReading {
            worked: None,
            ..r.clone()
        };
        assert_eq!(
            act(&mut st, &same, now + MIN),
            retried(RULE_API_RETRY, ApiCause::Server, &same)
        );
    }
}

/// A draft left standing is SUBMITTED in the check's place — it is a
/// person's words, not the check, so its done yield is a first report
/// again, never the end. And a check the worker never took (its `❯` row
/// still unanswered past the deadline) said nothing: the report stands, and
/// the next act is the check again.
#[test]
fn only_the_check_itself_can_end_the_task() {
    let mut now = t0();
    let mut st = TurnEndState::default();
    st.observe(&idle("All done.", Some(3 * MIN)), now);
    now += st.short_wait().unwrap_or_default();
    let drafted = TurnEndReading {
        composer: Composer::Typed,
        ..idle("All done.", None)
    };
    assert_eq!(
        act(&mut st, &drafted, now),
        TurnEndAction::Submit {
            rule_id: RULE_DONE_CHECK
        }
    );
    now += MIN;
    at(&mut st, &idle("DONE", Some(Duration::from_secs(3))), now);
    assert!(!st.task_done(), "a submitted draft is no check");
    assert_eq!(st.done_reports(), 1);
    // The check typed and never taken: judged at its deadline with the
    // report standing.
    now += st.short_wait().unwrap_or_default();
    assert_eq!(act(&mut st, &idle("DONE", None), now), checked());
    let unanswered = TurnEndReading {
        pending_input: true,
        said_tail: None,
        ..idle("", None)
    };
    now += st.timing.take_within + Duration::from_secs(1);
    at(&mut st, &unanswered, now);
    assert!(!st.task_done());
    assert_eq!(st.done_reports(), 1);
}

/// The owner's limits hold over the check: `continue = false` types none
/// (it is a continuation's words), and `answer_questions = false` still
/// escalates a done report that asks something.
#[test]
fn the_done_check_is_a_continuation_the_owner_can_limit() {
    let now = t0();
    let off = SupervisorConfig {
        continue_policy: false,
        ..cfg()
    };
    let mut st = TurnEndState::default();
    st.observe(&idle("All done.", Some(3 * MIN)), now);
    let later = now + st.short_wait().unwrap_or_default();
    assert_eq!(
        decide_turn_end(&st, &idle("All done.", None), &off, later),
        TurnEndAction::Nothing
    );
    assert_eq!(
        decide_turn_end(&st, &idle("All done.", None), &cfg(), later),
        checked()
    );
    let mut st = TurnEndState::default();
    let asks = "All done. I will wait for your go-ahead on the next stage.";
    st.observe(&idle(asks, Some(3 * MIN)), now);
    assert!(matches!(
        decide_turn_end(&st, &idle(asks, None), &no_answers(), later),
        TurnEndAction::Escalate { .. }
    ));
    // The standing rules ride with the check as with any continuation.
    assert_eq!(
        done_check_text(Some("never push to main")),
        format!("{DONE_CHECK} (standing rules: never push to main)")
    );
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
    // The vendor's banner, word for word — its own remedy tail included.
    let banner = format!(
        "{} (140.4GB) \u{2014} restart and resume with claude --continue",
        aterm_phase::anchor_text("wall.memory")
    );
    // The line the host read for THIS agent's own conversation
    // (`IdleHost::resume_command`, 2026-09-26).
    let resume = "claude --model opus --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
    let restart = format!(
        "memory critical: restart it, then resume with {resume}: {} (140.4GB)",
        aterm_phase::anchor_text("wall.memory")
    );
    let restart = restart.as_str();
    // Never the vendor's `--continue`: it resumes the directory's newest
    // conversation, a sibling tab's where two share it.
    let no_continue = |a: &TurnEndAction| match a {
        TurnEndAction::Escalate { reason } => !reason.contains("--continue"),
        TurnEndAction::Restart { otherwise, .. } => {
            matches!(&**otherwise, TurnEndAction::Escalate { reason } if !reason.contains("--continue"))
        }
        _ => true,
    };
    let is_restart = |a: &TurnEndAction| {
        no_continue(a)
            && matches!(
                a,
                TurnEndAction::Restart {
                    why: Restart::Memory,
                    rule_id: RULE_MEMORY_RESTART,
                    otherwise,
                } if is_escalate(otherwise, restart) && is_escalate(otherwise, "140.4GB")
            )
    };
    let base = TurnEndReading {
        resume: Some(resume.to_string()),
        ..walled(WallKind::Memory, &banner, Some(3 * MIN))
    };
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
    assert!(is_escalate(&a, restart) && no_continue(&a), "{a:?}");
    // No resume line read (no host, or none it could read): the remedy names
    // no command at all — never a guess.
    let unknown = TurnEndReading {
        resume: None,
        ..bare.clone()
    };
    let a = at(&mut TurnEndState::default(), &unknown, t);
    assert!(
        is_escalate(
            &a,
            &format!(
                "memory critical: restart it: {} (140.4GB)",
                aterm_phase::anchor_text("wall.memory")
            )
        ) && no_continue(&a)
            && !is_escalate(&a, "--resume"),
        "{a:?}"
    );
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

const ENOTFOUND: &str =
    "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";

fn api_wall(
    cause: ApiCause,
    message: &str,
    worked: Option<Duration>,
    reach: Reach,
) -> TurnEndReading {
    TurnEndReading {
        reach,
        ..walled(
            WallKind::ApiError {
                code: None,
                retryable: cause != ApiCause::Config,
                cause,
            },
            message,
            worked,
        )
    }
}

/// The point read again with no work since.
fn again(r: &TurnEndReading) -> TurnEndReading {
    TurnEndReading {
        worked: None,
        ..r.clone()
    }
}

/// AN API NEVER REACHED, ITS REACH NOT MEASURED (`drive watch`, a route the
/// host cannot reproduce): tried on the SHORT ladder — 1, 2, 5, then every 5
/// minutes from each appearance — in words that quote the vendor, never
/// `keep going`, and never an ask.
#[test]
fn an_unreachable_wall_is_retried_on_the_short_ladder_while_its_reach_is_unknown() {
    let t = t0();
    let mut st = TurnEndState::default();
    let r = api_wall(
        ApiCause::Unreachable,
        ENOTFOUND,
        Some(3 * MIN),
        Reach::Unknown,
    );
    assert!(waits_until(&at(&mut st, &r, t), t + MIN));
    let same = again(&r);
    let want = retried(RULE_API_RETRY, ApiCause::Unreachable, &same);
    assert_eq!(act(&mut st, &same, t + MIN), want);
    let TurnEndAction::Type { text, .. } = &want else {
        unreachable!()
    };
    assert!(
        text.starts_with("Claude Code reported \"API Error: Can't reach"),
        "{text}"
    );
    // Each try meets the wall again after the vendor's ~3 minutes of retries.
    let mut next = t + MIN;
    for wait in [2, 5, 5, 5] {
        next += 3 * MIN;
        assert!(
            waits_until(&at(&mut st, &r, next), next + wait * MIN),
            "{wait}"
        );
        assert_eq!(act(&mut st, &same, next + wait * MIN), want, "{wait}");
        next += wait * MIN;
    }
}

/// THE MEASURE DECIDES. The host measures the API definitely DOWN: nothing is
/// typed into it for the hold (15 min), then one try all the same — so a
/// wrong measure holds a worker no longer. It measures it reachable again:
/// continued at once, in words that say so; met by the wall again, spaced on
/// the short ladder. NEGATIVE CONTROL: the server's own failure ignores the
/// measure — a 503 keeps its ladder under an Up.
#[test]
fn a_measured_outage_types_nothing_until_the_hold_and_a_reachable_api_at_once() {
    let t = t0();
    let mut st = TurnEndState::default();
    let down = api_wall(
        ApiCause::Unreachable,
        ENOTFOUND,
        Some(3 * MIN),
        Reach::Down { since: t },
    );
    assert!(waits_until(&at(&mut st, &down, t), t + 15 * MIN));
    assert!(waits_until(
        &at(&mut st, &again(&down), t + 14 * MIN),
        t + 15 * MIN
    ));
    assert_eq!(
        act(&mut st, &again(&down), t + 15 * MIN),
        retried(RULE_API_RETRY, ApiCause::Unreachable, &down)
    );
    // The try meets the wall again; the API is measured back at +20 min.
    let met = t + 18 * MIN;
    assert!(waits_until(&at(&mut st, &down, met), met + 15 * MIN));
    let up = TurnEndReading {
        reach: Reach::Up {
            since: met + 2 * MIN,
        },
        ..again(&down)
    };
    let back = retried(RULE_API_BACK, ApiCause::Unreachable, &up);
    assert_eq!(act(&mut st, &up, met + 2 * MIN), back);
    let TurnEndAction::Type { text, .. } = &back else {
        unreachable!()
    };
    assert!(text.contains("the API is reachable again now"), "{text}");
    // Met again under an Up: the measure was wrong, or the API still fails —
    // the next back-act waits the ladder's first rung.
    let later_ = met + 5 * MIN;
    let up_again = TurnEndReading {
        worked: Some(3 * MIN),
        ..up.clone()
    };
    assert!(waits_until(&at(&mut st, &up_again, later_), later_ + MIN));
    // The control: the server's own failure keeps its ladder under an Up.
    let mut st = TurnEndState::default();
    let server = TurnEndReading {
        reach: Reach::Up { since: t },
        ..walled(
            WallKind::ApiError {
                code: Some(503),
                retryable: true,
                cause: ApiCause::Server,
            },
            "API Error: 503 Service unavailable",
            Some(3 * MIN),
        )
    };
    assert!(waits_until(&at(&mut st, &server, t), t + MIN));
}

/// A REPLY CUT OFF (the Mac slept mid-response): continued at once, in words
/// that say it may be incomplete; met again, on the short ladder. Under a
/// measured outage it takes the hold like an unreachable API.
#[test]
fn a_cut_off_reply_is_continued_at_once_then_on_the_short_ladder() {
    let sleep = "API Error: Your computer went to sleep mid-response. The response above may be \
                 incomplete.";
    let t = t0();
    let mut st = TurnEndState::default();
    let r = api_wall(ApiCause::CutOff, sleep, Some(40 * MIN), Reach::Unknown);
    let cut = retried(RULE_API_CUTOFF, ApiCause::CutOff, &r);
    assert_eq!(act(&mut st, &r, t), cut);
    let TurnEndAction::Type { text, .. } = &cut else {
        unreachable!()
    };
    assert!(text.contains("your last reply may be incomplete"), "{text}");
    // Met again after a minute of the vendor's retries: the ladder's first rung.
    let met = t + MIN;
    let quick = api_wall(ApiCause::CutOff, sleep, Some(MIN), Reach::Unknown);
    assert!(waits_until(&at(&mut st, &quick, met), met + MIN));
    let mut st = TurnEndState::default();
    let down = api_wall(
        ApiCause::CutOff,
        sleep,
        Some(3 * MIN),
        Reach::Down { since: t },
    );
    assert!(waits_until(&at(&mut st, &down, t), t + 15 * MIN));
}

/// A CERTIFICATE OR PROXY REFUSAL is never an ask by default (a captive
/// portal and an intercepting proxy clear by themselves): tried on the short
/// ladder. The host's verified handshake is NO evidence it has gone — the
/// host trusts the platform's store, the agent its own — so an `Up` leaves
/// it on the same ladder, in words that never say the API is reachable
/// again (the review of 2026-09-27: an inspecting root only the keychain
/// trusts read Up at once, and "reachable again" was typed on every rung).
/// NEGATIVE CONTROL: the same `Up` at an unreachable wall continues it at
/// once under `api-back@v1`. Only `retry_api_errors = false` escalates it,
/// as every API wall.
#[test]
fn a_certificate_or_proxy_refusal_is_retried_never_asked() {
    let cert = "API Error: Unable to connect to API: Self-signed certificate detected";
    let t = t0();
    let mut st = TurnEndState::default();
    let r = api_wall(ApiCause::Config, cert, Some(MIN), Reach::Unknown);
    assert!(waits_until(&at(&mut st, &r, t), t + MIN));
    assert_eq!(
        act(&mut st, &again(&r), t + MIN),
        retried(RULE_API_RETRY, ApiCause::Config, &r)
    );
    let mut st = TurnEndState::default();
    let up = api_wall(ApiCause::Config, cert, Some(MIN), Reach::Up { since: t });
    assert!(waits_until(&at(&mut st, &up, t), t + MIN), "no act at once");
    let tried = act(&mut st, &again(&up), t + MIN);
    assert_eq!(tried, retried(RULE_API_RETRY, ApiCause::Config, &up));
    let TurnEndAction::Type { text, .. } = &tried else {
        unreachable!()
    };
    assert!(!text.contains("reachable"), "{text}");
    // Met again under the same Up: the ladder's next rung, still a retry.
    let met = t + 2 * MIN;
    let again_up = api_wall(ApiCause::Config, cert, Some(MIN), Reach::Up { since: t });
    assert!(waits_until(&at(&mut st, &again_up, met), met + 2 * MIN));
    // The control: an unreachable wall under the same Up, at once.
    let mut st = TurnEndState::default();
    let back = api_wall(
        ApiCause::Unreachable,
        ENOTFOUND,
        Some(MIN),
        Reach::Up { since: t },
    );
    assert_eq!(
        act(&mut st, &back, t),
        retried(RULE_API_BACK, ApiCause::Unreachable, &back)
    );
    let mut off = cfg();
    off.retry_api_errors = false;
    let mut st = TurnEndState::default();
    st.observe(&r, t);
    assert!(is_escalate(
        &decide_turn_end(&st, &r, &off, t),
        "retry_api_errors is off"
    ));
}

/// A RETRY THAT LED TO REAL WORK ENDS THE EPISODE. The supervisor journal of
/// 2026-09-26: six sleep cut-offs in a row, each continuation working 14 to
/// 63 minutes before the next. Each is a new wall, continued at once — never
/// the sixth rung of one ladder. NEGATIVE CONTROL: a try that met the wall
/// after only the vendor's own retries (3 minutes) climbs the ladder.
#[test]
fn a_retry_that_led_to_real_work_ends_the_episode() {
    let sleep = "API Error: Your computer went to sleep mid-response. The response above may be \
                 incomplete.";
    let t = t0();
    let mut st = TurnEndState::default();
    let mut now = t;
    for worked in [14, 49, 63, 10, 32, 20] {
        let r = api_wall(ApiCause::CutOff, sleep, Some(worked * MIN), Reach::Unknown);
        assert_eq!(
            act(&mut st, &r, now),
            retried(RULE_API_CUTOFF, ApiCause::CutOff, &r),
            "after {worked} min of work"
        );
        now += (worked + 1) * MIN;
    }
    let vendor_only = api_wall(ApiCause::CutOff, sleep, Some(3 * MIN), Reach::Unknown);
    assert!(waits_until(&at(&mut st, &vendor_only, now), now + MIN));
}

/// THE OUTAGE OF 2026-09-27, 18:16–19:15Z. NEGATIVE CONTROL first: read as
/// that day's supervisor read it — no wall — each `ENOTFOUND` after the
/// vendor's ~3 minutes of retries is an ordinary turn end, continued at once
/// with `keep going` (the journal's sixteen `CONTINUED … rule=continue@v1`).
/// Then as it reads now: an unreachable wall the host measures down — tried
/// once per hold, never at every appearance — and continued once, the
/// moment the host measures the API back.
#[test]
fn the_2026_09_27_outage_is_held_then_continued_once_the_api_is_back() {
    let t = t0();
    let unread = TurnEndReading {
        said_tail: Some(ENOTFOUND.to_string()),
        ..idle(ENOTFOUND, Some(3 * MIN))
    };
    let mut st = TurnEndState::default();
    let mut blind = 0;
    for i in 0..14 {
        let now = t + i * 3 * MIN;
        if act(&mut st, &unread, now) == typed(RULE_CONTINUE) {
            blind += 1;
        }
    }
    assert_eq!(blind, 14, "the day's reading typed into every appearance");

    let mut st = TurnEndState::default();
    let down = api_wall(
        ApiCause::Unreachable,
        ENOTFOUND,
        Some(3 * MIN),
        Reach::Down { since: t },
    );
    let mut typed_into_outage = 0;
    let mut now = t;
    let back_at = t + 59 * MIN;
    while now < back_at {
        let a = act(&mut st, &down, now);
        if matches!(a, TurnEndAction::Type { .. }) {
            typed_into_outage += 1;
            now += 3 * MIN; // the vendor's retries, then the wall again
        } else {
            now += MIN;
            let _ = act(&mut st, &again(&down), now);
        }
    }
    assert!(
        typed_into_outage <= 4,
        "one try per 15-minute hold at most, not one per appearance: {typed_into_outage}"
    );
    let up = TurnEndReading {
        reach: Reach::Up { since: back_at },
        ..again(&down)
    };
    assert_eq!(
        act(&mut st, &up, back_at + Duration::from_secs(20)),
        retried(RULE_API_BACK, ApiCause::Unreachable, &up)
    );
}

// --- Codex's save-then-wait switch (2026-09-28) -------------------------------

const MARKER: &str = "ATERM-SAVED-3f9a1c2e";

fn astra() -> CodexSetting {
    CodexSetting {
        model: "GPT-6-Astra".to_string(),
        effort: Some("ultra".to_string()),
    }
}

fn luna() -> CodexSetting {
    CodexSetting {
        model: "GPT-6-Luna".to_string(),
        effort: Some("medium".to_string()),
    }
}

/// A Codex idle point: its footer showing `model` and `goal`.
fn codex_at(
    said: &str,
    worked: Option<Duration>,
    model: CodexSetting,
    goal: Option<CodexGoal>,
) -> TurnEndReading {
    TurnEndReading {
        program: Program::Codex,
        goal,
        model_field: Some(model),
        // Codex never relaunches in its tab.
        restartable: false,
        ..idle(said, worked)
    }
}

/// The open switch moved to `phase`, its first point past (a test's jump).
fn set_phase(st: &mut TurnEndState, phase: WindPhase) {
    let w = st.wind.as_mut().expect("open");
    w.phase = phase;
    w.pre = false;
}

/// A switch opened by the nudge's press at `t`, the goal pursued and
/// stopped by the harness's own Esc, the window back at `back`.
fn switched(t: Instant, back: Option<Instant>) -> TurnEndState {
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), back, MARKER.to_string());
    st.own_esc(t);
    st
}

fn no_continuation(a: &TurnEndAction) -> bool {
    !matches!(
        a.rule_id(),
        Some(RULE_CONTINUE | RULE_SUGGESTION | RULE_ANSWER | RULE_API_RETRY)
    ) && !matches!(a, TurnEndAction::Submit { .. })
}

/// THE INCIDENT, REPLAYED THE OWNER'S WAY (2026-09-28: "switch like that for
/// codex to git commit and push work and then wait for the original model
/// settings, not continue work"): the nudge's switch at 99% opens the
/// switch; the goal turn the harness's own Esc stopped is no person's (no
/// grace, no continuation); the next point types the instruction that saves
/// the work — never `keep going` (the incident typed it 8 s after the
/// press); its answer's marker line is the save; then `/model` puts the
/// thread back on GPT-6-Astra ultra (the approval policy picks it while the
/// restore is in flight); seen back, the session is HELD — nothing typed,
/// woken every 30 minutes — until a minute past the window's reset, where
/// `/goal resume` (the harness paused the goal) ends the switch; and with
/// Codex's goal pursued again, nothing more is typed. NEGATIVE CONTROLS at
/// every step: no continuation, answer or draft submit.
#[test]
fn the_incident_replayed_saves_restores_holds_and_resumes() {
    let t = t0();
    let back = t + 3 * 24 * 60 * MIN;
    let mut st = switched(t, Some(back));
    assert!(st.switch_open());
    // The interrupted goal turn: the harness's own Esc.
    let stopped = TurnEndReading {
        interrupted: true,
        ..codex_at(
            "■ Conversation interrupted - tell the model what to do differently.",
            Some(Duration::from_secs(4)),
            luna(),
            Some(CodexGoal::Paused),
        )
    };
    let a = act(&mut st, &stopped, t + Duration::from_secs(5));
    let TurnEndAction::Type { text, rule_id } = &a else {
        panic!("{a:?}")
    };
    assert_eq!(*rule_id, RULE_WIND_DOWN);
    for words in [
        "GPT-6-Astra is close to its usage limit",
        "gpt-6-luna for one job only: saving your work",
        "git pull --no-rebase",
        "Never force-push, rebase, reset, stash or switch branches",
        MARKER,
        "back on GPT-6-Astra ultra",
    ] {
        assert!(text.contains(words), "{words}: {text}");
    }
    assert!(matches!(
        st.wind().map(|w| w.phase),
        Some(WindPhase::Winding)
    ));
    // Its turn is running: awaited, never continued.
    let early = codex_at("Committing.", None, luna(), Some(CodexGoal::Paused));
    let a = at(&mut st, &early, t + Duration::from_secs(10));
    assert!(matches!(a, TurnEndAction::WaitUntil { .. }), "{a:?}");
    // The wind-down's answer carries the marker: saved; the model is owed.
    let saved = codex_at(
        &format!("Committed and pushed `wip: parser` to main.\n{MARKER}"),
        Some(3 * MIN),
        luna(),
        Some(CodexGoal::Paused),
    );
    let a = act(&mut st, &saved, t + 4 * MIN);
    assert_eq!(
        a,
        TurnEndAction::TypeCommand {
            command: "/model".to_string(),
            rule_id: RULE_MODEL_RESTORE,
            then: Then::Nothing
        }
    );
    assert_eq!(st.wind().and_then(|w| w.saved), Some(true));
    assert_eq!(st.restore_target(t + 4 * MIN), Some(astra()));
    assert_eq!(st.restore_target(t + 7 * MIN), None, "only while in flight");
    let events = st.take_wind_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WindEvent::Edge { what, .. } if what == "saved")),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, WindEvent::Note(_))),
        "{events:?}"
    );
    // The picker answered: the footer shows the thread's own model again.
    let back_on = codex_at(
        "Committed and pushed `wip: parser` to main.",
        None,
        astra(),
        Some(CodexGoal::Paused),
    );
    let a = at(&mut st, &back_on, t + 5 * MIN);
    assert!(st.holding(), "{:?}", st.wind());
    assert!(waits_until(&a, t + 35 * MIN), "{a:?}");
    // Woken on the way: held again, never typed into.
    for h in [1, 24, 71] {
        let now = t + h * 60 * MIN;
        let a = at(&mut st, &back_on, now);
        assert!(
            waits_until(&a, (now + 30 * MIN).min(back + MIN)),
            "{h}h: {a:?}"
        );
    }
    // A minute past the reset: the goal the harness paused is resumed.
    let a = act(&mut st, &back_on, back + MIN);
    assert_eq!(
        a,
        TurnEndAction::TypeCommand {
            command: "/goal resume".to_string(),
            rule_id: RULE_LIMIT_RESUME,
            then: Then::Nothing
        }
    );
    assert!(!st.switch_open());
    let events = st.take_wind_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WindEvent::Typed { phase: "done", .. })),
        "{events:?}"
    );
    // Codex's goal runs again: it goes on by itself, nothing is typed.
    let pursued = codex_at(
        "Stage 4 done.",
        Some(30 * MIN),
        astra(),
        Some(CodexGoal::Pursuing),
    );
    assert_eq!(
        at(&mut st, &pursued, back + 40 * MIN),
        TurnEndAction::Nothing
    );
}

/// No step of an open switch continues, answers or sends a draft: at every
/// phase, with every kind of last words, a draft standing or not, the act is
/// the switch's own or nothing (the incident's eleven `keep going`s, ten of
/// them on luna). NEGATIVE CONTROL: the same point with no switch open is
/// continued.
#[test]
fn an_open_switch_never_continues_answers_or_sends_a_draft() {
    let t = t0();
    let said = [
        "Stage 3 done.",
        "Want me to keep going?",
        "Which do you prefer: 1. arena 2. slab?",
    ];
    for phase in [
        WindPhase::Owed,
        WindPhase::Winding,
        WindPhase::Restore { hold: true },
        WindPhase::Restore { hold: false },
        WindPhase::Holding { since: t },
    ] {
        for words in said {
            for draft in [false, true] {
                let mut st = switched(t, Some(t + 60 * MIN));
                set_phase(&mut st, phase);
                let r = TurnEndReading {
                    composer: if draft {
                        Composer::Typed
                    } else {
                        Composer::Empty
                    },
                    ..codex_at(words, None, luna(), Some(CodexGoal::Paused))
                };
                let a = decide_turn_end(&st, &r, &cfg(), t + 3 * MIN);
                assert!(no_continuation(&a), "{phase:?} {words} {draft}: {a:?}");
                if draft {
                    assert!(
                        !matches!(
                            a,
                            TurnEndAction::Type { .. } | TurnEndAction::TypeCommand { .. }
                        ),
                        "never typed over a draft: {phase:?}: {a:?}"
                    );
                }
            }
        }
    }
    let mut st = TurnEndState::default();
    let r = codex_at("Stage 3 done.", Some(30 * MIN), astra(), None);
    assert_eq!(at(&mut st, &r, t), typed(RULE_CONTINUE), "the control");
}

/// An unsaved wind-down — the sandbox the daemon's restart left the thread
/// in denied `.git/FETCH_HEAD` (2026-09-28, 80 turns of it) — is said ONCE,
/// quoting the agent's last words, and the switch goes on: the model back,
/// then the hold. Never a continuation into it.
#[test]
fn an_unsaved_wind_down_is_said_and_the_switch_goes_on() {
    let t = t0();
    let mut st = switched(t, Some(t + 600 * MIN));
    let stopped = TurnEndReading {
        interrupted: true,
        ..codex_at(
            "■ Conversation interrupted",
            Some(Duration::from_secs(3)),
            luna(),
            None,
        )
    };
    let _ = act(&mut st, &stopped, t);
    let failed = codex_at(
        "I committed locally, but the sandbox denies .git/FETCH_HEAD, so I could not commit or \
         push.",
        Some(2 * MIN),
        luna(),
        None,
    );
    let a = act(&mut st, &failed, t + 3 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "{a:?}");
    let notes: Vec<String> = st
        .take_wind_events()
        .into_iter()
        .filter_map(|e| match e {
            WindEvent::Note(n) => Some(n),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("did not confirm") && notes[0].contains("FETCH_HEAD"),
        "{notes:?}"
    );
    assert_eq!(st.wind().and_then(|w| w.saved), Some(false));
    // Only a line that IS the marker saves: one quoting it is none.
    let mut st = switched(t, None);
    let _ = act(&mut st, &stopped, t);
    let quoting = codex_at(
        &format!("I will reply with {MARKER} once everything is pushed."),
        Some(2 * MIN),
        luna(),
        None,
    );
    let _ = act(&mut st, &quoting, t + 3 * MIN);
    assert_eq!(st.wind().and_then(|w| w.saved), Some(false));
}

/// The wind-down's own turn hits Codex's usage wall — luna shares the window
/// (99 → 100% in 5.5 minutes, 2026-09-27): the wall's reset pushes the hold
/// out, and the wall's own wait-and-continue is not used — the switch owns
/// the resume.
#[test]
fn a_usage_wall_at_the_wind_down_holds_to_its_reset() {
    let t = t0();
    let mut st = switched(t, Some(t + 60 * MIN));
    let stopped = TurnEndReading {
        interrupted: true,
        ..codex_at(
            "■ Conversation interrupted",
            Some(Duration::from_secs(3)),
            luna(),
            None,
        )
    };
    let _ = act(&mut st, &stopped, t);
    let wall = TurnEndReading {
        wall: Some(WallKind::UsageSession),
        wall_message: "You've hit your usage limit.".to_string(),
        reset_at: Some(t + 600 * MIN),
        phase: Phase::Limited {
            message: "You've hit your usage limit.".to_string(),
            reset: None,
        },
        ..codex_at("", Some(MIN), luna(), None)
    };
    let a = act(&mut st, &wall, t + 6 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "{a:?}");
    assert_eq!(st.wind().and_then(|w| w.back_at), Some(t + 600 * MIN));
}

/// A goal the harness could not stop with its Esc (no turn was running) is
/// paused with `/goal pause` — each one of the switch's [`GOAL_STOPS`]
/// stops; still pursued after them, nothing more is typed (the loop's busy
/// read tells a person, [`TurnEndState::goal_stop`]). The save instruction
/// is never typed while Codex's goal would carry on after it.
#[test]
fn a_goal_still_pursued_is_paused_up_to_its_stops_then_left() {
    let t = t0();
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
    let running = codex_at("Stage done.", Some(MIN), luna(), Some(CodexGoal::Pursuing));
    let pause = TurnEndAction::TypeCommand {
        command: "/goal pause".to_string(),
        rule_id: RULE_WIND_DOWN,
        then: Then::Nothing,
    };
    assert_eq!(act(&mut st, &running, t), pause);
    assert!(
        st.wind()
            .is_some_and(|w| w.goal_paused && w.pause_typed && w.stops == 1)
    );
    let still = codex_at("Stage done.", None, luna(), Some(CodexGoal::Pursuing));
    for k in 2..=GOAL_STOPS {
        let now = t + k * MIN;
        assert_eq!(act(&mut st, &still, now), pause, "stop {k}");
    }
    assert_eq!(st.wind().map(|w| w.stops), Some(GOAL_STOPS));
    // Still pursued with the stops spent: nothing typed, a person told —
    // once: the goal's next turn, read busy, says nothing again.
    assert_eq!(at(&mut st, &still, t + 10 * MIN), TurnEndAction::Nothing);
    assert_eq!(
        st.goal_stop(
            Some(Duration::ZERO),
            Some(CodexGoal::Pursuing),
            false,
            t + 11 * MIN
        ),
        None
    );
    let events = st.take_wind_events();
    assert!(
        events.iter().any(
            |e| matches!(e, WindEvent::GoalNote(n) if n.contains("keeps running on gpt-6-luna"))
        ),
        "{events:?}"
    );
    // Paused: the save instruction goes.
    let paused = codex_at("Stage done.", None, luna(), Some(CodexGoal::Paused));
    assert_eq!(
        at(&mut st, &paused, t + 13 * MIN).rule_id(),
        Some(RULE_WIND_DOWN)
    );
}

/// A hold whose reset nobody read lasts at most `unknown_hold` (5 h) from
/// its start, woken every 30 minutes; the usage reading dropping under its
/// limit ends it early (the owner's reset, or the window rolled over). With
/// no goal the harness paused, the resume is the carry-on naming the model.
#[test]
fn a_hold_ends_at_its_bound_or_when_the_window_reads_far() {
    let t = t0();
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
    set_phase(&mut st, WindPhase::Holding { since: t });
    let held = codex_at("Pushed.", None, astra(), None);
    assert!(waits_until(
        &decide_turn_end(&st, &held, &cfg(), t),
        t + 30 * MIN
    ));
    let a = decide_turn_end(&st, &held, &cfg(), t + 5 * 60 * MIN);
    let TurnEndAction::Type { text, rule_id } = &a else {
        panic!("{a:?}")
    };
    assert_eq!(*rule_id, RULE_LIMIT_RESUME);
    assert!(
        text.contains("GPT-6-Astra's usage limit has reset") && text.contains("GPT-6-Astra ultra"),
        "{text}"
    );
    let far = TurnEndReading {
        limits: LimitRead::Far { used: 0 },
        ..held.clone()
    };
    assert_eq!(
        decide_turn_end(&st, &far, &cfg(), t + MIN).rule_id(),
        Some(RULE_LIMIT_RESUME)
    );
    // Never into a thread fallen into a sandbox.
    let sandboxed = TurnEndReading {
        sandbox_fell: Some("workspace-write".to_string()),
        ..far
    };
    assert_eq!(
        decide_turn_end(&st, &sandboxed, &cfg(), t + MIN),
        TurnEndAction::Nothing
    );
}

/// A GOAL THE LIVE UPGRADE PAUSED, HELD OVER BY THE SWITCH, IS RESUMED AT
/// THE RESET (the goal-pause review of 2026-09-28): a switch that opened
/// over the upgrade's pause did not pause the goal itself, so at its reset it
/// typed its carry-on — a turn with the goal left paused — while the
/// upgrade's own resume waited on the switch, and the upgrade's row, 1.5 h
/// on, told a person to resume the goal before the reset. Now the reset
/// types `/goal resume` (the loop puts it on the upgrade's record as the
/// upgrade's resume, made by the switch), and closes the switch. NEGATIVE
/// CONTROL: with no hold of the upgrade's, the carry-on, as before.
#[test]
fn a_goal_the_upgrade_paused_is_resumed_at_the_switchs_reset() {
    let t = t0();
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
    set_phase(&mut st, WindPhase::Holding { since: t });
    let paused = codex_at("Pushed.", None, astra(), Some(CodexGoal::Paused));
    let held = TurnEndReading {
        upgrade_goal: true,
        ..paused.clone()
    };
    assert!(waits_until(
        &decide_turn_end(&st, &held, &cfg(), t),
        t + 30 * MIN
    ));
    assert_eq!(
        decide_turn_end(&st, &held, &cfg(), t + 5 * 60 * MIN),
        TurnEndAction::TypeCommand {
            command: "/goal resume".to_string(),
            rule_id: RULE_LIMIT_RESUME,
            then: Then::Nothing
        }
    );
    let a = decide_turn_end(&st, &paused, &cfg(), t + 5 * 60 * MIN);
    assert!(
        matches!(&a, TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_LIMIT_RESUME),
        "{a:?}"
    );
    // Never into a thread fallen into a sandbox, the upgrade's pause or not.
    let sandboxed = TurnEndReading {
        sandbox_fell: Some("workspace-write".to_string()),
        ..held
    };
    assert_eq!(
        decide_turn_end(&st, &sandboxed, &cfg(), t + 5 * 60 * MIN),
        TurnEndAction::Nothing
    );
}

/// A PAUSED GOAL'S TURN END IS THE SUPERVISOR'S ONLY WHERE THE HOST OWNS
/// NOTHING (the goal-pause review of 2026-09-28, its probe made real): a
/// Codex point whose goal the live upgrade paused for its move is continued
/// by the policy when the host does not own the point — which is why every
/// wait the upgrade words at a held goal is `goal-held`, which owns it
/// (`upgrade_codex_drive::goal_wait_word`) — and, owned, nothing is typed.
#[test]
fn a_paused_goals_turn_end_is_the_hosts_while_it_owns_it() {
    let t = t0();
    let r = codex_at(
        "The last turn made concrete progress. I'm now tracing the next missing link.",
        Some(30 * MIN),
        astra(),
        Some(CodexGoal::Paused),
    );
    let st = TurnEndState::default();
    assert_eq!(
        decide_turn_end(&st, &r, &cfg(), t).rule_id(),
        Some(RULE_CONTINUE),
        "unowned, the policy continues it"
    );
    let owned = TurnEndReading {
        upgrading: true,
        ..r
    };
    assert_eq!(
        decide_turn_end(&st, &owned, &cfg(), t),
        TurnEndAction::Nothing
    );
}

/// A PERSON'S HAND WINS: their own message during the hold ends it for good
/// (no `/goal resume` of the harness's after it); before the wind-down it
/// leaves the model owed back and nothing held; their own `/model` to
/// another model during the hold ends it too. NEGATIVE CONTROL: the thread
/// seen on the cheaper model again during the hold is restored again.
#[test]
fn a_persons_hand_releases_the_switch() {
    let t = t0();
    let mut st = switched(t, Some(t + 600 * MIN));
    set_phase(&mut st, WindPhase::Holding { since: t });
    let theirs = codex_at("Here is the answer.", Some(4 * MIN), astra(), None);
    let a = at(&mut st, &theirs, t + MIN);
    assert!(!st.switch_open());
    assert_ne!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{a:?}");
    let events = st.take_wind_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            WindEvent::Edge {
                phase: "released",
                ..
            }
        )),
        "{events:?}"
    );
    // The turn whose end the nudge covered is no person's of the switch's:
    // its point (the footer on the cheaper model now) owes the wind-down.
    let on_luna = codex_at("Here is the answer.", Some(4 * MIN), luna(), None);
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
    let a = act(&mut st, &on_luna, t);
    assert_eq!(a.rule_id(), Some(RULE_WIND_DOWN), "{a:?}");
    // Before the wind-down: the model is still owed, nothing held after.
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
    let _ = at(&mut st, &codex_at("Stage done.", None, luna(), None), t);
    let a = act(&mut st, &on_luna, t);
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "{a:?}");
    assert!(matches!(
        st.wind().map(|w| w.phase),
        Some(WindPhase::Restore { hold: false })
    ));
    let back = codex_at("Here is the answer.", None, astra(), None);
    let _ = at(&mut st, &back, t + MIN);
    assert!(!st.switch_open(), "restored, and nothing held");
    // A person's `/model` to another model during the hold.
    let mut st = switched(t, Some(t + 600 * MIN));
    set_phase(&mut st, WindPhase::Holding { since: t });
    let sol = CodexSetting {
        model: "GPT-6-Sol".to_string(),
        effort: Some("ultra".to_string()),
    };
    let _ = at(&mut st, &codex_at("Pushed.", None, sol, None), t + MIN);
    assert!(!st.switch_open());
    // A person's `/model` before the hold, and their `/goal resume` while
    // the model is owed: the switch is theirs, nothing more of it goes on.
    let sol = CodexSetting {
        model: "GPT-6-Sol".to_string(),
        effort: Some("high".to_string()),
    };
    let mut st = switched(t, Some(t + 600 * MIN));
    let a = at(
        &mut st,
        &codex_at("Here.", None, sol, Some(CodexGoal::Paused)),
        t + MIN,
    );
    assert!(!st.switch_open(), "{a:?}");
    assert_ne!(a.rule_id(), Some(RULE_WIND_DOWN));
    let mut st = switched(t, Some(t + 600 * MIN));
    set_phase(&mut st, WindPhase::Restore { hold: true });
    let _ = at(
        &mut st,
        &TurnEndReading {
            person: Some(Duration::from_secs(50)),
            ..codex_at("Here.", Some(MIN), luna(), Some(CodexGoal::Pursuing))
        },
        t + MIN,
    );
    assert!(
        matches!(
            st.wind().map(|w| w.phase),
            Some(WindPhase::Restore { hold: false })
        ),
        "theirs, the model still owed back, nothing held"
    );
    // The control: back on the cheaper model — restored again.
    let mut st = switched(t, Some(t + 600 * MIN));
    set_phase(&mut st, WindPhase::Holding { since: t });
    let _ = at(&mut st, &codex_at("Pushed.", None, luna(), None), t + MIN);
    assert!(matches!(
        st.wind().map(|w| w.phase),
        Some(WindPhase::Restore { hold: true })
    ));
}

/// A restore that does not verify after two `/model`s types nothing more and
/// hands the restore over with its steps (`/model → GPT-6-Astra → More
/// reasoning… → Ultra`, `s`).
#[test]
fn a_restore_that_does_not_verify_is_handed_over_with_its_steps() {
    let t = t0();
    let mut st = switched(t, Some(t + 600 * MIN));
    set_phase(&mut st, WindPhase::Restore { hold: true });
    let still = codex_at("Pushed.", None, luna(), None);
    for k in 0..RESTORE_TRIES {
        let a = act(&mut st, &still, t + k * MIN);
        assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "try {k}: {a:?}");
    }
    let a = act(&mut st, &still, t + 5 * MIN);
    assert_eq!(a, TurnEndAction::Nothing);
    let notes: Vec<String> = st
        .take_wind_events()
        .into_iter()
        .filter_map(|e| match e {
            WindEvent::Note(n) => Some(n),
            _ => None,
        })
        .collect();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("/model → GPT-6-Astra → More reasoning… → Ultra")),
        "{notes:?}"
    );
}

/// A switch a previous loop left open is carried on: its wind-down's turn
/// still WINDING (its end judges the marker), a hold from the start its row
/// named. A switch of this loop's own wins.
#[test]
fn a_seeded_switch_carries_on() {
    let t = t0();
    let wind = |phase| WindDown {
        phase,
        back_at: Some(t + 600 * MIN),
        goal_paused: true,
        pause_typed: true,
        pre: false,
        ..WindDown::opened(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string())
    };
    let mut st = TurnEndState::default();
    st.seed_wind(wind(WindPhase::Winding), t);
    assert!(matches!(
        st.wind().map(|w| w.phase),
        Some(WindPhase::Winding)
    ));
    assert!(st.take_wind_events().is_empty());
    let saved = codex_at(
        &format!("Pushed.\n{MARKER}"),
        Some(2 * MIN),
        luna(),
        Some(CodexGoal::Paused),
    );
    let a = at(&mut st, &saved, t + 2 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "{a:?}");
    assert_eq!(st.wind().and_then(|w| w.saved), Some(true));
    let since = t - 60 * MIN;
    let mut st = TurnEndState::default();
    st.seed_wind(wind(WindPhase::Holding { since }), t);
    assert!(matches!(
        st.wind().map(|w| w.phase),
        Some(WindPhase::Holding { since: s }) if s == since
    ));
    let mut own = switched(t, None);
    own.seed_wind(wind(WindPhase::Holding { since: t }), t);
    assert!(matches!(own.wind().map(|w| w.phase), Some(WindPhase::Owed)));
}

/// A HOST RESTART DURING THE WIND-DOWN'S OWN TURN (the review of
/// 2026-09-28, reproduced against the built crate): the new loop saw the
/// turn run and then end — nothing it typed — and took the harness's own
/// save for a person's message, dropped the hold, forgot the paused goal and
/// typed `keep going` at 95% of the window, days before its reset. Seeded,
/// the in-flight turn is the harness's own: its end judges the marker, the
/// model goes back, the session is HELD, the goal still owed its `/goal
/// resume`, and nothing is continued. NEGATIVE CONTROL: the same turn with a
/// person's keystroke in it is theirs.
#[test]
fn a_restart_during_the_wind_down_keeps_the_hold() {
    let t = t0();
    let back = t + 3 * 24 * 60 * MIN;
    let seeded = |phase| {
        let mut st = TurnEndState::new(TurnEndTiming {
            min_work: Duration::ZERO,
            short_backoff: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        st.seed_wind(
            WindDown {
                phase,
                goal_paused: true,
                pre: false,
                ..WindDown::opened(
                    astra(),
                    "gpt-6-luna".to_string(),
                    Some(back),
                    MARKER.to_string(),
                )
            },
            t,
        );
        st
    };
    let near = |r: TurnEndReading| TurnEndReading {
        limits: LimitRead::Near {
            used: 95,
            back_at: Some(1_900_000_000),
        },
        ..r
    };
    // Winding: the save's own end.
    let mut st = seeded(WindPhase::Winding);
    let end = near(codex_at(
        &format!("Committed and pushed.\n{MARKER}"),
        Some(2 * MIN),
        luna(),
        Some(CodexGoal::Paused),
    ));
    let a = act(&mut st, &end, t + 2 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "{a:?}");
    assert_eq!(
        st.wind().map(|w| (w.phase, w.goal_paused, w.saved)),
        Some((WindPhase::Restore { hold: true }, true, Some(true)))
    );
    let back_on = near(codex_at(
        "Model changed.",
        None,
        astra(),
        Some(CodexGoal::Paused),
    ));
    let a = at(&mut st, &back_on, t + 3 * MIN);
    assert!(st.holding(), "{:?}", st.wind());
    assert!(matches!(a, TurnEndAction::WaitUntil { .. }), "{a:?}");
    assert!(no_continuation(&a));
    // Owed: the goal turn the harness was stopping ends with the loop's
    // successor watching — no person's.
    let mut st = seeded(WindPhase::Owed);
    let goal_end = near(codex_at(
        "Step done.",
        Some(3 * MIN),
        luna(),
        Some(CodexGoal::Paused),
    ));
    let a = at(&mut st, &goal_end, t + 3 * MIN);
    assert!(matches!(st.wind().map(|w| w.phase), Some(WindPhase::Owed)));
    assert_eq!(a.rule_id(), Some(RULE_WIND_DOWN), "{a:?}");
    // The control: a person's keystroke in the turn makes it theirs.
    let mut st = seeded(WindPhase::Owed);
    let theirs = TurnEndReading {
        person: Some(3 * MIN),
        ..goal_end
    };
    let _ = at(&mut st, &theirs, t + 3 * MIN);
    assert!(matches!(
        st.wind().map(|w| w.phase),
        Some(WindPhase::Restore { hold: false })
    ));
}

/// CODEX'S GOAL ESCAPING ITS STOP (the review of 2026-09-28): while the
/// wind-down is owed, ANY turn running on the cheaper model is stopped — the
/// busy read's status row, not the footer's cached goal — up to
/// [`GOAL_STOPS`] times, never twice into one turn and never a person's;
/// then a person is told, once. The model owed back (hold: `Restore`) and
/// the hold: a goal seen running is released only with a person's hand in
/// it — without, the switch stands, `/goal pause` goes at a free point, and
/// the busy read stops its turn. NEGATIVE CONTROLS: the wind-down's own turn,
/// a person's turn, a switch a person took over, and the hold's plain turns
/// are never stopped.
#[test]
fn a_goal_escaping_its_stop_is_stopped_again_then_told() {
    let t = t0();
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
    // The goal turn under the box, whatever the footer cached (None).
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), None, false, t),
        Some(GoalStop::Esc)
    );
    assert_eq!(st.goal_stop(None, None, false, t), None, "no running turn");
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), None, true, t),
        None,
        "a person's hand"
    );
    st.own_esc(t);
    assert_eq!(
        st.goal_stop(
            Some(Duration::ZERO),
            None,
            false,
            t + Duration::from_secs(5)
        ),
        None,
        "never twice into one turn"
    );
    // Its point, then the goal escapes and runs again.
    let stopped = TurnEndReading {
        interrupted: true,
        ..codex_at(
            "■ Conversation interrupted",
            Some(Duration::from_secs(4)),
            luna(),
            Some(CodexGoal::Paused),
        )
    };
    st.observe(&stopped, t + Duration::from_secs(6));
    for k in 2..=GOAL_STOPS {
        let now = t + k * MIN;
        assert_eq!(
            st.goal_stop(Some(Duration::ZERO), Some(CodexGoal::Pursuing), false, now),
            Some(GoalStop::Esc)
        );
        st.own_esc(now);
        st.observe(&stopped, now + Duration::from_secs(5));
    }
    let now = t + 10 * MIN;
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), Some(CodexGoal::Pursuing), false, now),
        Some(GoalStop::Tell)
    );
    st.goal_told();
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), Some(CodexGoal::Pursuing), false, now),
        None,
        "told once"
    );
    // The wind-down's own turn, within its bound: never stopped
    // (`the_save_turn_is_bounded_then_judged_and_restored`: past it).
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
    set_phase(&mut st, WindPhase::Winding);
    assert_eq!(st.goal_stop(Some(Duration::ZERO), None, false, t), None);
    set_phase(&mut st, WindPhase::Restore { hold: false });
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), Some(CodexGoal::Pursuing), false, t),
        None,
        "the person's"
    );
    // The model owed back: the goal seen running releases nothing without a
    // person's hand; `/goal pause` at the point.
    set_phase(&mut st, WindPhase::Restore { hold: true });
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), None, false, t),
        Some(GoalStop::Esc)
    );
    let runs = codex_at(
        "More work.",
        Some(10 * MIN),
        luna(),
        Some(CodexGoal::Pursuing),
    );
    let a = at(&mut st, &runs, t + MIN);
    assert!(st.switch_open());
    assert_eq!(
        a,
        TurnEndAction::TypeCommand {
            command: "/goal pause".to_string(),
            rule_id: RULE_WIND_DOWN,
            then: Then::Nothing
        }
    );
    // The hold: a goal turn is stopped, a plain turn is not.
    set_phase(&mut st, WindPhase::Holding { since: t });
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), Some(CodexGoal::Pursuing), false, t),
        Some(GoalStop::Esc)
    );
    assert_eq!(st.goal_stop(Some(Duration::ZERO), None, false, t), None);
    let runs_own = codex_at(
        "More work.",
        Some(10 * MIN),
        astra(),
        Some(CodexGoal::Pursuing),
    );
    let a = at(&mut st, &runs_own, t + 2 * MIN);
    assert!(st.holding(), "no release without a person's hand");
    assert_eq!(a.rule_id(), Some(RULE_WIND_DOWN), "{a:?}");
    let theirs = TurnEndReading {
        person: Some(MIN),
        ..runs_own
    };
    let _ = at(&mut st, &theirs, t + 3 * MIN);
    assert!(!st.switch_open(), "a person's `/goal resume`");
}

/// THE HOLD'S END UNDER THE OWNER'S LIMITS (the review of 2026-09-28): with
/// `resume_limits = false` nothing is typed at the reset — no `/goal
/// resume`, no carry-on — and a person is told; with `continue_policy =
/// false` the carry-on is never typed (told instead), while the goal the
/// harness paused is still resumed. NEGATIVE CONTROL: the defaults resume.
#[test]
fn the_holds_end_keeps_resume_limits_and_continue_policy() {
    let t = t0();
    let far = TurnEndReading {
        limits: LimitRead::Far { used: 3 },
        ..codex_at("Pushed.", None, astra(), Some(CodexGoal::Paused))
    };
    let held = |goal_paused: bool| {
        let mut st = TurnEndState::default();
        st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
        set_phase(&mut st, WindPhase::Holding { since: t });
        st.wind.as_mut().expect("open").goal_paused = goal_paused;
        st
    };
    let no_resume = SupervisorConfig {
        resume_limits: false,
        ..cfg()
    };
    let no_continue = SupervisorConfig {
        continue_policy: false,
        ..cfg()
    };
    for goal_paused in [true, false] {
        let a = at_under(&mut held(goal_paused), &far, t + MIN, &no_resume);
        let TurnEndAction::Escalate { reason } = &a else {
            panic!("{goal_paused}: {a:?}")
        };
        assert!(reason.contains("resume_limits is off"), "{reason}");
    }
    let a = at_under(&mut held(false), &far, t + MIN, &no_continue);
    assert!(
        matches!(&a, TurnEndAction::Escalate { reason } if reason.contains("continue_policy is off")),
        "{a:?}"
    );
    assert_eq!(
        at_under(&mut held(true), &far, t + MIN, &no_continue).rule_id(),
        Some(RULE_LIMIT_RESUME),
        "the goal's own resume is no carry-on"
    );
    for goal_paused in [true, false] {
        assert_eq!(
            at_under(&mut held(goal_paused), &far, t + MIN, &cfg()).rule_id(),
            Some(RULE_LIMIT_RESUME),
            "the control"
        );
    }
}

/// NOTHING INTO A FALLEN SANDBOX (the owner: "not continue work in a
/// sandbox"): no save instruction, no `/goal pause`, no `/model`, no resume.
/// NEGATIVE CONTROL: the same points outside it act.
#[test]
fn nothing_of_the_switch_is_typed_into_a_fallen_sandbox() {
    let t = t0();
    let points = [
        (WindPhase::Owed, None),
        (WindPhase::Owed, Some(CodexGoal::Pursuing)),
        (WindPhase::Restore { hold: true }, None),
        (WindPhase::Restore { hold: false }, None),
        (WindPhase::Restore { hold: true }, Some(CodexGoal::Pursuing)),
        (
            WindPhase::Holding {
                since: t - 600 * MIN,
            },
            None,
        ),
        (WindPhase::Holding { since: t }, Some(CodexGoal::Pursuing)),
    ];
    for (phase, goal) in points {
        let mut st = switched(t, None);
        set_phase(&mut st, phase);
        let model = if matches!(phase, WindPhase::Holding { .. }) {
            astra()
        } else {
            luna()
        };
        let free = codex_at("Pushed.", None, model, goal);
        let fell = TurnEndReading {
            sandbox_fell: Some("workspace-write".to_string()),
            ..free.clone()
        };
        assert_eq!(
            decide_turn_end(&st, &fell, &cfg(), t),
            TurnEndAction::Nothing,
            "{phase:?} {goal:?}"
        );
        let a = decide_turn_end(&st, &free, &cfg(), t);
        assert!(
            matches!(
                a,
                TurnEndAction::Type { .. } | TurnEndAction::TypeCommand { .. }
            ),
            "the control, {phase:?} {goal:?}: {a:?}"
        );
    }
}

/// THE PRESS CONFIRMED BEFORE THE SAVE (the review of 2026-09-28): the save
/// instruction goes only once the footer shows the cheaper model. At the
/// point the press's box covered, a footer still on the thread's own model
/// is waited on (it may not have caught up); at any later point it is the
/// switch that did not land — released, nothing typed. NEGATIVE CONTROL:
/// the footer on the cheaper model gets the save.
#[test]
fn a_switch_whose_footer_never_shows_the_cheaper_model_is_released() {
    let t = t0();
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
    let own = codex_at("Stage done.", Some(MIN), astra(), None);
    let a = at(&mut st, &own, t);
    assert!(waits_until(&a, t + Duration::from_secs(30)), "{a:?}");
    assert!(st.switch_open());
    let again = codex_at("Stage done.", None, astra(), None);
    let a = at(&mut st, &again, t + Duration::from_secs(30));
    assert!(!st.switch_open(), "{a:?}");
    assert!(st.take_wind_events().iter().any(|e| matches!(
        e,
        WindEvent::Edge { phase: "released", what, .. } if what.contains("did not land")
    )));
    let mut st = TurnEndState::default();
    st.nudge_switched(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string());
    let on_luna = codex_at("Stage done.", Some(MIN), luna(), None);
    assert_eq!(at(&mut st, &on_luna, t).rule_id(), Some(RULE_WIND_DOWN));
}

/// THE HARNESS'S PICKER, AND A PERSON'S (the reviews of 2026-09-28): the
/// `/model` picker is the restore's while its picker is up — and through a
/// frame between its boxes (the model box, the effort box, `More
/// reasoning…`'s advanced one): only gone on two reads [`PICKER_SETTLE`]
/// apart, or at an idle point, has it left; once it has, or a person has
/// typed since the `/model`, a picker on the screen is the person's.
/// NEGATIVE CONTROLS: the restore's own picker, not yet left, is answered;
/// one frame without it, the next box then up, is still the restore's.
#[test]
fn a_picker_a_person_reopens_is_theirs() {
    let t = t0();
    let s = Duration::from_secs;
    let mut st = switched(t, None);
    set_phase(&mut st, WindPhase::Restore { hold: true });
    let still = codex_at("Pushed.", None, luna(), None);
    assert_eq!(act(&mut st, &still, t).rule_id(), Some(RULE_MODEL_RESTORE));
    st.picker_seen(false, None, t + s(1));
    assert_eq!(st.restore_target(t + s(1)), Some(astra()), "not up yet");
    st.picker_seen(true, None, t + s(2));
    assert_eq!(st.restore_target(t + s(2)), Some(astra()), "the control");
    // One frame between the model box and the effort box: still the
    // restore's (the re-review's risk), and the next box is answered.
    st.picker_seen(false, None, t + s(3));
    assert_eq!(st.restore_target(t + s(3)), Some(astra()), "one frame");
    st.picker_seen(true, None, t + s(3) + Duration::from_millis(400));
    assert_eq!(st.restore_target(t + s(4)), Some(astra()), "the effort box");
    // Gone, and still gone a settle later: it left.
    st.picker_seen(false, None, t + s(20));
    assert_eq!(st.restore_target(t + s(20)), Some(astra()), "one read");
    st.picker_seen(false, None, t + s(20) + PICKER_SETTLE);
    assert_eq!(
        st.restore_target(t + s(20) + PICKER_SETTLE),
        None,
        "it left"
    );
    // Gone at an IDLE POINT (a frame between its boxes the loop's own
    // settle outlasted, the round-3 re-review): still the restore's within
    // the settle, and no `/model` typed again into the picker's chain — it
    // left once it has been gone the settle, and the restore goes on.
    let mut st = switched(t, None);
    set_phase(&mut st, WindPhase::Restore { hold: true });
    let _ = act(&mut st, &still, t);
    st.picker_seen(true, None, t + s(2));
    let a = at(&mut st, &still, t + s(3));
    assert!(
        matches!(a, TurnEndAction::WaitUntil { until, .. } if until == t + s(3) + PICKER_SETTLE),
        "no `/model` within the settle: {a:?}"
    );
    assert_eq!(st.restore_target(t + s(3)), Some(astra()), "an idle frame");
    let a = at(&mut st, &still, t + s(3) + PICKER_SETTLE);
    assert_eq!(
        st.restore_target(t + s(3) + PICKER_SETTLE),
        None,
        "the idle point past the settle"
    );
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "tried again: {a:?}");
    // A person's keystroke after the `/model`.
    let mut st = switched(t, None);
    set_phase(&mut st, WindPhase::Restore { hold: true });
    let _ = act(&mut st, &still, t);
    st.picker_seen(true, None, t + Duration::from_secs(2));
    st.picker_seen(
        true,
        Some(Duration::from_secs(1)),
        t + Duration::from_secs(10),
    );
    assert_eq!(st.restore_target(t + Duration::from_secs(10)), None);
    // A keystroke from before the `/model` is no claim on it.
    let mut st = switched(t, None);
    set_phase(&mut st, WindPhase::Restore { hold: true });
    let _ = act(&mut st, &still, t);
    st.picker_seen(
        true,
        Some(Duration::from_secs(30)),
        t + Duration::from_secs(10),
    );
    assert_eq!(
        st.restore_target(t + Duration::from_secs(10)),
        Some(astra())
    );
}

/// THE HOLD IS SAID (the review of 2026-09-28: a hold can last days): the
/// point that shows the thread back on its own model begins the hold with
/// ONE `Hold` record, and no other point repeats it. The harness's own Esc
/// excuses only the turn it stopped: its next typed act ends that.
#[test]
fn the_hold_begins_with_one_record_and_the_own_esc_is_spent_by_the_next_act() {
    let t = t0();
    let mut st = switched(t, Some(t + 600 * MIN));
    assert!(st.own_interrupt.is_some());
    let stopped = TurnEndReading {
        interrupted: true,
        ..codex_at(
            "■ Conversation interrupted",
            Some(Duration::from_secs(3)),
            luna(),
            None,
        )
    };
    let a = act(&mut st, &stopped, t);
    assert_eq!(a.rule_id(), Some(RULE_WIND_DOWN));
    assert!(st.own_interrupt.is_none(), "spent by the save's own act");
    let saved = codex_at(&format!("Pushed.\n{MARKER}"), Some(2 * MIN), luna(), None);
    let _ = act(&mut st, &saved, t + 3 * MIN);
    let _ = st.take_wind_events();
    let back = codex_at("Pushed.", None, astra(), None);
    let _ = at(&mut st, &back, t + 4 * MIN);
    let holds = |st: &mut TurnEndState| {
        st.take_wind_events()
            .into_iter()
            .filter(|e| matches!(e, WindEvent::Hold { .. }))
            .count()
    };
    assert_eq!(holds(&mut st), 1);
    let _ = at(&mut st, &back, t + 40 * MIN);
    assert_eq!(holds(&mut st), 0);
}

/// CODEX'S GOAL CONTINUES BY ITSELF: at a point whose footer says the goal
/// is pursued, nothing is typed — no continuation, no answer (the incident's
/// eleven `keep going`s all landed inside a running goal turn). A thread
/// fallen into a sandbox its launch bypassed gets nothing either. NEGATIVE
/// CONTROLS: a paused goal, and Claude Code, are continued as before.
#[test]
fn codexs_pursued_goal_and_a_fallen_sandbox_get_no_continuation() {
    let t = t0();
    let pursued = codex_at(
        "Stage done.",
        Some(30 * MIN),
        astra(),
        Some(CodexGoal::Pursuing),
    );
    assert_eq!(
        at(&mut TurnEndState::default(), &pursued, t),
        TurnEndAction::Nothing
    );
    let asks = codex_at(
        "Which do you prefer?",
        Some(30 * MIN),
        astra(),
        Some(CodexGoal::Pursuing),
    );
    assert_eq!(
        at(&mut TurnEndState::default(), &asks, t),
        TurnEndAction::Nothing
    );
    let fell = TurnEndReading {
        sandbox_fell: Some("workspace-write".to_string()),
        ..codex_at("Stage done.", Some(30 * MIN), astra(), None)
    };
    assert_eq!(
        at(&mut TurnEndState::default(), &fell, t),
        TurnEndAction::Nothing
    );
    let paused = codex_at(
        "Stage done.",
        Some(30 * MIN),
        astra(),
        Some(CodexGoal::Paused),
    );
    assert_eq!(
        at(&mut TurnEndState::default(), &paused, t),
        typed(RULE_CONTINUE)
    );
    let claude = TurnEndReading {
        program: Program::Claude,
        ..pursued
    };
    assert_eq!(
        at(&mut TurnEndState::default(), &claude, t),
        typed(RULE_CONTINUE)
    );
}

/// CODEX'S HARD USAGE WALL with its goal stopped by it (`Goal hit usage
/// limits (/goal resume)` on the footer): waited out as before, and resumed
/// with `/goal resume` a minute past the reset. NEGATIVE CONTROL: with no
/// goal, the continuation.
#[test]
fn a_codex_goal_the_wall_stopped_is_resumed_with_goal_resume() {
    let t = t0();
    let wall = |goal| TurnEndReading {
        wall: Some(WallKind::UsageSession),
        wall_message: "You've hit your usage limit.".to_string(),
        reset_at: Some(t + 60 * MIN),
        phase: Phase::Limited {
            message: "You've hit your usage limit.".to_string(),
            reset: None,
        },
        ..codex_at("", Some(10 * MIN), astra(), goal)
    };
    let limited = wall(Some(CodexGoal::UsageLimited));
    let mut st = TurnEndState::default();
    assert!(waits_until(&at(&mut st, &limited, t), t + 61 * MIN));
    // A footer that still says the goal is pursued at the wall: the wall's
    // own rule, never left for good.
    let mut still = TurnEndState::default();
    let pursued = wall(Some(CodexGoal::Pursuing));
    assert!(waits_until(&at(&mut still, &pursued, t), t + 61 * MIN));
    assert_eq!(
        at(&mut still, &pursued, t + 62 * MIN).rule_id(),
        Some(RULE_LIMIT_RESUME)
    );
    assert_eq!(
        at(&mut st, &limited, t + 62 * MIN),
        TurnEndAction::TypeCommand {
            command: "/goal resume".to_string(),
            rule_id: RULE_LIMIT_RESUME,
            then: Then::Nothing
        }
    );
    let mut st = TurnEndState::default();
    let _ = at(&mut st, &wall(None), t);
    assert_eq!(
        at(&mut st, &wall(None), t + 62 * MIN),
        typed(RULE_LIMIT_RESUME)
    );
}

/// The wind-down's own turn ended on an API error: not its end — the wall's
/// own rule carries the wind-down on (its retry, on the cheaper model, is
/// the save going on), and the point after it is the one judged. NEGATIVE
/// CONTROL: its end with no wall is judged at once.
#[test]
fn an_api_error_in_the_wind_down_is_retried_not_judged() {
    let t = t0();
    let mut st = switched(t, Some(t + 600 * MIN));
    let stopped = TurnEndReading {
        interrupted: true,
        ..codex_at(
            "■ Conversation interrupted",
            Some(Duration::from_secs(3)),
            luna(),
            None,
        )
    };
    let _ = act(&mut st, &stopped, t);
    let failed = TurnEndReading {
        wall: Some(WallKind::ApiError {
            code: Some(500),
            retryable: true,
            cause: ApiCause::Server,
        }),
        wall_message: "stream disconnected before completion".to_string(),
        ..codex_at("", Some(MIN), luna(), None)
    };
    let a = act(&mut st, &failed, t + 2 * MIN);
    assert!(matches!(
        st.wind().map(|w| w.phase),
        Some(WindPhase::Winding)
    ));
    assert!(
        matches!(a, TurnEndAction::WaitUntil { .. }) || a.rule_id() == Some(RULE_API_RETRY),
        "{a:?}"
    );
    let a = act(&mut st, &failed, t + 4 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_API_RETRY), "{a:?}");
    let saved = codex_at(&format!("Pushed.\n{MARKER}"), Some(2 * MIN), luna(), None);
    let a = act(&mut st, &saved, t + 7 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "{a:?}");
    assert_eq!(st.wind().and_then(|w| w.saved), Some(true));
}

/// A switch opened by the nudge's press at `t` and moved to `phase`, its
/// first point past (`pre` off), nothing stopped yet.
fn open_at(t: Instant, phase: WindPhase) -> TurnEndState {
    let mut st = TurnEndState::default();
    st.nudge_switched(
        astra(),
        "gpt-6-luna".to_string(),
        Some(t + 3 * 24 * 60 * MIN),
        MARKER.to_string(),
    );
    set_phase(&mut st, phase);
    if let Some(w) = st.wind.as_mut() {
        w.goal_paused = true;
    }
    st
}

/// A PERSON'S OWN TURN IS NEVER STOPPED (the re-review of 2026-09-28, its
/// repro replayed): their message while the save is owed or the model owed
/// back, and their `/goal resume` during the hold, read at the loop's busy
/// reads 10, 60, 119, 121 and 180 s into the turn — the grace (120 s) runs
/// out at 121 s, the keystroke that began the turn does not leave it
/// ([`RunningTurn::person`]: latched) — get no Esc, and the turn's end is
/// theirs: the switch released as documented. NEGATIVE CONTROLS: the grace
/// alone (round 2's defect) stops each at 121 s; a keystroke a minute
/// before the turn began holds the stop only while its grace lasts, and the
/// turn is stopped once it has run out — the grace latches nothing.
#[test]
fn a_persons_own_turn_is_never_stopped() {
    let t = t0();
    let s = Duration::from_secs;
    let grace = s(u64::from(cfg().human_grace_s));
    for (phase, goal) in [
        (WindPhase::Owed, Some(CodexGoal::Paused)),
        (WindPhase::Restore { hold: true }, Some(CodexGoal::Paused)),
        (WindPhase::Holding { since: t }, Some(CodexGoal::Pursuing)),
    ] {
        let mut st = open_at(t, phase);
        let sent = t + 5 * MIN;
        let mut theirs = RunningTurn::default();
        theirs.busy(sent + s(1));
        let mut before = RunningTurn::default();
        before.busy(sent + s(1));
        let mut stopped_at = None;
        for d in [10, 60, 119, 121, 180] {
            let at_ = sent + s(d);
            let person = theirs.person(Some(sent), Some(t), None, grace, at_);
            assert_eq!(
                st.goal_stop(Some(Duration::ZERO), goal, person, at_),
                None,
                "{phase:?}: {d} s into their turn"
            );
            // The defect: the grace alone.
            if d >= 121 {
                assert_eq!(
                    st.goal_stop(Some(Duration::ZERO), goal, s(d) < grace, at_),
                    Some(GoalStop::Esc),
                    "{phase:?}: the grace alone ran out"
                );
            }
            // The control: their keystroke a minute before the turn.
            let older = before.person(Some(sent - MIN), Some(t), None, grace, at_);
            if stopped_at.is_none()
                && st
                    .goal_stop(Some(Duration::ZERO), goal, older, at_)
                    .is_some()
            {
                stopped_at = Some(d);
            }
        }
        assert!(theirs.latched(), "{phase:?}: latched");
        assert!(!before.latched(), "{phase:?}: the grace latched nothing");
        assert_eq!(
            stopped_at,
            Some(60),
            "{phase:?}: held while the grace lasted, stopped once it ran out"
        );
        // Their turn ends: theirs, as documented.
        let end = TurnEndReading {
            person: Some(s(181)),
            ..codex_at("Here is the answer.", Some(s(180)), luna(), goal)
        };
        let end = if matches!(phase, WindPhase::Holding { .. }) {
            TurnEndReading {
                model_field: Some(astra()),
                ..end
            }
        } else {
            end
        };
        let a = at(&mut st, &end, sent + s(181));
        match phase {
            WindPhase::Holding { .. } => assert!(!st.switch_open(), "released: {a:?}"),
            _ => assert!(
                matches!(
                    st.wind().map(|w| w.phase),
                    Some(WindPhase::Restore { hold: false })
                ),
                "{phase:?}: the model still owed back, nothing held after: {a:?}"
            ),
        }
        assert_ne!(a.rule_id(), Some(RULE_WIND_DOWN), "{phase:?}: {a:?}");
    }
}

/// THE RUNNING TURN IS MEASURED FROM THE NUDGE'S BOX AND FLOORED AT THE
/// SWITCH'S OPENING (the round-3 re-review's regression): a person's message
/// began the turn the nudge's box covered (their keystroke, then ten minutes
/// of it — past the grace); Codex's goal starts its own turn under the box
/// on the cheaper model, and the switch is pressed. That goal turn is none
/// of theirs, at its first busy read and past the grace — stopped; so is one
/// after a keep during the hold. NEGATIVE CONTROLS: round 3's reading — the
/// span carried across the box, no floor — spares it; a keystroke AFTER the
/// switch opened is their hand in the goal turn, latched; the point that
/// ends the covered turn with no busy read after the box is measured over
/// the covered turn (`pre`'s work), and a point measures its own turn.
#[test]
fn the_running_turn_is_measured_from_the_box_and_floored_at_the_switch() {
    let t = t0();
    let s = Duration::from_secs;
    let grace = s(u64::from(cfg().human_grace_s));
    let typed = t;
    let covered = t + s(1);
    let pressed = t + 10 * MIN;
    for (phase, goal, floor) in [
        // The press: the switch opens at the box's leaving.
        (WindPhase::Owed, Some(CodexGoal::Pursuing), pressed),
        // A keep during the hold (the switch opened long before).
        (
            WindPhase::Holding { since: t - MIN },
            Some(CodexGoal::Pursuing),
            t - 60 * MIN,
        ),
    ] {
        let st = open_at(t, phase);
        let mut hand = RunningTurn::default();
        hand.busy(covered);
        // Round 3's span, never closed at the box.
        let round3_for = |at: Instant| at.saturating_duration_since(covered);
        hand.boxed();
        for d in [1, 180] {
            let at_ = pressed + s(d);
            if d == 1 {
                hand.busy(at_);
            }
            let person = hand.person(Some(typed), Some(floor), None, grace, at_);
            assert!(!person, "{phase:?}: {d} s into the goal turn");
            assert_eq!(
                st.goal_stop(Some(Duration::ZERO), goal, person, at_),
                Some(GoalStop::Esc),
                "{phase:?}: {d} s in"
            );
            // Round 3: the keystroke within the span the box never closed.
            let ago = at_.saturating_duration_since(typed);
            assert!(
                ago <= round3_for(at_) + PERSON_TURN_SLACK,
                "{phase:?}: round 3 spares it"
            );
        }
        // A keystroke after the opening, in the goal turn: theirs.
        let mut theirs = RunningTurn::default();
        theirs.boxed();
        theirs.busy(pressed + s(1));
        assert!(theirs.person(
            Some(pressed + s(5)),
            Some(pressed),
            None,
            grace,
            pressed + s(10)
        ));
        assert!(theirs.person(
            Some(pressed + s(5)),
            Some(pressed),
            None,
            grace,
            pressed + s(900)
        ));
        assert!(theirs.latched());
    }
    // The points: over the covered turn when no busy read came after the
    // box (the `pre` point), else over the turn's own span; the latch spent.
    let mut hand = RunningTurn::default();
    hand.busy(covered);
    hand.boxed();
    assert_eq!(hand.point(pressed), Some(pressed - covered));
    assert_eq!(hand.point(pressed), None, "a fresh span");
    let mut hand = RunningTurn::default();
    hand.busy(covered);
    hand.boxed();
    hand.busy(pressed + s(1));
    let _ = hand.person(Some(pressed + s(2)), None, None, grace, pressed + s(3));
    assert!(hand.latched());
    assert_eq!(hand.point(pressed + s(4)), Some(s(3)));
    assert!(!hand.latched(), "spent at its point");
}

/// A CARRIED-ON SWITCH'S TURN IN FLIGHT, BOUNDED (the round-3 re-review): a
/// keystroke since the switch's last row is a person's hand in the turn in
/// flight at the loop's start only within [`SEEDED_TURN_BOUND`] of the
/// loop's first read of it — a hold's row three days old and a keystroke a
/// day ago is none. NEGATIVE CONTROLS: a keystroke two minutes ago is
/// theirs; one from before the row never.
#[test]
fn a_carried_on_turns_hand_is_bounded_to_the_turn_in_flight() {
    let t = t0();
    let s = Duration::from_secs;
    let day = 24 * 60 * MIN;
    let row = t - 3 * day;
    assert!(!seeded_hand(Some(day), Some(s(20)), row, t));
    assert!(seeded_hand(Some(2 * MIN), Some(s(20)), row, t));
    assert!(seeded_hand(Some(SEEDED_TURN_BOUND), Some(s(0)), row, t));
    assert!(!seeded_hand(
        Some(SEEDED_TURN_BOUND + s(1)),
        Some(s(0)),
        row,
        t
    ));
    assert!(!seeded_hand(Some(4 * day), Some(s(20)), row, t));
    // At the loop's busy reads: the same bound, latched.
    let grace = s(u64::from(cfg().human_grace_s));
    let mut hand = RunningTurn::default();
    hand.busy(t);
    assert!(!hand.person(Some(t - day), None, Some(row), grace, t + s(200)));
    assert!(hand.person(Some(t - 2 * MIN), None, Some(row), grace, t + s(200)));
    assert!(hand.latched());
    // At the turn's end: the goal escaped — the hold stands, the goal paused.
    let mut st = TurnEndState::default();
    st.seed_wind(
        WindDown {
            phase: WindPhase::Holding { since: row },
            goal_paused: true,
            pre: false,
            seeded_at: Some(row),
            ..WindDown::opened(
                astra(),
                "gpt-6-luna".to_string(),
                Some(t + 3 * day),
                MARKER.to_string(),
            )
        },
        t,
    );
    let end = TurnEndReading {
        person: Some(day),
        ..codex_at(
            "Step done.",
            Some(s(20)),
            astra(),
            Some(CodexGoal::Pursuing),
        )
    };
    let a = at(&mut st, &end, t + s(20));
    assert!(st.holding(), "{a:?}");
    assert_eq!(
        a,
        TurnEndAction::TypeCommand {
            command: GOAL_PAUSE.to_string(),
            rule_id: RULE_WIND_DOWN,
            then: Then::Nothing,
        }
    );
}

/// A SWITCH CARRIED ON MID-TURN, THE TURN A PERSON'S (the re-review's loop
/// scenario): the turn in flight when the loop started began out of its
/// sight, so a person's keystroke since the switch's last row is their hand
/// in it ([`WindDown::seeded_at`]) — their `/goal resume` during the hold
/// releases it at its end. NEGATIVE CONTROL: a keystroke from before the
/// row: the goal escaped its stop — the hold stands and the goal is paused.
#[test]
fn a_carried_on_switchs_turn_is_a_persons_with_a_keystroke_since_its_row() {
    let t = t0();
    let row = t - 20 * MIN;
    for (typed_ago, theirs) in [(2 * MIN, true), (25 * MIN, false)] {
        let mut st = TurnEndState::default();
        st.seed_wind(
            WindDown {
                phase: WindPhase::Holding {
                    since: t - 30 * MIN,
                },
                goal_paused: true,
                pre: false,
                seeded_at: Some(row),
                ..WindDown::opened(
                    astra(),
                    "gpt-6-luna".to_string(),
                    Some(t + 3 * 24 * 60 * MIN),
                    MARKER.to_string(),
                )
            },
            t,
        );
        let end = TurnEndReading {
            person: Some(typed_ago),
            ..codex_at(
                "Step done.",
                Some(Duration::from_secs(20)),
                astra(),
                Some(CodexGoal::Pursuing),
            )
        };
        let a = at(&mut st, &end, t + Duration::from_secs(20));
        assert_eq!(!st.switch_open(), theirs, "{typed_ago:?}: {a:?}");
        if !theirs {
            assert_eq!(
                a,
                TurnEndAction::TypeCommand {
                    command: GOAL_PAUSE.to_string(),
                    rule_id: RULE_WIND_DOWN,
                    then: Then::Nothing,
                }
            );
        }
    }
}

/// A PERSON'S OWN `/model` TO THE CHEAPER MODEL DURING THE HOLD (the
/// re-review, repro4): their keystroke since the hold began makes the model
/// the footer shows theirs — the switch is released, never driven back.
/// NEGATIVE CONTROL: no keystroke since the hold began — the thread back on
/// the cheaper model is restored again.
#[test]
fn a_persons_model_during_the_hold_releases_it() {
    let t = t0();
    let since = t + MIN;
    for (person, released) in [
        (Some(Duration::from_secs(10)), true),
        (Some(3 * MIN), false),
        (None, false),
    ] {
        let mut st = open_at(t, WindPhase::Holding { since });
        let on_luna = TurnEndReading {
            person,
            ..codex_at("Pushed.", None, luna(), Some(CodexGoal::Paused))
        };
        let now = since + 2 * MIN;
        let a = at(&mut st, &on_luna, now);
        assert_eq!(!st.switch_open(), released, "{person:?}: {a:?}");
        if released {
            assert_eq!(st.restore_target(now), None);
            assert!(st.take_wind_events().iter().any(|e| matches!(
                e,
                WindEvent::Edge { phase: "released", what, .. } if what.contains("GPT-6-Luna")
            )));
        } else {
            assert!(matches!(
                st.wind().map(|w| w.phase),
                Some(WindPhase::Restore { hold: true })
            ));
        }
    }
}

/// THE OWED FOOTER GATE (the re-review's nit): a footer still on the
/// thread's own model is the switch that did not land — unless the thread's
/// own rollout says its last turn ran on the cheaper model (the footer lags
/// it): then the save waits. A footer that shows no model holds the save
/// back, and past [`OWED_FOOTER_BOUND`] a person is told, once.
#[test]
fn the_owed_footer_gate_reads_the_threads_rollout_and_bounds_a_footer_with_no_model() {
    let t = t0();
    for (thread, released) in [
        (Some("gpt-6-luna"), false),
        (Some("gpt-6-astra"), true),
        (None, true),
    ] {
        let mut st = open_at(t, WindPhase::Owed);
        let own = TurnEndReading {
            thread_model: thread.map(str::to_string),
            ..codex_at("Stage done.", None, astra(), Some(CodexGoal::Paused))
        };
        let a = at(&mut st, &own, t + MIN);
        assert_eq!(!st.switch_open(), released, "{thread:?}: {a:?}");
        if !released {
            assert!(matches!(a, TurnEndAction::WaitUntil { .. }), "{a:?}");
        }
    }
    let mut st = open_at(t, WindPhase::Owed);
    let blank = TurnEndReading {
        model_field: None,
        ..codex_at("Stage done.", None, luna(), Some(CodexGoal::Paused))
    };
    let notes = |st: &mut TurnEndState| {
        st.take_wind_events()
            .into_iter()
            .filter(|e| matches!(e, WindEvent::Note(n) if n.contains("footer shows no model")))
            .count()
    };
    let a = at(&mut st, &blank, t);
    assert!(matches!(a, TurnEndAction::WaitUntil { .. }), "{a:?}");
    assert_eq!(notes(&mut st), 0);
    let _ = at(&mut st, &blank, t + OWED_FOOTER_BOUND - MIN);
    assert_eq!(notes(&mut st), 0, "within the bound");
    let _ = at(&mut st, &blank, t + OWED_FOOTER_BOUND);
    assert_eq!(notes(&mut st), 1, "past it, said");
    let _ = at(&mut st, &blank, t + OWED_FOOTER_BOUND + MIN);
    assert_eq!(notes(&mut st), 0, "once");
    assert!(st.switch_open());
}

/// THE OWED FOOTER GATE, BOUNDED AND RELEASED (the round-3 re-review, rr3's
/// scratch test made real): the thread's rollout says it ran the cheaper
/// model and the footer shows its own — the goal turn under the box ran on
/// luna and was stopped, then a person put the thread back with `/model`
/// and sent nothing. Their keystroke since the switch opened releases the
/// switch (the session is theirs); with none, the save waits, and past
/// [`OWED_FOOTER_BOUND`] a person is told ONCE — never a silent wait of a
/// day. NEGATIVE CONTROL: a keystroke from before the switch opened is no
/// person's `/model`: the wait stands, bounded.
#[test]
fn the_owed_footer_gate_is_bounded_and_a_persons_model_releases_it() {
    let t = t0();
    let opened = t - MIN;
    let lagging = |person: Option<Duration>| TurnEndReading {
        thread_model: Some("gpt-6-luna".to_string()),
        person,
        ..codex_at("Stage done.", None, astra(), Some(CodexGoal::Paused))
    };
    let owed = || {
        let mut st = open_at(t, WindPhase::Owed);
        if let Some(w) = st.wind.as_mut() {
            w.opened_at = Some(opened);
        }
        st
    };
    // A person's keystroke since the opening: released, theirs.
    let mut st = owed();
    let a = at(&mut st, &lagging(Some(Duration::from_secs(20))), t);
    assert!(!st.switch_open(), "{a:?}");
    assert!(st.take_wind_events().iter().any(|e| matches!(
        e,
        WindEvent::Edge { phase: "released", what, .. } if what.contains("a person put the thread back on GPT-6-Astra")
    )));
    // None (or one from before the opening): waited on, said once past the
    // bound, over a day of points.
    for person in [None, Some(2 * MIN)] {
        let mut st = owed();
        let mut notes = Vec::new();
        for i in 0..(24 * 60) {
            let now = t + MIN * i;
            let r = lagging(person.map(|p| p + MIN * i));
            let a = at(&mut st, &r, now);
            assert!(matches!(a, TurnEndAction::WaitUntil { .. }), "{i}: {a:?}");
            for e in st.take_wind_events() {
                if let WindEvent::Note(n) = e {
                    notes.push((i, n));
                }
            }
        }
        assert!(st.switch_open(), "{person:?}");
        assert_eq!(notes.len(), 1, "{person:?}: {notes:?}");
        let (i, n) = &notes[0];
        assert_eq!(MIN * *i, OWED_FOOTER_BOUND, "said at the bound");
        assert!(
            n.contains(
                "the footer shows GPT-6-Astra ultra while the thread's last turn ran gpt-6-luna"
            ),
            "{n}"
        );
    }
}

/// THE HARNESS'S LAST ESC RIDES THE ROWS (the re-review's nit, repro5): a
/// loop restarted between its Esc in the hold or the model owed back and
/// the interrupted point it makes takes that interrupt for its own — the
/// hold stands, the restore keeps its hold — never for a person's message.
/// NEGATIVE CONTROL: the same point with no Esc carried is a person's.
#[test]
fn a_restart_between_the_esc_and_its_point_keeps_the_switch() {
    let t = t0();
    for phase in [
        WindPhase::Holding {
            since: t - 60 * MIN,
        },
        WindPhase::Restore { hold: true },
    ] {
        for esc in [true, false] {
            let mut st = TurnEndState::default();
            st.seed_wind(
                WindDown {
                    phase,
                    goal_paused: true,
                    stops: 2,
                    pre: false,
                    stop_at: esc.then_some(t - Duration::from_secs(1)),
                    ..WindDown::opened(
                        astra(),
                        "gpt-6-luna".to_string(),
                        Some(t + 3 * 24 * 60 * MIN),
                        MARKER.to_string(),
                    )
                },
                t,
            );
            let footer = if matches!(phase, WindPhase::Holding { .. }) {
                astra()
            } else {
                luna()
            };
            let point = TurnEndReading {
                interrupted: true,
                ..codex_at(
                    "■ Conversation interrupted",
                    Some(Duration::from_secs(3)),
                    footer,
                    Some(CodexGoal::Paused),
                )
            };
            let _ = at(&mut st, &point, t + Duration::from_secs(2));
            let kept = st.wind().map(|w| w.phase) == Some(phase);
            assert_eq!(kept, esc, "{phase:?} esc={esc}: {:?}", st.wind());
            // An `esc=` whose row is older than `take_within`: SPENT — its
            // point came and went unwritten (a hold's Esc'd point writes no
            // row) — and the interrupt now is anyone's: a person's.
            let mut spent = TurnEndState::default();
            spent.seed_wind(
                WindDown {
                    phase,
                    goal_paused: true,
                    stops: 2,
                    pre: false,
                    stop_at: esc.then_some(t - 3 * 24 * 60 * MIN),
                    seeded_at: Some(t - 3 * 24 * 60 * MIN),
                    ..WindDown::opened(
                        astra(),
                        "gpt-6-luna".to_string(),
                        Some(t + 3 * 24 * 60 * MIN),
                        MARKER.to_string(),
                    )
                },
                t,
            );
            assert_eq!(spent.wind().and_then(|w| w.stop_at), None, "{phase:?}");
            assert!(spent.own_interrupt.is_none(), "{phase:?}");
            let theirs = TurnEndReading {
                person: Some(Duration::from_secs(5)),
                ..point.clone()
            };
            let _ = at(&mut spent, &theirs, t + Duration::from_secs(2));
            assert_ne!(
                spent.wind().map(|w| w.phase),
                Some(phase),
                "{phase:?} esc={esc}: a spent Esc keeps nothing: {:?}",
                spent.wind()
            );
            // The Esc's own point: no second Esc goes into that turn before it.
            if esc {
                let mut again = TurnEndState::default();
                again.seed_wind(
                    WindDown {
                        phase,
                        stop_at: Some(t),
                        ..WindDown::opened(
                            astra(),
                            "gpt-6-luna".to_string(),
                            None,
                            MARKER.to_string(),
                        )
                    },
                    t,
                );
                assert_eq!(
                    again.goal_stop(Some(Duration::ZERO), Some(CodexGoal::Pursuing), false, t),
                    None
                );
            }
        }
    }
}

/// A PRESS ONLY INTENDED (its intent row the last, a restarted loop's; the
/// re-review's nit): the key may never have gone — no Esc and no `/goal
/// pause` into the goal the thread runs on its own model, the nudge still up
/// decided again (a new press's switch replaces it), and a footer on the
/// thread's own model at a point closes it. The footer showing the cheaper
/// model: it landed, and the switch goes on. The notes said ride the rows:
/// the goal's note is not said again (`told=goal`).
#[test]
fn a_press_only_intended_waits_for_the_footer() {
    let t = t0();
    let intended = || WindDown {
        intent: true,
        pre: false,
        ..WindDown::opened(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string())
    };
    let mut st = TurnEndState::default();
    st.seed_wind(intended(), t);
    assert!(st.switch_open() && !st.switch_landed());
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), Some(CodexGoal::Pursuing), false, t),
        None
    );
    let running = codex_at("Stage done.", Some(MIN), astra(), Some(CodexGoal::Pursuing));
    let a = decide_turn_end(&st, &running, &cfg(), t);
    assert!(
        matches!(a, TurnEndAction::WaitUntil { .. }),
        "no `/goal pause`: {a:?}"
    );
    let _ = at(&mut st, &running, t);
    assert!(!st.switch_open(), "the footer on its own model: released");
    // A new press's switch replaces an intended one.
    let mut st = TurnEndState::default();
    st.seed_wind(intended(), t);
    st.nudge_switched(
        astra(),
        "gpt-6-luna".to_string(),
        Some(t + MIN),
        "ATERM-SAVED-new".to_string(),
    );
    assert!(st.switch_landed());
    assert_eq!(
        st.wind().map(|w| w.marker.as_str()),
        Some("ATERM-SAVED-new")
    );
    // The footer shows the cheaper model: landed.
    let mut st = TurnEndState::default();
    st.seed_wind(intended(), t);
    st.footer_seen(&luna());
    assert!(st.switch_landed());
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), Some(CodexGoal::Pursuing), false, t),
        Some(GoalStop::Esc)
    );
    // `told=goal`: the stops spent, the goal's note is not said again.
    let mut st = TurnEndState::default();
    st.seed_wind(
        WindDown {
            stops: GOAL_STOPS,
            said: vec!["goal"],
            pre: false,
            ..WindDown::opened(astra(), "gpt-6-luna".to_string(), None, MARKER.to_string())
        },
        t,
    );
    assert_eq!(
        st.goal_stop(Some(Duration::ZERO), Some(CodexGoal::Pursuing), false, t),
        None
    );
    assert_eq!(said_key("goal"), Some("goal"));
    assert_eq!(said_key("nonsense"), None);
}

/// A SWITCH STANDING STILL UNDER A BACKGROUND TERMINAL (the loop's bound):
/// said once per switch, naming what is still owed, the session staying on
/// the cheaper model, and WHAT HOLDS IT — a person's typing, a draft, the
/// terminal (the round-3 re-review: it blamed the terminal whatever held
/// it). Before the save (owed, winding) it never advises putting the model
/// back by hand; after it, it names the hand steps. NEGATIVE CONTROL: no
/// switch, nothing said.
#[test]
fn a_switch_standing_still_is_said_once() {
    let t = t0();
    for (phase, owed, saved) in [
        (WindPhase::Owed, "the save instruction is still owed", false),
        (WindPhase::Winding, "the save's answer is still owed", false),
        (
            WindPhase::Restore { hold: true },
            "the thread's own model is still owed back",
            true,
        ),
    ] {
        for (by, held) in [
            (StoodBy::Typing, "a person is typing"),
            (StoodBy::Draft, "a draft stands in the composer"),
            (
                StoodBy::Terminal,
                "a Codex background terminal keeps the session busy",
            ),
        ] {
            let mut st = open_at(t, phase);
            st.switch_stood_still(by);
            st.switch_stood_still(StoodBy::Terminal);
            let notes: Vec<String> = st
                .take_wind_events()
                .into_iter()
                .filter_map(|e| match e {
                    WindEvent::Note(n) => Some(n),
                    _ => None,
                })
                .collect();
            assert_eq!(notes.len(), 1, "{phase:?} {by:?}: {notes:?}");
            let n = &notes[0];
            assert!(
                n.contains(owed) && n.contains("stays on gpt-6-luna") && n.starts_with(held),
                "{phase:?} {by:?}: {n}"
            );
            assert_eq!(
                n.contains("/model"),
                saved,
                "{phase:?} {by:?}: the hand's model only after the save: {n}"
            );
            if by == StoodBy::Terminal {
                assert!(n.contains("/ps, /stop"), "{n}");
            } else {
                assert!(!n.contains("terminal"), "{n}");
            }
        }
    }
    let mut none = TurnEndState::default();
    none.switch_stood_still(StoodBy::Terminal);
    assert!(none.take_wind_events().is_empty());
}

// --- the round-4 re-review (2026-09-28) -------------------------------------

/// THE SAVE'S OWN TURN IS BOUNDED (the owner's rule: the cheaper model only
/// commits and pushes; the round-4 re-review: nothing bounded it): the save's
/// turn running on gpt-6-luna gets ONE Esc once its busy work since the save
/// was typed reaches [`WIND_DOWN_BOUND`] — its earlier turns carried on past
/// an API error counted with the one running — none of the goal's stops
/// spent; never a second into the same turn, never a person's; the save
/// still running after that Esc is said once. The interrupted point judges
/// the marker (none: a person is told, naming the stop) and the switch goes
/// on to `/model` and the hold. NEGATIVE CONTROLS: within the bound nothing
/// is stopped; a marker the save printed before it ran on is the save.
#[test]
fn the_save_turn_is_bounded_then_judged_and_restored() {
    let t = t0();
    let s = Duration::from_secs;
    let mut st = open_at(t, WindPhase::Winding);
    let wd = |st: &TurnEndState| st.wind().cloned().expect("open");
    // Within the bound: the save runs.
    for worked in [s(1), 5 * MIN, WIND_DOWN_BOUND - s(1)] {
        assert_eq!(
            st.goal_stop(Some(worked), None, false, t),
            None,
            "{worked:?}"
        );
    }
    assert_eq!(st.goal_stop(None, None, false, t), None, "no running turn");
    // A carried-on API error before it: its work counts.
    let failed = TurnEndReading {
        wall: Some(WallKind::ApiError {
            code: Some(500),
            retryable: true,
            cause: ApiCause::Server,
        }),
        wall_message: "stream disconnected before completion".to_string(),
        ..codex_at("", Some(10 * MIN), luna(), Some(CodexGoal::Paused))
    };
    st.observe(&failed, t);
    assert_eq!(wd(&st).wound, 10 * MIN);
    assert!(matches!(wd(&st).phase, WindPhase::Winding));
    assert_eq!(st.goal_stop(Some(4 * MIN), None, false, t), None);
    // Past the bound: a person's hand in it spares it; else ONE Esc.
    assert_eq!(
        st.goal_stop(Some(5 * MIN), None, true, t),
        None,
        "a person's"
    );
    assert_eq!(
        st.goal_stop(Some(5 * MIN), None, false, t),
        Some(GoalStop::Esc)
    );
    let stops = wd(&st).stops;
    st.own_esc(t);
    assert_eq!(wd(&st).stops, stops, "none of the goal's stops");
    assert!(wd(&st).overran);
    assert_eq!(
        st.goal_stop(Some(6 * MIN), None, false, t + s(5)),
        None,
        "never twice into one turn"
    );
    // The Esc did not take: said once, no second Esc.
    let later_ = t + 2 * MIN;
    assert_eq!(
        st.goal_stop(Some(7 * MIN), None, false, later_),
        Some(GoalStop::Tell)
    );
    st.goal_told();
    let notes: Vec<String> = st
        .take_wind_events()
        .into_iter()
        .filter_map(|e| match e {
            WindEvent::Note(n) => Some(n),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("only to commit and push") && notes[0].contains("Esc did not stop it"),
        "{}",
        notes[0]
    );
    assert_eq!(
        st.goal_stop(Some(8 * MIN), None, false, later_),
        None,
        "told once"
    );
    // Its interrupted point: judged, unsaved and said so, then `/model`.
    let stopped = TurnEndReading {
        interrupted: true,
        ..codex_at(
            "• Tests are failing; fixing the parser first.\n■ Conversation interrupted",
            Some(16 * MIN),
            luna(),
            Some(CodexGoal::Paused),
        )
    };
    let a = at(&mut st, &stopped, later_ + s(3));
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "{a:?}");
    assert_eq!(wd(&st).saved, Some(false));
    let events = st.take_wind_events();
    assert!(
        events.iter().any(|e| matches!(e, WindEvent::Note(n)
            if n.contains("did not confirm") && n.contains("aterm stopped its turn after 15 min"))),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WindEvent::Edge { what, .. }
            if what == "not saved: its turn stopped past 15 min")),
        "{events:?}"
    );
    // CONTROL: a marker printed before the save ran on is the save.
    let mut st = open_at(t, WindPhase::Winding);
    assert_eq!(
        st.goal_stop(Some(WIND_DOWN_BOUND), None, false, t),
        Some(GoalStop::Esc)
    );
    st.own_esc(t);
    let marked = TurnEndReading {
        interrupted: true,
        ..codex_at(
            &format!("Pushed.\n{MARKER}\n• Now the next stage.\n■ Conversation interrupted"),
            Some(16 * MIN),
            luna(),
            Some(CodexGoal::Paused),
        )
    };
    let a = at(&mut st, &marked, t + s(3));
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "{a:?}");
    assert_eq!(st.wind().and_then(|w| w.saved), Some(true));
}

/// A PRESS ONLY INTENDED IS NEVER SAID TO HAVE PUT THE SESSION ON THE
/// CHEAPER MODEL, AND ITS ROWS SAY `intent` (the round-4 re-review, its
/// unit probe made real): an owed switch whose press is only intended, its
/// footer showing no model or the thread's own at the switch's first point,
/// a draft standing — no `draft` note claiming the session is on
/// gpt-6-luna, and the switch stays an intent, its ledger word `intent`.
/// NEGATIVE CONTROL: the landed press's draft note is said, its word `owed`.
#[test]
fn an_intended_press_says_no_draft_note_and_rows_say_intent() {
    let t = t0();
    for (label, model) in [("footer none", None), ("footer astra", Some(astra()))] {
        let mut st = open_at(t, WindPhase::Owed);
        if let Some(w) = st.wind.as_mut() {
            w.intent = true;
            w.pre = true;
        }
        assert_eq!(wind_phase_word(st.wind().expect("open")), "intent");
        let r = TurnEndReading {
            composer: Composer::Typed,
            model_field: model,
            ..codex_at("Stage done.", None, astra(), Some(CodexGoal::Paused))
        };
        let _ = at(&mut st, &r, t + MIN);
        let ev = st.take_wind_events();
        assert!(
            !ev.iter()
                .any(|e| matches!(e, WindEvent::Note(n) if n.contains("gpt-6-luna to save"))),
            "{label}: {ev:?}"
        );
        let w = st.wind().expect("still open");
        assert!(w.intent, "{label}");
        assert_eq!(wind_phase_word(w), "intent", "{label}");
    }
    // CONTROL: the press seen landing — the draft note, its word `owed`.
    let mut st = open_at(t, WindPhase::Owed);
    let r = TurnEndReading {
        composer: Composer::Typed,
        ..codex_at("Stage done.", None, luna(), Some(CodexGoal::Paused))
    };
    let _ = at(&mut st, &r, t + MIN);
    let ev = st.take_wind_events();
    assert!(
        ev.iter()
            .any(|e| matches!(e, WindEvent::Note(n) if n.contains("a draft stands"))),
        "{ev:?}"
    );
    assert_eq!(wind_phase_word(st.wind().expect("open")), "owed");
    // An intent past its owed phase (a person's message took it over) is
    // that phase's word: the takeover is never read back as an intent.
    let mut st = open_at(t, WindPhase::Restore { hold: false });
    if let Some(w) = st.wind.as_mut() {
        w.intent = true;
    }
    assert_eq!(wind_phase_word(st.wind().expect("open")), "restore-free");
}

/// THE POLICY'S OWN HOLD IS NAMED (the round-4 re-review: a switch held by
/// the policy's own wait was told as the terminal's, with a fix that fixes
/// nothing): [`TurnEndState::switch_hold`] names the footer an owed save
/// waits on (no model, the thread's own model, a press only intended), a
/// fallen sandbox, and spent restores — and the stood-still note says each
/// with its own fix, never that the session is on the cheaper model when
/// the press is only intended. NEGATIVE CONTROLS: the owed save whose
/// footer shows the cheaper model, a winding turn and an untried restore
/// are held by nothing of the policy's.
#[test]
fn the_policys_own_hold_is_named() {
    let t = t0();
    let owed_on = |model: Option<CodexSetting>| TurnEndReading {
        model_field: model,
        ..codex_at("Stage done.", None, luna(), Some(CodexGoal::Paused))
    };
    let st = open_at(t, WindPhase::Owed);
    assert_eq!(st.switch_hold(&owed_on(None)), Some(StoodBy::Footer));
    assert_eq!(
        st.switch_hold(&owed_on(Some(astra()))),
        Some(StoodBy::Footer)
    );
    assert_eq!(st.switch_hold(&owed_on(Some(luna()))), None);
    let fell = TurnEndReading {
        sandbox_fell: Some("workspace-write".to_string()),
        ..owed_on(Some(luna()))
    };
    assert_eq!(st.switch_hold(&fell), Some(StoodBy::Sandbox));
    let mut intended = open_at(t, WindPhase::Owed);
    if let Some(w) = intended.wind.as_mut() {
        w.intent = true;
    }
    assert_eq!(
        intended.switch_hold(&owed_on(Some(luna()))),
        Some(StoodBy::Footer)
    );
    assert_eq!(
        open_at(t, WindPhase::Winding).switch_hold(&owed_on(Some(luna()))),
        None
    );
    let mut restore = open_at(t, WindPhase::Restore { hold: true });
    assert_eq!(restore.switch_hold(&owed_on(Some(luna()))), None);
    if let Some(w) = restore.wind.as_mut() {
        w.restore_tries = RESTORE_TRIES;
    }
    assert_eq!(
        restore.switch_hold(&owed_on(Some(luna()))),
        Some(StoodBy::RestoreSpent)
    );
    // Each said with its own fix.
    let note = |mut st: TurnEndState, by: StoodBy| {
        st.switch_stood_still(by);
        st.take_wind_events()
            .into_iter()
            .find_map(|e| match e {
                WindEvent::Note(n) => Some(n),
                _ => None,
            })
            .expect("said")
    };
    let n = note(open_at(t, WindPhase::Owed), StoodBy::Footer);
    assert!(
        n.contains("footer has not shown gpt-6-luna")
            && n.contains("cannot tell whether the switch landed")
            && !n.contains("stays on gpt-6-luna")
            && !n.contains("terminal"),
        "{n}"
    );
    let n = note(intended, StoodBy::Footer);
    assert!(!n.contains("stays on gpt-6-luna"), "{n}");
    let n = note(open_at(t, WindPhase::Owed), StoodBy::Sandbox);
    assert!(
        n.contains("sandbox") && n.contains("codex resume") && !n.contains("/ps"),
        "{n}"
    );
    let n = note(restore, StoodBy::RestoreSpent);
    assert!(
        n.contains("/model did not put the thread back")
            && n.contains("/model → GPT-6-Astra ultra")
            && !n.contains("terminal"),
        "{n}"
    );
}

/// A DONE TASK NEVER STRANDS AN OPEN SWITCH (the merge of 2026-09-29: main's
/// done check beside the save-then-wait switch). The worker said DONE after
/// the done check — nothing more is typed there — and the rate nudge's
/// switch is open on the session: its own steps still go, the save while it
/// is owed and `/model` while the thread's own model is owed back, so the
/// thread is never left on the cheaper model. NEGATIVE CONTROL: the same
/// done task with no switch open types nothing, as the done check rules.
#[test]
fn a_done_task_never_strands_an_open_switch() {
    let t = t0();
    let done = |wind: Option<WindPhase>| {
        let mut st = TurnEndState::default();
        if let Some(phase) = wind {
            st.nudge_switched(
                astra(),
                "gpt-6-luna".to_string(),
                Some(t + 3 * 24 * 60 * MIN),
                MARKER.to_string(),
            );
            set_phase(&mut st, phase);
        }
        // The check's own yield said DONE: the task is done.
        st.done = 2;
        st.point_at = Some(t - MIN);
        assert!(st.task_done());
        st
    };
    let reading = |footer: CodexSetting| codex_at("DONE", None, footer, Some(CodexGoal::Paused));
    // Owed its save, the footer on the cheaper model: the save is typed.
    let mut st = done(Some(WindPhase::Owed));
    let a = at(&mut st, &reading(luna()), t);
    assert_eq!(a.rule_id(), Some(RULE_WIND_DOWN), "{a:?}");
    assert!(st.task_done(), "the task stays done");
    // Owed its model back: `/model` is typed.
    let mut st = done(Some(WindPhase::Restore { hold: false }));
    let a = at(&mut st, &reading(luna()), t);
    assert_eq!(a.rule_id(), Some(RULE_MODEL_RESTORE), "{a:?}");
    // NEGATIVE CONTROL: no switch open, nothing typed.
    let mut st = done(None);
    let a = at(&mut st, &reading(astra()), t);
    assert_eq!(a, TurnEndAction::Nothing);
}
