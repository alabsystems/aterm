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
        upgrading: false,
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

/// `at`, and the act recorded as typed.
fn act(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> TurnEndAction {
    let a = at(st, r, now);
    st.acted(&a, r, now);
    a
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

#[test]
fn a_turn_end_after_real_work_is_continued_and_a_short_one_is_not() {
    let now = t0();
    let mut st = TurnEndState::default();
    assert_eq!(
        at(&mut st, &idle("Fixed the parser.", Some(3 * MIN)), now),
        typed(RULE_CONTINUE)
    );
    // Negative control: a quick exchange is the human's, not a stall.
    let mut st = TurnEndState::default();
    assert_eq!(
        at(
            &mut st,
            &idle("Fixed the parser.", Some(30 * Duration::from_secs(1))),
            now
        ),
        TurnEndAction::Nothing
    );
    // Nor is a point the loop never saw the worker busy before.
    let mut st = TurnEndState::default();
    assert_eq!(
        at(&mut st, &idle("Fixed the parser.", None), now),
        TurnEndAction::Nothing
    );
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

#[test]
fn a_stop_phrase_escalates_and_an_offer_continues() {
    let now = t0();
    for said in [
        "I need your decision on the schema before going on.",
        "Blocked on the signing key; waiting on you.",
        "Should I rewrite the parser or patch the lexer?",
        "Two ways forward:\n1. rewrite the parser\n2. patch the lexer\nWhich one?",
        "Did the suite pass on your machine?",
    ] {
        let mut st = TurnEndState::default();
        let a = at(&mut st, &idle(said, Some(3 * MIN)), now);
        assert!(matches!(a, TurnEndAction::Escalate { .. }), "{said}: {a:?}");
    }
    for said in [
        "Done: 12 of 67 solve. Next: the binder path; want me to take that on?",
        "Shall I carry on with stage 2?",
        "Two ways forward:\n1. rewrite the parser\n2. patch the lexer\nI recommend 1; shall I start?",
        "Next steps: wire the lane into the host.",
        "Created a.txt with one line. Should I also create b.txt?",
    ] {
        let mut st = TurnEndState::default();
        assert_eq!(
            at(&mut st, &idle(said, Some(3 * MIN)), now),
            typed(RULE_CONTINUE),
            "{said}"
        );
    }
}

#[test]
fn the_budget_stops_at_six_an_hour() {
    let mut now = t0();
    let mut st = TurnEndState::default();
    for i in 0..6 {
        let a = act(&mut st, &idle("Stage done.", Some(3 * MIN)), now);
        assert_eq!(a, typed(RULE_CONTINUE), "continuation {i}");
        now += 5 * MIN;
    }
    let a = at(&mut st, &idle("Stage done.", Some(3 * MIN)), now);
    assert!(is_escalate(&a, "budget spent: 6"), "{a:?}");
    // The window slides: an hour after the first, one more may go.
    now = t0() + 61 * MIN;
    assert_eq!(
        decide_turn_end(&st, &idle("Stage done.", Some(3 * MIN)), &cfg(), now),
        typed(RULE_CONTINUE)
    );
}

#[test]
fn two_short_continuations_in_a_row_are_worker_reports_done() {
    let now = t0();
    let mut st = TurnEndState::default();
    let short = Some(Duration::from_secs(20));
    assert_eq!(
        act(&mut st, &idle("Done.", Some(3 * MIN)), now),
        typed(RULE_CONTINUE)
    );
    // The first short yield is continued again: the point follows ours.
    assert_eq!(
        act(&mut st, &idle("Done.", short), now),
        typed(RULE_CONTINUE)
    );
    assert_eq!(st.short_streak(), 1);
    let a = at(&mut st, &idle("Done.", short), now);
    assert!(is_escalate(&a, "worker reports done"), "{a:?}");
    // Negative control: a long yield in between ends the streak.
    let mut st = TurnEndState::default();
    act(&mut st, &idle("Done.", Some(3 * MIN)), now);
    act(&mut st, &idle("Done.", short), now);
    assert_eq!(
        act(&mut st, &idle("Done.", Some(5 * MIN)), now),
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
            composer: Composer::Typed,
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
fn a_529_backs_off_then_continues_exactly_once_per_step_then_escalates() {
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
            why: "overloaded retry 1 of 3".to_string()
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
    // The wall again: 5 min from its new appearance, then 15, then escalate.
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
    let fourth = third + 16 * MIN;
    assert!(is_escalate(&at(&mut st, &back, fourth), "after 3 retries"));
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

#[test]
fn an_api_error_that_does_not_retry_escalates_and_retries_off_escalate() {
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
    assert!(is_escalate(&at(&mut st, &r, t), "API error 400"));
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
    // A notice that says the vendor goes on by itself: nothing at all.
    let mut st = TurnEndState::default();
    r.resumes_by_itself = true;
    assert_eq!(at(&mut st, &r, t + 90 * MIN), TurnEndAction::Nothing);
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
    // point is the continue policy's (a short turn: nothing).
    let mut st = TurnEndState::default();
    at(&mut st, &r, t);
    assert_eq!(
        at(
            &mut st,
            &idle("Back.", Some(Duration::from_secs(20))),
            t + MIN
        ),
        TurnEndAction::Nothing
    );
}

#[test]
fn a_fable_limit_switches_to_opus_continues_and_switches_back_at_its_reset() {
    let t = t0();
    let mut st = TurnEndState::default();
    let mut r = walled(
        WallKind::ModelBucket { consent: false },
        "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
        Some(3 * MIN),
    );
    r.reset_at = Some(t + 120 * MIN);
    let a = act(&mut st, &r, t);
    assert_eq!(
        a,
        TurnEndAction::TypeCommand {
            command: "/model opus".to_string(),
            rule_id: RULE_MODEL_FALLBACK,
            then: Then::Continue
        }
    );
    assert_eq!(
        st.model_switch(),
        Some(&ModelSwitch {
            from: Some("fable".to_string()),
            to: "opus".to_string(),
            back_at: Some(t + 120 * MIN)
        })
    );
    // The `/model` output: no wall, no work — the continuation owed.
    assert_eq!(
        act(&mut st, &idle("", None), t + MIN),
        typed(RULE_MODEL_FALLBACK)
    );
    // Hours of work on opus; the turn ends after the bucket's reset: back
    // to fable first, the continuation owed after it.
    let later = idle("Stage 3 done.", Some(30 * MIN));
    let a = act(&mut st, &later, t + 130 * MIN);
    assert_eq!(
        a,
        TurnEndAction::TypeCommand {
            command: "/model fable".to_string(),
            rule_id: RULE_MODEL_RESTORE,
            then: Then::Continue
        }
    );
    assert_eq!(st.model_switch(), None);
    assert_eq!(
        act(&mut st, &idle("", None), t + 131 * MIN),
        typed(RULE_MODEL_RESTORE)
    );
}

#[test]
fn a_model_bucket_never_switches_under_consent_a_box_or_twice() {
    let t = t0();
    let consent = walled(
        WallKind::ModelBucket { consent: true },
        "Fable limit reached · continuing on Sonnet uses usage credits, and the prompt to confirm",
        Some(3 * MIN),
    );
    let mut st = TurnEndState::default();
    assert!(is_escalate(&at(&mut st, &consent, t), "consent"));
    // The consent dialog is a box: nothing is typed.
    let boxed = TurnEndReading {
        phase: Phase::Prompt,
        ..consent.clone()
    };
    let mut st = TurnEndState::default();
    assert_eq!(at(&mut st, &boxed, t), TurnEndAction::Nothing);
    // The bucket again on the fallback: escalate, no second switch.
    let r = walled(
        WallKind::ModelBucket { consent: false },
        "You've reached your Fable limit.",
        Some(3 * MIN),
    );
    let mut st = TurnEndState::default();
    act(&mut st, &r, t);
    act(&mut st, &idle("", None), t + MIN);
    let opus = walled(
        WallKind::ModelBucket { consent: false },
        "You've reached your Opus limit.",
        Some(3 * MIN),
    );
    assert!(is_escalate(
        &at(&mut st, &opus, t + 5 * MIN),
        "again after switching"
    ));
    // No fallback configured: escalate — or, with limits resumed, wait out
    // the bucket's reset as a usage window's.
    let mut none = cfg();
    none.model_fallback = None;
    none.resume_limits = false;
    let mut st = TurnEndState::default();
    st.observe(&r, t);
    assert!(is_escalate(
        &decide_turn_end(&st, &r, &none, t),
        "no fallback"
    ));
    none.resume_limits = true;
    let reset = TurnEndReading {
        reset_at: Some(t + 90 * MIN),
        ..r.clone()
    };
    assert!(matches!(
        decide_turn_end(&st, &reset, &none, t),
        TurnEndAction::WaitUntil { until, .. } if until == t + 91 * MIN
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
fn a_full_context_is_compacted_then_continued_and_escalated_the_second_time() {
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
    // `/compact` did not free it (no work in between): escalate.
    let mut st = TurnEndState::default();
    act(&mut st, &r, t);
    assert!(is_escalate(
        &at(
            &mut st,
            &TurnEndReading {
                worked: None,
                ..r.clone()
            },
            t + MIN
        ),
        "still full"
    ));
}

#[test]
fn a_lost_login_types_login_then_escalates_and_money_escalates() {
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
    let s = walled(
        WallKind::Spend,
        "You've hit your monthly spend limit.",
        Some(3 * MIN),
    );
    assert!(is_escalate(&at(&mut st, &s, t), "spend limit"));
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
    // The worker took it, briefly: judged now.
    let short = Some(Duration::from_secs(20));
    assert_eq!(
        at(&mut st, &idle("Done.", short), now),
        typed(RULE_CONTINUE)
    );
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
/// (`All finished, nothing left.`) — was awaited forever, and the session
/// never continued or escalated again. It is waited for
/// [`TurnEndTiming::take_within`] from the first such point, then judged
/// as the short yield it is: once more continued, and the second time
/// escalated as done. Negative control: a busy read before the deadline is
/// the act's point as before, and a busy screen past the deadline is never
/// judged idle.
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
    // At the deadline: the short yield, continued once more.
    assert_eq!(act(&mut st, &quiet, first + take), typed(RULE_CONTINUE));
    assert_eq!(st.short_streak(), 1);
    // The second unseen yield: worker reports done — escalated, hours on.
    let later = first + take + Duration::from_secs(5);
    assert!(awaits(&at(&mut st, &quiet, later), later));
    for h in 1..=5 {
        let a = at(&mut st, &quiet, later + h * 60 * MIN);
        assert!(is_escalate(&a, "worker reports done"), "{a:?}");
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
/// no spinner, no reply) is escalated at its deadline as not taken, never
/// waited on silently. Control: before the deadline it is waited for.
#[test]
fn a_continuation_left_unanswered_is_escalated_as_not_taken() {
    let t = t0();
    let take = TurnEndTiming::default().take_within;
    let mut st = TurnEndState::default();
    act(&mut st, &idle("Stage 1 done.", Some(3 * MIN)), t);
    let pending = TurnEndReading {
        pending_input: true,
        ..idle("Stage 1 done.", None)
    };
    assert!(awaits(&at(&mut st, &pending, t), t));
    let a = at(&mut st, &pending, t + take);
    assert!(is_escalate(&a, "has not taken it"), "{a:?}");
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
    let stop = |said: &str| matches!(classify_said(Some(said)), Said::Stop(_));
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
    // And through the decider: the wrapped choice is escalated, not typed.
    let mut st = TurnEndState::default();
    let a = at(
        &mut st,
        &idle("Want me to keep the old API\nor rename it?", Some(3 * MIN)),
        now,
    );
    assert!(is_escalate(&a, "choose between options"), "{a:?}");
}

/// The safety review of 2026-09-24 (blocker): Esc on a turn that ran for
/// minutes drew `⎿  Interrupted · What should Claude do instead?`, read as a
/// question with no stop phrase, and the policy typed `keep going` —
/// restarting what the person had just stopped. The measured shapes, read
/// through aterm-phase's reader, now type nothing (and escalate nothing).
/// Negative control: the same turn without the interrupt row is continued.
#[test]
fn a_persons_interrupt_is_never_continued() {
    let rule = "─".repeat(100);
    let decide = |body: &[&str]| {
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
        let now = t0();
        let mut st = TurnEndState::default();
        (r.interrupted, at(&mut st, &r, now))
    };
    let (read, a) = decide(&[
        "⏺ Running the schema migration against the staging database now.",
        "",
        "⏺ Bash(./migrate.sh --env staging)",
        "  ⎿  Interrupted · What should Claude do instead?",
    ]);
    assert!(read);
    assert_eq!(a, TurnEndAction::Nothing);
    let (read, a) = decide(&[
        "⏺ Running the schema migration against the staging database now.",
        "  ⎿  Interrupted · What should Claude do instead?",
    ]);
    assert!(read);
    assert_eq!(a, TurnEndAction::Nothing);
    // Negative control: no interrupt, the same work — continued.
    let (read, a) = decide(&["⏺ Ran the schema migration against the staging database."]);
    assert!(!read);
    assert_eq!(a, typed(RULE_CONTINUE));
}

/// The safety review of 2026-09-24 (blocker): three shapes of a destructive
/// question were continued — a benign offer AFTER the destructive one, a
/// trailing `(y/n)` / `[y/N]`, and a question followed by an aside — and
/// `keep going` reads as yes. Each is escalated now, through the decider.
/// Negative controls: the offers and reports the audit's census continues
/// still continue (a destructive word BEFORE the offer is what was done).
#[test]
fn a_destructive_question_in_any_shape_is_escalated() {
    let now = t0();
    for said in [
        "Want me to delete the 3 stale worktrees under ~/aterm-*? Happy to also update the docs.",
        "Want me to delete the 3 stale worktrees under ~/aterm-*?\nOr want me to update the \
         changelog first?",
        "Cleanup is staged. Delete the stale release branches now? (y/n)",
        "All green locally. OK to force-push the rebased branch to main? [y/N]",
        "Should I drop the legacy table now?\n(I have not touched it yet.)",
        "Created a.txt. Should I also delete the old b.txt?",
        "Created a.txt. Should I also create b.txt, or stop here?",
    ] {
        assert!(matches!(classify_said(Some(said)), Said::Stop(_)), "{said}");
        let mut st = TurnEndState::default();
        let a = at(&mut st, &idle(said, Some(5 * MIN)), now);
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
        at(&mut st, &r, t0())
    };
    for last in [
        "  Should I drop the prod table or keep it?",
        "  Want me to force-push the rewritten history to origin/main?",
    ] {
        let a = decide(last);
        assert!(matches!(a, TurnEndAction::Escalate { .. }), "{last}: {a:?}");
    }
    assert_eq!(decide("  The suite is green."), typed(RULE_CONTINUE));
    // A question the reader has no words for.
    let mut r = idle("x?", Some(3 * MIN));
    r.said_tail = None;
    let mut st = TurnEndState::default();
    assert!(is_escalate(&at(&mut st, &r, t0()), "do not show"));
}

/// The reliability review of 2026-09-24 (major): the live upgrade sweep types
/// its announcement and waits for the worker to wind down and answer READY,
/// while the continue policy typed `keep going` into the wind-down —
/// restarting work (the upgrade deferred) or, after two short turns,
/// escalating a false "worker reports done". A point that answers the
/// announcement, or carries the READY marker, is the sweep's: nothing typed.
/// NEGATIVE CONTROLS: the sweep's `Upgraded:` continuation asks the worker
/// to go on, so a turn after it is continued; so is an ordinary turn.
#[test]
fn a_point_that_answers_the_upgrade_announcement_is_the_sweeps() {
    use crate::harness::upgrade::{Source, Version, continue_prompt, prepare_prompt, ready_marker};
    let from = Version::parse("2.1.280").expect("version");
    let to = Version::parse("2.1.281").expect("version");
    let marker = ready_marker("sess", &to, 7);
    let announce = prepare_prompt(&from, &to, Source::Native, &marker);
    let rule = "─".repeat(100);
    let decide = |user: &str, said: &[&str]| {
        let mut rows = rows_of(&["⏺ Earlier work.", "", &format!("❯ {user}"), ""]);
        rows.extend(rows_of(said));
        rows.extend(rows_of(&[
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
        let mut st = TurnEndState::default();
        (r.upgrading, at(&mut st, &r, t0()))
    };
    let (owned, a) = decide(
        &announce,
        &["⏺ Committed the parser work; nothing is running."],
    );
    assert!(owned);
    assert_eq!(a, TurnEndAction::Nothing);
    let (owned, a) = decide("summarise", &[&format!("⏺ Done.\n  {marker}")]);
    assert!(owned, "the READY marker in the worker's words");
    assert_eq!(a, TurnEndAction::Nothing);
    // Negative controls.
    let (owned, a) = decide(
        &continue_prompt(&from, &to),
        &["⏺ Resumed; stage 3 is done."],
    );
    assert!(!owned);
    assert_eq!(a, typed(RULE_CONTINUE));
    let (owned, a) = decide("carry on with the parser", &["⏺ Stage 3 is done."]);
    assert!(!owned);
    assert_eq!(a, typed(RULE_CONTINUE));
}
